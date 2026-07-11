//! Bit reader over RBSP data with optional per-field recording.
//!
//! Parsers call the named read methods (`u`, `ue`, `se`, `flag`); the generic
//! `S: FieldSink` parameter either discards names ([`NoRecord`], zero cost —
//! used by the indexer, which parses every slice header of a stream) or
//! builds a [`FieldTree`] for the Headers panel ([`Recorder`]).

use crate::Error;

/// A single parsed syntax field value.
#[derive(Debug, Clone, PartialEq)]
pub enum FieldValue {
    Uint(u64),
    Int(i64),
    Flag(bool),
    /// Value derived from other fields (not read from the stream).
    Derived(String),
}

impl std::fmt::Display for FieldValue {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            FieldValue::Uint(v) => write!(f, "{v}"),
            FieldValue::Int(v) => write!(f, "{v}"),
            FieldValue::Flag(v) => write!(f, "{}", *v as u8),
            FieldValue::Derived(s) => write!(f, "{s}"),
        }
    }
}

/// One node of the recorded syntax tree.
#[derive(Debug, Clone)]
pub struct FieldNode {
    pub name: String,
    pub value: FieldValue,
    /// Bit offset within the RBSP of the containing NAL. Derived fields keep
    /// the offset of the point they were emitted at, with `bit_width == 0`.
    pub bit_offset: u32,
    pub bit_width: u8,
    pub children: Vec<FieldNode>,
}

/// Recorded field tree of one parsed syntax structure.
#[derive(Debug, Clone, Default)]
pub struct FieldTree {
    pub roots: Vec<FieldNode>,
}

/// Receives parsed fields. Implementations must be cheap when disabled.
pub trait FieldSink {
    fn field(&mut self, name: &str, value: FieldValue, bit_offset: u32, bit_width: u8);
    fn open_group(&mut self, name: &str, bit_offset: u32);
    fn close_group(&mut self);
}

/// Discards everything; the compiler removes all recording code.
#[derive(Default)]
pub struct NoRecord;

impl FieldSink for NoRecord {
    #[inline(always)]
    fn field(&mut self, _: &str, _: FieldValue, _: u32, _: u8) {}
    #[inline(always)]
    fn open_group(&mut self, _: &str, _: u32) {}
    #[inline(always)]
    fn close_group(&mut self) {}
}

/// Builds a [`FieldTree`].
#[derive(Default)]
pub struct Recorder {
    tree: FieldTree,
    /// Index path to the currently open group.
    stack: Vec<usize>,
}

impl Recorder {
    pub fn finish(self) -> FieldTree {
        self.tree
    }

    fn current(&mut self) -> &mut Vec<FieldNode> {
        let mut nodes = &mut self.tree.roots;
        for &i in &self.stack {
            nodes = &mut nodes[i].children;
        }
        nodes
    }
}

impl FieldSink for Recorder {
    fn field(&mut self, name: &str, value: FieldValue, bit_offset: u32, bit_width: u8) {
        self.current().push(FieldNode {
            name: name.to_owned(),
            value,
            bit_offset,
            bit_width,
            children: Vec::new(),
        });
    }

    fn open_group(&mut self, name: &str, bit_offset: u32) {
        let nodes = self.current();
        nodes.push(FieldNode {
            name: name.to_owned(),
            value: FieldValue::Derived(String::new()),
            bit_offset,
            bit_width: 0,
            children: Vec::new(),
        });
        let idx = nodes.len() - 1;
        self.stack.push(idx);
    }

    fn close_group(&mut self) {
        self.stack.pop();
    }
}

/// Big-endian bit reader over an RBSP byte slice.
pub struct BitReader<'a, S: FieldSink> {
    data: &'a [u8],
    pos: usize,
    pub sink: S,
}

impl<'a> BitReader<'a, NoRecord> {
    pub fn new(data: &'a [u8]) -> Self {
        Self::with_sink(data, NoRecord)
    }
}

impl<'a, S: FieldSink> BitReader<'a, S> {
    pub fn with_sink(data: &'a [u8], sink: S) -> Self {
        Self { data, pos: 0, sink }
    }

    pub fn bit_pos(&self) -> usize {
        self.pos
    }

    pub fn bits_left(&self) -> usize {
        self.data.len() * 8 - self.pos
    }

    fn read_bit(&mut self) -> Result<u64, Error> {
        let byte = self
            .data
            .get(self.pos / 8)
            .ok_or(Error::UnexpectedEnd { bit_pos: self.pos })?;
        let bit = (byte >> (7 - self.pos % 8)) & 1;
        self.pos += 1;
        Ok(bit as u64)
    }

    fn read_bits(&mut self, n: u8) -> Result<u64, Error> {
        debug_assert!(n <= 64);
        let mut v = 0u64;
        for _ in 0..n {
            v = (v << 1) | self.read_bit()?;
        }
        Ok(v)
    }

    /// Fixed-width unsigned: `u(n)`.
    pub fn u(&mut self, n: u8, name: &str) -> Result<u64, Error> {
        let start = self.pos as u32;
        let v = self.read_bits(n)?;
        self.sink.field(name, FieldValue::Uint(v), start, n);
        Ok(v)
    }

    /// Single-bit flag: `u(1)`.
    pub fn flag(&mut self, name: &str) -> Result<bool, Error> {
        let start = self.pos as u32;
        let v = self.read_bit()? != 0;
        self.sink.field(name, FieldValue::Flag(v), start, 1);
        Ok(v)
    }

    /// Unsigned Exp-Golomb: `ue(v)`.
    pub fn ue(&mut self, name: &str) -> Result<u32, Error> {
        let start = self.pos as u32;
        let v = self.read_ue_raw()?;
        self.sink.field(
            name,
            FieldValue::Uint(v as u64),
            start,
            (self.pos as u32 - start) as u8,
        );
        Ok(v)
    }

