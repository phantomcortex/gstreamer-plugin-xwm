// xWMA RIFF demuxer.
//
// Streaming parser for the xWMA container. The container is a RIFF/"XWMA"
// file with three relevant chunks:
//   "fmt "  18-byte WAVEFORMATEX (channels, rate, block_align, ...)
//   "dpds"  per-packet cumulative decoded-byte seek index (kept for phase-2 seeking)
//   "data"  a stream of WMA packets, each `nBlockAlign` bytes
//
// We parse the header, emit `audio/x-wma` caps (synthesising the fixed 6-byte
// WMAv2 codec_data that the file omits), and push the packetised data stream
// downstream for `avdec_wmav2` to decode.
use gst::glib;
use gst::prelude::*;
use gst::subclass::prelude::*;

use std::sync::{Mutex, OnceLock};

// WAVEFORMATEX format tags we understand.
const WAVE_FORMAT_WMAUDIO2: u16 = 0x0161;

// The WMAv2 decoder extradata is not stored in xWMA files; it is a fixed,
// experimentally-known constant (see FFmpeg libavformat/xwma.c).
const WMAV2_CODEC_DATA: [u8; 6] = [0x00, 0x00, 0x00, 0x00, 0x1f, 0x00];

fn cat() -> gst::DebugCategory {
    static CAT: OnceLock<gst::DebugCategory> = OnceLock::new();
    *CAT.get_or_init(|| {
        gst::DebugCategory::new(
            "xwmademux",
            gst::DebugColorFlags::empty(),
            Some("xWMA demuxer"),
        )
    })
}

/// Parsed WAVEFORMATEX fields we need.
#[derive(Clone, Copy)]
struct Fmt {
    channels: u16,
    rate: u32,
    avg_bytes_per_sec: u32,
    block_align: u16,
    bits_per_sample: u16,
}

/// Where the streaming parser currently is.
#[derive(Clone, Copy)]
enum Stage {
    /// Expecting the 12-byte "RIFF <size> XWMA" header.
    RiffHeader,
    /// Expecting an 8-byte chunk header (4-byte id + u32 size).
    ChunkHeader,
    /// Buffering a full small chunk body (fmt /dpds/unknown) of `size` (+`pad`).
    ChunkBody { id: [u8; 4], size: u64, pad: u64 },
    /// Streaming the "data" chunk; `remaining` data bytes left (+ trailing `pad`).
    Data { remaining: u64, pad: u64 },
}

struct State {
    /// Unconsumed input bytes.
    buf: Vec<u8>,
    /// Bytes still to discard (chunk padding spanning chain() calls).
    skip: u64,
    stage: Stage,
    fmt: Option<Fmt>,
    /// Total decoded PCM bytes, from the last entry of the `dpds` index. Used to
    /// report stream duration (xWMA stores no duration in WAVEFORMATEX).
    dpds_total: Option<u64>,
    /// stream-start/caps/segment already pushed.
    started: bool,
    /// PTS=0 has been stamped on the first outgoing buffer.
    sent_first_buf: bool,
    /// A duration-changed message has been posted for the known duration.
    announced_duration: bool,
}

impl Default for State {
    fn default() -> Self {
        State {
            buf: Vec::new(),
            skip: 0,
            stage: Stage::RiffHeader,
            fmt: None,
            dpds_total: None,
            started: false,
            sent_first_buf: false,
            announced_duration: false,
        }
    }
}

/// Things the parser wants pushed downstream (collected so we can release the
/// state lock before pushing).
enum Output {
    Event(gst::Event),
    Buffer(gst::Buffer),
}

pub struct XwmaDemux {
    sinkpad: gst::Pad,
    srcpad: gst::Pad,
    state: Mutex<State>,
}

#[glib::object_subclass]
impl ObjectSubclass for XwmaDemux {
    const NAME: &'static str = "GstXwmaDemux";
    type Type = super::XwmaDemux;
    type ParentType = gst::Element;

    fn with_class(klass: &Self::Class) -> Self {
        let templ = klass.pad_template("sink").unwrap();
        let sinkpad = gst::Pad::builder_from_template(&templ)
            .chain_function(|pad, parent, buffer| {
                XwmaDemux::catch_panic_pad_function(
                    parent,
                    || Err(gst::FlowError::Error),
                    |this| this.sink_chain(pad, buffer),
                )
            })
            .event_function(|pad, parent, event| {
                XwmaDemux::catch_panic_pad_function(
                    parent,
                    || false,
                    |this| this.sink_event(pad, event),
                )
            })
            .build();

        let templ = klass.pad_template("src").unwrap();
        let srcpad = gst::Pad::builder_from_template(&templ)
            .query_function(|pad, parent, query| {
                XwmaDemux::catch_panic_pad_function(
                    parent,
                    || false,
                    |this| this.src_query(pad, query),
                )
            })
            .build();

        Self {
            sinkpad,
            srcpad,
            state: Mutex::new(State::default()),
        }
    }
}

