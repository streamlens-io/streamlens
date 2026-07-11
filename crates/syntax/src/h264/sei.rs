//! SEI message parsing (7.3.2.3, Annex D). Payloads the app consumes are
//! parsed structurally; the rest keep their type/size for the Stream Viewer.

use crate::bitread::{BitReader, FieldSink};
use crate::h264::Sps;
use crate::Error;

#[derive(Debug, Clone)]
pub enum SeiPayload {
    BufferingPeriod {
        seq_parameter_set_id: u32,
        /// (initial_cpb_removal_delay, initial_cpb_removal_delay_offset) per
        /// CPB of the NAL HRD, then the VCL HRD.
        nal_cpb: Vec<(u32, u32)>,
        vcl_cpb: Vec<(u32, u32)>,
    },
    PicTiming {
        cpb_removal_delay: u32,
        dpb_output_delay: u32,
        pic_struct: Option<u8>,
    },
    RecoveryPoint {
        recovery_frame_cnt: u32,
        exact_match_flag: bool,
    },
    /// Unparsed payload (type, size in bytes).
    Other { payload_type: u32, size: u32 },
}

#[derive(Debug, Clone)]
pub struct SeiMessage {
    pub payload_type: u32,
    pub payload: SeiPayload,
}

impl SeiMessage {
    pub fn type_name(&self) -> &'static str {
        match self.payload_type {
            0 => "buffering_period",
            1 => "pic_timing",
            2 => "pan_scan_rect",
            3 => "filler_payload",
            4 => "user_data_registered_itu_t_t35",
            5 => "user_data_unregistered",
            6 => "recovery_point",
            45 => "frame_packing_arrangement",
            129 => "active_parameter_sets",
            137 => "mastering_display_colour_volume",
            144 => "content_light_level_info",
            _ => "sei_payload",
        }
    }
}

/// Parses all SEI messages of a SEI NAL. `sps` is the currently-active (or
/// only) SPS; `pic_timing` cannot be decoded without its HRD lengths.
pub fn parse_sei<S: FieldSink>(
    r: &mut BitReader<'_, S>,
    sps: Option<&Sps>,
) -> Result<Vec<SeiMessage>, Error> {
    let mut messages = Vec::new();
    while r.more_rbsp_data() {
        let msg = r.group("sei_message", |r| {
            let mut payload_type = 0u32;
            loop {
                let b = r.u(8, "payload_type_byte")?;
                payload_type += b as u32;
                if b != 0xFF {
                    break;
                }
            }
            let mut payload_size = 0u32;
            loop {
                let b = r.u(8, "payload_size_byte")?;
                payload_size += b as u32;
                if b != 0xFF {
                    break;
                }
            }

            let start_bit = r.bit_pos();
            let payload = parse_payload(r, payload_type, payload_size, sps)?;
            // Skip to the declared payload end regardless of how much the
            // payload parser consumed (robustness against unknown extensions).
            let consumed = r.bit_pos() - start_bit;
            let total = payload_size as usize * 8;
            if consumed < total {
                r.u_skip(total - consumed)?;
            }
            Ok(SeiMessage {
                payload_type,
                payload,
            })
        })?;
        messages.push(msg);
    }
    Ok(messages)
}

fn parse_payload<S: FieldSink>(
    r: &mut BitReader<'_, S>,
    payload_type: u32,
    payload_size: u32,
    sps: Option<&Sps>,
) -> Result<SeiPayload, Error> {
    match payload_type {
        0 => {
            let seq_parameter_set_id = r.ue("seq_parameter_set_id")?;
            let mut nal_cpb = Vec::new();
            let mut vcl_cpb = Vec::new();
            if let Some(vui) = sps.and_then(|s| s.vui.as_ref()) {
                for (hrd, out) in [(&vui.nal_hrd, &mut nal_cpb), (&vui.vcl_hrd, &mut vcl_cpb)] {
                    if let Some(hrd) = hrd {
                        let len = hrd.initial_cpb_removal_delay_length;
                        for _ in 0..hrd.cpb_cnt {
                            let delay = r.u(len, "initial_cpb_removal_delay")? as u32;
                            let offset = r.u(len, "initial_cpb_removal_delay_offset")? as u32;
                            out.push((delay, offset));
                        }
                    }
                }
            }
            Ok(SeiPayload::BufferingPeriod {
                seq_parameter_set_id,
                nal_cpb,
                vcl_cpb,
            })
        }
        1 => {
            let vui = sps.and_then(|s| s.vui.as_ref());
            let hrd = vui.and_then(|v| v.nal_hrd.as_ref().or(v.vcl_hrd.as_ref()));
            let (mut cpb_removal_delay, mut dpb_output_delay) = (0, 0);
            if let Some(hrd) = hrd {
                cpb_removal_delay = r.u(hrd.cpb_removal_delay_length, "cpb_removal_delay")? as u32;
                dpb_output_delay = r.u(hrd.dpb_output_delay_length, "dpb_output_delay")? as u32;
            }
            let pic_struct = match vui {
                Some(v) if v.pic_struct_present_flag => Some(r.u(4, "pic_struct")? as u8),
                _ => None,
            };
            Ok(SeiPayload::PicTiming {
                cpb_removal_delay,
                dpb_output_delay,
                pic_struct,
            })
        }
        6 => {
            let recovery_frame_cnt = r.ue("recovery_frame_cnt")?;
            let exact_match_flag = r.flag("exact_match_flag")?;
            r.flag("broken_link_flag")?;
            r.u(2, "changing_slice_group_idc")?;
            Ok(SeiPayload::RecoveryPoint {
                recovery_frame_cnt,
                exact_match_flag,
            })
        }
        _ => Ok(SeiPayload::Other {
            payload_type,
            size: payload_size,
        }),
    }
}
