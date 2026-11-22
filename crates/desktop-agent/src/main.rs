//! meshwav-agent: capture system audio, encode Opus, send AEAD-sealed UDP to the paired receiver
//! that most recently completed a Noise handshake. The receiver keeps the session alive with
//! sealed `Punch` packets about once a second; the agent stops sending after `PEER_TIMEOUT`.
//!
//!   meshwav-agent [--pair] [--bind IP:PORT] [--identity PATH] [--authorized PATH] [--source SINK.monitor] [--kbps 160] [--frame-ms 10]
//!
//! Pairing: `--pair` (or typing `pair` on stdin) opens a 2-minute window with a 6-digit code.
//! Defaults: bind = <tailscale ipv4>:47900, identity = ~/.config/meshwav/identity and paired
//! devices = ~/.config/meshwav/authorized_peers (both 0600, created on first use). The listener
//! refuses non-tailnet addresses. Capture uses `parec` (PulseAudio or
//! PipeWire-pulse) on the default sink's monitor.
mod store;

use audio_codec::opus::OpusEncoder;
use audio_protocol::crypto::{Direction, Opener, Sealer};
use audio_protocol::handshake::{self, hex};
use audio_protocol::packet::{Header, PacketType};
use audio_protocol::server::{Outcome, Server};
use audio_transport::is_tailnet_or_loopback;
use std::io::{BufRead, Read};
use std::net::{SocketAddr, UdpSocket};
use std::process::{Command, Stdio};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

const STREAM_ID: u32 = 1;
/// Keepalives use their own stream id (own nonce/seq space) so audio sequence numbers stay gap-free.
const CONTROL_STREAM: u32 = 2;
const KEEPALIVE: Duration = Duration::from_secs(1);
const PEER_TIMEOUT: Duration = Duration::from_secs(5);
const DEFAULT_PORT: u16 = 47900;

/// The one active receiver session. A new successful handshake replaces it.
struct Session {
    addr: SocketAddr,
    peer: [u8; 32],
    last_seen: Instant,
    sealer: Sealer,
    keepalive: Sealer,
    opener: Opener,
}

impl Session {
    /// Sealed empty `Punch` on the control stream: proves liveness (and the handshake) to the receiver.
    fn send_keepalive(&mut self, sock: &UdpSocket) {
        let h = Header {
            ptype: PacketType::Punch,
            stream_id: 0,
            seq: 0,
            timestamp: 0,
            frag_idx: 0,
            frag_cnt: 1,
            flags: 0,
        };
        let mut out = Vec::with_capacity(64);
        if self.keepalive.seal(h, &[], &mut out).is_ok() {
            let _ = sock.send_to(&out, self.addr);
        }
    }
}

fn arg(args: &[String], name: &str) -> Option<String> {
    args.iter()
        .position(|a| a == name)
        .and_then(|i| args.get(i + 1).cloned())
}

/// Random 6-digit pairing code (uniform, rejection sampled).
fn new_code() -> String {
    loop {
        let mut b = [0u8; 4];
        getrandom::getrandom(&mut b).expect("random");
        let v = u32::from_le_bytes(b);
        if v < 4_000_000_000 {
            return format!("{:06}", v % 1_000_000);
        }
    }
}

fn print_pairing(addr: SocketAddr, public: &[u8; 32], code: &str) {
    println!("pairing code {code} (valid 2 minutes, one use)");
    println!(
        "pair: meshwav://pair?host={}&port={}&pk={}&code={code}",
        addr.ip(),
        addr.port(),
        hex(public)
    );
    println!("or enter in the app: host={} port={} pk={} code={code}", addr.ip(), addr.port(), hex(public));
}

fn tailscale_ip() -> Option<String> {
    let out = Command::new("tailscale").args(["ip", "-4"]).output().ok()?;
    String::from_utf8(out.stdout)
        .ok()?
        .lines()
        .next()
        .map(|s| s.trim().to_string())
}

