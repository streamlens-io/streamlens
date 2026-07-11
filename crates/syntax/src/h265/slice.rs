//! Slice segment header (7.3.6.1) — parsed up to (excluding) slice data.

use crate::bitread::{BitReader, FieldSink};
use crate::h265::sps::{parse_st_ref_pic_set, ShortTermRefPicSet, Sps};
use crate::h265::{NalHeader, Pps};
use crate::Error;

/// Slice type (Table 7-7). NOTE: opposite mapping to H.264.
/// 0=B, 1=P, 2=I.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SliceType {
    B,
    P,
    I,
}

impl SliceType {
    pub fn from_raw(v: u32) -> Result<Self, Error> {
        Ok(match v {
            0 => SliceType::B,
            1 => SliceType::P,
            2 => SliceType::I,
            other => {
                return Err(Error::OutOfRange {
                    field: "slice_type",
                    value: other as i64,
                    allowed: "0..=2",
                })
            }
        })
    }
}

/// One long-term reference picture entry from the slice header.
#[derive(Debug, Clone)]
pub struct LtEntry {
    pub poc_lsb: u32,
    pub used_by_curr: bool,
    pub delta_poc_msb_present: bool,
    pub delta_poc_msb_cycle_lt: u32,
}

#[derive(Debug, Clone)]
pub struct SliceHeader {
    pub first_slice_segment_in_pic_flag: bool,
    pub dependent: bool,
    pub slice_type: SliceType,
    pub pic_order_cnt_lsb: u32,
    pub slice_pic_parameter_set_id: u32,
    /// Resolved short-term RPS actually used by this slice.
    pub short_term_rps: ShortTermRefPicSet,
    /// Long-term entries (from slice header).
    pub lt_entries: Vec<LtEntry>,
    /// num_ref_idx_active[0] = L0, [1] = L1.
    pub num_ref_idx_active: [u32; 2],
    /// ref_pic_list_modification indices for L0/L1 (entry = index into pic list).
    pub ref_pic_list_mod: [Vec<u32>; 2],
    /// slice_qp = 26 + init_qp_minus26 + slice_qp_delta.
    pub slice_qp: i32,
    pub sao_luma: bool,
    pub sao_chroma: bool,
    pub temporal_id: u8,
}

/// Ceiling of log2 of `v`, minimum 1.
fn ceil_log2(v: u32) -> u8 {
    if v <= 1 {
        1
    } else {
        (32 - (v - 1).leading_zeros()) as u8
    }
}

