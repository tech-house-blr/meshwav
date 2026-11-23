use audio_protocol::control::{Codec, Message};
use audio_protocol::crypto::{Direction, Opener, Sealer};
use audio_protocol::packet::*;
use audio_protocol::reassembly::Reassembler;
use audio_protocol::replay::ReplayWindow;
use audio_protocol::ProtocolError;

const KEY: [u8; 32] = [7; 32];

fn hdr(ts: u32, idx: u8, cnt: u8) -> Header {
    Header { ptype: PacketType::Audio, stream_id: 0, seq: 0, timestamp: ts, frag_idx: idx, frag_cnt: cnt, flags: 0 }
}

fn pair() -> (Sealer, Opener) {
    (Sealer::new(&KEY, Direction::AgentToReceiver, 42, 0), Opener::new(&KEY, Direction::AgentToReceiver, 42))
}

#[test]
fn header_roundtrip() {
    let h = Header { ptype: PacketType::Parity, stream_id: 1, seq: 2, timestamp: 3, frag_idx: 1, frag_cnt: 4, flags: FLAG_KEYFRAME | FLAG_SILENCE };
    assert_eq!(Header::decode(&h.encode()).unwrap(), h);
}

#[test]
fn header_rejects_garbage() {
    let good = hdr(0, 0, 1).encode();
    assert_eq!(Header::decode(&good[..19]), Err(ProtocolError::TooShort));
    let mut b = good; b[0] = 0;
    assert_eq!(Header::decode(&b), Err(ProtocolError::BadMagic));
    let mut b = good; b[2] = 9;
    assert_eq!(Header::decode(&b), Err(ProtocolError::BadVersion(9)));
    let mut b = good; b[3] = 77;
    assert_eq!(Header::decode(&b), Err(ProtocolError::BadType(77)));
    let mut b = good; b[17] = 0;
    assert_eq!(Header::decode(&b), Err(ProtocolError::BadFragment));
    let mut b = good; b[16] = 1;
    assert_eq!(Header::decode(&b), Err(ProtocolError::BadFragment));
}

#[test]
fn seal_open_roundtrip_and_size() {
    let (mut s, mut o) = pair();
    let mut out = Vec::new();
    s.seal(hdr(480, 0, 1), b"opus-frame", &mut out).unwrap();
    assert_eq!(out.len(), HEADER_LEN + 10 + TAG_LEN);
    let (h, p) = o.open(&out).unwrap();
    assert_eq!((h.seq, h.stream_id, h.timestamp), (0, 42, 480));
    assert_eq!(p, b"opus-frame");
}

#[test]
fn tamper_detected_in_header_payload_and_tag() {
    for pos in [5usize, 9, HEADER_LEN, HEADER_LEN + 3, 20 + 10 + 2] {
        let (mut s, mut o) = pair();
        let mut out = Vec::new();
        s.seal(hdr(0, 0, 1), b"0123456789", &mut out).unwrap();
        out[pos] ^= 1;
        assert!(o.open(&out).is_err(), "tamper at {pos} accepted");
    }
}

#[test]
fn wrong_key_and_wrong_direction_rejected() {
    let (mut s, _) = pair();
    let mut out = Vec::new();
    s.seal(hdr(0, 0, 1), b"x", &mut out).unwrap();
    let mut bad_key = Opener::new(&[8; 32], Direction::AgentToReceiver, 42);
    assert_eq!(bad_key.open(&out).unwrap_err(), ProtocolError::AuthFailed);
    let mut bad_dir = Opener::new(&KEY, Direction::ReceiverToAgent, 42);
    assert_eq!(bad_dir.open(&out).unwrap_err(), ProtocolError::AuthFailed);
    let mut bad_stream = Opener::new(&KEY, Direction::AgentToReceiver, 43);
    assert_eq!(bad_stream.open(&out).unwrap_err(), ProtocolError::AuthFailed);
}

#[test]
fn replay_rejected_and_forgery_does_not_poison_window() {
    let (mut s, mut o) = pair();
    let mut a = Vec::new();
    s.seal(hdr(0, 0, 1), b"a", &mut a).unwrap();
    // forged packet claiming a huge seq must not advance the window
    let mut forged = a.clone();
    forged[8..12].copy_from_slice(&u32::MAX.to_be_bytes());
    assert!(o.open(&forged).is_err());
    assert!(o.open(&a).is_ok());
    assert_eq!(o.open(&a).unwrap_err(), ProtocolError::Replay);
}

