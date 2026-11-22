# Protocol v1

Two channels between agent (A) and receiver (R). R initiates. A listens only on its tailnet IP(s).
All integers are big-endian.

## 1. Control channel (TCP, default port 47800)
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
