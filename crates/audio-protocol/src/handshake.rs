//! Noise handshake (Noise_XX_25519_ChaChaPoly_SHA256, and the XXpsk3 variant used for pairing),
//! plus the datagram framing that carries it. Pure data/crypto, no I/O. See docs/protocol.md.
//!
//! * Pairing (`XXpsk3`): a 6-digit code, bound to the agent's static key, is mixed in as the PSK.
//! * Reconnect (`XX`): both sides present their pinned static keys; no code needed.
//! * Output is two raw 32-byte keys, one per direction, fed to `crypto::Sealer`/`Opener`.
//!   The initiator is always the receiver (phone); the responder is the agent.
use crate::ProtocolError;
use sha2::{Digest, Sha256};
use snow::{Builder, HandshakeState};

pub const PROLOGUE: &[u8] = b"meshwav-v1";
pub const PATTERN_XX: &str = "Noise_XX_25519_ChaChaPoly_SHA256";
pub const PATTERN_XXPSK3: &str = "Noise_XXpsk3_25519_ChaChaPoly_SHA256";
pub const MAX_DEVICE_NAME: usize = 32;

pub type PublicKey = [u8; 32];

/// Datagram kinds (byte 3 of the frame; audio packet types are 1..=3, so they never collide).
pub const KIND_XX: u8 = 0x10;
pub const KIND_XXPSK3: u8 = 0x11;
/// Pairing discovery probe: a sender with an open pairing window answers with its static public key.
pub const KIND_HELLO: u8 = 0x12;
/// A sender with an open pairing window tells a waiting phone where it is: body = public key (32) + port (u16 BE).
/// Unauthenticated by design, like HELLO: the pairing code and the pinned key protect the pairing itself.
pub const KIND_INVITE: u8 = 0x13;
const FRAME_HEADER: usize = 9;

#[derive(Clone)]
pub struct Keypair {
    pub private: [u8; 32],
    pub public: PublicKey,
}

impl Keypair {
    pub fn generate() -> Self {
        let kp = Builder::new(PATTERN_XX.parse().unwrap())
            .generate_keypair()
            .expect("keypair generation");
        let private: [u8; 32] = kp.private.try_into().expect("32-byte key");
        Self::from_private(private)
    }

    pub fn from_private(private: [u8; 32]) -> Self {
        let public = curve25519_dalek::montgomery::MontgomeryPoint::mul_base_clamped(private).to_bytes();
        Self { private, public }
    }
}

impl std::fmt::Debug for Keypair {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "Keypair(public={})", hex(&self.public))
    }
}

pub fn hex(b: &[u8]) -> String {
    b.iter().map(|x| format!("{x:02x}")).collect()
}

/// PSK for pairing: SHA-256("meshwav-pair-v1" | agent static public key | ASCII code).
pub fn pairing_psk(agent_public: &PublicKey, code: &str) -> [u8; 32] {
    let mut h = Sha256::new();
    h.update(b"meshwav-pair-v1");
    h.update(agent_public);
    h.update(code.as_bytes());
    h.finalize().into()
}

/// True for exactly six ASCII digits.
/// Short form of a sender's public key for people to compare, like `4F2A-91C0-7B3E` (48 bits, upper
/// case). A sender shows it with its pairing code; a receiver that found the sender by code alone
/// shows what it found, and the person confirms they match before anything is sent. Without that
/// check, any tailnet device could answer as the sender and recover the code offline (docs/security.md).
pub fn fingerprint(key: &PublicKey) -> String {
    let mut h = Sha256::new();
    h.update(b"meshwav-fp-v1");
    h.update(key);
    let d = h.finalize();
    let s = hex(&d[..6]).to_uppercase();
    format!("{}-{}-{}", &s[0..4], &s[4..8], &s[8..12])
}

/// A typed or pasted fingerprint in canonical form (`4F2A-91C0-7B3E`), or `None` if it is not 12 hex digits.
/// Dashes, spaces and case are ignored.
pub fn normalize_fingerprint(s: &str) -> Option<String> {
    let t: String = s.chars().filter(|c| !matches!(c, '-' | ' ' | ':')).collect::<String>().to_uppercase();
    if t.len() != 12 || !t.bytes().all(|b| b.is_ascii_hexdigit()) {
        return None;
    }
    Some(format!("{}-{}-{}", &t[0..4], &t[4..8], &t[8..12]))
}

pub fn valid_code(code: &str) -> bool {
    code.len() == 6 && code.bytes().all(|b| b.is_ascii_digit())
}

/// Keys of an established session. `to_agent` protects receiver->agent packets,
/// `to_receiver` protects agent->receiver packets.
#[derive(Clone, PartialEq, Eq)]
pub struct SessionKeys {
    pub to_agent: [u8; 32],
    pub to_receiver: [u8; 32],
}

