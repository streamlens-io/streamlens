//! H.265 (ITU-T Rec. H.265 / ISO 23008-2) syntax parsing.

pub mod analyzer;
pub mod poc;
pub mod pps;
pub mod ptl;
pub mod rps;
pub mod sei;
pub mod slice;
pub mod sps;
pub mod vps;

pub use analyzer::H265Analyzer;

pub use pps::Pps;
pub use sei::SeiMessage;
pub use slice::SliceHeader;
pub use sps::Sps;
pub use vps::Vps;

use crate::bitread::{BitReader, FieldSink};
use crate::Error;

/// H.265 nal_unit_type values (Table 7-1).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NalType {
    /// 0 — Coded slice of a trailing picture; sub-layer non-reference.
    TrailN,
    /// 1 — Coded slice of a trailing picture; reference.
    TrailR,
    /// 2 — Coded slice of a TSA picture; non-reference.
    TsaN,
    /// 3 — Coded slice of a TSA picture; reference.
    TsaR,
    /// 4 — Coded slice of an STSA picture; non-reference.
    StsaN,
    /// 5 — Coded slice of an STSA picture; reference.
    StsaR,
    /// 6 — Coded slice of a RADL picture; non-reference.
    RadlN,
    /// 7 — Coded slice of a RADL picture; reference.
    RadlR,
    /// 8 — Coded slice of a RASL picture; non-reference.
    RaslN,
    /// 9 — Coded slice of a RASL picture; reference.
    RaslR,
    /// 16 — BLA picture with leading RADL pictures.
    BlaWLp,
    /// 17 — BLA picture with a RADL picture.
    BlaWRadl,
    /// 18 — BLA picture without leading pictures.
    BlaNLp,
    /// 19 — IDR picture with RADL pictures.
    IdrWRadl,
    /// 20 — IDR picture without leading pictures.
    IdrNLp,
    /// 21 — Clean random access picture.
    Cra,
    /// 32 — Video parameter set.
    Vps,
    /// 33 — Sequence parameter set.
    Sps,
    /// 34 — Picture parameter set.
    Pps,
    /// 35 — Access unit delimiter.
    Aud,
    /// 36 — End of sequence.
    Eos,
    /// 37 — End of bitstream.
    Eob,
    /// 38 — Filler data.
    Fd,
    /// 39 — Prefix SEI.
    PrefixSei,
    /// 40 — Suffix SEI.
    SuffixSei,
    Other(u8),
}

impl NalType {
    pub fn from_raw(v: u8) -> Self {
        match v {
            0 => NalType::TrailN,
            1 => NalType::TrailR,
            2 => NalType::TsaN,
            3 => NalType::TsaR,
            4 => NalType::StsaN,
            5 => NalType::StsaR,
            6 => NalType::RadlN,
            7 => NalType::RadlR,
            8 => NalType::RaslN,
            9 => NalType::RaslR,
            16 => NalType::BlaWLp,
            17 => NalType::BlaWRadl,
            18 => NalType::BlaNLp,
            19 => NalType::IdrWRadl,
            20 => NalType::IdrNLp,
            21 => NalType::Cra,
            32 => NalType::Vps,
            33 => NalType::Sps,
            34 => NalType::Pps,
            35 => NalType::Aud,
            36 => NalType::Eos,
            37 => NalType::Eob,
            38 => NalType::Fd,
            39 => NalType::PrefixSei,
            40 => NalType::SuffixSei,
            other => NalType::Other(other),
        }
    }

    pub fn is_vcl(&self) -> bool {
        matches!(
            self,
            NalType::TrailN
                | NalType::TrailR
                | NalType::TsaN
                | NalType::TsaR
                | NalType::StsaN
                | NalType::StsaR
                | NalType::RadlN
                | NalType::RadlR
                | NalType::RaslN
                | NalType::RaslR
                | NalType::BlaWLp
                | NalType::BlaWRadl
                | NalType::BlaNLp
                | NalType::IdrWRadl
                | NalType::IdrNLp
                | NalType::Cra
        )
    }