#[test]
fn reordering_within_window_ok() {
    let (mut s, mut o) = pair();
    let pk: Vec<Vec<u8>> = (0..5).map(|i| { let mut v = Vec::new(); s.seal(hdr(i * 480, 0, 1), &[i as u8], &mut v).unwrap(); v }).collect();
    for i in [0usize, 2, 1, 4, 3] {
        assert_eq!(o.open(&pk[i]).unwrap().1, vec![i as u8]);
    }
}

#[test]
fn oversize_payload_rejected_and_seq_exhaustion_enforced() {
    let (mut s, _) = pair();
    let mut out = Vec::new();
    assert_eq!(s.seal(hdr(0, 0, 1), &vec![0; MAX_PAYLOAD + 1], &mut out), Err(ProtocolError::TooLong));
    assert!(s.seal(hdr(0, 0, 1), &vec![0; MAX_PAYLOAD], &mut out).is_ok());
    assert!(out.len() <= MAX_DATAGRAM);
    let mut last = Sealer::new(&KEY, Direction::AgentToReceiver, 1, u32::MAX);
    assert!(last.seal(hdr(0, 0, 1), b"x", &mut out).is_ok());
    assert_eq!(last.seal(hdr(0, 0, 1), b"x", &mut out), Err(ProtocolError::SeqExhausted));
}

#[test]
fn replay_window_semantics() {
    let mut w = ReplayWindow::new();
    assert!(w.accept(100));
    assert!(!w.accept(100));
    assert!(w.accept(99));
    assert!(w.accept(100 + 64)); // crosses a word boundary
    assert!(!w.accept(100)); // still remembered
    assert!(!w.accept(99));
    assert!(w.accept(98));
    assert!(w.accept(100 + 2000)); // far jump clears window
    assert!(!w.accept(100)); // too old
    assert!(w.would_accept(100 + 2001));
    assert!(!w.would_accept(100 + 2000));
}

#[test]
fn replay_window_matches_reference_model() {
    use std::collections::HashSet;
    let mut w = ReplayWindow::new();
    let mut seen: HashSet<u32> = HashSet::new();
    let mut highest: Option<u32> = None;
    let mut x: u64 = 0x1234_5678;
    for _ in 0..20000 {
        x = x.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
        let base = highest.unwrap_or(1000);
        let seq = (base as i64 + ((x >> 40) as i64 % 1500) - 700).max(0) as u32;
        let expect = match highest {
            None => true,
            Some(h) => seq > h || (h - seq < 1024 && !seen.contains(&seq)),
        };
        assert_eq!(w.would_accept(seq), expect);
        assert_eq!(w.accept(seq), expect, "seq {seq}");
        if expect {
            seen.insert(seq);
            highest = Some(highest.map_or(seq, |h| h.max(seq)));
        }
    }
}

#[test]
fn fragmentation_roundtrip_with_reordering_and_duplicates() {
    let frame: Vec<u8> = (0..3000u32).map(|i| (i % 251) as u8).collect();
    let frags: Vec<&[u8]> = fragment(&frame).unwrap().collect();
    assert_eq!(frags.len(), 3);
    let mut r = Reassembler::new(4);
    let order = [2usize, 0, 0, 1];
    let mut done = None;
    for &i in &order {
        if let Some(f) = r.push(&hdr(960, i as u8, 3), frags[i].to_vec()) { done = Some(f); }
    }
    let f = done.expect("frame complete");
    assert_eq!(f.data, frame);
    assert_eq!(f.timestamp, 960);
    assert_eq!(r.in_flight(), 0);
}

#[test]
fn reassembly_is_bounded_and_evicts_oldest() {
    let mut r = Reassembler::new(3);
    for ts in 0..10u32 {
        assert!(r.push(&hdr(ts, 0, 2), vec![1]).is_none()); // never completed
        assert!(r.in_flight() <= 3);
    }
    assert_eq!(r.evicted, 7);
    // oldest was evicted, so completing ts=0 starts a fresh slot
    assert!(r.push(&hdr(0, 1, 2), vec![2]).is_none());
}

