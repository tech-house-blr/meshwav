//! The receiver against a fake agent built from the real protocol crate (`Server`, `Sealer`): pairing,
//! playback with and without loss, standby, goodbye, and the safety refusals. Real time, real UDP on
//! loopback, so they take a couple of seconds each.
use audio_protocol::control::Message;
use audio_protocol::crypto::{Direction, Opener, Sealer};
use audio_protocol::handshake::{Keypair, PublicKey};
use audio_protocol::packet::{frame_flags, Header, PacketType, FLAG_PCM};
use audio_protocol::server::{Outcome, Server};
use meshwav_receiver::{pair, run, Settings, Status};
use std::net::{SocketAddr, UdpSocket};
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

struct Plan {
    /// Send frames 0..total at one per 5 ms, skipping those `drop` selects.
    total: u32,
    drop: fn(u32) -> bool,
    /// Tell the receiver it is standby from this frame on instead of sending audio.
    standby_from: Option<u32>,
    /// Send 10 ms Opus frames of a sine wave instead of lossless 5 ms PCM.
    opus: bool,
}

struct Agent {
    addr: SocketAddr,
    key: PublicKey,
    stop: Arc<AtomicBool>,
    bye: Arc<AtomicBool>,
    punches: Arc<AtomicU32>,
    plan: Arc<Mutex<Option<Plan>>>,
    code: Arc<Mutex<Option<String>>>,
    handle: Option<std::thread::JoinHandle<()>>,
}

fn marker(seq: u32) -> i16 {
    (seq % 30000) as i16 + 1
}

/// 5 ms of stereo PCM where every sample is the frame's marker.
fn pcm_frame(seq: u32) -> Vec<u8> {
    std::iter::repeat(marker(seq).to_le_bytes()).take(480).flatten().collect()
}

impl Agent {
    fn start() -> Self {
        Self::start_with(Keypair::generate())
    }

