//! The live connection: handshake, keepalives, receive loop, jitter buffer and playback.
//! Mirrors the Android `StreamReceiver`: three threads (receive, keepalive, playback) around a
//! jitter buffer, and a rebuild of the socket and session when the agent goes quiet.
use crate::conceal::Concealer;
use crate::jitter::{Counters, JitterBuffer, Out};
use crate::sink::Sink;
use audio_codec::opus::OpusDecoder;
use audio_protocol::control::{Message, CODEC_OPUS, CODEC_PCM, PCM_FRAME_BYTES};
use audio_protocol::crypto::{Direction, Opener, Sealer};
use audio_protocol::handshake::{self, Initiator, Keypair, PublicKey, KIND_XX, KIND_XXPSK3};
use audio_protocol::packet::{frame_ms_from_flags, Header, PacketType, FLAG_PCM};
use audio_protocol::ProtocolError;
use std::io;
use std::net::{SocketAddr, UdpSocket};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::time::{Duration, Instant};

const AUDIO_STREAM: u32 = 1;
const CONTROL_STREAM: u32 = 2; // agent keepalives; separate seq space so the audio seq stays gap-free
const AGENT_QUIET_REBIND_MS: u64 = 4_000; // the agent sends keepalives every second
const PAIRING_ACK_MS: u64 = 8_000;
const FIRST_FRAME_MS: u32 = 10; // until the agent says otherwise in an audio packet

pub struct Settings {
    pub agent: SocketAddr,
    pub agent_key: PublicKey,
    pub identity: Keypair,
    /// Shown in the agent's device list (at most 32 bytes).
    pub name: String,
}

#[derive(Default, Clone, Debug)]
pub struct Snapshot {
    pub state: String,
    pub standby: bool,
    pub lossless: bool,
    pub frame_ms: u32,
    /// Set when the sender refused or fell back from the requested codec.
    pub note: String,
    pub counters: Counters,
    pub reconnects: u64,
    pub buffer_ms: u32,
    pub target_ms: u32,
    pub jitter_ms: f64,
    /// The connected sender's own name (empty until connected, or if it sends none).
    pub sender_name: String,
    /// The sender said it can switch the audio to this device on request.
    pub can_play_here: bool,
    /// The level (0..=100) of the device that is playing, as the sender last relayed it. `None` when this
    /// device is the one playing, or the playing device cannot report one.
    pub remote_volume: Option<u8>,
}

/// Live state for the UI or the log; cheap to read from any thread.
#[derive(Default)]
pub struct Status {
    snap: Mutex<Snapshot>,
    want_lossless: std::sync::atomic::AtomicBool,
    /// The last `SetConfig` request the agent answered (applied or refused).
    cfg_ack: std::sync::atomic::AtomicU16,
    /// Copies of `PlayHere` still to send (UDP, so a few).
    play_here_left: std::sync::atomic::AtomicU8,
    // The four below hold `percent + 1`, so 0 (the default) means nothing.
    /// A level this device wants the playing device to use, and how many copies are still to go out.
    vol_ask: std::sync::atomic::AtomicU16,
    vol_ask_left: std::sync::atomic::AtomicU8,
    /// A level the sender asked this device to apply; the owner of the volume takes it.
    vol_apply: std::sync::atomic::AtomicU16,
    /// This device's own level, reported to the sender while it plays. 0 = this device cannot report one.
    vol_local: std::sync::atomic::AtomicU16,
}

