//! Malformed-input robustness: every parser that sees network bytes must return an error or
//! ignore the input, never panic or grow without bound. Deterministic (fixed seeds), so it runs in CI.

use audio_protocol::clocksync::ClockSync;
use audio_protocol::control::Message;
use audio_protocol::crypto::{Direction, Opener, Sealer};
use audio_protocol::handshake::{self, Initiator, Keypair, Responder};
use audio_protocol::packet::{fragment, Header, PacketType, HEADER_LEN};
use audio_protocol::reassembly::Reassembler;
use audio_protocol::server::{Authorized, Server};
use std::net::SocketAddr;

struct Rng(u64);
impl Rng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }
    fn bytes(&mut self, max: usize) -> Vec<u8> {
        let n = (self.next() as usize) % (max + 1);
        (0..n).map(|_| self.next() as u8).collect()
    }
    /// Flip, insert, delete or truncate a valid input.
    fn mutate(&mut self, base: &[u8]) -> Vec<u8> {
        let mut v = base.to_vec();
        for _ in 0..(1 + self.next() % 4) {
            match self.next() % 5 {
                0 if !v.is_empty() => { let i = self.next() as usize % v.len(); v[i] ^= 1 << (self.next() % 8); }
                1 if !v.is_empty() => { let i = self.next() as usize % v.len(); v[i] = self.next() as u8; }
                2 => { let i = self.next() as usize % (v.len() + 1); v.insert(i, self.next() as u8); }
                3 if !v.is_empty() => { let i = self.next() as usize % v.len(); v.remove(i); }
                _ => { let n = self.next() as usize % (v.len() + 1); v.truncate(n); }
            }
        }
        v
    }
}

const ROUNDS: usize = 20_000;

fn corpus_messages() -> Vec<Vec<u8>> {
    use audio_protocol::control::Codec;
    let _ = Codec::Opus;
    vec![
        Message::SenderStopped.encode(),
        Message::VolumeLevel { percent: 40 }.encode(),
        Message::SetVolume { percent: 40 }.encode(),
        Message::PlayHere.encode(),
        Message::PlayHereSupported.encode(),
        Message::Bye { reason: 1 }.encode(),
    ]
}

#[test]
fn control_decode_never_panics() {
    let mut r = Rng(0x9E3779B97F4A7C15);
    let corpus = corpus_messages();
    for i in 0..ROUNDS {
        let _ = Message::decode(&r.bytes(80));
        let _ = Message::decode(&r.mutate(&corpus[i % corpus.len()]));
    }
    // A stream of messages is consumed one at a time without looping forever.
    let mut buf: Vec<u8> = corpus.concat();
    while let Ok(Some((_, n))) = Message::decode(&buf) {
        assert!(n > 0 && n <= buf.len());
        buf.drain(..n);
    }
    assert!(buf.is_empty());
}

#[test]
fn header_and_frames_never_panic() {
    let mut r = Rng(0xD1B54A32D192ED03);
    let h = Header { ptype: PacketType::Audio, flags: 0, stream_id: 7, seq: 9, timestamp: 11, frag_idx: 0, frag_cnt: 1 };
    let valid = h.encode();
    assert_eq!(valid.len(), HEADER_LEN);
    let hello = handshake::frame(1, 5, 1, &[1, 2, 3]);
    let invite = handshake::invite_frame(3, &Keypair::generate().public, 47900);
    for _ in 0..ROUNDS {
        let _ = Header::decode(&r.bytes(40));
        let _ = Header::decode(&r.mutate(&valid));
        let _ = handshake::parse_frame(&r.bytes(200));
        let _ = handshake::parse_frame(&r.mutate(&hello));
        let _ = handshake::parse_invite(&r.bytes(80));
        let _ = handshake::parse_invite(&r.mutate(&invite));
        let _ = handshake::is_handshake(&r.bytes(20));
        let _ = handshake::normalize_fingerprint(&String::from_utf8_lossy(&r.bytes(40)));
        let _ = handshake::valid_code(&String::from_utf8_lossy(&r.bytes(12)));
    }
}

