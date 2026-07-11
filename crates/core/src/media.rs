//! GStreamer pipeline construction shared by the indexer and decode service.

use gstreamer as gst;
use gstreamer::prelude::*;
use streamlens_types::Codec;

use crate::Error;

/// Caps accepted by analysis appsinks: parsed elementary video, one access
/// unit per buffer, Annex B byte-stream. The parser elements convert
/// avc/hvc1/length-prefixed input to match.
pub fn au_caps() -> gst::Caps {
    let mut builder = gst::Caps::builder_full();
    for name in ["video/x-h264", "video/x-h265"] {
        builder = builder.structure(
            gst::Structure::builder(name)
                .field("stream-format", "byte-stream")
                .field("alignment", "au")
                .build(),
        );
    }
    builder.build()
}

pub fn codec_from_caps(caps: &gst::CapsRef) -> Result<Codec, Error> {
    let s = caps
        .structure(0)
        .ok_or_else(|| Error::Pipeline("empty caps".into()))?;
    match s.name().as_str() {
        "video/x-h264" => Ok(Codec::H264),
        "video/x-h265" => Ok(Codec::H265),
        other => Err(Error::UnsupportedCodec(other.to_string())),
    }
}

/// Builds `filesrc ! parsebin ! (h264parse|h265parse) ! appsink(au_caps)`.
///
/// parsebin autoplugs the demuxer (or typefind for raw ES); its exposed pad
/// carries whatever format the internal parser settled on (often
/// length-prefixed avc/hvc1), so a second parser is inserted per pad to
/// renegotiate to Annex B byte-stream with AU alignment. The first
/// H.264/H.265 pad wins; other pads are ignored.
pub fn build_au_pipeline(
    path: &std::path::Path,
) -> Result<(gst::Pipeline, gstreamer_app::AppSink), Error> {
    let pipeline = gst::Pipeline::new();
    let filesrc = gst::ElementFactory::make("filesrc")
        .property("location", path.to_str().unwrap_or_default())
        .build()?;
    let parsebin = gst::ElementFactory::make("parsebin").build()?;
    let appsink = gstreamer_app::AppSink::builder()
        .caps(&au_caps())
        .sync(false)
        .max_buffers(64u32)
        .build();

    pipeline.add_many([&filesrc, &parsebin, appsink.upcast_ref()])?;
    filesrc.link(&parsebin)?;

    let pipeline_weak = pipeline.downgrade();
    let appsink_weak = appsink.downgrade();
    parsebin.connect_pad_added(move |_, pad| {
        let (Some(pipeline), Some(appsink)) = (pipeline_weak.upgrade(), appsink_weak.upgrade())
        else {
            return;
        };
        let sinkpad = appsink.static_pad("sink").expect("appsink has sink pad");
        if sinkpad.is_linked() {
            return;
        }
        // parsebin pads carry negotiated caps by the time pad-added fires;
        // current_caps is the truth (query_caps can answer with templates).
        let caps = pad.current_caps().unwrap_or_else(|| pad.query_caps(None));
        let parser_name = match caps.structure(0).map(|s| s.name().as_str()) {
            Some("video/x-h264") => "h264parse",
            Some("video/x-h265") => "h265parse",
            _ => {
                log::debug!("ignoring pad {} ({caps:?})", pad.name());
                return;
            }
        };
        let result: Result<(), Error> = (|| {
            // config-interval=-1: re-insert SPS/PPS before every IDR so any
            // keyframe-aligned slice of the ES cache decodes standalone.
            let parser = gst::ElementFactory::make(parser_name)
                .property("config-interval", -1i32)
                .build()?;
            pipeline.add(&parser)?;
            pad.link(&parser.static_pad("sink").expect("parser sink"))
                .map_err(|e| Error::Pipeline(format!("link {parser_name}: {e:?}")))?;
            parser
                .static_pad("src")
                .expect("parser src")
                .link(&sinkpad)
                .map_err(|e| Error::Pipeline(format!("link AU sink: {e:?}")))?;
            parser.sync_state_with_parent()?;
            Ok(())
        })();
        if let Err(e) = result {
            log::error!("failed to attach {parser_name}: {e}");
            gst::element_error!(
                pipeline,
                gst::StreamError::Failed,
                ["failed to attach {}: {}", parser_name, e]
            );
        }
    });

    Ok((pipeline, appsink))
}

/// Non-blocking check for a posted pipeline error.
pub fn check_bus_error(pipeline: &gst::Pipeline) -> Result<(), Error> {
    let bus = pipeline.bus().expect("pipeline has bus");
    while let Some(msg) = bus.pop_filtered(&[gst::MessageType::Error]) {
        if let gst::MessageView::Error(e) = msg.view() {
            return Err(Error::Pipeline(format!(
                "{} ({})",
                e.error(),
                e.debug().unwrap_or_default()
            )));
        }
    }
    Ok(())
}
