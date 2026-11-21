//! The stream index: everything known about every frame after the index
//! pass, and what it takes to read each frame back from the file.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;

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

/// Parameter sets to insert into a keyframe that has none in-band, at
/// byte offset `at` of its Annex B access unit.
#[derive(Debug, Clone)]
pub struct ParamInsertion {
    pub at: u32,
    pub bytes: Arc<[u8]>,
}

/// Result of indexing one video stream.
///
/// NAL offsets in `analysis` are relative to each frame's Annex B access
/// unit as [`crate::source::AuReader`] produces it (insertions applied).
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
    /// The analyzed file; decoding reads the frames back from it.
    pub path: PathBuf,
    /// Total size of the video track's frames in the file.
    pub video_bytes: u64,
    /// Keyframes that get parameter sets inserted (decode index → what).
    pub insertions: HashMap<u32, ParamInsertion>,
    /// (file offset, size) of each frame's packet → decode index.
    pub(crate) packet_frames: HashMap<(u64, u32), u32>,
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
    /// The frame whose packet sits at `file_offset` with `size` bytes.
    pub(crate) fn frame_at(&self, file_offset: Option<u64>, size: u32) -> Option<u32> {
        self.packet_frames.get(&(file_offset?, size)).copied()
    }

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
