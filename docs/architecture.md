# Architecture

## 1. Goals and priorities
1. Minimal end-to-end latency (live monitoring grade).
2. Fidelity up to high-resolution lossless (up to 24-bit / 192 kHz where the receiver chain supports it).
3. Reliability, bounded memory, security by default.

Two operating profiles share one pipeline; only the codec and buffer sizes differ:

| Profile | Codec | Typical use | Bandwidth |
|---|---|---|---|
| Low-latency (default) | Opus 48k stereo, 10 ms, 64-256 kbps (default 160) | Wireless/relayed, video, gaming | 0.2 Mbps |
| Lossless | FLAC (small blocks) or raw PCM, source rate/depth | Wired DAC/IEMs, direct path | PCM 48k/16: 1.5 Mbps; 96k/24: 4.6; 192k/24: 9.2 (FLAC about 50-65% of that) |

Rule: no resampling or processing on the sender. The stream carries the capture device's native mix format; the
receiver decides how to render it. Opus requires 48 kHz, so Opus mode resamples only when the source is not 48 kHz.

## 2. Components
```mermaid
flowchart LR
  subgraph Desktop[Desktop agent (Rust)]
    CAP[audio-capture\nWASAPI loopback / PipeWire] --> RB[(SPSC ring buffer)]
    RB --> ENC[audio-codec\nOpus / FLAC / PCM]
    ENC --> PKT[audio-protocol\nframing + AEAD]
    PKT --> TX[audio-transport\nUDP send]
    CTL[control server\nTCP/TLS-PSK on tailnet IP] --- PKT
    DISC[discovery\ntailscale status + probe] --- CTL
  end
  subgraph Tailnet[Tailscale WireGuard tunnel]
  end
  subgraph Android[Android receiver (Kotlin)]
    RX[UDP receive] --> JB[Jitter buffer\nadaptive + PLC]
    JB --> DEC[Decoder\nlibopus / FLAC / PCM]
    DEC --> PLAY[AudioTrack\nlow-latency, foreground service]
    UI[Compose UI + diagnostics] --- CC[Control client]
  end
  TX -- UDP --> Tailnet --> RX
  CTL <-- TCP --> Tailnet <--> CC
```

### Crates (Rust workspace)
- audio-protocol: packet/control message types, (de)serialization, AEAD framing. No I/O. Heavily unit/fuzz tested.
- audio-capture: `CaptureSource` trait + backends (windows-wasapi, linux-pipewire, macOS later). Real-time callback only copies into a lock-free ring buffer.
- audio-codec: `Encoder`/`Decoder` traits; Opus (libopus bindings), FLAC, PCM.
- audio-transport: tokio UDP sender, pacing, bounded send queue, stats.
- discovery: there is no separate crate. `audio-transport` reads online peers from `tailscale status`, and pairing probes them (see protocol.md, "Pairing discovery"). Identity is only ever established by key pinning.
- desktop-agent: library behind `meshwav send`; capture, encode, send, receiver selection, redirect, config (flat TOML), control socket, logging. Runs as a systemd user service. (A Windows service is not built.)
- linux-receiver: library behind `meshwav receive`; handshake, jitter buffer, concealment, Opus and PCM decode, playback through `pacat`.
- linux-receiver-gui: the window (egui) with the Receive / Send switch, and the tray icon.
- meshwav: the single binary that dispatches to the window, the sender and the receiver. `meshwav-agent` and `meshwav-receiver` are thin compatibility binaries.
- android-spike: the sending path (pairing, handshake, Opus or PCM, sealed UDP) as a library loaded by the Android app through JNI for its Send mode.

Android (apps/android): Kotlin + Jetpack Compose, modules `net`, `codec` (JNI libopus, FLAC decoder), `jitter`, `playback`, `ui`. The receive path is Kotlin and does not use the Rust protocol crate: the protocol is specified in docs and tested with shared test vectors (docs/vectors/), avoiding a cross-language FFI in the audio path. Only the Send mode loads the Rust code (`android-spike`) through JNI.