#[test]
fn fragment_limits() {
    assert!(fragment(&[]).is_none());
    assert!(fragment(&vec![0; MAX_PAYLOAD * 255 + 1]).is_none());
    assert_eq!(fragment(&vec![0; MAX_PAYLOAD * 255]).unwrap().count(), 255);
}

fn samples() -> Vec<Message> {
    vec![
        Message::Hello { version: 1, name: "galaxy-m51".into(), codecs: vec![Codec::Opus, Codec::Flac, Codec::Pcm], max_rate: 192000, max_depth: 24 },
        Message::Offer { name: "atlas".into(), rate: 96000, channels: 2, depth: 24, codecs: vec![Codec::Flac], min_kbps: 64, max_kbps: 256 },
        Message::Accept { codec: Codec::Opus, rate: 48000, depth: 16, frame_us: 10_000, bitrate_kbps: 160, udp_port: 47801 },
        Message::StreamKey { stream_id: 9, key: [3; 32], start_seq: 0 },
        Message::Ping { t_us: 123456789 },
        Message::Pong { echo_us: 1, t_us: 2 },
        Message::Stats { received: 10, lost: 1, late: 2, concealed: 3, buffer_ms: 20, relayed: true },
        Message::Bye { reason: 4 },
        Message::SetConfig { req: 7, kbps: 128, frame_ms: 20, codec: 0 },
        Message::ConfigApplied { req: 7, kbps: 128, frame_ms: 20, codec: 1 },
        Message::ConfigRejected { req: 8, reason: 1 },
        Message::ActiveState { active: true },
        Message::ActiveState { active: false },
    ]
}

#[test]
fn control_roundtrip_and_streaming_decode() {
    let mut stream = Vec::new();
    for m in samples() { stream.extend(m.encode()); }
    let mut buf = &stream[..];
    for expect in samples() {
        let (m, n) = Message::decode(buf).unwrap().unwrap();
        assert_eq!(m, expect);
        buf = &buf[n..];
    }
    assert!(buf.is_empty());
}

#[test]
fn control_partial_input_needs_more() {
    for m in samples() {
        let b = m.encode();
        for cut in 0..b.len() {
            assert_eq!(Message::decode(&b[..cut]).unwrap(), None, "cut {cut}");
        }
    }
}

#[test]
fn control_rejects_malformed() {
    // unknown type
    assert!(Message::decode(&[0, 1, 99]).is_err());
    // zero and oversized length
    assert!(Message::decode(&[0, 0]).is_err());
    assert!(Message::decode(&[0xff, 0xff]).is_err());
    // trailing bytes
    let mut b = Message::Bye { reason: 1 }.encode();
    b.push(0);
    b[1] += 1;
    assert!(Message::decode(&b).is_err());
    // bad bool in Stats
    let mut b = Message::Stats { received: 0, lost: 0, late: 0, concealed: 0, buffer_ms: 0, relayed: false }.encode();
    *b.last_mut().unwrap() = 2;
    assert!(Message::decode(&b).is_err());
}

#[test]
fn control_decoder_never_panics_on_random_input() {
    let mut x: u64 = 99;
    for _ in 0..50000 {
        let mut buf = Vec::new();
        x = x.wrapping_mul(6364136223846793005).wrapping_add(1);
        let len = (x >> 58) as usize + 2;
        for _ in 0..len {
            x = x.wrapping_mul(6364136223846793005).wrapping_add(1);
            buf.push((x >> 56) as u8);
        }
        // force plausible length so the body parser is exercised
        buf[0] = 0;
        buf[1] = (buf.len() - 2) as u8;
        let _ = Message::decode(&buf);
    }
}

#[test]
fn long_names_are_truncated_on_char_boundary() {
    let name = "é".repeat(100);
    let b = Message::Hello { version: 1, name, codecs: vec![], max_rate: 48000, max_depth: 16 }.encode();
    let (m, _) = Message::decode(&b).unwrap().unwrap();
    match m { Message::Hello { name, .. } => assert!(name.len() <= 64 && name.chars().all(|c| c == 'é')), _ => unreachable!() }
}

