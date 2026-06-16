// Bethesda FUZ container demuxer.
//
// FUZ format (used by Skyrim/Fallout 4/Starfield for voiced dialogue):
//   [0..4]   "FUZE"  magic
//   [4]      version byte (typically 0x01; others logged and accepted)
//   [5..9]   u32 little-endian: FNAM (lip-animation) data size; may be 0
//   [9..]    FNAM data (lip_size bytes), then a complete RIFF/XWMA audio file
//
// This element strips that header and emits audio/x-xwma so that xwmademux
// can parse the embedded xWMA stream via normal decodebin autoplugging.
use gst::glib;
use gst::prelude::*;
use gst::subclass::prelude::*;
use std::sync::{Mutex, OnceLock};

fn cat() -> gst::DebugCategory {
    static CAT: OnceLock<gst::DebugCategory> = OnceLock::new();
    *CAT.get_or_init(|| {
        gst::DebugCategory::new("fuzdemux", gst::DebugColorFlags::empty(), Some("FUZ demuxer"))
    })
}

#[derive(Clone, Copy)]
enum Stage {
    /// Waiting for the 9-byte fixed header (magic + version + lip_size).
    Header,
    /// Skipping `remaining` bytes of lip-animation data.
    Fnam(u32),
    /// Passing through raw RIFF/XWMA audio data.
    Audio,
}

struct State {
    buf: Vec<u8>,
    stage: Stage,
    started: bool,
}

impl Default for State {
    fn default() -> Self {
        State { buf: Vec::new(), stage: Stage::Header, started: false }
    }
}

enum Output {
    Event(gst::Event),
    Buffer(gst::Buffer),
}

pub struct FuzDemux {
    sinkpad: gst::Pad,
    srcpad: gst::Pad,
    state: Mutex<State>,
}

#[glib::object_subclass]
impl ObjectSubclass for FuzDemux {
    const NAME: &'static str = "GstFuzDemux";
    type Type = super::FuzDemux;
    type ParentType = gst::Element;

    fn with_class(klass: &Self::Class) -> Self {
        let templ = klass.pad_template("sink").unwrap();
        let sinkpad = gst::Pad::builder_from_template(&templ)
            .chain_function(|pad, parent, buffer| {
                FuzDemux::catch_panic_pad_function(
                    parent,
                    || Err(gst::FlowError::Error),
                    |this| this.sink_chain(pad, buffer),
                )
            })
            .event_function(|pad, parent, event| {
                FuzDemux::catch_panic_pad_function(
                    parent,
                    || false,
                    |this| this.sink_event(pad, event),
                )
            })
            .build();

        let templ = klass.pad_template("src").unwrap();
        let srcpad = gst::Pad::builder_from_template(&templ).build();

        Self { sinkpad, srcpad, state: Mutex::new(State::default()) }
    }
}

impl ObjectImpl for FuzDemux {
    fn constructed(&self) {
        self.parent_constructed();
        let obj = self.obj();
        obj.add_pad(&self.sinkpad).unwrap();
        obj.add_pad(&self.srcpad).unwrap();
    }
}

impl GstObjectImpl for FuzDemux {}

impl ElementImpl for FuzDemux {
    fn metadata() -> Option<&'static gst::subclass::ElementMetadata> {
        static METADATA: OnceLock<gst::subclass::ElementMetadata> = OnceLock::new();
        Some(METADATA.get_or_init(|| {
            gst::subclass::ElementMetadata::new(
                "FUZ Demuxer",
                "Codec/Demuxer/Audio",
                "Strips the Bethesda FUZ header and exposes the embedded xWMA audio stream",
                "phantomcortex <phantom.github@proton.me>",
            )
        }))
    }

    fn pad_templates() -> &'static [gst::PadTemplate] {
        static TEMPLATES: OnceLock<Vec<gst::PadTemplate>> = OnceLock::new();
        TEMPLATES.get_or_init(|| {
            let sink = gst::PadTemplate::new(
                "sink",
                gst::PadDirection::Sink,
                gst::PadPresence::Always,
                &gst::Caps::builder("audio/x-fuz").build(),
            )
            .unwrap();
            let src = gst::PadTemplate::new(
                "src",
                gst::PadDirection::Src,
                gst::PadPresence::Always,
                &gst::Caps::builder("audio/x-xwma").build(),
            )
            .unwrap();
            vec![sink, src]
        })
    }

    fn change_state(
        &self,
        transition: gst::StateChange,
    ) -> Result<gst::StateChangeSuccess, gst::StateChangeError> {
        if transition == gst::StateChange::ReadyToPaused {
            *self.state.lock().unwrap() = State::default();
        }
        self.parent_change_state(transition)
    }
}

