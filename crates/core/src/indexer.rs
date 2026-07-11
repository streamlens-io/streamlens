//! The indexing pass: demux + parse the whole file once, feed every access
//! unit through the syntax analyzer, write the normalized elementary stream
//! to a cache file, and assemble the [`StreamIndex`].

use std::fs::File;
use std::io::{BufWriter, Write};
use std::path::{Path, PathBuf};

use gstreamer as gst;
use gstreamer::prelude::*;
use streamlens_syntax::diag::Diagnostics;
use streamlens_syntax::h264::H264Analyzer;
use streamlens_syntax::h265::H265Analyzer;
use streamlens_syntax::{AuAnalyzer, AuSummary};
use streamlens_types::{Codec, FrameRecord, StreamInfo};

use crate::index::{IndexProgress, StreamIndex};
use crate::media;
use crate::Error;

/// Indexes a media file. Blocking; run on a worker thread.
pub struct Indexer {
    /// Directory for the elementary-stream cache (defaults to the system
    /// temp dir).
    pub cache_dir: PathBuf,
}

impl Default for Indexer {
    fn default() -> Self {
        Self {
            cache_dir: std::env::temp_dir(),
        }
    }
}

impl Indexer {
    /// Runs the index pass. `progress` is called from the indexing thread
    /// roughly every 64 frames. Also returns the analyzer with its final
    /// parameter-set state (used to re-parse AUs for the Headers panel).
    pub fn index(
        &self,
        path: &Path,
        mut progress: impl FnMut(IndexProgress),
    ) -> Result<(StreamIndex, Box<dyn AuAnalyzer>), Error> {
        let (pipeline, appsink) = media::build_au_pipeline(path)?;
        pipeline.set_state(gst::State::Playing)?;
        // Ensure teardown also on early error paths.
        let result = self.pull_all(path, &pipeline, &appsink, &mut progress);
        let _ = pipeline.set_state(gst::State::Null);
        result
    }

    fn pull_all(
        &self,
        path: &Path,
        pipeline: &gst::Pipeline,
        appsink: &gstreamer_app::AppSink,
        progress: &mut impl FnMut(IndexProgress),
    ) -> Result<(StreamIndex, Box<dyn AuAnalyzer>), Error> {
        let stem = path
            .file_stem()
            .map(|s| s.to_string_lossy().into_owned())
            .unwrap_or_else(|| "stream".into());
        // pid + counter: several sessions (or parallel tests) may index
        // files with the same stem concurrently.
        static SEQ: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        let seq = SEQ.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let es_cache_path = self
            .cache_dir
            .join(format!("streamlens-{stem}-{}-{seq}.es", std::process::id()));
        let mut es_writer = BufWriter::new(File::create(&es_cache_path)?);

        let mut codec: Option<Codec> = None;
        let mut analyzer: Option<Box<dyn AuAnalyzer>> = None;
        let mut diags = Diagnostics::default();
        let mut frames: Vec<FrameRecord> = Vec::new();
        let mut analysis: Vec<AuSummary> = Vec::new();
        let mut es_offset = 0u64;
        let mut duration: Option<gst::ClockTime> = None;

        loop {
            let sample = match appsink.try_pull_sample(gst::ClockTime::from_mseconds(100)) {
                Some(s) => s,
                None => {
                    media::check_bus_error(pipeline)?;
                    if appsink.is_eos() {
                        break;
                    }
                    continue;
                }
            };

            if codec.is_none() {
                let caps = sample
                    .caps()
                    .ok_or_else(|| Error::Pipeline("sample without caps".into()))?;
                let c = media::codec_from_caps(caps)?;
                analyzer = Some(make_analyzer(c)?);
                codec = Some(c);
                duration = pipeline
                    .query_duration::<gst::ClockTime>()
                    .filter(|d| !d.is_zero());
            }

            let Some(buffer) = sample.buffer() else {
                continue;
            };
            let map = buffer
                .map_readable()
                .map_err(|_| Error::Pipeline("unmappable buffer".into()))?;
            let au = map.as_slice();

            let decode_index = frames.len() as u32;
            let Some(summary) = analyzer.as_mut().expect("analyzer set with codec").analyze(
                decode_index,
                au,
                &mut diags,
            ) else {
                // Parameter sets only (no picture): bytes still belong to the
                // ES so offsets stay contiguous; attribute them to the next
                // frame by simply writing them through.
                es_writer.write_all(au)?;
                es_offset += au.len() as u64;
                continue;
            };

            es_writer.write_all(au)?;
            frames.push(FrameRecord {
                decode_index,
                display_index: decode_index, // finalized below
                byte_offset: es_offset,
                size_bytes: au.len() as u32,
                frame_type: summary.frame_type,
                is_keyframe: summary.is_keyframe
                    || !buffer.flags().contains(gst::BufferFlags::DELTA_UNIT),
                is_idr: summary.is_idr,
                is_rasl: summary.is_rasl,
                pts: buffer.pts().map(|t| t.nseconds() as i64),
                dts: buffer.dts().map(|t| t.nseconds() as i64),
                duration: buffer.duration().map(|t| t.nseconds() as i64),
                poc: summary.poc,
                temporal_id: summary.temporal_id,
                slice_qp: summary.slice_qp.clamp(i8::MIN as i32, i8::MAX as i32) as i8,
                slice_count: summary.slice_count,
                ref_frames: summary.ref_frames.clone(),
            });
            analysis.push(summary);
            es_offset += au.len() as u64;

            if frames.len().is_multiple_of(64) {
                let fraction = duration.and_then(|d| {
                    let pts = frames.last()?.pts?;
                    Some((pts as f64 / d.nseconds() as f64) as f32)
                });
                progress(IndexProgress {
                    frames_indexed: frames.len() as u32,
                    fraction,
                });
            }
        }
        es_writer.flush()?;

        // Everything downstream assumes at least one frame (frames.len()-1
        // arithmetic, keyframe lookups); an empty index must be an error.
        let (Some(codec), Some(analyzer)) = (codec, analyzer) else {
            std::fs::remove_file(&es_cache_path).ok();
            return Err(Error::NoVideoStream(path.to_string_lossy().into_owned()));
        };
        if frames.is_empty() {
            std::fs::remove_file(&es_cache_path).ok();
            return Err(Error::Pipeline(format!(
                "no decodable video frames found in {}",
                path.display()
            )));
        }

        let display_to_decode = assign_display_order(&mut frames);
        let props = analyzer.stream_props().unwrap_or_default();
        let info = stream_info(codec, &frames, props, path);
        let qp_range = frames.iter().fold((i8::MAX, i8::MIN), |(lo, hi), f| {
            (lo.min(f.slice_qp), hi.max(f.slice_qp))
        });
        let stats = stream_stats(&frames);

        Ok((
            StreamIndex {
                codec,
                info,
                frames,
                analysis,
                display_to_decode,
                qp_range,
                stats,
                diagnostics: diags,
                es_cache_path,
                es_size: es_offset,
            },
            analyzer,
        ))
    }
}

