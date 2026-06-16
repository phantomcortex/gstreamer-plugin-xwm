# CLAUDE.md — gstreamer-plugin-xwm

Part of the **DistinctionOS** subproject family (`../DistinctionOS`). Built into
the image as an RPM from the cache-builder pipeline.

GStreamer plugin that makes Microsoft **xWMA** (`.xwm`) and Bethesda **FUZ**
(`.fuz`) audio files play in any GStreamer application (Decibels, Totem, etc.).
It provides typefinders for both formats plus two demuxers: **`xwmademux`**
(parses the xWMA RIFF container) and **`fuzdemux`** (strips the FUZ header and
feeds the embedded xWMA stream to `xwmademux`). Actual decoding is delegated to
`avdec_wmav2` from `gst-libav` via `decodebin` autoplugging.

## Build environment

All compilation and testing happens **inside the `Rawhide` distrobox container**, never
on the host. Build deps already installed there: `gstreamer1-devel`,
`gstreamer1-plugins-base-devel`, `gstreamer1-plugins-good`, `gstreamer1-libav`
(provides `avdec_wmav2`), `cargo-c`.

```bash
# Build the plugin (.so) with cargo-c
distrobox enter Rawhide -- bash -lc 'cd <repo> && cargo cbuild --release'

# Inspect without installing
distrobox enter Rawhide -- bash -lc \
  'cd <repo> && GST_PLUGIN_PATH=$PWD/target/<triple>/release gst-inspect-1.0 xwmademux'

# Plain cargo build / test also work
distrobox enter Rawhide -- bash -lc 'cd <repo> && cargo build && cargo test'
```

## Layout

- `src/lib.rs` — `plugin_define!`, registers all typefinders and demuxers.
- `src/typefind.rs` — typefinders for `audio/x-xwma` (RIFF/XWMA magic, `.xwm`) and
  `audio/x-fuz` (FUZE magic, `.fuz`), both at PRIMARY rank.
- `src/xwmademux/imp.rs` — streaming RIFF parser; emits `audio/x-wma` caps with the fixed
  6-byte WMAv2 `codec_data` (`00 00 00 00 1F 00`) and pushes `nBlockAlign` packets.
- `src/fuzdemux/imp.rs` — strips the FUZE header (magic + version byte + lip-animation data)
  and passes the remaining RIFF/XWMA bytes downstream as `audio/x-xwma`.
- `data/gstreamer-plugin-xwm.xml` — freedesktop MIME type definitions for `.xwm` and `.fuz`.
- `packaging/gstreamer-plugin-xwm.spec` — Fedora RPM (installs `.so` + MIME file,
  requires `gstreamer1-libav`).

## Verify end-to-end

```bash
# xWMA
gst-discoverer-1.0 sample.xwm
gst-launch-1.0 filesrc location=sample.xwm ! decodebin ! audioconvert ! autoaudiosink

# FUZ
gst-discoverer-1.0 sample.fuz
gst-launch-1.0 filesrc location=sample.fuz ! decodebin ! audioconvert ! autoaudiosink
```

## Status / TODO

- Duration reporting and accurate `dpds`-based seeking are implemented (push-mode:
  TIME seeks are translated to upstream byte seeks at packet boundaries).
- Only WMAv2 (`wFormatTag` 0x0161) is verified; other tags warn and proceed.