/// Cross-language golden vector (also documented in docs/vectors/packet_v1.txt).
/// The Android implementation must decrypt exactly these bytes.
#[test]
fn golden_vector() {
    let key: [u8; 32] = core::array::from_fn(|i| i as u8);
    let mut s = Sealer::new(&key, Direction::AgentToReceiver, 0x0102_0304, 5);
    let h = Header { ptype: PacketType::Audio, stream_id: 0, seq: 0, timestamp: 480, frag_idx: 0, frag_cnt: 1, flags: FLAG_KEYFRAME };
    let mut out = Vec::new();
    s.seal(h, b"meshwav", &mut out).unwrap();
    let hex: String = out.iter().map(|b| format!("{b:02x}")).collect();
    assert_eq!(hex, GOLDEN);
}
const GOLDEN: &str = "4d5701010102030400000005000001e0000100016c2d66a77895e6de6ab18ab34b758807a42b8c6bed4fb0";

#[test]
fn frame_flags_roundtrip_and_reject_garbage() {
    use audio_protocol::packet::{frame_flags, frame_ms_from_flags, FLAG_KEYFRAME, FLAG_SILENCE};
    for ms in [5u32, 10, 20] {
        assert_eq!(frame_ms_from_flags(frame_flags(ms)), ms);
        // other flag bits do not disturb it
        assert_eq!(frame_ms_from_flags(frame_flags(ms) | FLAG_KEYFRAME | FLAG_SILENCE), ms);
    }
    for ms in [0u32, 1, 2, 3, 15, 40, 60] {
        assert_eq!(frame_flags(ms), 0);
    }
    for code in [0u16, 3, 5, 6, 7] {
        assert_eq!(frame_ms_from_flags(code << 2), 0, "code {code}");
    }
}

#[test]
fn control_packet_type_is_sealed_and_authenticated() {
    let key = [9u8; 32];
    let mut s = Sealer::new(&key, Direction::ReceiverToAgent, 2, 0);
    let mut o = Opener::new(&key, Direction::ReceiverToAgent, 2);
    let body = Message::SetConfig { req: 1, kbps: 96, frame_ms: 5, codec: 0 }.encode();
    let h = Header { ptype: PacketType::Control, stream_id: 0, seq: 0, timestamp: 0, frag_idx: 0, frag_cnt: 1, flags: 0 };
    let mut out = Vec::new();
    s.seal(h, &body, &mut out).unwrap();
    let (hh, pt) = o.open(&out).unwrap();
    assert_eq!(hh.ptype, PacketType::Control);
    assert_eq!(Message::decode(&pt).unwrap().unwrap().0, Message::SetConfig { req: 1, kbps: 96, frame_ms: 5, codec: 0 });
    // a flipped bit anywhere fails authentication
    let mut bad = out.clone();
    bad[25] ^= 1;
    assert!(Opener::new(&key, Direction::ReceiverToAgent, 2).open(&bad).is_err());
}

/// The same literals are asserted by the Kotlin ControlTest, which keeps both parsers on identical bytes.
#[test]
fn runtime_config_messages_have_fixed_wire_bytes() {
    let hex = |m: Message| m.encode().iter().map(|b| format!("{b:02x}")).collect::<String>();
    assert_eq!(hex(Message::SetConfig { req: 7, kbps: 128, frame_ms: 20, codec: 0 }), "00090b000700000080".to_string() + "1400");
    assert_eq!(hex(Message::ConfigApplied { req: 7, kbps: 128, frame_ms: 20, codec: 1 }), "00090c000700000080".to_string() + "1401");
    assert_eq!(hex(Message::SetConfig { req: 1, kbps: 0, frame_ms: 0, codec: 2 }), "00090b0001000000000002");
    assert_eq!(hex(Message::ConfigApplied { req: 1, kbps: 160, frame_ms: 10, codec: 2 }), "00090c0001000000a00a02");
    assert_eq!(hex(Message::ConfigRejected { req: 8, reason: 1 }), "00040d000801");
    assert_eq!(hex(Message::ActiveState { active: true }), "00020e01");
    assert_eq!(hex(Message::ActiveState { active: false }), "00020e00");
}

#[test]
fn active_state_rejects_non_bool() {
    assert!(Message::decode(&[0, 2, 14, 2]).is_err());
}
