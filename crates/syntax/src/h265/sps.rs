//! Sequence parameter set (7.3.2.2) including VUI and HRD parameters.

use crate::bitread::{BitReader, FieldSink};
use crate::h265::ptl::{parse_ptl, ProfileTierLevel};
use crate::Error;

/// HRD parameters (E.2.2).
#[derive(Debug, Clone, Default)]
pub struct HrdParams {
    pub initial_cpb_removal_delay_length: u8,
    pub au_cpb_removal_delay_length: u8,
    pub dpb_output_delay_length: u8,
    pub sub_pic_hrd_params_present: bool,
}

/// One decoded short-term reference picture set (resolved delta POCs from current).
#[derive(Debug, Clone, Default)]
pub struct ShortTermRefPicSet {
    /// Negative delta POCs (past pictures). delta_poc_s0[0] is closest (e.g. -1).
    pub delta_poc_s0: Vec<i32>,
    pub used_by_curr_s0: Vec<bool>,
    /// Positive delta POCs (future pictures). delta_poc_s1[0] is closest (e.g. +1).
    pub delta_poc_s1: Vec<i32>,
    pub used_by_curr_s1: Vec<bool>,
}

impl ShortTermRefPicSet {
    pub fn num_negative(&self) -> usize {
        self.delta_poc_s0.len()
    }
    pub fn num_positive(&self) -> usize {
        self.delta_poc_s1.len()
    }
}

#[derive(Debug, Clone, Default)]
pub struct Vui {
    pub timing_info: Option<TimingInfo>,
    pub hrd: Option<HrdParams>,
    pub pic_struct_present_flag: bool,
    pub frame_field_info_present_flag: bool,
    pub video_full_range_flag: bool,
    pub colour_primaries: u8,
    pub transfer_characteristics: u8,
    pub matrix_coefficients: u8,
}

#[derive(Debug, Clone, Copy)]
pub struct TimingInfo {
    pub num_units_in_tick: u32,
    pub time_scale: u32,
}

#[derive(Debug, Clone)]
pub struct Sps {
    pub sps_video_parameter_set_id: u8,
    pub max_sub_layers: u8,
    pub temporal_id_nesting_flag: bool,
    pub ptl: ProfileTierLevel,
    pub sps_seq_parameter_set_id: u32,
    pub chroma_format_idc: u32,
    pub separate_colour_plane_flag: bool,
    pub pic_width_in_luma_samples: u32,
    pub pic_height_in_luma_samples: u32,
    /// Conformance window offsets (left, right, top, bottom) in sub-sample units.
    pub conf_win: (u32, u32, u32, u32),
    pub bit_depth_luma: u8,
    pub bit_depth_chroma: u8,
    pub log2_max_pic_order_cnt_lsb: u8,
    /// max_dec_pic_buffering_minus1+1 per sub-layer.
    pub max_dec_pic_buffering: Vec<u32>,
    pub log2_min_luma_coding_block_size_minus3: u32,
    pub log2_diff_max_min_luma_coding_block_size: u32,
    pub log2_min_luma_transform_block_size_minus2: u32,
    pub log2_diff_max_min_luma_transform_block_size: u32,
    pub sample_adaptive_offset_enabled: bool,
    /// Decoded short-term RPS list.
    pub short_term_ref_pic_sets: Vec<ShortTermRefPicSet>,
    pub long_term_ref_pics_present_flag: bool,
    pub num_long_term_ref_pics: u32,
    pub lt_ref_poc_lsb: Vec<u32>,
    pub lt_used_by_curr: Vec<bool>,
    pub vui: Option<Vui>,
}

impl Sps {
    pub fn coded_width(&self) -> u32 {
        self.pic_width_in_luma_samples
    }

    pub fn coded_height(&self) -> u32 {
        self.pic_height_in_luma_samples
    }

