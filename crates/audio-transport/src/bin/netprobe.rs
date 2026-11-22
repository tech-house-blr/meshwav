//! netprobe: measure UDP RTT, loss, reordering and jitter between two tailnet hosts using real
//! Meshwav packets (AEAD-sealed, 20-byte header). Listeners refuse non-tailnet addresses.
//!
//!   netprobe server --bind 100.x.y.z:47900 --key <64 hex>
//!   netprobe client --bind 100.a.b.c:0 --target 100.x.y.z:47900 --key <64 hex>
//!                   [--count 3000] [--interval-us 10000] [--size 200]
use audio_protocol::crypto::{Direction, Opener, Sealer};
use audio_protocol::packet::{Header, PacketType, MAX_PAYLOAD};
use audio_transport::is_tailnet_or_loopback;
use std::net::{SocketAddr, UdpSocket};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

const STREAM: u32 = 0xA11D_0001;

fn arg(args: &[String], name: &str) -> Option<String> {
    args.iter().position(|a| a == name).and_then(|i| args.get(i + 1).cloned())
}

fn key_from_hex(s: &str) -> [u8; 32] {
    assert!(s.len() == 64, "--key must be 64 hex chars");
    let mut k = [0u8; 32];
    for i in 0..32 { k[i] = u8::from_str_radix(&s[2 * i..2 * i + 2], 16).expect("bad hex") }
    k
}

fn bind(addr: &str) -> UdpSocket {
    let a: SocketAddr = addr.parse().expect("bad --bind");
    if !is_tailnet_or_loopback(a.ip()) {
        eprintln!("refusing to bind {}: not a tailnet or loopback address", a.ip());
        std::process::exit(2);
    }
    UdpSocket::bind(a).expect("bind failed")
}

fn hdr() -> Header {
    Header { ptype: PacketType::Audio, stream_id: 0, seq: 0, timestamp: 0, frag_idx: 0, frag_cnt: 1, flags: 0 }
}

fn server(args: &[String]) {
    let key = key_from_hex(&arg(args, "--key").expect("--key"));
    let sock = bind(&arg(args, "--bind").expect("--bind"));
    sock.set_read_timeout(Some(Duration::from_millis(500))).unwrap();
    let mut opener = Opener::new(&key, Direction::AgentToReceiver, STREAM);
    let mut sealer = Sealer::new(&key, Direction::ReceiverToAgent, STREAM, 0);
    let idle: u64 = arg(args, "--idle-secs").map(|v| v.parse().unwrap()).unwrap_or(10);
    println!("listening on {}", sock.local_addr().unwrap());
    let (mut n, mut bad, mut last) = (0u64, 0u64, Instant::now());
    let mut buf = [0u8; 2048];
    let mut out = Vec::with_capacity(1500);
    let mut peer: Option<SocketAddr> = None;
    loop {
        match sock.recv_from(&mut buf) {
            Ok((len, from)) => match opener.open(&buf[..len]) {
                Ok((h, payload)) => {
                    peer.get_or_insert(from);
                    if Some(from) != peer { bad += 1; continue }
                    last = Instant::now();
                    n += 1;
                    out.clear();
                    let mut rh = hdr();
                    rh.timestamp = h.timestamp;
                    if sealer.seal(rh, &payload, &mut out).is_ok() { let _ = sock.send_to(&out, from); }
                }
                Err(_) => bad += 1,
            },
            Err(_) => if peer.is_some() && last.elapsed().as_secs() >= idle { break },
        }
    }
    println!("echoed {n}, dropped {bad} unauthenticated/invalid packets");
}

fn pct(sorted: &[f64], p: f64) -> f64 {
    sorted[((sorted.len() - 1) as f64 * p).round() as usize]
}