## 3. Threading and backpressure
- Capture callback (OS RT thread): copy to SPSC ring (fixed capacity, ~100 ms). Never allocates, locks or does I/O. On overflow: drop oldest, increment counter.
- Encode thread: pulls frames, encodes, hands packets to the send queue (bounded, ~3 frames). Queue full: drop oldest packet (latency over completeness; stale audio is worthless).
- Receiver: network thread to jitter buffer (bounded, hard cap e.g. 200 ms). Playback thread (AudioTrack write loop, high priority, or callback with Oboe) never blocks on the network; on underrun it plays PLC/silence.
- Network congestion never grows memory: all queues are bounded with drop-oldest and counters surfaced in diagnostics.

## 4. Latency budget (direct path, Opus 10 ms)
capture buffer 10 + encode 10 (frame) + network 2-10 + jitter buffer 20 + decode <1 + AudioTrack low-latency 10-20 = about 55-70 ms.
Lossless adds no codec delay for PCM and 5-10 ms for FLAC blocks, but needs a larger jitter buffer on weak links.
Targets (not met yet: the click-loopback test measured p50 91.5 ms for PCM and 114.9 ms for Opus to the phone, see measurements.md): p50 <= 60 ms, p95 <= 80 ms direct; <= 120 ms on DERP relay. Wired USB DAC assumed; Bluetooth output adds 100-200 ms and is outside the target (UI warns).
Reliability: < 1 audible glitch/hour at <= 0.5% loss; PLC concealment at up to 5% loss; reconnect within 3 s of a network blip.
Resources: agent < 3% of one core (Opus), < 5% (lossless); Android < 5% CPU, < 50 MB RSS.

## 5. Technology stack
Desktop (Rust):
- tokio (async net), cpal (WASAPI loopback) or wasapi crate for finer control, pipewire-rs (Linux), audiopus/opus-sys (libopus), flac encoder (libFLAC bindings or flacenc), ringbuf (lock-free SPSC), chacha20poly1305, ed25519-dalek, rustls or a Noise handshake (snow), serde+postcard not used on the wire (hand-defined layout), tracing, clap, toml.
Android:
- Kotlin, Compose, coroutines, AudioTrack (PERFORMANCE_MODE_LOW_LATENCY; ENCODING_PCM_FLOAT, 24/32-bit packed on API 31+), JNI libopus, FLAC decoder (libFLAC via NDK or a Kotlin port), Foreground service (mediaPlayback type), WifiLock/WakeLock, Tailscale app provides the VPN (we never use VpnService; Android allows one VPN).
- Oboe/AAudio is a later option if AudioTrack latency measurements miss targets.

Library comparison (decisions)
- Capture on Windows: cpal vs wasapi crate: use `wasapi` for explicit event-driven loopback and format control; cpal as fallback.
- Linux: PipeWire native (monitor of default sink) rather than PulseAudio compat: lower latency and exact format.
- Control security: Noise (snow) with pinned static keys beats full TLS/PKI: no CA, no certificates, small.
- Android playback: AudioTrack first (simple, supports float/24-bit), Oboe if needed.

## 6. High-res lossless on Android: honest limits
- Android's mixer resamples to the output rate (usually 48 kHz) unless the route supports a matching rate. Bit-perfect needs USB audio direct output or the Android 14+ bit-perfect mixer for USB DACs. The app reads `AudioDeviceInfo`/`AudioTrack` actual sample rate and shows "bit-perfect: yes/no/unknown" in diagnostics. We never claim bit-perfect without detecting it.
- Source rate comes from the OS mix format (Windows shared-mode loopback = device format; PipeWire = graph rate). Exclusive-mode capture is not possible for loopback; so true source-native high-res depends on the OS mixer being set to that rate.
- Format changes mid-stream (e.g. mixer rate change) trigger a StreamConfig renegotiation; the receiver rebuilds the AudioTrack with a short mute.

## 7. Repo layout
See README. Cargo workspace at root (`crates/`; packaging in `packaging/`); Android Gradle project in apps/android; docs/vectors for cross-language test vectors; scripts/ for latency and loss measurement tools.
