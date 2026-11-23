# Meshwav

Private, low-latency system-audio streaming from a computer to an Android phone over Tailscale.
No cloud, no relay servers, no third-party services.

**Status: early development (pre-release).** The Linux desktop agent and the Android receiver work end to end on a tailnet.
There are no release builds yet, the Windows capture path is not implemented, and the security design has not had an external review.
See [docs/milestones.md](docs/milestones.md) for what is done and what is open.

## How it works

- The desktop agent captures system audio (PulseAudio/PipeWire via `parec` on Linux), encodes it (Opus, or lossless PCM with automatic fall back to Opus) and sends it over UDP to a paired phone.
- The agent binds only to Tailscale addresses and refuses anything else.
- Devices pair once with a six-digit code (Noise handshake). Keys are then pinned and each session gets fresh keys. Packets are authenticated and encrypted (ChaCha20-Poly1305) with a replay window.
- The receiver uses an adaptive jitter buffer, loss concealment and automatic reconnect.

## Install (Linux agent)

Packages are attached to each [GitHub release](https://github.com/tech-house-blr/meshwav/releases) for x86_64 and aarch64:

```sh
sudo apt install ./meshwav-agent_VERSION_ARCH.deb      # Debian, Ubuntu, Raspberry Pi OS
sudo dnf install ./meshwav-agent-VERSION-1.ARCH.rpm    # Fedora, openSUSE (zypper install)
```

Arch: `packaging/PKGBUILD` builds the tagged source (`makepkg -si`). It is not on the AUR yet.

Then, as your normal user:

```sh
systemctl --user enable --now meshwav-agent
meshwav-agent pair
```

The packages need `libopus` and `pulseaudio-utils` (`parec`, `pactl`; they work with PipeWire too) and install the binary, the user unit, a man page and an example config. Tailscale must run on both devices. They are not in the distributions' own repositories, so there is no `apt install meshwav-agent` without the file yet. Release binaries are built on Ubuntu 22.04, so they need glibc 2.35 or newer.

## Play on a Linux computer (receiver)

`meshwav-receiver` plays an agent's stream on another Linux machine, the way the Android app does (same handshake, jitter buffer, Opus and lossless PCM, loss concealment). It needs `pacat` (`pulseaudio-utils`, also works with PipeWire) for sound.

```sh
# on the agent's machine: open a 2-minute pairing window and copy the `pair:` link it prints
meshwav-agent pair

# on the receiving machine, once
meshwav-receiver pair 'meshwav://pair?host=100.x.y.z&port=47900&pk=...&code=123456'

# then, any time
meshwav-receiver run
```

For a window instead of commands, run `meshwav-receiver-gui` (built with `cargo build --release -p meshwav-receiver-gui`): it has a pairing screen where you paste the agent's link, shows Playing, Standby or Connecting, buffer and loss numbers, and a volume slider. It puts an icon in the notification area (menu: state, Show window, Quit) on desktops that support StatusNotifierItem. It connects by itself when already paired and says goodbye to the agent when closed.

Only the active device gets the audio. With several receivers paired, choose with `meshwav-agent use <name>`; the others show "Standby" and take over when chosen. Ctrl-C stops the receiver and hands the agent's audio back. `--output FILE` writes the received audio as raw 48 kHz stereo s16le instead of playing it (for machines without a sound server). The receiver connects only to tailnet or loopback addresses. The receiver is not built into releases yet; build it with `cargo build --release -p meshwav-receiver`.

## Build and run (Linux agent)

Requirements: Rust (stable), `parec`/`pactl` (PulseAudio or PipeWire-pulse), Tailscale running on both devices.

```sh
cargo build --release -p meshwav-agent
./target/release/meshwav-agent --pair
```

### Run it as a service (systemd, per user)

```sh
scripts/install-user-service.sh        # copies the binary to ~/.local/bin, installs and starts the unit
meshwav-agent pair                     # prints a code for the app; works while the service runs
systemctl --user status meshwav-agent
journalctl --user -u meshwav-agent -f
loginctl enable-linger "$USER"         # optional: start at boot without logging in
```

It is a user service because it needs your audio session. If the tailnet is not up yet the agent exits and systemd retries every 5 s. Stopping it hands the PC's audio output back first.

Settings live in `~/.config/meshwav/config.toml` (example: `packaging/config.toml.example`): `bind`, `kbps`, `frame_ms`, `source`, `no_redirect`, `log_level`. Flags override the file, and a typo or bad value stops the agent with the line number. Restart the service after editing it.

Several phones can be paired and connected at once; one gets the audio. With the agent running, from another terminal:

```
meshwav-agent pair                   # open a 2-minute pairing window on the running agent
meshwav-agent devices                # who is paired, online, active or standby
meshwav-agent use "Phone name"       # send the audio there (name, name prefix or key prefix)
meshwav-agent use auto               # first connected device keeps it again
meshwav-agent rename "Phone name" "New name"
```

The chosen device is remembered across restarts. A device that is not chosen never takes the stream from the active one.

Common flags: `--bind IP:PORT`, `--kbps 160`, `--frame-ms 10`, `--source SINK.monitor`, `--no-redirect`, `--identity PATH`, `--authorized PATH`, `--config PATH`, `--log-level error|warn|info|debug`.
Full list at the top of `crates/desktop-agent/src/main.rs`.

## Android receiver

Kotlin app in `apps/android` (Gradle). Build with `./gradlew assembleDebug` inside that directory. Enter the agent's tailnet IP and the pairing code.

## Tests

```sh
cargo test --workspace
```

## Docs

- [docs/architecture.md](docs/architecture.md): components, stack, targets
- [docs/protocol.md](docs/protocol.md): wire protocol v1
- [docs/security.md](docs/security.md): threat model and assumptions
- [docs/risks.md](docs/risks.md): technical risks and mitigations
- [docs/measurements.md](docs/measurements.md): measured numbers and their limits
- [docs/milestones.md](docs/milestones.md): plan

## Security

Please report vulnerabilities privately; see [SECURITY.md](SECURITY.md).

## License

Apache-2.0. Part of the Mesh family (with Mesh Pilot), by Tech House BLR.
