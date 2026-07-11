//! Stream-level info and timeline models (DPB, HRD).

use serde::{Deserialize, Serialize};

use crate::frame::Codec;

/// Summary of an opened video stream, shown in the Stream panel.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StreamInfo {
    pub codec: Codec,
    pub profile: String,
    pub level: String,
    pub tier: Option<String>,
    pub width: u32,
    pub height: u32,
    pub bit_depth: u8,
    pub chroma_format: String,
    /// Frames per second as (num, den), when declared.
    pub frame_rate: Option<(u32, u32)>,
    pub container: String,
    pub duration_ns: Option<i64>,
    /// Declared HRD parameters (bitrate bits/s, CPB size bits), when the
    /// stream carries them.
    pub hrd: Option<(u64, u64)>,
}

/// How a picture is held in the decoded picture buffer.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum DpbMarking {
    ShortTerm,
    LongTerm,
    /// Decoded but only awaiting output (no longer used for reference).
    OutputOnly,
}

/// One picture in a DPB snapshot.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DpbEntry {
    pub decode_index: u32,
    pub poc: i32,
    pub marking: DpbMarking,
}

/// DPB contents immediately after decoding a given frame.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct DpbSnapshot {
    pub entries: Vec<DpbEntry>,
}

/// Coded-picture-buffer event for the HRD/buffer analyzer: the buffer fills
/// continuously at `arrival_rate` and instantaneously drains by
/// `removed_bits` when a picture is decoded.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HrdEvent {
    pub decode_index: u32,
    /// Time the picture's final bit arrives in the CPB, in 90 kHz ticks
    /// scaled to nanoseconds.
    pub final_arrival_ns: i64,
    /// Nominal removal (decode) time.
    pub removal_ns: i64,
    pub removed_bits: u64,
}

/// HRD model parameters extracted from the stream (or reconstructed from
/// level limits when absent).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HrdModel {
    /// CPB size in bits.
    pub cpb_size: u64,
    /// Input bitrate in bits/s.
    pub bitrate: u64,
    /// Buffer fullness at the first picture's removal time.
    pub initial_fullness_bits: u64,
    /// True when parameters came from SPS/SEI rather than level fallback.
    pub declared: bool,
    /// True when the initial fullness came from a buffering-period SEI
    /// rather than the 90%-of-CPB assumption.
    pub initial_fullness_declared: bool,
    /// True when removal times came from container DTS rather than being
    /// synthesized from the frame rate.
    pub timing_declared: bool,
    pub events: Vec<HrdEvent>,
}
