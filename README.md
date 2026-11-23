# Meshwav

Private, low-latency system-audio streaming between your own devices over Tailscale: a Linux computer or an Android phone sends, a Linux computer or an Android phone plays.
No cloud, no relay servers, no third-party services.

**Status: early development.** Sending from Linux, receiving on Android or Linux, and sending from an Android phone work end to end on a tailnet. Releases are on GitHub (v0.3.0 is the first with the single `meshwav` package described below). Windows is not implemented, latency has not reached its target (see [docs/measurements.md](docs/measurements.md)), and the security design has not had an external review.
See [docs/milestones.md](docs/milestones.md) for what is done and what is open.

## How it works

- A sender captures audio, encodes it (Opus, or lossless PCM) and sends it over UDP to the one paired receiver that is active; others stay connected on standby. On Linux the sender is `meshwav send` (PulseAudio/PipeWire via `parec`); on Android it is the app's Send mode (other apps' audio, via Android's audio playback capture).
- Senders and receivers listen and connect only on Tailscale (or loopback) addresses and refuse anything else.
- Devices pair once with a six-digit code (Noise handshake). Keys are then pinned and each session gets fresh keys. Packets are authenticated and encrypted (ChaCha20-Poly1305) with a replay window.
- The receiver uses an adaptive jitter buffer, loss concealment and automatic reconnect.

## Install (Linux)

