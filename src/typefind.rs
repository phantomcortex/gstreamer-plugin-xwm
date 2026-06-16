// Typefinders for xWMA (.xwm) and Bethesda FUZ (.fuz) containers.
//
// xWMA: RIFF form type "XWMA" — offsets 0..4 == "RIFF", 8..12 == "XWMA".
// FUZ:  Bethesda container with magic "FUZE" at offset 0; embeds an xWMA
//       stream after a variable-length lip-animation header.
use gst::glib;

const XWMA_CAPS: &str = "audio/x-xwma";
const FUZ_CAPS: &str = "audio/x-fuz";

pub fn register(plugin: &gst::Plugin) -> Result<(), glib::BoolError> {
    gst::TypeFind::register(
        Some(plugin),
        "xwma_typefind",
        gst::Rank::PRIMARY,
        Some("xwm"),
        Some(&gst::Caps::builder(XWMA_CAPS).build()),
        type_find_xwma,
    )?;
    gst::TypeFind::register(
        Some(plugin),
        "fuz_typefind",
        gst::Rank::PRIMARY,
        Some("fuz"),
        Some(&gst::Caps::builder(FUZ_CAPS).build()),
        type_find_fuz,
    )
}

fn type_find_xwma(tf: &mut gst::TypeFind) {
    if let Some(data) = tf.peek(0, 12) {
        if &data[0..4] == b"RIFF" && &data[8..12] == b"XWMA" {
            tf.suggest(
                gst::TypeFindProbability::Maximum,
                &gst::Caps::builder(XWMA_CAPS).build(),
            );
        }
    }
}

fn type_find_fuz(tf: &mut gst::TypeFind) {
    if let Some(data) = tf.peek(0, 4) {
        if &data[0..4] == b"FUZE" {
            tf.suggest(
                gst::TypeFindProbability::Maximum,
                &gst::Caps::builder(FUZ_CAPS).build(),
            );
        }
    }
}
