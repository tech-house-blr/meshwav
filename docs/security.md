# Security model

## Assets
System audio content (may contain private material); control over the desktop's audio capture; device identity.

## Assumptions
- Tailscale is correctly installed; WireGuard confidentiality/integrity hold between nodes.
- Tailnet is NOT a fully trusted network: other users/devices/shared nodes may exist, and ACLs may be permissive.
- Endpoints (desktop OS, phone OS) are not compromised.
- Tailscale DERP relays see only WireGuard ciphertext; our payload is additionally AEAD-encrypted end-to-end.

## Threats and controls
| Threat | Control |
|---|---|
| Arbitrary host on the internet reaches the agent | Agent binds only to tailnet interface addresses (100.64.0.0/10 and fd7a:115c:a1e0::/48); refuses 0.0.0.0. Startup check fails closed if no tailnet address. |
| Other tailnet device connects and listens | Noise handshake with pinned static keys, pairing code as PSK on first use, whois check against an allowlist. |
| Rogue device injects/replays audio to the phone | Per-session AEAD key, replay window, receiver only accepts packets from the agent address that completed the control handshake. |
| Spoofed discovery response | Discovery is only a hint (Tailscale peer list, a probe, or an invite), and a key learned that way is not authenticated. Pairing with an unauthenticated sender would give it what it needs to recover the 6-digit code offline (see the second-pass review below), so a receiver that found a sender by code alone first shows the sender's key fingerprint, and the person confirms it matches the one on the sender's screen. Nothing code-dependent is sent before that. The pairing link carries the key and avoids the question. After pairing, identity is the key pinned by the handshake. |
| Pairing brute force | 6-digit code, 5 attempts, lockout, 2-min expiry, code never logged at info level. |
| Resource exhaustion (handshake flood, UDP flood) | At most 2 pending unauthenticated handshakes and 5 new handshakes per second per source address, inside a shared cap of 16 pending and 30 per second, so one device cannot lock the others out. Short timeouts, unauthenticated UDP dropped before any parsing beyond header, bounded queues. |
| A pairing link or typed line points the phone at a host outside the tailnet | The Android app refuses any host that is not a tailnet address literal, in the activity and again in the playback service. A `meshwav://pair` link also asks for confirmation (host and key fingerprint) before the code is used. |
| Credentials leakage | No hardcoded secrets. Keys generated on first run, stored with 0600 perms (Windows: DPAPI; Android: Keystore-wrapped). |
| Audio sent to third parties | None: no analytics, no cloud calls; only tailnet peers. Documented and test-checked (no outbound to non-tailnet addresses). |
| Downgrade | Version and codec set bound into the handshake transcript. |

## Status after M4b
Built and tested: Noise XX/XXpsk3 with pinned keys, pairing code (expiry, single use, lockout), per-session keys, bounded handshake state, 0600 key files, name sanitising.
Not built yet: Tailscale `whois` allowlist, a `status` command, Keystore wrapping of the phone's key (it sits in app-private storage, backups disabled), version/codec binding beyond the fixed `meshwav-v1` prologue, Windows DPAPI.
Known limits: the phone's X25519 uses BigInteger and is not constant-time (local-attacker only). Anyone on the tailnet who can reach the agent can burn the pairing attempts and force a 60 s lockout while a window is open. A capture of a legitimate message 3 gives no offline code-guessing oracle to a passive observer; an active impostor agent is stopped by the pinned agent key in the pairing line.

## Pairing discovery
While a pairing window is open, a sender answers a `HELLO` probe with its public key and invites waiting phones with an `INVITE`. Both are unauthenticated and only exchanged with tailnet or loopback addresses; a closed or locked sender stays silent. They reveal nothing but the public key and port, and the pairing itself still needs the code and the handshake.

## Out of scope / residual risk
Compromised endpoint; Tailscale account takeover (attacker adds a device, but still needs pairing/pinned key); traffic analysis of bitrate patterns.
Recommendation in docs: add a Tailscale ACL limiting the agent port to the phone's tag/user.

## Operational
Logs never contain audio, keys or pairing codes. `meshwav devices` shows paired devices and their state; there is no revoke command yet (see above).

Removing a paired device: `meshwav remove <device>` (or Remove in the Send tab) deletes its key from `authorized_peers`, drops its live session at once and clears it as the chosen receiver. It can only return by pairing again with a new code.

