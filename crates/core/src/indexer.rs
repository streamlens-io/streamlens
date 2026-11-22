//! The indexing pass: demux + parse the whole file once, feed every access
//! unit through the syntax analyzer, and assemble the [`StreamIndex`]. The
//! video is not copied: decoding reads it back from the file (see
//! [`crate::source`]).
//!
//! [`start`] runs the pass on a background thread and publishes growing
//! snapshots, so a long file can be viewed while it is still indexed.

use std::collections::HashMap;

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{mpsc, Arc, Mutex};
use std::time::{Duration, Instant};

use streamlens_media::{Demuxer, Source};
use streamlens_syntax::analyzer::StreamProps;
use streamlens_syntax::diag::Diagnostics;
use streamlens_syntax::h264::H264Analyzer;
use streamlens_syntax::h265::H265Analyzer;
use streamlens_syntax::{AuAnalyzer, AuSummary};
use streamlens_types::{Codec, FrameRecord, FrameType, StreamInfo};

use crate::chunked::Chunked;
use crate::index::{IndexHandle, IndexProgress, ParamInsertion, StreamIndex, StreamStats};
use crate::Error;

/// The analyzer, shared between the index pass and header re-parses.
pub type SharedAnalyzer = Arc<Mutex<Box<dyn AuAnalyzer>>>;

/// A frame's display position is final once no frame decoded later can sort
/// before it. Decoders reorder within a bounded window (the DPB), so a cut
/// with this many frames known after it, all sorting after it, is final.
const PUBLISH_LAG: usize = 32;

/// Indexes a media file. Blocking; run on a worker thread.
#[derive(Default)]
pub struct Indexer;

impl Indexer {
    /// Runs the whole index pass. `progress` is called roughly every 64
    /// frames. Also returns the analyzer with its final parameter-set state
    /// (used to re-parse AUs for the Headers panel).
    pub fn index(
        &self,
        source: impl Into<Source>,
        mut progress: impl FnMut(IndexProgress),
    ) -> Result<(StreamIndex, Box<dyn AuAnalyzer>), Error> {
        let source = source.into();
        let (mut demuxer, analyzer) = open(&source)?;
        let analyzer = Mutex::new(analyzer);
        let mut last = None;
        run_pass(
            &mut demuxer,
            &analyzer,
            &source,
            None,
            &AtomicBool::new(false),
            &mut progress,
            &mut |snapshot| last = Some(snapshot),
        )?;
        let index = last.expect("a complete pass publishes");
        let analyzer = analyzer.into_inner().unwrap_or_else(|e| e.into_inner());
        Ok((index, analyzer))
    }
}

/// Indexing running on a background thread; dropping it stops the thread.
pub(crate) struct Indexing {
    cancel: Arc<AtomicBool>,
}

impl Drop for Indexing {
    fn drop(&mut self) {
        self.cancel.store(true, Ordering::Relaxed);
    }
}

/// Starts indexing `path` on a background thread and returns once the first
/// snapshot is published (for short files, the complete index).
/// `on_publish` sees every snapshot, the first one included.
pub(crate) fn start(
    source: &Source,
    mut progress: impl FnMut(IndexProgress) + Send + 'static,
    on_publish: impl Fn(&Arc<StreamIndex>) + Send + 'static,
) -> Result<(IndexHandle, SharedAnalyzer, Indexing), Error> {
    let (mut demuxer, analyzer) = open(source)?;
    let analyzer: SharedAnalyzer = Arc::new(Mutex::new(analyzer));
    let cancel = Arc::new(AtomicBool::new(false));
    let (first_tx, first_rx) = mpsc::channel::<Result<IndexHandle, Error>>();

    let thread_analyzer = analyzer.clone();
    let thread_cancel = cancel.clone();
    let source = source.clone();
    std::thread::Builder::new()
        .name("streamlens-index".into())
        .spawn(move || {
            let mut handle: Option<IndexHandle> = None;
            let mut publish = |snapshot: StreamIndex| {
                let snapshot = Arc::new(snapshot);
                match &handle {
                    Some(h) => h.set(snapshot.clone()),
                    None => {
                        let h = IndexHandle::new(snapshot.clone());
                        handle = Some(h.clone());
                        let _ = first_tx.send(Ok(h));
                    }
                }
                on_publish(&snapshot);
            };
            let result = run_pass(
                &mut demuxer,
                &thread_analyzer,
                &source,
                Some(Duration::from_millis(200)),
                &thread_cancel,
                &mut progress,
                &mut publish,
            );
            if let Err(e) = result {
                // Before the first snapshot the caller gets the error;
                // after it, what was published stays usable.
                log::error!("indexing {source}: {e}");
                let _ = first_tx.send(Err(e));
            }
        })?;

    let handle = first_rx
        .recv()
        .map_err(|_| Error::Pipeline("indexing stopped".into()))??;
    Ok((handle, analyzer, Indexing { cancel }))
}