    /// Signed Exp-Golomb: `se(v)`.
    pub fn se(&mut self, name: &str) -> Result<i32, Error> {
        let start = self.pos as u32;
        let code = self.read_ue_raw()?;
        // (9-3): k -> ceil(k/2) with alternating sign, 0 -> 0.
        let v = if code % 2 == 1 {
            (code / 2 + 1) as i32
        } else {
            -((code / 2) as i32)
        };
        self.sink.field(
            name,
            FieldValue::Int(v as i64),
            start,
            (self.pos as u32 - start) as u8,
        );
        Ok(v)
    }

    fn read_ue_raw(&mut self) -> Result<u32, Error> {
        let mut leading_zeros = 0u8;
        while self.read_bit()? == 0 {
            leading_zeros += 1;
            if leading_zeros > 32 {
                return Err(Error::InvalidExpGolomb { bit_pos: self.pos });
            }
        }
        if leading_zeros == 0 {
            return Ok(0);
        }
        let rest = self.read_bits(leading_zeros)?;
        let v = (1u64 << leading_zeros) - 1 + rest;
        u32::try_from(v).map_err(|_| Error::InvalidExpGolomb { bit_pos: self.pos })
    }

    /// Remaining bytes from the current position; requires byte alignment.
    pub fn remaining_data(&self) -> &'a [u8] {
        debug_assert_eq!(self.pos % 8, 0);
        &self.data[self.pos / 8..]
    }

    /// Skip `n` bits without recording (reserved/unparsed payload regions).
    pub fn u_skip(&mut self, n: usize) -> Result<(), Error> {
        if n > self.bits_left() {
            return Err(Error::UnexpectedEnd { bit_pos: self.pos });
        }
        self.pos += n;
        Ok(())
    }

    /// Emit a value computed from other fields (not read from the stream).
    pub fn derived(&mut self, name: &str, value: impl std::fmt::Display) {
        self.sink.field(
            name,
            FieldValue::Derived(value.to_string()),
            self.pos as u32,
            0,
        );
    }

    /// Parse a named sub-structure.
    pub fn group<T>(
        &mut self,
        name: &str,
        f: impl FnOnce(&mut Self) -> Result<T, Error>,
    ) -> Result<T, Error> {
        self.sink.open_group(name, self.pos as u32);
        let r = f(self);
        self.sink.close_group();
        r
    }

    /// `more_rbsp_data()` per spec: true if there are bits before the final
    /// rbsp_stop_one_bit.
    pub fn more_rbsp_data(&self) -> bool {
        if self.bits_left() == 0 {
            return false;
        }
        // Find the last byte with any bit set; the stop bit is its lowest set bit.
        let mut last = self.data.len();
        while last > 0 && self.data[last - 1] == 0 {
            last -= 1;
        }
        if last == 0 {
            return false;
        }
        let stop_bit_pos = (last - 1) * 8 + (7 - self.data[last - 1].trailing_zeros() as usize);
        self.pos < stop_bit_pos
    }

    /// Skip the byte-alignment bits (rbsp_trailing_bits or byte_alignment).
    pub fn byte_align(&mut self) {
        self.pos = self.pos.div_ceil(8) * 8;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fixed_and_expgolomb() {
        // Bit string: 1 | 010 | 011 | 01 | 010 (+ zero padding)
        let data = [0b1010_0110, 0b1010_0000];
        let mut r = BitReader::new(&data);
        assert_eq!(r.ue("a").unwrap(), 0); // '1'
        assert_eq!(r.ue("b").unwrap(), 1); // '010'
        assert_eq!(r.ue("c").unwrap(), 2); // '011'
        assert_eq!(r.u(2, "d").unwrap(), 0b01);
        assert_eq!(r.se("e").unwrap(), 1); // '010' -> +1
    }

    #[test]
    fn se_mapping() {
        // codes 0,1,2,3,4 -> 0,1,-1,2,-2
        let vals = [(0u32, 0i32), (1, 1), (2, -1), (3, 2), (4, -2)];
        for (code, expect) in vals {
            // Encode ue(code) manually.
            let bits = code + 1;
            let width = 32 - bits.leading_zeros();
            let total = 2 * width - 1;
            let mut bytes = Vec::new();
            let mut acc = 0u64;
            let mut n = 0;
            for i in (0..total).rev() {
                acc = (acc << 1) | ((bits >> i) & 1) as u64;
                n += 1;
            }
            acc <<= 64 - n;
            for i in 0..8 {
                bytes.push(((acc >> (56 - i * 8)) & 0xff) as u8);
            }
            let mut r = BitReader::new(&bytes);
            assert_eq!(r.se("x").unwrap(), expect, "code {code}");
        }
    }

    #[test]
    fn recorder_builds_tree() {
        let data = [0b1101_0000];
        let mut r = BitReader::with_sink(&data, Recorder::default());
        r.group("sps", |r| {
            r.flag("f0")?;
            r.flag("f1")?;
            Ok(())
        })
        .unwrap();
        let tree = r.sink.finish();
        assert_eq!(tree.roots.len(), 1);
        assert_eq!(tree.roots[0].children.len(), 2);
        assert_eq!(tree.roots[0].children[0].value, FieldValue::Flag(true));
        assert_eq!(tree.roots[0].children[1].bit_offset, 1);
    }

    #[test]
    fn more_rbsp_data_stop_bit() {
        // one flag + stop bit + padding
        let data = [0b1100_0000];
        let mut r = BitReader::new(&data);
        assert!(r.more_rbsp_data());
        r.flag("f").unwrap();
        assert!(!r.more_rbsp_data()); // only the stop bit remains
    }
}
