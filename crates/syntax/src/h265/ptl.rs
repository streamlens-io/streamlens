//! profile_tier_level() syntax (7.3.3).

use crate::bitread::{BitReader, FieldSink};
use crate::Error;

#[derive(Debug, Clone, Default)]
pub struct ProfileTierLevel {
    pub general_profile_space: u8,
    pub general_tier_flag: bool,
    pub general_profile_idc: u8,
    pub general_level_idc: u8,
}

impl ProfileTierLevel {
    /// Profile name from general_profile_idc (Annex A).
    pub fn profile_name(&self) -> &'static str {
        match self.general_profile_idc {
            1 => "Main",
            2 => "Main 10",
            3 => "Main Still Picture",
            _ => "Unknown",
        }
    }

    /// Level formatted as "major.minor" (general_level_idc / 30).
    pub fn level_name(&self) -> String {
        let idc = self.general_level_idc as u32;
        format!("{}.{}", idc / 30, (idc % 30) / 3)
    }

    /// "Main" or "High" tier (A.4.1).
    pub fn tier_name(&self) -> &'static str {
        if self.general_tier_flag {
            "High"
        } else {
            "Main"
        }
    }
}

/// profile_tier_level(profilePresentFlag, maxNumSubLayersMinus1) — 7.3.3.
pub fn parse_ptl<S: FieldSink>(
    r: &mut BitReader<'_, S>,
    profile_present_flag: bool,
    max_num_sub_layers_minus1: u8,
) -> Result<ProfileTierLevel, Error> {
    r.group("profile_tier_level", |r| {
        let mut ptl = ProfileTierLevel::default();
        if profile_present_flag {
            ptl.general_profile_space = r.u(2, "general_profile_space")? as u8;
            ptl.general_tier_flag = r.flag("general_tier_flag")?;
            ptl.general_profile_idc = r.u(5, "general_profile_idc")? as u8;
            // general_profile_compatibility_flag[32] (7.3.3)
            r.u_skip(32)?;
            // general_progressive_source_flag + general_interlaced_source_flag
            // + general_non_packed_constraint_flag + general_frame_only_constraint_flag
            // + 43 reserved bits + general_inbld_flag = 48 bits total
            r.u_skip(48)?;
        }
        ptl.general_level_idc = r.u(8, "general_level_idc")? as u8;

        // sub_layer_profile_present_flag[i] and sub_layer_level_present_flag[i]
        let mut sub_layer_profile_present = [false; 8];
        let mut sub_layer_level_present = [false; 8];
        for i in 0..max_num_sub_layers_minus1 as usize {
            sub_layer_profile_present[i] = r.flag("sub_layer_profile_present_flag")?;
            sub_layer_level_present[i] = r.flag("sub_layer_level_present_flag")?;
        }
        // reserved_zero_2bits: only present when maxNumSubLayersMinus1 > 0 (7.3.3).
        if max_num_sub_layers_minus1 > 0 {
            for _ in max_num_sub_layers_minus1..8 {
                r.u(2, "reserved_zero_2bits")?;
            }
        }
        // sub-layer profile/level data
        for i in 0..max_num_sub_layers_minus1 as usize {
            if sub_layer_profile_present[i] {
                r.u(2, "sub_layer_profile_space")?;
                r.flag("sub_layer_tier_flag")?;
                r.u(5, "sub_layer_profile_idc")?;
                // compatibility_flags[32] + constraints[48]
                r.u_skip(80)?;
            }
            if sub_layer_level_present[i] {
                r.u(8, "sub_layer_level_idc")?;
            }
        }
        Ok(ptl)
    })
}
