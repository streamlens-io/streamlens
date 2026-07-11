use gst::glib;
use gst::prelude::*;
use gstreamer as gst;
use gstreamer_video as gst_video;

mod imp;

glib::wrapper! {
    pub struct SlH265Dec(ObjectSubclass<imp::SlH265Dec>)
        @extends gst_video::VideoDecoder, gst::Element, gst::Object;
}

pub fn register(plugin: &gst::Plugin) -> Result<(), glib::BoolError> {
    gst::Element::register(
        Some(plugin),
        "slh265dec",
        gst::Rank::NONE,
        SlH265Dec::static_type(),
    )
}
