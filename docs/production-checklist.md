# Production checklist

Status as of v0.9.2. "Done" means verified by CI or a test run; "Open" needs a person, a device or a decision.

## Done
- Parsers fuzz-tested with seeded inputs; three peer-reachable crashes fixed (#64).
- CI actions pinned to SHAs, weekly cargo-deny run, Dependabot limited to minor and patch (#65, #80).
- Per-receiver rate limits on control messages, play-here and retune (#77).
- Config folders 0700, key files 0600, world-readable key refused (#78).
- Light systemd sandbox for the agent unit (#79).
- 3-hour soak: no memory or file-descriptor growth. Agent restart: receiver back in about 4.5 s. 15 s agent stall: receiver reconnects by itself.

## Open
1. Install v0.9.2 on every machine in use and confirm the agent starts under the sandboxed unit.
2. Phone network change: switch Wi-Fi and mobile data, and sleep and wake, while streaming; the receiver must reconnect.
3. Update the second Android phone to 0.12.1 and stream to it.
4. Windows pass on a real PC: install, tray (right-click Quit), pairing, streaming, key storage.
5. Slow Tailscale boot (#46): reboot with Tailscale slow to come up; the agent must wait and bind.
6. Package install test for the rpm.
7. Independent read of the handshake and pairing code.
8. Windows DPAPI key wrapping (not built).
9. Android Keystore key wrapping (not built; skipped on purpose, keys are in private app storage with backups off).
10. Latency: the default Android output misses the target; only the native output meets it, on one phone.
11. Licenses check in cargo-deny (not enabled).
12. Release hosting: apt/dnf repo and signed updates (none).
