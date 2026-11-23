//! The sending side: capture system audio, encode Opus, send AEAD-sealed UDP to the paired receiver
//! that most recently completed a Noise handshake. The receiver keeps the session alive with
//! sealed `Punch` packets about once a second; the agent stops sending after `PEER_TIMEOUT`.
//!
//!   meshwav-agent [--pair] [--bind IP:PORT] [--identity PATH] [--authorized PATH] [--source SINK.monitor] [--no-redirect] [--kbps 160] [--frame-ms 10] [--config PATH] [--log-level LEVEL]
//!
//! Redirect (default): while a receiver is connected, the default output becomes a virtual "Meshwav"
//! sink and every stream is moved onto it, so the PC plays nothing locally. The previous output is
//! restored 15 s after the receiver goes away and on exit. `--no-redirect` or `--source` turn it off.
//!
//! Pairing: `meshwav-agent pair` (against the running agent), `--pair` or typing `pair` on stdin opens a 2-minute window with a 6-digit code.
//! Defaults: bind = <tailscale ipv4>:47900, identity = ~/.config/meshwav/identity and paired
//! devices = ~/.config/meshwav/authorized_peers (both 0600, created on first use). The listener
//! refuses non-tailnet addresses. Capture uses `parec` (PulseAudio or
//! PipeWire-pulse) on the default sink's monitor.
#[macro_use]
mod log;
mod config;
mod fleet;
mod invite;
mod redirect;
mod store;

use audio_codec::opus::OpusEncoder;
use audio_protocol::crypto::{Direction, Opener, Sealer};
use audio_protocol::handshake::{self, hex};
use audio_protocol::control::{Message, CODEC_KEEP, CODEC_OPUS, CODEC_PCM, FRAME_CHOICES_MS, MAX_KBPS, MIN_KBPS, PCM_FRAME_BYTES, REJECT_BITRATE, REJECT_CODEC, REJECT_ENCODER, REJECT_FRAME};
use audio_protocol::packet::{frame_flags, Header, PacketType, FLAG_PCM};
use audio_protocol::server::{Outcome, Server};
use audio_transport::is_tailnet_or_loopback;
use std::io::{BufRead, Read};
use std::net::{SocketAddr, UdpSocket};
use std::os::unix::net::{UnixListener, UnixStream};
use std::io::Write;
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

/// One connected receiver. A new successful handshake from the same device replaces its session.
struct Session {
    addr: SocketAddr,
    peer: [u8; 32],
    name: String,
    connected_at: Instant,
    /// Last active/standby state announced to the receiver (`None` = not told yet).
    told: Option<bool>,
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
    pending: Option<(u16, u32, u8, u8)>,
    last_req: Option<u16>,
    last_reply: Option<Message>,
}

impl ControlState {
    /// Handle a decoded receiver message. Returns a reply to send right away, if any.
    /// A repeated request id is answered with the stored reply and never applied twice.
    fn on_message(&mut self, msg: Message) -> Option<Message> {
        let Message::SetConfig { req, kbps, frame_ms, codec } = msg else { return None };
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
        } else if ![CODEC_KEEP, CODEC_OPUS, CODEC_PCM].contains(&codec) {
            reject(REJECT_CODEC)
        } else {
            self.pending = Some((req, kbps, frame_ms, codec));
            None
        };
        self.last_reply = reply.clone();
        reply
    }

    /// Called by the capture loop between frames. `apply` switches the encoder and returns the values
    /// now in force, or an error reason. Returns the reply to send.
    fn take_pending(&mut self, apply: impl FnOnce(u32, u8, u8) -> Result<(u32, u8, u8), u8>) -> Option<Message> {
        let (req, kbps, frame_ms, codec) = self.pending.take()?;
        let reply = match apply(kbps, frame_ms, codec) {
            Ok((kbps, frame_ms, codec)) => Message::ConfigApplied { req, kbps, frame_ms, codec },
            Err(reason) => Message::ConfigRejected { req, reason },
        };
        self.last_reply = Some(reply.clone());
        Some(reply)
    }
}