#[test]
fn opener_rejects_garbage_and_keeps_working() {
    let mut r = Rng(0xA0761D6478BD642F);
    let key = [7u8; 32];
    let mut sealer = Sealer::new(&key, Direction::AgentToReceiver, 42, 0);
    let mut opener = Opener::new(&key, Direction::AgentToReceiver, 42);
    let h = Header { ptype: PacketType::Audio, flags: 0, stream_id: 0, seq: 0, timestamp: 1, frag_idx: 0, frag_cnt: 1 };
    let mut good = Vec::new();
    sealer.seal(h, b"hello audio", &mut good).unwrap();
    for _ in 0..ROUNDS {
        let _ = opener.open(&r.bytes(1600));
        let bad = r.mutate(&good);
        if bad != good {
            assert!(opener.open(&bad).is_err(), "a modified packet must not authenticate");
        }
    }
    // Garbage never used up the replay window: the genuine packet still opens, once.
    assert_eq!(opener.open(&good).unwrap().1, b"hello audio");
    assert!(opener.open(&good).is_err());
}

#[test]
fn reassembler_stays_bounded() {
    let mut r = Rng(0xE7037ED1A0B428DB);
    let mut re = Reassembler::new(8);
    for i in 0..ROUNDS {
        let h = Header {
            ptype: PacketType::Audio,
            flags: r.next() as u16,
            stream_id: 1,
            seq: i as u32,
            timestamp: r.next() as u32 % 50,
            frag_idx: r.next() as u8,
            frag_cnt: r.next() as u8,
        };
        let _ = re.push(&h, r.bytes(1200));
        assert!(re.in_flight() <= 8 + 1, "in-flight frames must stay at the cap");
    }
    assert!(fragment(&vec![0u8; 1_000_000]).is_none());
    assert!(fragment(&[]).is_none());
}

#[test]
fn clocksync_handles_extreme_values() {
    let mut r = Rng(0x8EBC6AF09C88C6E3);
    let mut c = ClockSync::new();
    for _ in 0..ROUNDS {
        c.on_pong(r.next(), r.next(), r.next());
        c.on_timeref(r.next() as u32, r.next());
        let _ = c.captured_at_local_us(r.next() as u32);
        let _ = c.error_us();
        let _ = c.ready();
    }
}

fn peer(n: u32) -> SocketAddr {
    format!("100.64.{}.{}:4000", (n >> 8) & 255, n & 255).parse().unwrap()
}

#[test]
fn server_survives_random_and_mutated_handshakes() {
    let mut r = Rng(0x589965CC75374CC3);
    let agent = Keypair::generate();
    let phone = Keypair::generate();
    let mut srv = Server::new(agent, vec![Authorized { public: phone.public, name: "phone".into() }]);
    assert!(srv.open_pairing("123456", 0));
    // A real handshake provides valid frames to mutate.
    let mut ini = Initiator::new(&phone, None, Some(srv.public_key())).unwrap();
    let m1 = handshake::frame(handshake::KIND_XX, 9, 1, &ini.write_msg1().unwrap());
    let mut now = 1u64;
    for i in 0..ROUNDS as u32 {
        now += 1 + r.next() % 40;
        let from = peer(i % 3000);
        let dg = if i % 2 == 0 { r.bytes(300) } else { r.mutate(&m1) };
        let _ = srv.handle(from, &dg, now);
    }
    // Flooded with garbage, the server still completes a genuine handshake from a fresh address.
    now += 120_000;
    let mut ini = Initiator::new(&phone, None, Some(srv.public_key())).unwrap();
    let out = srv.handle(peer(60000), &handshake::frame(handshake::KIND_XX, 77, 1, &ini.write_msg1().unwrap()), now);
    assert!(matches!(out, audio_protocol::server::Outcome::Reply(_)), "the server still answers a real device after a flood");
}

#[test]
fn responder_and_initiator_reject_garbage_messages() {
    let mut r = Rng(0xF1357AEA2E62A9C5);
    let kp = Keypair::generate();
    for _ in 0..2000 {
        let mut res = Responder::new(&kp, None).unwrap();
        let _ = res.read_msg1(&r.bytes(200));
        let _ = res.write_msg2();
        let _ = res.read_msg3(&r.bytes(200));
        let mut ini = Initiator::new(&Keypair::generate(), None, None).unwrap();
        let _ = ini.write_msg1();
        let _ = ini.read_msg2(&r.bytes(200));
        let _ = ini.read_msg2_named(&r.bytes(200));
        let _ = ini.write_msg3("x");
    }
}
