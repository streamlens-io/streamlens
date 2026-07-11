//! H.264 (ITU-T Rec. H.264 / ISO 14496-10) syntax parsing.

pub mod analyzer;
pub mod dpb;
pub mod poc;
pub mod pps;
pub mod sei;
pub mod slice;
pub mod sps;

pub use analyzer::H264Analyzer;

pub use pps::Pps;
pub use sei::SeiMessage;
pub use slice::SliceHeader;
pub use sps::Sps;

use crate::bitread::{BitReader, FieldSink};
use crate::Error;

/// H.264 nal_unit_type values (Table 7-1).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NalType {
    SliceNonIdr,
    SlicePartitionA,
    SlicePartitionB,
    SlicePartitionC,
    SliceIdr,
    Sei,
    Sps,
    Pps,
    AccessUnitDelimiter,
    EndOfSequence,
    EndOfStream,
    FillerData,
    SpsExtension,
    Prefix,
    SubsetSps,
    SliceAux,
    SliceExtension,
    Other(u8),
}

impl NalType {
    pub fn from_raw(v: u8) -> Self {
        match v {
            1 => NalType::SliceNonIdr,
            2 => NalType::SlicePartitionA,
            3 => NalType::SlicePartitionB,
            4 => NalType::SlicePartitionC,
            5 => NalType::SliceIdr,
            6 => NalType::Sei,
            7 => NalType::Sps,
            8 => NalType::Pps,
            9 => NalType::AccessUnitDelimiter,
            10 => NalType::EndOfSequence,
            11 => NalType::EndOfStream,
            12 => NalType::FillerData,
            13 => NalType::SpsExtension,
            14 => NalType::Prefix,
            15 => NalType::SubsetSps,
            19 => NalType::SliceAux,
            20 => NalType::SliceExtension,
            other => NalType::Other(other),
        }
    }

    pub fn is_vcl(&self) -> bool {
        matches!(
            self,
            NalType::SliceNonIdr
                | NalType::SlicePartitionA
                | NalType::SliceIdr
                | NalType::SliceAux
                | NalType::SliceExtension
        )
    }

    /// Human-readable name for the Stream Viewer.
    pub fn name(&self) -> &'static str {
        match self {
            NalType::SliceNonIdr => "Coded slice (non-IDR)",
            NalType::SlicePartitionA => "Slice data partition A",
            NalType::SlicePartitionB => "Slice data partition B",
            NalType::SlicePartitionC => "Slice data partition C",
            NalType::SliceIdr => "Coded slice (IDR)",
            NalType::Sei => "SEI",
            NalType::Sps => "SPS",
            NalType::Pps => "PPS",
            NalType::AccessUnitDelimiter => "Access unit delimiter",
            NalType::EndOfSequence => "End of sequence",
            NalType::EndOfStream => "End of stream",
            NalType::FillerData => "Filler data",
            NalType::SpsExtension => "SPS extension",
            NalType::Prefix => "Prefix NAL",
            NalType::SubsetSps => "Subset SPS",
            NalType::SliceAux => "Coded slice (auxiliary)",
            NalType::SliceExtension => "Coded slice extension",
            NalType::Other(_) => "Reserved/unspecified",
        }
    }
}

/// Parsed H.264 NAL header (one byte).
#[derive(Debug, Clone, Copy)]
pub struct NalHeader {
    pub nal_ref_idc: u8,
    pub nal_type: NalType,
}

pub fn parse_nal_header<S: FieldSink>(r: &mut BitReader<'_, S>) -> Result<NalHeader, Error> {
    r.u(1, "forbidden_zero_bit")?;
    let nal_ref_idc = r.u(2, "nal_ref_idc")? as u8;
    let nal_type = NalType::from_raw(r.u(5, "nal_unit_type")? as u8);
    Ok(NalHeader {
        nal_ref_idc,
        nal_type,
    })
}

/// Scaling list parse (7.3.2.1.1.1), shared by SPS and PPS.
pub(crate) fn parse_scaling_list<S: FieldSink>(
    r: &mut BitReader<'_, S>,
    name: &str,
    size: usize,
) -> Result<(), Error> {
    r.group(name, |r| {
        let mut last_scale = 8i32;
        let mut next_scale = 8i32;
        for j in 0..size {
            if next_scale != 0 {
                let delta = r.se("delta_scale")?;
                next_scale = (last_scale + delta + 256) % 256;
            }
            let cur = if next_scale == 0 {
                last_scale
            } else {
                next_scale
            };
            last_scale = cur;
            let _ = j;
        }
        Ok(())
    })
}