impl FuzDemux {
    fn sink_chain(
        &self,
        _pad: &gst::Pad,
        buffer: gst::Buffer,
    ) -> Result<gst::FlowSuccess, gst::FlowError> {
        let mut outputs = Vec::new();
        {
            let map = buffer.map_readable().map_err(|_| {
                gst::error!(cat(), imp = self, "Failed to map input buffer");
                gst::FlowError::Error
            })?;
            let mut state = self.state.lock().unwrap();
            state.buf.extend_from_slice(map.as_slice());
            self.parse(&mut state, &mut outputs)?;
        }
        for out in outputs {
            match out {
                Output::Event(e) => { self.srcpad.push_event(e); }
                Output::Buffer(b) => { self.srcpad.push(b)?; }
            }
        }
        Ok(gst::FlowSuccess::Ok)
    }

    fn sink_event(&self, _pad: &gst::Pad, event: gst::Event) -> bool {
        use gst::EventView;
        match event.view() {
            // Drop upstream byte-stream framing; we emit our own from start_audio().
            EventView::Caps(_) | EventView::Segment(_) | EventView::StreamStart(_) => true,
            _ => self.srcpad.push_event(event),
        }
    }

    fn parse(&self, state: &mut State, outputs: &mut Vec<Output>) -> Result<(), gst::FlowError> {
        loop {
            match state.stage {
                Stage::Header => {
                    if state.buf.len() < 9 {
                        break;
                    }
                    if &state.buf[0..4] != b"FUZE" {
                        gst::element_imp_error!(
                            self,
                            gst::StreamError::Demux,
                            ["Not a FUZ stream (missing FUZE magic)"]
                        );
                        return Err(gst::FlowError::Error);
                    }
                    let version = state.buf[4];
                    if version != 1 {
                        gst::warning!(cat(), imp = self, "Unexpected FUZ version {version}; trying anyway");
                    }
                    let lip_size =
                        u32::from_le_bytes(state.buf[5..9].try_into().unwrap());
                    gst::debug!(cat(), imp = self, "FUZ header: version={version}, lip_size={lip_size}");
                    state.buf.drain(0..9);
                    state.stage = Stage::Fnam(lip_size);
                }

                Stage::Fnam(remaining) => {
                    let skip = (remaining as usize).min(state.buf.len());
                    state.buf.drain(0..skip);
                    let left = remaining - skip as u32;
                    if left > 0 {
                        state.stage = Stage::Fnam(left);
                        break;
                    }
                    // Lip data consumed; emit xwma stream-start/caps/segment and switch to passthrough.
                    if !state.started {
                        let stream_id =
                            self.srcpad.create_stream_id(&*self.obj(), Option::<&str>::None);
                        outputs.push(Output::Event(
                            gst::event::StreamStart::builder(&stream_id).build(),
                        ));
                        outputs.push(Output::Event(gst::event::Caps::new(
                            &gst::Caps::builder("audio/x-xwma").build(),
                        )));
                        outputs.push(Output::Event(gst::event::Segment::new(
                            &gst::FormattedSegment::<gst::ClockTime>::new(),
                        )));
                        state.started = true;
                    }
                    state.stage = Stage::Audio;
                }

                Stage::Audio => {
                    if state.buf.is_empty() {
                        break;
                    }
                    let data: Vec<u8> = state.buf.drain(..).collect();
                    outputs.push(Output::Buffer(gst::Buffer::from_mut_slice(data)));
                }
            }
        }
        Ok(())
    }
}
