# Measurements

## M1: UDP path over Tailscale (netprobe)

Setup: sender/echo client on the Pi (`atlas`, Wi-Fi 2.4/5 GHz link at -43 dBm, Wi-Fi power save **on**), echo server on `liberation`
(x86_64, direct tailnet path 192.168.0.x, ~2 ms `tailscale ping`). Real Meshwav packets (AEAD, 20-byte header).
Tool: `cargo run --release -p audio-transport --bin netprobe`. Round-trip time (RTT) is measured; one-way is about RTT/2.

| Scenario | Wire size / interval | Offered | Loss (fwd+ret) | RTT p50 | p95 | p99 | max |
|---|---|---|---|---|---|---|---|
| Opus-like, 60 s | 236 B / 10 ms | 0.19 Mbit/s | 0.77% | 2.4 ms | 9.7 ms | 27 ms | 405 ms |
| FLAC 96k/24-like, 20 s | 636 B / 2 ms | 2.5 Mbit/s | 0.21% | 4.3 ms | 99 ms | 172 ms | 334 ms |
| PCM 96k/24-like, 20 s | 1200 B / 2 ms | 4.8 Mbit/s | 0.24% | 3.9 ms | 63 ms | 117 ms | 132 ms |
| PCM 192k/24-like, 20 s | 1200 B / 1 ms | 9.6 Mbit/s | 0.38% | 6.3 ms | 173 ms | 346 ms | 412 ms |

Findings:
- Median path latency is small (2-6 ms RTT), so the latency target is achievable on a direct path.
- Tail latency and loss are dominated by the Pi's Wi-Fi hop, not Tailscale or the crypto/protocol layer (0 unauthenticated drops, 0 sender-late events,
  0 reordering except 1 packet). Likely cause: Wi-Fi power save (hypothesis, not yet verified by toggling it).
- Consequence for design: a 20 ms jitter buffer is not enough on a weak Wi-Fi leg (p99 27 ms at the lightest load). Keep the buffer adaptive with a
  40-60 ms upper default for lossless, and report p99 in diagnostics. Lossless at 9.6 Mbit/s over Wi-Fi shows severe tails: the profile
  needs a wired/good-link warning and automatic fallback to Opus.
- The Pi is not the final receiver. The meaningful numbers (PC to phone over Wi-Fi) come in M3.

## M1: Codec results (synthetic tones + noise, so real music will differ)

- Opus 48 kHz stereo: SNR 33-45 dB at 64-256 kbps for 5-20 ms frames; actual bitrate within 2-4% of target; packets always < 1164 B.
  2.5 ms frames work but are noticeably worse at 64 kbps (18.5 dB).
- FLAC (stateless frames, bare frames without STREAMINFO): bit-exact at 44.1/48/96/192 kHz, 16/24-bit. Size on the synthetic signal:
  39% (48k/16), 59% (48k/24), 52% (96k/24), 44% (192k/24) of raw PCM. Each frame decodes independently; corruption is detected by the frame CRC.
- flacenc caps sample rate at 96 kHz; we label frames with 96 kHz for higher rates (does not affect coding; verified bit-exact at 192 kHz).
- PCM s16/s24/f32 round-trip exactly.

## M4b: pairing and handshake (verified, loopback only)

- Rust: 13 handshake/server tests (vectors, pairing, reconnect, wrong code, lockout, expiry, pin mismatch, retransmits, pending cap, rate limit, sanitising) and 3 end-to-end tests that run the real agent binary with a fake `parec`.
- Kotlin: 10 JVM tests (RFC 7748 X25519 vectors, XX and XXpsk3 byte-identical to the Rust vectors, pin mismatch, tampering, framing).
- Interop: the Kotlin `Handshake.run` (JVM, on liberation) paired with the real Rust agent over 127.0.0.1, received gap-free audio plus keepalives, reconnected with the pinned key only, was refused with a wrong pin, an unpaired key got no audio, and the used code got no reply.
- Not measured: handshake time on the phone (BigInteger X25519 is slower than native), behaviour over a real tailnet path, and the phone UI flow (a hands-on check is pending).

## M4b: runtime config (verified, loopback only)

- Rust: 6 agent unit tests for the request policy (range checks, replay of a repeated request id, supersede, encoder failure), and 2 more end-to-end tests with the real agent binary and a fake `parec`: 10 to 20 ms frames and 96 kbit/s, a repeated request replayed not re-applied, out-of-range bitrate and frame size refused with no change, bitrate-only and 5 ms switches with `0` meaning keep, sequence numbers gap-free and growing across every switch, a stranger's request gets no reply at all.
- Kotlin: 10 tests (wire bytes identical to the Rust literals, strict decoding, retry, give-up, refusal, stale reply, new session) and 3 jitter buffer tests at 5, 10 and 20 ms (clean link: no concealment; 3% loss: concealed frames within 2-6% of the frames sent; depth never above 200 ms).
- Interop: the Kotlin code, run on liberation, switched the real Rust agent to 96 kbit/s and 20 ms frames, saw them announced in the audio flags, and got a refusal for 999 kbit/s.
- Not measured: how smooth the switch sounds on the phone (the jitter buffer is flushed on a frame-size change), and whether 5 ms frames hold up over Wi-Fi (more than 200 packets per second). Latency at each frame size is M4c work.

