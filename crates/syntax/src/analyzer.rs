//! Codec-agnostic access-unit analysis driven by the indexer.

use std::collections::BTreeMap;

use streamlens_types::FrameType;

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

/// Everything the syntax layer learns about one access unit.
#[derive(Debug, Clone)]
pub struct AuSummary {
    pub frame_type: FrameType,
    pub is_idr: bool,
    /// Random access point: IDR, CRA/BLA (H.265) or recovery point SEI.
    pub is_keyframe: bool,
    /// Leading picture that may reference the GOP before its keyframe
    /// (undecodable when decode starts at that keyframe): H.265 RASL, or an
    /// H.264 picture after a recovery point keyframe with a lower POC.
    pub is_rasl: bool,
    pub poc: i32,
    pub temporal_id: u8,
    /// SliceQPY of the first slice.
    pub slice_qp: i32,
    pub slice_count: u16,
    /// Decode-order indices of the reference pictures in L0/L1 of the first
    /// slice.
    pub ref_frames: [Vec<u32>; 2],
    pub nals: Vec<NalSummary>,
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

    /// Makes a keyframe AU decodable on its own: when it lacks in-band
    /// parameter sets, returns the AU with every parameter set seen so far
    /// inserted (after a leading access unit delimiter) and updates
    /// `summary.nals` to the new layout. `None` when the AU needs no change.
    fn insert_parameter_sets(&self, au: &[u8], summary: &mut AuSummary) -> Option<Vec<u8>>;

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
}

/// Raw parameter-set NALs seen so far, for re-insertion before keyframes.
/// Keyed by (NAL type, id): ascending NAL type is also the required order
/// (H.264 SPS 7 < PPS 8; H.265 VPS 32 < SPS 33 < PPS 34).
#[derive(Default)]
pub(crate) struct ParameterSetStore {
    nals: BTreeMap<(u8, u32), Vec<u8>>,
}

impl ParameterSetStore {
    pub fn insert(&mut self, raw_type: u8, id: u32, nal: &[u8]) {
        self.nals.insert((raw_type, id), nal.to_vec());
    }

    /// See [`AuAnalyzer::insert_parameter_sets`]. `required` lists the
    /// parameter-set NAL types a standalone keyframe needs; `describe`
    /// gives the name and VCL flag of a NAL type.
    pub fn splice(
        &self,
        au: &[u8],
        summary: &mut AuSummary,
        required: &[u8],
        aud_type: u8,
        describe: impl Fn(u8) -> (&'static str, bool),
    ) -> Option<Vec<u8>> {
        if !summary.is_keyframe
            || self.nals.is_empty()
            || required
                .iter()
                .all(|t| summary.nals.iter().any(|n| n.raw_type == *t))
        {
            return None;
        }
        let at = summary
            .nals
            .first()
            .filter(|n| n.raw_type == aud_type)
            .map_or(0, |n| n.offset + n.size)
            .min(au.len());

        let mut block = Vec::new();
        let mut inserted = Vec::with_capacity(self.nals.len());
        for (&(raw_type, _), nal) in &self.nals {
            let (name, is_vcl) = describe(raw_type);
            inserted.push(NalSummary {
                offset: at + block.len(),
                size: 4 + nal.len(),
                raw_type,
                name,
                is_vcl,
            });
            block.extend_from_slice(&[0, 0, 0, 1]);
            block.extend_from_slice(nal);
        }

        let split = summary.nals.partition_point(|n| n.offset < at);
        for n in &mut summary.nals[split..] {
            n.offset += block.len();
        }
        summary.nals.splice(split..split, inserted);

        let mut out = Vec::with_capacity(au.len() + block.len());
        out.extend_from_slice(&au[..at]);
        out.extend_from_slice(&block);
        out.extend_from_slice(&au[at..]);
        Some(out)
    }
}
