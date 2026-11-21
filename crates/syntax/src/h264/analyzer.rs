//! Stateful H.264 access-unit analyzer.

use std::collections::HashMap;

use streamlens_types::FrameType;

use crate::analyzer::{
    AuAnalyzer, AuSummary, NalSummary, ParameterSetStore, RecordedNal, StreamProps,
};
use crate::bitread::{BitReader, Recorder};
use crate::diag::{Diagnostics, Severity};
use crate::h264::sei::SeiPayload;
use crate::h264::slice::SliceType;
use crate::h264::{self, dpb::Dpb, poc::PocState, NalType, Pps, Sps};
use crate::nal::{iter_nals, unescape_rbsp};

#[derive(Default)]
pub struct H264Analyzer {
    sps: HashMap<u32, Sps>,
    pps: HashMap<u32, Pps>,
    /// SPS referenced by the last activated PPS (SEI parsing context).
    active_sps: Option<u32>,
    poc_state: PocState,
    dpb: Dpb,
    /// POC of the last non-IDR (recovery point) keyframe; pictures that
    /// follow it in decode order with a lower POC are its leading pictures.
    recovery_poc: Option<i32>,
    param_sets: ParameterSetStore,
}

impl H264Analyzer {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn sps(&self, id: u32) -> Option<&Sps> {
        self.sps.get(&id)
    }

    pub fn pps(&self, id: u32) -> Option<&Pps> {
        self.pps.get(&id)
    }
}

impl AuAnalyzer for H264Analyzer {
    fn analyze(
        &mut self,
        decode_index: u32,
        au: &[u8],
        diags: &mut Diagnostics,
    ) -> Option<AuSummary> {
        let mut nals = Vec::new();
        let mut first_slice: Option<(h264::NalHeader, h264::SliceHeader)> = None;
        let mut slice_count = 0u16;
        let mut recovery_point = false;

        for nal in iter_nals(au) {
            if nal.bytes.is_empty() {
                continue;
            }
            let raw_type = nal.bytes[0] & 0x1F;
            let nal_type = NalType::from_raw(raw_type);
            nals.push(NalSummary {
                offset: nal.start_code_offset,
                size: nal.bytes.len() + (nal.payload_offset - nal.start_code_offset),
                raw_type,
                name: nal_type.name(),
                is_vcl: nal_type.is_vcl(),
            });

            let (rbsp, _) = unescape_rbsp(nal.bytes);
            let mut r = BitReader::new(&rbsp);
            let header = match h264::parse_nal_header(&mut r) {
                Ok(h) => h,
                Err(e) => {
                    diags.push(Severity::Error, nal.start_code_offset as u64, e.to_string());
                    continue;
                }
            };

            let result: Result<(), crate::Error> = (|| {
                match header.nal_type {
                    NalType::Sps => {
                        let sps = h264::sps::parse_sps(&mut r)?;
                        self.active_sps.get_or_insert(sps.seq_parameter_set_id);
                        self.param_sets
                            .insert(raw_type, sps.seq_parameter_set_id, nal.bytes);
                        self.sps.insert(sps.seq_parameter_set_id, sps);
                    }
                    NalType::Pps => {
                        let pps = h264::pps::parse_pps(&mut r)?;
                        if !self.sps.contains_key(&pps.seq_parameter_set_id) {
                            return Err(crate::Error::MissingParameterSet {
                                what: "PPS",
                                referenced: "SPS",
                                id: pps.seq_parameter_set_id,
                            });
                        }
                        self.active_sps = Some(pps.seq_parameter_set_id);
                        self.param_sets
                            .insert(raw_type, pps.pic_parameter_set_id, nal.bytes);
                        self.pps.insert(pps.pic_parameter_set_id, pps);
                    }
                    NalType::Sei => {
                        let sps = self.active_sps.and_then(|id| self.sps.get(&id));
                        for msg in h264::sei::parse_sei(&mut r, sps)? {
                            if matches!(msg.payload, SeiPayload::RecoveryPoint { .. }) {
                                recovery_point = true;
                            }
                        }
                    }
                    NalType::SliceIdr | NalType::SliceNonIdr => {
                        slice_count += 1;
                        if first_slice.is_none() {
                            let sh = self.parse_first_slice(&mut r, header)?;
                            first_slice = Some((header, sh));
                        }
                    }
                    _ => {}
                }
                Ok(())
            })();
            if let Err(e) = result {
                diags.push(Severity::Error, nal.start_code_offset as u64, e.to_string());
            }
        }

        let (nal_header, sh) = first_slice?;
        let is_idr = nal_header.nal_type == NalType::SliceIdr;
        let is_ref = nal_header.nal_ref_idc != 0;
        let pps = self.pps.get(&sh.pic_parameter_set_id)?.clone();
        let sps = self.sps.get(&pps.seq_parameter_set_id)?.clone();
        self.active_sps = Some(pps.seq_parameter_set_id);

        let poc = self.poc_state.compute(&sps, &sh, is_idr, is_ref);
        let ref_frames = self.dpb.build_ref_lists(&sps, &sh, poc);
        let mmco5 = self
            .dpb
            .mark_picture(&sps, &sh, decode_index, poc, is_ref, is_idr);
        if mmco5 {
            self.poc_state.mmco5(0);
        }

        // Open GOP: leading pictures of a recovery point may reference the
        // GOP before it, so (like H.265 RASL) a decode started at the
        // recovery point cannot produce them.
        let is_keyframe = is_idr || recovery_point;
        let is_leading = !is_keyframe && self.recovery_poc.is_some_and(|k| poc < k);
        if is_keyframe {
            self.recovery_poc = (!is_idr).then_some(poc);
        } else if mmco5 {
            self.recovery_poc = None;
        }

        let frame_type = match sh.slice_type {
            SliceType::I | SliceType::Si => FrameType::I,
            SliceType::P | SliceType::Sp => FrameType::P,
            SliceType::B => FrameType::B,
        };

        Some(AuSummary {
            frame_type,
            is_idr,
            is_keyframe,
            is_rasl: is_leading,
            poc,
            temporal_id: 0,
            slice_qp: sh.slice_qp,
            slice_count,
            ref_frames,
            nals,
        })
    }

