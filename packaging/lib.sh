# Shared by build-deb.sh and build-rpm.sh (one package: meshwav): where things come from and the package version.
root=$(cd "$(dirname "$0")/.." && pwd)
pkg=meshwav
version=$(sed -n 's/^version = "\(.*\)"/\1/p' "$root/Cargo.toml" | head -1)
[ -n "$version" ] || { echo "cannot read the version from Cargo.toml" >&2; exit 1; }