    /// Conformance-cropped display width in luma samples (7-19..7-22).
    pub fn display_width(&self) -> u32 {
        let sub_w = match self.chroma_format_idc {
            1 | 2 => 2u32,
            _ => 1,
        };
        self.coded_width() - (self.conf_win.0 + self.conf_win.1) * sub_w
    }

    /// Conformance-cropped display height in luma samples.
    pub fn display_height(&self) -> u32 {
        let sub_h = if self.chroma_format_idc == 1 { 2u32 } else { 1 };
        self.coded_height() - (self.conf_win.2 + self.conf_win.3) * sub_h
    }

    /// CTB (coding tree block) size in luma samples.
    pub fn ctb_size_y(&self) -> u32 {
        let log2_min = self.log2_min_luma_coding_block_size_minus3 + 3;
        1 << (log2_min + self.log2_diff_max_min_luma_coding_block_size)
    }

    /// Frame rate from VUI timing info (HEVC: time_scale / num_units_in_tick, no /2).
    pub fn frame_rate(&self) -> Option<(u32, u32)> {
        let t = self.vui.as_ref()?.timing_info?;
        if t.time_scale == 0 || t.num_units_in_tick == 0 {
            return None;
        }
        Some((t.time_scale, t.num_units_in_tick))
    }

    pub fn profile_name(&self) -> &'static str {
        self.ptl.profile_name()
    }

    /// Highest-layer max_dec_pic_buffering value.
    pub fn max_dec_pic_buffering_value(&self) -> u32 {
        self.max_dec_pic_buffering.last().copied().unwrap_or(1)
    }
}