fn open(source: &Source) -> Result<(Demuxer, Box<dyn AuAnalyzer>), Error> {
    let demuxer = Demuxer::open(source).map_err(|e| match e {
        streamlens_media::Error::NoVideoStream => Error::NoVideoStream(source.to_string()),
        e => e.into(),
    })?;
    let analyzer: Box<dyn AuAnalyzer> = match demuxer.codec() {
        Codec::H264 => Box::new(H264Analyzer::new()),
        Codec::H265 => Box::new(H265Analyzer::new()),
    };
    Ok((demuxer, analyzer))
}

/// Analyzes every access unit and publishes snapshots: every `interval`
/// when set, and always a complete one at the end. Keyframes that lack
/// in-band parameter sets get an insertion recorded, so a decode can start
/// at any keyframe.
fn run_pass(
    demuxer: &mut Demuxer,
    analyzer: &Mutex<Box<dyn AuAnalyzer>>,
    source: &Source,
    interval: Option<Duration>,
    cancel: &AtomicBool,
    progress: &mut dyn FnMut(IndexProgress),
    publish: &mut dyn FnMut(StreamIndex),
) -> Result<(), Error> {
    let lock = || analyzer.lock().unwrap_or_else(|e| e.into_inner());
    let container = match source {
        Source::File(p) => p
            .extension()
            .map(|e| e.to_string_lossy().into_owned())
            .unwrap_or_default(),
        Source::Url(_) => demuxer.format_name().to_string(),
    };
    let mut builder = Builder::new(demuxer.codec(), source, container);

    // MP4/MKV keep the parameter sets in the track header; the analyzer
    // needs them, but they belong to no frame.
    let config = demuxer.config_nals();
    if !config.is_empty() {
        lock().analyze(0, config, &mut builder.diags);
    }

    let mut last_publish = Instant::now();
    let mut analyzed = 0u32;
    while let Some(au) = demuxer.read()? {
        if cancel.load(Ordering::Relaxed) {
            return Ok(());
        }
        let mut a = lock();
        // Packets without a picture (parameter sets only) are no frame;
        // the source reader passes them on with the next one.
        let Some(mut summary) = a.analyze(analyzed, au.data, &mut builder.diags) else {
            continue;
        };
        let file_offset = au
            .file_offset
            .ok_or_else(|| Error::Pipeline("the container reports no packet positions".into()))?;
        summary.is_keyframe |= au.keyframe;
        let insertion = a.parameter_sets_to_insert(au.data, &mut summary);
        drop(a);

        let frame = FrameRecord {
            decode_index: analyzed,
            display_index: analyzed, // assigned when published
            size_bytes: au.coded_size,
            file_offset,
            frame_type: summary.frame_type,
            is_keyframe: summary.is_keyframe,
            is_idr: summary.is_idr,
            is_rasl: summary.is_rasl,
            pts: au.pts_ns,
            dts: au.dts_ns,
            duration: au.duration_ns,
            poc: summary.poc,
            temporal_id: summary.temporal_id,
            slice_qp: summary.slice_qp.clamp(i8::MIN as i32, i8::MAX as i32) as i8,
            slice_count: summary.slice_count,
            ref_frames: summary.ref_frames.clone(),
        };
        builder.push(frame, summary, insertion.map(|i| (i.at, i.bytes)));
        analyzed += 1;

        if analyzed.is_multiple_of(64) {
            progress(IndexProgress {
                frames_indexed: analyzed,
                fraction: au.progress,
            });
        }
        if interval.is_some_and(|i| last_publish.elapsed() >= i) {
            last_publish = Instant::now();
            if builder.publish_ready(false) {
                publish(builder.snapshot(lock().stream_props(), false));
            }
        }
    }
    if analyzed == 0 {
        return Err(Error::Pipeline(format!(
            "no decodable video frames found in {source}"
        )));
    }
    builder.publish_ready(true);
    publish(builder.snapshot(lock().stream_props(), true));
    Ok(())
}

type Pending = (FrameRecord, AuSummary, u32);

/// Collects frames and moves them into the published lists at clean cuts.
struct Builder {
    codec: Codec,
    source: Source,
    container: String,
    frames: Chunked<FrameRecord>,
    analysis: Chunked<AuSummary>,
    display_to_decode: Chunked<u32>,
    insertions: HashMap<u32, ParamInsertion>,
    diags: Diagnostics,
    /// Frames analyzed but not published yet, with their IDR period.
    pending: Vec<Pending>,
    period: u32,
    /// Insertions repeat the same parameter sets; share the bytes.
    last_block: Option<Arc<[u8]>>,
    stats: StatsAcc,
}

