#!/bin/sh
# Build the .rpm from the release binary:  packaging/build-rpm.sh [meshwav binary] [outdir]   (needs rpmbuild)
set -eu
. "$(dirname "$0")/lib.sh"
bin=${1:-$root/target/release/meshwav}
out=${2:-$root/dist}
[ -x "$bin" ] || { echo "no executable at $bin" >&2; exit 1; }
top=$(mktemp -d)
trap 'rm -rf "$top"' EXIT
mkdir -p "$top/SOURCES" "$out"
cp "$bin" "$top/SOURCES/meshwav"
cp "$root/packaging/systemd/meshwav-agent.service" "$root/packaging/config.toml.example" "$root/packaging/meshwav.1" "$root/packaging/meshwav.desktop" \
    "$root/crates/linux-receiver-gui/assets/icon-256.png" "$root/assets/icon.svg" "$root/LICENSE" "$top/SOURCES/"
rpmbuild -bb --quiet --define "_topdir $top" --define "pkgversion $version" \
    --define "_userunitdir /usr/lib/systemd/user" "$root/packaging/meshwav.spec"
cp "$top"/RPMS/*/*.rpm "$out/"
ls "$out"/meshwav-*.rpm