impl std::fmt::Debug for SessionKeys {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("SessionKeys(..)")
    }
}

fn build(
    local: &Keypair,
    psk: Option<&[u8; 32]>,
    ephemeral: Option<&[u8; 32]>,
    initiator: bool,
) -> Result<HandshakeState, ProtocolError> {
    let pattern = if psk.is_some() { PATTERN_XXPSK3 } else { PATTERN_XX };
    let err = |_| ProtocolError::Handshake("setup");
    let mut b = Builder::new(pattern.parse().map_err(err)?)
        .prologue(PROLOGUE)
        .map_err(err)?
        .local_private_key(&local.private)
        .map_err(err)?;
    if let Some(p) = psk {
        b = b.psk(3, p).map_err(err)?;
    }
    if let Some(e) = ephemeral {
        b = b.fixed_ephemeral_key_for_testing_only(e);
    }
    if initiator { b.build_initiator() } else { b.build_responder() }.map_err(err)
}

fn raw_split(hs: &mut HandshakeState) -> Result<SessionKeys, ProtocolError> {
    if !hs.is_handshake_finished() {
        return Err(ProtocolError::Handshake("not finished"));
    }
    let (k1, k2) = hs.dangerously_get_raw_split();
    // k1 protects initiator->responder (receiver->agent), k2 the reverse.
    Ok(SessionKeys { to_agent: k1, to_receiver: k2 })
}

fn static_of(hs: &HandshakeState) -> Result<PublicKey, ProtocolError> {
    hs.get_remote_static()
        .and_then(|s| s.try_into().ok())
        .ok_or(ProtocolError::Handshake("no remote static key"))
}

fn clean_name(raw: &[u8]) -> String {
    String::from_utf8_lossy(raw)
        .chars()
        .filter(|c| !c.is_control())
        .take(MAX_DEVICE_NAME)
        .collect()
}

/// Receiver side (phone). Mirrors what the Android implementation does.
pub struct Initiator {
    hs: HandshakeState,
    expect: Option<PublicKey>,
}

impl Initiator {
    /// `expect` pins the agent's static key: a different key aborts before anything about us is sent.
    pub fn new(local: &Keypair, psk: Option<&[u8; 32]>, expect: Option<PublicKey>) -> Result<Self, ProtocolError> {
        Ok(Self { hs: build(local, psk, None, true)?, expect })
    }

    /// Fixed ephemeral key: test vectors only. Never use outside tests.
    pub fn with_fixed_ephemeral_for_tests(
        local: &Keypair,
        psk: Option<&[u8; 32]>,
        expect: Option<PublicKey>,
        ephemeral: &[u8; 32],
    ) -> Result<Self, ProtocolError> {
        Ok(Self { hs: build(local, psk, Some(ephemeral), true)?, expect })
    }

    pub fn write_msg1(&mut self) -> Result<Vec<u8>, ProtocolError> {
        let mut buf = [0u8; 128];
        let n = self.hs.write_message(&[], &mut buf).map_err(|_| ProtocolError::Handshake("msg1"))?;
        Ok(buf[..n].to_vec())
    }

    /// Returns the agent's static key (already checked against the pin, if any).
    pub fn read_msg2(&mut self, msg: &[u8]) -> Result<PublicKey, ProtocolError> {
        let mut buf = [0u8; 256];
        self.hs.read_message(msg, &mut buf).map_err(|_| ProtocolError::Handshake("msg2"))?;
        let remote = static_of(&self.hs)?;
        if let Some(pin) = self.expect {
            if pin != remote {
                return Err(ProtocolError::PeerMismatch);
            }
        }
        Ok(remote)
    }

    pub fn write_msg3(&mut self, device_name: &str) -> Result<Vec<u8>, ProtocolError> {
        let name = clean_name(device_name.as_bytes());
        let mut buf = [0u8; 256];
        let n = self.hs.write_message(name.as_bytes(), &mut buf).map_err(|_| ProtocolError::Handshake("msg3"))?;
        Ok(buf[..n].to_vec())
    }

    pub fn finish(mut self) -> Result<SessionKeys, ProtocolError> {
        raw_split(&mut self.hs)
    }
}

/// Agent side.
pub struct Responder {
    hs: HandshakeState,
}

impl Responder {
    pub fn new(local: &Keypair, psk: Option<&[u8; 32]>) -> Result<Self, ProtocolError> {
        Ok(Self { hs: build(local, psk, None, false)? })
    }

    /// Fixed ephemeral key: test vectors only. Never use outside tests.
    pub fn with_fixed_ephemeral_for_tests(
        local: &Keypair,
        psk: Option<&[u8; 32]>,
        ephemeral: &[u8; 32],
    ) -> Result<Self, ProtocolError> {
        Ok(Self { hs: build(local, psk, Some(ephemeral), false)? })
    }

