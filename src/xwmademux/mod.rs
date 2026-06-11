// The `xwmademux` element: parses an xWMA RIFF container and outputs an
// `audio/x-wma` elementary stream for a downstream WMA decoder.
use gst::glib;
use gst::prelude::*;

mod imp;

glib::wrapper! {
    pub struct XwmaDemux(ObjectSubclass<imp::XwmaDemux>) @extends gst::Element, gst::Object;
}

pub fn register(plugin: &gst::Plugin) -> Result<(), glib::BoolError> {
    gst::Element::register(
        Some(plugin),
        "xwmademux",
        gst::Rank::PRIMARY,
        XwmaDemux::static_type(),
    )
}