impl Status {
    pub fn snapshot(&self) -> Snapshot {
        self.snap.lock().unwrap().clone()
    }
    /// Ask the agent for lossless PCM (`true`) or Opus (`false`). Applies live and is repeated after a reconnect.
    pub fn set_lossless(&self, on: bool) {
        self.want_lossless.store(on, Ordering::Relaxed);
    }
    /// Ask the sender to send the audio to this device and keep doing so. Only meaningful once
    /// `Snapshot::can_play_here` is set.
    pub fn play_here(&self) {
        self.play_here_left.store(3, Ordering::Relaxed);
    }
    /// Ask the device that is playing to use this level. Only meaningful while `Snapshot::remote_volume` is set.
    pub fn set_remote_volume(&self, percent: u32) {
        self.vol_ask.store(percent.min(100) as u16 + 1, Ordering::Relaxed);
        self.vol_ask_left.store(2, Ordering::Relaxed);
    }
    /// Tell the sender this device's own level, so the other devices can show it. Call whenever it changes
    /// or once a frame; it is only sent while this device is the one playing.
    pub fn report_volume(&self, percent: u32) {
        self.vol_local.store(percent.min(100) as u16 + 1, Ordering::Relaxed);
    }
    /// A level another device asked this one to play at, if any. Apply it to the output volume.
    pub fn take_volume_request(&self) -> Option<u32> {
        match self.vol_apply.swap(0, Ordering::Relaxed) {
            0 => None,
            v => Some(v as u32 - 1),
        }
    }
    fn update(&self, f: impl FnOnce(&mut Snapshot)) {
        f(&mut self.snap.lock().unwrap());
    }
    fn state(&self, s: impl Into<String>) {
        self.update(|x| x.state = s.into());
    }
}

enum Hs {
    Fatal(String),
    Retry(String),
    Stopped,
}

struct Live {
    sock: UdpSocket,
    punch: Mutex<Sealer>,
    ctl: Mutex<Sealer>,
}

fn seal(s: &Mutex<Sealer>, ptype: PacketType, payload: &[u8]) -> Vec<u8> {
    let h = Header { ptype, stream_id: 0, seq: 0, timestamp: 0, frag_idx: 0, frag_cnt: 1, flags: 0 };
    let mut out = Vec::new();
    s.lock().unwrap().seal(h, payload, &mut out).expect("small payload, fresh sealer");
    out
}

impl Live {
    fn punch(&self) -> io::Result<()> {
        self.sock.send(&seal(&self.punch, PacketType::Punch, &[])).map(|_| ())
    }

    /// Tell the agent we are leaving so it hands the PC's audio output back now. Sent 3 times (UDP).
    fn goodbye(&self) {
        for i in 0..3 {
            let _ = self.sock.send(&seal(&self.ctl, PacketType::Control, &Message::Bye { reason: 0 }.encode()));
            if i < 2 {
                std::thread::sleep(Duration::from_millis(20));
            }
        }
    }
}

struct Conn {
    live: Arc<Live>,
    audio: Opener,
    ctl: Opener,
    /// The sender's own name from the handshake; empty from a sender that predates it.
    peer_name: String,
}

fn random_u32() -> u32 {
    let mut b = [0u8; 4];
    getrandom::getrandom(&mut b).expect("system randomness");
    u32::from_be_bytes(b)
}