One package, `meshwav`, does both directions: the window, the sender and the receiver. Packages are attached to each [GitHub release](https://github.com/tech-house-blr/meshwav/releases) (v0.3.0 is the first with the single package) for x86_64 and aarch64:

```sh
sudo apt install ./meshwav_VERSION_ARCH.deb      # Debian, Ubuntu, Raspberry Pi OS
sudo dnf install ./meshwav-VERSION-1.ARCH.rpm    # Fedora, openSUSE (zypper install)
```

Arch: `packaging/PKGBUILD` builds the tagged source (`makepkg -si`). It is not on the AUR yet.

It replaces the older `meshwav-agent` and `meshwav-receiver` packages (the package manager removes them on install). Their commands are gone from the packages: use `meshwav send` and `meshwav receive`. Settings and paired devices are kept.

Then open **Meshwav** from the application menu (or run `meshwav`). Choose Send to set up and start the sender, or Receive to play audio from another device. Without the window:

```sh
systemctl --user enable --now meshwav-agent    # the sender, as a user service
meshwav pair                                   # prints a 6-digit code
```

The package needs `libopus`, `pulseaudio-utils` (`parec`, `pactl`, `pacat`; they work with PipeWire too) and a few graphics libraries for the window (`libxkbcommon`, EGL or GL). It installs the program, the user unit, the application-menu entry and icon, a man page (`man meshwav`) and an example config. Tailscale must run on both devices. The packages are not in the distributions' own repositories, so there is no `apt install meshwav` without the file yet. Release binaries are built on Ubuntu 22.04, so they need glibc 2.35 or newer. The release tarball holds the `meshwav` binary, the unit, the example config and `scripts/install-user-service.sh`.

## One program

Run `meshwav` with no arguments for the window. It has a Receive / Send switch (the last choice is remembered; only one side runs at a time). Send starts and stops the `meshwav-agent` systemd user service (the unit keeps this name; it runs `meshwav send`), shows a pairing code with a 2-minute countdown, lists paired receivers (play here, rename, choose automatically) and edits the default quality (Opus or Lossless; the Opus bitrate and frame size are under it) and the keep-playing-locally setting in `~/.config/meshwav/config.toml`. If no service exists it offers to set one up for your user. The service keeps sending when the window is closed. The same things work from the command line:

`meshwav send` is the sender, `meshwav receive run|outputs|forget` is the receiver, and `meshwav pair`, `devices`, `use` and `rename` manage pairing and the active receiver. `man meshwav` lists everything. Built from source, `meshwav-agent` and `meshwav-receiver` still exist as thin compatibility commands running the same code, but the packages do not install them. The receiver's files are in `~/.config/meshwav/receiver`; an existing `~/.config/meshwav-receiver` is moved there the first time it runs.

## Play on a Linux computer (receiver)

`meshwav receive` plays a sender's stream on another Linux machine, the way the Android app does (same handshake, jitter buffer, Opus and lossless PCM, loss concealment). It needs `pacat` (`pulseaudio-utils`, also works with PipeWire) for sound.

```sh
# on the sending machine: open a 2-minute pairing window; it prints a 6-digit code
meshwav pair

# on the receiving machine, once: type just the code (the sender is found on your tailnet)
meshwav pair 123456

# then, any time
meshwav receive run
```

For a window instead of commands, run `meshwav` and choose Receive: it has a pairing screen where you type the sender's 6-digit code, shows Playing, Standby or Connecting, buffer and loss numbers, a quality choice and a volume slider. It puts an icon in the notification area (menu: state, Show window, Quit) on desktops that support StatusNotifierItem. It connects by itself when already paired and says goodbye to the sender when closed.

Only the active device gets the audio. With several receivers paired, choose with `meshwav use <name>`; the others show "Standby" and take over when chosen. Ctrl-C stops the receiver and hands the sender's audio back. `--output FILE` writes the received audio as raw 48 kHz stereo s16le instead of playing it (for machines without a sound server). The receiver connects only to tailnet or loopback addresses.

## Build and run (Linux)

Requirements: Rust (stable), `cmake`, `pkg-config`, `libopus-dev`, `parec`/`pactl` (PulseAudio or PipeWire-pulse), Tailscale running on both devices.

```sh
cargo build --release -p meshwav
./target/release/meshwav            # the window
./target/release/meshwav send --pair
```

### Run it as a service (systemd, per user)

```sh
scripts/install-user-service.sh        # copies the binary to ~/.local/bin, installs and starts the unit
meshwav pair                           # prints a code for the app; works while the service runs
systemctl --user status meshwav-agent
journalctl --user -u meshwav-agent -f
loginctl enable-linger "$USER"         # optional: start at boot without logging in
```

It is a user service because it needs your audio session. If the tailnet is not up yet the sender exits and systemd retries every 5 s. Stopping it hands the PC's audio output back first.

Settings live in `~/.config/meshwav/config.toml` (example: `packaging/config.toml.example`): `bind`, `kbps`, `frame_ms`, `codec` (`opus` or `lossless`, what is sent until a receiver picks), `source`, `no_redirect`, `log_level`. Flags override the file, and a typo or bad value stops the sender with the line number. Restart the service after editing it.

Several receivers can be paired and connected at once; one gets the audio. With the sender running, from another terminal:

```
meshwav pair                         # open a 2-minute pairing window on the running sender
meshwav devices                      # who is paired, online, active or standby
meshwav use "Phone name"             # send the audio there (name, name prefix or key prefix)
meshwav use auto                     # first connected device keeps it again
meshwav rename "Phone name" "New name"
```

The chosen device is remembered across restarts. A device that is not chosen never takes the stream from the active one.

Common `meshwav send` flags: `--bind IP:PORT`, `--kbps 160`, `--frame-ms 10`, `--codec opus|lossless`, `--source SINK.monitor`, `--no-redirect`, `--identity PATH`, `--authorized PATH`, `--config PATH`, `--log-level error|warn|info|debug`.
Full list at the top of `crates/desktop-agent/src/lib.rs`.

## Android receiver

Kotlin app in `apps/android` (Gradle, version 0.7.1). Build with `./gradlew assembleDebug` inside that directory (it needs the native library from `crates/android-spike`, built with `cargo ndk`). Release APKs, when attached, are debug-signed: there is no release signing key yet. The app has a Receive / Send switch (the last mode is remembered). Receive: pair by typing only the 6-digit code: on the computer open a pairing window (`meshwav pair`, or Send, Show pairing code in the window), then on the phone choose Receive, type the code and tap Pair. While the window is open the sender tells waiting phones on the tailnet where it is, so no address or key is typed. A `meshwav://pair` link still works under "Use a pairing link instead". The app remembers every sender it has paired with. Under "Paired senders" you can Connect to one, Make default, or Forget it. With "Reconnect automatically" on (the default) it connects to the default sender whenever the app opens, so it shows as online without any tap; Disconnect stops it until you ask again, and tapping Pair always stops the current connection first so a new code is used. Lossless PCM is a Quality setting on the receiving side: in the phone app, and in the Linux receive window (or `meshwav receive run --lossless`).

Send mode turns the phone into a sender: tap Start, allow the screen-capture prompt (Android's own wording; only audio is taken), choose Lossless or Opus, and the app shows a pairing code, a link, and sent / dropped / level numbers. On the computer, run `meshwav pair 123456` or use the Receive window's pairing screen with the code; the computer finds the phone on the tailnet. While sending, media volume on the phone is held at 0 and restored afterwards (some phones pause playback at 0). Lossless here is raw 48 kHz 16-bit audio as captured after Android's mixer, so it is not bit-perfect; apps that block capture may be silent. The computer picks its output under Receive (`meshwav receive outputs`, `run --device SINK`).

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
