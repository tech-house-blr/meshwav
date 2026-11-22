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
| Spoofed discovery response | Discovery is only a hint (Tailscale peer list + probe). Identity is established solely by key pinning in the handshake. UI shows peer name + key fingerprint. |
| Pairing brute force | 6-digit code, 5 attempts, lockout, 2-min expiry, code never logged at info level. |
| Resource exhaustion (handshake flood, UDP flood) | Max 1 pending unauthenticated handshake per source, short timeouts, unauthenticated UDP dropped before any parsing beyond header, bounded queues. |
| Credentials leakage | No hardcoded secrets. Keys generated on first run, stored with 0600 perms (Windows: DPAPI; Android: Keystore-wrapped). |
| Audio sent to third parties | None: no analytics, no cloud calls; only tailnet peers. Documented and test-checked (no outbound to non-tailnet addresses). |
| Downgrade | Version and codec set bound into the handshake transcript. |

## Status after M4b
Built and tested: Noise XX/XXpsk3 with pinned keys, pairing code (expiry, single use, lockout), per-session keys, bounded handshake state, 0600 key files, name sanitising.
Not built yet: Tailscale `whois` allowlist, `revoke`/`status` commands (edit `authorized_peers` by hand for now), Keystore wrapping of the phone's key (it sits in app-private storage, backups disabled), version/codec binding beyond the fixed `meshwav-v1` prologue, Windows DPAPI.
Known limits: the phone's X25519 uses BigInteger and is not constant-time (local-attacker only). Anyone on the tailnet who can reach the agent can burn the pairing attempts and force a 60 s lockout while a window is open. A capture of a legitimate message 3 gives no offline code-guessing oracle to a passive observer; an active impostor agent is stopped by the pinned agent key in the pairing line.

## Out of scope / residual risk
Compromised endpoint; Tailscale account takeover (attacker adds a device, but still needs pairing/pinned key); traffic analysis of bitrate patterns.
Recommendation in docs: add a Tailscale ACL limiting the agent port to the phone's tag/user.

## Operational
Logs never contain audio, keys or pairing codes. `agent status` shows paired peers and fingerprints; `agent revoke <peer>` removes a pairing.
