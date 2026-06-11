# CLAUDE.md — gstreamer-plugin-xwm

GStreamer plugin that makes Microsoft **xWMA** (`.xwm`) audio files play in any
GStreamer application (Decibels, Totem, etc.). It provides a **typefinder** for the
xWMA RIFF magic and an **`xwmademux`** demuxer; actual decoding is delegated to
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

- `src/lib.rs` — `plugin_define!`, registers typefinder + demuxer.
- `src/typefind.rs` — `audio/x-xwma` typefinder (RIFF/XWMA magic, `.xwm` ext, PRIMARY rank).
- `src/xwmademux/imp.rs` — streaming RIFF parser; emits `audio/x-wma` caps with the fixed
  6-byte WMAv2 `codec_data` (`00 00 00 00 1F 00`) and pushes `nBlockAlign` packets.
- `packaging/gstreamer-plugin-xwm.spec` — Fedora RPM (installs `libgstxwm.so` to
  `%{_libdir}/gstreamer-1.0/`, requires `gstreamer1-libav`).

## Verify end-to-end

```bash
gst-discoverer-1.0 sample.xwm
gst-launch-1.0 filesrc location=sample.xwm ! decodebin ! audioconvert ! autoaudiosink
```

## Status / TODO

- Duration reporting and accurate `dpds`-based seeking are implemented (push-mode:
  TIME seeks are translated to upstream byte seeks at packet boundaries).
- Only WMAv2 (`wFormatTag` 0x0161) is verified; other tags warn and proceed.
