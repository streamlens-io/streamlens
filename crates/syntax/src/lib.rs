//! Pure-Rust bitstream syntax layer: NAL parsing, recording bit-reader,
//! parameter set / slice header parsers, POC + DPB models.
//!
//! No GStreamer or decoder dependencies; operates on raw byte slices.

pub mod analyzer;
pub mod bitread;
pub mod diag;
pub mod h264;
pub mod h265;
pub mod nal;

pub use analyzer::{AuAnalyzer, AuSummary, NalSummary};

pub use bitread::{BitReader, FieldNode, FieldSink, FieldTree, FieldValue, NoRecord, Recorder};

/// Syntax-level parse error.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("bitstream ended unexpectedly at bit {bit_pos}")]
    UnexpectedEnd { bit_pos: usize },
    #[error("invalid exp-golomb code at bit {bit_pos}")]
    InvalidExpGolomb { bit_pos: usize },
    #[error("{what} references unknown {referenced} id {id}")]
    MissingParameterSet {
        what: &'static str,
        referenced: &'static str,
        id: u32,
    },
    #[error("invalid {field}: {value} (allowed: {allowed})")]
    OutOfRange {
        field: &'static str,
        value: i64,
        allowed: &'static str,
    },
    #[error("unsupported: {0}")]
    Unsupported(&'static str),
}