pub fn parse_sps<S: FieldSink>(r: &mut BitReader<'_, S>) -> Result<Sps, Error> {
    r.group("seq_parameter_set_rbsp", |r| {
        let sps_video_parameter_set_id = r.u(4, "sps_video_parameter_set_id")? as u8;
        let max_sub_layers_minus1 = r.u(3, "sps_max_sub_layers_minus1")? as u8;
        let max_sub_layers = max_sub_layers_minus1 + 1;
        let temporal_id_nesting_flag = r.flag("sps_temporal_id_nesting_flag")?;

        let ptl = parse_ptl(r, true, max_sub_layers_minus1)?;

        let sps_seq_parameter_set_id = r.ue("sps_seq_parameter_set_id")?;
        let chroma_format_idc = r.ue("chroma_format_idc")?;
        let separate_colour_plane_flag = if chroma_format_idc == 3 {
            r.flag("separate_colour_plane_flag")?
        } else {
            false
        };

        let pic_width_in_luma_samples = r.ue("pic_width_in_luma_samples")?;
        let pic_height_in_luma_samples = r.ue("pic_height_in_luma_samples")?;

        let conf_win = if r.flag("conformance_window_flag")? {
            let l = r.ue("conf_win_left_offset")?;
            let ri = r.ue("conf_win_right_offset")?;
            let t = r.ue("conf_win_top_offset")?;
            let b = r.ue("conf_win_bottom_offset")?;
            (l, ri, t, b)
        } else {
            (0, 0, 0, 0)
        };

        let bit_depth_luma = r.ue("bit_depth_luma_minus8")? as u8 + 8;
        let bit_depth_chroma = r.ue("bit_depth_chroma_minus8")? as u8 + 8;
        let lsb_minus4 = r.ue("log2_max_pic_order_cnt_lsb_minus4")?;
        if lsb_minus4 > 12 {
            return Err(crate::Error::OutOfRange {
                field: "log2_max_pic_order_cnt_lsb_minus4",
                value: lsb_minus4 as i64,
                allowed: "0..=12",
            });
        }
        let log2_max_pic_order_cnt_lsb = lsb_minus4 as u8 + 4;

        let sub_layer_ordering_info_present_flag =
            r.flag("sps_sub_layer_ordering_info_present_flag")?;
        let start = if sub_layer_ordering_info_present_flag {
            0usize
        } else {
            max_sub_layers_minus1 as usize
        };
        let mut max_dec_pic_buffering = vec![0u32; max_sub_layers as usize];
        for entry in max_dec_pic_buffering
            .iter_mut()
            .take(max_sub_layers as usize)
            .skip(start)
        {
            *entry = r.ue("sps_max_dec_pic_buffering_minus1")? + 1;
            r.ue("sps_max_num_reorder_pics")?;
            r.ue("sps_max_latency_increase_plus1")?;
        }
        if !sub_layer_ordering_info_present_flag && max_sub_layers > 1 {
            let v = max_dec_pic_buffering[max_sub_layers_minus1 as usize];
            for entry in max_dec_pic_buffering
                .iter_mut()
                .take(max_sub_layers_minus1 as usize)
            {
                *entry = v;
            }
        }

        let log2_min_luma_coding_block_size_minus3 =
            r.ue("log2_min_luma_coding_block_size_minus3")?;
        let log2_diff_max_min_luma_coding_block_size =
            r.ue("log2_diff_max_min_luma_coding_block_size")?;
        // CtbLog2SizeY must land in 4..=6 (CTB 16..64); anything else makes
        // the shift in ctb_size_y() and every CTB-count derivation garbage.
        let ctb_log2 =
            log2_min_luma_coding_block_size_minus3 + 3 + log2_diff_max_min_luma_coding_block_size;
        if !(4..=6).contains(&ctb_log2) {
            return Err(crate::Error::OutOfRange {
                field: "CtbLog2SizeY",
                value: ctb_log2 as i64,
                allowed: "4..=6",
            });
        }
        let log2_min_luma_transform_block_size_minus2 =
            r.ue("log2_min_luma_transform_block_size_minus2")?;
        let log2_diff_max_min_luma_transform_block_size =
            r.ue("log2_diff_max_min_luma_transform_block_size")?;
        r.ue("max_transform_hierarchy_depth_inter")?;
        r.ue("max_transform_hierarchy_depth_intra")?;

        if r.flag("scaling_list_enabled_flag")? && r.flag("sps_scaling_list_data_present_flag")? {
            parse_scaling_list_data(r)?;
        }

        r.flag("amp_enabled_flag")?;
        let sample_adaptive_offset_enabled = r.flag("sample_adaptive_offset_enabled_flag")?;

        if r.flag("pcm_enabled_flag")? {
            r.u(4, "pcm_sample_bit_depth_luma_minus1")?;
            r.u(4, "pcm_sample_bit_depth_chroma_minus1")?;
            r.ue("log2_min_pcm_luma_coding_block_size_minus3")?;
            r.ue("log2_diff_max_min_pcm_luma_coding_block_size")?;
            r.flag("pcm_loop_filter_disabled_flag")?;
        }

        let num_short_term_ref_pic_sets = r.ue("num_short_term_ref_pic_sets")?;
        let mut short_term_ref_pic_sets = Vec::with_capacity(num_short_term_ref_pic_sets as usize);
        for i in 0..num_short_term_ref_pic_sets {
            let rps = parse_st_ref_pic_set(r, i, &short_term_ref_pic_sets)?;
            short_term_ref_pic_sets.push(rps);
        }

        let long_term_ref_pics_present_flag = r.flag("long_term_ref_pics_present_flag")?;
        let mut num_long_term_ref_pics = 0u32;
        let mut lt_ref_poc_lsb = Vec::new();
        let mut lt_used_by_curr = Vec::new();
        if long_term_ref_pics_present_flag {
            num_long_term_ref_pics = r.ue("num_long_term_ref_pics_sps")?;
            for _ in 0..num_long_term_ref_pics {
                let poc_lsb = r.u(log2_max_pic_order_cnt_lsb, "lt_ref_pic_poc_lsb_sps")? as u32;
                let used = r.flag("used_by_curr_pic_lt_sps_flag")?;
                lt_ref_poc_lsb.push(poc_lsb);
                lt_used_by_curr.push(used);
            }
        }

        r.flag("sps_temporal_mvp_enabled_flag")?;
        r.flag("strong_intra_smoothing_enabled_flag")?;

        let vui = if r.flag("vui_parameters_present_flag")? {
            Some(parse_vui(r, max_sub_layers_minus1)?)
        } else {
            None
        };

        // sps_extension_flag — skip remainder.
        if r.more_rbsp_data() {
            let rem = r.bits_left();
            if rem > 1 {
                r.u_skip(rem - 1)?;
            }
        }

        Ok(Sps {
            sps_video_parameter_set_id,
            max_sub_layers,
            temporal_id_nesting_flag,
            ptl,
            sps_seq_parameter_set_id,
            chroma_format_idc,
            separate_colour_plane_flag,
            pic_width_in_luma_samples,
            pic_height_in_luma_samples,
            conf_win,
            bit_depth_luma,
            bit_depth_chroma,
            log2_max_pic_order_cnt_lsb,
            max_dec_pic_buffering,
            log2_min_luma_coding_block_size_minus3,
            log2_diff_max_min_luma_coding_block_size,
            log2_min_luma_transform_block_size_minus2,
            log2_diff_max_min_luma_transform_block_size,
            sample_adaptive_offset_enabled,
            short_term_ref_pic_sets,
            long_term_ref_pics_present_flag,
            num_long_term_ref_pics,
            lt_ref_poc_lsb,
            lt_used_by_curr,
            vui,
        })
    })
}

