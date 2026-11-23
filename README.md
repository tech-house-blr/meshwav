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

## Build and run (Linux agent)

Requirements: Rust (stable), `parec`/`pactl` (PulseAudio or PipeWire-pulse), Tailscale running on both devices.

```sh
cargo build --release -p meshwav-agent
./target/release/meshwav-agent --pair
```

Common flags: `--bind IP:PORT`, `--kbps 160`, `--frame-ms 10`, `--source SINK.monitor`, `--no-redirect`, `--identity PATH`, `--authorized PATH`.
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
