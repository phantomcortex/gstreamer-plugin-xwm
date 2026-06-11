// Typefinder for the Microsoft xWMA container.
//
// xWMA is a RIFF container whose form type is "XWMA":
//   offset 0: 'R' 'I' 'F' 'F'
//   offset 4: <u32 riff size>
//   offset 8: 'X' 'W' 'M' 'A'
// Registering this at PRIMARY rank lets decodebin/playbin recognise .xwm files
// (which otherwise have no typefinder) and autoplug our demuxer.
use gst::glib;

const XWMA_CAPS: &str = "audio/x-xwma";

pub fn register(plugin: &gst::Plugin) -> Result<(), glib::BoolError> {
    gst::TypeFind::register(
        Some(plugin),
        "xwma_typefind",
        gst::Rank::PRIMARY,
        Some("xwm"),
        Some(&gst::Caps::builder(XWMA_CAPS).build()),
        type_find,
    )
}

fn type_find(tf: &mut gst::TypeFind) {
    if let Some(data) = tf.peek(0, 12) {
        if &data[0..4] == b"RIFF" && &data[8..12] == b"XWMA" {
            tf.suggest(
                gst::TypeFindProbability::Maximum,
                &gst::Caps::builder(XWMA_CAPS).build(),
            );
        }
    }
}
