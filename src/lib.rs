// gst-plugin-xwm — GStreamer support for Microsoft xWMA (.xwm) audio.
//
// This plugin fills the *autoplugging* gap for .xwm files: it provides a
// typefinder for the xWMA RIFF magic and a demuxer that parses the container
// and emits `audio/x-wma` caps. The actual WMA decoding is delegated to the
// existing `avdec_wmav2` element from gst-libav via decodebin autoplugging.
use gst::glib;

mod fuzdemux;
mod typefind;
mod xwmademux;

fn plugin_init(plugin: &gst::Plugin) -> Result<(), glib::BoolError> {
    typefind::register(plugin)?;
    xwmademux::register(plugin)?;
    fuzdemux::register(plugin)?;
    Ok(())
}

gst::plugin_define!(
    xwm,
    env!("CARGO_PKG_DESCRIPTION"),
    plugin_init,
    concat!(env!("CARGO_PKG_VERSION"), "-", env!("COMMIT_ID")),
    "MPL",
    env!("CARGO_PKG_NAME"),
    env!("CARGO_PKG_NAME"),
    env!("CARGO_PKG_REPOSITORY"),
    env!("BUILD_REL_DATE")
);
