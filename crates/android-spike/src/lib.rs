//! Spike: the agent's send path (pairing server, Opus, sealed UDP) driven by PCM pushed from outside.
use audio_codec::opus::OpusEncoder;
use audio_protocol::crypto::{Direction, Opener, Sealer};
use audio_protocol::handshake::{self, hex, Keypair};
use audio_protocol::packet::{frame_flags, Header, PacketType, FLAG_PCM};
use audio_protocol::server::{Outcome, Server};
use audio_transport::is_tailnet_or_loopback;
use std::collections::VecDeque;
use std::net::{SocketAddr, UdpSocket};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

/// How long the phone accepts a pairing code (single use, five wrong codes still lock it).
const PAIR_WINDOW_MS: u64 = 10 * 60 * 1000;
const STREAM_ID: u32 = 1;
const CONTROL_STREAM: u32 = 2;
const FRAME_MS: u32 = 10;
const FRAME_SAMPLES: usize = 480; // 10 ms at 48 kHz
const FRAME_VALUES: usize = FRAME_SAMPLES * 2; // stereo
/// Lossless frames are always 5 ms.
const PCM_SAMPLES: usize = 240;
const PCM_VALUES: usize = PCM_SAMPLES * 2;
const MAX_QUEUE: usize = FRAME_VALUES * 20; // keep at most 200 ms

#[derive(Default)]
pub struct Source {
    pcm: Mutex<VecDeque<i16>>,
    stop: AtomicBool,
    lossless: AtomicBool,
    pub sent: AtomicU64,
    pub dropped: AtomicU64,
    pub status: Mutex<String>,
}

impl Source {
    /// Interleaved stereo s16, 48 kHz.
    pub fn push(&self, samples: &[i16]) {
        let mut q = self.pcm.lock().unwrap();
        q.extend(samples.iter().copied());
        while q.len() > MAX_QUEUE {
            q.pop_front();
            self.dropped.fetch_add(1, Ordering::Relaxed);
        }
    }
    /// Lossless sends raw 48 kHz 16-bit PCM in 5 ms packets instead of Opus.
    pub fn set_lossless(&self, on: bool) {
        self.lossless.store(on, Ordering::SeqCst);
    }
    pub fn stop(&self) {
        self.stop.store(true, Ordering::SeqCst);
    }
}

struct Peer {
    addr: SocketAddr,
    sealer: Sealer,
    keepalive: Sealer,
    opener: Opener,
    last_seen: Instant,
}

/// Start the source. Returns the pairing link and the handle.
pub fn start(bind_ip: &str, port: u16, code: &str) -> Result<(String, Arc<Source>), String> {
    let ip: std::net::IpAddr = bind_ip.parse().map_err(|_| "bad address".to_string())?;
    if !is_tailnet_or_loopback(ip) {
        return Err("refusing to bind outside the tailnet or loopback".into());
    }
    let sock = UdpSocket::bind((ip, port)).map_err(|e| format!("bind: {e}"))?;
    sock.set_read_timeout(Some(Duration::from_millis(2))).ok();
    let port = sock.local_addr().map_err(|e| e.to_string())?.port();
    let identity = Keypair::generate();
    let mut server = Server::new(identity, vec![]);
    let t0 = Instant::now();
    if !server.open_pairing_for(code, 0, PAIR_WINDOW_MS) {
        return Err("bad pairing code".into());
    }
    let link = format!("meshwav://pair?host={ip}&port={port}&pk={}&code={code}", hex(&server.public_key()));
    let src = Arc::new(Source::default());
    let s2 = src.clone();
    std::thread::spawn(move || run(sock, server, s2, t0));
    Ok((link, src))
}

