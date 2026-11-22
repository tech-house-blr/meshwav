//! meshwav-agent: capture system audio, encode Opus, send AEAD-sealed UDP to the paired receiver
//! that most recently completed a Noise handshake. The receiver keeps the session alive with
//! sealed `Punch` packets about once a second; the agent stops sending after `PEER_TIMEOUT`.
//!
//!   meshwav-agent [--pair] [--bind IP:PORT] [--identity PATH] [--authorized PATH] [--source SINK.monitor] [--no-redirect] [--kbps 160] [--frame-ms 10]
//!
//! Redirect (default): while a receiver is connected, the default output becomes a virtual "Meshwav"
//! sink and every stream is moved onto it, so the PC plays nothing locally. The previous output is
//! restored 15 s after the receiver goes away and on exit. `--no-redirect` or `--source` turn it off.
//!
//! Pairing: `--pair` (or typing `pair` on stdin) opens a 2-minute window with a 6-digit code.
//! Defaults: bind = <tailscale ipv4>:47900, identity = ~/.config/meshwav/identity and paired
//! devices = ~/.config/meshwav/authorized_peers (both 0600, created on first use). The listener
//! refuses non-tailnet addresses. Capture uses `parec` (PulseAudio or
//! PipeWire-pulse) on the default sink's monitor.
mod redirect;
mod store;

use audio_codec::opus::OpusEncoder;
use audio_protocol::crypto::{Direction, Opener, Sealer};
use audio_protocol::handshake::{self, hex};
use audio_protocol::control::{Message, FRAME_CHOICES_MS, MAX_KBPS, MIN_KBPS, REJECT_BITRATE, REJECT_ENCODER, REJECT_FRAME};
use audio_protocol::packet::{frame_flags, Header, PacketType};
use audio_protocol::server::{Outcome, Server};
use audio_transport::is_tailnet_or_loopback;
use std::io::{BufRead, Read};
use std::net::{SocketAddr, UdpSocket};
use std::process::{Command, Stdio};
use std::sync::{Arc, Mutex};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

static STOP: AtomicBool = AtomicBool::new(false);
/// Set when the receiver says goodbye, so the redirect is undone right away instead of after the grace period.
static BYE: AtomicBool = AtomicBool::new(false);

extern "C" fn on_signal(_: libc::c_int) {
    STOP.store(true, Ordering::SeqCst);
}

/// Restores the audio output when main unwinds or returns (not on `process::exit`).
struct RedirectGuard(Arc<Mutex<redirect::Redirect<redirect::Pactl>>>);

impl Drop for RedirectGuard {
    fn drop(&mut self) {
        if let Ok(mut r) = self.0.lock() {
            r.shutdown();
        }
    }
}

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
    /// Agent-to-receiver packets on the control stream: keepalives and control replies.
    keepalive: Sealer,
    opener: Opener,
    /// Receiver-to-agent control packets (own stream id, own nonce space).
    ctl_opener: Opener,
    ctl: ControlState,
    /// The receiver said goodbye: ignore its packets until a new handshake replaces this session.
    closed: bool,
}

/// Runtime configuration requests from the receiver. Pure state, so the policy is unit-testable.
#[derive(Default)]
struct ControlState {
    /// Accepted request waiting for the capture loop to apply it at the next frame boundary.
    pending: Option<(u16, u32, u8)>,
    last_req: Option<u16>,
    last_reply: Option<Message>,
}

impl ControlState {
    /// Handle a decoded receiver message. Returns a reply to send right away, if any.
    /// A repeated request id is answered with the stored reply and never applied twice.
    fn on_message(&mut self, msg: Message) -> Option<Message> {
        let Message::SetConfig { req, kbps, frame_ms } = msg else { return None };
        if self.last_req == Some(req) {
            return self.last_reply.clone(); // still pending: stay silent, the receiver will retry
        }
        self.last_req = Some(req);
        self.last_reply = None;
        self.pending = None;
        let reject = |reason| Some(Message::ConfigRejected { req, reason });
        let reply = if kbps != 0 && !(MIN_KBPS..=MAX_KBPS).contains(&kbps) {
            reject(REJECT_BITRATE)
        } else if frame_ms != 0 && !FRAME_CHOICES_MS.contains(&frame_ms) {
            reject(REJECT_FRAME)
        } else {
            self.pending = Some((req, kbps, frame_ms));
            None
        };
        self.last_reply = reply.clone();
        reply
    }

