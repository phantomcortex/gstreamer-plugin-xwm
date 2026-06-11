Name:           gstreamer-plugin-xwm
Version:        1.0.0
Release:        1%{?dist}
Summary:        GStreamer plugin for Microsoft xWMA (.xwm) audio

License:        MPL-2.0
URL:            https://github.com/phantomcortex/gstreamer-plugin-xwm
Source0:        %{name}-%{version}.tar.gz

BuildRequires:  rust >= 1.80
BuildRequires:  cargo
BuildRequires:  cargo-c
BuildRequires:  gstreamer1-devel
BuildRequires:  gstreamer1-plugins-base-devel
BuildRequires:  pkgconfig(glib-2.0)

# The plugin only demuxes; actual WMA decoding is delegated to avdec_wmav2,
# which ships in gstreamer1-libav.
Requires:       gstreamer1 >= 1.20
Requires:       gstreamer1-libav

%description
A GStreamer typefinder and demuxer (xwmademux) for Microsoft xWMA (.xwm) audio
files, a RIFF container wrapping WMAv2 audio used by many games. With this plugin
installed, any GStreamer application (Decibels, Totem, Rhythmbox, ...) can play
.xwm files transparently via decodebin/playbin autoplugging, delegating the actual
decode to avdec_wmav2 from gstreamer1-libav.

%prep
%autosetup -n %{name}-%{version}

%build
# cargo-c builds a proper GStreamer plugin .so and places it under the
# gstreamer-1.0 subdir (see [package.metadata.capi] in Cargo.toml).
cargo cbuild --release --prefix=%{_prefix} --libdir=%{_libdir}

%install
cargo cinstall --release \
    --prefix=%{_prefix} \
    --libdir=%{_libdir} \
    --destdir=%{buildroot}

# We ship only the runtime plugin, not dev artifacts (cargo-c also emits a
# pkg-config file and a static archive for capi consumers).
rm -f %{buildroot}%{_libdir}/pkgconfig/gstxwm.pc
rm -f %{buildroot}%{_libdir}/gstreamer-1.0/libgstxwm.a

%files
%license LICENSE
%doc README.md
%{_libdir}/gstreamer-1.0/libgstxwm.so

%changelog
* Thu Jun 11 2026 killawatt <killawattgamer@gmail.com> - 1.0.0-1
- First stable release.
- xWMA typefinder + xwmademux demuxer; decode delegated to avdec_wmav2.
- Accurate duration and seeking derived from the dpds index.