/// Display order: by IDR period, then POC, decode order breaking ties.
fn display_key(p: &Pending) -> (u32, i32, u32) {
    (p.2, p.0.poc, p.0.decode_index)
}

impl Builder {
    fn new(codec: Codec, source: &Source, container: String) -> Self {
        Self {
            codec,
            source: source.clone(),
            container,
            frames: Chunked::default(),
            analysis: Chunked::default(),
            display_to_decode: Chunked::default(),
            insertions: HashMap::new(),
            diags: Diagnostics::default(),
            pending: Vec::new(),
            period: 0,
            last_block: None,
            stats: StatsAcc::default(),
        }
    }

    fn push(
        &mut self,
        frame: FrameRecord,
        summary: AuSummary,
        insertion: Option<(usize, Vec<u8>)>,
    ) {
        if frame.is_idr && frame.decode_index != 0 {
            self.period += 1;
        }
        if let Some((at, bytes)) = insertion {
            let bytes = match &self.last_block {
                Some(b) if **b == *bytes => b.clone(),
                _ => Arc::from(bytes),
            };
            self.last_block = Some(bytes.clone());
            self.insertions.insert(
                frame.decode_index,
                ParamInsertion {
                    at: at as u32,
                    bytes,
                },
            );
        }
        self.pending.push((frame, summary, self.period));
    }

    /// The largest number of pending frames that can be published: a cut is
    /// clean when every frame before it sorts before every frame after it,
    /// and at least [`PUBLISH_LAG`] frames are known after it.
    fn clean_cut(&self) -> usize {
        let n = self.pending.len();
        let mut suffix_min = vec![None; n + 1];
        for i in (0..n).rev() {
            let k = display_key(&self.pending[i]);
            suffix_min[i] = Some(suffix_min[i + 1].map_or(k, |m: (u32, i32, u32)| m.min(k)));
        }
        let mut prefix_max = None;
        let mut cut = 0;
        let last = n.saturating_sub(PUBLISH_LAG);
        for (c, after) in suffix_min.iter().enumerate().take(last + 1).skip(1) {
            let k = display_key(&self.pending[c - 1]);
            let m = prefix_max.map_or(k, |m: (u32, i32, u32)| m.max(k));
            prefix_max = Some(m);
            if after.is_some_and(|s| m < s) {
                cut = c;
            }
        }
        cut
    }

    /// Publishes the pending frames up to the last clean cut (all of them
    /// when `all`), with their final display positions. Returns whether
    /// anything was published.
    fn publish_ready(&mut self, all: bool) -> bool {
        let cut = if all {
            self.pending.len()
        } else {
            self.clean_cut()
        };
        if cut == 0 {
            return false;
        }
        let base = self.display_to_decode.len() as u32;
        let mut batch: Vec<Pending> = self.pending.drain(..cut).collect();
        let mut order: Vec<usize> = (0..batch.len()).collect();
        order.sort_by_key(|&i| display_key(&batch[i]));
        for (rank, &i) in order.iter().enumerate() {
            batch[i].0.display_index = base + rank as u32;
        }
        self.display_to_decode
            .extend(order.iter().map(|&i| batch[i].0.decode_index));
        for (frame, _, _) in &batch {
            self.stats.add(frame);
        }
        let (frames, analysis): (Vec<_>, Vec<_>) =
            batch.into_iter().map(|(f, s, _)| (f, s)).unzip();
        self.frames.extend(frames);
        self.analysis.extend(analysis);
        true
    }

    fn snapshot(&self, props: Option<StreamProps>, complete: bool) -> StreamIndex {
        let props = props.unwrap_or_default();
        StreamIndex {
            codec: self.codec,
            info: StreamInfo {
                codec: self.codec,
                profile: props.profile,
                level: props.level,
                tier: props.tier,
                width: props.width,
                height: props.height,
                bit_depth: props.bit_depth,
                chroma_format: props.chroma_format,
                frame_rate: props.frame_rate,
                container: self.container.clone(),
                duration_ns: self.stats.duration_ns(),
            },
            frames: self.frames.clone(),
            analysis: self.analysis.clone(),
            display_to_decode: self.display_to_decode.clone(),
            qp_range: self.stats.qp_range,
            stats: self.stats.stats(),
            diagnostics: self.diags.clone(),
            source: self.source.clone(),
            video_bytes: self.stats.video_bytes,
            insertions: self.insertions.clone(),
            complete,
        }
    }
}