impl ObjectImpl for XwmaDemux {
    fn constructed(&self) {
        self.parent_constructed();
        let obj = self.obj();
        obj.add_pad(&self.sinkpad).unwrap();
        obj.add_pad(&self.srcpad).unwrap();
    }
}

impl GstObjectImpl for XwmaDemux {}

impl ElementImpl for XwmaDemux {
    fn metadata() -> Option<&'static gst::subclass::ElementMetadata> {
        static METADATA: OnceLock<gst::subclass::ElementMetadata> = OnceLock::new();
        Some(METADATA.get_or_init(|| {
            gst::subclass::ElementMetadata::new(
                "xWMA Demuxer",
                "Codec/Demuxer/Audio",
                "Parses Microsoft xWMA (.xwm) files into an audio/x-wma stream",
                "killawatt <killawattgamer@gmail.com>",
            )
        }))
    }

    fn pad_templates() -> &'static [gst::PadTemplate] {
        static TEMPLATES: OnceLock<Vec<gst::PadTemplate>> = OnceLock::new();
        TEMPLATES.get_or_init(|| {
            let sink_caps = gst::Caps::builder("audio/x-xwma").build();
            let sink = gst::PadTemplate::new(
                "sink",
                gst::PadDirection::Sink,
                gst::PadPresence::Always,
                &sink_caps,
            )
            .unwrap();

            let src_caps = gst::Caps::builder("audio/x-wma").build();
            let src = gst::PadTemplate::new(
                "src",
                gst::PadDirection::Src,
                gst::PadPresence::Always,
                &src_caps,
            )
            .unwrap();

            vec![sink, src]
        })
    }

    fn change_state(
        &self,
        transition: gst::StateChange,
    ) -> Result<gst::StateChangeSuccess, gst::StateChangeError> {
        // Reset the parser whenever we (re)enter the running states.
        if transition == gst::StateChange::ReadyToPaused {
            *self.state.lock().unwrap() = State::default();
        }
        self.parent_change_state(transition)
    }
}

impl XwmaDemux {
    fn sink_chain(
        &self,
        _pad: &gst::Pad,
        buffer: gst::Buffer,
    ) -> Result<gst::FlowSuccess, gst::FlowError> {
        let mut outputs = Vec::new();
        {
            let map = buffer.map_readable().map_err(|_| {
                gst::error!(cat(), imp = self, "Failed to map input buffer readable");
                gst::FlowError::Error
            })?;
            let mut state = self.state.lock().unwrap();
            state.buf.extend_from_slice(map.as_slice());
            self.parse(&mut state, &mut outputs)?;
        }

        for out in outputs {
            match out {
                Output::Event(e) => {
                    self.srcpad.push_event(e);
                }
                Output::Buffer(b) => {
                    self.srcpad.push(b)?;
                }
            }
        }

        // Once the duration is known (dpds parsed), tell the application so it can
        // (re)query and draw a complete progress bar.
        let announce = {
            let mut state = self.state.lock().unwrap();
            if state.dpds_total.is_some() && !state.announced_duration {
                state.announced_duration = true;
                true
            } else {
                false
            }
        };
        if announce {
            let _ = self
                .obj()
                .post_message(gst::message::DurationChanged::builder().build());
        }

        Ok(gst::FlowSuccess::Ok)
    }

    /// Total stream duration derived from the dpds index and WAVEFORMATEX.
    fn compute_duration(&self) -> Option<gst::ClockTime> {
        let state = self.state.lock().unwrap();
        let fmt = state.fmt?;
        let total = state.dpds_total?;
        let bytes_per_frame = (fmt.channels as u64) * ((fmt.bits_per_sample as u64).max(8) / 8);
        if bytes_per_frame == 0 || fmt.rate == 0 {
            return None;
        }
        let frames = total / bytes_per_frame;
        let nanos = (frames as u128 * 1_000_000_000u128 / fmt.rate as u128) as u64;
        Some(gst::ClockTime::from_nseconds(nanos))
    }

    fn src_query(&self, pad: &gst::Pad, query: &mut gst::QueryRef) -> bool {
        use gst::QueryViewMut;
        match query.view_mut() {
            QueryViewMut::Duration(q) => {
                if q.format() == gst::Format::Time {
                    if let Some(dur) = self.compute_duration() {
                        q.set(dur);
                        return true;
                    }
                }
                false
            }
            _ => gst::Pad::query_default(pad, Some(&*self.obj()), query),
        }
    }

