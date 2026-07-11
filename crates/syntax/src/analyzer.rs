//! Codec-agnostic access-unit analysis driven by the indexer.

use streamlens_types::{DpbSnapshot, FrameType};

use crate::diag::Diagnostics;

/// One NAL unit inside an access unit (Stream Viewer row).
#[derive(Debug, Clone)]
pub struct NalSummary {
    /// Offset of the start code within the AU buffer.
    pub offset: usize,
    /// Size including start code.
    pub size: usize,
    pub raw_type: u8,
    pub name: &'static str,
    pub is_vcl: bool,
}

/// Timing values from buffering period / pic timing SEI, feeding the HRD
/// model. All values are in units of the HRD clock tick.
#[derive(Debug, Clone, Default)]
pub struct HrdTiming {
    /// From buffering period SEI: initial_cpb_removal_delay of CPB 0.
    pub initial_cpb_removal_delay: Option<u32>,
    /// From pic timing SEI.
    pub cpb_removal_delay: Option<u32>,
    pub dpb_output_delay: Option<u32>,
}

/// Everything the syntax layer learns about one access unit.
#[derive(Debug, Clone)]
pub struct AuSummary {
    pub frame_type: FrameType,
    pub is_idr: bool,
    /// Random access point: IDR, CRA/BLA (H.265) or recovery point SEI.
    pub is_keyframe: bool,
    /// H.265 RASL leading picture (undecodable when decode starts at its
    /// associated CRA). Always false for H.264.
    pub is_rasl: bool,
    pub poc: i32,
    pub temporal_id: u8,
    /// SliceQPY of the first slice.
    pub slice_qp: i32,
    pub slice_count: u16,
    /// Decode-order indices of the reference pictures in L0/L1 of the first
    /// slice.
    pub ref_frames: [Vec<u32>; 2],
    /// DPB contents after this picture's marking.
    pub dpb: DpbSnapshot,
    pub nals: Vec<NalSummary>,
    pub hrd: HrdTiming,
}

/// Stateful per-stream analyzer: feed access units in decode order.
///
/// Implementations keep parameter-set, POC and DPB state; one instance per
/// stream. New codecs plug in by implementing this trait.
pub trait AuAnalyzer: Send {
    /// Analyzes one Annex B access unit. Returns `None` when the AU contains
    /// no picture (parameter sets only). Malformed NALs are reported through
    /// `diags`, not errors; the analyzer recovers and continues.
    fn analyze(
        &mut self,
        decode_index: u32,
        au: &[u8],
        diags: &mut Diagnostics,
    ) -> Option<AuSummary>;

    /// Properties of the active sequence, once parameter sets were seen.
    fn stream_props(&self) -> Option<StreamProps> {
        None
    }

    /// Re-parses one AU with field recording for the Headers panel, using
    /// the current parameter-set state. Bit offsets in the trees are
    /// RBSP-relative to each NAL.
    fn record_au(&self, au: &[u8]) -> Vec<RecordedNal> {
        let _ = au;
        Vec::new()
    }
}

/// One NAL of an AU with its recorded syntax field tree.
#[derive(Debug, Clone)]
pub struct RecordedNal {
    pub name: &'static str,
    /// Start-code offset within the AU.
    pub offset: usize,
    pub size: usize,
    pub tree: crate::bitread::FieldTree,
    /// Parse error, if the NAL could not be fully parsed (tree holds the
    /// fields read up to the error).
    pub error: Option<String>,
}

/// Stream-level properties from the active parameter sets.
#[derive(Debug, Clone, Default)]
pub struct StreamProps {
    pub profile: String,
    pub level: String,
    pub tier: Option<String>,
    pub width: u32,
    pub height: u32,
    pub bit_depth: u8,
    pub chroma_format: String,
    pub frame_rate: Option<(u32, u32)>,
    /// Declared HRD parameters of scheduler 0: (bitrate bits/s, CPB bits).
    pub hrd: Option<(u64, u64)>,
}