    /// Called by the capture loop between frames. `apply` switches the encoder and returns the values
    /// now in force, or an error reason. Returns the reply to send.
    fn take_pending(&mut self, apply: impl FnOnce(u32, u8) -> Result<(u32, u8), u8>) -> Option<Message> {
        let (req, kbps, frame_ms) = self.pending.take()?;
        let reply = match apply(kbps, frame_ms) {
            Ok((kbps, frame_ms)) => Message::ConfigApplied { req, kbps, frame_ms },
            Err(reason) => Message::ConfigRejected { req, reason },
        };
        self.last_reply = Some(reply.clone());
        Some(reply)
    }
}

impl Session {
    /// Sealed empty `Punch` on the control stream: proves liveness (and the handshake) to the receiver.
    fn send_control(&mut self, msg: &Message, sock: &UdpSocket) {
        let h = Header { ptype: PacketType::Control, stream_id: 0, seq: 0, timestamp: 0, frag_idx: 0, frag_cnt: 1, flags: 0 };
        let mut out = Vec::with_capacity(96);
        if self.keepalive.seal(h, &msg.encode(), &mut out).is_ok() {
            let _ = sock.send_to(&out, self.addr);
        }
    }

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
    let mut frame_samples = enc.frame_samples();
    let (mut cur_kbps, mut cur_frame_ms) = (kbps, frame_ms);

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
                                ctl_opener: Opener::new(&keys.to_agent, Direction::ReceiverToAgent, CONTROL_STREAM),
                                ctl: ControlState::default(),
                                closed: false,
                            };
                            s.send_keepalive(&sock); // doubles as the receiver's pairing/handshake confirmation
                            *cur = Some(s);
                        }
                        Outcome::Rejected(why) => eprintln!("handshake from {from} rejected: {why}"),
                        Outcome::None => {}
                    }
                } else if let Some(s) = session.lock().unwrap().as_mut().filter(|s| s.addr == from) {
                    if let Ok((h, _)) = s.opener.open(d) {
                        if h.ptype == PacketType::Punch && !s.closed {
                            s.last_seen = Instant::now();
                        }
                    } else if let Ok((h, body)) = s.ctl_opener.open(d) {
                        if !s.closed {
                            s.last_seen = Instant::now();
                        }
                        if h.ptype == PacketType::Control {
                            if let Ok(Some((msg, _))) = Message::decode(&body) {
                                if matches!(msg, Message::Bye { .. }) {
                                    // Sealed, so only the paired device can send it. Stop sending now.
                                    s.closed = true;
                                    if let Some(t) = Instant::now().checked_sub(PEER_TIMEOUT * 2) {
                                        s.last_seen = t;
                                    }
                                    BYE.store(true, Ordering::SeqCst);
                                    println!("receiver said goodbye");
                                } else if s.closed {
                                } else if let Some(reply) = s.ctl.on_message(msg) {
                                    s.send_control(&reply, &sock);
                                }
                            }
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

    let want_redirect = !args.iter().any(|a| a == "--no-redirect") && arg(&args, "--source").is_none();
    let redirect = Arc::new(Mutex::new(redirect::Redirect::new(redirect::Pactl, Some(cfg_dir.join("redirect.state")))));
    let redirect_on = want_redirect
        && match redirect.lock().unwrap().start() {
            Ok(()) => true,
            Err(e) => {
                eprintln!("redirect unavailable ({e}); capturing without it");
                false
            }
        };
    let _guard = redirect_on.then(|| RedirectGuard(redirect.clone()));
    unsafe {
        libc::signal(libc::SIGINT, on_signal as *const () as usize);
        libc::signal(libc::SIGTERM, on_signal as *const () as usize);
    }
    if redirect_on {
        let (redirect, session) = (redirect.clone(), session.clone());
        std::thread::spawn(move || loop {
            if STOP.load(Ordering::SeqCst) {
                redirect.lock().unwrap().shutdown();
                std::process::exit(0);
            }
            let connected = session.lock().unwrap().as_ref().is_some_and(|s| s.last_seen.elapsed() < PEER_TIMEOUT);
            if BYE.swap(false, Ordering::SeqCst) {
                redirect.lock().unwrap().release();
            }
            redirect.lock().unwrap().tick(connected, Instant::now());
            std::thread::sleep(Duration::from_millis(500));
        });
    }
    let pinned = if redirect_on { Some(redirect::MONITOR.to_string()) } else { arg(&args, "--source") };
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
                        flags: frame_flags(cur_frame_ms),
                    };
                    out.clear();
                    if sess.sealer.seal(h, frame, &mut out).is_err() {
                        exhausted = true;
                    } else {
                        let _ = sock.send_to(&out, sess.addr);
                        sent += 1;
                    }
                    // Apply a requested change after this frame, so the old encoder finishes its frame and
                    // every packet announces the frame size it was actually encoded with.
                    let applied = sess.ctl.take_pending(|k, f| {
                        let want_k = if k == 0 { cur_kbps } else { k };
                        let want_f = if f == 0 { cur_frame_ms } else { u32::from(f) };
                        if want_f != cur_frame_ms {
                            let e = OpusEncoder::new(want_f * 1000, want_k, false).map_err(|_| REJECT_ENCODER)?;
                            frame_samples = e.frame_samples();
                            enc = e;
                            raw.resize(frame_samples * 4, 0);
                            pcm.resize(frame_samples * 2, 0);
                        } else if want_k != cur_kbps {
                            enc.set_bitrate_kbps(want_k).map_err(|_| REJECT_ENCODER)?;
                        }
                        (cur_kbps, cur_frame_ms) = (want_k, want_f);
                        println!("receiver set {want_k} kbit/s, {want_f} ms frames");
                        Ok((want_k, want_f as u8))
                    });
                    if let Some(reply) = applied {
                        sess.send_control(&reply, &sock);
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

#[cfg(test)]
mod tests {
    use super::*;

    fn set(req: u16, kbps: u32, frame_ms: u8) -> Message {
        Message::SetConfig { req, kbps, frame_ms }
    }

    #[test]
    fn valid_request_is_pending_until_applied_then_replied() {
        let mut c = ControlState::default();
        assert_eq!(c.on_message(set(1, 128, 20)), None);
        let r = c.take_pending(|k, f| { assert_eq!((k, f), (128, 20)); Ok((k, f)) });
        assert_eq!(r, Some(Message::ConfigApplied { req: 1, kbps: 128, frame_ms: 20 }));
        assert_eq!(c.take_pending(|_, _| panic!("applied twice")), None);
    }

    #[test]
    fn repeated_request_replays_reply_and_is_not_applied_twice() {
        let mut c = ControlState::default();
        c.on_message(set(5, 96, 0));
        c.take_pending(|k, f| Ok((k, if f == 0 { 10 } else { f })));
        let again = c.on_message(set(5, 96, 0));
        assert_eq!(again, Some(Message::ConfigApplied { req: 5, kbps: 96, frame_ms: 10 }));
        assert_eq!(c.take_pending(|_, _| panic!("applied twice")), None);
    }

    #[test]
    fn retry_while_pending_stays_silent() {
        let mut c = ControlState::default();
        assert_eq!(c.on_message(set(2, 0, 5)), None);
        assert_eq!(c.on_message(set(2, 0, 5)), None);
        assert!(c.pending.is_some());
    }

    #[test]
    fn out_of_range_values_are_rejected_and_never_applied() {
        let mut c = ControlState::default();
        for (k, f, why) in [(63, 10, REJECT_BITRATE), (257, 10, REJECT_BITRATE), (u32::MAX, 0, REJECT_BITRATE), (128, 7, REJECT_FRAME), (128, 40, REJECT_FRAME), (0, 255, REJECT_FRAME)] {
            let req = (k % 1000) as u16 + u16::from(f);
            assert_eq!(c.on_message(set(req, k, f)), Some(Message::ConfigRejected { req, reason: why }), "{k} {f}");
            assert!(c.pending.is_none());
        }
    }

    #[test]
    fn encoder_failure_is_reported_and_new_request_supersedes_old() {
        let mut c = ControlState::default();
        c.on_message(set(1, 100, 10));
        c.on_message(set(2, 200, 5)); // supersedes the unapplied first request
        let r = c.take_pending(|k, f| { assert_eq!((k, f), (200, 5)); Err(REJECT_ENCODER) });
        assert_eq!(r, Some(Message::ConfigRejected { req: 2, reason: REJECT_ENCODER }));
    }

    #[test]
    fn other_messages_are_ignored() {
        let mut c = ControlState::default();
        assert_eq!(c.on_message(Message::Bye { reason: 0 }), None);
        assert_eq!(c.on_message(Message::ConfigApplied { req: 1, kbps: 1, frame_ms: 1 }), None);
        assert!(c.pending.is_none());
    }
}
