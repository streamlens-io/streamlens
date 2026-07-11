//! HEVC SEI message parsing (D.2 / Annex D).

use crate::bitread::{BitReader, FieldSink};
use crate::h265::sps::{HrdParams, Sps};
use crate::Error;

#[derive(Debug, Clone)]
pub enum SeiPayload {
    BufferingPeriod {
        bp_seq_parameter_set_id: u32,
        /// initial_cpb_removal_delay of the first CPB entry.
        initial_cpb_removal_delay: Option<u32>,
        initial_cpb_removal_offset: Option<u32>,
    },
    PicTiming {
        au_cpb_removal_delay_minus1: u32,
        pic_dpb_output_delay: u32,
        pic_struct: Option<u8>,
    },
    RecoveryPoint {
        recovery_poc_cnt: i32,
        exact_match_flag: bool,
        broken_link_flag: bool,
    },
    MasteringDisplayColourVolume {
        display_primaries: [[u16; 2]; 3],
        white_point: [u16; 2],
        max_display_mastering_luminance: u32,
        min_display_mastering_luminance: u32,
    },
    ContentLightLevel {
        max_content_light_level: u16,
        max_pic_average_light_level: u16,
    },
    Other {
        payload_type: u32,
        size: u32,
    },
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
            6 => "recovery_point",
            129 => "active_parameter_sets",
            137 => "mastering_display_colour_volume",
            144 => "content_light_level_info",
            _ => "sei_payload",
        }
    }
}

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

fn hrd(sps: Option<&Sps>) -> Option<&HrdParams> {
    sps?.vui.as_ref()?.hrd.as_ref()
}

fn parse_payload<S: FieldSink>(
    r: &mut BitReader<'_, S>,
    payload_type: u32,
    payload_size: u32,
    sps: Option<&Sps>,
) -> Result<SeiPayload, Error> {
    match payload_type {
        // buffering_period (D.2.2)
        0 => {
            let bp_seq_parameter_set_id = r.ue("bp_seq_parameter_set_id")?;
            let mut initial_cpb_removal_delay = None;
            let mut initial_cpb_removal_offset = None;
            if let Some(h) = hrd(sps) {
                let len = h.initial_cpb_removal_delay_length;
                initial_cpb_removal_delay = Some(r.u(len, "nal_initial_cpb_removal_delay")? as u32);
                initial_cpb_removal_offset =
                    Some(r.u(len, "nal_initial_cpb_removal_offset")? as u32);
            }
            Ok(SeiPayload::BufferingPeriod {
                bp_seq_parameter_set_id,
                initial_cpb_removal_delay,
                initial_cpb_removal_offset,
            })
        }
        // pic_timing (D.2.3)
        1 => {
            let h = hrd(sps);
            let au_len = h.map(|h| h.au_cpb_removal_delay_length).unwrap_or(24);
            let out_len = h.map(|h| h.dpb_output_delay_length).unwrap_or(24);
            let au_cpb_removal_delay_minus1 = r.u(au_len, "au_cpb_removal_delay_minus1")? as u32;
            let pic_dpb_output_delay = r.u(out_len, "pic_dpb_output_delay")? as u32;
            let pic_struct = match sps.and_then(|s| s.vui.as_ref()) {
                Some(v) if v.frame_field_info_present_flag => Some(r.u(4, "pic_struct")? as u8),
                _ => None,
            };
            Ok(SeiPayload::PicTiming {
                au_cpb_removal_delay_minus1,
                pic_dpb_output_delay,
                pic_struct,
            })
        }
        // recovery_point (D.2.8)
        6 => {
            let recovery_poc_cnt = r.se("recovery_poc_cnt")?;
            let exact_match_flag = r.flag("exact_match_flag")?;
            let broken_link_flag = r.flag("broken_link_flag")?;
            Ok(SeiPayload::RecoveryPoint {
                recovery_poc_cnt,
                exact_match_flag,
                broken_link_flag,
            })
        }
        // mastering_display_colour_volume (D.2.28 / SMPTE ST 2086)
        137 => {
            let mut display_primaries = [[0u16; 2]; 3];
            for p in &mut display_primaries {
                p[0] = r.u(16, "display_primaries_x")? as u16;
                p[1] = r.u(16, "display_primaries_y")? as u16;
            }
            let white_point = [
                r.u(16, "white_point_x")? as u16,
                r.u(16, "white_point_y")? as u16,
            ];
            let max_display_mastering_luminance =
                r.u(32, "max_display_mastering_luminance")? as u32;
            let min_display_mastering_luminance =
                r.u(32, "min_display_mastering_luminance")? as u32;
            Ok(SeiPayload::MasteringDisplayColourVolume {
                display_primaries,
                white_point,
                max_display_mastering_luminance,
                min_display_mastering_luminance,
            })
        }
        // content_light_level_info (D.2.35)
        144 => {
            let max_content_light_level = r.u(16, "max_content_light_level")? as u16;
            let max_pic_average_light_level = r.u(16, "max_pic_average_light_level")? as u16;
            Ok(SeiPayload::ContentLightLevel {
                max_content_light_level,
                max_pic_average_light_level,
            })
        }
        _ => Ok(SeiPayload::Other {
            payload_type,
            size: payload_size,
        }),
    }
}
