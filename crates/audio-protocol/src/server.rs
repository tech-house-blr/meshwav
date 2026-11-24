//! Agent-side handshake server: a pure state machine (no sockets, injected clock) that turns
//! handshake datagrams into replies and, eventually, an authenticated session.
//!
//! Policy:
//! * `XX` (kind 0x10): the initiator's static key must already be authorized.
//! * `XXpsk3` (kind 0x11): only while a pairing window is open. Window: 2 minutes, single use,
//!   5 failed codes then a 60 s lockout. While closed or locked, pairing handshakes get no reply.
//! * Pending handshakes are bounded and short-lived; msg1 intake is rate limited.
use crate::handshake::{self, Keypair, PublicKey, Responder, SessionKeys, KIND_HELLO, KIND_XX, KIND_XXPSK3};
use std::collections::HashMap;
use std::net::SocketAddr;

pub const PAIR_TTL_MS: u64 = 120_000;
pub const MAX_PAIR_FAILURES: u32 = 5;
pub const LOCKOUT_MS: u64 = 60_000;
pub const PENDING_TTL_MS: u64 = 5_000;
pub const DONE_TTL_MS: u64 = 30_000;
pub const MAX_PENDING: usize = 16;
pub const MSG1_PER_SEC: u32 = 30;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Authorized {
    pub public: PublicKey,
    pub name: String,
}

struct PairingWindow {
    psk: [u8; 32],
    expires_ms: u64,
    failures: u32,
    locked_until_ms: u64,
}

struct Pending {
    resp: Responder,
    msg2: Vec<u8>,
    kind: u8,
    created_ms: u64,
}

#[derive(Debug)]
pub enum Outcome {
    /// Nothing to do (ignored, duplicate, rate limited, or silently dropped on purpose).
    None,
    /// Send this datagram back to the sender.
    Reply(Vec<u8>),
    Established { keys: SessionKeys, peer: PublicKey, name: String, newly_paired: bool },
    Rejected(&'static str),
}

pub struct Server {
    identity: Keypair,
    authorized: Vec<Authorized>,
    pairing: Option<PairingWindow>,
    pending: HashMap<(SocketAddr, u32), Pending>,
    done: HashMap<(SocketAddr, u32), u64>,
    budget_sec: u64,
    budget_used: u32,
}

impl Server {
    pub fn new(identity: Keypair, authorized: Vec<Authorized>) -> Self {
        Self {
            identity,
            authorized,
            pairing: None,
            pending: HashMap::new(),
            done: HashMap::new(),
            budget_sec: 0,
            budget_used: 0,
        }
    }

    pub fn public_key(&self) -> PublicKey {
        self.identity.public
    }

    pub fn authorized(&self) -> &[Authorized] {
        &self.authorized
    }

    /// Change the stored name of a paired device. Returns false when the key is not paired.
    pub fn rename(&mut self, public: &PublicKey, name: &str) -> bool {
        match self.authorized.iter_mut().find(|a| &a.public == public) {
            Some(a) => {
                a.name = name.to_string();
                true
            }
            None => false,
        }
    }

    /// Forget a paired device. Returns false when the key is not paired.
    pub fn revoke(&mut self, public: &PublicKey) -> bool {
        let before = self.authorized.len();
        self.authorized.retain(|a| &a.public != public);
        self.authorized.len() != before
    }

    /// Open (or restart) the pairing window with a 6-digit code. Returns false for a bad code.
    pub fn open_pairing(&mut self, code: &str, now_ms: u64) -> bool {
        self.open_pairing_for(code, now_ms, PAIR_TTL_MS)
    }

    /// Like `open_pairing`, with a window of `ttl_ms` instead of the default 2 minutes.
    pub fn open_pairing_for(&mut self, code: &str, now_ms: u64, ttl_ms: u64) -> bool {
        if !handshake::valid_code(code) {
            return false;
        }
        self.pairing = Some(PairingWindow {
            psk: handshake::pairing_psk(&self.identity.public, code),
            expires_ms: now_ms + ttl_ms,
            failures: 0,
            locked_until_ms: 0,
        });
        true
    }

    pub fn pairing_open(&self, now_ms: u64) -> bool {
        self.pairing.as_ref().is_some_and(|p| now_ms < p.expires_ms)
    }

    fn gc(&mut self, now_ms: u64) {
        self.pending.retain(|_, p| now_ms.saturating_sub(p.created_ms) < PENDING_TTL_MS);
        self.done.retain(|_, t| now_ms.saturating_sub(*t) < DONE_TTL_MS);
        if self.pairing.as_ref().is_some_and(|p| now_ms >= p.expires_ms) {
            self.pairing = None;
        }
    }

