Name:           gstreamer-plugin-xwm
Version:        1.1.0
Release:        1%{?dist}
Summary:        GStreamer plugin for Microsoft xWMA (.xwm) and Bethesda FUZ (.fuz) audio

License:        MPL-2.0
URL:            https://github.com/phantomcortex/gstreamer-plugin-xwm
Source0:        %{name}-%{version}.tar.gz

BuildRequires:  rust >= 1.80
BuildRequires:  cargo
BuildRequires:  cargo-c
BuildRequires:  gstreamer1-devel
BuildRequires:  gstreamer1-plugins-base-devel
BuildRequires:  pkgconfig(glib-2.0)
BuildRequires:  shared-mime-info

# The plugin only demuxes; actual WMA decoding is delegated to avdec_wmav2,
# which ships in gstreamer1-libav.
Requires:       gstreamer1 >= 1.20
Requires:       gstreamer1-libav
Requires(post): shared-mime-info
Requires(postun): shared-mime-info

%description
GStreamer typefinders and demuxers for Microsoft xWMA (.xwm) and Bethesda FUZ
(.fuz) audio files. xWMA is a RIFF container wrapping WMAv2 audio used by many
games; FUZ is Bethesda's dialogue format that embeds xWMA audio with optional
lip-animation data. With this plugin installed, any GStreamer application
(Decibels, Totem, Rhythmbox, ...) can play both formats transparently via
decodebin/playbin autoplugging, delegating the actual decode to avdec_wmav2
from gstreamer1-libav.

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

install -Dpm 0644 data/gstreamer-plugin-xwm.xml \
    %{buildroot}%{_datadir}/mime/packages/gstreamer-plugin-xwm.xml

%post
update-mime-database %{_datadir}/mime &>/dev/null ||:

%postun
update-mime-database %{_datadir}/mime &>/dev/null ||:

%files
%license LICENSE
%doc README.md
%{_libdir}/gstreamer-1.0/libgstxwm.so
%{_datadir}/mime/packages/gstreamer-plugin-xwm.xml

%changelog
* Tue Jun 16 2026 killawatt <phantom.github@proton.me> - 1.1.0-1
- Add fuzdemux demuxer for Bethesda .fuz dialogue audio format.
- Register freedesktop MIME types for .xwm and .fuz files.

* Thu Jun 11 2026 killawatt <phantom.github@proton.me> - 1.0.0-1
- First stable release.
- xWMA typefinder + xwmademux demuxer; decode delegated to avdec_wmav2.
- Accurate duration and seeking derived from the dpds index.
