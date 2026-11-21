//! The indexing pass: demux + parse the whole file once, feed every access
//! unit through the syntax analyzer, and assemble the [`StreamIndex`]. The
//! video is not copied: decoding reads it back from the file (see
//! [`crate::source`]).

use std::collections::HashMap;
use std::path::Path;
use std::sync::Arc;

use streamlens_media::Demuxer;
use streamlens_syntax::diag::Diagnostics;
use streamlens_syntax::h264::H264Analyzer;
use streamlens_syntax::h265::H265Analyzer;
use streamlens_syntax::{AuAnalyzer, AuSummary};
use streamlens_types::{Codec, FrameRecord, StreamInfo};

use crate::index::{IndexProgress, ParamInsertion, StreamIndex};
use crate::Error;

/// Indexes a media file. Blocking; run on a worker thread.
#[derive(Default)]
pub struct Indexer;

impl Indexer {
    /// Runs the index pass. `progress` is called from the indexing thread
    /// roughly every 64 frames. Also returns the analyzer with its final
    /// parameter-set state (used to re-parse AUs for the Headers panel).
    pub fn index(
        &self,
        path: &Path,
        mut progress: impl FnMut(IndexProgress),
    ) -> Result<(StreamIndex, Box<dyn AuAnalyzer>), Error> {
        let mut demuxer = Demuxer::open(path).map_err(|e| match e {
            streamlens_media::Error::NoVideoStream => {
                Error::NoVideoStream(path.to_string_lossy().into_owned())
            }
            e => e.into(),
        })?;
        let codec = demuxer.codec();
        let mut analyzer = make_analyzer(codec);

        let mut pass = IndexPass::default();
        pass.run(&mut demuxer, analyzer.as_mut(), &mut progress)?;
        if pass.frames.is_empty() {
            // Everything downstream assumes at least one frame
            // (frames.len()-1 arithmetic, keyframe lookups).
            return Err(Error::Pipeline(format!(
                "no decodable video frames found in {}",
                path.display()
            )));
        }
        let IndexPass {
            mut frames,
            analysis,
            diags,
            insertions,
            packet_frames,
        } = pass;

        let display_to_decode = assign_display_order(&mut frames);
        let props = analyzer.stream_props().unwrap_or_default();
        let info = stream_info(codec, &frames, props, path);
        let qp_range = frames.iter().fold((i8::MAX, i8::MIN), |(lo, hi), f| {
            (lo.min(f.slice_qp), hi.max(f.slice_qp))
        });
        let stats = stream_stats(&frames);
        let video_bytes = frames.iter().map(|f| f.size_bytes as u64).sum();

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
                path: path.to_path_buf(),
                video_bytes,
                insertions,
                packet_frames,
            },
            analyzer,
        ))
    }
}

#[derive(Default)]
struct IndexPass {
    frames: Vec<FrameRecord>,
    analysis: Vec<AuSummary>,
    diags: Diagnostics,
    insertions: HashMap<u32, ParamInsertion>,
    packet_frames: HashMap<(u64, u32), u32>,
}

impl IndexPass {
    /// Analyzes every access unit. Keyframes that lack in-band parameter
    /// sets get an insertion recorded, so a decode can start at any
    /// keyframe.
    fn run(
        &mut self,
        demuxer: &mut Demuxer,
        analyzer: &mut dyn AuAnalyzer,
        progress: &mut impl FnMut(IndexProgress),
    ) -> Result<(), Error> {
        // MP4/MKV keep the parameter sets in the track header; the analyzer
        // needs them, but they belong to no frame.
        let config = demuxer.config_nals();
        if !config.is_empty() {
            analyzer.analyze(0, config, &mut self.diags);
        }

        // Insertions repeat the same parameter sets; share the bytes.
        let mut last_block: Option<Arc<[u8]>> = None;
        while let Some(au) = demuxer.read()? {
            let decode_index = self.frames.len() as u32;
            // Packets without a picture (parameter sets only) are no frame;
            // the source reader passes them on with the next one.
            let Some(mut summary) = analyzer.analyze(decode_index, au.data, &mut self.diags) else {
                continue;
            };
            let file_offset = au.file_offset.ok_or_else(|| {
                Error::Pipeline("the container reports no packet positions".into())
            })?;
            if self
                .packet_frames
                .insert((file_offset, au.coded_size), decode_index)
                .is_some()
            {
                return Err(Error::Pipeline(format!(
                    "two packets at file offset {file_offset:#x} (laced video?)"
                )));
            }
            summary.is_keyframe |= au.keyframe;
            if let Some(ins) = analyzer.parameter_sets_to_insert(au.data, &mut summary) {
                let bytes = match &last_block {
                    Some(b) if **b == *ins.bytes => b.clone(),
                    _ => Arc::from(ins.bytes),
                };
                last_block = Some(bytes.clone());
                self.insertions.insert(
                    decode_index,
                    ParamInsertion {
                        at: ins.at as u32,
                        bytes,
                    },
                );
            }

            self.frames.push(FrameRecord {
                decode_index,
                display_index: decode_index, // finalized below
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
            });
            let fraction = au.progress;
            self.analysis.push(summary);

            if self.frames.len().is_multiple_of(64) {
                progress(IndexProgress {
                    frames_indexed: self.frames.len() as u32,
                    fraction,
                });
            }
        }
        Ok(())
    }
}

fn make_analyzer(codec: Codec) -> Box<dyn AuAnalyzer> {
    match codec {
        Codec::H264 => Box::new(H264Analyzer::new()),
        Codec::H265 => Box::new(H265Analyzer::new()),
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
    // Containers rarely start at 0 (MPEG-TS PTS, edit lists).
    let end = frames
        .iter()
        .filter_map(|f| Some(f.pts? + f.duration.unwrap_or(0)))
        .max();
    let start = frames.iter().filter_map(|f| f.pts).min();
    let duration_ns = end.zip(start).map(|(e, s)| e - s);
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
    }
}
