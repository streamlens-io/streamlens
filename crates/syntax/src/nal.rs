//! Annex B NAL unit framing and RBSP extraction.

/// A NAL unit located in an Annex B byte stream.
#[derive(Debug, Clone, Copy)]
pub struct NalUnit<'a> {
    /// Offset of the start code in the containing buffer.
    pub start_code_offset: usize,
    /// Offset of the first NAL header byte.
    pub payload_offset: usize,
    /// NAL bytes including the header, excluding the start code.
    pub bytes: &'a [u8],
}

/// Iterates NAL units in an Annex B buffer (3- or 4-byte start codes).
pub fn iter_nals(data: &[u8]) -> impl Iterator<Item = NalUnit<'_>> {
    NalIter { data, pos: 0 }
}

struct NalIter<'a> {
    data: &'a [u8],
    pos: usize,
}

impl<'a> Iterator for NalIter<'a> {
    type Item = NalUnit<'a>;

    fn next(&mut self) -> Option<NalUnit<'a>> {
        let start = find_start_code(self.data, self.pos)?;
        let payload = start + start_code_len(self.data, start);
        let end = find_start_code(self.data, payload)
            .map(|next| trim_trailing_zeros(self.data, payload, next))
            .unwrap_or(self.data.len());
        self.pos = end;
        Some(NalUnit {
            start_code_offset: start,
            payload_offset: payload,
            bytes: &self.data[payload..end],
        })
    }
}

fn find_start_code(data: &[u8], from: usize) -> Option<usize> {
    let mut i = from;
    while i + 3 <= data.len() {
        if data[i] == 0 && data[i + 1] == 0 {
            if data[i + 2] == 1 {
                return Some(i);
            }
            if i + 4 <= data.len() && data[i + 2] == 0 && data[i + 3] == 1 {
                return Some(i);
            }
            // A zero run: skip one byte, the run may still lead a start code.
            i += 1;
        } else {
            i += if data[i + 2] == 0 { 1 } else { 3 };
        }
    }
    None
}

fn start_code_len(data: &[u8], at: usize) -> usize {
    if data[at + 2] == 1 {
        3
    } else {
        4
    }
}

/// A 4-byte start code's leading zero belongs to the start code, and encoders
/// may pad with zeros before it; exclude trailing zeros from the previous NAL.
fn trim_trailing_zeros(data: &[u8], payload: usize, next_start: usize) -> usize {
    let mut end = next_start;
    while end > payload && data[end - 1] == 0 {
        end -= 1;
    }
    end
}

/// Removes emulation-prevention bytes (0x03 after 0x00 0x00) from a NAL
/// payload. Returns the RBSP and the payload-relative offsets of each removed
/// byte (needed to map RBSP bit offsets back to file offsets).
pub fn unescape_rbsp(payload: &[u8]) -> (Vec<u8>, Vec<u32>) {
    let mut rbsp = Vec::with_capacity(payload.len());
    let mut removed = Vec::new();
    let mut zeros = 0u32;
    for (i, &b) in payload.iter().enumerate() {
        if zeros >= 2 && b == 3 && payload.get(i + 1).is_none_or(|&n| n <= 3) {
            removed.push(i as u32);
            zeros = 0;
            continue;
        }
        zeros = if b == 0 { zeros + 1 } else { 0 };
        rbsp.push(b);
    }
    (rbsp, removed)
}

/// Maps an RBSP bit offset back to a bit offset within the escaped payload,
/// given the removed-byte positions from [`unescape_rbsp`].
pub fn rbsp_bit_to_payload_bit(rbsp_bit: u32, removed: &[u32]) -> u32 {
    let rbsp_byte = rbsp_bit / 8;
    // A removed byte at escaped position p sits at unescaped position
    // p - (number of removals before it); it shifts every RBSP byte at or
    // past that position by one.
    let shift = removed
        .iter()
        .enumerate()
        .take_while(|&(k, &p)| rbsp_byte >= p - k as u32)
        .count() as u32;
    rbsp_bit + shift * 8
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn splits_three_and_four_byte_start_codes() {
        let data = [
            0, 0, 0, 1, 0x67, 0xAA, // 4-byte
            0, 0, 1, 0x68, 0xBB, // 3-byte
            0, 0, 0, 1, 0x65, 0xCC, 0xDD,
        ];
        let nals: Vec<_> = iter_nals(&data).collect();
        assert_eq!(nals.len(), 3);
        assert_eq!(nals[0].bytes, &[0x67, 0xAA]);
        assert_eq!(nals[1].bytes, &[0x68, 0xBB]);
        assert_eq!(nals[2].bytes, &[0x65, 0xCC, 0xDD]);
        assert_eq!(nals[1].start_code_offset, 6);
        assert_eq!(nals[1].payload_offset, 9);
    }

    #[test]
    fn trims_zero_padding_before_next_start_code() {
        let data = [0, 0, 1, 0x67, 0xAA, 0, 0, 0, 0, 1, 0x68];
        let nals: Vec<_> = iter_nals(&data).collect();
        assert_eq!(nals.len(), 2);
        assert_eq!(nals[0].bytes, &[0x67, 0xAA]);
    }

    #[test]
    fn unescapes_emulation_prevention() {
        let payload = [0x67, 0x00, 0x00, 0x03, 0x01, 0x00, 0x00, 0x03, 0x03];
        let (rbsp, removed) = unescape_rbsp(&payload);
        assert_eq!(rbsp, [0x67, 0x00, 0x00, 0x01, 0x00, 0x00, 0x03]);
        assert_eq!(removed, [3, 7]);
    }

    #[test]
    fn keeps_0003_when_next_byte_large() {
        // 00 00 03 followed by > 0x03 is not an emulation sequence.
        let payload = [0x00, 0x00, 0x03, 0x40];
        let (rbsp, removed) = unescape_rbsp(&payload);
        assert_eq!(rbsp, payload);
        assert!(removed.is_empty());
    }

    #[test]
    fn maps_rbsp_offsets_back() {
        // removed byte at payload offset 3: rbsp bytes 0..=2 unshifted,
        // rbsp byte 3+ shifted by one byte.
        let removed = [3u32];
        assert_eq!(rbsp_bit_to_payload_bit(16, &removed), 16);
        assert_eq!(rbsp_bit_to_payload_bit(24, &removed), 32);
    }
}
