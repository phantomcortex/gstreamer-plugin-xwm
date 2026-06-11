# gstreamer-plugin-xwm

A GStreamer plugin that makes **Microsoft xWMA** (`.xwm`) audio files play in any
GStreamer-based application — [Decibels](https://apps.gnome.org/Decibels/) (GNOME Audio
Player), Totem, Rhythmbox, etc. — with no special player required.

`.xwm` is a RIFF container (`RIFF…XWMA`) wrapping WMAv2 audio, used heavily by games
(e.g. Skyrim/Bethesda titles). GStreamer already ships the WMA decoder
(`avdec_wmav2`, from `gst-libav`), but `.xwm` files never play because nothing
**recognises** the container or autoplugs a demuxer for it. This plugin fills exactly
that gap.

## What it provides

| Feature | Description |
|---|---|
| `xwma_typefind` | Typefinder for the `RIFF…XWMA` magic + `.xwm` extension (rank PRIMARY) |
| `xwmademux` | Demuxer: parses the RIFF chunks and emits an `audio/x-wma` stream |

Decoding is **delegated to `avdec_wmav2`** via `decodebin` autoplugging. The demuxer
synthesises the fixed 6-byte WMAv2 `codec_data` (`00 00 00 00 1F 00`) that xWMA files
omit but the decoder requires.

```
filesrc ! [typefind] ! xwmademux ! avdec_wmav2 ! audioconvert ! autoaudiosink
          \________________ all autoplugged by decodebin/playbin _____________/
```

## Building

Requires GStreamer ≥ 1.20, `gst-libav` (for `avdec_wmav2`), Rust, and
[`cargo-c`](https://github.com/lu-zero/cargo-c).

```bash
cargo cbuild --release
# Try it without installing:
GST_PLUGIN_PATH=$PWD/target/<triple>/release \
  gst-launch-1.0 filesrc location=song.xwm ! decodebin ! audioconvert ! autoaudiosink
```

Install system-wide (drops `libgstxwm.so` into the GStreamer plugin dir):

```bash
cargo cinstall --release --prefix=/usr --libdir=/usr/lib64
```

### Fedora RPM

```bash
rpmdev-setuptree
git archive --format=tar.gz --prefix=gstreamer-plugin-xwm-0.1.0/ \
    -o ~/rpmbuild/SOURCES/gstreamer-plugin-xwm-0.1.0.tar.gz HEAD
cp packaging/gstreamer-plugin-xwm.spec ~/rpmbuild/SPECS/
rpmbuild -bb ~/rpmbuild/SPECS/gstreamer-plugin-xwm.spec
sudo dnf install ~/rpmbuild/RPMS/x86_64/gstreamer-plugin-xwm-0.1.0-1.*.x86_64.rpm
```

This installs `libgstxwm.so` to `%{_libdir}/gstreamer-1.0/`. Because `xwmademux`
is rank *primary*, it then outranks libav's `avdemux_xwma` (rank *marginal*) so
`decodebin`/`playbin` pick it automatically — giving correct duration and seeking.

> **Note on Decibels (and other flatpak apps):** a flatpak is sandboxed and uses
> its *own* bundled GStreamer, so it will **not** see a host-installed plugin and
> will keep falling back to `avdemux_xwma`. Use a non-flatpak (RPM) build of the
> player, or build this plugin against the matching freedesktop SDK and expose it
> with a flatpak override.

## Status

- ✅ Playback of WMAv2 xWMA via autoplugging (`decodebin`/`playbin`).
- ✅ Accurate duration and a complete progress bar (derived from the `dpds` index).
- ✅ Accurate seeking via the `dpds` index (TIME→byte mapping, per-packet timestamps).

## License

[MPL-2.0](LICENSE), matching the GStreamer Rust plugin convention.