fn client(args: &[String]) {
    let key = key_from_hex(&arg(args, "--key").expect("--key"));
    let sock = bind(&arg(args, "--bind").unwrap_or("0.0.0.0:0".into()));
    let target: SocketAddr = arg(args, "--target").expect("--target").parse().expect("bad --target");
    let count: u32 = arg(args, "--count").map(|v| v.parse().unwrap()).unwrap_or(3000);
    let interval = Duration::from_micros(arg(args, "--interval-us").map(|v| v.parse().unwrap()).unwrap_or(10_000));
    let size: usize = arg(args, "--size").map(|v| v.parse().unwrap()).unwrap_or(200).clamp(16, MAX_PAYLOAD);
    sock.connect(target).unwrap();
    let rx_sock = sock.try_clone().unwrap();
    rx_sock.set_read_timeout(Some(Duration::from_millis(200))).unwrap();
    let t0 = Instant::now();
    let rtts: Arc<Mutex<Vec<(u32, f64)>>> = Arc::new(Mutex::new(Vec::new()));
    let done = Arc::new(Mutex::new(false));
    let (r2, d2) = (rtts.clone(), done.clone());
    let rx = std::thread::spawn(move || {
        let mut opener = Opener::new(&key, Direction::ReceiverToAgent, STREAM);
        let mut buf = [0u8; 2048];
        let mut after_done = 0;
        loop {
            match rx_sock.recv(&mut buf) {
                Ok(len) => {
                    if let Ok((h, p)) = opener.open(&buf[..len]) {
                        let sent_us = u64::from_be_bytes(p[..8].try_into().unwrap());
                        let rtt = t0.elapsed().as_micros() as f64 - sent_us as f64;
                        r2.lock().unwrap().push((h.timestamp, rtt / 1000.0));
                    }
                }
                Err(_) => { if *d2.lock().unwrap() { after_done += 1; if after_done >= 5 { break } } }
            }
        }
    });
    let mut sealer = Sealer::new(&key, Direction::AgentToReceiver, STREAM, 0);
    let mut out = Vec::with_capacity(1500);
    let mut payload = vec![0u8; size];
    let mut late_sends = 0u32;
    for i in 0..count {
        let deadline = t0 + interval * i;
        let now = Instant::now();
        if now > deadline + interval { late_sends += 1 }
        if deadline > now + Duration::from_micros(300) { std::thread::sleep(deadline - now - Duration::from_micros(200)) }
        while Instant::now() < deadline {}
        payload[..8].copy_from_slice(&(t0.elapsed().as_micros() as u64).to_be_bytes());
        out.clear();
        let mut h = hdr();
        h.timestamp = i;
        sealer.seal(h, &payload, &mut out).unwrap();
        sock.send(&out).unwrap();
    }
    *done.lock().unwrap() = true;
    rx.join().unwrap();
    let got = rtts.lock().unwrap().clone();
    let mut reordered = 0;
    let mut maxts = 0;
    for &(ts, _) in &got { if ts < maxts { reordered += 1 } else { maxts = ts } }
    let mut r: Vec<f64> = got.iter().map(|x| x.1).collect();
    println!("target {target}  packets {count}  size {} B on wire  interval {:?}  offered {:.2} Mbit/s",
        size + 36, interval, (size + 36) as f64 * 8.0 / interval.as_secs_f64() / 1e6);
    println!("received {}  loss {:.3}%  reordered {}  sender late {}", got.len(), 100.0 * (count as f64 - got.len() as f64) / count as f64, reordered, late_sends);
    if r.is_empty() { println!("no replies"); return }
    let jitter: f64 = got.windows(2).map(|w| (w[1].1 - w[0].1).abs()).sum::<f64>() / (got.len().max(2) - 1) as f64;
    r.sort_by(|a, b| a.partial_cmp(b).unwrap());
    println!("RTT ms  min {:.2}  p50 {:.2}  p95 {:.2}  p99 {:.2}  max {:.2}  jitter(mean |dRTT|) {:.2}",
        r[0], pct(&r, 0.5), pct(&r, 0.95), pct(&r, 0.99), r[r.len() - 1], jitter);
    println!("approx one-way p50 {:.2} ms, p99 {:.2} ms", pct(&r, 0.5) / 2.0, pct(&r, 0.99) / 2.0);
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    match args.get(1).map(String::as_str) {
        Some("server") => server(&args),
        Some("client") => client(&args),
        _ => eprintln!("usage: netprobe server|client ... (see source header)"),
    }
}
