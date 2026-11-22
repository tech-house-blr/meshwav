//! Transport helpers shared by the agent and tools.
use std::net::IpAddr;

/// True if `ip` is a Tailscale address (100.64.0.0/10 or fd7a:115c:a1e0::/48) or loopback.
/// Listeners must refuse to bind anything else (see docs/security.md).
pub fn is_tailnet_or_loopback(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(v4) => {
            let o = v4.octets();
            v4.is_loopback() || (o[0] == 100 && (64..=127).contains(&o[1]))
        }
        IpAddr::V6(v6) => {
            let s = v6.segments();
            v6.is_loopback() || (s[0] == 0xfd7a && s[1] == 0x115c && s[2] == 0xa1e0)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn tailnet_ranges() {
        for ok in ["100.64.0.1", "100.127.255.255", "127.0.0.1", "fd7a:115c:a1e0::1", "::1"] {
            assert!(is_tailnet_or_loopback(ok.parse().unwrap()), "{ok}");
        }
        for bad in ["0.0.0.0", "100.63.255.255", "100.128.0.0", "192.168.1.1", "8.8.8.8", "::", "fd7a:115c:a1e1::1", "fe80::1"] {
            assert!(!is_tailnet_or_loopback(bad.parse().unwrap()), "{bad}");
        }
    }
}