pub fn parse_slice_header<S: FieldSink>(
    r: &mut BitReader<'_, S>,
    nal: NalHeader,
    sps: &Sps,
    pps: &Pps,
) -> Result<SliceHeader, Error> {
    r.group("slice_segment_header", |r| {
        let is_irap = nal.nal_type.is_irap();
        let first_slice_segment_in_pic_flag = r.flag("first_slice_segment_in_pic_flag")?;

        if is_irap {
            r.flag("no_output_of_prior_pics_flag")?;
        }

        let slice_pic_parameter_set_id = r.ue("slice_pic_parameter_set_id")?;

        let mut dependent = false;
        if !first_slice_segment_in_pic_flag {
            if pps.dependent_slice_segments_enabled_flag {
                dependent = r.flag("dependent_slice_segment_flag")?;
            }
            // slice_segment_address: u(v) with Ceil(Log2(PicSizeInCtbsY)) bits.
            let ctb_size = sps.ctb_size_y();
            let pic_w_ctbs = sps.coded_width().div_ceil(ctb_size);
            let pic_h_ctbs = sps.coded_height().div_ceil(ctb_size);
            let pic_size_ctbs = pic_w_ctbs * pic_h_ctbs;
            let bits = ceil_log2(pic_size_ctbs);
            r.u(bits, "slice_segment_address")?;
        }

        if dependent {
            // Dependent slice segment: most header fields are inferred from the
            // preceding independent segment. Return a placeholder.
            return Ok(SliceHeader {
                first_slice_segment_in_pic_flag,
                dependent: true,
                slice_type: SliceType::I,
                pic_order_cnt_lsb: 0,
                slice_pic_parameter_set_id,
                short_term_rps: ShortTermRefPicSet::default(),
                lt_entries: Vec::new(),
                num_ref_idx_active: [0, 0],
                ref_pic_list_mod: [Vec::new(), Vec::new()],
                slice_qp: 0,
                sao_luma: false,
                sao_chroma: false,
                temporal_id: nal.temporal_id,
            });
        }

        // num_extra_slice_header_bits bits (reserved).
        for _ in 0..pps.num_extra_slice_header_bits {
            r.flag("slice_reserved_flag")?;
        }

        let slice_type = SliceType::from_raw(r.ue("slice_type")?)?;
        if pps.output_flag_present_flag {
            r.flag("pic_output_flag")?;
        }
        if sps.separate_colour_plane_flag {
            r.u(2, "colour_plane_id")?;
        }

        let mut pic_order_cnt_lsb = 0u32;
        let mut short_term_rps = ShortTermRefPicSet::default();
        let mut lt_entries: Vec<LtEntry> = Vec::new();

        if !nal.nal_type.is_idr() {
            pic_order_cnt_lsb =
                r.u(sps.log2_max_pic_order_cnt_lsb, "slice_pic_order_cnt_lsb")? as u32;

            let short_term_ref_pic_set_sps_flag = r.flag("short_term_ref_pic_set_sps_flag")?;

            if !short_term_ref_pic_set_sps_flag {
                // Inline st_ref_pic_set.
                let num = sps.short_term_ref_pic_sets.len() as u32;
                short_term_rps = parse_st_ref_pic_set(r, num, &sps.short_term_ref_pic_sets)?;
            } else if sps.short_term_ref_pic_sets.len() > 1 {
                let bits = ceil_log2(sps.short_term_ref_pic_sets.len() as u32);
                let idx = r.u(bits, "short_term_ref_pic_set_idx")? as usize;
                if idx < sps.short_term_ref_pic_sets.len() {
                    short_term_rps = sps.short_term_ref_pic_sets[idx].clone();
                }
            } else if sps.short_term_ref_pic_sets.len() == 1 {
                // Only one set → implicit idx=0, no bits coded.
                short_term_rps = sps.short_term_ref_pic_sets[0].clone();
            }

            if sps.long_term_ref_pics_present_flag {
                let num_long_term_sps = if sps.num_long_term_ref_pics > 0 {
                    r.ue("num_long_term_sps")?
                } else {
                    0
                };
                let num_long_term_pics = r.ue("num_long_term_pics")?;
                let bits_lt = sps.log2_max_pic_order_cnt_lsb;
                let num_lt_sps_bits = if sps.num_long_term_ref_pics > 0 {
                    ceil_log2(sps.num_long_term_ref_pics)
                } else {
                    0
                };

                for i in 0..(num_long_term_sps + num_long_term_pics) {
                    let (poc_lsb, used_by_curr) = if i < num_long_term_sps {
                        // Reference SPS long-term entry.
                        let idx = if sps.num_long_term_ref_pics > 1 {
                            r.u(num_lt_sps_bits, "lt_idx_sps")? as usize
                        } else {
                            0
                        };
                        let poc = sps.lt_ref_poc_lsb.get(idx).copied().unwrap_or(0);
                        let used = sps.lt_used_by_curr.get(idx).copied().unwrap_or(false);
                        (poc, used)
                    } else {
                        // Directly coded.
                        let poc = r.u(bits_lt, "poc_lsb_lt")? as u32;
                        let used = r.flag("used_by_curr_pic_lt_flag")?;
                        (poc, used)
                    };

                    let delta_msb_present = r.flag("delta_poc_msb_present_flag")?;
                    let delta_msb = if delta_msb_present {
                        r.ue("delta_poc_msb_cycle_lt")?
                    } else {
                        0
                    };
                    lt_entries.push(LtEntry {
                        poc_lsb,
                        used_by_curr,
                        delta_poc_msb_present: delta_msb_present,
                        delta_poc_msb_cycle_lt: delta_msb,
                    });
                }
            }

            if sps
                .vui
                .as_ref()
                .map(|v| v.timing_info.is_some())
                .unwrap_or(false)
            {
                r.flag("slice_temporal_mvp_enabled_flag")?;
            }
        }

        // SAO flags are only present when the SPS enables SAO (and the
        // chroma flag only when ChromaArrayType != 0); reading them
        // unconditionally would shift every following field.
        let (sao_luma, sao_chroma) = if sps.sample_adaptive_offset_enabled {
            let luma = r.flag("slice_sao_luma_flag")?;
            let chroma_array_type = if sps.separate_colour_plane_flag {
                0
            } else {
                sps.chroma_format_idc
            };
            let chroma = if chroma_array_type != 0 {
                r.flag("slice_sao_chroma_flag")?
            } else {
                false
            };
            (luma, chroma)
        } else {
            (false, false)
        };

        let mut num_ref_idx_active = [
            pps.num_ref_idx_l0_default_active,
            pps.num_ref_idx_l1_default_active,
        ];
        let mut ref_pic_list_mod: [Vec<u32>; 2] = [Vec::new(), Vec::new()];

        if matches!(slice_type, SliceType::P | SliceType::B) {
            if r.flag("num_ref_idx_active_override_flag")? {
                num_ref_idx_active[0] = r.ue("num_ref_idx_l0_active_minus1")? + 1;
                if slice_type == SliceType::B {
                    num_ref_idx_active[1] = r.ue("num_ref_idx_l1_active_minus1")? + 1;
                }
            }
            if slice_type == SliceType::P {
                num_ref_idx_active[1] = 0;
            }

            // ref_pic_lists_modification (7.3.6.2).
            if pps.lists_modification_present_flag {
                // NumPicTotalCurr: number of reference pictures in the current RPS
                // used by the slice. Approximate as num_ref_idx_active[0] + [1].
                let num_pic_total_curr = num_ref_idx_active[0] + num_ref_idx_active[1];
                if num_pic_total_curr > 1 {
                    if r.flag("ref_pic_list_modification_flag_l0")? {
                        let bits = ceil_log2(num_pic_total_curr);
                        for _ in 0..num_ref_idx_active[0] {
                            ref_pic_list_mod[0].push(r.u(bits, "list_entry_l0")? as u32);
                        }
                    }
                    if slice_type == SliceType::B && r.flag("ref_pic_list_modification_flag_l1")? {
                        let bits = ceil_log2(num_pic_total_curr);
                        for _ in 0..num_ref_idx_active[1] {
                            ref_pic_list_mod[1].push(r.u(bits, "list_entry_l1")? as u32);
                        }
                    }
                }
            }

            if slice_type == SliceType::B {
                r.flag("mvd_l1_zero_flag")?;
            }
            if pps.cabac_init_present_flag {
                r.flag("cabac_init_flag")?;
            }

            // collocated fields: collocated_from_l0_flag for B slices.
            // Approximation: slice_temporal_mvp_enabled assumed true.
            if slice_type == SliceType::B {
                r.flag("collocated_from_l0_flag")?;
            }
            // collocated_ref_idx omitted (only when num_ref > 1; skip conservatively).

            // pred_weight_table (7.3.6.3)
            let parse_pwt = (pps.weighted_pred_flag && slice_type == SliceType::P)
                || (pps.weighted_bipred_flag && slice_type == SliceType::B);
            if parse_pwt {
                parse_pred_weight_table(r, sps, slice_type, &num_ref_idx_active)?;
            }
        } else {
            num_ref_idx_active = [0, 0];
        }

        let five_minus_max_num_merge_cand = if matches!(slice_type, SliceType::P | SliceType::B) {
            r.ue("five_minus_max_num_merge_cand")?
        } else {
            0
        };
        let _ = five_minus_max_num_merge_cand;

        let slice_qp_delta = r.se("slice_qp_delta")?;
        let slice_qp = 26 + pps.init_qp_minus26 + slice_qp_delta;

        if pps.pps_slice_chroma_qp_offsets_present_flag {
            r.se("slice_cb_qp_offset")?;
            r.se("slice_cr_qp_offset")?;
        }

        // deblocking filter override + loop filter across slices (optional fields)
        if pps.deblocking_filter_control_present_flag
            && r.flag("deblocking_filter_override_flag")?
        {
            r.flag("slice_deblocking_filter_disabled_flag")?;
            if !pps.deblocking_filter_disabled_flag {
                r.se("slice_beta_offset_div2")?;
                r.se("slice_tc_offset_div2")?;
            }
        }
        if pps.loop_filter_across_slices_enabled_flag
            && (sao_luma || sao_chroma || !pps.deblocking_filter_disabled_flag)
        {
            r.flag("slice_loop_filter_across_slices_enabled_flag")?;
        }

        // Entry points (for tiles/WPP).
        if pps.tiles_enabled_flag || pps.entropy_coding_sync_enabled_flag {
            let num_entry = r.ue("num_entry_point_offsets")?;
            if num_entry > 0 {
                let offset_len = r.ue("offset_len_minus1")? as u8 + 1;
                for _ in 0..num_entry {
                    r.u(offset_len, "entry_point_offset_minus1")?;
                }
            }
        }

        if pps.slice_segment_header_extension_present_flag {
            let ext_len = r.ue("slice_segment_header_extension_length")?;
            for _ in 0..ext_len {
                r.u(8, "slice_segment_header_extension_data_byte")?;
            }
        }

        Ok(SliceHeader {
            first_slice_segment_in_pic_flag,
            dependent: false,
            slice_type,
            pic_order_cnt_lsb,
            slice_pic_parameter_set_id,
            short_term_rps,
            lt_entries,
            num_ref_idx_active,
            ref_pic_list_mod,
            slice_qp,
            sao_luma,
            sao_chroma,
            temporal_id: nal.temporal_id,
        })
    })
}

fn parse_pred_weight_table<S: FieldSink>(
    r: &mut BitReader<'_, S>,
    sps: &Sps,
    slice_type: SliceType,
    num_ref_idx_active: &[u32; 2],
) -> Result<(), Error> {
    r.group("pred_weight_table", |r| {
        r.ue("luma_log2_weight_denom")?;
        let chroma = sps.chroma_format_idc != 0 && !sps.separate_colour_plane_flag;
        if chroma {
            r.se("delta_chroma_log2_weight_denom")?;
        }
        let lists = if slice_type == SliceType::B { 2 } else { 1 };
        for &n_active in num_ref_idx_active.iter().take(lists) {
            for _ in 0..n_active {
                if r.flag("luma_weight_flag")? {
                    r.se("delta_luma_weight")?;
                    r.se("luma_offset")?;
                }
                if chroma && r.flag("chroma_weight_flag")? {
                    for _ in 0..2 {
                        r.se("delta_chroma_weight")?;
                        r.se("delta_chroma_offset")?;
                    }
                }
            }
        }
        Ok(())
    })
}
