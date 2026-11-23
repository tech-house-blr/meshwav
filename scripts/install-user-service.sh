#!/bin/sh
# Install the sender as a systemd user service from a built copy (packages install the unit themselves).
#   scripts/install-user-service.sh [path/to/meshwav]
# Copies the binary to ~/.local/bin, installs the unit with that path, creates a commented
# config if none exists, then enables and starts the service. Re-run it to upgrade.
set -eu
here=$(cd "$(dirname "$0")/.." && pwd)
bin=${1:-$here/target/release/meshwav}
[ -x "$bin" ] || { echo "no executable at $bin (build with: cargo build --release -p meshwav)" >&2; exit 1; }
mkdir -p "$HOME/.local/bin" "$HOME/.config/systemd/user" "$HOME/.config/meshwav"
dest=$HOME/.local/bin/meshwav
if [ "$(readlink -f "$bin")" != "$(readlink -f "$dest")" ]; then
    cp "$bin" "$dest.new" && mv "$dest.new" "$dest"
fi
sed "s|^ExecStart=.*|ExecStart=$dest send|" "$here/packaging/systemd/meshwav-agent.service" > "$HOME/.config/systemd/user/meshwav-agent.service"
[ -e "$HOME/.config/meshwav/config.toml" ] || cp "$here/packaging/config.toml.example" "$HOME/.config/meshwav/config.toml"
systemctl --user daemon-reload
systemctl --user enable meshwav-agent.service
systemctl --user restart meshwav-agent.service
echo "installed. status: systemctl --user status meshwav-agent    logs: journalctl --user -u meshwav-agent -f"
echo "pair a device:     $dest pair"
echo "run without logging in (after boot): loginctl enable-linger $USER"
