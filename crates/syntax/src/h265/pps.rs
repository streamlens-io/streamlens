//! Picture parameter set (7.3.2.3).

use crate::bitread::{BitReader, FieldSink};
use crate::Error;

#[derive(Debug, Clone)]
pub struct Pps {
    pub pps_pic_parameter_set_id: u32,
    pub pps_seq_parameter_set_id: u32,
    pub dependent_slice_segments_enabled_flag: bool,
    pub output_flag_present_flag: bool,
    pub num_extra_slice_header_bits: u8,
    pub sign_data_hiding_enabled_flag: bool,
    pub cabac_init_present_flag: bool,
    pub num_ref_idx_l0_default_active: u32,
    pub num_ref_idx_l1_default_active: u32,
    pub init_qp_minus26: i32,
    pub constrained_intra_pred_flag: bool,
    pub transform_skip_enabled_flag: bool,
    pub cu_qp_delta_enabled_flag: bool,
    pub diff_cu_qp_delta_depth: u32,
    pub pps_cb_qp_offset: i32,
    pub pps_cr_qp_offset: i32,
    pub pps_slice_chroma_qp_offsets_present_flag: bool,
    pub weighted_pred_flag: bool,
    pub weighted_bipred_flag: bool,
    pub transquant_bypass_enabled_flag: bool,
    pub tiles_enabled_flag: bool,
    pub num_tile_columns: u32,
    pub num_tile_rows: u32,
    pub entropy_coding_sync_enabled_flag: bool,
    pub loop_filter_across_slices_enabled_flag: bool,
    pub deblocking_filter_control_present_flag: bool,
    pub deblocking_filter_disabled_flag: bool,
    pub pps_beta_offset_div2: i32,
    pub pps_tc_offset_div2: i32,
    pub lists_modification_present_flag: bool,
    pub log2_parallel_merge_level_minus2: u32,
    pub slice_segment_header_extension_present_flag: bool,
}