    fn start_with(id: Keypair) -> Self {
        let sock = UdpSocket::bind("127.0.0.1:0").unwrap();
        sock.set_read_timeout(Some(Duration::from_millis(2))).unwrap();
        let addr = sock.local_addr().unwrap();
        let key = id.public;
        let (stop, bye, punches) = (Arc::new(AtomicBool::new(false)), Arc::new(AtomicBool::new(false)), Arc::new(AtomicU32::new(0)));
        let plan: Arc<Mutex<Option<Plan>>> = Arc::new(Mutex::new(None));
        let code: Arc<Mutex<Option<String>>> = Arc::new(Mutex::new(None));
        let (s2, b2, p2, pl2, c2) = (stop.clone(), bye.clone(), punches.clone(), plan.clone(), code.clone());
        let handle = std::thread::spawn(move || {
            let mut server = Server::new(id, vec![]);
            let t0 = Instant::now();
            let mut peer: Option<(SocketAddr, [u8; 32], Sealer, Opener, Opener)> = None;
            let mut started: Option<Instant> = None;
            let mut sent = 0u32;
            let mut last_keepalive = Instant::now();
            let mut buf = [0u8; 2048];
            let mut opus = audio_codec::opus::OpusEncoder::new(10_000, 160, false).unwrap();
            while !s2.load(Ordering::Relaxed) {
                if let Some(c) = c2.lock().unwrap().take() {
                    assert!(server.open_pairing(&c, t0.elapsed().as_millis() as u64));
                }
                if let Ok((n, from)) = sock.recv_from(&mut buf) {
                    let d = &buf[..n];
                    match server.handle(from, d, t0.elapsed().as_millis() as u64) {
                        Outcome::Reply(r) => {
                            sock.send_to(&r, from).unwrap();
                        }
                        Outcome::Established { keys, .. } => {
                            peer = Some((
                                from,
                                keys.to_receiver,
                                Sealer::new(&keys.to_receiver, Direction::AgentToReceiver, 2, 0),
                                Opener::new(&keys.to_agent, Direction::ReceiverToAgent, 1),
                                Opener::new(&keys.to_agent, Direction::ReceiverToAgent, 2),
                            ));
                            started = None;
                            sent = 0;
                        }
                        Outcome::None | Outcome::Rejected(_) => {
                            if let Some((_, _, _, audio_in, ctl_in)) = peer.as_mut() {
                                if let Ok((h, _)) = audio_in.open(d) {
                                    if h.ptype == PacketType::Punch {
                                        p2.fetch_add(1, Ordering::Relaxed);
                                    }
                                } else if let Ok((h, p)) = ctl_in.open(d) {
                                    if h.ptype == PacketType::Control && matches!(Message::decode(&p), Ok(Some((Message::Bye { .. }, _)))) {
                                        b2.store(true, Ordering::Relaxed);
                                    }
                                }
                            }
                        }
                    }
                }
                let Some((to, audio_key, ctl_out, _, _)) = peer.as_mut() else { continue };
                let hdr = |t| Header { ptype: t, stream_id: 0, seq: 0, timestamp: 0, frag_idx: 0, frag_cnt: 1, flags: 0 };
                if last_keepalive.elapsed() > Duration::from_millis(500) {
                    last_keepalive = Instant::now();
                    let mut o = vec![];
                    ctl_out.seal(hdr(PacketType::Punch), &[], &mut o).unwrap();
                    sock.send_to(&o, *to).unwrap();
                }
                let plan = pl2.lock().unwrap();
                let Some(plan) = plan.as_ref() else { continue };
                let t = *started.get_or_insert_with(Instant::now);
                while sent < plan.total && t.elapsed() >= Duration::from_millis(if plan.opus { 10 } else { 5 } * sent as u64) {
                    let mut o = vec![];
                    if plan.standby_from == Some(sent) {
                        ctl_out.seal(hdr(PacketType::Control), &Message::ActiveState { active: false }.encode(), &mut o).unwrap();
                        sock.send_to(&o, *to).unwrap();
                        sent = plan.total;
                    } else if plan.opus {
                        let samples: Vec<i16> = (0..480).flat_map(|i| {
                            let t = (sent as usize * 480 + i) as f32 / 48_000.0;
                            let v = ((t * 440.0 * std::f32::consts::TAU).sin() * 12_000.0) as i16;
                            [v, v]
                        }).collect();
                        let pkt = opus.encode(&samples).unwrap().to_vec();
                        let mut h = hdr(PacketType::Audio);
                        h.flags = frame_flags(10);
                        Sealer::new(audio_key, Direction::AgentToReceiver, 1, sent).seal(h, &pkt, &mut o).unwrap();
                        sock.send_to(&o, *to).unwrap();
                    } else if !(plan.drop)(sent) {
                        let mut h = hdr(PacketType::Audio);
                        h.flags = FLAG_PCM | frame_flags(5);
                        // One sealer per frame, started at the frame's number, so a dropped frame leaves a gap in the sequence.
                        Sealer::new(audio_key, Direction::AgentToReceiver, 1, sent).seal(h, &pcm_frame(sent), &mut o).unwrap();
                        sock.send_to(&o, *to).unwrap();
                    }
                    sent += 1;
                }
            }
        });
        Agent { addr, key, stop, bye, punches, plan, code, handle: Some(handle) }
    }

    fn open_pairing(&self, code: &str) {
        *self.code.lock().unwrap() = Some(code.into());
        std::thread::sleep(Duration::from_millis(20));
    }

    fn settings(&self, id: &Keypair) -> Settings {
        Settings { agent: self.addr, agent_key: self.key, identity: id.clone(), name: "test pc".into() }
    }
}

impl Drop for Agent {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(h) = self.handle.take() {
            let _ = h.join();
        }
    }
}

