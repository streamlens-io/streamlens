//! Length-prefixed samples (MP4, MKV, FLV) to Annex B, and the parameter
//! sets those containers keep out of band in their decoder configuration
//! record (ISO/IEC 14496-15: avcC 5.3.3.1, hvcC 8.3.3.1).

use streamlens_types::Codec;

use crate::Error;

const START_CODE: [u8; 4] = [0, 0, 0, 1];

/// Decoder configuration of a length-prefixed track.
pub(crate) struct LengthPrefixed {
    /// Bytes of the NAL length field in front of every NAL of a sample.
    pub length_size: usize,
    /// Every NAL of the record's arrays (parameter sets, and SEI where the
    /// encoder put it there), as Annex B.
    pub config_nals: Vec<u8>,
}

/// Parses the track's extradata as an avcC/hvcC record. `None` when it is
/// not one: the samples are Annex B already.
pub(crate) fn parse_config(
    codec: Codec,
    extradata: &[u8],
) -> Result<Option<LengthPrefixed>, Error> {
    // configurationVersion is 1 in both records; Annex B extradata starts
    // with a start code (0).
    if extradata.first() != Some(&1) {
        return Ok(None);
    }
    let mut r = Reader {
        data: extradata,
        pos: 0,
    };
    let mut config_nals = Vec::new();
    let length_size = match codec {
        Codec::H264 => {
            // version, profile, compatibility, level
            r.skip(4)?;
            let length_size = (r.u8()? & 0x03) as usize + 1;
            let num_sps = r.u8()? & 0x1F;
            for _ in 0..num_sps {
                r.nal_into(&mut config_nals)?;
            }
            let num_pps = r.u8()?;
            for _ in 0..num_pps {
                r.nal_into(&mut config_nals)?;
            }
            // Trailing high-profile fields (chroma format, bit depths, SPS
            // extensions) repeat what the SPS says.
            length_size
        }
        Codec::H265 => {
            // version through avgFrameRate (22 bytes), then
            // constantFrameRate(2) numTemporalLayers(3)
            // temporalIdNested(1) lengthSizeMinusOne(2).
            r.skip(21)?;
            let length_size = (r.u8()? & 0x03) as usize + 1;
            let num_arrays = r.u8()?;
            for _ in 0..num_arrays {
                // array_completeness(1) reserved(1) NAL_unit_type(6)
                r.skip(1)?;
                let num_nalus = r.u16()?;
                for _ in 0..num_nalus {
                    r.nal_into(&mut config_nals)?;
                }
            }
            length_size
        }
    };
    Ok(Some(LengthPrefixed {
        length_size,
        config_nals,
    }))
}

/// Rewrites one length-prefixed sample as Annex B into `out` (cleared
/// first): each NAL length field becomes a 4-byte start code.
pub(crate) fn convert_sample(
    sample: &[u8],
    length_size: usize,
    out: &mut Vec<u8>,
) -> Result<(), Error> {
    out.clear();
    out.reserve(sample.len() + 4);
    let mut pos = 0;
    while pos < sample.len() {
        let len_bytes = sample
            .get(pos..pos + length_size)
            .ok_or_else(|| malformed("truncated NAL length field in sample"))?;
        let len = len_bytes
            .iter()
            .fold(0usize, |acc, &b| (acc << 8) | b as usize);
        pos += length_size;
        let nal = sample
            .get(pos..pos + len)
            .ok_or_else(|| malformed("NAL length exceeds sample"))?;
        out.extend_from_slice(&START_CODE);
        out.extend_from_slice(nal);
        pos += len;
    }
    Ok(())
}

struct Reader<'a> {
    data: &'a [u8],
    pos: usize,
}

impl Reader<'_> {
    fn bytes(&mut self, n: usize) -> Result<&[u8], Error> {
        let b = self
            .data
            .get(self.pos..self.pos + n)
            .ok_or_else(|| malformed("truncated decoder configuration record"))?;
        self.pos += n;
        Ok(b)
    }

    fn skip(&mut self, n: usize) -> Result<(), Error> {
        self.bytes(n).map(|_| ())
    }

    fn u8(&mut self) -> Result<u8, Error> {
        Ok(self.bytes(1)?[0])
    }

    fn u16(&mut self) -> Result<u16, Error> {
        let b = self.bytes(2)?;
        Ok(u16::from_be_bytes([b[0], b[1]]))
    }

    /// One 16-bit-length-prefixed NAL of a record array, appended as Annex B.
    fn nal_into(&mut self, out: &mut Vec<u8>) -> Result<(), Error> {
        let len = self.u16()? as usize;
        let nal = self.bytes(len)?;
        out.extend_from_slice(&START_CODE);
        out.extend_from_slice(nal);
        Ok(())
    }
}

fn malformed(msg: &str) -> Error {
    Error::Malformed(msg.into())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn converts_length_prefixed_sample() {
        let sample = [0, 0, 0, 2, 0x65, 0xAA, 0, 0, 0, 1, 0x06];
        let mut out = Vec::new();
        convert_sample(&sample, 4, &mut out).unwrap();
        assert_eq!(out, [0, 0, 0, 1, 0x65, 0xAA, 0, 0, 0, 1, 0x06]);
    }

    #[test]
    fn rejects_overlong_nal() {
        let mut out = Vec::new();
        assert!(convert_sample(&[0, 0, 0, 9, 0x65], 4, &mut out).is_err());
        assert!(convert_sample(&[0, 0], 4, &mut out).is_err());
    }

    #[test]
    fn parses_avcc() {
        // version 1, profile/compat/level, lengthSizeMinusOne = 3,
        // 1 SPS (2 bytes), 1 PPS (1 byte).
        let avcc = [
            1, 0x64, 0, 0x0D, 0xFF, 0xE1, 0, 2, 0x67, 0x64, 1, 0, 1, 0x68,
        ];
        let cfg = parse_config(Codec::H264, &avcc).unwrap().unwrap();
        assert_eq!(cfg.length_size, 4);
        assert_eq!(cfg.config_nals, [0, 0, 0, 1, 0x67, 0x64, 0, 0, 0, 1, 0x68]);
    }

    #[test]
    fn annexb_extradata_is_not_a_record() {
        assert!(parse_config(Codec::H264, &[0, 0, 0, 1, 0x67])
            .unwrap()
            .is_none());
        assert!(parse_config(Codec::H265, &[]).unwrap().is_none());
    }

    #[test]
    fn truncated_record_is_an_error() {
        assert!(parse_config(Codec::H265, &[1, 2, 3]).is_err());
    }
}