/// One handshake attempt: a pairing code the first time (used exactly once, retrying a wrong code only
/// burns the agent's attempt budget), the pinned agent key afterwards.
fn connect(s: &Settings, code: Option<&str>, stop: &AtomicBool) -> Result<Conn, Hs> {
    if !audio_transport::is_tailnet_or_loopback(s.agent.ip()) {
        return Err(Hs::Fatal(format!("{} is not a tailnet or loopback address; refusing to connect", s.agent.ip())));
    }
    if let Some(c) = code {
        if !handshake::valid_code(c) {
            return Err(Hs::Fatal("the pairing code must be 6 digits".into()));
        }
    }
    let retry = |e: io::Error| Hs::Retry(e.to_string());
    let bind = match s.agent {
        SocketAddr::V4(a) if a.ip().is_loopback() => "127.0.0.1:0",
        SocketAddr::V4(_) => "0.0.0.0:0",
        SocketAddr::V6(a) if a.ip().is_loopback() => "[::1]:0",
        SocketAddr::V6(_) => "[::]:0",
    };
    let sock = UdpSocket::bind(bind).map_err(retry)?;
    sock.connect(s.agent).map_err(retry)?;
    sock.set_read_timeout(Some(Duration::from_millis(500))).map_err(retry)?;

    let psk = code.map(|c| handshake::pairing_psk(&s.agent_key, c));
    let kind = if psk.is_some() { KIND_XXPSK3 } else { KIND_XX };
    let mut ini = Initiator::new(&s.identity, psk.as_ref(), Some(s.agent_key)).map_err(|e| Hs::Fatal(e.to_string()))?;
    let sid = random_u32();
    let m1 = handshake::frame(kind, sid, 1, &ini.write_msg1().map_err(|e| Hs::Fatal(e.to_string()))?);
    let mut buf = [0u8; 2048];
    let mut reply: Option<Vec<u8>> = None;
    'attempts: for _ in 0..6 {
        if stop.load(Ordering::Relaxed) {
            return Err(Hs::Stopped);
        }
        let _ = sock.send(&m1);
        let until = Instant::now() + Duration::from_millis(500);
        while Instant::now() < until {
            match sock.recv(&mut buf) {
                Ok(n) => {
                    if let Some(f) = handshake::parse_frame(&buf[..n]) {
                        if f.kind == kind && f.sid == sid && f.step == 2 {
                            reply = Some(f.body.to_vec());
                            break 'attempts;
                        }
                    }
                }
                Err(e) if matches!(e.kind(), io::ErrorKind::WouldBlock | io::ErrorKind::TimedOut) => break,
                Err(_) => std::thread::sleep(Duration::from_millis(100)), // e.g. connection refused while the agent is down
            }
        }
    }
    let reply = reply.ok_or_else(|| {
        Hs::Retry(if code.is_some() {
            "no answer from the agent. Is its pairing window open (meshwav pair, valid 2 minutes)?".into()
        } else {
            "no answer from the agent".into()
        })
    })?;
    let peer_name = match ini.read_msg2_named(&reply) {
        Ok((_, n)) => n,
        Err(ProtocolError::PeerMismatch) => {
            return Err(Hs::Fatal("The agent's key is not the one this receiver paired with. Pair again if you reinstalled the agent.".into()))
        }
        Err(e) => return Err(Hs::Retry(e.to_string())),
    };
    let name: String = s.name.chars().filter(|c| !c.is_control()).collect();
    let name = truncate_utf8(&name, handshake::MAX_DEVICE_NAME);
    let m3 = handshake::frame(kind, sid, 3, &ini.write_msg3(name).map_err(|e| Hs::Fatal(e.to_string()))?);
    for i in 0..3 {
        let _ = sock.send(&m3);
        if i < 2 {
            std::thread::sleep(Duration::from_millis(20));
        }
    }
    let keys = ini.finish().map_err(|e| Hs::Fatal(e.to_string()))?;
    Ok(Conn {
        peer_name,
        audio: Opener::new(&keys.to_receiver, Direction::AgentToReceiver, AUDIO_STREAM),
        ctl: Opener::new(&keys.to_receiver, Direction::AgentToReceiver, CONTROL_STREAM),
        live: Arc::new(Live {
            sock,
            punch: Mutex::new(Sealer::new(&keys.to_agent, Direction::ReceiverToAgent, AUDIO_STREAM, 0)),
            ctl: Mutex::new(Sealer::new(&keys.to_agent, Direction::ReceiverToAgent, CONTROL_STREAM, 0)),
        }),
    })
}

fn truncate_utf8(s: &str, max: usize) -> &str {
    let mut n = s.len().min(max);
    while !s.is_char_boundary(n) {
        n -= 1;
    }
    &s[..n]
}

/// Handshake until a session exists, retrying while the agent is down. Err = stopped or fatal.
fn establish(s: &Settings, status: &Status, stop: &AtomicBool) -> Result<Conn, String> {
    while !stop.load(Ordering::Relaxed) {
        status.state(format!("Connecting to {}", s.agent));
        match connect(s, None, stop) {
            Ok(c) => {
                let n = c.peer_name.clone();
                status.update(|x| {
                    x.sender_name = n;
                    x.can_play_here = false;
                    x.remote_volume = None;
                });
                return Ok(c);
            }
            Err(Hs::Fatal(m)) => return Err(m),
            Err(Hs::Stopped) => break,
            Err(Hs::Retry(m)) => status.state(format!("Waiting for the agent ({m})")),
        }
        sleep_unless(stop, 1500);
    }
    Err("stopped".into())
}

