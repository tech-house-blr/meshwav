# Built from a prebuilt binary by packaging/build-rpm.sh. Library dependencies (libopus) are found automatically.
%global debug_package %{nil}
%global __strip /bin/true
%global _build_id_links none

Name:           meshwav
Version:        %{pkgversion}
Release:        1
Summary:        Private low-latency audio over your tailnet, send and receive
License:        Apache-2.0
URL:            https://github.com/tech-house-blr/meshwav
Requires:       pulseaudio-utils
Recommends:     tailscale
Obsoletes:      meshwav-agent < 0.3.0
Obsoletes:      meshwav-receiver < 0.3.0

%description
One program with a window and a command line. Send this computer's audio
(Opus or lossless PCM, encrypted) to paired phones and computers, or play
audio sent from another device. The sender runs as a systemd user service:
meshwav, then Send, or systemctl --user enable --now meshwav-agent.

%install
install -Dm755 %{_sourcedir}/meshwav %{buildroot}%{_bindir}/meshwav
install -Dm644 %{_sourcedir}/meshwav-agent.service %{buildroot}%{_userunitdir}/meshwav-agent.service
install -Dm644 %{_sourcedir}/config.toml.example %{buildroot}%{_docdir}/meshwav/config.toml.example
install -Dm644 %{_sourcedir}/meshwav.desktop %{buildroot}%{_datadir}/applications/meshwav.desktop
install -Dm644 %{_sourcedir}/icon-256.png %{buildroot}%{_datadir}/icons/hicolor/256x256/apps/meshwav.png
install -Dm644 %{_sourcedir}/icon.svg %{buildroot}%{_datadir}/icons/hicolor/scalable/apps/meshwav.svg
install -Dm644 %{_sourcedir}/LICENSE %{buildroot}%{_licensedir}/meshwav/LICENSE
install -Dm644 %{_sourcedir}/meshwav.1 %{buildroot}%{_mandir}/man1/meshwav.1

%files
%license %{_licensedir}/meshwav/LICENSE
%{_bindir}/meshwav
%{_userunitdir}/meshwav-agent.service
%{_docdir}/meshwav/config.toml.example
%{_datadir}/applications/meshwav.desktop
%{_datadir}/icons/hicolor/256x256/apps/meshwav.png
%{_datadir}/icons/hicolor/scalable/apps/meshwav.svg
%{_mandir}/man1/meshwav.1*