/// st_ref_pic_set(stRpsIdx) — 7.3.7 / 7.4.8.
pub(crate) fn parse_st_ref_pic_set<S: FieldSink>(
    r: &mut BitReader<'_, S>,
    idx: u32,
    prev_sets: &[ShortTermRefPicSet],
) -> Result<ShortTermRefPicSet, Error> {
    r.group("st_ref_pic_set", |r| {
        let inter = if idx > 0 {
            r.flag("inter_ref_pic_set_prediction_flag")?
        } else {
            false
        };

        if inter {
            // 7.4.8: inter_ref_pic_set_prediction path.
            let mut delta_idx = 1u32;
            if idx == prev_sets.len() as u32 {
                // Inline in slice header: delta_idx_minus1 present.
                delta_idx = r.ue("delta_idx_minus1")? + 1;
            }
            let ref_idx = (idx as i32 - delta_idx as i32) as usize;
            if ref_idx >= prev_sets.len() {
                return Err(Error::OutOfRange {
                    field: "delta_idx_minus1",
                    value: delta_idx as i64,
                    allowed: "1..=idx",
                });
            }
            let ref_set = &prev_sets[ref_idx];

            let delta_rps_sign = r.flag("delta_rps_sign")?;
            let abs_delta_rps = r.ue("abs_delta_rps_minus1")? as i32 + 1;
            let delta_rps = if delta_rps_sign {
                -abs_delta_rps
            } else {
                abs_delta_rps
            };

            // num entries in the ref_set = NumDeltaPocs[ref_idx]
            let num_ref = ref_set.num_negative() + ref_set.num_positive() + 1;
            let mut used_by_curr = vec![false; num_ref];
            let mut use_delta = vec![false; num_ref];
            for j in 0..num_ref {
                used_by_curr[j] = r.flag("used_by_curr_pic_flag")?;
                if !used_by_curr[j] {
                    use_delta[j] = r.flag("use_delta_flag")?;
                }
            }

            // Reconstruct new set per 7.4.8 Table 7-8.
            // j indices in used_by_curr:
            //   j ∈ [0, numPositive)          → ref_set.delta_poc_s1[j]
            //   j ∈ [numPositive, numNeg+numPositive) → ref_set.delta_poc_s0[j-numPositive]
            //   j == numNeg+numPositive         → reference picture (delta=0)
            let num_pos = ref_set.num_positive();
            let num_neg = ref_set.num_negative();

            let mut new_s0: Vec<(i32, bool)> = Vec::new();
            let mut new_s1: Vec<(i32, bool)> = Vec::new();

            // S1 entries of ref_set → new delta = ref_s1[i] + delta_rps
            for (i, &d_ref) in ref_set.delta_poc_s1.iter().enumerate() {
                let d = d_ref + delta_rps;
                let j = i; // j index for this S1 entry
                if used_by_curr[j] || use_delta[j] {
                    if d < 0 {
                        new_s0.push((d, used_by_curr[j]));
                    } else if d > 0 {
                        new_s1.push((d, used_by_curr[j]));
                    }
                    // d == 0: current pic, not included
                }
            }
            // S0 entries of ref_set → new delta = ref_s0[i] + delta_rps
            for (i, &d_ref) in ref_set.delta_poc_s0.iter().enumerate() {
                let d = d_ref + delta_rps;
                let j = num_pos + i;
                if used_by_curr[j] || use_delta[j] {
                    if d < 0 {
                        new_s0.push((d, used_by_curr[j]));
                    } else if d > 0 {
                        new_s1.push((d, used_by_curr[j]));
                    }
                }
            }
            // The "current" ref_set picture at delta_rps
            {
                let j = num_neg + num_pos;
                if used_by_curr[j] || use_delta[j] {
                    if delta_rps < 0 {
                        new_s0.push((delta_rps, used_by_curr[j]));
                    } else if delta_rps > 0 {
                        new_s1.push((delta_rps, used_by_curr[j]));
                    }
                }
            }

            // Sort: s0 descending (closest-to-0 negative first: -1 before -2).
            new_s0.sort_by_key(|a| std::cmp::Reverse(a.0));
            // s1 ascending (closest positive first: +1 before +2).
            new_s1.sort_by_key(|a| a.0);

            Ok(ShortTermRefPicSet {
                delta_poc_s0: new_s0.iter().map(|&(d, _)| d).collect(),
                used_by_curr_s0: new_s0.iter().map(|&(_, u)| u).collect(),
                delta_poc_s1: new_s1.iter().map(|&(d, _)| d).collect(),
                used_by_curr_s1: new_s1.iter().map(|&(_, u)| u).collect(),
            })
        } else {
            // Direct coding path (7.4.8).
            let num_negative = r.ue("num_negative_pics")? as usize;
            let num_positive = r.ue("num_positive_pics")? as usize;

            let mut delta_poc_s0 = Vec::with_capacity(num_negative);
            let mut used_s0 = Vec::with_capacity(num_negative);
            let mut cum = 0i32;
            for _ in 0..num_negative {
                let delta = r.ue("delta_poc_s0_minus1")? as i32 + 1;
                cum -= delta;
                delta_poc_s0.push(cum);
                used_s0.push(r.flag("used_by_curr_pic_s0_flag")?);
            }

            let mut delta_poc_s1 = Vec::with_capacity(num_positive);
            let mut used_s1 = Vec::with_capacity(num_positive);
            cum = 0;
            for _ in 0..num_positive {
                let delta = r.ue("delta_poc_s1_minus1")? as i32 + 1;
                cum += delta;
                delta_poc_s1.push(cum);
                used_s1.push(r.flag("used_by_curr_pic_s1_flag")?);
            }

            Ok(ShortTermRefPicSet {
                delta_poc_s0,
                used_by_curr_s0: used_s0,
                delta_poc_s1,
                used_by_curr_s1: used_s1,
            })
        }
    })
}

