# Security Policy

Meshwav is pre-release software. The pairing and handshake code (a Noise XX/XXpsk3 implementation using the `snow` crate) is custom to this project and has not been independently audited. Do not rely on it for high-stakes use yet.

## Reporting a vulnerability

Please do not open a public issue. Use GitHub's private vulnerability reporting
(Security tab, "Report a vulnerability") on this repository. Include steps to reproduce and the affected version or commit.

You can expect an acknowledgement within a few days. Fixes are made on a best-effort basis while the project is in early development.

## Scope

In scope: the wire protocol, handshake and pairing, key storage, the agent's network binding, and the Android receiver.
Out of scope: Tailscale itself, and attacks that need an already-compromised device.

See [docs/security.md](docs/security.md) for the threat model.
