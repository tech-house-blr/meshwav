# Protocol v1

Two channels between agent (A) and receiver (R). R initiates. A listens only on its tailnet IP(s).
All integers are big-endian.

## 0. Handshake and session keys (implemented, M4b)
Deviations from the original design below, made for the first integration: the handshake runs over the same UDP socket (no TCP control channel yet) and uses `SHA256` instead of `BLAKE2s`, because Android has no BLAKE2s in its platform crypto and we avoid extra dependencies. Rust uses `snow`; Kotlin implements the initiator by hand (X25519 on BigInteger, HMAC-SHA256 HKDF, ChaCha20-Poly1305 from javax.crypto). Both are checked byte-for-byte against `docs/vectors/noise_v1.txt`.

- Patterns: `Noise_XX_25519_ChaChaPoly_SHA256` (reconnect) and `Noise_XXpsk3_25519_ChaChaPoly_SHA256` (pairing). Prologue `meshwav-v1`. The receiver is always the initiator.
- PSK = SHA-256("meshwav-pair-v1" | agent static public key | 6-digit code). The pairing line carries the agent's public key, so the phone pins it before connecting and an impostor agent is rejected before the phone sends its own identity.
- Datagram: `"MW" | 0x01 | kind | sid:u32 | step:u8 | noise message`. kind 0x10 = XX, 0x11 = XXpsk3 (audio packet types are 1..3, so they never collide). Step 1 and 3 go to the agent, step 2 is the reply. Message 3 carries the device name (max 32 chars) as payload.
- Loss handling: the receiver retransmits message 1 (the agent replays its stored reply, no new work) and sends message 3 three times (the agent ignores duplicates).
- Session keys are the two raw Noise split outputs: initiator-to-responder protects receiver-to-agent packets, the other protects agent-to-receiver. Keys are fresh per handshake, sequence numbers restart at 0.
- Agent keepalive: an empty sealed `Punch` on stream id 2 (own nonce space, so audio `seq` stays gap-free) right after the handshake and then every second. It confirms pairing to the phone and tells it the agent is alive even when nothing is playing. The receiver re-handshakes after 4 s without any authentic packet.
- Agent policy: XX only for authorized keys; XXpsk3 only while a pairing window is open (2 min, single use, 5 failed codes then 60 s lockout, no reply while closed or locked). At most 16 pending handshakes, 5 s TTL, 30 msg1 per second.
- Control packets: packet type 4 (`Control`), sealed on stream id 2 in both directions (own nonce and sequence space, so audio `seq` stays gap-free). The body is one control message in the framing of section 1 (`u16 length | u8 type | body`).
- Runtime config (implemented): `SetConfig{req:u16, kbps:u32, frame_ms:u8, codec:u8}` (type 11, receiver to agent; 0 means keep the current value; codec 1 = Opus, 2 = lossless PCM), answered by `ConfigApplied{req, kbps, frame_ms, codec}` (12) or `ConfigRejected{req, reason}` (13; 1 bitrate, 2 frame size, 3 encoder, 4 codec). Accepted range: 64-256 kbit/s, frames of 5, 10 or 20 ms. The receiver repeats a request every 300 ms (at most 8 times) with the same `req`; the agent applies a request once, between two frames, and replays its stored answer for a repeated `req`. Values persist for the agent process, across sessions. After a reconnect the receiver asks again for what the user chose.
- Lossless (implemented, PCM only): `codec` 2 makes the agent send the capture untouched: 48 kHz, stereo, 16-bit little-endian PCM, **always in 5 ms packets of exactly 960 bytes**, flagged with bit 5 (`FLAG_PCM`) of the header `flags` and announcing 5 ms in bits 2-4. One packet is one whole frame, so lossless needs no fragmentation, no reassembly and no change to the jitter buffer; a lost packet is 5 ms of concealment. Sequence numbers stay gap-free across a switch. The Opus bitrate and frame size are remembered and come back with `codec` 1. Cost: about 1.6 Mbit/s of payload, 200 packets a second, about 1.7 Mbit/s on the wire. A receiver drops a packet whose flag and size disagree (PCM must be 960 bytes at 5 ms; an Opus packet is shorter than 960 bytes), and treats the 960-byte size as the codec marker. FLAC (codec 3) is not implemented: the agent answers `ConfigRejected` reason 4. The Android app watches loss, lateness and underruns while lossless is on and asks for Opus again if 2% or more of a 4 s window is bad. "Lossless" means lossless as far as the app: what Android then does with it (mixing, resampling, the route to the headset) is outside the protocol, and the app only reports what the AudioTrack says it opened.
- Receiver selection (implemented): several paired receivers may be connected at once, and exactly one is *active* and gets audio. The others stay on keepalives. The agent tells each receiver with `ActiveState{active:u8}` (type 14, agent to receiver, 1 = active, 0 = standby; any other value is invalid), sent on every change and repeated with each keepalive, so a lost packet only delays it. The active device is chosen locally on the PC (`meshwav devices | use <device|auto> | rename <device> <new name>`, over an owner-only Unix socket `~/.config/meshwav/control.sock`): the chosen device wins whenever it is online; otherwise the current one stays, so a newcomer never steals the stream; otherwise the longest-connected one. The choice is stored in `~/.config/meshwav/active_peer`. A standby receiver's `SetConfig` is ignored (the encoder is shared), and it can ask again once active. An older app ignores type 14 and keeps working. It then shows no standby state.
- Play here (implemented, no version bump): a standby receiver can ask for the audio with `PlayHere` (type 16, receiver to agent, no payload). The agent treats it exactly like `meshwav use <that device>`: the device becomes the chosen one and is stored in `active_peer`, so it keeps the stream until someone chooses otherwise. It is accepted only inside a session sealed with that receiver's key, so only a paired device can send it, and it is sent a few times because the transport is UDP. The agent repeats `PlayHereSupported` (type 17, agent to receiver, no payload) with each keepalive. A receiver offers the action only after it has seen that message, so an older agent, which ignores unknown types, is never asked. Both messages must have no payload.
- Remote volume (implemented, no version bump): `SetVolume` (type 18, one byte 0..=100) from a receiver asks the agent to forward it to the receiver that is playing; the agent forwards at most one per 30 ms per sender and only to a receiver that has reported a level. `VolumeLevel` (type 19, one byte 0..=100) is the playing receiver's own level: it sends it when it changes and about every 1.5 s, and the agent repeats the last one to every other receiver with its keepalive. A level from a receiver that is not playing is ignored. Values over 100 and wrong lengths are rejected. Both are sealed with the sender's session key, so only paired devices can use them. A receiver that cannot report a level never shows the control.
- End-to-end delay measurement (implemented, no version bump): a receiver sends `Ping{t_us:u64}` (type 6) on the control stream and the agent answers at once with `Pong{echo_us:u64, t_us:u64}` (type 7: the receiver's value echoed, then the agent's monotonic clock in microseconds). About twice a second the agent also sends `TimeRef{ts:u32, t_us:u64}` (type 15, agent to the active receiver): the audio frame whose header `timestamp` is `ts` was read from the capture pipe at agent time `t_us`. The receiver keeps the Ping/Pong sample with the shortest round trip, assumes the answer was made half way through it (so the offset is wrong by at most half the round trip), and adds `(ts - ts_ref) / 48000` to the reference to get each frame's capture time on its own clock; the 32-bit sample clock may wrap. Pure arithmetic in `crates/audio-protocol/src/clocksync.rs` (Kotlin port `ClockSync.kt`, same test vectors). The capture time is when the agent read the frame, so audio held by PipeWire before that is not included. An older agent ignores both messages (the app then shows no end-to-end figure), and an older app ignores type 15.
- Pairing discovery (implemented): `KIND_HELLO` 0x12 is an unauthenticated probe (step 1, empty body) that a sender answers (step 2) with its 32-byte static public key, only while its pairing window is open and not locked out. `KIND_INVITE` 0x13 (step 1, body = public key 32 bytes + port u16) is sent by a sender with an open window to a waiting phone on UDP port 47902. Frames are 9-byte header plus body, at most 512 bytes. With either, the user types only the 6-digit code; the code still feeds the XXpsk3 handshake. Ports: 47900 is the Linux sender, 47901 the phone as a sender, 47902 the phone's invite listener. Test vector for an invite: `4d570113 00000007 01` + key `09`x32 + port `bb1c` (47900), checked in Rust and Kotlin.
- Key fingerprint (implemented): because HELLO and INVITE are unauthenticated, a receiver that found a sender by code alone must have the person confirm the sender's key before it sends anything that depends on the code. The fingerprint is the first 6 bytes of `SHA-256("meshwav-fp-v1" || public key)` as upper-case hex in three groups of four, like `4F2A-91C0-7B3E`. Test vector: key `07`x32 gives `DBA9-8DD2-C3ED` (checked in Rust and Kotlin). A sender shows its fingerprint next to the code. Pairing from a link needs no confirmation, since the link carries the key and the receiver pins it. A scripted pairing can pass `--fingerprint`.
- Sender default codec (implemented): `codec` in the sender's config (`opus` or `lossless`, flag `--codec`) sets what is sent from the start; a receiver's `SetConfig` still changes it.
- Phone as a sender (implemented): the phone runs the same sender state machine (`audio-protocol` `Server`) through JNI on port 47901 and sends 48 kHz stereo s16le PCM in 5 ms packets (`FLAG_PCM`) or Opus. Its pairing window lasts 10 minutes.
- Frame size on the wire: bits 2-4 of the audio header `flags` carry `frame_ms / 5` (1, 2 or 4). The receiver follows what the packets say, so it never depends on a control message arriving. It switches its jitter buffer after 3 consecutive packets announcing the new size (stragglers of the old size cannot flap it) and keeps the same 20 ms floor (never below 2 frames) and 200 ms cap.
- Switching the frame size flushes the receiver's jitter buffer, which costs a short audible gap; changing only the bitrate does not.
- Agent files (0600): `~/.config/meshwav/identity` (private key, hex) and `authorized_peers` (`<public key> <name>` per line).

