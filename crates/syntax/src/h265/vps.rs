//! Video parameter set (7.3.2.1).

use crate::bitread::{BitReader, FieldSink};
use crate::h265::ptl::{parse_ptl, ProfileTierLevel};
use crate::Error;

#[derive(Debug, Clone)]
pub struct Vps {
    pub vps_id: u8,
    pub max_layers: u8,
    pub max_sub_layers: u8,
    pub temporal_id_nesting_flag: bool,
    pub ptl: ProfileTierLevel,
    /// max_dec_pic_buffering_minus1 per sub-layer (index = sub_layer_id).
    pub max_dec_pic_buffering: Vec<u32>,
}

pub fn parse_vps<S: FieldSink>(r: &mut BitReader<'_, S>) -> Result<Vps, Error> {
    r.group("video_parameter_set_rbsp", |r| {
        let vps_id = r.u(4, "vps_video_parameter_set_id")? as u8;
        r.u(2, "vps_reserved_three_2bits")?;
        let max_layers = r.u(6, "vps_max_layers_minus1")? as u8 + 1;
        let max_sub_layers_minus1 = r.u(3, "vps_max_sub_layers_minus1")? as u8;
        let max_sub_layers = max_sub_layers_minus1 + 1;
        let temporal_id_nesting_flag = r.flag("vps_temporal_id_nesting_flag")?;
        r.u(16, "vps_reserved_0xffff_16bits")?;

        let ptl = parse_ptl(r, true, max_sub_layers_minus1)?;

        let sub_layer_ordering_info_present_flag =
            r.flag("vps_sub_layer_ordering_info_present_flag")?;
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
            *entry = r.ue("vps_max_dec_pic_buffering_minus1")? + 1;
            r.ue("vps_max_num_reorder_pics")?;
            r.ue("vps_max_latency_increase_plus1")?;
        }
        if !sub_layer_ordering_info_present_flag && max_sub_layers > 1 {
            // Fill lower layers with the highest layer value.
            let v = max_dec_pic_buffering[max_sub_layers_minus1 as usize];
            for entry in max_dec_pic_buffering
                .iter_mut()
                .take(max_sub_layers_minus1 as usize)
            {
                *entry = v;
            }
        }

        r.u(6, "vps_max_layer_id")?;
        let vps_num_layer_sets_minus1 = r.ue("vps_num_layer_sets_minus1")?;
        // layer_id_included_flag — skip the whole matrix
        for _ in 0..vps_num_layer_sets_minus1 {
            // max_layer_id + 1 bits per set, but we don't track layer ids;
            // skip conservatively by reading individual flags.
            // vps_max_layer_id was read as 6 bits above.
            for _ in 0..max_layers {
                r.flag("layer_id_included_flag")?;
            }
        }

        // vps_timing_info_present_flag + extension: skip gracefully.
        if r.more_rbsp_data() {
            let timing = r.flag("vps_timing_info_present_flag")?;
            if timing {
                r.u(32, "vps_num_units_in_tick")?;
                r.u(32, "vps_time_scale")?;
                let poc_prop = r.flag("vps_poc_proportional_to_timing_flag")?;
                if poc_prop {
                    r.ue("vps_num_ticks_poc_diff_one_minus1")?;
                }
                let num_hrd = r.ue("vps_num_hrd_parameters")?;
                for _ in 0..num_hrd {
                    r.ue("hrd_layer_set_idx")?;
                    r.flag("cprms_present_flag")?;
                    // hrd_parameters(): too complex to parse fully; skip remaining.
                    // We record extension bytes as skipped.
                }
            }
            // Record remaining RBSP data as skipped (vps_extension_flag etc.)
            let rem = r.bits_left();
            if rem > 1 {
                r.u_skip(rem - 1)?;
            }
        }

        Ok(Vps {
            vps_id,
            max_layers,
            max_sub_layers,
            temporal_id_nesting_flag,
            ptl,
            max_dec_pic_buffering,
        })
    })
}