/// scaling_list_data() — 7.3.4.
fn parse_scaling_list_data<S: FieldSink>(r: &mut BitReader<'_, S>) -> Result<(), Error> {
    r.group("scaling_list_data", |r| {
        for size_id in 0..4usize {
            let matrix_count = if size_id == 3 { 2 } else { 6 };
            for _ in 0..matrix_count {
                let pred_mode_flag = r.flag("scaling_list_pred_mode_flag")?;
                if !pred_mode_flag {
                    r.ue("scaling_list_pred_matrix_id_delta")?;
                } else {
                    // coeff count: sizeId==0 → 16, else 64; dc coeff at sizeId>1
                    let n_coeffs = if size_id == 0 { 16 } else { 64 };
                    if size_id > 1 {
                        r.se("scaling_list_dc_coef_minus8")?;
                    }
                    for _ in 0..n_coeffs {
                        r.se("scaling_list_delta_coef")?;
                    }
                }
            }
        }
        Ok(())
    })
}

/// VUI parameters (E.2.1).
fn parse_vui<S: FieldSink>(
    r: &mut BitReader<'_, S>,
    max_sub_layers_minus1: u8,
) -> Result<Vui, Error> {
    r.group("vui_parameters", |r| {
        let mut vui = Vui::default();

        if r.flag("aspect_ratio_info_present_flag")? {
            let idc = r.u(8, "aspect_ratio_idc")?;
            if idc == 255 {
                r.u(16, "sar_width")?;
                r.u(16, "sar_height")?;
            }
        }
        if r.flag("overscan_info_present_flag")? {
            r.flag("overscan_appropriate_flag")?;
        }
        if r.flag("video_signal_type_present_flag")? {
            r.u(3, "video_format")?;
            vui.video_full_range_flag = r.flag("video_full_range_flag")?;
            if r.flag("colour_description_present_flag")? {
                vui.colour_primaries = r.u(8, "colour_primaries")? as u8;
                vui.transfer_characteristics = r.u(8, "transfer_characteristics")? as u8;
                vui.matrix_coefficients = r.u(8, "matrix_coefficients")? as u8;
            }
        }
        if r.flag("chroma_loc_info_present_flag")? {
            r.ue("chroma_sample_loc_type_top_field")?;
            r.ue("chroma_sample_loc_type_bottom_field")?;
        }
        r.flag("neutral_chroma_indication_flag")?;
        r.flag("field_seq_flag")?;
        vui.frame_field_info_present_flag = r.flag("frame_field_info_present_flag")?;
        if r.flag("default_display_window_flag")? {
            r.ue("def_disp_win_left_offset")?;
            r.ue("def_disp_win_right_offset")?;
            r.ue("def_disp_win_top_offset")?;
            r.ue("def_disp_win_bottom_offset")?;
        }
        if r.flag("vui_timing_info_present_flag")? {
            let num_units_in_tick = r.u(32, "vui_num_units_in_tick")? as u32;
            let time_scale = r.u(32, "vui_time_scale")? as u32;
            vui.timing_info = Some(TimingInfo {
                num_units_in_tick,
                time_scale,
            });
            if r.flag("vui_poc_proportional_to_timing_flag")? {
                r.ue("vui_num_ticks_poc_diff_one_minus1")?;
            }
            if r.flag("vui_hrd_parameters_present_flag")? {
                vui.hrd = Some(parse_hrd(r, true, max_sub_layers_minus1)?);
            }
        }
        if r.flag("bitstream_restriction_flag")? {
            r.flag("tiles_fixed_structure_flag")?;
            r.flag("motion_vectors_over_pic_boundaries_flag")?;
            r.flag("restricted_ref_pic_lists_flag")?;
            r.ue("min_spatial_segmentation_idc")?;
            r.ue("max_bytes_per_pic_denom")?;
            r.ue("max_bits_per_min_cu_denom")?;
            r.ue("log2_max_mv_length_horizontal")?;
            r.ue("log2_max_mv_length_vertical")?;
        }

        Ok(vui)
    })
}

