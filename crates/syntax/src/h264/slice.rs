//! Slice header (7.3.3) — parsed up to (excluding) slice data.

use crate::bitread::{BitReader, FieldSink};
use crate::h264::sps::PocType;
use crate::h264::{NalHeader, NalType, Pps, Sps};
use crate::Error;

/// Slice type (Table 7-6, modulo 5).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SliceType {
    P,
    B,
    I,
    Sp,
    Si,
}

impl SliceType {
    pub fn from_raw(v: u32) -> Result<Self, Error> {
        Ok(match v % 5 {
            0 => SliceType::P,
            1 => SliceType::B,
            2 => SliceType::I,
            3 => SliceType::Sp,
            4 => SliceType::Si,
            _ => unreachable!(),
        })
    }
}

/// One ref_pic_list_modification operation.
#[derive(Debug, Clone, Copy)]
pub struct RefListMod {
    pub idc: u32,
    pub value: u32,
}

/// One memory_management_control_operation.
#[derive(Debug, Clone, Copy)]
pub struct Mmco {
    pub op: u32,
    pub args: [u32; 2],
}

#[derive(Debug, Clone)]
pub struct SliceHeader {
    pub first_mb_in_slice: u32,
    pub slice_type: SliceType,
    /// Raw slice_type value (5..9 means "all slices in picture share type").
    pub slice_type_raw: u32,
    pub pic_parameter_set_id: u32,
    pub frame_num: u32,
    pub field_pic_flag: bool,
    pub bottom_field_flag: bool,
    pub idr_pic_id: Option<u32>,
    pub pic_order_cnt_lsb: u32,
    pub delta_pic_order_cnt_bottom: i32,
    pub delta_pic_order_cnt: [i32; 2],
    pub num_ref_idx_active: [u32; 2],
    pub ref_list_mods: [Vec<RefListMod>; 2],
    pub mmcos: Vec<Mmco>,
    /// IDR: (no_output_of_prior_pics_flag, long_term_reference_flag).
    pub idr_marking: Option<(bool, bool)>,
    pub adaptive_ref_pic_marking: bool,
    /// SliceQPY = 26 + pic_init_qp_minus26 + slice_qp_delta.
    pub slice_qp: i32,
    pub cabac_init_idc: Option<u32>,
    pub disable_deblocking_filter_idc: u32,
}

impl SliceHeader {
    pub fn is_field(&self) -> bool {
        self.field_pic_flag
    }
}

