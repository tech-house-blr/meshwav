#!/bin/sh
# Build the receiver .rpm from release binaries:  packaging/build-receiver-rpm.sh [cli] [gui] [outdir]   (needs rpmbuild)
set -eu
. "$(dirname "$0")/lib.sh"
cli=${1:-$root/target/release/meshwav-receiver}
gui=${2:-$root/target/release/meshwav-receiver-gui}
out=${3:-$root/dist}
for b in "$cli" "$gui"; do [ -x "$b" ] || { echo "no executable at $b" >&2; exit 1; }; done
top=$(mktemp -d)
trap 'rm -rf "$top"' EXIT
mkdir -p "$top/SOURCES" "$out"
cp "$cli" "$top/SOURCES/meshwav-receiver"
cp "$gui" "$top/SOURCES/meshwav-receiver-gui"
cp "$root/packaging/receiver/meshwav-receiver.desktop" "$root/crates/linux-receiver-gui/assets/icon-256.png" "$root/assets/icon.svg" "$root/LICENSE" "$top/SOURCES/"
rpmbuild -bb --quiet --define "_topdir $top" --define "pkgversion $version" "$root/packaging/receiver/meshwav-receiver.spec"
cp "$top"/RPMS/*/*.rpm "$out/"
ls "$out"/meshwav-receiver-*.rpm
