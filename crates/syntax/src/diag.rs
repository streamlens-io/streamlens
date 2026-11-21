//! Conformance diagnostics emitted while parsing (Message panel).

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Severity {
    Info,
    Warning,
    Error,
}

/// One diagnostic tied to a byte range in the file.
#[derive(Debug, Clone)]
pub struct Diagnostic {
    pub severity: Severity,
    pub message: String,
    /// Byte offset of the containing NAL in the elementary stream.
    pub byte_offset: u64,
    /// Decode-order frame index, when attributable to a frame.
    pub decode_index: Option<u32>,
}

/// Collects diagnostics during a parse pass.
#[derive(Debug, Clone, Default)]
pub struct Diagnostics {
    pub items: Vec<Diagnostic>,
}

impl Diagnostics {
    pub fn push(&mut self, severity: Severity, byte_offset: u64, message: impl Into<String>) {
        self.items.push(Diagnostic {
            severity,
            message: message.into(),
            byte_offset,
            decode_index: None,
        });
    }
}