## 1. Control channel (TCP, default port 47800) (original design, not built)
The implemented control path is the sealed `Control` packets of section 0. This section and the `whois` allowlist in section 4 are the original design and are not implemented.

Encrypted and mutually authenticated with a Noise XX handshake (Noise_XX_25519_ChaChaPoly_BLAKE2s) using long-term static keys.
Each side pins the peer's static key after pairing (section 4). Messages after the handshake: `u16 length | u8 type | body`.

| Type | Name | Direction | Body |
|---|---|---|---|
| 1 | Hello | R to A | proto version, device name, capabilities (codecs, rates, max bit depth, bit-perfect hint) |
| 2 | Offer | A to R | agent name, source format (rate, channels, bit depth), codecs offered, bitrate range |
| 3 | Accept | R to A | chosen codec + params (rate, depth, frame_ms, bitrate), UDP port, requested latency mode |
| 4 | StreamKey | A to R | stream_id (u32), 32-byte session key derived from Noise handshake hash (HKDF), starting seq |
| 5 | Config | either | change bitrate/frame_ms/codec at runtime; applies at a given seq |
| 6 | Ping / 7 Pong | either | timestamps for RTT, clock offset estimate |
| 8 | Stats | R to A | loss, late, concealed, buffer ms, path type (direct/relay) |
| 9 | Bye | either | reason |
| 10 | Volume | R to A | (post-MVP) |