/// hrd_parameters(commonInfPresentFlag, maxNumSubLayersMinus1) — E.2.2.
/// We parse enough to extract the delay lengths; sub-layer CPB data is skipped.
pub(crate) fn parse_hrd<S: FieldSink>(
    r: &mut BitReader<'_, S>,
    common_inf_present: bool,
    max_num_sub_layers_minus1: u8,
) -> Result<HrdParams, Error> {
    r.group("hrd_parameters", |r| {
        let mut hrd = HrdParams {
            initial_cpb_removal_delay_length: 24,
            au_cpb_removal_delay_length: 24,
            dpb_output_delay_length: 24,
            sub_pic_hrd_params_present: false,
        };

        let mut nal_hrd_present = false;
        let mut vcl_hrd_present = false;

        if common_inf_present {
            nal_hrd_present = r.flag("nal_hrd_parameters_present_flag")?;
            vcl_hrd_present = r.flag("vcl_hrd_parameters_present_flag")?;
            if nal_hrd_present || vcl_hrd_present {
                hrd.sub_pic_hrd_params_present = r.flag("sub_pic_hrd_params_present_flag")?;
                if hrd.sub_pic_hrd_params_present {
                    r.u(8, "tick_divisor_minus2")?;
                    r.u(5, "du_cpb_removal_delay_increment_length_minus1")?;
                    r.flag("sub_pic_cpb_params_in_pic_timing_sei_flag")?;
                    r.u(5, "dpb_output_delay_du_length_minus1")?;
                }
                r.u(4, "bit_rate_scale")?;
                r.u(4, "cpb_size_scale")?;
                if hrd.sub_pic_hrd_params_present {
                    r.u(4, "cpb_size_du_scale")?;
                }
                hrd.initial_cpb_removal_delay_length =
                    r.u(5, "initial_cpb_removal_delay_length_minus1")? as u8 + 1;
                hrd.au_cpb_removal_delay_length =
                    r.u(5, "au_cpb_removal_delay_length_minus1")? as u8 + 1;
                hrd.dpb_output_delay_length = r.u(5, "dpb_output_delay_length_minus1")? as u8 + 1;
            }
        }

        // sub_layer HRD parameters loop — parse structure but discard CPB data.
        for _ in 0..=max_num_sub_layers_minus1 as usize {
            let fixed_general = r.flag("fixed_pic_rate_general_flag")?;
            let fixed_within_cvs = if !fixed_general {
                r.flag("fixed_pic_rate_within_cvs_flag")?
            } else {
                true
            };
            let low_delay = if fixed_within_cvs {
                r.ue("elemental_duration_in_tc_minus1")?;
                false
            } else {
                r.flag("low_delay_hrd_flag")?
            };
            let cpb_cnt = if !low_delay {
                r.ue("cpb_cnt_minus1")? + 1
            } else {
                1u32
            };
            // sub_layer_hrd_parameters — skip CPB entries for nal/vcl if present.
            for _ in 0..if nal_hrd_present { 1 } else { 0 } + if vcl_hrd_present { 1 } else { 0 } {
                parse_sub_layer_hrd(r, cpb_cnt, hrd.sub_pic_hrd_params_present)?;
            }
        }

        Ok(hrd)
    })
}

/// sub_layer_hrd_parameters() — E.2.3 / E.3.3. Parses and discards CPB entries.
fn parse_sub_layer_hrd<S: FieldSink>(
    r: &mut BitReader<'_, S>,
    cpb_cnt: u32,
    sub_pic_present: bool,
) -> Result<(), Error> {
    for _ in 0..cpb_cnt {
        r.ue("bit_rate_value_minus1")?;
        r.ue("cpb_size_value_minus1")?;
        if sub_pic_present {
            r.ue("cpb_size_du_value_minus1")?;
            r.ue("bit_rate_du_value_minus1")?;
        }
        r.flag("cbr_flag")?;
    }
    Ok(())
}
