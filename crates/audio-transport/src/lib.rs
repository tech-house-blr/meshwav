//! Transport helpers shared by the agent and tools.
use std::net::IpAddr;

/// IPs of online peers from the text output of `tailscale status`.
pub fn parse_peers(status: &str) -> Vec<IpAddr> {
    status
        .lines()
        .filter(|l| !l.to_lowercase().contains("offline"))
        .filter_map(|l| l.split_whitespace().next()?.parse::<IpAddr>().ok())
        .filter(|ip| is_tailnet_or_loopback(*ip))
        .collect()
}

/// Online tailnet peers according to the local `tailscale` command (empty if it is missing or fails).
pub fn tailnet_peers() -> Vec<IpAddr> {
    match {
        let mut c = std::process::Command::new("tailscale");
        c.arg("status");
        #[cfg(windows)]
        std::os::windows::process::CommandExt::creation_flags(&mut c, 0x0800_0000); // no console window flashing up
        c.output()
    } {
        Ok(o) => parse_peers(&String::from_utf8_lossy(&o.stdout)),
        Err(_) => vec![],
    }
}

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
mod peers_tests {
    use super::*;

    #[test]
    fn peers_skip_offline_and_non_tailnet() {
        let t = "100.72.29.9    liberation   me@  linux   -\n100.127.116.12 galaxy  me@ android offline, last seen 1h ago\n100.100.1.2 pi me@ linux active; direct\n192.168.1.5 lan x linux -\n# Health check:\n";
        let p = parse_peers(t);
        assert_eq!(p, vec!["100.72.29.9".parse::<IpAddr>().unwrap(), "100.100.1.2".parse().unwrap()]);
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