Keepalive: Ping every 1 s; 3 s of silence is a dead connection, so the receiver reconnects with exponential backoff (200 ms to 5 s) and the agent stops sending UDP after 3 s without a control keepalive.

## 2. Audio channel (UDP, receiver-chosen port)
Receiver sends an authenticated UDP "punch" packet (type 0x02, AEAD-sealed) so the agent learns the source address/port and NAT/relay mapping. The agent sends audio only to the address that proved key possession.

### Packet layout
```
 0       1       2       3
+-------+-------+-------+-------+
| magic 'M''W'  | ver=1 | type  |   type: 1=audio 2=punch 3=parity
+-------+-------+-------+-------+
|          stream_id (u32)      |
+-------+-------+-------+-------+
|          seq (u32)            |   per-packet counter, also AEAD nonce component
+-------+-------+-------+-------+
|          timestamp (u32)      |   in samples at stream rate of first sample in frame
+-------+-------+-------+-------+
| frag_idx(u8)|frag_cnt(u8)|flags(u16)|  flags: bit0 = keyframe/format-start, bit1 = silence (DTX)
+-------+-------+-------+-------+
| payload (AEAD ciphertext) ... | 16-byte Poly1305 tag
```
Header = 20 bytes, authenticated as AAD. Nonce (12 bytes) = direction(1) | 0(3) | seq(4) | stream_id(4). The key is per session; seq is a u32 and a sender MUST rekey (new control handshake) before it wraps (the reference sender refuses to send past 2^32-1). Replays and out-of-window seq are dropped (sliding window of 1024); the replay window is updated only after authentication.