fn sleep_unless(stop: &AtomicBool, ms: u64) {
    let end = Instant::now() + Duration::from_millis(ms);
    while Instant::now() < end && !stop.load(Ordering::Relaxed) {
        std::thread::sleep(Duration::from_millis(20));
    }
}

/// Pair with a one-time code: handshake, then prove the session works by waiting for the first
/// authentic packet from the agent. The caller saves the pairing afterwards.
pub fn pair(s: &Settings, code: &str, stop: &AtomicBool) -> Result<(), String> {
    let mut conn = match connect(s, Some(code), stop) {
        Ok(c) => c,
        Err(Hs::Fatal(m)) | Err(Hs::Retry(m)) => return Err(m),
        Err(Hs::Stopped) => return Err("stopped".into()),
    };
    let end = Instant::now() + Duration::from_millis(PAIRING_ACK_MS);
    let mut buf = [0u8; 2048];
    let mut next_punch = Instant::now();
    while Instant::now() < end && !stop.load(Ordering::Relaxed) {
        if Instant::now() >= next_punch {
            let _ = conn.live.punch();
            next_punch = Instant::now() + Duration::from_millis(150);
        }
        if let Ok(n) = conn.live.sock.recv(&mut buf) {
            if conn.audio.open(&buf[..n]).is_ok() || conn.ctl.open(&buf[..n]).is_ok() {
                conn.live.goodbye();
                return Ok(());
            }
        }
    }
    Err("no answer after pairing. The code was wrong or expired, or the agent is not in pairing mode.".into())
}

struct Core {
    jb: JitterBuffer,
    /// Counters of buffers retired by a frame-size switch, so the totals keep growing.
    base: Counters,
    frame_changed: bool,
}

struct Shared<'a> {
    stop: &'a AtomicBool,
    start: Instant,
    core: Mutex<Core>,
    cv: Condvar,
    live: Mutex<Option<Arc<Live>>>,
    rebind: Mutex<Option<String>>,
    status: &'a Status,
    fatal: Mutex<Option<String>>,
}

impl Shared<'_> {
    /// Monotonic milliseconds; offset so 0 never occurs (the jitter buffer uses 0 as "unset").
    fn now(&self) -> u64 {
        self.start.elapsed().as_millis() as u64 + 1000
    }
    fn fail(&self, msg: String) {
        self.fatal.lock().unwrap().get_or_insert(msg);
        self.stop.store(true, Ordering::Relaxed);
        self.cv.notify_all();
    }
}

/// Connect to the paired agent and play its stream until `stop` is set or something fatal happens.
/// Reconnects by itself when the network changes or the agent restarts.
pub fn run(s: &Settings, sink: Box<dyn Sink>, stop: &AtomicBool, status: &Status) -> Result<(), String> {
    status.update(|x| *x = Snapshot { state: "Starting".into(), ..Default::default() });
    let sh = Shared {
        stop,
        start: Instant::now(),
        core: Mutex::new(Core { jb: JitterBuffer::for_frame_ms(FIRST_FRAME_MS), base: Counters::default(), frame_changed: false }),
        cv: Condvar::new(),
        live: Mutex::new(None),
        rebind: Mutex::new(None),
        status,
        fatal: Mutex::new(None),
    };
    std::thread::scope(|sc| {
        sc.spawn(|| play_loop(&sh, sink));
        sc.spawn(|| punch_loop(&sh));
        if let Err(m) = recv_loop(&sh, s) {
            if m != "stopped" {
                sh.fail(m);
            }
        }
        stop.store(true, Ordering::Relaxed);
        sh.cv.notify_all();
    });
    let live = sh.live.lock().unwrap().take();
    if let Some(l) = live {
        l.goodbye();
    }
    status.state("Idle");
    match sh.fatal.into_inner().unwrap() {
        Some(m) => Err(m),
        None => Ok(()),
    }
}

