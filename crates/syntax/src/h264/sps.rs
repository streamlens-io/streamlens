//! Sequence parameter set (7.3.2.1) including VUI and HRD parameters.

use crate::bitread::{BitReader, FieldSink};
use crate::h264::parse_scaling_list;
use crate::Error;

/// HRD parameters (Annex E.1.2). Shared shape with the buffer analyzer.
#[derive(Debug, Clone, Default)]
pub struct HrdParams {
    pub cpb_cnt: u32,
    pub bit_rate_scale: u8,
    pub cpb_size_scale: u8,
    /// Per CPB: (bit_rate_value_minus1, cpb_size_value_minus1, cbr_flag).
    pub cpbs: Vec<(u32, u32, bool)>,
    pub initial_cpb_removal_delay_length: u8,
    pub cpb_removal_delay_length: u8,
    pub dpb_output_delay_length: u8,
    pub time_offset_length: u8,
}

impl HrdParams {
    /// Bitrate of scheduler selection `i` in bits/second (E-37).
    pub fn bit_rate(&self, i: usize) -> u64 {
        (self.cpbs[i].0 as u64 + 1) << (6 + self.bit_rate_scale)
    }

    /// CPB size of scheduler selection `i` in bits (E-38).
    pub fn cpb_size(&self, i: usize) -> u64 {
        (self.cpbs[i].1 as u64 + 1) << (4 + self.cpb_size_scale)
    }
}

/// VUI parameters (Annex E.1.1); only fields the app consumes are kept.
#[derive(Debug, Clone, Default)]
pub struct Vui {
    pub timing_info: Option<TimingInfo>,
    pub nal_hrd: Option<HrdParams>,
    pub vcl_hrd: Option<HrdParams>,
    pub low_delay_hrd_flag: bool,
    pub pic_struct_present_flag: bool,
    pub max_num_reorder_frames: Option<u32>,
    pub max_dec_frame_buffering: Option<u32>,
    pub video_full_range_flag: bool,
    pub colour_primaries: u8,
    pub transfer_characteristics: u8,
    pub matrix_coefficients: u8,
}

#[derive(Debug, Clone, Copy)]
pub struct TimingInfo {
    pub num_units_in_tick: u32,
    pub time_scale: u32,
    pub fixed_frame_rate_flag: bool,
}

/// Picture order count config (from pic_order_cnt_type).
#[derive(Debug, Clone)]
pub enum PocType {
    Type0 {
        log2_max_pic_order_cnt_lsb: u8,
    },
    Type1 {
        delta_pic_order_always_zero_flag: bool,
        offset_for_non_ref_pic: i32,
        offset_for_top_to_bottom_field: i32,
        offsets_for_ref_frame: Vec<i32>,
    },
    Type2,
}

#[derive(Debug, Clone)]
pub struct Sps {
    pub profile_idc: u8,
    pub constraint_set_flags: u8,
    pub level_idc: u8,
    pub seq_parameter_set_id: u32,
    pub chroma_format_idc: u32,
    pub separate_colour_plane_flag: bool,
    pub bit_depth_luma: u8,
    pub bit_depth_chroma: u8,
    pub log2_max_frame_num: u8,
    pub poc: PocType,
    pub max_num_ref_frames: u32,
    pub gaps_in_frame_num_value_allowed_flag: bool,
    pub pic_width_in_mbs: u32,
    pub pic_height_in_map_units: u32,
    pub frame_mbs_only_flag: bool,
    pub mb_adaptive_frame_field_flag: bool,
    pub direct_8x8_inference_flag: bool,
    /// Crop in luma samples: left, right, top, bottom (already scaled by
    /// chroma units).
    pub crop: (u32, u32, u32, u32),
    pub vui: Option<Vui>,
}

impl Sps {
    /// Coded width in luma samples (before cropping).
    pub fn coded_width(&self) -> u32 {
        self.pic_width_in_mbs * 16
    }

    pub fn coded_height(&self) -> u32 {
        let map_unit = if self.frame_mbs_only_flag { 16 } else { 32 };
        self.pic_height_in_map_units * map_unit
    }

    pub fn display_width(&self) -> u32 {
        self.coded_width() - self.crop.0 - self.crop.1
    }

    pub fn display_height(&self) -> u32 {
        self.coded_height() - self.crop.2 - self.crop.3
    }

    pub fn frame_rate(&self) -> Option<(u32, u32)> {
        let t = self.vui.as_ref()?.timing_info?;
        if t.time_scale == 0 {
            return None;
        }
        // Frame rate = time_scale / (2 * num_units_in_tick) for frame coding.
        let denom = t.num_units_in_tick.checked_mul(2).filter(|&d| d > 0)?;
        Some((t.time_scale, denom))
    }

