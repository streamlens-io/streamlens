//! Picture parameter set (7.3.2.2).

use crate::bitread::{BitReader, FieldSink};
use crate::h264::parse_scaling_list;
use crate::Error;

#[derive(Debug, Clone)]
pub struct Pps {
    pub pic_parameter_set_id: u32,
    pub seq_parameter_set_id: u32,
    pub entropy_coding_mode_flag: bool,
    pub bottom_field_pic_order_in_frame_present_flag: bool,
    pub num_slice_groups: u32,
    pub num_ref_idx_default_active: [u32; 2],
    pub weighted_pred_flag: bool,
    pub weighted_bipred_idc: u8,
    pub pic_init_qp: i32,
    pub chroma_qp_index_offset: i32,
    pub deblocking_filter_control_present_flag: bool,
    pub constrained_intra_pred_flag: bool,
    pub redundant_pic_cnt_present_flag: bool,
    pub transform_8x8_mode_flag: bool,
    pub second_chroma_qp_index_offset: i32,
}

pub fn parse_pps<S: FieldSink>(r: &mut BitReader<'_, S>) -> Result<Pps, Error> {
    r.group("pic_parameter_set_rbsp", |r| {
        let pic_parameter_set_id = r.ue("pic_parameter_set_id")?;
        let seq_parameter_set_id = r.ue("seq_parameter_set_id")?;
        let entropy_coding_mode_flag = r.flag("entropy_coding_mode_flag")?;
        let bottom_field_pic_order_in_frame_present_flag =
            r.flag("bottom_field_pic_order_in_frame_present_flag")?;

        let num_slice_groups = r.ue("num_slice_groups_minus1")? + 1;
        if num_slice_groups > 1 {
            parse_fmo(r, num_slice_groups)?;
        }

        let l0 = r.ue("num_ref_idx_l0_default_active_minus1")? + 1;
        let l1 = r.ue("num_ref_idx_l1_default_active_minus1")? + 1;
        let weighted_pred_flag = r.flag("weighted_pred_flag")?;
        let weighted_bipred_idc = r.u(2, "weighted_bipred_idc")? as u8;
        let pic_init_qp = r.se("pic_init_qp_minus26")? + 26;
        r.se("pic_init_qs_minus26")?;
        let chroma_qp_index_offset = r.se("chroma_qp_index_offset")?;
        let deblocking_filter_control_present_flag =
            r.flag("deblocking_filter_control_present_flag")?;
        let constrained_intra_pred_flag = r.flag("constrained_intra_pred_flag")?;
        let redundant_pic_cnt_present_flag = r.flag("redundant_pic_cnt_present_flag")?;

        let mut transform_8x8_mode_flag = false;
        let mut second_chroma_qp_index_offset = chroma_qp_index_offset;
        if r.more_rbsp_data() {
            transform_8x8_mode_flag = r.flag("transform_8x8_mode_flag")?;
            if r.flag("pic_scaling_matrix_present_flag")? {
                let count = 6 + if transform_8x8_mode_flag { 2 } else { 0 };
                for i in 0..count {
                    if r.flag("pic_scaling_list_present_flag")? {
                        parse_scaling_list(r, "scaling_list", if i < 6 { 16 } else { 64 })?;
                    }
                }
            }
            second_chroma_qp_index_offset = r.se("second_chroma_qp_index_offset")?;
        }

        Ok(Pps {
            pic_parameter_set_id,
            seq_parameter_set_id,
            entropy_coding_mode_flag,
            bottom_field_pic_order_in_frame_present_flag,
            num_slice_groups,
            num_ref_idx_default_active: [l0, l1],
            weighted_pred_flag,
            weighted_bipred_idc,
            pic_init_qp,
            chroma_qp_index_offset,
            deblocking_filter_control_present_flag,
            constrained_intra_pred_flag,
            redundant_pic_cnt_present_flag,
            transform_8x8_mode_flag,
            second_chroma_qp_index_offset,
        })
    })
}

/// Slice group (FMO) map syntax; values are recorded but not retained (FMO is
/// baseline-profile only and does not affect anything downstream of parsing).
fn parse_fmo<S: FieldSink>(r: &mut BitReader<'_, S>, num_slice_groups: u32) -> Result<(), Error> {
    let map_type = r.ue("slice_group_map_type")?;
    match map_type {
        0 => {
            for _ in 0..num_slice_groups {
                r.ue("run_length_minus1")?;
            }
        }
        1 => {}
        2 => {
            for _ in 0..num_slice_groups - 1 {
                r.ue("top_left")?;
                r.ue("bottom_right")?;
            }
        }
        3..=5 => {
            r.flag("slice_group_change_direction_flag")?;
            r.ue("slice_group_change_rate_minus1")?;
        }
        6 => {
            let n = r.ue("pic_size_in_map_units_minus1")? + 1;
            let bits = 32 - (num_slice_groups - 1).leading_zeros().min(31) as u8;
            let bits = bits.max(1);
            for _ in 0..n {
                r.u(bits, "slice_group_id")?;
            }
        }
        other => {
            return Err(Error::OutOfRange {
                field: "slice_group_map_type",
                value: other as i64,
                allowed: "0..=6",
            })
        }
    }
    Ok(())
}
