//! Session orchestration: stream indexing, on-demand decoding, caching,
//! thumbnails and metrics. This is the only crate the UI talks to.

pub mod cache;
pub mod chunked;
pub mod decode;
pub mod index;
pub mod indexer;
pub mod metrics;
pub mod playback;
pub mod session;
mod source;
pub mod sweep;

pub use cache::CacheWindow;
pub use decode::{DecodeService, DecodedFrame};
pub use index::{IndexProgress, StreamIndex};
pub use indexer::Indexer;
pub use playback::Playback;
pub use session::{estimated_frame_bytes, Session};
pub use streamlens_media::Source;
/// Syntax trees for the Headers panel; re-exported so the UI depends on
/// core only.
pub use streamlens_syntax::{analyzer::RecordedNal, FieldNode};
pub use sweep::{Sweep, SweepFrame};

/// Errors surfaced to the UI.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("media: {0}")]
    Media(#[from] streamlens_media::Error),
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
    #[error("no H.264/H.265 video stream found in {0}")]
    NoVideoStream(String),
    #[error("pipeline error: {0}")]
    Pipeline(String),
    #[error("unsupported codec: {0}")]
    UnsupportedCodec(String),
}
