//! Session orchestration: stream indexing, on-demand decoding, caching,
//! thumbnails and metrics. This is the only crate the UI talks to.

pub mod cache;
pub mod decode;
pub mod index;
pub mod indexer;
pub mod media;
pub mod metrics;
pub mod playback;
pub mod session;
pub mod sweep;

pub use decode::{DecodeService, DecodedFrame};
pub use index::{IndexProgress, StreamIndex};
pub use indexer::Indexer;
pub use playback::Playback;
pub use session::Session;
pub use sweep::{Sweep, SweepFrame};

/// Errors surfaced to the UI.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("gstreamer: {0}")]
    Gst(#[from] gstreamer::glib::Error),
    #[error("gstreamer: {0}")]
    GstBool(#[from] gstreamer::glib::BoolError),
    #[error("pipeline state change failed: {0}")]
    StateChange(#[from] gstreamer::StateChangeError),
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
    #[error("no H.264/H.265 video stream found in {0}")]
    NoVideoStream(String),
    #[error("pipeline error: {0}")]
    Pipeline(String),
    #[error("unsupported codec: {0}")]
    UnsupportedCodec(String),
}

/// Initializes GStreamer and registers the streamlens elements. Idempotent;
/// call before using anything else in this crate.
pub fn init() -> Result<(), Error> {
    gstreamer::init()?;
    streamlens_gst::register_static()?;
    Ok(())
}