fn punch_loop(sh: &Shared) {
    let (mut n, mut next, mut cur) = (0u32, 0u64, 0usize);
    let mut errors = 0;
    // Codec requests: a new series starts when the wish changes or the connection is rebuilt, and runs
    // until the agent answers it (or 10 tries), so a sender that refuses is not asked forever.
    let (mut want_seen, mut req, mut tries, mut cfg_next, mut play_next) = (false, 0u16, 0u32, 0u64, 0u64);
    let (mut vol_next, mut vol_sent, mut vol_report_at) = (0u64, 0u16, 0u64);
    while !sh.stop.load(Ordering::Relaxed) {
        let live = sh.live.lock().unwrap().clone();
        if let Some(l) = live {
            let id = Arc::as_ptr(&l) as usize;
            if id != cur {
                cur = id;
                n = 0;
                next = 0;
                tries = 0;
                req = req.wrapping_add(1).max(1);
            }
            let now = sh.now();
            let want = sh.status.want_lossless.load(Ordering::Relaxed);
            if want != want_seen {
                want_seen = want;
                tries = 0;
                req = req.wrapping_add(1).max(1);
            }
            if want != sh.status.snapshot().lossless && tries < 10 && sh.status.cfg_ack.load(Ordering::Relaxed) != req && now >= cfg_next {
                let codec = if want { CODEC_PCM } else { CODEC_OPUS };
                let m = Message::SetConfig { req, kbps: 0, frame_ms: 0, codec };
                let _ = l.sock.send(&seal(&l.ctl, PacketType::Control, &m.encode()));
                tries += 1;
                cfg_next = now + 500;
            }
            if sh.status.play_here_left.load(Ordering::Relaxed) > 0 && now >= play_next {
                sh.status.play_here_left.fetch_sub(1, Ordering::Relaxed);
                let _ = l.sock.send(&seal(&l.ctl, PacketType::Control, &Message::PlayHere.encode()));
                play_next = now + 200;
            }
            if sh.status.vol_ask_left.load(Ordering::Relaxed) > 0 && now >= vol_next {
                sh.status.vol_ask_left.fetch_sub(1, Ordering::Relaxed);
                if let Some(p) = sh.status.vol_ask.load(Ordering::Relaxed).checked_sub(1) {
                    let _ = l.sock.send(&seal(&l.ctl, PacketType::Control, &Message::SetVolume { percent: p as u8 }.encode()));
                }
                vol_next = now + 150;
            }
            // Report our own level when it changes, and every 1.5 s so a lost packet only delays it.
            let local = sh.status.vol_local.load(Ordering::Relaxed);
            if local > 0 && (local != vol_sent || now >= vol_report_at) && !sh.status.snapshot().standby {
                let _ = l.sock.send(&seal(&l.ctl, PacketType::Control, &Message::VolumeLevel { percent: (local - 1) as u8 }.encode()));
                vol_sent = local;
                vol_report_at = now + 1500;
            }
            if now >= next {
                match l.punch() {
                    Ok(()) => errors = 0,
                    Err(_) => {
                        errors += 1;
                        if errors >= 3 {
                            *sh.rebind.lock().unwrap() = Some("send failed".into());
                            errors = 0;
                        }
                    }
                }
                n += 1;
                next = now + if n < 5 { 150 } else { 1000 };
            }
        }
        std::thread::sleep(Duration::from_millis(50));
    }
}