## Pre-public check (2026-11-23, main at f849aa9)

- `cargo test --workspace` on the Pi (aarch64): 76 passed, 0 failed. Per target: audio-codec 10, audio-protocol handshake 13 and protocol 22, audio-transport 1, meshwav-agent 22 unit and 8 end-to-end (real binary with a fake `parec`).
- GitHub Actions (ubuntu-latest, `cargo test --workspace`) passed on PR #7 in 1m20s.
- Tracked files grepped for keys and secrets: none found (tailnet range literals in docs and tests only). All commits are authored by one person.
- The agent built from this commit started on liberation and an already-paired phone (SM-S938B) reconnected with its pinned key and redirect took over audio, observed in the agent log.
- Not measured: end-to-end PC-to-phone latency, the phone's pairing UI flow from a fresh install, and behaviour on Windows.


## M4: end-to-end latency, click loopback (2026-11-23)

Method: `scripts/latency/click_test.sh` plays 50 noise-burst clicks (5 ms, every 1.2 s) into a PulseAudio null sink on `liberation`. The agent captured that sink (`--no-redirect --source mwtest.monitor`) and streamed to the phone (`galaxy`, SM-S938B) over Tailscale. The phone played through its own speaker, next to liberation's Maono USB microphone. One ffmpeg process recorded the sink's monitor (reference) and the microphone; `scripts/latency/analyze.py` takes mic onset minus reference onset per click.

| Profile | Clicks | p50 | p95 | min | max | sd |
|---|---|---|---|---|---|---|
| Lossless PCM, 5 ms packets (~1.5 Mbit/s) | 50/50 | 91.5 ms | 92.1 ms | 90.9 ms | 92.2 ms | 0.4 ms |
| Opus 10 ms, 160 kbps | 50/50 | 114.9 ms | 115.5 ms | 114.1 ms | 115.6 ms | 0.4 ms |

Target in docs/architecture.md (Opus 10 ms, direct path): p50 <= 60 ms, p95 <= 80 ms. **Not met**: Opus is about 55 ms over, and PCM was lower than Opus.

Caveats:
- The figure includes the microphone's USB capture latency, acoustic travel, and the phone speaker path, none of which were calibrated (no third device). The offset between the two ffmpeg inputs was not verified. Treat as an upper bound by an unknown few to a few tens of ms.
- One session, one Wi-Fi condition, phone speaker output (not IEMs, not Bluetooth), 50 clicks per profile.
- The very small spread (sd 0.4 ms) suggests a fixed buffering delay, not network jitter. Which stage holds the extra delay (receiver jitter buffer depth, MediaCodec Opus decode, AudioTrack buffer) is not yet known; why Opus is slower than PCM is also unexplained. Both are open hypotheses.

## M4d: where the receiver's delay is, phone-side readouts (2026-11-24)

Method: the Android app now reports, in its stats panel, the decode time per frame, the AudioTrack buffer, and an "output queue" (frames handed to AudioTrack minus the output position from `AudioTrack.getTimestamp()`, corrected for the age of that position). Read by eye from screenshots on the Galaxy (SM-S938B) streaming from `liberation`; each value is one smoothed snapshot (about +-5 ms).

| Setting | Output queue | Decode | AudioTrack underruns |
|---|---|---|---|
| PCM, USB audio, 120 ms buffer | 59.8 ms | 0 | 0 |
| Opus 10 ms, USB audio, 120 ms buffer | 51.0 ms | 1.2 ms | 0 |
| PCM, USB audio, 40 ms buffer | 63.8 ms | 0 | 8 |
| Opus 10 ms, USB audio, 40 ms buffer | 62.6 ms | 0.6 ms | 8 |
| Opus 10 ms, phone speaker, 40 ms buffer | 63.3 ms | 1.2 ms | 0 |
| Opus 10 ms, phone speaker, 120 ms buffer, low-latency mode granted | 54.0 ms | 0.5 ms | 0 |
| PCM, phone speaker, 120 ms buffer, low-latency mode granted | 58.9 ms | 0 | 0 |

Other readings: `tailscale ping` to the phone about 5 ms round trip on a direct path. Capture on `liberation` could not be read (PipeWire reports 0 for the capture stream); PipeWire's default block there is 1024 samples (about 21 ms), and `parec` asks for 10 ms.

What this shows:
- Opus decoding takes about 1 ms or less, so it does not explain Opus being about 23 ms slower than PCM in M4.
- The output queue stayed at roughly 50 to 64 ms across a 40 ms and a 120 ms AudioTrack buffer, USB and speaker output, Opus and PCM, with the low-latency path requested and granted. Shrinking the buffer added underruns on USB and lowered nothing.

Caveats:
- A figure that does not move with any setting may be a limit of `getTimestamp()` or a fixed cost in the phone's audio path; these were not separated, so it is not shown that 55 to 60 ms of real delay sits there.
- One phone, one network, single snapshots, read from screenshots. No acoustic measurement was repeated.
- The phone's own audio path past AudioTrack cannot be split further from inside the app.