/// Every connected receiver and which one is active (gets audio). The others stay on keepalives.
struct Fleet {
    sessions: Vec<Session>,
    preferred: Option<fleet::Key>,
    active: Option<fleet::Key>,
    preferred_path: std::path::PathBuf,
}

impl Fleet {
    fn online_keys(&self) -> Vec<fleet::Key> {
        let mut v: Vec<&Session> = self.sessions.iter().filter(|s| s.online()).collect();
        v.sort_by_key(|s| s.connected_at);
        v.iter().map(|s| s.peer).collect()
    }

    fn any_online(&self) -> bool {
        self.sessions.iter().any(Session::online)
    }

    fn is_online(&self, key: &fleet::Key) -> bool {
        self.sessions.iter().any(|s| &s.peer == key && s.online())
    }

    fn active_session(&mut self) -> Option<&mut Session> {
        let a = self.active?;
        self.sessions.iter_mut().find(|s| s.peer == a && s.online())
    }

    /// Re-run the selection and tell receivers whose state changed. Cheap; called every frame.
    fn update(&mut self, sock: &UdpSocket) {
        self.sessions.retain(|s| s.last_seen.elapsed() < Duration::from_secs(60));
        let new = fleet::choose(self.preferred, self.active, &self.online_keys());
        if new != self.active {
            let label = |k: Option<fleet::Key>| match k.and_then(|k| self.sessions.iter().find(|s| s.peer == k)) {
                Some(s) => format!("'{}'", s.name),
                None => "nobody".to_string(),
            };
            info!("active receiver: {} (was {})", label(new), label(self.active));
            self.active = new;
        }
        let active = self.active;
        for s in self.sessions.iter_mut().filter(|s| s.online()) {
            let want = Some(s.peer) == active;
            if s.told != Some(want) {
                s.send_control(&Message::ActiveState { active: want }, sock);
                s.told = Some(want);
            }
        }
    }

    /// Once a second: keepalives for everyone, and the active/standby state again (a lost packet only delays it).
    fn keepalive(&mut self, sock: &UdpSocket) {
        self.update(sock);
        let active = self.active;
        for s in self.sessions.iter_mut().filter(|s| s.online()) {
            s.send_keepalive(sock);
            s.send_control(&Message::ActiveState { active: Some(s.peer) == active }, sock);
        }
    }
}