fn recv_loop(sh: &Shared, s: &Settings) -> Result<(), String> {
    let mut conn = establish(s, sh.status, sh.stop)?;
    *sh.live.lock().unwrap() = Some(conn.live.clone());
    sh.status.state("Connected, waiting for audio");
    let mut buf = [0u8; 2048];
    let now = sh.now();
    let (mut last_alive, mut last_audio) = (now, now);
    let (mut errors, mut cand_ms, mut cand_n) = (0, 0u32, 0u32);
    let mut standby = false;
    let mut first_audio = true; // the first packet of a session sets the frame size at once
    while !sh.stop.load(Ordering::Relaxed) {
        if let Some(why) = sh.rebind.lock().unwrap().take() {
            sh.status.update(|x| {
                x.reconnects += 1;
                x.state = format!("Reconnecting ({why})");
            });
            *sh.live.lock().unwrap() = None;
            conn = establish(s, sh.status, sh.stop)?;
            *sh.live.lock().unwrap() = Some(conn.live.clone());
            sh.status.state("Connected, waiting for audio");
            last_alive = sh.now();
            last_audio = last_alive;
            first_audio = true;
        }
        let n = match conn.live.sock.recv(&mut buf) {
            Ok(n) => {
                errors = 0;
                n
            }
            Err(e) if matches!(e.kind(), io::ErrorKind::WouldBlock | io::ErrorKind::TimedOut) => {
                let now = sh.now();
                if now - last_alive > AGENT_QUIET_REBIND_MS {
                    *sh.rebind.lock().unwrap() = Some("agent not responding".into());
                    last_alive = now;
                } else if now - last_audio > 3000 && !standby {
                    sh.status.state(format!("No audio from {} (is anything playing?)", s.agent));
                }
                continue;
            }
            Err(_) => {
                errors += 1;
                if errors >= 5 {
                    *sh.rebind.lock().unwrap() = Some("socket error".into());
                    errors = 0;
                }
                std::thread::sleep(Duration::from_millis(100));
                continue;
            }
        };
        let d = &buf[..n];
        let (h, payload) = match conn.audio.open(d) {
            Ok(x) => x,
            Err(_) => match conn.ctl.open(d) {
                Ok(x) => x,
                Err(_) => continue,
            },
        };
        let now = sh.now();
        last_alive = now;
        match h.ptype {
            PacketType::Control => {
                match Message::decode(&payload) {
                    Ok(Some((Message::ConfigApplied { req, codec, .. }, _))) => {
                        sh.status.cfg_ack.store(req, Ordering::Relaxed);
                        sh.status.update(|x| x.note = if (codec == CODEC_PCM) != sh.status.want_lossless.load(Ordering::Relaxed) { "The sender is using a different codec than requested".into() } else { String::new() });
                    }
                    Ok(Some((Message::ConfigRejected { req, .. }, _))) => {
                        sh.status.cfg_ack.store(req, Ordering::Relaxed);
                        sh.status.update(|x| x.note = "The sender refused the codec change".into());
                    }
                    _ => {}
                }
                match Message::decode(&payload) {
                    Ok(Some((Message::SetVolume { percent }, _))) => {
                        sh.status.vol_apply.store(percent as u16 + 1, Ordering::Relaxed);
                    }
                    Ok(Some((Message::VolumeLevel { percent }, _))) if sh.status.snapshot().remote_volume != Some(percent) => {
                        sh.status.update(|x| x.remote_volume = Some(percent));
                    }
                    _ => {}
                }
                if let Ok(Some((Message::PlayHereSupported, _))) = Message::decode(&payload) {
                    if !sh.status.snapshot().can_play_here {
                        sh.status.update(|x| x.can_play_here = true);
                    }
                }
                if let Ok(Some((Message::ActiveState { active }, _))) = Message::decode(&payload) {
                    if !active {
                        standby = true;
                        sh.status.update(|x| {
                            x.standby = true;
                            x.state = "Standby: the agent is sending audio to another device".into();
                        });
                    } else if standby {
                        standby = false;
                        sh.status.update(|x| {
                            x.standby = false;
                            x.state = "Connected".into();
                        });
                    }
                }
            }
            PacketType::Audio => {
                if h.frag_cnt != 1 {
                    continue;
                }
                let pcm = h.flags & FLAG_PCM != 0;
                let ms = frame_ms_from_flags(h.flags);
                // A lossless packet is exactly one whole 5 ms frame; its size doubles as the codec marker
                // downstream (an Opus packet is never 960 bytes), so a packet whose flag and size
                // disagree is dropped rather than played as noise.
                let plausible = if pcm { payload.len() == PCM_FRAME_BYTES && ms == 5 } else { (1..PCM_FRAME_BYTES).contains(&payload.len()) };
                if !plausible {
                    continue;
                }
                last_audio = now;
                {
                    let mut c = sh.core.lock().unwrap();
                    // A switch needs 3 packets in a row announcing the new size, so a straggler of the old
                    // size that arrives late (reordering across the switch) cannot make the buffer flap.
                    if ms != 0 && ms != c.jb.frame_ms {
                        if ms == cand_ms {
                            cand_n += 1;
                        } else {
                            cand_ms = ms;
                            cand_n = 1;
                        }
                        if cand_n >= 3 || first_audio {
                            let old = std::mem::replace(&mut c.jb, JitterBuffer::for_frame_ms(ms));
                            c.base.add(&old.counters);
                            c.frame_changed = true;
                            cand_ms = 0;
                            cand_n = 0;
                        }
                    } else {
                        cand_n = 0;
                    }
                    c.jb.push(h.seq as i64, payload, now);
                    first_audio = false;
                }
                sh.cv.notify_all();
                sh.status.update(|x| {
                    x.lossless = pcm;
                    if ms != 0 {
                        x.frame_ms = ms;
                    }
                });
            }
            _ => {}
        }
    }
    Ok(())
}

