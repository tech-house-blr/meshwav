//! meshwav-agent: capture system audio, encode Opus, send AEAD-sealed UDP to the receiver that
//! most recently authenticated. Receivers subscribe by sending sealed `Punch` packets about
//! once a second; the agent stops sending after `PEER_TIMEOUT` of silence.
//!
//!   meshwav-agent [--bind IP:PORT] [--key-file PATH] [--source SINK.monitor] [--kbps 160] [--frame-ms 10]
//!
//! Defaults: bind = <tailscale ipv4>:47900, key-file = ~/.config/meshwav/key (created 0600 on
//! first run). The listener refuses non-tailnet addresses. Capture uses `parec` (PulseAudio or
//! PipeWire-pulse) on the default sink's monitor.
use audio_codec::opus::OpusEncoder;
use audio_protocol::crypto::{Direction, Opener, Sealer};
use audio_protocol::packet::{Header, PacketType};
use audio_transport::is_tailnet_or_loopback;
use std::io::Read;
use std::net::{SocketAddr, UdpSocket};
use std::process::{Command, Stdio};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

const STREAM_ID: u32 = 1;
const PEER_TIMEOUT: Duration = Duration::from_secs(5);
const DEFAULT_PORT: u16 = 47900;

fn arg(args: &[String], name: &str) -> Option<String> {
    args.iter()
        .position(|a| a == name)
        .and_then(|i| args.get(i + 1).cloned())
}

fn hex(b: &[u8]) -> String {
    b.iter().map(|x| format!("{x:02x}")).collect()
}

fn load_or_create_key(path: &std::path::Path) -> std::io::Result<[u8; 32]> {
    if let Ok(s) = std::fs::read_to_string(path) {
        let s = s.trim();
        if s.len() == 64 {
            let mut k = [0u8; 32];
            for i in 0..32 {
                k[i] = u8::from_str_radix(&s[2 * i..2 * i + 2], 16)
                    .map_err(|_| std::io::Error::other("key file is not hex"))?;
            }
            return Ok(k);
        }
        return Err(std::io::Error::other("key file must hold 64 hex chars"));
    }
    let mut k = [0u8; 32];
    getrandom::getrandom(&mut k).map_err(std::io::Error::other)?;
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    #[cfg(unix)]
    {
        use std::io::Write;
        use std::os::unix::fs::OpenOptionsExt;
        let mut f = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(path)?;
        writeln!(f, "{}", hex(&k))?;
    }
    #[cfg(not(unix))]
    std::fs::write(path, format!("{}\n", hex(&k)))?;
    Ok(k)
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
    let key_path = arg(&args, "--key-file")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|| {
            std::path::PathBuf::from(std::env::var("HOME").expect("HOME"))
                .join(".config/meshwav/key")
        });
    let key = load_or_create_key(&key_path).unwrap_or_else(|e| {
        eprintln!("key: {e}");
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
    println!(
        "pairing: host={} port={} key={}",
        addr.ip(),
        addr.port(),
        hex(&key)
    );

    let mut enc = OpusEncoder::new(frame_ms * 1000, kbps, false).expect("encoder");
    let frame_samples = enc.frame_samples();

    let peer: Arc<Mutex<Option<(SocketAddr, Instant)>>> = Arc::new(Mutex::new(None));
    {
        let (sock, peer) = (sock.try_clone().unwrap(), peer.clone());
        std::thread::spawn(move || {
            let mut opener = Opener::new(&key, Direction::ReceiverToAgent, STREAM_ID);
            let mut buf = [0u8; 2048];
            loop {
                let Ok((n, from)) = sock.recv_from(&mut buf) else {
                    continue;
                };
                if let Ok((h, _)) = opener.open(&buf[..n]) {
                    if h.ptype == PacketType::Punch {
                        let mut p = peer.lock().unwrap();
                        if p.map(|(a, _)| a != from).unwrap_or(true) {
                            println!("receiver connected: {from}");
                        }
                        *p = Some((from, Instant::now()));
                    }
                }
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

    let mut sealer = Sealer::new(&key, Direction::AgentToReceiver, STREAM_ID, 0);
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
                continue 'outer;
            }
            ts = ts.wrapping_add(frame_samples as u32);
            let target = match *peer.lock().unwrap() {
                Some((a, seen)) if seen.elapsed() < PEER_TIMEOUT => Some(a),
                _ => None,
            };
            match target {
                None => {
                    if connected {
                        println!("receiver timed out; idle");
                        connected = false;
                    }
                }
                Some(to) => {
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
                    if sealer.seal(h, frame, &mut out).is_err() {
                        eprintln!("sequence space exhausted; restart the agent to rekey");
                        let _ = child.kill();
                        break 'outer;
                    }
                    let _ = sock.send_to(&out, to);
                    sent += 1;
                }
            }
            if last_report.elapsed() > Duration::from_secs(10) && connected {
                println!("sent {sent} packets");
                last_report = Instant::now();
            }
        }
    }
}