pub fn parse_slice_header<S: FieldSink>(
    r: &mut BitReader<'_, S>,
    nal: NalHeader,
    sps: &Sps,
    pps: &Pps,
) -> Result<SliceHeader, Error> {
    r.group("slice_header", |r| {
        let idr = nal.nal_type == NalType::SliceIdr;
        let first_mb_in_slice = r.ue("first_mb_in_slice")?;
        let slice_type_raw = r.ue("slice_type")?;
        if slice_type_raw > 9 {
            return Err(Error::OutOfRange {
                field: "slice_type",
                value: slice_type_raw as i64,
                allowed: "0..=9",
            });
        }
        let slice_type = SliceType::from_raw(slice_type_raw)?;
        let pic_parameter_set_id = r.ue("pic_parameter_set_id")?;
        if sps.separate_colour_plane_flag {
            r.u(2, "colour_plane_id")?;
        }
        let frame_num = r.u(sps.log2_max_frame_num, "frame_num")? as u32;

        let mut field_pic_flag = false;
        let mut bottom_field_flag = false;
        if !sps.frame_mbs_only_flag {
            field_pic_flag = r.flag("field_pic_flag")?;
            if field_pic_flag {
                bottom_field_flag = r.flag("bottom_field_flag")?;
            }
        }

        let idr_pic_id = if idr { Some(r.ue("idr_pic_id")?) } else { None };

        let mut pic_order_cnt_lsb = 0;
        let mut delta_pic_order_cnt_bottom = 0;
        let mut delta_pic_order_cnt = [0i32; 2];
        match &sps.poc {
            PocType::Type0 {
                log2_max_pic_order_cnt_lsb,
            } => {
                pic_order_cnt_lsb = r.u(*log2_max_pic_order_cnt_lsb, "pic_order_cnt_lsb")? as u32;
                if pps.bottom_field_pic_order_in_frame_present_flag && !field_pic_flag {
                    delta_pic_order_cnt_bottom = r.se("delta_pic_order_cnt_bottom")?;
                }
            }
            PocType::Type1 {
                delta_pic_order_always_zero_flag,
                ..
            } => {
                if !delta_pic_order_always_zero_flag {
                    delta_pic_order_cnt[0] = r.se("delta_pic_order_cnt[0]")?;
                    if pps.bottom_field_pic_order_in_frame_present_flag && !field_pic_flag {
                        delta_pic_order_cnt[1] = r.se("delta_pic_order_cnt[1]")?;
                    }
                }
            }
            PocType::Type2 => {}
        }

        if pps.redundant_pic_cnt_present_flag {
            r.ue("redundant_pic_cnt")?;
        }

        if slice_type == SliceType::B {
            r.flag("direct_spatial_mv_pred_flag")?;
        }

        let mut num_ref_idx_active = pps.num_ref_idx_default_active;
        if matches!(slice_type, SliceType::P | SliceType::Sp | SliceType::B)
            && r.flag("num_ref_idx_active_override_flag")?
        {
            num_ref_idx_active[0] = r.ue("num_ref_idx_l0_active_minus1")? + 1;
            if slice_type == SliceType::B {
                num_ref_idx_active[1] = r.ue("num_ref_idx_l1_active_minus1")? + 1;
            }
        }
        if slice_type != SliceType::B {
            num_ref_idx_active[1] = 0;
        }
        if matches!(slice_type, SliceType::I | SliceType::Si) {
            num_ref_idx_active = [0, 0];
        }

        // ref_pic_list_modification (7.3.3.1)
        let mut ref_list_mods: [Vec<RefListMod>; 2] = [Vec::new(), Vec::new()];
        if !matches!(slice_type, SliceType::I | SliceType::Si) {
            ref_list_mods[0] = parse_ref_list_mod(r, "ref_pic_list_modification_flag_l0")?;
        }
        if slice_type == SliceType::B {
            ref_list_mods[1] = parse_ref_list_mod(r, "ref_pic_list_modification_flag_l1")?;
        }

        // pred_weight_table (7.3.3.2)
        if (pps.weighted_pred_flag && matches!(slice_type, SliceType::P | SliceType::Sp))
            || (pps.weighted_bipred_idc == 1 && slice_type == SliceType::B)
        {
            parse_pred_weight_table(r, sps, slice_type, &num_ref_idx_active)?;
        }

        // dec_ref_pic_marking (7.3.3.3)
        let mut mmcos = Vec::new();
        let mut idr_marking = None;
        let mut adaptive_ref_pic_marking = false;
        if nal.nal_ref_idc != 0 {
            r.group("dec_ref_pic_marking", |r| {
                if idr {
                    idr_marking = Some((
                        r.flag("no_output_of_prior_pics_flag")?,
                        r.flag("long_term_reference_flag")?,
                    ));
                } else {
                    adaptive_ref_pic_marking = r.flag("adaptive_ref_pic_marking_mode_flag")?;
                    if adaptive_ref_pic_marking {
                        loop {
                            let op = r.ue("memory_management_control_operation")?;
                            if op == 0 {
                                break;
                            }
                            if op > 6 {
                                return Err(Error::OutOfRange {
                                    field: "memory_management_control_operation",
                                    value: op as i64,
                                    allowed: "0..=6",
                                });
                            }
                            let mut args = [0u32; 2];
                            if matches!(op, 1 | 3) {
                                args[0] = r.ue("difference_of_pic_nums_minus1")?;
                            }
                            if op == 2 {
                                args[0] = r.ue("long_term_pic_num")?;
                            }
                            if matches!(op, 3 | 6) {
                                args[1] = r.ue("long_term_frame_idx")?;
                            }
                            if op == 4 {
                                args[0] = r.ue("max_long_term_frame_idx_plus1")?;
                            }
                            mmcos.push(Mmco { op, args });
                        }
                    }
                }
                Ok(())
            })?;
        }

        let cabac_init_idc = if pps.entropy_coding_mode_flag
            && !matches!(slice_type, SliceType::I | SliceType::Si)
        {
            Some(r.ue("cabac_init_idc")?)
        } else {
            None
        };

        let slice_qp_delta = r.se("slice_qp_delta")?;
        let slice_qp = pps.pic_init_qp + slice_qp_delta;

        if slice_type == SliceType::Sp {
            r.flag("sp_for_switch_flag")?;
        }
        if matches!(slice_type, SliceType::Sp | SliceType::Si) {
            r.se("slice_qs_delta")?;
        }

        let mut disable_deblocking_filter_idc = 0;
        if pps.deblocking_filter_control_present_flag {
            disable_deblocking_filter_idc = r.ue("disable_deblocking_filter_idc")?;
            if disable_deblocking_filter_idc != 1 {
                r.se("slice_alpha_c0_offset_div2")?;
                r.se("slice_beta_offset_div2")?;
            }
        }
        // slice_group_change_cycle (FMO, map types 3..5) intentionally not
        // parsed: requires PicSizeInMapUnits math and nothing downstream
        // needs it; the header is complete for all non-FMO streams.

        Ok(SliceHeader {
            first_mb_in_slice,
            slice_type,
            slice_type_raw,
            pic_parameter_set_id,
            frame_num,
            field_pic_flag,
            bottom_field_flag,
            idr_pic_id,
            pic_order_cnt_lsb,
            delta_pic_order_cnt_bottom,
            delta_pic_order_cnt,
            num_ref_idx_active,
            ref_list_mods,
            mmcos,
            idr_marking,
            adaptive_ref_pic_marking,
            slice_qp,
            cabac_init_idc,
            disable_deblocking_filter_idc,
        })
    })
}

fn parse_ref_list_mod<S: FieldSink>(
    r: &mut BitReader<'_, S>,
    flag_name: &str,
) -> Result<Vec<RefListMod>, Error> {
    let mut mods = Vec::new();
    if r.flag(flag_name)? {
        loop {
            let idc = r.ue("modification_of_pic_nums_idc")?;
            if idc == 3 {
                break;
            }
            let value = match idc {
                0 | 1 => r.ue("abs_diff_pic_num_minus1")?,
                2 => r.ue("long_term_pic_num")?,
                other => {
                    return Err(Error::OutOfRange {
                        field: "modification_of_pic_nums_idc",
                        value: other as i64,
                        allowed: "0..=3",
                    })
                }
            };
            mods.push(RefListMod { idc, value });
        }
    }
    Ok(mods)
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
            r.ue("chroma_log2_weight_denom")?;
        }
        let lists = if slice_type == SliceType::B { 2 } else { 1 };
        for &n_active in num_ref_idx_active.iter().take(lists) {
            for _ in 0..n_active {
                if r.flag("luma_weight_flag")? {
                    r.se("luma_weight")?;
                    r.se("luma_offset")?;
                }
                if chroma && r.flag("chroma_weight_flag")? {
                    for _ in 0..2 {
                        r.se("chroma_weight")?;
                        r.se("chroma_offset")?;
                    }
                }
            }
        }
        Ok(())
    })
}