/// A sink that records every sample and paces playback like a sound server, slightly faster than real
/// time so the test's own scheduling jitter never builds a backlog (the buffer would rightly trim one).
fn recorder() -> (Box<dyn meshwav_receiver::sink::Sink>, Arc<Mutex<Vec<i16>>>) {
    let got = Arc::new(Mutex::new(Vec::new()));
    let g2 = got.clone();
    let sink = move |pcm: &[i16]| {
        g2.lock().unwrap().extend_from_slice(pcm);
        std::thread::sleep(Duration::from_micros(pcm.len() as u64 * 800_000 / (48_000 * 2)));
        Ok(())
    };
    (Box::new(sink), got)
}

fn wait_for(secs: u64, mut f: impl FnMut() -> bool) -> bool {
    let end = Instant::now() + Duration::from_secs(secs);
    while Instant::now() < end {
        if f() {
            return true;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    false
}

/// The frames played, as their markers, with concealed frames (which carry a faded copy) kept in place.
fn frames(samples: &[i16], frame_samples: usize) -> Vec<i16> {
    samples.chunks(frame_samples).map(|c| c[c.len() / 2]).collect()
}

fn paired(agent: &Agent, id: &Keypair) {
    agent.open_pairing("123456");
    pair(&agent.settings(id), "123456", &AtomicBool::new(false)).expect("pairing");
}

#[test]
fn pairs_with_a_code_then_plays_the_stream_bit_exact() {
    let agent = Agent::start();
    let id = Keypair::generate();
    paired(&agent, &id);
    *agent.plan.lock().unwrap() = Some(Plan { total: 400, drop: |_| false, standby_from: None, opus: false });

    let (sink, got) = recorder();
    let (stop, status) = (Arc::new(AtomicBool::new(false)), Arc::new(Status::default()));
    let s = agent.settings(&id);
    let (st2, stat2) = (stop.clone(), status.clone());
    let t = std::thread::spawn(move || run(&s, sink, &st2, &stat2));
    let ok = wait_for(10, || got.lock().unwrap().len() >= 400 * 480);
    if !ok {
        { let g = got.lock().unwrap(); let f = frames(&g, 480); eprintln!("played {} frames, first {:?} last {:?} gaps {:?}", f.len(), &f[..5.min(f.len())], &f[f.len().saturating_sub(3)..], f.windows(2).enumerate().filter(|(_, w)| w[1] != w[0] + 1).collect::<Vec<_>>()); }
        eprintln!("{:?}", status.snapshot());
    }
    assert!(ok, "audio did not arrive");
    stop.store(true, Ordering::Relaxed);
    t.join().unwrap().unwrap();

    let played = got.lock().unwrap().clone();
    let f = frames(&played, 480);
    assert!(f.windows(2).all(|w| w[1] == w[0] + 1), "frames must play in order with nothing missing or altered");
    assert!(played.chunks(480).all(|c| c.iter().all(|s| *s == c[0])), "samples within a frame are untouched");
    let snap = status.snapshot();
    assert!(snap.lossless && snap.frame_ms == 5, "{snap:?}");
    assert_eq!(snap.counters.lost, 0, "{snap:?}");
    assert!(agent.punches.load(Ordering::Relaxed) >= 3, "keepalives must reach the agent");
}

#[test]
fn lost_packets_are_concealed_in_place() {
    let agent = Agent::start();
    let id = Keypair::generate();
    paired(&agent, &id);
    *agent.plan.lock().unwrap() = Some(Plan { total: 400, drop: |i| i % 10 == 5 && i > 30, standby_from: None, opus: false });

    let (sink, got) = recorder();
    let (stop, status) = (Arc::new(AtomicBool::new(false)), Arc::new(Status::default()));
    let s = agent.settings(&id);
    let (st2, stat2) = (stop.clone(), status.clone());
    let t = std::thread::spawn(move || run(&s, sink, &st2, &stat2));
    let ok = wait_for(10, || got.lock().unwrap().len() >= 400 * 480);
    if !ok {
        eprintln!("played {} frames, {:?}", got.lock().unwrap().len() / 480, status.snapshot());
    }
    assert!(ok, "audio did not arrive");
    stop.store(true, Ordering::Relaxed);
    t.join().unwrap().unwrap();

    let snap = status.snapshot();
    let dropped = (31..400).filter(|i| i % 10 == 5).count() as u64;
    assert_eq!(snap.counters.lost, dropped, "every dropped frame is counted once: {snap:?}");
    let played = got.lock().unwrap().clone();
    // No frame is missing: the output has the same length as if nothing had been lost.
    assert_eq!(played.len(), 400 * 480, "concealment fills every gap");
}

#[test]
fn standby_is_shown_and_the_receiver_keeps_listening() {
    let agent = Agent::start();
    let id = Keypair::generate();
    paired(&agent, &id);
    *agent.plan.lock().unwrap() = Some(Plan { total: 100, drop: |_| false, standby_from: Some(60), opus: false });

    let (sink, _got) = recorder();
    let (stop, status) = (Arc::new(AtomicBool::new(false)), Arc::new(Status::default()));
    let s = agent.settings(&id);
    let (st2, stat2) = (stop.clone(), status.clone());
    let t = std::thread::spawn(move || run(&s, sink, &st2, &stat2));
    assert!(wait_for(10, || status.snapshot().standby), "standby never shown");
    assert!(status.snapshot().state.starts_with("Standby"), "{:?}", status.snapshot().state);
    stop.store(true, Ordering::Relaxed);
    t.join().unwrap().unwrap();
}

#[test]
fn stopping_says_goodbye_so_the_agent_restores_its_output() {
    let agent = Agent::start();
    let id = Keypair::generate();
    paired(&agent, &id);
    *agent.plan.lock().unwrap() = Some(Plan { total: 200, drop: |_| false, standby_from: None, opus: false });

    let (sink, got) = recorder();
    let (stop, status) = (Arc::new(AtomicBool::new(false)), Arc::new(Status::default()));
    let s = agent.settings(&id);
    let (st2, stat2) = (stop.clone(), status.clone());
    let t = std::thread::spawn(move || run(&s, sink, &st2, &stat2));
    assert!(wait_for(10, || got.lock().unwrap().len() > 480 * 20));
    stop.store(true, Ordering::Relaxed);
    t.join().unwrap().unwrap();
    assert!(wait_for(2, || agent.bye.load(Ordering::Relaxed)), "the agent never heard a goodbye");
}

#[test]
fn a_wrong_code_does_not_pair() {
    let agent = Agent::start();
    agent.open_pairing("123456");
    let id = Keypair::generate();
    let err = pair(&agent.settings(&id), "654321", &AtomicBool::new(false)).unwrap_err();
    assert!(err.contains("no answer"), "{err}");
}

#[test]
fn an_agent_with_another_key_is_refused() {
    let agent = Agent::start();
    let id = Keypair::generate();
    paired(&agent, &id);
    let mut s = agent.settings(&id);
    s.agent_key = [9; 32]; // the pinned key no longer matches what the agent presents
    let (sink, _) = recorder();
    let err = run(&s, sink, &AtomicBool::new(false), &Status::default()).unwrap_err();
    assert!(err.contains("not the one this receiver paired with"), "{err}");
}

#[test]
fn only_tailnet_or_loopback_agents_are_contacted() {
    let id = Keypair::generate();
    let s = Settings { agent: "8.8.8.8:47900".parse().unwrap(), agent_key: [1; 32], identity: id, name: "x".into() };
    let err = pair(&s, "123456", &AtomicBool::new(false)).unwrap_err();
    assert!(err.contains("refusing to connect"), "{err}");
    let (sink, _) = recorder();
    assert!(run(&s, sink, &AtomicBool::new(false), &Status::default()).unwrap_err().contains("refusing to connect"));
}

#[test]
fn opus_streams_are_decoded_and_played() {
    let agent = Agent::start();
    let id = Keypair::generate();
    paired(&agent, &id);
    *agent.plan.lock().unwrap() = Some(Plan { total: 200, drop: |_| false, standby_from: None, opus: true });

    let (sink, got) = recorder();
    let (stop, status) = (Arc::new(AtomicBool::new(false)), Arc::new(Status::default()));
    let s = agent.settings(&id);
    let (st2, stat2) = (stop.clone(), status.clone());
    let t = std::thread::spawn(move || run(&s, sink, &st2, &stat2));
    assert!(wait_for(10, || got.lock().unwrap().len() >= 200 * 960), "audio did not arrive");
    stop.store(true, Ordering::Relaxed);
    t.join().unwrap().unwrap();

    let snap = status.snapshot();
    assert!(!snap.lossless && snap.frame_ms == 10, "{snap:?}");
    assert_eq!(snap.counters.lost, 0, "{snap:?}");
    let played = got.lock().unwrap().clone();
    assert_eq!(played.len(), 200 * 960);
    // A 440 Hz tone at amplitude 12000 comes out of the decoder as a tone, not silence or noise.
    let tail = &played[played.len() / 2..];
    let peak = tail.iter().map(|s| (*s as i32).abs()).max().unwrap();
    assert!((9_000..=15_000).contains(&peak), "peak {peak}");
    let crossings = tail.chunks(2).map(|c| c[0]).collect::<Vec<_>>().windows(2).filter(|w| (w[0] < 0) != (w[1] < 0)).count();
    let seconds = tail.len() as f32 / 2.0 / 48_000.0;
    let hz = crossings as f32 / 2.0 / seconds;
    assert!((420.0..=460.0).contains(&hz), "measured {hz} Hz");
}

#[test]
fn a_sender_is_found_only_while_it_shows_a_code_and_pairs_with_just_the_code() {
    use meshwav_receiver::discover::probe;
    let agent = Agent::start();
    let id = Keypair::generate();
    assert!(probe(&[agent.addr], Duration::from_millis(700)).is_empty(), "a closed sender must stay silent");
    agent.open_pairing("654321");
    let found = probe(&[agent.addr], Duration::from_millis(1500));
    assert_eq!(found.len(), 1);
    assert_eq!((found[0].addr, found[0].key), (agent.addr, agent.key));
    // The key came from the probe, not from the user.
    let s = Settings { agent: found[0].addr, agent_key: found[0].key, identity: id, name: "test pc".into() };
    pair(&s, "654321", &AtomicBool::new(false)).expect("pairing with a discovered key");
}

#[test]
fn a_sender_found_by_code_is_only_tried_after_its_fingerprint_is_confirmed() {
    use meshwav_receiver::discover::{pair_with_senders, Found};
    use std::net::UdpSocket;
    let agent = Agent::start();
    agent.open_pairing("246810");
    let real = Found { addr: agent.addr, key: agent.key };
    // An impostor answers the probe with a key of its own and records everything sent to it.
    let impostor = UdpSocket::bind("127.0.0.1:0").unwrap();
    impostor.set_read_timeout(Some(Duration::from_millis(300))).unwrap();
    let fake = Found { addr: impostor.local_addr().unwrap(), key: Keypair::generate().public };
    let stop = AtomicBool::new(false);
    let want = real.fingerprint();

    // Declined: nothing is sent to anyone, and the real sender's pairing window is still usable.
    let r = pair_with_senders(vec![fake.clone(), real.clone()], "246810", Keypair::generate(), "pc".into(), &stop, &mut |_| false);
    assert!(r.is_err());
    let mut buf = [0u8; 256];
    assert!(impostor.recv_from(&mut buf).is_err(), "nothing may reach a sender that was not confirmed");

    // The person compares fingerprints: the impostor's differs, so only the real sender is tried.
    let seen = std::cell::RefCell::new(vec![]);
    let r = pair_with_senders(vec![fake.clone(), real.clone()], "246810", Keypair::generate(), "pc".into(), &stop, &mut |f| {
        seen.borrow_mut().push(f.addr);
        f.fingerprint() == want
    });
    assert_eq!(r.expect("the confirmed sender pairs").key, real.key);
    assert_eq!(*seen.borrow(), vec![fake.addr, real.addr], "every sender is asked about");
    assert!(impostor.recv_from(&mut buf).is_err(), "the impostor never received a handshake message");
}
