//! Stateful H.265 access-unit analyzer.

use std::collections::HashMap;

use streamlens_types::FrameType;

use crate::analyzer::{
    AuAnalyzer, AuSummary, NalSummary, ParameterSetStore, RecordedNal, StreamProps,
};
use crate::bitread::{BitReader, Recorder};
use crate::diag::{Diagnostics, Severity};
use crate::h265::sei::SeiPayload;
use crate::h265::slice::SliceType;
use crate::h265::{self, poc::PocState, rps::Dpb, NalType, Pps, Sps, Vps};
use crate::nal::{iter_nals, unescape_rbsp};

#[derive(Default)]
pub struct H265Analyzer {
    vps: HashMap<u8, Vps>,
    sps: HashMap<u32, Sps>,
    pps: HashMap<u32, Pps>,
    /// SPS id of the most recently activated parameter set (SEI parsing context).
    active_sps_id: Option<u32>,
    poc_state: PocState,
    dpb: Dpb,
    param_sets: ParameterSetStore,
}

impl H265Analyzer {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn sps(&self, id: u32) -> Option<&Sps> {
        self.sps.get(&id)
    }

    pub fn pps(&self, id: u32) -> Option<&Pps> {
        self.pps.get(&id)
    }

    pub fn vps(&self, id: u8) -> Option<&Vps> {
        self.vps.get(&id)
    }
}