    fn sink_event(&self, _pad: &gst::Pad, event: gst::Event) -> bool {
        use gst::EventView;
        match event.view() {
            // We generate our own stream-start/caps/segment from the container,
            // so drop the upstream (byte-stream) ones.
            EventView::Caps(_) | EventView::Segment(_) | EventView::StreamStart(_) => true,
            EventView::Eos(_) => {
                let mut outputs = Vec::new();
                {
                    let mut state = self.state.lock().unwrap();
                    self.drain_eos(&mut state, &mut outputs);
                }
                for out in outputs {
                    match out {
                        Output::Buffer(b) => {
                            let _ = self.srcpad.push(b);
                        }
                        Output::Event(e) => {
                            self.srcpad.push_event(e);
                        }
                    }
                }
                self.srcpad.push_event(event)
            }
            EventView::FlushStop(_) => {
                *self.state.lock().unwrap() = State::default();
                self.srcpad.push_event(event)
            }
            _ => self.srcpad.push_event(event),
        }
    }

    /// Make as much parsing progress as the buffered bytes allow.
    fn parse(&self, state: &mut State, outputs: &mut Vec<Output>) -> Result<(), gst::FlowError> {
        loop {
            // Consume pending padding/skip first.
            if state.skip > 0 {
                let n = std::cmp::min(state.skip as usize, state.buf.len());
                state.buf.drain(0..n);
                state.skip -= n as u64;
                if state.skip > 0 {
                    break;
                }
            }

            match state.stage {
                Stage::RiffHeader => {
                    if state.buf.len() < 12 {
                        break;
                    }
                    if &state.buf[0..4] != b"RIFF" || &state.buf[8..12] != b"XWMA" {
                        return self.error(state, "Not a valid xWMA (RIFF/XWMA) stream");
                    }
                    state.buf.drain(0..12);
                    state.stage = Stage::ChunkHeader;
                }

                Stage::ChunkHeader => {
                    if state.buf.len() < 8 {
                        break;
                    }
                    let mut id = [0u8; 4];
                    id.copy_from_slice(&state.buf[0..4]);
                    let size = u32::from_le_bytes(state.buf[4..8].try_into().unwrap()) as u64;
                    state.buf.drain(0..8);
                    let pad = size & 1;

                    if &id == b"data" {
                        let fmt = match state.fmt {
                            Some(f) => f,
                            None => {
                                return self
                                    .error(state, "Found data chunk before a valid fmt chunk")
                            }
                        };
                        if !state.started {
                            self.start_stream(state, fmt, outputs);
                        }
                        state.stage = Stage::Data {
                            remaining: size,
                            pad,
                        };
                    } else {
                        state.stage = Stage::ChunkBody { id, size, pad };
                    }
                }

                Stage::ChunkBody { id, size, pad } => {
                    let need = (size + pad) as usize;
                    if state.buf.len() < need {
                        break;
                    }
                    if &id == b"fmt " {
                        self.parse_fmt(state, size as usize)?;
                    } else if &id == b"dpds" && size >= 4 {
                        // The dpds index is a list of cumulative decoded-byte counts;
                        // the last entry is the total decoded PCM size, which gives the
                        // stream duration. (Per-entry seeking is phase 2.)
                        let last = (size - 4) as usize;
                        state.dpds_total = Some(u32::from_le_bytes(
                            state.buf[last..last + 4].try_into().unwrap(),
                        ) as u64);
                    }
                    state.buf.drain(0..need);
                    state.stage = Stage::ChunkHeader;
                }

                Stage::Data { remaining, pad } => {
                    if remaining == 0 {
                        // Empty/exhausted data chunk: skip any pad, look for more chunks.
                        state.skip = pad;
                        state.stage = Stage::ChunkHeader;
                        continue;
                    }

                    let block_align = state.fmt.map(|f| f.block_align).unwrap_or(1).max(1) as usize;
                    let avail = std::cmp::min(state.buf.len() as u64, remaining) as usize;
                    if avail == 0 {
                        break;
                    }

                    let is_tail = avail as u64 == remaining;
                    // Emit exactly one WMA packet (block_align bytes) per buffer, which
                    // is what avdec_wmav2 expects. A short final packet is only emitted
                    // once we know no more data bytes are coming for this chunk.
                    let take = if avail >= block_align {
                        block_align
                    } else if is_tail {
                        avail
                    } else {
                        break; // wait for a full packet to arrive
                    };

                    let chunk: Vec<u8> = state.buf.drain(0..take).collect();
                    let new_remaining = remaining - take as u64;

                    let mut buffer = gst::Buffer::from_mut_slice(chunk);
                    if !state.sent_first_buf {
                        buffer.get_mut().unwrap().set_pts(gst::ClockTime::ZERO);
                        state.sent_first_buf = true;
                    }
                    outputs.push(Output::Buffer(buffer));

                    if new_remaining == 0 {
                        state.skip = pad;
                        state.stage = Stage::ChunkHeader;
                    } else {
                        // Keep looping to emit further packets already buffered.
                        state.stage = Stage::Data {
                            remaining: new_remaining,
                            pad,
                        };
                    }
                }
            }
        }

        Ok(())
    }