### Sizing
Tailscale MTU is 1280. Max UDP payload kept <= 1200 bytes including header and tag, so max codec bytes per packet = 1164.
Frames larger than that (lossless at high rates) are split into frag_cnt fragments; a frame is usable only if all fragments arrive (or parity recovers one).
Opus at 160 kbps, 10 ms = about 200 bytes: one packet.

### Codecs (codec id in Accept)
- 1 Opus: 48 kHz, 2 ch, frame_ms in {2.5,5,10,20}, CBR/CVBR, 64-256 kbps. PLC via decoder on gap, optional in-band FEC (post-MVP).
- 2 PCM: s16/s24/f32 interleaved, any rate up to 192 kHz, frame_ms 2.5-10. Concealment: repeat last frame with short fade, then silence.
- 3 FLAC: stateless per-frame encode (each packet is a self-contained FLAC frame set with its own header), so a lost frame never breaks following ones. Block size matches frame_ms.
Negotiation picks the best codec both support for the selected profile; the receiver may pick PCM or FLAC only if the Offer's source format is within its declared capability.

### Loss and ordering
No retransmission. Optional XOR parity packet (type 3) per N frames for lossless mode (post-MVP, off by default). Receiver reorders within the jitter window.

## 3. Jitter buffer contract
Target depth configurable (default 20 ms Opus, 40 ms lossless). Adaptive: grows quickly on late packets, shrinks slowly. Clock drift handled by occasional single-sample-class adjustments (time-stretch only within +/-0.5%); never silently.

## 4. Pairing and identity
- First connect: agent shows a 6-digit pairing code (CLI/log/tray). R enters it; the code is mixed into the Noise handshake as a PSK (Noise XXpsk3 for pairing) so an observer or a rogue tailnet device cannot complete pairing without it. Pairing is rate limited (5 tries then 60 s lockout, code expires in 2 min).
- Afterwards both store each other's static public key (fingerprint shown in UI as 4 words/hex). Later connections use plain XX with pinned keys; unknown key means reject.
- Agent also verifies the peer's tailnet identity via Tailscale LocalAPI `whois` on the TCP source IP and checks it against an allowlist (default: only devices owned by the same tailnet user).

## 5. Test vectors
docs/vectors/ (added in M1): fixed key/nonce/packets so Rust and Kotlin parsers are tested against identical bytes.

## Audio redirection (agent, Linux)

By default the agent captures a virtual sink instead of a physical output's monitor. On start it creates a null sink named `meshwav` ("Meshwav" in sound settings; creating it changes nothing audible) and captures `meshwav.monitor`. While a receiver is connected it makes `meshwav` the default output and moves every playing stream onto it, so the PC plays nothing locally; IEMs, speakers and HDMI can be plugged and unplugged freely. If a device grabs the default while redirected, the agent takes it back and remembers that device as the one to restore. Streams started on a specific device are moved within about 0.5 s.

The previous output is restored, and streams moved back, 15 s after the receiver goes away (so a network blip does not flip the output) and on SIGINT/SIGTERM or a panic; the virtual sink is removed on exit. If the saved output no longer exists, any other real sink is used. The output to restore is written to `~/.config/meshwav/redirect.state` while redirected, so a run that was killed (SIGKILL, power loss) is undone on the next start. The receiver also sends a sealed control `Bye` when the user disconnects (3 copies); the agent then stops sending, ignores that session's packets and restores the output immediately, with no grace period. The grace period still covers silent drops such as a lost network. Volume and mute of physical sinks are never changed. `--no-redirect`, or pinning a capture with `--source`, turns this off and keeps the old behavior (the PC keeps playing locally).

Known limits: this relies on `pactl` (PulseAudio or PipeWire-pulse); a desktop's stream-restore may remember moved streams, so an app might open on the Meshwav sink after the agent is gone only if the sink still exists (it is removed on a clean exit).
