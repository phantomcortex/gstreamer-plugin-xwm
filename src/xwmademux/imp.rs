// xWMA RIFF demuxer.
//
// Streaming parser for the xWMA container. The container is a RIFF/"XWMA"
// file with three relevant chunks:
//   "fmt "  18-byte WAVEFORMATEX (channels, rate, block_align, ...)
//   "dpds"  per-packet cumulative decoded-byte index (used for duration & seeking)
//   "data"  a stream of WMA packets, each `nBlockAlign` bytes
//
// We parse the header, emit `audio/x-wma` caps (synthesising the fixed 6-byte
// WMAv2 codec_data that the file omits) and push the packetised data stream
// downstream for `avdec_wmav2` to decode.
//
// Timing comes entirely from the `dpds` index: entry `i` is the total number of
// decoded PCM bytes after packet `i`. That gives us each packet's timestamp, the
// total duration, and an exact TIME->byte mapping for seeking. Seeking is done in
// push mode: a TIME seek is translated to a BYTE seek on the upstream (filesrc)
// at the target packet boundary.
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

/// Convert a count of decoded PCM bytes to a stream time.
fn bytes_to_time(bytes: u64, rate: u32, bytes_per_frame: u64) -> gst::ClockTime {
    if rate == 0 || bytes_per_frame == 0 {
        return gst::ClockTime::ZERO;
    }
    let nanos = (bytes as u128 * 1_000_000_000u128 / (rate as u128 * bytes_per_frame as u128)) as u64;
    gst::ClockTime::from_nseconds(nanos)
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

impl Fmt {
    fn bytes_per_frame(&self) -> u64 {
        (self.channels as u64) * ((self.bits_per_sample as u64).max(8) / 8)
    }
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

/// Post-seek parameters, computed when a TIME seek arrives and applied once the
/// upstream flush completes.
struct SeekTarget {
    remaining: u64,
    packet_index: u64,
    byte_offset: u64,
    seg_time: gst::ClockTime,
}

struct State {
    /// Unconsumed input bytes.
    buf: Vec<u8>,
    /// Bytes still to discard (chunk padding spanning chain() calls).
    skip: u64,
    /// Absolute byte offset in the upstream stream of the next byte in `buf`.
    consumed: u64,
    stage: Stage,
    fmt: Option<Fmt>,
    /// Cumulative decoded-byte counts, one per packet (the dpds index).
    dpds: Vec<u64>,
    /// Absolute byte offset of the data chunk payload, and its total size/padding.
    data_start: Option<u64>,
    data_total: u64,
    data_pad: u64,
    /// Index of the next packet to emit (drives per-packet timestamps).
    packet_index: u64,
    /// stream-start/caps already pushed.
    started: bool,
    /// A duration-changed message has been posted for the known duration.
    announced_duration: bool,
    /// A new (post-seek) segment to emit before the next buffer.
    pending_segment_time: Option<gst::ClockTime>,
    /// Seek to apply on the next upstream flush-stop.
    pending_seek: Option<SeekTarget>,
}

impl Default for State {
    fn default() -> Self {
        State {
            buf: Vec::new(),
            skip: 0,
            consumed: 0,
            stage: Stage::RiffHeader,
            fmt: None,
            dpds: Vec::new(),
            data_start: None,
            data_total: 0,
            data_pad: 0,
            packet_index: 0,
            started: false,
            announced_duration: false,
            pending_segment_time: None,
            pending_seek: None,
        }
    }
}

impl State {
    /// Drain `n` bytes from the front of the buffer, tracking absolute position.
    fn drain(&mut self, n: usize) {
        self.buf.drain(0..n);
        self.consumed += n as u64;
    }

    /// Total stream duration, if the dpds index and format are known.
    fn duration(&self) -> Option<gst::ClockTime> {
        let fmt = self.fmt?;
        let total = *self.dpds.last()?;
        Some(bytes_to_time(total, fmt.rate, fmt.bytes_per_frame()))
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
            .event_function(|pad, parent, event| {
                XwmaDemux::catch_panic_pad_function(
                    parent,
                    || false,
                    |this| this.src_event(pad, event),
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
                "killawatt <phantom.github@proton.me>",
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
            if state.duration().is_some() && !state.announced_duration {
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
                // Apply any pending seek now that upstream has flushed and is about
                // to resume from the new byte offset. Keep the parsed header
                // (format/dpds/data layout); only the read position changes.
                {
                    let mut state = self.state.lock().unwrap();
                    if let Some(target) = state.pending_seek.take() {
                        gst::debug!(
                            cat(),
                            imp = self,
                            "flush-stop: applying seek to packet {} byte {} seg_time {}",
                            target.packet_index,
                            target.byte_offset,
                            target.seg_time
                        );
                        state.buf.clear();
                        state.skip = 0;
                        state.consumed = target.byte_offset;
                        state.stage = Stage::Data {
                            remaining: target.remaining,
                            pad: state.data_pad,
                        };
                        state.packet_index = target.packet_index;
                        state.pending_segment_time = Some(target.seg_time);
                    }
                }
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
                state.drain(n);
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
                        return self.error("Not a valid xWMA (RIFF/XWMA) stream");
                    }
                    state.drain(12);
                    state.stage = Stage::ChunkHeader;
                }

                Stage::ChunkHeader => {
                    if state.buf.len() < 8 {
                        break;
                    }
                    let mut id = [0u8; 4];
                    id.copy_from_slice(&state.buf[0..4]);
                    let size = u32::from_le_bytes(state.buf[4..8].try_into().unwrap()) as u64;
                    state.drain(8);
                    let pad = size & 1;

                    if &id == b"data" {
                        let fmt = match state.fmt {
                            Some(f) => f,
                            None => {
                                return self.error("Found data chunk before a valid fmt chunk")
                            }
                        };
                        state.data_start = Some(state.consumed);
                        state.data_total = size;
                        state.data_pad = pad;
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
                    } else if &id == b"dpds" {
                        // The dpds index is a list of cumulative decoded-byte counts,
                        // one per packet: timing, duration and seeking all derive from it.
                        let n = (size as usize) / 4;
                        state.dpds = (0..n)
                            .map(|i| {
                                let o = i * 4;
                                u32::from_le_bytes(state.buf[o..o + 4].try_into().unwrap()) as u64
                            })
                            .collect();
                    }
                    state.drain(need);
                    state.stage = Stage::ChunkHeader;
                }

                Stage::Data { remaining, pad } => {
                    // Emit a fresh segment after a seek, before any buffer.
                    if let Some(t) = state.pending_segment_time.take() {
                        let mut segment = gst::FormattedSegment::<gst::ClockTime>::new();
                        segment.set_start(t);
                        segment.set_time(t);
                        segment.set_position(t);
                        if let Some(dur) = state.duration() {
                            segment.set_stop(dur);
                        }
                        outputs.push(Output::Event(gst::event::Segment::new(&segment)));
                    }

                    if remaining == 0 {
                        // Exhausted data chunk: skip any pad, look for more chunks.
                        state.skip = pad;
                        state.stage = Stage::ChunkHeader;
                        continue;
                    }

                    let fmt = state.fmt.unwrap();
                    let block_align = (fmt.block_align.max(1)) as usize;
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

                    let chunk: Vec<u8> = state.buf[0..take].to_vec();
                    state.drain(take);
                    let new_remaining = remaining - take as u64;

                    let mut buffer = gst::Buffer::from_mut_slice(chunk);
                    self.stamp_buffer(state, buffer.get_mut().unwrap());
                    state.packet_index += 1;
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

    /// Timestamp a packet buffer from the dpds index (PTS + duration).
    fn stamp_buffer(&self, state: &State, buffer: &mut gst::BufferRef) {
        let fmt = state.fmt.unwrap();
        let idx = state.packet_index as usize;
        if !state.dpds.is_empty() {
            let bpf = fmt.bytes_per_frame();
            let last = state.dpds.len() - 1;
            let before = if idx == 0 {
                0
            } else {
                state.dpds[(idx - 1).min(last)]
            };
            let after = state.dpds[idx.min(last)];
            buffer.set_pts(bytes_to_time(before, fmt.rate, bpf));
            buffer.set_duration(bytes_to_time(after.saturating_sub(before), fmt.rate, bpf));
        } else if idx == 0 {
            buffer.set_pts(gst::ClockTime::ZERO);
        }
    }

    /// Parse the WAVEFORMATEX in the first `size` bytes of the buffer.
    fn parse_fmt(&self, state: &mut State, size: usize) -> Result<(), gst::FlowError> {
        if size < 16 {
            return self.error("fmt chunk too small to be WAVEFORMATEX");
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

    /// Emit stream-start, caps and the initial segment for the WMA stream.
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

        let mut segment = gst::FormattedSegment::<gst::ClockTime>::new();
        if let Some(dur) = state.duration() {
            segment.set_stop(dur);
        }
        outputs.push(Output::Event(gst::event::Segment::new(&segment)));

        state.started = true;
    }

    /// On EOS, flush any partial trailing packet we were still holding.
    fn drain_eos(&self, state: &mut State, outputs: &mut Vec<Output>) {
        if let Stage::Data { remaining, .. } = state.stage {
            let avail = std::cmp::min(state.buf.len() as u64, remaining) as usize;
            if avail > 0 {
                let chunk: Vec<u8> = state.buf[0..avail].to_vec();
                state.drain(avail);
                let mut buffer = gst::Buffer::from_mut_slice(chunk);
                self.stamp_buffer(state, buffer.get_mut().unwrap());
                state.packet_index += 1;
                outputs.push(Output::Buffer(buffer));
                state.stage = Stage::Data {
                    remaining: remaining - avail as u64,
                    pad: 0,
                };
            }
        }
    }

    fn src_query(&self, pad: &gst::Pad, query: &mut gst::QueryRef) -> bool {
        use gst::QueryViewMut;
        match query.view_mut() {
            QueryViewMut::Duration(q) => {
                if q.format() == gst::Format::Time {
                    if let Some(dur) = self.state.lock().unwrap().duration() {
                        q.set(dur);
                        return true;
                    }
                }
                false
            }
            QueryViewMut::Seeking(q) => {
                if q.format() == gst::Format::Time {
                    match self.state.lock().unwrap().duration() {
                        Some(dur) => q.set(true, gst::ClockTime::ZERO, dur),
                        None => q.set(false, gst::ClockTime::ZERO, gst::ClockTime::NONE),
                    }
                    true
                } else {
                    gst::Pad::query_default(pad, Some(&*self.obj()), query)
                }
            }
            _ => gst::Pad::query_default(pad, Some(&*self.obj()), query),
        }
    }

    fn src_event(&self, pad: &gst::Pad, event: gst::Event) -> bool {
        match event.view() {
            gst::EventView::Seek(_) => self.handle_seek(event),
            _ => gst::Pad::event_default(pad, Some(&*self.obj()), event),
        }
    }

    /// Handle a TIME seek by translating it to an upstream BYTE seek at the target
    /// packet boundary, using the dpds index for the time->byte mapping.
    fn handle_seek(&self, event: gst::Event) -> bool {
        let (rate, flags, start_type, start, _stop_type, _stop) = match event.view() {
            gst::EventView::Seek(s) => s.get(),
            _ => return false,
        };

        let start = match start {
            gst::GenericFormattedValue::Time(Some(t)) => t,
            // Only TIME seeks are handled here; let the default path try the rest.
            _ => return gst::Pad::event_default(&self.srcpad, Some(&*self.obj()), event),
        };
        if start_type != gst::SeekType::Set {
            return gst::Pad::event_default(&self.srcpad, Some(&*self.obj()), event);
        }

        // Compute the target packet and byte offset from the dpds index, and stash
        // the post-seek parameters to apply once upstream finishes flushing.
        let (packet_index, byte_offset) = {
            let mut state = self.state.lock().unwrap();
            let fmt = match state.fmt {
                Some(f) => f,
                None => return false,
            };
            let data_start = match state.data_start {
                Some(d) => d,
                None => return false,
            };
            if state.dpds.is_empty() {
                return false;
            }
            let bpf = fmt.bytes_per_frame();
            let block_align = fmt.block_align.max(1) as u64;

            // Target decoded-byte position for the requested time.
            let target_decoded =
                (start.nseconds() as u128 * fmt.rate as u128 * bpf as u128 / 1_000_000_000u128) as u64;
            // First packet whose cumulative decoded bytes exceed the target, i.e. the
            // packet that contains the requested sample.
            let k = state
                .dpds
                .partition_point(|&cum| cum <= target_decoded)
                .min(state.dpds.len() - 1);
            let decoded_before = if k == 0 { 0 } else { state.dpds[k - 1] };
            let seg_time = bytes_to_time(decoded_before, fmt.rate, bpf);
            let byte_offset = data_start + (k as u64) * block_align;
            let remaining = state.data_total.saturating_sub((k as u64) * block_align);

            state.pending_seek = Some(SeekTarget {
                remaining,
                packet_index: k as u64,
                byte_offset,
                seg_time,
            });
            (k as u64, byte_offset)
        };

        let flush = flags.contains(gst::SeekFlags::FLUSH);
        if flush {
            // Unblock the streaming thread so it can process the upstream flush.
            self.srcpad.push_event(gst::event::FlushStart::new());
        }

        // Translate to a byte seek on the upstream element (e.g. filesrc).
        let byte_seek = gst::event::Seek::new(
            rate,
            gst::SeekFlags::FLUSH | gst::SeekFlags::ACCURATE,
            gst::SeekType::Set,
            gst::format::Bytes::from_u64(byte_offset),
            gst::SeekType::None,
            gst::format::Bytes::NONE,
        );

        gst::debug!(
            cat(),
            imp = self,
            "Seek to {} -> packet {} byte {}",
            start,
            packet_index,
            byte_offset
        );

        let upstream_ok = self.sinkpad.push_event(byte_seek);
        gst::debug!(
            cat(),
            imp = self,
            "upstream byte seek accepted: {}",
            upstream_ok
        );
        if !upstream_ok {
            // Upstream refused; drop the pending seek so we don't apply it later.
            self.state.lock().unwrap().pending_seek = None;
            return false;
        }
        true
    }

    fn error(&self, msg: &str) -> Result<(), gst::FlowError> {
        gst::element_imp_error!(self, gst::StreamError::Demux, ["{}", msg]);
        Err(gst::FlowError::Error)
    }
}
