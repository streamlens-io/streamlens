//! The stream index: everything known about every frame after the index
//! pass, plus the normalized elementary-stream cache it was built from.

use std::path::PathBuf;

use streamlens_syntax::diag::Diagnostics;
use streamlens_syntax::AuSummary;
use streamlens_types::{Codec, FrameRecord, StreamInfo};

/// Progress reported by the indexer while it runs.
#[derive(Debug, Clone)]
pub struct IndexProgress {
    pub frames_indexed: u32,
    /// 0.0..=1.0 when the source duration is known.
    pub fraction: Option<f32>,
}

/// Result of indexing one video stream.
///
/// `es_offset`/`es_size` in each [`FrameRecord`] address the **normalized
/// elementary stream** written to `es_cache_path` (Annex B, one AU after
/// another, parameter sets in front of keyframes). NAL offsets, header
/// re-parses and the decode service address that file, independent of the
/// original container; `size_bytes`/`file_offset` describe the frame in the
/// analyzed file.
#[derive(Debug)]
pub struct StreamIndex {
    pub codec: Codec,
    pub info: StreamInfo,
    pub frames: Vec<FrameRecord>,
    /// Per-frame syntax analysis (same order as `frames`).
    pub analysis: Vec<AuSummary>,
    /// Decode index at each display position (inverse of
    /// `FrameRecord::display_index`). Precomputed: the chart, ruler and
    /// playback look this up per bar/frame, and an O(n) scan there melts
    /// the UI on six-figure frame counts.
    pub display_to_decode: Vec<u32>,
    /// (min, max) slice QP across the stream, for chart scaling.
    pub qp_range: (i8, i8),
    /// Whole-stream aggregates for the Stats panel (computed once here so
    /// the panel doesn't rescan every frame record per redraw).
    pub stats: StreamStats,
    pub diagnostics: Diagnostics,
    pub es_cache_path: PathBuf,
    /// Total size of the normalized elementary stream in bytes.
    pub es_size: u64,
}

/// Whole-stream aggregates shown in the Stats panel.
#[derive(Debug, Clone, Default)]
pub struct StreamStats {
    /// Frame count and total coded bytes per type, indexed I/P/B.
    pub type_counts: [u32; 3],
    pub type_bytes: [u64; 3],
    /// (min, avg, max) distance between random access points, when the
    /// stream has at least two.
    pub keyframe_distance: Option<(u32, f32, u32)>,
}

impl StreamIndex {
    /// Decode-order index of the closest random access point at or before
    /// `decode_index`.
    pub fn keyframe_before(&self, decode_index: u32) -> u32 {
        self.frames[..=decode_index as usize]
            .iter()
            .rev()
            .find(|f| f.is_keyframe)
            .map(|f| f.decode_index)
            .unwrap_or(0)
    }

    /// True for leading (RASL) pictures whose reference GOP precedes the
    /// start of the stream — no decoder can produce them.
    pub fn is_undecodable_leading(&self, decode_index: u32) -> bool {
        self.frames[decode_index as usize].is_rasl && self.keyframe_before(decode_index) == 0
    }

    /// Decode index shown at a display position.
    pub fn decode_for_display(&self, display_index: u32) -> Option<u32> {
        self.display_to_decode.get(display_index as usize).copied()
    }
}