    fn msg1_allowed(&mut self, now_ms: u64) -> bool {
        let sec = now_ms / 1000;
        if sec != self.budget_sec {
            self.budget_sec = sec;
            self.budget_used = 0;
        }
        self.budget_used += 1;
        self.budget_used <= MSG1_PER_SEC
    }

    pub fn handle(&mut self, from: SocketAddr, datagram: &[u8], now_ms: u64) -> Outcome {
        self.gc(now_ms);
        let Some(f) = handshake::parse_frame(datagram) else { return Outcome::None };
        let key = (from, f.sid);
        if f.kind == KIND_HELLO {
            return self.on_hello(f.sid, f.step, now_ms);
        }
        match f.step {
            1 => self.on_msg1(key, f.kind, f.body, now_ms),
            3 => self.on_msg3(key, f.kind, f.body, now_ms),
            _ => Outcome::None,
        }
    }

    /// Discovery probe: answer with the public key, but only while a pairing window is open and not
    /// locked. A closed sender stays silent, so it cannot be found or fingerprinted.
    fn on_hello(&mut self, sid: u32, step: u8, now_ms: u64) -> Outcome {
        let open = self.pairing.as_ref().is_some_and(|w| now_ms >= w.locked_until_ms);
        if step != 1 || !open || !self.msg1_allowed(now_ms) {
            return Outcome::None;
        }
        Outcome::Reply(handshake::frame(KIND_HELLO, sid, 2, &self.identity.public))
    }

    fn on_msg1(&mut self, key: (SocketAddr, u32), kind: u8, body: &[u8], now_ms: u64) -> Outcome {
        if let Some(p) = self.pending.get(&key) {
            // Retransmitted msg1 (our reply was lost): resend the stored reply, do no new work.
            return if p.kind == kind { Outcome::Reply(handshake::frame(kind, key.1, 2, &p.msg2)) } else { Outcome::None };
        }
        if self.done.contains_key(&key) || !self.msg1_allowed(now_ms) {
            return Outcome::None;
        }
        let psk = match kind {
            KIND_XX => None,
            KIND_XXPSK3 => match &self.pairing {
                Some(w) if now_ms < w.locked_until_ms => return Outcome::None,
                Some(w) => Some(w.psk),
                None => return Outcome::None, // no pairing window: no reply, no oracle
            },
            _ => return Outcome::None,
        };
        if self.pending.len() >= MAX_PENDING {
            return Outcome::None;
        }
        let Ok(mut resp) = Responder::new(&self.identity, psk.as_ref()) else { return Outcome::None };
        if resp.read_msg1(body).is_err() {
            return Outcome::None;
        }
        let Ok(msg2) = resp.write_msg2() else { return Outcome::None };
        let reply = handshake::frame(kind, key.1, 2, &msg2);
        self.pending.insert(key, Pending { resp, msg2, kind, created_ms: now_ms });
        Outcome::Reply(reply)
    }

    fn on_msg3(&mut self, key: (SocketAddr, u32), kind: u8, body: &[u8], now_ms: u64) -> Outcome {
        if self.pending.get(&key).is_none_or(|p| p.kind != kind) {
            return Outcome::None; // unknown, expired or already completed (retransmitted msg3)
        }
        let mut p = self.pending.remove(&key).unwrap();
        let psk_mode = kind == KIND_XXPSK3;
        if psk_mode {
            match &self.pairing {
                Some(w) if now_ms < w.locked_until_ms => return Outcome::Rejected("pairing locked"),
                Some(_) => {}
                None => return Outcome::Rejected("pairing window closed"),
            }
        }
        let (peer, name) = match p.resp.read_msg3(body) {
            Ok(v) => v,
            Err(_) => {
                if psk_mode {
                    if let Some(w) = self.pairing.as_mut() {
                        w.failures += 1;
                        if w.failures >= MAX_PAIR_FAILURES {
                            w.failures = 0;
                            w.locked_until_ms = now_ms + LOCKOUT_MS;
                            return Outcome::Rejected("wrong pairing code (locked for 60 s)");
                        }
                    }
                    return Outcome::Rejected("wrong pairing code");
                }
                return Outcome::Rejected("handshake failed");
            }
        };
        let newly_paired = if psk_mode {
            self.pairing = None; // single use
            match self.authorized.iter_mut().find(|a| a.public == peer) {
                Some(a) => a.name = name.clone(),
                None => self.authorized.push(Authorized { public: peer, name: name.clone() }),
            }
            true
        } else {
            if !self.authorized.iter().any(|a| a.public == peer) {
                return Outcome::Rejected("device is not paired");
            }
            false
        };
        let Ok(keys) = p.resp.finish() else { return Outcome::Rejected("handshake failed") };
        self.done.insert(key, now_ms);
        Outcome::Established { keys, peer, name, newly_paired }
    }
}