    fn insert_parameter_sets(&self, au: &[u8], summary: &mut AuSummary) -> Option<Vec<u8>> {
        self.param_sets.splice(au, summary, &[7, 8], 9, |t| {
            let n = NalType::from_raw(t);
            (n.name(), n.is_vcl())
        })
    }

    fn stream_props(&self) -> Option<StreamProps> {
        let sps = self.active_sps.and_then(|id| self.sps.get(&id))?;
        Some(StreamProps {
            profile: sps.profile_name().to_string(),
            level: format!("{}.{}", sps.level_idc / 10, sps.level_idc % 10),
            tier: None,
            width: sps.display_width(),
            height: sps.display_height(),
            bit_depth: sps.bit_depth_luma,
            chroma_format: match sps.chroma_format_idc {
                0 => "4:0:0",
                1 => "4:2:0",
                2 => "4:2:2",
                _ => "4:4:4",
            }
            .to_string(),
            frame_rate: sps.frame_rate(),
        })
    }

    fn record_au(&self, au: &[u8]) -> Vec<RecordedNal> {
        let mut out = Vec::new();
        for nal in iter_nals(au) {
            if nal.bytes.is_empty() {
                continue;
            }
            let nal_type = NalType::from_raw(nal.bytes[0] & 0x1F);
            let (rbsp, _) = unescape_rbsp(nal.bytes);
            let mut r = BitReader::with_sink(&rbsp, Recorder::default());
            let result: Result<(), crate::Error> = (|| {
                let header = h264::parse_nal_header(&mut r)?;
                match header.nal_type {
                    NalType::Sps => {
                        h264::sps::parse_sps(&mut r)?;
                    }
                    NalType::Pps => {
                        h264::pps::parse_pps(&mut r)?;
                    }
                    NalType::Sei => {
                        let sps = self.active_sps.and_then(|id| self.sps.get(&id));
                        h264::sei::parse_sei(&mut r, sps)?;
                    }
                    NalType::SliceIdr | NalType::SliceNonIdr => {
                        let mut peek = BitReader::new(r.remaining_data());
                        peek.ue("first_mb_in_slice")?;
                        peek.ue("slice_type")?;
                        let pps_id = peek.ue("pic_parameter_set_id")?;
                        let pps =
                            self.pps
                                .get(&pps_id)
                                .ok_or(crate::Error::MissingParameterSet {
                                    what: "slice",
                                    referenced: "PPS",
                                    id: pps_id,
                                })?;
                        let sps = self.sps.get(&pps.seq_parameter_set_id).ok_or(
                            crate::Error::MissingParameterSet {
                                what: "PPS",
                                referenced: "SPS",
                                id: pps.seq_parameter_set_id,
                            },
                        )?;
                        h264::slice::parse_slice_header(&mut r, header, sps, pps)?;
                    }
                    _ => {}
                }
                Ok(())
            })();
            out.push(RecordedNal {
                name: nal_type.name(),
                offset: nal.start_code_offset,
                size: nal.bytes.len() + (nal.payload_offset - nal.start_code_offset),
                tree: r.sink.finish(),
                error: result.err().map(|e| e.to_string()),
            });
        }
        out
    }
}

impl H264Analyzer {
    fn parse_first_slice(
        &self,
        r: &mut BitReader<'_, crate::bitread::NoRecord>,
        header: h264::NalHeader,
    ) -> Result<h264::SliceHeader, crate::Error> {
        // Peek pic_parameter_set_id: first_mb ue, slice_type ue, pps_id ue.
        // Cheapest correct approach: parse a throwaway prefix on a clone.
        let mut peek = BitReader::new(r.remaining_data());
        peek.ue("first_mb_in_slice")?;
        peek.ue("slice_type")?;
        let pps_id = peek.ue("pic_parameter_set_id")?;

        let pps = self
            .pps
            .get(&pps_id)
            .ok_or(crate::Error::MissingParameterSet {
                what: "slice",
                referenced: "PPS",
                id: pps_id,
            })?;
        let sps =
            self.sps
                .get(&pps.seq_parameter_set_id)
                .ok_or(crate::Error::MissingParameterSet {
                    what: "PPS",
                    referenced: "SPS",
                    id: pps.seq_parameter_set_id,
                })?;
        h264::slice::parse_slice_header(r, header, sps, pps)
    }
}
