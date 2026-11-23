#!/bin/sh
# Build a .deb from a release binary:  packaging/build-deb.sh [binary] [outdir]
set -eu
. "$(dirname "$0")/lib.sh"
bin=${1:-$root/target/release/meshwav-agent}
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
install -d "$d/usr/share/man/man1"
gzip -9n < "$root/packaging/meshwav-agent.1" > "$d/usr/share/man/man1/$pkg.1.gz"
chmod 644 "$d/usr/share/man/man1/$pkg.1.gz"
size=$(du -sk "$d" | cut -f1)
install -d "$d/DEBIAN"
cat > "$d/DEBIAN/control" <<CTL
Package: $pkg
Version: $version
Architecture: $arch
Maintainer: Shreyas Salankimatt <shreyassalankimatt@gmail.com>
Installed-Size: $size
Depends: libc6, libgcc-s1, libopus0, pulseaudio-utils
Suggests: tailscale
Section: sound
Priority: optional
Homepage: https://github.com/tech-house-blr/meshwav
Description: stream this computer's audio to a paired phone over Tailscale
 Captures the system audio, encodes it (Opus or lossless PCM) and sends it
 encrypted to a paired receiver on your tailnet. Runs as a systemd user
 service: systemctl --user enable --now meshwav-agent, then meshwav-agent pair.
CTL
mkdir -p "$out"
dpkg-deb --root-owner-group --build "$d" "$out/${pkg}_${version}_${arch}.deb" >/dev/null
echo "$out/${pkg}_${version}_${arch}.deb"
