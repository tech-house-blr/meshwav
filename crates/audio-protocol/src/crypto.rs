//! AEAD framing: ChaCha20-Poly1305, header authenticated as AAD.
//! Nonce = direction(1) | 0(3) | seq(4) | stream_id(4). The key is per session, so a
//! (key, nonce) pair is never reused as long as `seq` is not reused; senders must rekey
//! before `seq` wraps (enforced by `Sealer`).
use crate::packet::{Header, HEADER_LEN, MAX_DATAGRAM, MAX_PAYLOAD, TAG_LEN};
use crate::replay::ReplayWindow;
use crate::ProtocolError;
use chacha20poly1305::aead::{AeadInPlace, KeyInit};
use chacha20poly1305::{ChaCha20Poly1305, Key, Nonce};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u8)]
pub enum Direction {
    AgentToReceiver = 0,
    ReceiverToAgent = 1,
}

fn nonce(dir: Direction, h: &Header) -> Nonce {
    let mut n = [0u8; 12];
    n[0] = dir as u8;
    n[4..8].copy_from_slice(&h.seq.to_be_bytes());
    n[8..12].copy_from_slice(&h.stream_id.to_be_bytes());
    *Nonce::from_slice(&n)
}

/// Encrypts packets and assigns sequence numbers.
pub struct Sealer {
    cipher: ChaCha20Poly1305,
    dir: Direction,
    stream_id: u32,
    next_seq: u32,
    exhausted: bool,
}

impl Sealer {
    pub fn new(key: &[u8; 32], dir: Direction, stream_id: u32, start_seq: u32) -> Self {
        Self {
            cipher: ChaCha20Poly1305::new(Key::from_slice(key)),
            dir,
            stream_id,
            next_seq: start_seq,
            exhausted: false,
        }
    }

    /// Seal one packet; `h.stream_id` and `h.seq` are overwritten. Appends to `out`.
    pub fn seal(
        &mut self,
        mut h: Header,
        payload: &[u8],
        out: &mut Vec<u8>,
    ) -> Result<(), ProtocolError> {
        if payload.len() > MAX_PAYLOAD {
            return Err(ProtocolError::TooLong);
        }
        if self.exhausted {
            return Err(ProtocolError::SeqExhausted);
        }
        h.stream_id = self.stream_id;
        h.seq = self.next_seq;
        match self.next_seq.checked_add(1) {
            Some(n) => self.next_seq = n,
            None => self.exhausted = true, // this packet is the last one allowed
        }
        let hdr = h.encode();
        let start = out.len();
        out.extend_from_slice(&hdr);
        out.extend_from_slice(payload);
        let tag = self
            .cipher
            .encrypt_in_place_detached(&nonce(self.dir, &h), &hdr, &mut out[start + HEADER_LEN..])
            .map_err(|_| ProtocolError::AuthFailed)?;
        out.extend_from_slice(&tag);
        Ok(())
    }
}

/// Authenticates, decrypts and replay-filters packets.
pub struct Opener {
    cipher: ChaCha20Poly1305,
    dir: Direction,
    stream_id: u32,
    replay: ReplayWindow,
}

impl Opener {
    pub fn new(key: &[u8; 32], dir: Direction, stream_id: u32) -> Self {
        Self {
            cipher: ChaCha20Poly1305::new(Key::from_slice(key)),
            dir,
            stream_id,
            replay: ReplayWindow::new(),
        }
    }

    /// Returns the header and decrypted payload. Unauthenticated input never updates state.
    pub fn open(&mut self, datagram: &[u8]) -> Result<(Header, Vec<u8>), ProtocolError> {
        if datagram.len() > MAX_DATAGRAM {
            return Err(ProtocolError::TooLong);
        }
        if datagram.len() < HEADER_LEN + TAG_LEN {
            return Err(ProtocolError::TooShort);
        }
        let h = Header::decode(datagram)?;
        if h.stream_id != self.stream_id {
            return Err(ProtocolError::AuthFailed);
        }
        if !self.replay.would_accept(h.seq) {
            return Err(ProtocolError::Replay);
        }
        let (hdr, rest) = datagram.split_at(HEADER_LEN);
        let (ct, tag) = rest.split_at(rest.len() - TAG_LEN);
        let mut buf = ct.to_vec();
        self.cipher
            .decrypt_in_place_detached(&nonce(self.dir, &h), hdr, &mut buf, tag.into())
            .map_err(|_| ProtocolError::AuthFailed)?;
        self.replay.accept(h.seq);
        Ok((h, buf))
    }
}
