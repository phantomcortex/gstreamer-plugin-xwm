// Integration test: drive a synthetic xWMA stream through `xwmademux` and assert
// it produces the expected `audio/x-wma` caps (with reconstructed codec_data) and
// the right number of packet buffers. No real WMA decode is needed here.
use gst::prelude::*;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Arc, Mutex};

fn init() {
    use std::sync::Once;
    static START: Once = Once::new();
    START.call_once(|| {
        gst::init().unwrap();
        gstxwm::plugin_register_static().expect("Failed to register xwm plugin");
    });
}

/// Build a minimal valid xWMA file: RIFF/XWMA + fmt + dpds + data.
/// `dpds` is the cumulative decoded-byte index, one entry per packet.
fn synth_xwma(channels: u16, rate: u32, block_align: u16, dpds: &[u32], data: &[u8]) -> Vec<u8> {
    fn chunk(id: &[u8; 4], body: &[u8]) -> Vec<u8> {
        let mut v = Vec::new();
        v.extend_from_slice(id);
        v.extend_from_slice(&(body.len() as u32).to_le_bytes());
        v.extend_from_slice(body);
        if body.len() % 2 == 1 {
            v.push(0); // RIFF word padding
        }
        v
    }

    // WAVEFORMATEX (18 bytes): WMAv2.
    let mut fmt = Vec::new();
    fmt.extend_from_slice(&0x0161u16.to_le_bytes()); // wFormatTag = WMAv2
    fmt.extend_from_slice(&channels.to_le_bytes());
    fmt.extend_from_slice(&rate.to_le_bytes());
    fmt.extend_from_slice(&24000u32.to_le_bytes()); // nAvgBytesPerSec
    fmt.extend_from_slice(&block_align.to_le_bytes());
    fmt.extend_from_slice(&16u16.to_le_bytes()); // wBitsPerSample
    fmt.extend_from_slice(&0u16.to_le_bytes()); // cbSize

    // dpds: cumulative decoded-byte counts, one entry per packet.
    let mut dpds_bytes = Vec::new();
    for v in dpds {
        dpds_bytes.extend_from_slice(&v.to_le_bytes());
    }

    let mut body = Vec::new();
    body.extend_from_slice(b"XWMA");
    body.extend_from_slice(&chunk(b"fmt ", &fmt));
    body.extend_from_slice(&chunk(b"dpds", &dpds_bytes));
    body.extend_from_slice(&chunk(b"data", data));

    let mut riff = Vec::new();
    riff.extend_from_slice(b"RIFF");
    riff.extend_from_slice(&(body.len() as u32).to_le_bytes());
    riff.extend_from_slice(&body);
    riff
}

#[test]
fn demuxes_to_wma_caps_and_packets() {
    init();

    let block_align: u16 = 64;
    let data = vec![0xABu8; 256]; // 4 packets of 64 bytes
    // One dpds entry per packet (cumulative decoded bytes); 4096 decoded bytes/packet.
    let dpds = [4096u32, 8192, 12288, 16384];
    let xwma = synth_xwma(2, 44100, block_align, &dpds, &data);

    let pipeline = gst::Pipeline::new();
    let src = gst::ElementFactory::make("appsrc")
        .property("is-live", false)
        .build()
        .unwrap();
    let demux = gst::ElementFactory::make("xwmademux").build().unwrap();
    let sink = gst::ElementFactory::make("fakesink")
        .property("signal-handoffs", true)
        .build()
        .unwrap();

    pipeline.add_many([&src, &demux, &sink]).unwrap();
    gst::Element::link_many([&src, &demux, &sink]).unwrap();

    // Count buffers and total bytes that reach the sink.
    let buffer_count = Arc::new(AtomicU32::new(0));
    let total_bytes = Arc::new(AtomicU32::new(0));
    {
        let bc = buffer_count.clone();
        let tb = total_bytes.clone();
        sink.connect("handoff", false, move |args| {
            let buffer = args[1].get::<gst::Buffer>().unwrap();
            bc.fetch_add(1, Ordering::SeqCst);
            tb.fetch_add(buffer.size() as u32, Ordering::SeqCst);
            None
        });
    }

    // Capture the caps that xwmademux negotiates on its src pad.
    let caps = Arc::new(Mutex::new(None::<gst::Caps>));
    {
        let caps = caps.clone();
        let srcpad = demux.static_pad("src").unwrap();
        srcpad.add_probe(gst::PadProbeType::EVENT_DOWNSTREAM, move |_, info| {
            if let Some(gst::PadProbeData::Event(ref ev)) = info.data {
                if let gst::EventView::Caps(c) = ev.view() {
                    *caps.lock().unwrap() = Some(c.caps_owned());
                }
            }
            gst::PadProbeReturn::Ok
        });
    }

    let appsrc = src.dynamic_cast::<gst_app::AppSrc>().unwrap();
    pipeline.set_state(gst::State::Playing).unwrap();
    appsrc.push_buffer(gst::Buffer::from_slice(xwma)).unwrap();
    appsrc.end_of_stream().unwrap();

    // Wait for EOS or error.
    let bus = pipeline.bus().unwrap();
    for msg in bus.iter_timed(5 * gst::ClockTime::SECOND) {
        match msg.view() {
            gst::MessageView::Eos(_) => break,
            gst::MessageView::Error(e) => panic!("pipeline error: {}", e.error()),
            _ => {}
        }
    }
    pipeline.set_state(gst::State::Null).unwrap();

    // Assert caps.
    let caps = caps.lock().unwrap().clone().expect("no caps negotiated");
    let s = caps.structure(0).unwrap();
    assert_eq!(s.name(), "audio/x-wma");
    assert_eq!(s.get::<i32>("wmaversion").unwrap(), 2);
    assert_eq!(s.get::<i32>("rate").unwrap(), 44100);
    assert_eq!(s.get::<i32>("channels").unwrap(), 2);
    assert_eq!(s.get::<i32>("block_align").unwrap(), block_align as i32);
    let codec_data = s.get::<gst::Buffer>("codec_data").unwrap();
    let map = codec_data.map_readable().unwrap();
    assert_eq!(map.as_slice(), &[0x00, 0x00, 0x00, 0x00, 0x1f, 0x00]);

    // Assert all data bytes were forwarded as packets.
    assert_eq!(total_bytes.load(Ordering::SeqCst), data.len() as u32);
    assert_eq!(buffer_count.load(Ordering::SeqCst), 4); // 256 / 64

    // Assert the duration query is answered from the dpds index. The last dpds
    // entry is 16384 decoded bytes => 16384 / (2ch * 2 bytes) = 4096 frames =>
    // 4096 / 44100 s.
    let srcpad = demux.static_pad("src").unwrap();
    let mut q = gst::query::Duration::new(gst::Format::Time);
    assert!(srcpad.query(q.query_mut()), "src pad did not answer duration query");
    let dur = match q.result() {
        gst::GenericFormattedValue::Time(Some(t)) => t,
        other => panic!("unexpected duration result: {other:?}"),
    };
    let expected = gst::ClockTime::from_nseconds(4096u64 * 1_000_000_000 / 44100);
    assert_eq!(dur, expected);

    // The src pad should report itself seekable in TIME now that dpds is known.
    let mut sq = gst::query::Seeking::new(gst::Format::Time);
    assert!(srcpad.query(sq.query_mut()), "src pad did not answer seeking query");
    assert!(sq.result().0, "stream should be seekable in TIME");
}