    pub fn profile_name(&self) -> &'static str {
        match self.profile_idc {
            66 => "Baseline",
            77 => "Main",
            88 => "Extended",
            100 => "High",
            110 => "High 10",
            122 => "High 4:2:2",
            244 => "High 4:4:4 Predictive",
            _ => "Unknown",
        }
    }
}

pub fn parse_sps<S: FieldSink>(r: &mut BitReader<'_, S>) -> Result<Sps, Error> {
    r.group("seq_parameter_set_rbsp", |r| {
        let profile_idc = r.u(8, "profile_idc")? as u8;
        let constraint_set_flags = r.u(6, "constraint_set_flags")? as u8;
        r.u(2, "reserved_zero_2bits")?;
        let level_idc = r.u(8, "level_idc")? as u8;
        let seq_parameter_set_id = r.ue("seq_parameter_set_id")?;

        let mut chroma_format_idc = 1;
        let mut separate_colour_plane_flag = false;
        let mut bit_depth_luma = 8;
        let mut bit_depth_chroma = 8;
        if matches!(
            profile_idc,
            100 | 110 | 122 | 244 | 44 | 83 | 86 | 118 | 128 | 138 | 139 | 134 | 135
        ) {
            chroma_format_idc = r.ue("chroma_format_idc")?;
            if chroma_format_idc == 3 {
                separate_colour_plane_flag = r.flag("separate_colour_plane_flag")?;
            }
            bit_depth_luma = r.ue("bit_depth_luma_minus8")? as u8 + 8;
            bit_depth_chroma = r.ue("bit_depth_chroma_minus8")? as u8 + 8;
            r.flag("qpprime_y_zero_transform_bypass_flag")?;
            if r.flag("seq_scaling_matrix_present_flag")? {
                let count = if chroma_format_idc == 3 { 12 } else { 8 };
                for i in 0..count {
                    if r.flag("seq_scaling_list_present_flag")? {
                        parse_scaling_list(r, "scaling_list", if i < 6 { 16 } else { 64 })?;
                    }
                }
            }
        }

        let frame_num_minus4 = r.ue("log2_max_frame_num_minus4")?;
        if frame_num_minus4 > 12 {
            return Err(crate::Error::OutOfRange {
                field: "log2_max_frame_num_minus4",
                value: frame_num_minus4 as i64,
                allowed: "0..=12",
            });
        }
        let log2_max_frame_num = frame_num_minus4 as u8 + 4;
        let pic_order_cnt_type = r.ue("pic_order_cnt_type")?;
        let poc = match pic_order_cnt_type {
            0 => {
                let lsb_minus4 = r.ue("log2_max_pic_order_cnt_lsb_minus4")?;
                if lsb_minus4 > 12 {
                    return Err(crate::Error::OutOfRange {
                        field: "log2_max_pic_order_cnt_lsb_minus4",
                        value: lsb_minus4 as i64,
                        allowed: "0..=12",
                    });
                }
                PocType::Type0 {
                    log2_max_pic_order_cnt_lsb: lsb_minus4 as u8 + 4,
                }
            }
            1 => {
                let delta_pic_order_always_zero_flag =
                    r.flag("delta_pic_order_always_zero_flag")?;
                let offset_for_non_ref_pic = r.se("offset_for_non_ref_pic")?;
                let offset_for_top_to_bottom_field = r.se("offset_for_top_to_bottom_field")?;
                let n = r.ue("num_ref_frames_in_pic_order_cnt_cycle")?;
                let mut offsets = Vec::with_capacity(n as usize);
                for _ in 0..n {
                    offsets.push(r.se("offset_for_ref_frame")?);
                }
                PocType::Type1 {
                    delta_pic_order_always_zero_flag,
                    offset_for_non_ref_pic,
                    offset_for_top_to_bottom_field,
                    offsets_for_ref_frame: offsets,
                }
            }
            2 => PocType::Type2,
            other => {
                return Err(Error::OutOfRange {
                    field: "pic_order_cnt_type",
                    value: other as i64,
                    allowed: "0..=2",
                })
            }
        };

        let max_num_ref_frames = r.ue("max_num_ref_frames")?;
        let gaps_in_frame_num_value_allowed_flag =
            r.flag("gaps_in_frame_num_value_allowed_flag")?;
        let pic_width_in_mbs = r.ue("pic_width_in_mbs_minus1")? + 1;
        let pic_height_in_map_units = r.ue("pic_height_in_map_units_minus1")? + 1;
        let frame_mbs_only_flag = r.flag("frame_mbs_only_flag")?;
        let mb_adaptive_frame_field_flag = if !frame_mbs_only_flag {
            r.flag("mb_adaptive_frame_field_flag")?
        } else {
            false
        };
        let direct_8x8_inference_flag = r.flag("direct_8x8_inference_flag")?;

        let crop = if r.flag("frame_cropping_flag")? {
            let left = r.ue("frame_crop_left_offset")?;
            let right = r.ue("frame_crop_right_offset")?;
            let top = r.ue("frame_crop_top_offset")?;
            let bottom = r.ue("frame_crop_bottom_offset")?;
            // Scale crop units to luma samples (7-21..7-24).
            let (sub_w, sub_h) = match (chroma_format_idc, separate_colour_plane_flag) {
                (1, false) => (2, 2),
                (2, false) => (2, 1),
                _ => (1, 1),
            };
            let frame_mult = if frame_mbs_only_flag { 1 } else { 2 };
            (
                left * sub_w,
                right * sub_w,
                top * sub_h * frame_mult,
                bottom * sub_h * frame_mult,
            )
        } else {
            (0, 0, 0, 0)
        };

        let vui = if r.flag("vui_parameters_present_flag")? {
            Some(parse_vui(r)?)
        } else {
            None
        };

        Ok(Sps {
            profile_idc,
            constraint_set_flags,
            level_idc,
            seq_parameter_set_id,
            chroma_format_idc,
            separate_colour_plane_flag,
            bit_depth_luma,
            bit_depth_chroma,
            log2_max_frame_num,
            poc,
            max_num_ref_frames,
            gaps_in_frame_num_value_allowed_flag,
            pic_width_in_mbs,
            pic_height_in_map_units,
            frame_mbs_only_flag,
            mb_adaptive_frame_field_flag,
            direct_8x8_inference_flag,
            crop,
            vui,
        })
    })
}

