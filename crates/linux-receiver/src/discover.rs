//! Find the sender that is showing a pairing code, so the user only types the code.
//!
//! Every online tailnet peer (plus loopback) gets a small hello on the agent ports. A sender answers
//! only while its pairing window is open, and the answer carries its public key. Only tailnet and
//! loopback addresses are ever probed.
use audio_protocol::handshake::{self, PublicKey, KIND_HELLO};
use audio_transport::is_tailnet_or_loopback;
use std::net::{SocketAddr, UdpSocket};
use std::time::{Duration, Instant};

/// Agent port, and the phone's source port.
pub const PORTS: [u16; 2] = [47900, 47901];

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Found {
    pub addr: SocketAddr,
    pub key: PublicKey,
}

impl Found {
    /// What a person compares with the fingerprint the sender shows next to its code.
    pub fn fingerprint(&self) -> String {
        handshake::fingerprint(&self.key)
    }
}

use audio_transport::tailnet_peers;

/// Probe the given targets and return every sender that answered.
pub fn probe(targets: &[SocketAddr], wait: Duration) -> Vec<Found> {
    let targets: Vec<SocketAddr> = targets.iter().copied().filter(|t| is_tailnet_or_loopback(t.ip())).collect();
    let Ok(sock) = UdpSocket::bind("0.0.0.0:0") else { return vec![] };
    let _ = sock.set_read_timeout(Some(Duration::from_millis(100)));
    let sid = u32::from_be_bytes(rand4());
    let hello = handshake::frame(KIND_HELLO, sid, 1, &[]);
    let end = Instant::now() + wait;
    let mut next_send = Instant::now();
    let mut found: Vec<Found> = vec![];
    let mut buf = [0u8; 512];
    while Instant::now() < end {
        if Instant::now() >= next_send {
            for t in &targets {
                let _ = sock.send_to(&hello, t);
            }
            next_send = Instant::now() + Duration::from_millis(500);
        }
        if let Ok((n, from)) = sock.recv_from(&mut buf) {
            if !targets.contains(&from) {
                continue;
            }
            if let Some(f) = handshake::parse_frame(&buf[..n]) {
                if f.kind == KIND_HELLO && f.sid == sid && f.step == 2 && f.body.len() == 32 {
                    let mut key = [0u8; 32];
                    key.copy_from_slice(f.body);
                    let fnd = Found { addr: from, key };
                    if !found.contains(&fnd) {
                        found.push(fnd);
                    }
                }
            }
        }
    }
    found
}

fn rand4() -> [u8; 4] {
    let mut b = [0u8; 4];
    let _ = getrandom::getrandom(&mut b);
    b
}

/// Senders currently showing a pairing code: tailnet peers and this machine.
pub fn find_senders() -> Vec<Found> {
    let mut ips = tailnet_peers();
    ips.push("127.0.0.1".parse().unwrap());
    let targets: Vec<SocketAddr> = ips.iter().flat_map(|ip| PORTS.iter().map(move |p| SocketAddr::new(*ip, *p))).collect();
    probe(&targets, Duration::from_millis(2500))
}

/// Pair using only the 6-digit code: find the sender showing it, then run the normal pairing.
///
/// A sender found this way is not authenticated: any tailnet device can answer the probe with a
/// key of its own, and pairing with it would hand it the material to recover the code offline. So
/// `confirm` is asked about every sender before anything code-dependent is sent, and only senders
/// it approves are tried. Callers approve by comparing [`handshake::fingerprint`] with the one the
/// real sender shows next to its code.
pub fn pair_with_code(
    code: &str,
    identity: handshake::Keypair,
    name: String,
    stop: &std::sync::atomic::AtomicBool,
    confirm: &mut dyn FnMut(&Found) -> bool,
) -> Result<crate::store::Paired, String> {
    if !handshake::valid_code(code) {
        return Err("the pairing code is 6 digits".into());
    }
    let senders = find_senders();
    if senders.is_empty() {
        return Err("No device is showing a pairing code. Start pairing on the sender first (Start in Send mode on the phone, or `meshwav pair`).".into());
    }
    pair_with_senders(senders, code, identity, name, stop, confirm)
}

/// The part of [`pair_with_code`] after discovery: ask about each sender, try the approved ones.
pub fn pair_with_senders(
    senders: Vec<Found>,
    code: &str,
    identity: handshake::Keypair,
    name: String,
    stop: &std::sync::atomic::AtomicBool,
    confirm: &mut dyn FnMut(&Found) -> bool,
) -> Result<crate::store::Paired, String> {
    let mut last = "No sender was confirmed, so nothing was sent. Compare the key fingerprint with the one the sender shows.".to_string();
    for f in senders {
        if stop.load(std::sync::atomic::Ordering::Relaxed) {
            return Err("stopped".into());
        }
        if !confirm(&f) {
            continue;
        }
        let s = crate::session::Settings { agent: f.addr, agent_key: f.key, identity: identity.clone(), name: name.clone() };
        match crate::session::pair(&s, code, stop) {
            Ok(()) => return Ok(crate::store::Paired { agent: f.addr, key: f.key }),
            Err(e) => last = e,
        }
    }
    Err(last)
}
