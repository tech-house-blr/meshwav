# Built from a prebuilt binary by packaging/build-rpm.sh. Library dependencies (libopus) are found automatically.
%global debug_package %{nil}
%global __strip /bin/true

Name:           meshwav-agent
Version:        %{pkgversion}
Release:        1
Summary:        Stream this computer's audio to a paired phone over Tailscale
License:        Apache-2.0
URL:            https://github.com/tech-house-blr/meshwav
Requires:       pulseaudio-utils
Recommends:     tailscale

%description
Captures the system audio, encodes it (Opus or lossless PCM) and sends it
encrypted to a paired receiver on your tailnet. Runs as a systemd user
service: systemctl --user enable --now meshwav-agent, then meshwav-agent pair.

%install
install -Dm755 %{_sourcedir}/meshwav-agent %{buildroot}%{_bindir}/meshwav-agent
install -Dm644 %{_sourcedir}/meshwav-agent.service %{buildroot}%{_userunitdir}/meshwav-agent.service
install -Dm644 %{_sourcedir}/config.toml.example %{buildroot}%{_docdir}/meshwav-agent/config.toml.example
install -Dm644 %{_sourcedir}/LICENSE %{buildroot}%{_licensedir}/meshwav-agent/LICENSE
install -Dm644 %{_sourcedir}/meshwav-agent.1 %{buildroot}%{_mandir}/man1/meshwav-agent.1

%files
%license %{_licensedir}/meshwav-agent/LICENSE
%{_bindir}/meshwav-agent
%{_userunitdir}/meshwav-agent.service
%{_docdir}/meshwav-agent/config.toml.example
%{_mandir}/man1/meshwav-agent.1*