/// Whole-stream aggregates, accumulated as frames are published.
struct StatsAcc {
    type_counts: [u32; 3],
    type_bytes: [u64; 3],
    video_bytes: u64,
    qp_range: (i8, i8),
    prev_keyframe: Option<u32>,
    /// min, max, sum, count of keyframe distances.
    distances: (u32, u32, u64, u32),
    pts_start: Option<i64>,
    pts_end: Option<i64>,
}

impl Default for StatsAcc {
    fn default() -> Self {
        Self {
            type_counts: [0; 3],
            type_bytes: [0; 3],
            video_bytes: 0,
            qp_range: (i8::MAX, i8::MIN),
            prev_keyframe: None,
            distances: (u32::MAX, 0, 0, 0),
            pts_start: None,
            pts_end: None,
        }
    }
}

impl StatsAcc {
    /// Adds one frame; frames arrive in decode order.
    fn add(&mut self, f: &FrameRecord) {
        let k = match f.frame_type {
            FrameType::I => 0,
            FrameType::P => 1,
            FrameType::B => 2,
        };
        self.type_counts[k] += 1;
        self.type_bytes[k] += f.size_bytes as u64;
        self.video_bytes += f.size_bytes as u64;
        self.qp_range = (
            self.qp_range.0.min(f.slice_qp),
            self.qp_range.1.max(f.slice_qp),
        );
        if f.is_keyframe {
            if let Some(p) = self.prev_keyframe {
                let d = f.decode_index - p;
                let (min, max, sum, n) = &mut self.distances;
                *min = (*min).min(d);
                *max = (*max).max(d);
                *sum += d as u64;
                *n += 1;
            }
            self.prev_keyframe = Some(f.decode_index);
        }
        if let Some(pts) = f.pts {
            self.pts_start = Some(self.pts_start.map_or(pts, |s| s.min(pts)));
            let end = pts + f.duration.unwrap_or(0);
            self.pts_end = Some(self.pts_end.map_or(end, |e| e.max(end)));
        }
    }

    fn stats(&self) -> StreamStats {
        let (min, max, sum, n) = self.distances;
        StreamStats {
            type_counts: self.type_counts,
            type_bytes: self.type_bytes,
            keyframe_distance: (n > 0).then(|| (min, sum as f32 / n as f32, max)),
        }
    }

    /// Containers rarely start at 0 (MPEG-TS PTS, edit lists).
    fn duration_ns(&self) -> Option<i64> {
        self.pts_end.zip(self.pts_start).map(|(e, s)| e - s)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture(name: &str) -> std::path::PathBuf {
        std::path::Path::new(concat!(env!("CARGO_MANIFEST_DIR"), "/../../fixtures")).join(name)
    }

    /// Publishing at every possible clean cut must give exactly the display
    /// order of sorting the whole stream at once.
    #[test]
    fn progressive_display_order_matches_full_sort() {
        for name in [
            "h264_prog.mp4",
            "h264_opengop.ts",
            "hevc_8bit.mp4",
            "hevc_10bit.mkv",
        ] {
            let (mut demuxer, analyzer) = open(&fixture(name).into()).unwrap();
            let analyzer = Mutex::new(analyzer);
            let mut snapshots = Vec::new();
            run_pass(
                &mut demuxer,
                &analyzer,
                &fixture(name).into(),
                Some(Duration::ZERO),
                &AtomicBool::new(false),
                &mut |_| {},
                &mut |s| snapshots.push(s),
            )
            .unwrap();
            assert!(snapshots.len() > 2, "{name}: published in batches");
            let last = snapshots.last().unwrap();
            assert!(last.complete);

            let mut period = 0;
            let mut keys: Vec<(u32, i32, u32)> = Vec::new();
            for f in last.frames.iter() {
                if f.is_idr && f.decode_index != 0 {
                    period += 1;
                }
                keys.push((period, f.poc, f.decode_index));
            }
            let mut expected: Vec<u32> = (0..keys.len() as u32).collect();
            expected.sort_by_key(|&i| keys[i as usize]);
            let actual: Vec<u32> = last.display_to_decode.iter().copied().collect();
            assert_eq!(actual, expected, "{name}");
            for (d, &i) in expected.iter().enumerate() {
                assert_eq!(last.frames[i as usize].display_index, d as u32, "{name}");
            }
            // Earlier snapshots are prefixes of the final one.
            for s in &snapshots {
                assert!(!s.frames.is_empty());
                for i in 0..s.frames.len() {
                    assert_eq!(s.frames[i].display_index, last.frames[i].display_index);
                }
            }
        }
    }
}