impl AuAnalyzer for H265Analyzer {
    fn analyze(
        &mut self,
        decode_index: u32,
        au: &[u8],
        diags: &mut Diagnostics,
    ) -> Option<AuSummary> {
        let _ = decode_index;
        let mut nals = Vec::new();
        let mut first_slice: Option<(h265::NalHeader, h265::SliceHeader)> = None;
        let mut slice_count = 0u16;
        let mut recovery_point = false;

        for nal in iter_nals(au) {
            if nal.bytes.len() < 2 {
                continue;
            }
            // HEVC NAL header is 2 bytes.
            let raw_type = (nal.bytes[0] >> 1) & 0x3F;
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
            let header = match h265::parse_nal_header(&mut r) {
                Ok(h) => h,
                Err(e) => {
                    diags.push(Severity::Error, nal.start_code_offset as u64, e.to_string());
                    continue;
                }
            };

            let result: Result<(), crate::Error> = (|| {
                match header.nal_type {
                    NalType::Vps => {
                        let v = h265::vps::parse_vps(&mut r)?;
                        self.param_sets.insert(raw_type, v.vps_id as u32, nal.bytes);
                        self.vps.insert(v.vps_id, v);
                    }
                    NalType::Sps => {
                        let s = h265::sps::parse_sps(&mut r)?;
                        let id = s.sps_seq_parameter_set_id;
                        self.active_sps_id.get_or_insert(id);
                        self.param_sets.insert(raw_type, id, nal.bytes);
                        self.sps.insert(id, s);
                    }
                    NalType::Pps => {
                        let p = h265::pps::parse_pps(&mut r)?;
                        if !self.sps.contains_key(&p.pps_seq_parameter_set_id) {
                            return Err(crate::Error::MissingParameterSet {
                                what: "PPS",
                                referenced: "SPS",
                                id: p.pps_seq_parameter_set_id,
                            });
                        }
                        self.active_sps_id = Some(p.pps_seq_parameter_set_id);
                        self.param_sets
                            .insert(raw_type, p.pps_pic_parameter_set_id, nal.bytes);
                        self.pps.insert(p.pps_pic_parameter_set_id, p);
                    }
                    NalType::PrefixSei | NalType::SuffixSei => {
                        let sps = self.active_sps_id.and_then(|id| self.sps.get(&id));
                        for msg in h265::sei::parse_sei(&mut r, sps)? {
                            if matches!(msg.payload, SeiPayload::RecoveryPoint { .. }) {
                                recovery_point = true;
                            }
                        }
                    }
                    t if t.is_vcl() => {
                        // Count ALL VCL NALs as slice_count (independent or not).
                        slice_count += 1;
                        // Prefer the first independent segment: dependent
                        // segments only carry placeholder header fields.
                        if first_slice.as_ref().is_none_or(|(_, sh)| sh.dependent) {
                            let sh = self.parse_first_slice(&mut r, header)?;
                            if first_slice.is_none() || !sh.dependent {
                                first_slice = Some((header, sh));
                            }
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

        let pps = self.pps.get(&sh.slice_pic_parameter_set_id)?.clone();
        let sps = self.sps.get(&pps.pps_seq_parameter_set_id)?.clone();
        self.active_sps_id = Some(pps.pps_seq_parameter_set_id);

        let nal_type = nal_header.nal_type;
        let temporal_id = nal_header.temporal_id;
        let is_irap = nal_type.is_irap();
        let is_idr = nal_type.is_idr();

        // Compute POC (8.3.1).
        let poc = self.poc_state.compute(
            nal_type,
            sh.pic_order_cnt_lsb,
            sps.log2_max_pic_order_cnt_lsb,
        );

        // Update prevTid0Pic state for qualifying pictures.
        if temporal_id == 0
            && !nal_type.is_rasl()
            && !nal_type.is_radl()
            && !nal_type.is_sub_layer_non_ref()
        {
            self.poc_state.update_prev_tid0(
                poc,
                sh.pic_order_cnt_lsb,
                sps.log2_max_pic_order_cnt_lsb,
            );
        }

        // Build ref lists and update DPB (8.3.2 / 8.3.4).
        let ref_frames = self.dpb.process(
            nal_type,
            poc,
            &sh.short_term_rps,
            &sh.lt_entries,
            sh.slice_type,
            &sh.num_ref_idx_active,
            &sh.ref_pic_list_mod,
            sps.max_dec_pic_buffering_value() + 1,
            sps.log2_max_pic_order_cnt_lsb,
        );

        let frame_type = match sh.slice_type {
            SliceType::I => FrameType::I,
            SliceType::P => FrameType::P,
            SliceType::B => FrameType::B,
        };

        Some(AuSummary {
            frame_type,
            is_idr,
            is_keyframe: is_irap || recovery_point,
            is_rasl: nal_type.is_rasl(),
            poc,
            temporal_id,
            slice_qp: sh.slice_qp,
            slice_count,
            ref_frames,
            nals,
        })
    }

    fn insert_parameter_sets(&self, au: &[u8], summary: &mut AuSummary) -> Option<Vec<u8>> {
        self.param_sets.splice(au, summary, &[32, 33, 34], 35, |t| {
            let n = NalType::from_raw(t);
            (n.name(), n.is_vcl())
        })
    }

    fn stream_props(&self) -> Option<StreamProps> {
        let sps = self.active_sps_id.and_then(|id| self.sps.get(&id))?;
        Some(StreamProps {
            profile: sps.profile_name().to_string(),
            level: sps.ptl.level_name(),
            tier: Some(sps.ptl.tier_name().to_string()),
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
            if nal.bytes.len() < 2 {
                continue;
            }
            let nal_type = NalType::from_raw((nal.bytes[0] >> 1) & 0x3F);
            let (rbsp, _) = unescape_rbsp(nal.bytes);
            let mut r = BitReader::with_sink(&rbsp, Recorder::default());
            let result: Result<(), crate::Error> = (|| {
                let header = h265::parse_nal_header(&mut r)?;
                match header.nal_type {
                    NalType::Vps => {
                        h265::vps::parse_vps(&mut r)?;
                    }
                    NalType::Sps => {
                        h265::sps::parse_sps(&mut r)?;
                    }
                    NalType::Pps => {
                        h265::pps::parse_pps(&mut r)?;
                    }
                    NalType::PrefixSei | NalType::SuffixSei => {
                        let sps = self.active_sps_id.and_then(|id| self.sps.get(&id));
                        h265::sei::parse_sei(&mut r, sps)?;
                    }
                    t if t.is_vcl() => {
                        let mut peek = BitReader::new(r.remaining_data());
                        peek.flag("first_slice_segment_in_pic_flag")?;
                        if header.nal_type.is_irap() {
                            peek.flag("no_output_of_prior_pics_flag")?;
                        }
                        let pps_id = peek.ue("slice_pic_parameter_set_id")?;
                        let pps =
                            self.pps
                                .get(&pps_id)
                                .ok_or(crate::Error::MissingParameterSet {
                                    what: "slice",
                                    referenced: "PPS",
                                    id: pps_id,
                                })?;
                        let sps = self.sps.get(&pps.pps_seq_parameter_set_id).ok_or(
                            crate::Error::MissingParameterSet {
                                what: "PPS",
                                referenced: "SPS",
                                id: pps.pps_seq_parameter_set_id,
                            },
                        )?;
                        h265::slice::parse_slice_header(&mut r, header, sps, pps)?;
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

impl H265Analyzer {
    fn parse_first_slice(
        &self,
        r: &mut BitReader<'_, crate::bitread::NoRecord>,
        header: h265::NalHeader,
    ) -> Result<h265::SliceHeader, crate::Error> {
        // We need PPS to finish parsing; peek it from the stream.
        // first_slice_segment_in_pic_flag u(1), then (if IRAP) no_output_of_prior_pics_flag u(1),
        // then slice_pic_parameter_set_id ue(v).
        let mut peek = BitReader::new(r.remaining_data());
        let first_flag = peek.flag("first_slice_segment_in_pic_flag")?;
        if header.nal_type.is_irap() {
            peek.flag("no_output_of_prior_pics_flag")?;
        }
        let pps_id = peek.ue("slice_pic_parameter_set_id")?;

        let pps = self
            .pps
            .get(&pps_id)
            .ok_or(crate::Error::MissingParameterSet {
                what: "slice",
                referenced: "PPS",
                id: pps_id,
            })?;
        let sps = self.sps.get(&pps.pps_seq_parameter_set_id).ok_or(
            crate::Error::MissingParameterSet {
                what: "PPS",
                referenced: "SPS",
                id: pps.pps_seq_parameter_set_id,
            },
        )?;
        let _ = first_flag;
        h265::slice::parse_slice_header(r, header, sps, pps)
    }
}
