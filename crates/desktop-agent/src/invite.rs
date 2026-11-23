//! While a pairing window is open, tell phones that are waiting for a sender where this one is, so the
//! user types only the 6-digit code. Online tailnet peers get a small unauthenticated invite on the
//! phone's pairing port; a phone that is not waiting simply has nothing listening. The code and the
//! key pinned by the handshake still protect the pairing itself. Only tailnet and loopback addresses
//! are ever contacted.
use audio_protocol::handshake::{invite_frame, PublicKey};
use audio_transport::is_tailnet_or_loopback;
use std::net::{IpAddr, SocketAddr, UdpSocket};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

/// Where a waiting phone listens.
pub const PHONE_PORT: u16 = 47902;
const EVERY: Duration = Duration::from_secs(2);
const PEERS_EVERY: Duration = Duration::from_secs(10);

static GENERATION: AtomicU64 = AtomicU64::new(0);

/// Invite phones for `ttl`. A newer call (the window was reopened) replaces an older one.
pub fn announce(own_ip: IpAddr, key: PublicKey, port: u16, ttl: Duration) {
    announce_to(own_ip, key, port, ttl, move || {
        audio_transport::tailnet_peers().into_iter().filter(|ip| *ip != own_ip).map(|ip| SocketAddr::new(ip, PHONE_PORT)).collect()
    });
}

pub fn announce_to(own_ip: IpAddr, key: PublicKey, port: u16, ttl: Duration, targets: impl Fn() -> Vec<SocketAddr> + Send + 'static) {
    let me = GENERATION.fetch_add(1, Ordering::SeqCst) + 1;
    std::thread::spawn(move || {
        let Ok(sock) = UdpSocket::bind(SocketAddr::new(own_ip, 0)) else { return };
        let mut sid = [0u8; 4];
        let _ = getrandom::getrandom(&mut sid);
        let msg = invite_frame(u32::from_be_bytes(sid), &key, port);
        let end = Instant::now() + ttl;
        let (mut peers, mut peers_at) = (Vec::new(), None::<Instant>);
        while Instant::now() < end && GENERATION.load(Ordering::SeqCst) == me {
            if peers_at.map_or(true, |t: Instant| t.elapsed() >= PEERS_EVERY) {
                peers = targets();
                peers_at = Some(Instant::now());
            }
            for t in peers.iter().filter(|t| is_tailnet_or_loopback(t.ip())) {
                let _ = sock.send_to(&msg, t);
            }
            std::thread::sleep(EVERY);
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use audio_protocol::handshake::parse_invite;

    #[test]
    fn a_waiting_phone_gets_the_senders_key_and_port() {
        let phone = UdpSocket::bind("127.0.0.1:0").unwrap();
        phone.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
        let target = phone.local_addr().unwrap();
        announce_to("127.0.0.1".parse().unwrap(), [7; 32], 47900, Duration::from_secs(4), move || vec![target, "8.8.8.8:47902".parse().unwrap()]);
        let mut buf = [0u8; 128];
        let (n, _) = phone.recv_from(&mut buf).unwrap();
        assert_eq!(parse_invite(&buf[..n]), Some(([7; 32], 47900)));
    }
}
