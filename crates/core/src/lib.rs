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

/// Positioned read (pread): fills `buf` from `offset` without depending on
/// the file cursor.
#[cfg(unix)]
pub(crate) fn read_exact_at(
    file: &std::fs::File,
    buf: &mut [u8],
    offset: u64,
) -> std::io::Result<()> {
    std::os::unix::fs::FileExt::read_exact_at(file, buf, offset)
}

/// Windows `seek_read` can return short reads and moves the file cursor —
/// callers here never rely on the cursor.
#[cfg(windows)]
pub(crate) fn read_exact_at(
    file: &std::fs::File,
    mut buf: &mut [u8],
    mut offset: u64,
) -> std::io::Result<()> {
    use std::os::windows::fs::FileExt;
    while !buf.is_empty() {
        match file.seek_read(buf, offset)? {
            0 => {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::UnexpectedEof,
                    "failed to fill whole buffer",
                ))
            }
            n => {
                buf = &mut buf[n..];
                offset += n as u64;
            }
        }
    }
    Ok(())
}