    /// Parse the WAVEFORMATEX in the first `size` bytes of the buffer.
    fn parse_fmt(&self, state: &mut State, size: usize) -> Result<(), gst::FlowError> {
        if size < 16 {
            return self.error(state, "fmt chunk too small to be WAVEFORMATEX");
        }
        let b = &state.buf;
        let format_tag = u16::from_le_bytes([b[0], b[1]]);
        let channels = u16::from_le_bytes([b[2], b[3]]);
        let rate = u32::from_le_bytes([b[4], b[5], b[6], b[7]]);
        let avg_bytes_per_sec = u32::from_le_bytes([b[8], b[9], b[10], b[11]]);
        let block_align = u16::from_le_bytes([b[12], b[13]]);
        let bits_per_sample = u16::from_le_bytes([b[14], b[15]]);

        if format_tag != WAVE_FORMAT_WMAUDIO2 {
            // Only WMAv2 has the known fixed codec_data; warn but try anyway.
            gst::warning!(
                cat(),
                imp = self,
                "Unexpected WAVEFORMATEX format tag {:#06x}; assuming WMAv2 codec_data",
                format_tag
            );
        }

        state.fmt = Some(Fmt {
            channels,
            rate,
            avg_bytes_per_sec,
            block_align,
            bits_per_sample,
        });

        gst::debug!(
            cat(),
            imp = self,
            "xWMA fmt: {} ch, {} Hz, block_align {}, {} bytes/s",
            channels,
            rate,
            block_align,
            avg_bytes_per_sec
        );
        Ok(())
    }

    /// Emit stream-start, caps and segment for the WMA elementary stream.
    fn start_stream(&self, state: &mut State, fmt: Fmt, outputs: &mut Vec<Output>) {
        let stream_id = self
            .srcpad
            .create_stream_id(&*self.obj(), Option::<&str>::None);
        outputs.push(Output::Event(
            gst::event::StreamStart::builder(&stream_id).build(),
        ));

        let codec_data = gst::Buffer::from_slice(WMAV2_CODEC_DATA);
        let caps = gst::Caps::builder("audio/x-wma")
            .field("wmaversion", 2i32)
            .field("rate", fmt.rate as i32)
            .field("channels", fmt.channels as i32)
            .field("block_align", fmt.block_align as i32)
            .field("bitrate", (fmt.avg_bytes_per_sec as i64 * 8) as i32)
            .field("depth", fmt.bits_per_sample as i32)
            .field("codec_data", codec_data)
            .build();
        outputs.push(Output::Event(gst::event::Caps::new(&caps)));

        let segment = gst::FormattedSegment::<gst::ClockTime>::new();
        outputs.push(Output::Event(gst::event::Segment::new(&segment)));

        state.started = true;
    }

    /// On EOS, flush any partial trailing packet we were still holding.
    fn drain_eos(&self, state: &mut State, outputs: &mut Vec<Output>) {
        if let Stage::Data { remaining, .. } = state.stage {
            let avail = std::cmp::min(state.buf.len() as u64, remaining) as usize;
            if avail > 0 {
                let chunk: Vec<u8> = state.buf.drain(0..avail).collect();
                let mut buffer = gst::Buffer::from_mut_slice(chunk);
                if !state.sent_first_buf {
                    buffer.get_mut().unwrap().set_pts(gst::ClockTime::ZERO);
                    state.sent_first_buf = true;
                }
                outputs.push(Output::Buffer(buffer));
                state.stage = Stage::Data {
                    remaining: remaining - avail as u64,
                    pad: 0,
                };
            }
        }
    }

    fn error(&self, _state: &mut State, msg: &str) -> Result<(), gst::FlowError> {
        gst::element_imp_error!(self, gst::StreamError::Demux, ["{}", msg]);
        Err(gst::FlowError::Error)
    }
}
