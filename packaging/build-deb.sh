#!/bin/sh
# Build the .deb from the release binary:  packaging/build-deb.sh [meshwav binary] [outdir]
set -eu
. "$(dirname "$0")/lib.sh"
bin=${1:-$root/target/release/meshwav}
out=${2:-$root/dist}
[ -x "$bin" ] || { echo "no executable at $bin" >&2; exit 1; }
arch=$(dpkg --print-architecture)
stage=$(mktemp -d)
trap 'rm -rf "$stage"' EXIT
d=$stage/${pkg}_${version}_${arch}
install -Dm755 "$bin" "$d/usr/bin/$pkg"
strip --strip-unneeded "$d/usr/bin/$pkg" 2>/dev/null || true
install -Dm644 "$root/packaging/systemd/meshwav-agent.service" "$d/usr/lib/systemd/user/meshwav-agent.service"
install -Dm644 "$root/packaging/config.toml.example" "$d/usr/share/doc/$pkg/config.toml.example"
install -Dm644 "$root/packaging/copyright" "$d/usr/share/doc/$pkg/copyright"
install -Dm644 "$root/packaging/meshwav.desktop" "$d/usr/share/applications/meshwav.desktop"
install -Dm644 "$root/crates/linux-receiver-gui/assets/icon-256.png" "$d/usr/share/icons/hicolor/256x256/apps/meshwav.png"
install -Dm644 "$root/assets/icon.svg" "$d/usr/share/icons/hicolor/scalable/apps/meshwav.svg"
install -d "$d/usr/share/man/man1"
gzip -9n < "$root/packaging/meshwav.1" > "$d/usr/share/man/man1/$pkg.1.gz"
chmod 644 "$d/usr/share/man/man1/$pkg.1.gz"
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
Conflicts: meshwav-agent, meshwav-receiver
Replaces: meshwav-agent, meshwav-receiver
Section: sound
Priority: optional
Homepage: https://github.com/tech-house-blr/meshwav
Description: private low-latency audio over your tailnet, send and receive
 One program with a window and a command line. Send this computer's audio
 (Opus or lossless PCM, encrypted) to paired phones and computers, or play
 audio sent from another device. Runs the sender as a systemd user service:
 meshwav, then Send, or systemctl --user enable --now meshwav-agent.
CTL
mkdir -p "$out"
dpkg-deb --root-owner-group --build "$d" "$out/${pkg}_${version}_${arch}.deb" >/dev/null
echo "$out/${pkg}_${version}_${arch}.deb"
