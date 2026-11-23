#!/bin/sh
# Build the receiver .deb from release binaries:  packaging/build-receiver-deb.sh [cli] [gui] [outdir]
set -eu
. "$(dirname "$0")/lib.sh"
pkg=meshwav-receiver
cli=${1:-$root/target/release/meshwav-receiver}
gui=${2:-$root/target/release/meshwav-receiver-gui}
out=${3:-$root/dist}
for b in "$cli" "$gui"; do [ -x "$b" ] || { echo "no executable at $b" >&2; exit 1; }; done
arch=$(dpkg --print-architecture)
stage=$(mktemp -d)
trap 'rm -rf "$stage"' EXIT
d=$stage/${pkg}_${version}_${arch}
install -Dm755 "$cli" "$d/usr/bin/meshwav-receiver"
install -Dm755 "$gui" "$d/usr/bin/meshwav-receiver-gui"
strip --strip-unneeded "$d/usr/bin/meshwav-receiver" "$d/usr/bin/meshwav-receiver-gui" 2>/dev/null || true
install -Dm644 "$root/packaging/receiver/meshwav-receiver.desktop" "$d/usr/share/applications/meshwav-receiver.desktop"
install -Dm644 "$root/crates/linux-receiver-gui/assets/icon-256.png" "$d/usr/share/icons/hicolor/256x256/apps/meshwav-receiver.png"
install -Dm644 "$root/assets/icon.svg" "$d/usr/share/icons/hicolor/scalable/apps/meshwav-receiver.svg"
install -Dm644 "$root/packaging/copyright" "$d/usr/share/doc/$pkg/copyright"
size=$(du -sk "$d" | cut -f1)
install -d "$d/DEBIAN"
cat > "$d/DEBIAN/control" <<CTL
Package: $pkg
Version: $version
Architecture: $arch
Maintainer: Shreyas Salankimatt <shreyassalankimatt@gmail.com>
Installed-Size: $size
Depends: libc6, libgcc-s1, libopus0, pulseaudio-utils, libxkbcommon0, libegl1 | libgl1
Recommends: libwayland-client0, libx11-6, libxcursor1, libxi6, libxrandr2, libxkbcommon-x11-0
Suggests: tailscale
Section: sound
Priority: optional
Homepage: https://github.com/tech-house-blr/meshwav
Description: play audio streamed from a Meshwav agent over Tailscale
 A small window (meshwav-receiver-gui) and a command line tool
 (meshwav-receiver) that pair with a Meshwav agent and play its stream
 through PulseAudio or PipeWire. Shows a tray icon where supported.
CTL
mkdir -p "$out"
dpkg-deb --root-owner-group --build "$d" "$out/${pkg}_${version}_${arch}.deb" >/dev/null
echo "$out/${pkg}_${version}_${arch}.deb"