/// Monitors of every sink that currently carries a real, uncorked stream (system chatter ignored).
/// The capture follows these live; `--source NAME` pins one instead.
fn active_sources() -> Vec<String> {
    let mut found = Vec::new();
    let list = |what: &str| -> Vec<Vec<String>> {
        Command::new("pactl")
            .args(["list", "short", what])
            .output()
            .ok()
            .map(|o| {
                String::from_utf8_lossy(&o.stdout)
                    .lines()
                    .map(|l| l.split('\t').map(str::to_string).collect())
                    .collect()
            })
            .unwrap_or_default()
    };
    let sinks = list("sinks");
    // Long form so system chatter (speech-dispatcher) can be ignored; first real stream wins.
    let long = Command::new("pactl")
        .args(["list", "sink-inputs"])
        .output()
        .map(|o| String::from_utf8_lossy(&o.stdout).into_owned())
        .unwrap_or_default();
    for block in long.split("Sink Input #").skip(1) {
        if block.contains("speech-dispatcher") || block.contains("Corked: yes") {
            continue;
        }
        let id = block
            .lines()
            .find_map(|l| l.trim().strip_prefix("Sink: "))
            .map(str::trim);
        if let Some(name) = id
            .and_then(|id| {
                sinks
                    .iter()
                    .find(|s| s.first().map(String::as_str) == Some(id))
            })
            .and_then(|s| s.get(1))
        {
            let m = format!("{name}.monitor");
            if !found.contains(&m) {
                found.push(m);
            }
        }
    }
    found
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let kbps: u32 = arg(&args, "--kbps")
        .map(|v| v.parse().expect("--kbps"))
        .unwrap_or(160);
    let frame_ms: u32 = arg(&args, "--frame-ms")
        .map(|v| v.parse().expect("--frame-ms"))
        .unwrap_or(10);
    let bind = arg(&args, "--bind").unwrap_or_else(|| match tailscale_ip() {
        Some(ip) => format!("{ip}:{DEFAULT_PORT}"),
        None => {
            eprintln!("cannot determine tailscale IP; pass --bind IP:PORT");
            std::process::exit(2);
        }
    });
    let cfg_dir = std::path::PathBuf::from(std::env::var("HOME").expect("HOME")).join(".config/meshwav");
    let identity_path = arg(&args, "--identity").map(std::path::PathBuf::from).unwrap_or_else(|| cfg_dir.join("identity"));
    let authorized_path = arg(&args, "--authorized").map(std::path::PathBuf::from).unwrap_or_else(|| cfg_dir.join("authorized_peers"));
    if args.iter().any(|a| a == "--key-file") {
        eprintln!("--key-file is gone: devices now pair with a one-time code (run with --pair)");
        std::process::exit(2);
    }
    let identity = store::load_or_create_identity(&identity_path).unwrap_or_else(|e| {
        eprintln!("identity: {e}");
        std::process::exit(2);
    });
    let authorized = store::load_authorized(&authorized_path).unwrap_or_else(|e| {
        eprintln!("authorized peers: {e}");
        std::process::exit(2);
    });

    let addr: SocketAddr = bind.parse().expect("bad --bind");
    if !is_tailnet_or_loopback(addr.ip()) {
        eprintln!(
            "refusing to bind {}: not a tailnet or loopback address",
            addr.ip()
        );
        std::process::exit(2);
    }
    let sock = UdpSocket::bind(addr).expect("bind failed");
    println!("meshwav-agent listening on {addr}");
    println!("agent public key: {}", hex(&identity.public));
    println!("paired devices: {}", authorized.len());
    let t0 = Instant::now();
    let now_ms = move || t0.elapsed().as_millis() as u64;
    let public = identity.public;
    let server = Arc::new(Mutex::new(Server::new(identity, authorized)));
    let open_window = {
        let server = server.clone();
        move || {
            let code = new_code();
            server.lock().unwrap().open_pairing(&code, now_ms());
            print_pairing(addr, &public, &code);
        }
    };
    if args.iter().any(|a| a == "--pair") {
        open_window();
    } else if server.lock().unwrap().authorized().is_empty() {
        println!("no paired devices yet: restart with --pair, or type `pair` + Enter here");
    }
    std::thread::spawn(move || {
        for line in std::io::stdin().lock().lines() {
            match line {
                Ok(l) if l.trim() == "pair" => open_window(),
                Ok(_) => {}
                Err(_) => break,
            }
        }
    });

    let mut enc = OpusEncoder::new(frame_ms * 1000, kbps, false).expect("encoder");
    let frame_samples = enc.frame_samples();

    let session: Arc<Mutex<Option<Session>>> = Arc::new(Mutex::new(None));
    {
        let (sock, session, server) = (sock.try_clone().unwrap(), session.clone(), server.clone());
        let authorized_path = authorized_path.clone();
        std::thread::spawn(move || {
            let mut buf = [0u8; 2048];
            loop {
                let Ok((n, from)) = sock.recv_from(&mut buf) else {
                    // e.g. ICMP unreachable after a receiver vanished; never spin
                    std::thread::sleep(Duration::from_millis(50));
                    continue;
                };
                let d = &buf[..n];
                if handshake::is_handshake(d) {
                    let out = server.lock().unwrap().handle(from, d, now_ms());
                    match out {
                        Outcome::Reply(r) => {
                            let _ = sock.send_to(&r, from);
                        }
                        Outcome::Established { keys, peer, name, newly_paired } => {
                            if newly_paired {
                                let list = server.lock().unwrap().authorized().to_vec();
                                if let Err(e) = store::save_authorized(&authorized_path, &list) {
                                    eprintln!("could not save paired devices: {e}");
                                }
                            }
                            let mut cur = session.lock().unwrap();
                            // A device re-keying itself (network change, idle reconnect) is not news.
                            let rekey = cur.as_ref().is_some_and(|s| s.peer == peer && s.last_seen.elapsed() < PEER_TIMEOUT);
                            if newly_paired || !rekey {
                                println!(
                                    "{} '{name}' (key {}) connected from {from}",
                                    if newly_paired { "paired new device" } else { "device" },
                                    &hex(&peer)[..16]
                                );
                            }
                            let mut s = Session {
                                addr: from,
                                peer,
                                last_seen: Instant::now(),
                                sealer: Sealer::new(&keys.to_receiver, Direction::AgentToReceiver, STREAM_ID, 0),
                                keepalive: Sealer::new(&keys.to_receiver, Direction::AgentToReceiver, CONTROL_STREAM, 0),
                                opener: Opener::new(&keys.to_agent, Direction::ReceiverToAgent, STREAM_ID),
                            };
                            s.send_keepalive(&sock); // doubles as the receiver's pairing/handshake confirmation
                            *cur = Some(s);
                        }
                        Outcome::Rejected(why) => eprintln!("handshake from {from} rejected: {why}"),
                        Outcome::None => {}
                    }
                } else if let Some(s) = session.lock().unwrap().as_mut().filter(|s| s.addr == from) {
                    if let Ok((h, _)) = s.opener.open(d) {
                        if h.ptype == PacketType::Punch {
                            s.last_seen = Instant::now();
                        }
                    }
                }
            }
        });
    }

    {
        let (sock, session) = (sock.try_clone().unwrap(), session.clone());
        std::thread::spawn(move || loop {
            std::thread::sleep(KEEPALIVE);
            if let Some(s) = session.lock().unwrap().as_mut().filter(|s| s.last_seen.elapsed() < PEER_TIMEOUT) {
                s.send_keepalive(&sock);
            }
        });
    }

    let pinned = arg(&args, "--source");
    let current: Arc<Mutex<Option<String>>> = Arc::new(Mutex::new(pinned.clone()));
    let child_pid: Arc<Mutex<Option<u32>>> = Arc::new(Mutex::new(None));
    if pinned.is_none() {
        let (current, child_pid) = (current.clone(), child_pid.clone());
        std::thread::spawn(move || loop {
            let act = active_sources();
            let mut cur = current.lock().unwrap();
            // Stay on the current source while it is still active; otherwise follow the first active one.
            let want = match &*cur {
                Some(c) if act.contains(c) => None,
                _ => act.first().cloned(),
            };
            if let Some(w) = want {
                println!("capturing from {w}");
                *cur = Some(w);
                if let Some(pid) = *child_pid.lock().unwrap() {
                    let _ = Command::new("kill").arg(pid.to_string()).status(); // loop respawns on the new source
                }
            }
            drop(cur);
            std::thread::sleep(Duration::from_millis(500));
        });
    } else {
        println!("capturing from {}", pinned.as_ref().unwrap());
    }

    let mut raw = vec![0u8; frame_samples * 4];
    let mut pcm = vec![0i16; frame_samples * 2];
    let mut out = Vec::with_capacity(1300);
    let (mut ts, mut sent, mut last_report, mut connected) = (0u32, 0u64, Instant::now(), false);
    'outer: loop {
        let source = loop {
            if let Some(src) = current.lock().unwrap().clone() {
                break src;
            }
            std::thread::sleep(Duration::from_millis(200));
        };
        let mut child = Command::new("parec")
            .args([
                "-d",
                source.as_str(),
                "--format=s16le",
                "--rate=48000",
                "--channels=2",
                "--raw",
                "--latency-msec=10",
            ])
            .stdout(Stdio::piped())
            .spawn()
            .expect("failed to start parec (is PulseAudio/PipeWire-pulse installed?)");
        *child_pid.lock().unwrap() = Some(child.id());
        let mut stdout = child.stdout.take().unwrap();
        loop {
            if stdout.read_exact(&mut raw).is_err() {
                *child_pid.lock().unwrap() = None;
                let _ = child.wait();
                if pinned.is_some() {
                    eprintln!("capture stream ended");
                    break 'outer;
                }
                // Source vanished or was switched: back off so a dead sink cannot cause a respawn storm.
                std::thread::sleep(Duration::from_millis(250));
                continue 'outer;
            }
            ts = ts.wrapping_add(frame_samples as u32);
            let mut guard = session.lock().unwrap();
            let mut exhausted = false;
            match guard.as_mut().filter(|s| s.last_seen.elapsed() < PEER_TIMEOUT) {
                None => {
                    if connected {
                        println!("receiver timed out; idle");
                        connected = false;
                    }
                }
                Some(sess) => {
                    connected = true;
                    for (i, c) in raw.chunks_exact(2).enumerate() {
                        pcm[i] = i16::from_le_bytes([c[0], c[1]]);
                    }
                    let Ok(frame) = enc.encode(&pcm) else {
                        continue;
                    };
                    let h = Header {
                        ptype: PacketType::Audio,
                        stream_id: 0,
                        seq: 0,
                        timestamp: ts,
                        frag_idx: 0,
                        frag_cnt: 1,
                        flags: 0,
                    };
                    out.clear();
                    if sess.sealer.seal(h, frame, &mut out).is_err() {
                        exhausted = true;
                    } else {
                        let _ = sock.send_to(&out, sess.addr);
                        sent += 1;
                    }
                }
            }
            if exhausted {
                // The receiver reconnects on silence and gets fresh keys from a new handshake.
                eprintln!("sequence space exhausted; dropping the session so the receiver rekeys");
                *guard = None;
                connected = false;
            }
            drop(guard);
            if last_report.elapsed() > Duration::from_secs(10) && connected {
                println!("sent {sent} packets");
                last_report = Instant::now();
            }
        }
    }
}