## M4e: opt-in low-latency receiver mode, phone-side readouts (2026-11-24)

Method as M4d: Galaxy (SM-S938B), USB audio output, sender on `liberation` over a direct tailnet path, one stats-panel snapshot per row (about +-5 ms). The "Low latency" checkbox lowers the jitter-buffer floor from 20 ms to 10 ms; the buffer still grows with measured jitter and the 200 ms cap is unchanged.

| Mode | Setting | Jitter target | Output queue | Lost / late / receive underruns | AudioTrack underruns |
|---|---|---|---|---|---|
| PCM | off (M4d, speaker) | 40 ms | 58.9 ms | 0 / 0 / 0 | 0 |
| PCM | on | 30 ms | 46.6 ms | 0 / 0 / 0 | 0 |
| Opus 10 ms | off (M4d, speaker) | 40 ms | 54.0 ms | 0 / 0 / 0 | 0 |
| Opus 10 ms | on | 20 ms | 38.7 ms | 0 / 0 / 0 | 5 |

Caveats:
- The earlier rows were on the phone speaker and these on USB audio, and the jitter target adapts, so the comparison is indicative only. The drop in output queue was not predicted; the cause was not isolated.
- The end-to-end total was not re-measured, so no new latency figure is claimed.
- Boss reported no audible clicks on this phone. Opus showed 5 AudioTrack underruns in the snapshot; their cause was not investigated.
- One phone, one direct link. A weak, congested or relayed link was not tested and is expected to be worse with the smaller buffer.

## M4f: end-to-end readout from the sender's capture stamps (2026-11-24)

Method: the sender stamps each audio packet with its capture time (a `TimeRef` control message maps the packet's sample clock to that time). The receiver estimates the clock offset with NTP-style `Ping`/`Pong` exchanges, keeps the shortest round trip, and shows `end to end = network + buffer + output` with the clock error bound. No microphone is involved. Galaxy (SM-S938B), sender on `liberation`, direct tailnet path, Opus 10 ms, low-latency mode granted, 0 packets lost, one stats-panel snapshot per connect, a few connects per row (sample sizes are small and were not tallied).

| Output | Network + buffer | Output stage | End to end |
|---|---|---|---|
| AudioTrack, USB audio | 2 to 5 ms (two runs had 24 and 44 ms of buffer) | 41 to 73 ms, two-valued by connect | 42 to 113 ms |
| AudioTrack, phone speaker | about 1 to 5 ms | 41 to 139 ms | 46 to 143 ms |

Findings and limits:
- Network plus jitter buffer cost about 2 to 5 ms in the normal case. The output stage is almost the whole delay and it changes from one connect to the next.
- Capping the AudioTrack buffer (40 ms) and trimming the queue did not change the output stage, so the extra delay is inside Android's output path, not our buffer. Both experiments were reverted.
- The figure excludes capture on the sender (PipeWire quantum, about 10 to 21 ms, not measured).
- The output stage is Android's own timestamp estimate, not a measurement of sound at the speaker.
- The clock error shown on the panel was 1.5 to 2.1 ms.
- Readouts from the phone speaker and USB are from one phone; the spread was not explained.

## M4g: optional native AAudio output (2026-11-24)

Method: a standalone probe wrote silence to an AAudio stream and read AAudio's own timestamp; then the real player was run with the "Native audio output" checkbox on. Same phone, sender and link as M4f.

Probe (silence, not a real stream; speaker and USB audio gave the same values within 0.3 ms):

| Mode | Buffer held | Write-to-output delay |
|---|---|---|
| Exclusive, 2 bursts | 4.0 ms | 7.3 to 7.4 ms |
| Shared, 2 bursts | 4.0 ms | 13.0 to 13.2 ms |
| Shared, 8 bursts | 16 ms | 25.3 ms |

Real stream, AAudio shared, Opus 10 ms, 8 ms buffer, 0 packets lost, 0 output underruns:

| Output route | Snapshots | Output stage | Buffer stage | End to end |
|---|---|---|---|---|
| USB audio | 3 | 16.5 to 16.7 ms | 8 to 16 ms | 27 to 36 ms |
| Phone speaker | 2 | 16.5 to 16.7 ms | 15 to 16 ms | 32 and 34 ms |

Findings and limits:
- The output stage is about 17 ms and steady, against 41 to 139 ms on AudioTrack.
- With the sender's capture (about 10 to 21 ms, not measured) the total is roughly 40 to 57 ms. This rests on Android's output estimate, so it is not a confirmed acoustic figure.
- The buffer stage is higher than on AudioTrack (8 to 16 ms against about 1 ms). The AAudio write blocks until the output has room, so that waiting is probably counted there rather than in the output queue. This reading of the code was not confirmed.
- The "trimmed" counter rose from 14 to 18 between two speaker snapshots with no loss and 1.3 to 1.5 ms jitter. The cause was not investigated.
- Audio was reported clean on the phone speaker and on USB audio; no crackle was reported. Not measured: other phones, Bluetooth, long runs, a weak or relayed link, and exclusive mode (the player uses shared mode).
