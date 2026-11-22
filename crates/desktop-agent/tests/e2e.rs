//! End-to-end: the real agent binary (with a fake `parec`) against a test receiver that speaks the
//! same handshake the phone does. Covers pairing, audio under session keys, reconnect after an
//! agent restart, wrong code, unpaired device, and a second pairing via the stdin command.
use audio_protocol::crypto::{Direction, Opener, Sealer};
use audio_protocol::handshake::*;
use audio_protocol::control::Message;
use audio_protocol::packet::{frame_ms_from_flags, Header, PacketType};
use std::io::{BufRead, BufReader, Write};
use std::net::UdpSocket;
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::mpsc::{channel, Receiver};
use std::time::{Duration, Instant};

struct Agent {
    child: Child,
    stdin: ChildStdin,
    lines: Receiver<String>,
    port: u16,
}

impl Drop for Agent {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

fn workdir(name: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("mw-e2e-{}-{name}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(d.join("bin")).unwrap();
    let parec = d.join("bin/parec");
    std::fs::write(&parec, "#!/bin/sh\nwhile true; do head -c 1920 /dev/zero; sleep 0.01; done\n").unwrap();
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(&parec, std::fs::Permissions::from_mode(0o755)).unwrap();
    d
}

fn free_port() -> u16 {
    UdpSocket::bind("127.0.0.1:0").unwrap().local_addr().unwrap().port()
}

fn start(dir: &Path, pair: bool, port: u16) -> Agent {
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_meshwav-agent"));
    cmd.env("HOME", dir)
        .env("PATH", format!("{}:{}", dir.join("bin").display(), std::env::var("PATH").unwrap()))
        .args(["--bind", &format!("127.0.0.1:{port}"), "--source", "fake.monitor"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    if pair {
        cmd.arg("--pair");
    }
    let mut child = cmd.spawn().unwrap();
    let (tx, rx) = channel();
    for r in [Box::new(child.stdout.take().unwrap()) as Box<dyn std::io::Read + Send>, Box::new(child.stderr.take().unwrap())] {
        let tx = tx.clone();
        std::thread::spawn(move || {
            for l in BufReader::new(r).lines().map_while(Result::ok) {
                let _ = tx.send(l);
            }
        });
    }
    let stdin = child.stdin.take().unwrap();
    Agent { child, stdin, lines: rx, port }
}

impl Agent {
    fn wait_line(&self, needle: &str) -> String {
        let end = Instant::now() + Duration::from_secs(10);
        while Instant::now() < end {
            if let Ok(l) = self.lines.recv_timeout(Duration::from_millis(200)) {
                if l.contains(needle) {
                    return l;
                }
            }
        }
        panic!("agent never printed {needle:?}");
    }

    /// (public key, code) from the `pair:` line.
    fn pairing(&self) -> (PublicKey, String) {
        let l = self.wait_line("pair: meshwav://pair?");
        let q = l.split('?').nth(1).unwrap();
        let get = |k: &str| q.split('&').find_map(|p| p.strip_prefix(&format!("{k}="))).unwrap().to_string();
        let mut pk = [0u8; 32];
        let h = get("pk");
        for i in 0..32 {
            pk[i] = u8::from_str_radix(&h[2 * i..2 * i + 2], 16).unwrap();
        }
        (pk, get("code"))
    }
}

struct Device {
    sock: UdpSocket,
    keys: SessionKeys,
}

fn dial(port: u16, dev: &Keypair, pin: PublicKey, psk: Option<[u8; 32]>, sid: u32) -> Option<Device> {
    let sock = UdpSocket::bind("127.0.0.1:0").unwrap();
    sock.connect(("127.0.0.1", port)).unwrap();
    sock.set_read_timeout(Some(Duration::from_millis(400))).unwrap();
    let kind = if psk.is_some() { KIND_XXPSK3 } else { KIND_XX };
    let mut ini = Initiator::new(dev, psk.as_ref(), Some(pin)).unwrap();
    let m1 = frame(kind, sid, 1, &ini.write_msg1().unwrap());
    let mut buf = [0u8; 2048];
    for _ in 0..4 {
        sock.send(&m1).unwrap();
        if let Ok(n) = sock.recv(&mut buf) {
            let f = parse_frame(&buf[..n])?;
            ini.read_msg2(f.body).ok()?;
            let m3 = frame(kind, sid, 3, &ini.write_msg3("e2e phone").unwrap());
            sock.send(&m3).unwrap();
            return Some(Device { sock, keys: ini.finish().unwrap() });
        }
    }
    None
}

impl Device {
    /// Punch once, then wait up to `secs` for an authentic audio packet.
    fn hear_audio(&self, secs: u64) -> bool {
        let mut sealer = Sealer::new(&self.keys.to_agent, Direction::ReceiverToAgent, 1, 0);
        let mut opener = Opener::new(&self.keys.to_receiver, Direction::AgentToReceiver, 1);
        let h = Header { ptype: PacketType::Punch, stream_id: 0, seq: 0, timestamp: 0, frag_idx: 0, frag_cnt: 1, flags: 0 };
        let mut out = Vec::new();
        sealer.seal(h, &[], &mut out).unwrap();
        self.sock.send(&out).unwrap();
        let end = Instant::now() + Duration::from_secs(secs);
        let mut buf = [0u8; 2048];
        while Instant::now() < end {
            if let Ok(n) = self.sock.recv(&mut buf) {
                if let Ok((h, _)) = opener.open(&buf[..n]) {
                    if h.ptype == PacketType::Audio {
                        return true;
                    }
                }
            }
        }
        false
    }
}

impl Device {
    /// Listen for `secs`; returns (audio seqs in arrival order, number of authentic control keepalives).
    fn listen(&self, secs: u64) -> (Vec<u32>, u32) {
        let mut audio = Opener::new(&self.keys.to_receiver, Direction::AgentToReceiver, 1);
        let mut ctl = Opener::new(&self.keys.to_receiver, Direction::AgentToReceiver, 2);
        let mut sealer = Sealer::new(&self.keys.to_agent, Direction::ReceiverToAgent, 1, 0);
        let h = Header { ptype: PacketType::Punch, stream_id: 0, seq: 0, timestamp: 0, frag_idx: 0, frag_cnt: 1, flags: 0 };
        let (mut seqs, mut ka) = (vec![], 0);
        let mut buf = [0u8; 2048];
        let end = Instant::now() + Duration::from_secs(secs);
        let mut next_punch = Instant::now();
        while Instant::now() < end {
            if Instant::now() >= next_punch {
                let mut out = Vec::new();
                sealer.seal(h, &[], &mut out).unwrap();
                self.sock.send(&out).unwrap();
                next_punch += Duration::from_millis(500);
            }
            if let Ok(n) = self.sock.recv(&mut buf) {
                if let Ok((h, _)) = audio.open(&buf[..n]) {
                    if h.ptype == PacketType::Audio {
                        seqs.push(h.seq);
                    }
                } else if let Ok((h, _)) = ctl.open(&buf[..n]) {
                    if h.ptype == PacketType::Punch {
                        ka += 1;
                    }
                }
            }
        }
        (seqs, ka)
    }
}

fn authorized_file(dir: &Path) -> String {
    std::fs::read_to_string(dir.join(".config/meshwav/authorized_peers")).unwrap_or_default()
}

#[test]
fn pair_stream_restart_reconnect() {
    let dir = workdir("pair");
    let port = free_port();
    let dev = Keypair::generate();
    let agent = start(&dir, true, port);
    let (pk, code) = agent.pairing();
    assert!(valid_code(&code));
    let d = dial(port, &dev, pk, Some(pairing_psk(&pk, &code)), 1).expect("pairing handshake");
    let (seqs, keepalives) = d.listen(3);
    assert!(seqs.len() > 50, "audio flows: {}", seqs.len());
    assert!(keepalives >= 2, "agent keepalives (incl. the handshake ack) arrive: {keepalives}");
    assert!(seqs.windows(2).all(|w| w[1] == w[0] + 1), "audio seqs stay gap-free while keepalives interleave");
    agent.wait_line("paired new device");
    let auth = authorized_file(&dir);
    assert!(auth.contains(&hex(&dev.public)) && auth.contains("e2e phone"), "{auth}");
    use std::os::unix::fs::PermissionsExt;
    let mode = std::fs::metadata(dir.join(".config/meshwav/authorized_peers")).unwrap().permissions().mode();
    assert_eq!(mode & 0o777, 0o600);
    drop(d);
    drop(agent);

    // restart without --pair: identity and pairing persist, a plain XX handshake works
    let agent2 = start(&dir, false, port);
    agent2.wait_line("paired devices: 1");
    let d2 = dial(port, &dev, pk, None, 2).expect("reconnect handshake");
    assert!(d2.hear_audio(5), "no audio after reconnect");
    agent2.wait_line("device 'e2e phone'");
    // the code from before is dead: no pairing window after restart
    assert!(dial(port, &Keypair::generate(), pk, Some(pairing_psk(&pk, &code)), 3).is_none(), "stale code got a reply");
}

#[test]
fn wrong_code_and_unpaired_devices_get_no_audio() {
    let dir = workdir("deny");
    let port = free_port();
    let agent = start(&dir, true, port);
    let (pk, _code) = agent.pairing();
    let bad = dial(port, &Keypair::generate(), pk, Some(pairing_psk(&pk, "000000")), 1).expect("msg2 still arrives");
    assert!(!bad.hear_audio(2), "wrong code must not stream");
    agent.wait_line("wrong pairing code");
    assert!(authorized_file(&dir).is_empty() || !authorized_file(&dir).contains("e2e phone"));
    let stranger = dial(port, &Keypair::generate(), pk, None, 2).expect("xx msg2");
    assert!(!stranger.hear_audio(2), "unpaired device must not stream");
    agent.wait_line("device is not paired");
}

#[test]
fn pin_mismatch_aborts_and_second_pairing_via_stdin() {
    let dir = workdir("second");
    let port = free_port();
    let mut agent = start(&dir, true, port);
    let (pk, code) = agent.pairing();
    let a = Keypair::generate();
    // a receiver that pinned a different agent key refuses before sending msg3
    assert!(dial(port, &a, Keypair::generate().public, Some(pairing_psk(&pk, &code)), 1).is_none());
    let da = dial(port, &a, pk, Some(pairing_psk(&pk, &code)), 2).unwrap();
    assert!(da.hear_audio(5));
    // the code was single-use; open a new window from stdin for a second device
    writeln!(agent.stdin, "pair").unwrap();
    agent.stdin.flush().unwrap();
    let (_, code2) = agent.pairing();
    assert_ne!(code, code2);
    let b = Keypair::generate();
    let db = dial(port, &b, pk, Some(pairing_psk(&pk, &code2)), 3).unwrap();
    assert!(db.hear_audio(5));
    let auth = authorized_file(&dir);
    assert!(auth.contains(&hex(&a.public)) && auth.contains(&hex(&b.public)), "{auth}");
    assert_eq!(agent.port, port);
}

enum Ev {
    Audio { seq: u32, frame_ms: u32 },
    Ctl(Message),
}

/// A receiver link: punches every 500 ms, can send control requests, reports what arrives.
struct Link<'a> {
    d: &'a Device,
    audio: Opener,
    ctl: Opener,
    punch: Sealer,
    req: Sealer,
    next_punch: Instant,
}

impl<'a> Link<'a> {
    fn new(d: &'a Device) -> Self {
        Link {
            d,
            audio: Opener::new(&d.keys.to_receiver, Direction::AgentToReceiver, 1),
            ctl: Opener::new(&d.keys.to_receiver, Direction::AgentToReceiver, 2),
            punch: Sealer::new(&d.keys.to_agent, Direction::ReceiverToAgent, 1, 0),
            req: Sealer::new(&d.keys.to_agent, Direction::ReceiverToAgent, 2, 0),
            next_punch: Instant::now(),
        }
    }

    fn send(&mut self, m: &Message) {
        let h = Header { ptype: PacketType::Control, stream_id: 0, seq: 0, timestamp: 0, frag_idx: 0, frag_cnt: 1, flags: 0 };
        let mut out = Vec::new();
        self.req.seal(h, &m.encode(), &mut out).unwrap();
        self.d.sock.send(&out).unwrap();
    }

    /// Collect events for `ms` milliseconds.
    fn pump(&mut self, ms: u64) -> Vec<Ev> {
        let h = Header { ptype: PacketType::Punch, stream_id: 0, seq: 0, timestamp: 0, frag_idx: 0, frag_cnt: 1, flags: 0 };
        let end = Instant::now() + Duration::from_millis(ms);
        let (mut evs, mut buf) = (vec![], [0u8; 2048]);
        while Instant::now() < end {
            if Instant::now() >= self.next_punch {
                let mut out = Vec::new();
                self.punch.seal(h, &[], &mut out).unwrap();
                self.d.sock.send(&out).unwrap();
                self.next_punch = Instant::now() + Duration::from_millis(500);
            }
            let Ok(n) = self.d.sock.recv(&mut buf) else { continue };
            if let Ok((h, _)) = self.audio.open(&buf[..n]) {
                if h.ptype == PacketType::Audio {
                    evs.push(Ev::Audio { seq: h.seq, frame_ms: frame_ms_from_flags(h.flags) });
                }
            } else if let Ok((h, body)) = self.ctl.open(&buf[..n]) {
                if h.ptype == PacketType::Control {
                    evs.push(Ev::Ctl(Message::decode(&body).unwrap().unwrap().0));
                }
            }
        }
        evs
    }

    /// Send `m` (re-sending every 300 ms like the phone) until a reply for `req` arrives.
    /// Returns the reply and the audio events that arrived after it.
    fn request(&mut self, m: Message, req: u16) -> (Message, Vec<Ev>) {
        for _ in 0..10 {
            self.send(&m);
            let mut evs = self.pump(300);
            if let Some(i) = evs.iter().position(|e| matches!(e, Ev::Ctl(Message::ConfigApplied { req: r, .. } | Message::ConfigRejected { req: r, .. }) if *r == req)) {
                let after = evs.split_off(i + 1);
                let Some(Ev::Ctl(reply)) = evs.pop() else { unreachable!() };
                return (reply, after);
            }
        }
        panic!("no reply to request {req}");
    }
}

fn frame_sizes(evs: &[Ev]) -> Vec<u32> {
    evs.iter().filter_map(|e| if let Ev::Audio { frame_ms, .. } = e { Some(*frame_ms) } else { None }).collect()
}

fn seqs(evs: &[Ev]) -> Vec<u32> {
    evs.iter().filter_map(|e| if let Ev::Audio { seq, .. } = e { Some(*seq) } else { None }).collect()
}

#[test]
fn runtime_bitrate_and_frame_size_changes() {
    let dir = workdir("config");
    let port = free_port();
    let dev = Keypair::generate();
    let agent = start(&dir, true, port);
    let (pk, code) = agent.pairing();
    let d = dial(port, &dev, pk, Some(pairing_psk(&pk, &code)), 1).unwrap();
    let mut l = Link::new(&d);

    let evs = l.pump(1000);
    assert!(frame_sizes(&evs).len() > 50 && frame_sizes(&evs).iter().all(|&f| f == 10), "default is 10 ms frames");
    let mut last_seq = *seqs(&evs).last().unwrap();

    // switch to 20 ms frames at 96 kbit/s
    let (reply, after) = l.request(Message::SetConfig { req: 1, kbps: 96, frame_ms: 20 }, 1);
    assert_eq!(reply, Message::ConfigApplied { req: 1, kbps: 96, frame_ms: 20 });
    agent.wait_line("receiver set 96 kbit/s, 20 ms frames");
    let mut evs = after;
    evs.extend(l.pump(1000));
    let fs = frame_sizes(&evs);
    assert!(fs.len() > 20 && fs.iter().all(|&f| f == 20), "every packet after the ack announces 20 ms: {fs:?}");
    assert!(fs.len() < 70, "20 ms frames mean about 50 packets per second, got {}", fs.len());
    let sq = seqs(&evs);
    assert!(sq.windows(2).all(|w| w[1] == w[0] + 1), "no gap in sequence numbers across the switch");
    assert!(sq[0] > last_seq, "same session, same sequence space");
    last_seq = *sq.last().unwrap();

    // a repeated request id is answered again, not applied again
    l.send(&Message::SetConfig { req: 1, kbps: 96, frame_ms: 20 });
    let again = l.pump(600);
    assert!(again.iter().any(|e| matches!(e, Ev::Ctl(Message::ConfigApplied { req: 1, kbps: 96, frame_ms: 20 }))), "reply replayed");

    // out-of-range values are refused and change nothing
    let (reply, after) = l.request(Message::SetConfig { req: 2, kbps: 999, frame_ms: 0 }, 2);
    assert_eq!(reply, Message::ConfigRejected { req: 2, reason: audio_protocol::control::REJECT_BITRATE });
    let (reply, _) = l.request(Message::SetConfig { req: 3, kbps: 0, frame_ms: 40 }, 3);
    assert_eq!(reply, Message::ConfigRejected { req: 3, reason: audio_protocol::control::REJECT_FRAME });
    let mut evs = after;
    evs.extend(l.pump(500));
    assert!(frame_sizes(&evs).iter().all(|&f| f == 20), "still 20 ms after rejected requests");

    // bitrate only (0 keeps the frame size), then 5 ms frames (0 keeps the bitrate)
    let (reply, _) = l.request(Message::SetConfig { req: 4, kbps: 200, frame_ms: 0 }, 4);
    assert_eq!(reply, Message::ConfigApplied { req: 4, kbps: 200, frame_ms: 20 });
    let (reply, after) = l.request(Message::SetConfig { req: 5, kbps: 0, frame_ms: 5 }, 5);
    assert_eq!(reply, Message::ConfigApplied { req: 5, kbps: 200, frame_ms: 5 });
    let mut evs = after;
    evs.extend(l.pump(1000));
    let fs = frame_sizes(&evs);
    assert!(fs.len() > 100 && fs.iter().all(|&f| f == 5), "5 ms frames: {} packets", fs.len());
    assert!(seqs(&evs)[0] > last_seq, "sequence numbers keep growing");
}

#[test]
fn control_requests_from_an_unpaired_device_do_nothing() {
    let dir = workdir("ctlauth");
    let port = free_port();
    let agent = start(&dir, true, port);
    let (pk, code) = agent.pairing();
    let owner = dial(port, &Keypair::generate(), pk, Some(pairing_psk(&pk, &code)), 1).unwrap();
    let mut lo = Link::new(&owner);
    assert!(!lo.pump(500).is_empty());
    // a stranger completes XX msg2 but is not authorised, and a forged request with a wrong key goes nowhere
    let stranger = dial(port, &Keypair::generate(), pk, None, 2).unwrap();
    let mut ls = Link::new(&stranger);
    ls.send(&Message::SetConfig { req: 1, kbps: 64, frame_ms: 5 });
    assert!(ls.pump(800).is_empty(), "stranger gets nothing, not even a rejection");
    let fs = frame_sizes(&lo.pump(500));
    assert!(fs.iter().all(|&f| f == 10), "owner's stream unchanged");
}

#[test]
fn goodbye_stops_the_stream_at_once() {
    let dir = workdir("bye");
    let port = free_port();
    let dev = Keypair::generate();
    let agent = start(&dir, true, port);
    let (pk, code) = agent.pairing();
    let d = dial(port, &dev, pk, Some(pairing_psk(&pk, &code)), 1).unwrap();
    let mut l = Link::new(&d);
    assert!(!frame_sizes(&l.pump(500)).is_empty());
    l.send(&Message::Bye { reason: 0 });
    agent.wait_line("receiver said goodbye");
    l.pump(100); // drain what was already in flight
    assert!(frame_sizes(&l.pump(700)).is_empty(), "no audio after goodbye, even though punches keep arriving");
}