    pub fn read_msg1(&mut self, msg: &[u8]) -> Result<(), ProtocolError> {
        let mut buf = [0u8; 128];
        self.hs.read_message(msg, &mut buf).map_err(|_| ProtocolError::Handshake("msg1"))?;
        Ok(())
    }

    pub fn write_msg2(&mut self) -> Result<Vec<u8>, ProtocolError> {
        let mut buf = [0u8; 256];
        let n = self.hs.write_message(&[], &mut buf).map_err(|_| ProtocolError::Handshake("msg2"))?;
        Ok(buf[..n].to_vec())
    }

    /// Returns the initiator's static key and its self-reported (sanitised) device name.
    /// A wrong pairing code fails here (the PSK is mixed in at the end of message 3).
    pub fn read_msg3(&mut self, msg: &[u8]) -> Result<(PublicKey, String), ProtocolError> {
        let mut buf = [0u8; 256];
        let n = self.hs.read_message(msg, &mut buf).map_err(|_| ProtocolError::AuthFailed)?;
        Ok((static_of(&self.hs)?, clean_name(&buf[..n])))
    }

    pub fn finish(mut self) -> Result<SessionKeys, ProtocolError> {
        raw_split(&mut self.hs)
    }
}

/// A handshake datagram: `"MW" | 1 | kind | sid:u32 BE | step:u8 | noise message`.
#[derive(Debug, PartialEq, Eq)]
pub struct Frame<'a> {
    pub kind: u8,
    pub sid: u32,
    pub step: u8,
    pub body: &'a [u8],
}

pub fn frame(kind: u8, sid: u32, step: u8, body: &[u8]) -> Vec<u8> {
    let mut v = Vec::with_capacity(FRAME_HEADER + body.len());
    v.extend_from_slice(b"MW");
    v.push(crate::packet::VERSION);
    v.push(kind);
    v.extend_from_slice(&sid.to_be_bytes());
    v.push(step);
    v.extend_from_slice(body);
    v
}

/// True if the datagram is a handshake frame (as opposed to an audio-layer packet).
pub fn is_handshake(d: &[u8]) -> bool {
    d.len() >= FRAME_HEADER && d[0..2] == *b"MW" && d[2] == crate::packet::VERSION && (KIND_XX..=KIND_HELLO).contains(&d[3])
}

/// The datagram a sender with an open pairing window sends to a waiting phone.
pub fn invite_frame(sid: u32, key: &PublicKey, port: u16) -> Vec<u8> {
    let mut body = key.to_vec();
    body.extend_from_slice(&port.to_be_bytes());
    frame(KIND_INVITE, sid, 1, &body)
}

/// The sender's key and port from an invite datagram, or `None` for anything else.
pub fn parse_invite(d: &[u8]) -> Option<(PublicKey, u16)> {
    if d.len() != FRAME_HEADER + 34 || d[0..2] != *b"MW" || d[2] != crate::packet::VERSION || d[3] != KIND_INVITE || d[8] != 1 {
        return None;
    }
    let mut key = [0u8; 32];
    key.copy_from_slice(&d[FRAME_HEADER..FRAME_HEADER + 32]);
    let port = u16::from_be_bytes([d[FRAME_HEADER + 32], d[FRAME_HEADER + 33]]);
    (port != 0).then_some((key, port))
}

pub fn parse_frame(d: &[u8]) -> Option<Frame<'_>> {
    if !is_handshake(d) || d.len() > 512 {
        return None;
    }
    Some(Frame {
        kind: d[3],
        sid: u32::from_be_bytes(d[4..8].try_into().unwrap()),
        step: d[8],
        body: &d[FRAME_HEADER..],
    })
}

#[cfg(test)]
mod invite_tests {
    use super::*;

    #[test]
    fn invite_round_trips_and_rejects_junk() {
        let f = invite_frame(7, &[9; 32], 47900);
        // The same bytes are checked by the Android test (InviteTest.kt), so both sides agree on the layout.
        let hex: String = f.iter().map(|b| format!("{b:02x}")).collect();
        assert_eq!(hex, "4d57011300000007010909090909090909090909090909090909090909090909090909090909090909bb1c");
        assert_eq!(parse_invite(&f), Some(([9; 32], 47900)));
        assert_eq!(parse_invite(&f[..f.len() - 1]), None);
        assert_eq!(parse_invite(&invite_frame(1, &[1; 32], 0)), None);
        let mut h = f.clone();
        h[3] = KIND_HELLO;
        assert_eq!(parse_invite(&h), None);
        // An invite is not a handshake frame, so a sender's handshake server never sees one.
        assert!(!is_handshake(&f));
    }
}
