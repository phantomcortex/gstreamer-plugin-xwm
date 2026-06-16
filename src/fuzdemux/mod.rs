// The `fuzdemux` element: strips the Bethesda FUZ header and emits the
// embedded xWMA data as `audio/x-xwma` for xwmademux to parse downstream.
use gst::glib;
use gst::prelude::*;

mod imp;

glib::wrapper! {
    pub struct FuzDemux(ObjectSubclass<imp::FuzDemux>) @extends gst::Element, gst::Object;
}

pub fn register(plugin: &gst::Plugin) -> Result<(), glib::BoolError> {
    gst::Element::register(
        Some(plugin),
        "fuzdemux",
        gst::Rank::PRIMARY,
        FuzDemux::static_type(),
    )
}
