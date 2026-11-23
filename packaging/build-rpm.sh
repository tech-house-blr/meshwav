#!/bin/sh
# Build an .rpm from a release binary:  packaging/build-rpm.sh [binary] [outdir]   (needs rpmbuild)
set -eu
. "$(dirname "$0")/lib.sh"
bin=${1:-$root/target/release/meshwav-agent}
out=${2:-$root/dist}
[ -x "$bin" ] || { echo "no executable at $bin" >&2; exit 1; }
top=$(mktemp -d)
trap 'rm -rf "$top"' EXIT
mkdir -p "$top/SOURCES" "$out"
cp "$bin" "$top/SOURCES/meshwav-agent"
cp "$root/packaging/systemd/meshwav-agent.service" "$root/packaging/config.toml.example" "$root/packaging/meshwav-agent.1" "$root/LICENSE" "$top/SOURCES/"
rpmbuild -bb --quiet --define "_topdir $top" --define "pkgversion $version" \
    --define "_userunitdir /usr/lib/systemd/user" "$root/packaging/meshwav-agent.spec"
cp "$top"/RPMS/*/*.rpm "$out/"
ls "$out"/*.rpm