fn make_analyzer(codec: Codec) -> Result<Box<dyn AuAnalyzer>, Error> {
    match codec {
        Codec::H264 => Ok(Box::new(H264Analyzer::new())),
        Codec::H265 => Ok(Box::new(H265Analyzer::new())),
    }
}

/// Display order: within each IDR period, order by POC (decode order breaks
/// ties); periods are sequential. Returns the display→decode table.
fn assign_display_order(frames: &mut [FrameRecord]) -> Vec<u32> {
    let mut period = 0u32;
    let periods: Vec<u32> = frames
        .iter()
        .enumerate()
        .map(|(i, f)| {
            if f.is_idr && i != 0 {
                period += 1;
            }
            period
        })
        .collect();
    let mut order: Vec<u32> = (0..frames.len() as u32).collect();
    order.sort_by_key(|&i| (periods[i as usize], frames[i as usize].poc, i));
    for (display_index, &decode_index) in order.iter().enumerate() {
        frames[decode_index as usize].display_index = display_index as u32;
    }
    order
}

fn stream_stats(frames: &[FrameRecord]) -> crate::index::StreamStats {
    let mut stats = crate::index::StreamStats::default();
    let mut prev_key = None;
    let (mut dist_min, mut dist_max, mut dist_sum, mut dist_n) = (u32::MAX, 0u32, 0u64, 0u32);
    for f in frames {
        let k = match f.frame_type {
            streamlens_types::FrameType::I => 0,
            streamlens_types::FrameType::P => 1,
            streamlens_types::FrameType::B => 2,
        };
        stats.type_counts[k] += 1;
        stats.type_bytes[k] += f.size_bytes as u64;
        if f.is_keyframe {
            if let Some(p) = prev_key {
                let d = f.decode_index - p;
                dist_min = dist_min.min(d);
                dist_max = dist_max.max(d);
                dist_sum += d as u64;
                dist_n += 1;
            }
            prev_key = Some(f.decode_index);
        }
    }
    if dist_n > 0 {
        stats.keyframe_distance = Some((dist_min, dist_sum as f32 / dist_n as f32, dist_max));
    }
    stats
}

fn stream_info(
    codec: Codec,
    frames: &[FrameRecord],
    props: streamlens_syntax::analyzer::StreamProps,
    path: &Path,
) -> StreamInfo {
    let duration_ns = frames
        .iter()
        .filter_map(|f| Some(f.pts? + f.duration.unwrap_or(0)))
        .max();
    StreamInfo {
        codec,
        profile: props.profile,
        level: props.level,
        tier: props.tier,
        width: props.width,
        height: props.height,
        bit_depth: props.bit_depth,
        chroma_format: props.chroma_format,
        frame_rate: props.frame_rate,
        container: path
            .extension()
            .map(|e| e.to_string_lossy().into_owned())
            .unwrap_or_default(),
        duration_ns,
        hrd: props.hrd,
    }
}
