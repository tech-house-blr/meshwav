<p align="center">
  <img src="assets/icon.svg" alt="Meshwav" width="128" height="128">
</p>

<h1 align="center">Meshwav</h1>

<p align="center">
  Private, low-latency audio between your own devices, over Tailscale.<br>
  No cloud, no relay servers, no accounts.
</p>

<p align="center">
  <a href="https://github.com/tech-house-blr/meshwav/actions/workflows/ci.yml"><img src="https://github.com/tech-house-blr/meshwav/actions/workflows/ci.yml/badge.svg" alt="CI"></a>
  <a href="https://github.com/tech-house-blr/meshwav/releases/latest"><img src="https://img.shields.io/github/v/release/tech-house-blr/meshwav" alt="Latest release"></a>
  <a href="LICENSE"><img src="https://img.shields.io/badge/license-Apache--2.0-blue" alt="License: Apache-2.0"></a>
</p>

Meshwav streams a device's audio to another device you own. A Linux or Windows computer or an Android phone can **send** (everything it plays), and a Linux or Windows computer or an Android phone can **receive** and play it. Devices find each other on your tailnet, pair once with a six-digit code, and from then on audio is encrypted end to end. Several receivers can be paired; exactly one plays at a time and you choose which.

> **Status: pre-1.0, still being tested.** It works end to end on a real tailnet. The end-to-end latency target is only met with the optional native Android output, which is off by default and was tested on two phones (see [Performance](#performance)). The Windows build is new. The security design has not been independently reviewed. See [SECURITY.md](SECURITY.md) before relying on it.

## Features

- **Send from Linux, Windows or Android, receive on any of them.** One `meshwav` program (window and command line) on Linux, one portable `meshwav.exe` on Windows, one app on Android with a Receive / Send switch.
- **Opus or lossless PCM.** Opus at 64 to 256 kbit/s with 5, 10 or 20 ms frames, or raw 48 kHz 16-bit PCM (about 1.7 Mbit/s) for a clean copy of what the sender plays. The receiver can switch live.
- **Private by construction.** Noise XX / XXpsk3 handshake, pinned device keys, per-session keys, ChaCha20-Poly1305 packets with a replay window. Everything listens and connects on Tailscale (or loopback) addresses only.
- **Pair with just a code.** The sender shows a six-digit code and a key fingerprint, and the other device finds it on your tailnet by itself. There is no address or key to type; you only confirm that the fingerprint it shows matches the sender's.
- **Choose where it plays.** Pair several receivers and pick the active one from the window or with `meshwav use`. The others stay connected on standby and never steal the stream.
- **Resilient playback.** Adaptive jitter buffer, loss concealment and automatic reconnect.
- **Runs in the background.** The Linux sender runs as a systemd user service, and the Windows sender runs hidden from your login startup. Both keep sending when the window is closed.

## Platforms

| Role | Linux | Android | Windows |
|---|---|---|---|
| Send | Yes (PulseAudio or PipeWire) | Yes (other apps' audio, via Android's audio capture) | Yes (WASAPI loopback of every app) |
| Receive | Yes | Yes | Yes (WASAPI) |

On Windows the sender captures the audio of every app before any output's volume is applied, and turns the default output down to 0 while a receiver is connected, then gives the volume back. It keeps working when you switch the default output (the muting moves to the new one). This needs Windows 10 version 2004 or later; on older versions it falls back to a loopback of the default output, which hears nothing on outputs whose volume is applied in software (HDMI monitors, for example) while they are muted. The sender has to run in your own login session, as the login startup does. Windows 11 on x86_64 is what has been tested.

## Quick start

You need [Tailscale](https://tailscale.com) running on both devices.

**1. Install on Linux.** Packages for x86_64 and aarch64 are on the [releases page](https://github.com/tech-house-blr/meshwav/releases):

```sh
sudo apt install ./meshwav_VERSION_ARCH.deb      # Debian, Ubuntu, Raspberry Pi OS
sudo dnf install ./meshwav-VERSION-1.ARCH.rpm    # Fedora (openSUSE: zypper install)
```

On Arch, `packaging/PKGBUILD` builds the tagged source (`makepkg -si`). On Windows, download `meshwav-VERSION-windows-x86_64.exe` from the release, check it against the `.sha256` file next to it, and put it anywhere; it needs no installer and no administrator rights. The file is not code-signed, so Windows SmartScreen asks you to confirm on first run (More info, then Run anyway). Its settings live in `%APPDATA%\meshwav`. On Android, install the APK attached to the release (signed with the Tech House release key; see [Verifying the Android APK](#verifying-the-android-apk)).

**2. Send from the computer.** Open **Meshwav** from the application menu (on Windows, double-click `meshwav.exe`), choose **Send**, and set up the sender. On Windows this adds it to your login startup. Or from a terminal on Linux:

```sh
systemctl --user enable --now meshwav-agent    # start the sender
meshwav pair                                   # open a 2-minute pairing window, prints a 6-digit code
```

**3. Receive on the phone.** Open the app, choose **Receive**, type the code and tap **Pair**. The phone shows the key fingerprint of the sender it found; check that it matches the one the computer shows next to the code and tap **Yes**. Audio from the computer now plays on the phone.

To receive on another Linux computer instead, run `meshwav pair 123456` there with the code the sender shows (it asks you to confirm the sender's fingerprint, or pass `--fingerprint 4F2A-91C0-7B3E`), then `meshwav receive run` (or choose Receive in the window).

## How it works

```
 sender                                                      receiver (the active one)
 ------                                                      -------------------------
 capture audio                                               jitter buffer
   |  PulseAudio / PipeWire, or Android audio capture          ^  adaptive, loss concealment
   v                                                           |
 encode (Opus or PCM)                                        decode (Opus or PCM)
   |                                                           ^
   v                                                           |
 seal (ChaCha20-Poly1305) ----- UDP over Tailscale ------>   open + replay check
```

- **Transport.** UDP datagrams over the tailnet. Tailscale provides the route and the WireGuard tunnel; Meshwav adds its own authentication and encryption on top, so another device on the tailnet still cannot listen in or inject audio.
- **Pairing.** A sender opens a pairing window (2 minutes on Linux, 10 on the phone) and shows a six-digit code. It tells waiting devices on the tailnet where it is, and the receiver proves it knows the code in a Noise XXpsk3 handshake. After that the two devices pin each other's keys, and later connections use a plain XX handshake with fresh session keys. Five wrong codes lock pairing for 60 seconds.
- **Receiver selection.** Every paired receiver can stay connected. The sender tells each one whether it is active or on standby, and only the active one gets audio (the others get keepalives). A newly connected device never takes over.
- **Local output.** While a receiver is active, the Linux sender redirects its own output to a virtual sink, so the computer plays nothing locally (turn this off with `no_redirect`). The output is handed back when the receiver leaves and when the sender exits.

The wire format and handshake are specified in [docs/protocol.md](docs/protocol.md), and the threat model in [docs/security.md](docs/security.md).

## Usage

Run `meshwav` with no arguments for the window. It has a Receive / Send switch; the last choice is remembered and only one side runs at a time.

- **Send** starts and stops the sender service, shows the pairing code with a countdown, lists paired receivers (play here, rename, remove, choose automatically), and edits the default quality and the keep-playing-locally setting.
- **Receive** has a pairing screen for the code, shows Playing, Standby or Connecting with buffer and loss numbers, a quality choice (Opus or Lossless PCM), a volume slider, and a tray icon on desktops that support StatusNotifierItem.

The same things work from the command line (`man meshwav` lists everything):

```sh
meshwav send [--codec opus|lossless] [--kbps 160] [--frame-ms 10]   # the sender (normally run by the service)
meshwav pair                         # open a pairing window on the running sender
meshwav pair 123456                  # pair this computer as a receiver with a sender's code (asks to confirm the fingerprint)
meshwav devices                      # paired devices: online, active or standby
meshwav use "Phone name"             # send the audio there (name, name prefix or key prefix)
meshwav use auto                     # let the first connected device keep it
meshwav rename "Phone name" "New name"
meshwav remove "Phone name"         # forget a paired device; it must pair again
meshwav receive run [--device SINK] [--lossless]   # play the paired sender's stream
meshwav receive outputs              # list sound outputs
meshwav receive forget               # forget the paired sender
```

### Configuration

The sender reads `~/.config/meshwav/config.toml` (`%APPDATA%\meshwav\config.toml` on Windows; an example is installed at `/usr/share/doc/meshwav/config.toml.example`):

| Key | Meaning |
|---|---|
| `bind` | Address to listen on (default: this machine's tailnet IPv4, port 47900) |
| `codec` | `opus` or `lossless`: what is sent until a receiver asks for something else |
| `kbps` | Opus bitrate, 64 to 256 (default 160) |
| `frame_ms` | Opus frame size: 5, 10 or 20 (default 10) |
| `source` | Capture from this source instead of redirecting the default output (Linux) |
| `no_redirect` | Keep playing locally instead of redirecting (on Windows: instead of turning the output down) |
| `log_level` | `error`, `warn`, `info` or `debug` |

Command-line flags override the file. A typo or bad value stops the sender with the line number. Restart the service after editing the file. A receiver's own choice of codec always wins over the sender's default.

### Running the sender as a service

The package installs a systemd user unit named `meshwav-agent` (it runs `meshwav send`). It is a user service because it needs your audio session.

```sh
systemctl --user status meshwav-agent
journalctl --user -u meshwav-agent -f
loginctl enable-linger "$USER"        # optional: start at boot without logging in
```

If the tailnet is not up yet, the sender exits and systemd retries every 5 seconds. From a source build, `scripts/install-user-service.sh` installs the binary and the unit for your user.

## Android

The app is in [`apps/android`](apps/android) (Kotlin, version 0.9.0). It has a Receive / Send switch and remembers the last mode.

- **Receive.** Open a pairing window on the computer, then type the six-digit code on the phone and tap Pair. The app remembers every sender it has paired with: connect, make default or forget them under "Paired senders". With "Reconnect automatically" on (the default) it connects to the default sender when the app opens. A `meshwav://pair` link still works under "Use a pairing link instead". An opt-in "Low latency" checkbox lowers the receiver's jitter-buffer floor from 20 ms to 10 ms; it can crackle on a weak or relayed link, so it is off by default. An opt-in, experimental "Native audio output (AAudio)" checkbox plays through the phone's native audio API instead of AudioTrack; if it cannot open, the app falls back to AudioTrack and says why. It was tested on one phone only.
- **Send.** Tap Start, allow Android's screen-capture prompt (only audio is taken), choose Lossless or Opus, then type the shown code on the computer (`meshwav pair 123456`). While sending, media volume on the phone is held at 0 and restored afterwards; some phones pause playback at 0. Lossless here is 48 kHz 16-bit audio as captured after Android's mixer, so it is not bit-perfect, and apps that block capture may be silent.
- **Signing.** From 0.9.0 the APK is signed with the Tech House release key. Earlier APKs were debug-signed. The new key is chained to the old debug key (Android key rotation, Android 9 and later), so a phone with an earlier build updates in place and keeps its pairings (tried on one phone, going from the debug-signed 0.8.0 to 0.9.0). If Android ever refuses an update, uninstall once and pair again.

### Verifying the Android APK

Each release lists `SHA256SUMS-android.txt` next to the APK. The APK itself carries the signing certificate; check that it is ours with `apksigner` from the Android SDK build-tools:

```
apksigner verify --print-certs meshwav-0.9.0.apk
```

The signer's certificate SHA-256 digest must be:

```
90:25:CB:D4:13:C3:F8:A9:D1:4F:9E:9A:61:36:C1:CF:4A:B0:4E:A1:35:50:51:60:0A:47:57:08:9A:FD:0E:82
```

(`9025cbd4…0e82` without the colons.) If it differs, do not install the file. The signing key is not in this repository.

## Performance

Measured numbers and their limits are in [docs/measurements.md](docs/measurements.md). In short:

- **Network.** Over a direct tailnet path the round trip is about 2 to 6 ms at the median. Tail latency and loss on a Wi-Fi leg depend on the link.
- **End to end (computer to phone).** The phone's stats panel shows `end to end = network + buffer + output`, computed from capture stamps the sender puts on each packet, with no microphone. With the default AudioTrack output the output stage swung between 41 and 139 ms from one connect to the next, so the target is **not reliably met**. With the optional native AAudio output the output stage was about 17 ms and steady, and end to end read 27 to 36 ms on one Galaxy phone, on USB audio and the phone speaker. That figure leaves out sender capture (about 10 to 21 ms, not measured) and rests on Android's own output estimate. The target is a median of 60 ms or less. The older click-loopback test (about 91 ms for 5 ms PCM and about 115 ms for 10 ms Opus) included the microphone's own delay and is an upper bound. Details: [M4f and M4g](docs/measurements.md).
- **Not measured.** Audio quality by ear, PipeWire-specific behaviour and battery use.

## Build from source

Requirements: Rust (stable), `cmake`, `pkg-config`, `libopus-dev`, `parec` / `pactl` (PulseAudio or PipeWire-pulse), and Tailscale on both devices.

```sh
cargo build --release -p meshwav
./target/release/meshwav                     # the window
./target/release/meshwav send --pair         # or run the sender directly
```

Packages are built from the release binary with `packaging/build-deb.sh`, `packaging/build-rpm.sh` (needs `rpmbuild`) and `packaging/PKGBUILD`. Tagging `vX.Y.Z` runs the release workflow, which builds the deb, rpm and tarballs for x86_64 and aarch64. Release binaries are built on Ubuntu 22.04 and need glibc 2.35 or newer.

The Android app needs the native library from `crates/android-spike` (built with `cargo ndk`), then `./gradlew assembleDebug` in `apps/android`.

## Testing

```sh
cargo test --workspace
```

This includes end-to-end tests that start the real sender and receive from it, and protocol tests against vectors shared with the Kotlin app (`docs/vectors`). The Android unit tests run with `./gradlew testDebugUnitTest`.

## Repository layout

| Path | What it is |
|---|---|
| `crates/meshwav` | The single `meshwav` binary: window, sender and receiver |
| `crates/desktop-agent` | The sender (library): capture, encode, send, receiver selection, control socket |
| `crates/linux-receiver` | The Linux receiver (library): handshake, jitter buffer, decode, playback |
| `crates/linux-receiver-gui` | The window (egui) and tray icon |
| `crates/audio-protocol` | Wire format, Noise handshake, crypto and replay protection (no I/O) |
| `crates/audio-codec` | Opus and PCM codecs |
| `crates/audio-capture` | Audio capture |
| `crates/audio-transport` | UDP transport, Tailscale peer list, tailnet-only address checks |
| `crates/android-spike` | The Android sending path as a native library loaded over JNI |
| `apps/android` | The Android app |
| `packaging/` | deb, rpm and Arch packaging, the systemd unit, the man page |
| `docs/` | Architecture, protocol, security, risks, measurements, milestones |

## Documentation

- [Architecture](docs/architecture.md): components, stack, targets
- [Protocol](docs/protocol.md): wire protocol and pairing discovery
- [Security](docs/security.md): threat model and assumptions
- [Risks](docs/risks.md): technical risks and mitigations
- [Measurements](docs/measurements.md): measured numbers and their limits
- [Milestones](docs/milestones.md): what is done and what is open

## Roadmap

Open work, roughly in order: confirm the native Android output on more phones and over longer runs, then make it the default; measure Windows latency; code-sign the Windows build; and a hosted apt/dnf repository or an AUR package. See [docs/milestones.md](docs/milestones.md).

## Contributing

Issues and pull requests are welcome; please open an issue first for anything large. Work happens on a feature branch with conventional commit messages. `cargo test --workspace` should pass, and user-facing changes should update this README.

## Security

Please report vulnerabilities privately through GitHub's private vulnerability reporting; see [SECURITY.md](SECURITY.md). The handshake and pairing code has not been independently audited.

## License

[Apache-2.0](LICENSE). Part of the Mesh family (with Mesh Pilot), by [Tech House BLR](https://github.com/tech-house-blr).