pub fn parse_pps<S: FieldSink>(r: &mut BitReader<'_, S>) -> Result<Pps, Error> {
    r.group("pic_parameter_set_rbsp", |r| {
        let pps_pic_parameter_set_id = r.ue("pps_pic_parameter_set_id")?;
        let pps_seq_parameter_set_id = r.ue("pps_seq_parameter_set_id")?;
        let dependent_slice_segments_enabled_flag =
            r.flag("dependent_slice_segments_enabled_flag")?;
        let output_flag_present_flag = r.flag("output_flag_present_flag")?;
        let num_extra_slice_header_bits = r.u(3, "num_extra_slice_header_bits")? as u8;
        let sign_data_hiding_enabled_flag = r.flag("sign_data_hiding_enabled_flag")?;
        let cabac_init_present_flag = r.flag("cabac_init_present_flag")?;
        let num_ref_idx_l0_default_active = r.ue("num_ref_idx_l0_default_active_minus1")? + 1;
        let num_ref_idx_l1_default_active = r.ue("num_ref_idx_l1_default_active_minus1")? + 1;
        let init_qp_minus26 = r.se("init_qp_minus26")?;
        let constrained_intra_pred_flag = r.flag("constrained_intra_pred_flag")?;
        let transform_skip_enabled_flag = r.flag("transform_skip_enabled_flag")?;
        let cu_qp_delta_enabled_flag = r.flag("cu_qp_delta_enabled_flag")?;
        let diff_cu_qp_delta_depth = if cu_qp_delta_enabled_flag {
            r.ue("diff_cu_qp_delta_depth")?
        } else {
            0
        };
        let pps_cb_qp_offset = r.se("pps_cb_qp_offset")?;
        let pps_cr_qp_offset = r.se("pps_cr_qp_offset")?;
        let pps_slice_chroma_qp_offsets_present_flag =
            r.flag("pps_slice_chroma_qp_offsets_present_flag")?;
        let weighted_pred_flag = r.flag("weighted_pred_flag")?;
        let weighted_bipred_flag = r.flag("weighted_bipred_flag")?;
        let transquant_bypass_enabled_flag = r.flag("transquant_bypass_enabled_flag")?;
        let tiles_enabled_flag = r.flag("tiles_enabled_flag")?;
        let entropy_coding_sync_enabled_flag = r.flag("entropy_coding_sync_enabled_flag")?;

        let mut num_tile_columns = 1u32;
        let mut num_tile_rows = 1u32;
        if tiles_enabled_flag {
            num_tile_columns = r.ue("num_tile_columns_minus1")? + 1;
            num_tile_rows = r.ue("num_tile_rows_minus1")? + 1;
            let uniform_spacing = r.flag("uniform_spacing_flag")?;
            if !uniform_spacing {
                for _ in 0..num_tile_columns - 1 {
                    r.ue("column_width_minus1")?;
                }
                for _ in 0..num_tile_rows - 1 {
                    r.ue("row_height_minus1")?;
                }
            }
            r.flag("loop_filter_across_tiles_enabled_flag")?;
        }

        let loop_filter_across_slices_enabled_flag =
            r.flag("pps_loop_filter_across_slices_enabled_flag")?;

        let mut deblocking_filter_disabled_flag = false;
        let mut pps_beta_offset_div2 = 0i32;
        let mut pps_tc_offset_div2 = 0i32;
        let deblocking_filter_control_present_flag =
            r.flag("deblocking_filter_control_present_flag")?;
        if deblocking_filter_control_present_flag {
            let override_enabled = r.flag("deblocking_filter_override_enabled_flag")?;
            deblocking_filter_disabled_flag = r.flag("pps_deblocking_filter_disabled_flag")?;
            if !deblocking_filter_disabled_flag {
                pps_beta_offset_div2 = r.se("pps_beta_offset_div2")?;
                pps_tc_offset_div2 = r.se("pps_tc_offset_div2")?;
            }
            let _ = override_enabled;
        }

        if r.flag("pps_scaling_list_data_present_flag")? {
            parse_pps_scaling_list_data(r)?;
        }

        let lists_modification_present_flag = r.flag("lists_modification_present_flag")?;
        let log2_parallel_merge_level_minus2 = r.ue("log2_parallel_merge_level_minus2")?;
        let slice_segment_header_extension_present_flag =
            r.flag("slice_segment_header_extension_present_flag")?;

        // pps_extension_flag — skip.
        if r.more_rbsp_data() {
            let rem = r.bits_left();
            if rem > 1 {
                r.u_skip(rem - 1)?;
            }
        }

        Ok(Pps {
            pps_pic_parameter_set_id,
            pps_seq_parameter_set_id,
            dependent_slice_segments_enabled_flag,
            output_flag_present_flag,
            num_extra_slice_header_bits,
            sign_data_hiding_enabled_flag,
            cabac_init_present_flag,
            num_ref_idx_l0_default_active,
            num_ref_idx_l1_default_active,
            init_qp_minus26,
            constrained_intra_pred_flag,
            transform_skip_enabled_flag,
            cu_qp_delta_enabled_flag,
            diff_cu_qp_delta_depth,
            pps_cb_qp_offset,
            pps_cr_qp_offset,
            pps_slice_chroma_qp_offsets_present_flag,
            weighted_pred_flag,
            weighted_bipred_flag,
            transquant_bypass_enabled_flag,
            tiles_enabled_flag,
            num_tile_columns,
            num_tile_rows,
            entropy_coding_sync_enabled_flag,
            loop_filter_across_slices_enabled_flag,
            deblocking_filter_control_present_flag,
            deblocking_filter_disabled_flag,
            pps_beta_offset_div2,
            pps_tc_offset_div2,
            lists_modification_present_flag,
            log2_parallel_merge_level_minus2,
            slice_segment_header_extension_present_flag,
        })
    })
}

/// PPS uses same scaling_list_data() structure as SPS.
fn parse_pps_scaling_list_data<S: FieldSink>(r: &mut BitReader<'_, S>) -> Result<(), Error> {
    r.group("scaling_list_data", |r| {
        for size_id in 0..4usize {
            let matrix_count = if size_id == 3 { 2 } else { 6 };
            for _ in 0..matrix_count {
                let pred_mode_flag = r.flag("scaling_list_pred_mode_flag")?;
                if !pred_mode_flag {
                    r.ue("scaling_list_pred_matrix_id_delta")?;
                } else {
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