    /// IRAP: nal_unit_type in 16..=23 (Table 7-1).
    pub fn is_irap(&self) -> bool {
        matches!(
            self,
            NalType::BlaWLp
                | NalType::BlaWRadl
                | NalType::BlaNLp
                | NalType::IdrWRadl
                | NalType::IdrNLp
                | NalType::Cra
        )
    }

    pub fn is_idr(&self) -> bool {
        matches!(self, NalType::IdrWRadl | NalType::IdrNLp)
    }

    pub fn is_rasl(&self) -> bool {
        matches!(self, NalType::RaslN | NalType::RaslR)
    }

    pub fn is_radl(&self) -> bool {
        matches!(self, NalType::RadlN | NalType::RadlR)
    }

    /// Sub-layer non-reference: even nal_unit_type values 0, 2, 4, 6, 8
    /// (the _N suffix types in 0..=14).
    pub fn is_sub_layer_non_ref(&self) -> bool {
        matches!(
            self,
            NalType::TrailN | NalType::TsaN | NalType::StsaN | NalType::RadlN | NalType::RaslN
        )
    }

    /// Human-readable name for the Stream Viewer.
    pub fn name(&self) -> &'static str {
        match self {
            NalType::TrailN => "TRAIL_N",
            NalType::TrailR => "TRAIL_R",
            NalType::TsaN => "TSA_N",
            NalType::TsaR => "TSA_R",
            NalType::StsaN => "STSA_N",
            NalType::StsaR => "STSA_R",
            NalType::RadlN => "RADL_N",
            NalType::RadlR => "RADL_R",
            NalType::RaslN => "RASL_N",
            NalType::RaslR => "RASL_R",
            NalType::BlaWLp => "BLA_W_LP",
            NalType::BlaWRadl => "BLA_W_RADL",
            NalType::BlaNLp => "BLA_N_LP",
            NalType::IdrWRadl => "IDR_W_RADL",
            NalType::IdrNLp => "IDR_N_LP",
            NalType::Cra => "CRA_NUT",
            NalType::Vps => "VPS",
            NalType::Sps => "SPS",
            NalType::Pps => "PPS",
            NalType::Aud => "AUD",
            NalType::Eos => "EOS_NUT",
            NalType::Eob => "EOB_NUT",
            NalType::Fd => "FD_NUT",
            NalType::PrefixSei => "PREFIX_SEI",
            NalType::SuffixSei => "SUFFIX_SEI",
            NalType::Other(_) => "Reserved/unspecified",
        }
    }
}

/// Parsed H.265 NAL unit header (two bytes, 7.3.1.2).
#[derive(Debug, Clone, Copy)]
pub struct NalHeader {
    pub nal_type: NalType,
    pub nuh_layer_id: u8,
    /// temporal_id = nuh_temporal_id_plus1 - 1; 0 = base layer.
    pub temporal_id: u8,
}

/// Parse the 2-byte HEVC NAL unit header (7.3.1.2).
pub fn parse_nal_header<S: FieldSink>(r: &mut BitReader<'_, S>) -> Result<NalHeader, Error> {
    r.u(1, "forbidden_zero_bit")?;
    let nal_unit_type = r.u(6, "nal_unit_type")? as u8;
    let nuh_layer_id = r.u(6, "nuh_layer_id")? as u8;
    let nuh_temporal_id_plus1 = r.u(3, "nuh_temporal_id_plus1")? as u8;
    if nuh_temporal_id_plus1 == 0 {
        return Err(Error::OutOfRange {
            field: "nuh_temporal_id_plus1",
            value: 0,
            allowed: "1..=7",
        });
    }
    Ok(NalHeader {
        nal_type: NalType::from_raw(nal_unit_type),
        nuh_layer_id,
        temporal_id: nuh_temporal_id_plus1 - 1,
    })
}