fn run(sock: UdpSocket, mut server: Server, src: Arc<Source>, t0: Instant) {
    let mut enc = OpusEncoder::new(FRAME_MS * 1000, 160, false).expect("encoder");
    let mut peer: Option<Peer> = None;
    let mut buf = [0u8; 2048];
    let mut out = Vec::with_capacity(1500);
    let mut raw: Vec<u8> = Vec::with_capacity(PCM_VALUES * 2);
    let mut ts: u32 = 0;
    let mut last_ka = Instant::now();
    while !src.stop.load(Ordering::SeqCst) {
        if let Ok((n, from)) = sock.recv_from(&mut buf) {
            let d = &buf[..n];
            let now = t0.elapsed().as_millis() as u64;
            if handshake::is_handshake(d) {
                match server.handle(from, d, now) {
                    Outcome::Reply(r) => {
                        let _ = sock.send_to(&r, from);
                    }
                    Outcome::Established { keys, name, newly_paired, .. } => {
                        *src.status.lock().unwrap() = format!("{} {name} from {from}", if newly_paired { "paired" } else { "connected" });
                        peer = Some(Peer {
                            addr: from,
                            sealer: Sealer::new(&keys.to_receiver, Direction::AgentToReceiver, STREAM_ID, 0),
                            keepalive: Sealer::new(&keys.to_receiver, Direction::AgentToReceiver, CONTROL_STREAM, 0),
                            opener: Opener::new(&keys.to_agent, Direction::ReceiverToAgent, STREAM_ID),
                            last_seen: Instant::now(),
                        });
                    }
                    Outcome::Rejected(why) => *src.status.lock().unwrap() = format!("rejected: {why}"),
                    Outcome::None => {}
                }
            } else if let Some(p) = peer.as_mut() {
                if from == p.addr {
                    if let Ok((h, _)) = p.opener.open(d) {
                        if h.ptype == PacketType::Punch {
                            p.last_seen = Instant::now();
                        }
                    }
                }
            }
        }
        let online = peer.as_ref().is_some_and(|p| p.last_seen.elapsed() < Duration::from_secs(10));
        if let (Some(p), true) = (peer.as_mut(), last_ka.elapsed() >= Duration::from_secs(1)) {
            last_ka = Instant::now();
            let h = Header { ptype: PacketType::Punch, stream_id: 0, seq: 0, timestamp: 0, frag_idx: 0, frag_cnt: 1, flags: 0 };
            out.clear();
            if p.keepalive.seal(h, &[], &mut out).is_ok() {
                let _ = sock.send_to(&out, p.addr);
            }
        }
        loop {
            let lossless = src.lossless.load(Ordering::SeqCst);
            let want = if lossless { PCM_VALUES } else { FRAME_VALUES };
            let frame: Vec<i16> = {
                let mut q = src.pcm.lock().unwrap();
                if q.len() < want {
                    break;
                }
                q.drain(..want).collect()
            };
            let (Some(p), true) = (peer.as_mut(), online) else { continue };
            let (bytes, flags, samples): (&[u8], u16, usize) = if lossless {
                raw.clear();
                for v in &frame {
                    raw.extend_from_slice(&v.to_le_bytes());
                }
                (&raw, frame_flags(5) | FLAG_PCM, PCM_SAMPLES)
            } else {
                let Ok(b) = enc.encode(&frame) else { continue };
                (b, frame_flags(FRAME_MS), FRAME_SAMPLES)
            };
            let h = Header { ptype: PacketType::Audio, stream_id: 0, seq: 0, timestamp: ts, frag_idx: 0, frag_cnt: 1, flags };
            ts = ts.wrapping_add(samples as u32);
            out.clear();
            if p.sealer.seal(h, bytes, &mut out).is_ok() {
                let _ = sock.send_to(&out, p.addr);
                src.sent.fetch_add(1, Ordering::Relaxed);
            }
        }
    }
}

#[cfg(target_os = "android")]
mod jni_api {
    use super::*;
    use jni::objects::{JClass, JShortArray, JString};
    use jni::sys::{jboolean, jint, jlong, jstring};
    use jni::JNIEnv;

    fn out(env: &mut JNIEnv, s: &str) -> jstring {
        env.new_string(s).map(|j| j.into_raw()).unwrap_or(std::ptr::null_mut())
    }

    static CUR: Mutex<Option<Arc<Source>>> = Mutex::new(None);

    /// Returns the pairing link, or "error: ...".
    #[no_mangle]
    pub extern "system" fn Java_com_techhouseblr_meshwav_NativeSource_start(mut env: JNIEnv, _: JClass, ip: JString, port: jint, code: JString) -> jstring {
        let ip: String = env.get_string(&ip).map(|s| s.into()).unwrap_or_default();
        let code: String = env.get_string(&code).map(|s| s.into()).unwrap_or_default();
        if let Some(old) = CUR.lock().unwrap().take() {
            old.stop();
        }
        match start(&ip, port as u16, &code) {
            Ok((link, s)) => {
                *CUR.lock().unwrap() = Some(s);
                out(&mut env, &link)
            }
            Err(e) => out(&mut env, &format!("error: {e}")),
        }
    }

    #[no_mangle]
    pub extern "system" fn Java_com_techhouseblr_meshwav_NativeSource_push(env: JNIEnv, _: JClass, pcm: JShortArray, n: jint) {
        let Some(s) = CUR.lock().unwrap().clone() else { return };
        let mut v = vec![0i16; n.max(0) as usize];
        if env.get_short_array_region(&pcm, 0, &mut v).is_ok() {
            s.push(&v);
        }
    }

    #[no_mangle]
    pub extern "system" fn Java_com_techhouseblr_meshwav_NativeSource_setLossless(_: JNIEnv, _: JClass, on: jboolean) {
        if let Some(s) = CUR.lock().unwrap().clone() {
            s.set_lossless(on != 0);
        }
    }

    #[no_mangle]
    pub extern "system" fn Java_com_techhouseblr_meshwav_NativeSource_stats(mut env: JNIEnv, _: JClass) -> jstring {
        let s = CUR.lock().unwrap().clone();
        let t = match s {
            Some(s) => format!("sent {} dropped {} | {}", s.sent.load(Ordering::Relaxed), s.dropped.load(Ordering::Relaxed), s.status.lock().unwrap()),
            None => "stopped".into(),
        };
        out(&mut env, &t)
    }

    #[no_mangle]
    pub extern "system" fn Java_com_techhouseblr_meshwav_NativeSource_stop(_: JNIEnv, _: JClass) -> jlong {
        if let Some(s) = CUR.lock().unwrap().take() {
            s.stop();
        }
        0
    }
}
