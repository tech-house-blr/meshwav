//! End-to-end: the real agent binary (with a fake `parec`) against a test receiver that speaks the
//! same handshake the phone does. Covers pairing, audio under session keys, reconnect after an
//! agent restart, wrong code, unpaired device, and a second pairing via the stdin command.
use audio_protocol::crypto::{Direction, Opener, Sealer};
use audio_protocol::handshake::*;
use audio_protocol::packet::{Header, PacketType};
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
