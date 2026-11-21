//! Custom GStreamer elements exposing per-block decoder internals.
//!
//! `slh264dec` (libavcodec) and `slh265dec` (libde265) decode video while
//! attaching [`streamlens_types::FrameAnalysis`] to each output buffer via
//! [`AnalysisMeta`].

use std::sync::Arc;

use gstreamer as gst;
use streamlens_types::FrameAnalysis;

pub mod ffi;
pub mod meta;
pub mod slh264dec;
pub mod slh265dec;

pub use meta::AnalysisMeta;

/// Registers the streamlens elements with GStreamer. Idempotent; safe to
/// call from concurrent sessions.
pub fn register_static() -> Result<(), gst::glib::BoolError> {
    static ONCE: std::sync::OnceLock<Result<(), gst::glib::BoolError>> = std::sync::OnceLock::new();
    ONCE.get_or_init(plugin_register_static).clone()
}

gst::plugin_define!(
    streamlens,
    "streamlens analysis decoder elements",
    plugin_init,
    "0.1.0",
    "MPL-2.0",
    "streamlens",
    "streamlens",
    "https://github.com/axeltob/streamlens"
);

fn plugin_init(plugin: &gst::Plugin) -> Result<(), gst::glib::BoolError> {
    slh265dec::register(plugin)?;
    slh264dec::register(plugin)?;
    Ok(())
}

/// Extracts the analysis attached to a decoded buffer by the streamlens
/// decoder elements, if any.
pub fn analysis_from_buffer(buffer: &gst::Buffer) -> Option<Arc<FrameAnalysis>> {
    buffer.meta::<AnalysisMeta>().map(|m| m.analysis().clone())
}
