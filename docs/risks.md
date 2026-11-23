# Technical risks and mitigations

| # | Risk | Impact | Mitigation |
|---|---|---|---|
| 1 | Bluetooth headphones on the phone add 100-200 ms | Misses latency targets | Detect route, warn in UI; wired/USB-C path is the reference. |
| 2 | Tailscale DERP relay adds latency/jitter | Glitches or latency > target | Show path type (direct/relay); larger adaptive buffer; lossless auto-suggests Opus on relay. |
| 3 | Lossless bandwidth vs 1280 MTU and weak links | Loss means dropouts (no PLC for PCM) | Fragmentation, optional parity, FLAC to cut rate, profile fallback to Opus. |
| 4 | Android mixer resamples / not bit-perfect | High-res claim false | Detect actual rate/route, display bit-perfect status honestly; USB DAC + Android 14+ for true bit-perfect. |
| 5 | Loopback format is the OS mix format | Cannot capture exclusive-mode/high-res source independent of OS setting | Document; read and follow the mix format, renegotiate on change. |
| 6 | Clock drift between PC and phone | Slow buffer growth/underrun | Drift estimator + bounded, tiny rate adjust. |
| 7 | Android doze / background kill | Playback stops | Foreground service (mediaPlayback), wake/wifi locks, battery exemption guidance. |
| 8 | Android allows one VPN | Cannot embed our own tunnel | Rely on the Tailscale app; no VpnService. |
| 9 | PipeWire/WASAPI differences | Platform bugs | `CaptureSource` trait with conformance tests; per-OS CI. |
| 10 | Opus JNI / FLAC native build complexity | Build friction | Prebuilt NDK static libs built via script in scripts/; pinned versions. |
| 11 | AudioTrack latency higher than expected | Target missed | Measure early (M3); switch to Oboe/AAudio if needed. |
| 12 | Discovery on tailnet (no mDNS) | Weaker UX | Done for pairing: `tailscale status` lists peers, a probe or invite finds the sender showing a code. The pairing link still works as a fallback. |
| 13 | Noise/AEAD implementation errors | Security | Use audited crates (snow, RustCrypto); test vectors; no custom crypto. |
| 14 | Power use of constant 10 ms packets | Phone battery | Measure; allow 20 ms frames; Wi-Fi low-latency lock. |
