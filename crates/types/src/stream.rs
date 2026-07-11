//! Stream-level info.

use serde::{Deserialize, Serialize};

use crate::frame::Codec;

/// Summary of an opened video stream, shown in the Stream panel.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StreamInfo {
    pub codec: Codec,
    pub profile: String,
    pub level: String,
    pub tier: Option<String>,
    pub width: u32,
    pub height: u32,
    pub bit_depth: u8,
    pub chroma_format: String,
    /// Frames per second as (num, den), when declared.
    pub frame_rate: Option<(u32, u32)>,
    pub container: String,
    pub duration_ns: Option<i64>,
}