impl Session {
    fn online(&self) -> bool {
        self.last_seen.elapsed() < PEER_TIMEOUT
    }

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

fn pairing_text(addr: SocketAddr, public: &[u8; 32], code: &str) -> String {
    format!(
        "pairing code {code} (valid 2 minutes, one use)\npair: meshwav://pair?host={}&port={}&pk={}&code={code}\nor enter in the app: host={} port={} pk={} code={code}",
        addr.ip(),
        addr.port(),
        hex(public),
        addr.ip(),
        addr.port(),
        hex(public)
    )
}

fn fatal(msg: &str) -> ! {
    eprintln!("meshwav-agent: {msg}");
    std::process::exit(2);
}

/// The settings file: `--config PATH` (must exist), else `config.toml` in the config dir (optional).
fn load_config(args: &[String], cfg_dir: &std::path::Path) -> config::Config {
    let (path, required) = match arg(args, "--config") {
        Some(p) => (std::path::PathBuf::from(p), true),
        None => (cfg_dir.join("config.toml"), false),
    };
    match std::fs::read_to_string(&path) {
        Ok(text) => config::Config::parse(&text).unwrap_or_else(|e| fatal(&format!("{}: {e}", path.display()))),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound && !required => config::Config::default(),
        Err(e) => fatal(&format!("cannot read {}: {e}", path.display())),
    }
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

/// Run one local control command (`devices`, `use`, `rename`) and return the reply text.
/// Requests are tab-separated lines: `devices`, `use\t<device|auto>`, `rename\t<device>\t<new name>`.
fn run_command(
    line: &str,
    server: &Mutex<Server>,
    fleet: &Mutex<Fleet>,
    sock: &UdpSocket,
    authorized_path: &std::path::Path,
) -> String {
    let parts: Vec<&str> = line.trim_end_matches(['\r', '\n']).split('\t').collect();
    let list: Vec<(fleet::Key, String)> = server.lock().unwrap().authorized().iter().map(|a| (a.public, a.name.clone())).collect();
    let find = |q: &str| match fleet::resolve(q, &list) {
        fleet::Resolved::One(i) => Ok(i),
        fleet::Resolved::None => Err(format!("error: no paired device matches '{q}' (try `devices`)")),
        fleet::Resolved::Ambiguous => Err(format!("error: '{q}' matches more than one device; use more of the name or the key")),
    };
    match parts.as_slice() {
        ["devices"] => {
            let mut f = fleet.lock().unwrap();
            f.update(sock);
            if list.is_empty() {
                return "no paired devices yet".to_string();
            }
            let mut out = String::new();
            for (key, name) in &list {
                let state = if f.active == Some(*key) {
                    "active"
                } else if f.is_online(key) {
                    "standby"
                } else {
                    "offline"
                };
                let pref = if f.preferred == Some(*key) { "  (chosen)" } else { "" };
                out.push_str(&format!("{:<20} {}  {state}{pref}\n", name, &hex(key)[..16]));
            }
            out.trim_end().to_string()
        }
        ["use", q] if q.trim().eq_ignore_ascii_case("auto") => {
            let mut f = fleet.lock().unwrap();
            f.preferred = None;
            if let Err(e) = store::save_preferred(&f.preferred_path, None) {
                return format!("error: could not save the choice: {e}");
            }
            f.update(sock);
            "automatic selection: the device that was connected first keeps the audio".to_string()
        }
        ["use", q] => {
            let (key, name) = match find(q) {
                Ok(i) => list[i].clone(),
                Err(e) => return e,
            };
            let mut f = fleet.lock().unwrap();
            if let Err(e) = store::save_preferred(&f.preferred_path, Some(&key)) {
                return format!("error: could not save the choice: {e}");
            }
            f.preferred = Some(key);
            f.update(sock);
            if f.is_online(&key) {
                format!("audio now goes to '{name}'")
            } else {
                format!("'{name}' is offline; it gets the audio as soon as it connects")
            }
        }
        ["rename", q, new] => {
            let new: String = new.chars().filter(|c| !c.is_control()).collect::<String>().trim().to_string();
            if new.is_empty() {
                return "error: the new name is empty".to_string();
            }
            let (key, _) = match find(q) {
                Ok(i) => list[i].clone(),
                Err(e) => return e,
            };
            let snapshot = {
                let mut srv = server.lock().unwrap();
                srv.rename(&key, &new);
                srv.authorized().to_vec()
            };
            if let Err(e) = store::save_authorized(authorized_path, &snapshot) {
                return format!("error: could not save paired devices: {e}");
            }
            if let Some(s) = fleet.lock().unwrap().sessions.iter_mut().find(|s| s.peer == key) {
                s.name = new.clone();
            }
            format!("renamed to '{new}'")
        }
        _ => "error: commands are `devices`, `use <device|auto>`, `rename <device> <new name>`".to_string(),
    }
}

/// `meshwav-agent devices | use <device|auto> | rename <device> <new name>`: talk to the running agent.
fn client(args: &[String], sock_path: &std::path::Path) -> ! {
    let mut words: Vec<&str> = Vec::new();
    let mut it = args.iter().skip(1);
    while let Some(a) = it.next() {
        if a == "--control" {
            it.next();
        } else if !a.starts_with("--") {
            words.push(a);
        }
    }
    let line = match words.as_slice() {
        ["devices"] => "devices".to_string(),
        ["pair"] => "pair".to_string(),
        ["use", rest @ ..] if !rest.is_empty() => format!("use\t{}", rest.join(" ")),
        ["rename", dev, rest @ ..] if !rest.is_empty() => format!("rename\t{dev}\t{}", rest.join(" ")),
        _ => {
            eprintln!("usage: meshwav-agent devices | pair | use <device|auto> | rename <device> <new name>");
            std::process::exit(2);
        }
    };
    let mut c = UnixStream::connect(sock_path).unwrap_or_else(|e| {
        eprintln!("cannot reach the running agent at {} ({e}); is it started?", sock_path.display());
        std::process::exit(1);
    });
    let _ = c.set_read_timeout(Some(Duration::from_secs(5)));
    let _ = writeln!(c, "{line}");
    let mut reply = String::new();
    let _ = std::io::BufReader::new(c).read_to_string(&mut reply);
    println!("{}", reply.trim_end());
    std::process::exit(if reply.starts_with("error") { 1 } else { 0 });
}

/// Entry point shared by `meshwav-agent` and `meshwav send`. `args[0]` is the program name.
pub fn run(args: Vec<String>) {
    let cfg_dir = std::path::PathBuf::from(std::env::var("HOME").expect("HOME")).join(".config/meshwav");
    let control_path = arg(&args, "--control").map(std::path::PathBuf::from).unwrap_or_else(|| cfg_dir.join("control.sock"));
    if matches!(args.get(1).map(String::as_str), Some("devices" | "use" | "rename" | "pair")) {
        client(&args, &control_path);
    }
    let cfg = load_config(&args, &cfg_dir);
    let origin = |flag: &str, key: &str| if arg(&args, flag).is_some() { flag.to_string() } else { format!("config.toml `{key}`") };
    let setting = |flag: &str, key: &str| arg(&args, flag).or_else(|| cfg.get(key).map(String::from));
    let number = |flag: &str, key: &str, default: u32| match setting(flag, key) {
        None => default,
        Some(v) => v.parse().unwrap_or_else(|_| fatal(&format!("{}: `{v}` is not a number", origin(flag, key)))),
    };
    let kbps = number("--kbps", "kbps", 160);
    let frame_ms = number("--frame-ms", "frame_ms", 10);
    if let Some(v) = setting("--log-level", "log_level") {
        log::set_level(log::parse_level(&v).unwrap_or_else(|| fatal(&format!("{}: `{v}` is not one of error, warn, info, debug", origin("--log-level", "log_level")))));
    }
    let source = setting("--source", "source");
    let no_redirect = args.iter().any(|a| a == "--no-redirect") || cfg.flag("no_redirect").unwrap_or_else(|e| fatal(&e)).unwrap_or(false);
    let bind = setting("--bind", "bind").unwrap_or_else(|| match tailscale_ip() {
        Some(ip) => format!("{ip}:{DEFAULT_PORT}"),
        None => {
            eprintln!("cannot determine tailscale IP; pass --bind IP:PORT");
            std::process::exit(2);
        }
    });
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
    info!("meshwav-agent listening on {addr}");
    info!("agent public key: {}", hex(&identity.public));
    info!("paired devices: {}", authorized.len());
    let t0 = Instant::now();
    let now_ms = move || t0.elapsed().as_millis() as u64;
    let public = identity.public;
    let server = Arc::new(Mutex::new(Server::new(identity, authorized)));
    let open_window = {
        let server = server.clone();
        move || {
            let code = new_code();
            server.lock().unwrap().open_pairing(&code, now_ms());
            invite::announce(addr.ip(), public, addr.port(), Duration::from_millis(audio_protocol::server::PAIR_TTL_MS));
            pairing_text(addr, &public, &code)
        }
    };
    let pair_cmd = open_window.clone();
    if args.iter().any(|a| a == "--pair") {
        println!("{}", open_window());
    } else if server.lock().unwrap().authorized().is_empty() {
        info!("no paired devices yet: run `meshwav-agent pair`");
    }
    std::thread::spawn(move || {
        for line in std::io::stdin().lock().lines() {
            match line {
                Ok(l) if l.trim() == "pair" => println!("{}", open_window()),
                Ok(_) => {}
                Err(_) => break,
            }
        }
    });

    let mut enc = OpusEncoder::new(frame_ms * 1000, kbps, false).expect("encoder");
    let mut frame_samples = enc.frame_samples();
    let (mut cur_kbps, mut cur_frame_ms, mut cur_codec) = (kbps, frame_ms, CODEC_OPUS);

    let preferred_path = cfg_dir.join("active_peer");
    let session: Arc<Mutex<Fleet>> = Arc::new(Mutex::new(Fleet {
        sessions: Vec::new(),
        preferred: store::load_preferred(&preferred_path),
        active: None,
        preferred_path,
    }));
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
                                    warn!("could not save paired devices: {e}");
                                }
                            }
                            let mut cur = session.lock().unwrap();
                            // A device re-keying itself (network change, idle reconnect) is not news.
                            let old = cur.sessions.iter().position(|s| s.peer == peer);
                            let rekey = old.is_some_and(|i| cur.sessions[i].online());
                            if newly_paired || !rekey {
                                info!(
                                    "{} '{name}' (key {}) connected from {from}",
                                    if newly_paired { "paired new device" } else { "device" },
                                    &hex(&peer)[..16]
                                );
                            }
                            let connected_at = match old {
                                Some(i) if rekey => cur.sessions[i].connected_at,
                                _ => Instant::now(),
                            };
                            let mut s = Session {
                                addr: from,
                                peer,
                                name: name.clone(),
                                connected_at,
                                told: None,
                                last_seen: Instant::now(),
                                sealer: Sealer::new(&keys.to_receiver, Direction::AgentToReceiver, STREAM_ID, 0),
                                keepalive: Sealer::new(&keys.to_receiver, Direction::AgentToReceiver, CONTROL_STREAM, 0),
                                opener: Opener::new(&keys.to_agent, Direction::ReceiverToAgent, STREAM_ID),
                                ctl_opener: Opener::new(&keys.to_agent, Direction::ReceiverToAgent, CONTROL_STREAM),
                                ctl: ControlState::default(),
                                closed: false,
                            };
                            s.send_keepalive(&sock); // doubles as the receiver's pairing/handshake confirmation
                            cur.sessions.retain(|x| x.peer != peer);
                            cur.sessions.push(s);
                            cur.update(&sock);
                        }
                        Outcome::Rejected(why) => warn!("handshake from {from} rejected: {why}"),
                        Outcome::None => {}
                    }
                } else {
                    let mut f = session.lock().unwrap();
                    let active = f.active;
                    let Some(i) = f.sessions.iter().position(|s| s.addr == from) else { continue };
                    let is_active = Some(f.sessions[i].peer) == active;
                    let others_online = f.sessions.iter().enumerate().any(|(j, s)| j != i && s.online());
                    let s = &mut f.sessions[i];
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
                                    // Undo the redirect right away, unless another receiver is still listening.
                                    if !others_online {
                                        BYE.store(true, Ordering::SeqCst);
                                    }
                                    info!("receiver '{}' said goodbye", s.name);
                                } else if s.closed || !is_active {
                                    // A standby receiver must not retune the shared encoder; it can ask again once active.
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
            session.lock().unwrap().keepalive(&sock);
        });
    }

    // Local control socket (owner-only): `meshwav-agent devices | use | rename` talk to it.
    {
        let _ = std::fs::remove_file(&control_path); // a stale socket from a crashed run; the UDP bind above already proved we are the only agent
        let old_umask = unsafe { libc::umask(0o177) };
        let listener = UnixListener::bind(&control_path);
        unsafe { libc::umask(old_umask) };
        match listener {
            Ok(listener) => {
                let (sock, session, server, authorized_path) = (sock.try_clone().unwrap(), session.clone(), server.clone(), authorized_path.clone());
                let pair_cmd = pair_cmd.clone();
                std::thread::spawn(move || {
                    for conn in listener.incoming().flatten() {
                        let _ = conn.set_read_timeout(Some(Duration::from_secs(2)));
                        let mut line = String::new();
                        let mut reader = std::io::BufReader::new(&conn);
                        if reader.read_line(&mut line).is_ok() {
                            let reply = if line.trim() == "pair" {
                                info!("pairing window opened from the command line");
                                pair_cmd()
                            } else {
                                run_command(&line, &server, &session, &sock, &authorized_path)
                            };
                            let _ = writeln!(&conn, "{reply}");
                        }
                    }
                });
            }
            Err(e) => warn!("control socket unavailable at {}: {e}", control_path.display()),
        }
    }

    let want_redirect = !no_redirect && source.is_none();
    let redirect = Arc::new(Mutex::new(redirect::Redirect::new(redirect::Pactl, Some(cfg_dir.join("redirect.state")))));
    let redirect_on = want_redirect
        && match redirect.lock().unwrap().start() {
            Ok(()) => true,
            Err(e) => {
                warn!("redirect unavailable ({e}); capturing without it");
                false
            }
        };
    let _guard = redirect_on.then(|| RedirectGuard(redirect.clone()));
    unsafe {
        libc::signal(libc::SIGINT, on_signal as *const () as usize);
        libc::signal(libc::SIGTERM, on_signal as *const () as usize);
    }
    // Ctrl-C and SIGTERM must stop the agent with or without the redirect (the handler only sets a flag).
    {
        let (redirect, session) = (redirect.clone(), session.clone());
        std::thread::spawn(move || loop {
            if STOP.load(Ordering::SeqCst) {
                if redirect_on {
                    redirect.lock().unwrap().shutdown();
                }
                std::process::exit(0);
            }
            if !redirect_on {
                std::thread::sleep(Duration::from_millis(200));
                continue;
            }
            let connected = session.lock().unwrap().any_online();
            if BYE.swap(false, Ordering::SeqCst) {
                redirect.lock().unwrap().release();
            }
            redirect.lock().unwrap().tick(connected, Instant::now());
            std::thread::sleep(Duration::from_millis(500));
        });
    }
    let pinned = if redirect_on { Some(redirect::MONITOR.to_string()) } else { source.clone() };
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
                info!("capturing from {w}");
                *cur = Some(w);
                if let Some(pid) = *child_pid.lock().unwrap() {
                    let _ = Command::new("kill").arg(pid.to_string()).status(); // loop respawns on the new source
                }
            }
            drop(cur);
            std::thread::sleep(Duration::from_millis(500));
        });
    } else {
        info!("capturing from {}", pinned.as_ref().unwrap());
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
                    error!("capture stream ended");
                    break 'outer;
                }
                // Source vanished or was switched: back off so a dead sink cannot cause a respawn storm.
                std::thread::sleep(Duration::from_millis(250));
                continue 'outer;
            }
            ts = ts.wrapping_add(frame_samples as u32);
            let mut guard = session.lock().unwrap();
            guard.update(&sock);
            let mut exhausted = false;
            match guard.active_session() {
                None => {
                    if connected {
                        info!("receiver timed out; idle");
                        connected = false;
                    }
                }
                Some(sess) => {
                    connected = true;
                    let lossless = cur_codec == CODEC_PCM;
                    let frame: &[u8] = if lossless {
                        &raw // the capture already is 48 kHz stereo s16le: send it untouched
                    } else {
                        for (i, c) in raw.chunks_exact(2).enumerate() {
                            pcm[i] = i16::from_le_bytes([c[0], c[1]]);
                        }
                        let Ok(frame) = enc.encode(&pcm) else {
                            continue;
                        };
                        frame
                    };
                    let h = Header {
                        ptype: PacketType::Audio,
                        stream_id: 0,
                        seq: 0,
                        timestamp: ts,
                        frag_idx: 0,
                        frag_cnt: 1,
                        flags: if lossless { frame_flags(5) | FLAG_PCM } else { frame_flags(cur_frame_ms) },
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
                    let applied = sess.ctl.take_pending(|k, f, c| {
                        let want_k = if k == 0 { cur_kbps } else { k };
                        let want_f = if f == 0 { cur_frame_ms } else { u32::from(f) };
                        let want_c = if c == CODEC_KEEP { cur_codec } else { c };
                        if want_c == CODEC_PCM {
                            // Lossless is always 5 ms packets of exactly PCM_FRAME_BYTES, so none needs fragmenting.
                            // The Opus settings are only remembered for when the receiver switches back.
                            frame_samples = PCM_FRAME_BYTES / 4;
                            raw.resize(PCM_FRAME_BYTES, 0);
                        } else if want_c != cur_codec || want_f != cur_frame_ms {
                            let e = OpusEncoder::new(want_f * 1000, want_k, false).map_err(|_| REJECT_ENCODER)?;
                            frame_samples = e.frame_samples();
                            enc = e;
                            raw.resize(frame_samples * 4, 0);
                            pcm.resize(frame_samples * 2, 0);
                        } else if want_k != cur_kbps {
                            enc.set_bitrate_kbps(want_k).map_err(|_| REJECT_ENCODER)?;
                        }
                        (cur_kbps, cur_frame_ms, cur_codec) = (want_k, want_f, want_c);
                        if want_c == CODEC_PCM {
                            info!("receiver set lossless PCM (5 ms packets, ~1.5 Mbit/s)");
                        } else {
                            info!("receiver set {want_k} kbit/s, {want_f} ms frames");
                        }
                        Ok((want_k, want_f as u8, want_c))
                    });
                    if let Some(reply) = applied {
                        sess.send_control(&reply, &sock);
                    }
                }
            }
            if exhausted {
                // The receiver reconnects on silence and gets fresh keys from a new handshake.
                error!("sequence space exhausted; dropping the session so the receiver rekeys");
                if let Some(a) = guard.active {
                    guard.sessions.retain(|s| s.peer != a);
                }
                connected = false;
            }
            drop(guard);
            if last_report.elapsed() > Duration::from_secs(10) && connected {
                debug!("sent {sent} packets");
                last_report = Instant::now();
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn set(req: u16, kbps: u32, frame_ms: u8) -> Message {
        Message::SetConfig { req, kbps, frame_ms, codec: 0 }
    }

    #[test]
    fn valid_request_is_pending_until_applied_then_replied() {
        let mut c = ControlState::default();
        assert_eq!(c.on_message(set(1, 128, 20)), None);
        let r = c.take_pending(|k, f, c| { assert_eq!((k, f, c), (128, 20, 0)); Ok((k, f, 1)) });
        assert_eq!(r, Some(Message::ConfigApplied { req: 1, kbps: 128, frame_ms: 20, codec: 1 }));
        assert_eq!(c.take_pending(|_, _, _| panic!("applied twice")), None);
    }

    #[test]
    fn repeated_request_replays_reply_and_is_not_applied_twice() {
        let mut c = ControlState::default();
        c.on_message(set(5, 96, 0));
        c.take_pending(|k, f, c| Ok((k, if f == 0 { 10 } else { f }, c)));
        let again = c.on_message(set(5, 96, 0));
        assert_eq!(again, Some(Message::ConfigApplied { req: 5, kbps: 96, frame_ms: 10, codec: 0 }));
        assert_eq!(c.take_pending(|_, _, _| panic!("applied twice")), None);
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
        let r = c.take_pending(|k, f, _| { assert_eq!((k, f), (200, 5)); Err(REJECT_ENCODER) });
        assert_eq!(r, Some(Message::ConfigRejected { req: 2, reason: REJECT_ENCODER }));
    }

    #[test]
    fn other_messages_are_ignored() {
        let mut c = ControlState::default();
        assert_eq!(c.on_message(Message::Bye { reason: 0 }), None);
        assert_eq!(c.on_message(Message::ConfigApplied { req: 1, kbps: 1, frame_ms: 1, codec: 1 }), None);
        assert!(c.pending.is_none());
    }

    #[test]
    fn unknown_codec_is_rejected_and_pcm_is_accepted() {
        let mut c = ControlState::default();
        for codec in [3u8, 4, 255] {
            assert_eq!(c.on_message(Message::SetConfig { req: 10 + u16::from(codec), kbps: 0, frame_ms: 0, codec }), Some(Message::ConfigRejected { req: 10 + u16::from(codec), reason: REJECT_CODEC }));
            assert!(c.pending.is_none());
        }
        assert_eq!(c.on_message(Message::SetConfig { req: 1, kbps: 0, frame_ms: 0, codec: CODEC_PCM }), None);
        assert_eq!(c.pending, Some((1, 0, 0, CODEC_PCM)));
    }
}