## Security review of 0.4.0

A first review of the protocol, handshake, key storage, binding, pairing link and dependencies, done before 1.0. It was a read-through plus targeted tests, not an audit by a third party.

Fixed after the review:
- **Pairing link (high).** The Android app started pairing as soon as a `meshwav://pair` link was opened, with no confirmation and no check that the host was on the tailnet. A link on any web page could make the phone play an attacker's audio, contact an address outside the tailnet and keep that sender as its default. The app now refuses non-tailnet hosts and asks the user to confirm.
- **Handshake slots (medium).** The limit of pending handshakes was shared, not per source, so one tailnet device sending handshake starts under different session ids could fill the table and keep every other device out, with no key and no pairing window. The limit is now per source address, and each source has its own start rate before the shared one.

Checked and found sound:
- Per-direction AEAD nonces, the replay window, the pairing PSK derivation, pairing code generation (uniform, 5 attempts, lockout, single use), file permissions on keys and the control socket, and the tailnet bind check.
- 400,000 random inputs to the packet, control, handshake and server parsers produced no panics. This was a one-off run, not a standing fuzz test.
- `cargo audit` reports no vulnerabilities. It warns about unmaintained or unsound crates (`audiopus_sys`, `paste`, `ttf-parser`, `zbus_names`, `zvariant`, and `heapless` 0.8 through the FLAC encoder in the sender). None is reachable from untrusted network input as far as this review could tell.

Still open:
- The Android Noise code, the Linux receiver session logic, the GUI and the `pactl` redirect handling were not reviewed.
- The Android release is debug-signed. Releases carry no signatures or checksums from the build, and the workflow actions are pinned by tag, not by commit.
- `meshwav pair CODE` puts the code on the command line, where a local user can see it in `ps` while it runs. The code is single-use.
- The limits above count per address. Several malicious tailnet devices acting together can still use up the shared limits. A Tailscale ACL narrowing who can reach the agent port remains the better control.
- Gradle (Android) dependencies were not audited.

## Second pass (0.4.0): code-only pairing

Reading the receivers, the Noise code and the process handling turned up one real problem.

- **A fake sender could learn the pairing code and then pair with the real one (medium to high).** With only the code typed, a receiver finds the sender from an unauthenticated probe answer or invite, so any tailnet device could answer with a key of its own. The receiver then sent its final handshake message, whose payload is encrypted under a key that depends on the code. Trying all 1,000,000 codes against that message recovered the code from a captured handshake in a test (977 microseconds per guess on the Pi, so about 16 minutes on one core with the stock library; a faster implementation on several cores would fit inside the 2-minute window, which is an estimate and was not measured). The real sender's window stays open because the receiver paired with the impostor, so the impostor could then pair as a receiver and get the computer's audio. The Linux receiver was worse, since it tried the code against every sender that answered. Pairing from a link was never affected.
- **Fix.** The sender shows a 48-bit key fingerprint with its code, and every receiver that finds a sender by code alone shows the fingerprint of what it found and sends nothing until the person confirms they match (phone: a dialog; Linux window: Yes or No; command line: a prompt, or `--fingerprint`, and no answer without a terminal). A refused sender is skipped and the search goes on. 48 bits make it impractical for an attacker to produce a key with a matching fingerprint within a pairing window.
- **Limits.** The check relies on the person comparing the two fingerprints. A pairing link or QR code remains the safer way. This does not make the 6-digit code strong on its own, and the code alone must not be treated as proof of who the sender is.

Checked and found fine in the same pass: no shell or command-string execution anywhere (`pactl`, `parec`, `pacat` and `systemctl` get argument lists); device names from a paired receiver have control characters removed, so they cannot inject terminal escapes; the control socket is owner-only; the Linux GUI only fills in a pairing link and never pairs by itself; the Android Noise code and message parsing are strict and bounded; the Android app has no third-party runtime libraries, only JUnit for tests.

Small items left: pasting a hostile link into the Linux receiver and pairing triggers a DNS lookup for its host before the tailnet check refuses it. The Android receiver has no replay window of its own (the jitter buffer drops duplicates and the socket accepts only the sender's address). The `pactl` redirect logic was read for injection, not for races, and the GUI drawing code was not reviewed.
