# Milestones

Each milestone ends with working, tested, measured output. No placeholders presented as done.

- M0: Architecture docs (this commit). Exit: approved by Boss.
- M1: Spike and protocol. Cargo workspace; `audio-protocol` with serialization, AEAD, replay window, test vectors + tests; Opus/FLAC/PCM round-trip tests; UDP + TCP echo and RTT/loss measurement over real Tailscale (Pi to phone/PC). Exit: numbers recorded in docs/measurements.md.
- M2: Desktop prototype. `audio-capture` (PipeWire on the Pi/Linux, WASAPI on Windows), Opus encode, send to a configured tailnet IP; verify with a Rust test receiver that plays audio. Exit: audible and glitch-free for 30 min.
- M3: Android prototype. Kotlin receiver: UDP, Opus decode, AudioTrack; manual IP; foreground service. Exit: real end-to-end playback on a phone; latency measured with a loopback click test.
- M4: Integration. Noise handshake, pairing, control channel, jitter buffer + PLC, reconnection, stats/diagnostics, bitrate/frame config, then lossless profile (PCM, FLAC) with format negotiation and bit-perfect detection. Exit: targets in architecture.md measured; loss-injection tests pass.
- M5: Productization. Compose UI polish, discovery via Tailscale peer list, systemd/Windows services, config + logging, install docs, release builds.
- Later (not before M4 stable): per-app capture, multi-receiver, adaptive bitrate, remote volume, bidirectional audio, macOS.
