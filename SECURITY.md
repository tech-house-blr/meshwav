# Security Policy

Meshwav is pre-1.0 software. The pairing and handshake code (a Noise XX/XXpsk3 implementation using the `snow` crate on the computer side, and a separate implementation on Android) is custom to this project and has not been independently audited. Do not rely on it for high-stakes use yet.

An internal review of 0.4.0 found and fixed a pairing-link problem on Android, a handshake lock-out by a single tailnet device, and a weakness in code-only pairing (a fake sender could learn the code), which now needs a key fingerprint check. The full list of what was checked, fixed and left open is in [docs/security.md](docs/security.md#security-review-of-040).

## Supported versions

Only the latest release gets fixes. Please update before reporting.

## Reporting a vulnerability

Please do not open a public issue. Use GitHub's private vulnerability reporting
(Security tab, "Report a vulnerability") on this repository. Include steps to reproduce and the affected version or commit.

You can expect an acknowledgement within a few days. Fixes are made on a best-effort basis while the project is in early development.

## Scope

In scope: the wire protocol, handshake and pairing (including `meshwav://pair` links and discovery), key storage, the agent's network binding, the Android and Linux receivers, and the release packages.
Out of scope: Tailscale itself, and attacks that need an already-compromised device.

## What to know before using it

- Meshwav only talks to Tailscale addresses. Listeners bind to the tailnet or loopback, and the receivers refuse other hosts.
- Open a pairing link or type a pairing code only when you started pairing on the sender just now. The Android app asks you to confirm a link before using it.
- When you pair with just the code, the receiver shows the key fingerprint of the sender it found. Confirm it matches the fingerprint on the sender's screen; if it does not, choose No. A pairing link or QR code skips this step because it carries the key.
- Releases are not yet signed: the Android APK is debug-signed and the packages come without signatures. Download them only from this repository's releases page.
- Limiting the agent port with a Tailscale ACL to your own devices is recommended.

See [docs/security.md](docs/security.md) for the threat model.