fn parse_vui<S: FieldSink>(r: &mut BitReader<'_, S>) -> Result<Vui, Error> {
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
        if r.flag("timing_info_present_flag")? {
            vui.timing_info = Some(TimingInfo {
                num_units_in_tick: r.u(32, "num_units_in_tick")? as u32,
                time_scale: r.u(32, "time_scale")? as u32,
                fixed_frame_rate_flag: r.flag("fixed_frame_rate_flag")?,
            });
        }
        let nal_hrd_present = r.flag("nal_hrd_parameters_present_flag")?;
        if nal_hrd_present {
            vui.nal_hrd = Some(parse_hrd(r)?);
        }
        let vcl_hrd_present = r.flag("vcl_hrd_parameters_present_flag")?;
        if vcl_hrd_present {
            vui.vcl_hrd = Some(parse_hrd(r)?);
        }
        if nal_hrd_present || vcl_hrd_present {
            vui.low_delay_hrd_flag = r.flag("low_delay_hrd_flag")?;
        }
        vui.pic_struct_present_flag = r.flag("pic_struct_present_flag")?;
        if r.flag("bitstream_restriction_flag")? {
            r.flag("motion_vectors_over_pic_boundaries_flag")?;
            r.ue("max_bytes_per_pic_denom")?;
            r.ue("max_bits_per_mb_denom")?;
            r.ue("log2_max_mv_length_horizontal")?;
            r.ue("log2_max_mv_length_vertical")?;
            vui.max_num_reorder_frames = Some(r.ue("max_num_reorder_frames")?);
            vui.max_dec_frame_buffering = Some(r.ue("max_dec_frame_buffering")?);
        }
        Ok(vui)
    })
}

pub(crate) fn parse_hrd<S: FieldSink>(r: &mut BitReader<'_, S>) -> Result<HrdParams, Error> {
    r.group("hrd_parameters", |r| {
        let cpb_cnt = r.ue("cpb_cnt_minus1")? + 1;
        let bit_rate_scale = r.u(4, "bit_rate_scale")? as u8;
        let cpb_size_scale = r.u(4, "cpb_size_scale")? as u8;
        let mut cpbs = Vec::with_capacity(cpb_cnt as usize);
        for _ in 0..cpb_cnt {
            let bit_rate = r.ue("bit_rate_value_minus1")?;
            let cpb_size = r.ue("cpb_size_value_minus1")?;
            let cbr = r.flag("cbr_flag")?;
            cpbs.push((bit_rate, cpb_size, cbr));
        }
        Ok(HrdParams {
            cpb_cnt,
            bit_rate_scale,
            cpb_size_scale,
            cpbs,
            initial_cpb_removal_delay_length: r.u(5, "initial_cpb_removal_delay_length_minus1")?
                as u8
                + 1,
            cpb_removal_delay_length: r.u(5, "cpb_removal_delay_length_minus1")? as u8 + 1,
            dpb_output_delay_length: r.u(5, "dpb_output_delay_length_minus1")? as u8 + 1,
            time_offset_length: r.u(5, "time_offset_length")? as u8,
        })
    })
}