fn publish(sh: &Shared, c: &Core) {
    let mut total = c.base;
    total.add(&c.jb.counters);
    sh.status.update(|x| {
        x.counters = total;
        x.buffer_ms = c.jb.depth_frames() as u32 * c.jb.frame_ms;
        x.target_ms = c.jb.target_frames as u32 * c.jb.frame_ms;
        x.jitter_ms = c.jb.jitter_ms;
    });
}

fn play_loop(sh: &Shared, sink: Box<dyn Sink>) {
    play(sh, sink);
    publish(sh, &sh.core.lock().unwrap()); // the final numbers, not the ones from up to 250 ms ago
}

fn play(sh: &Shared, mut sink: Box<dyn Sink>) {
    let mut concealer = Concealer::default();
    let mut decoder: Option<(u32, OpusDecoder)> = None;
    let mut last_publish = 0u64;
    while !sh.stop.load(Ordering::Relaxed) {
        let (out, frame_ms) = {
            let mut c = sh.core.lock().unwrap();
            if c.frame_changed {
                c.frame_changed = false;
                concealer.reset();
            }
            let now = sh.now();
            let out = c.jb.pop(now);
            let frame_ms = c.jb.frame_ms;
            if now >= last_publish + 250 {
                last_publish = now;
                publish(sh, &c);
            }
            if out == Out::Wait {
                let _ = sh.cv.wait_timeout(c, Duration::from_millis(3));
            }
            (out, frame_ms)
        };
        let pcm = match out {
            Out::Wait => continue,
            Out::Lost => concealer.conceal(48 * frame_ms as usize * 2),
            Out::Frame(p) if p.len() == PCM_FRAME_BYTES => {
                concealer.accept(p.chunks_exact(2).map(|b| i16::from_le_bytes([b[0], b[1]])).collect())
            }
            Out::Frame(p) => {
                if decoder.as_ref().map(|d| d.0) != Some(frame_ms) {
                    match OpusDecoder::new(frame_ms * 1000) {
                        Ok(d) => decoder = Some((frame_ms, d)),
                        Err(e) => return sh.fail(format!("Opus decoder: {e}")),
                    }
                }
                match decoder.as_mut().unwrap().1.decode(Some(&p)) {
                    Ok(v) if !v.is_empty() => concealer.accept(v),
                    _ => concealer.conceal(48 * frame_ms as usize * 2), // a corrupt packet counts as lost
                }
            }
        };
        if pcm.is_empty() {
            continue;
        }
        if let Err(e) = sink.write(&pcm) {
            return sh.fail(format!("playback: {e}"));
        }
        sh.status.update(|x| {
            if !x.standby && x.state != "Playing" {
                x.state = "Playing".into();
            }
        });
    }
}
