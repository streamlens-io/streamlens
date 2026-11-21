//! All FFI to the vendored, patched FFmpeg and libde265: container demuxing
//! and the analysis decoders that expose per-block internals as
//! [`streamlens_types::FrameAnalysis`].

mod annexb;
pub mod decode;
pub mod demux;
pub mod ffi;
mod h264;
mod h265;

pub use decode::{open_decoder, Decoder, Picture};
pub use demux::{AccessUnit, Demuxer};

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("{call} failed: {msg}")]
    Ffmpeg { call: &'static str, msg: String },
    #[error("decoder: {0}")]
    Decoder(String),
    #[error("unsupported format: {0}")]
    UnsupportedFormat(String),
    #[error("malformed stream: {0}")]
    Malformed(String),
    #[error("no H.264/H.265 video stream found")]
    NoVideoStream,
    #[error("path is not representable for FFmpeg")]
    InvalidPath,
}
