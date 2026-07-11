//! Per-frame index records built during the indexing pass.

use serde::{Deserialize, Serialize};

/// Video codec of an elementary stream.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum Codec {
    H264,
    H265,
}

/// Dominant slice type of a frame.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum FrameType {
    I,
    P,
    B,
}

/// One frame (access unit) as seen by the indexer. Kept compact: streams can
/// have hundreds of thousands of frames.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FrameRecord {
    /// Position in decode (stream) order, 0-based.
    pub decode_index: u32,
    /// Position in display order, 0-based. Filled once the reorder window
    /// around this frame is complete.
    pub display_index: u32,
    /// Byte offset of the first NAL of this access unit in the elementary
    /// stream (Annex B, as delivered by the parser).
    pub byte_offset: u64,
    /// Total access-unit size in bytes.
    pub size_bytes: u32,
    pub frame_type: FrameType,
    /// Random access point (IDR / CRA / BLA, or recovery point).
    pub is_keyframe: bool,
    pub is_idr: bool,
    /// H.265 RASL leading picture: references the GOP before its CRA, so a
    /// decode started at the CRA cannot produce it (decoders drop it).
    pub is_rasl: bool,
    /// Presentation timestamp in nanoseconds.
    pub pts: Option<i64>,
    /// Decode timestamp in nanoseconds.
    pub dts: Option<i64>,
    pub duration: Option<i64>,
    pub poc: i32,
    pub temporal_id: u8,
    /// Slice-header QP of the first slice (SliceQPY). Per-block QP lives in
    /// `FrameAnalysis`.
    pub slice_qp: i8,
    /// Number of slices in the access unit.
    pub slice_count: u16,
    /// Decode-order indices of reference frames used by this frame,
    /// list 0 then list 1.
    pub ref_frames: [Vec<u32>; 2],
}

impl FrameRecord {
    pub fn is_ref_of(&self, other: &FrameRecord) -> bool {
        other.ref_frames[0].contains(&self.decode_index)
            || other.ref_frames[1].contains(&self.decode_index)
    }
}
