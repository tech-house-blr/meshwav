# Built from prebuilt binaries by packaging/build-receiver-rpm.sh. libopus is found automatically.
%global debug_package %{nil}
%global __strip /bin/true
%global _build_id_links none

Name:           meshwav-receiver
Version:        %{pkgversion}
Release:        1
Summary:        Play audio streamed from a Meshwav agent over Tailscale
License:        Apache-2.0
URL:            https://github.com/tech-house-blr/meshwav
Requires:       pulseaudio-utils
Recommends:     tailscale

%description
A small window (meshwav-receiver-gui) and a command line tool (meshwav-receiver)
that pair with a Meshwav agent and play its stream through PulseAudio or
PipeWire. Shows a tray icon where supported.

%install
install -Dm755 %{_sourcedir}/meshwav-receiver %{buildroot}%{_bindir}/meshwav-receiver
install -Dm755 %{_sourcedir}/meshwav-receiver-gui %{buildroot}%{_bindir}/meshwav-receiver-gui
install -Dm644 %{_sourcedir}/meshwav-receiver.desktop %{buildroot}%{_datadir}/applications/meshwav-receiver.desktop
install -Dm644 %{_sourcedir}/icon-256.png %{buildroot}%{_datadir}/icons/hicolor/256x256/apps/meshwav-receiver.png
install -Dm644 %{_sourcedir}/icon.svg %{buildroot}%{_datadir}/icons/hicolor/scalable/apps/meshwav-receiver.svg
install -Dm644 %{_sourcedir}/LICENSE %{buildroot}%{_licensedir}/meshwav-receiver/LICENSE

%files
%license %{_licensedir}/meshwav-receiver/LICENSE
%{_bindir}/meshwav-receiver
%{_bindir}/meshwav-receiver-gui
%{_datadir}/applications/meshwav-receiver.desktop
%{_datadir}/icons/hicolor/256x256/apps/meshwav-receiver.png
%{_datadir}/icons/hicolor/scalable/apps/meshwav-receiver.svg
