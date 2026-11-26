//! System-audio capture on Windows. Preferably a per-process loopback of every app (see
//! `process_loopback_windows`); where that is unavailable, WASAPI loopback of the default output, through `cpal`.
//!
//! The sender reads fixed-size frames of 48 kHz stereo s16le, as `parec` gives them on Linux. Here the
//! device's own mix format (any rate, any channel count) is converted into that. Loopback delivers
//! nothing while the output is silent, so `read` fills the gap with silence at real-time pace, which
//! keeps the frame clock running the way a PulseAudio monitor does.
use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use cpal::{Sample, SampleFormat};
use std::collections::VecDeque;
use std::io::{self, Read};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{mpsc, Arc, Condvar, Mutex};
use std::time::{Duration, Instant};

/// Never hold more than this much captured audio: if the sender stalls, old audio is dropped, not queued.
const MAX_BYTES: usize = 48_000 * 4 / 5; // 200 ms
/// Bytes of 48 kHz stereo s16le per second.
const BYTES_PER_SEC: u128 = 48_000 * 4;
/// Silence is only made up once the stream is this far behind real time. Loopback delivers a block
/// about every 10 ms while anything plays, with some jitter, so a smaller margin would insert silence
/// into audio that is actually flowing.
const BEHIND_MS: u128 = 20;
/// A stall longer than this is not made up with a burst of silence; the clock is moved instead.
const RESET_MS: u128 = 200;

/// Keeps the byte stream on real time. Loopback delivers nothing while the output is idle, but the
/// sender needs a frame every 5 ms for the receiver's clock, so the gap is filled with exactly as much
/// silence as real time has passed, no more (more would run the stream fast and make the receiver trim).
struct Pacer {
    start: Instant,
    emitted: u128,
}

impl Pacer {
    fn new(now: Instant) -> Self {
        Self { start: now, emitted: 0 }
    }
    fn expected(&self, now: Instant) -> u128 {
        now.duration_since(self.start).as_micros() * BYTES_PER_SEC / 1_000_000
    }
    /// Audio of `n` bytes was handed out.
    fn sent(&mut self, n: usize, now: Instant) {
        self.emitted += n as u128;
        // data that arrived ahead of real time (a backlog): move the clock forward instead of
        // counting it as owed
        let ahead = self.emitted.saturating_sub(self.expected(now));
        if ahead > BYTES_PER_SEC * RESET_MS / 1000 {
            self.start += Duration::from_micros((ahead * 1_000_000 / BYTES_PER_SEC) as u64);
        }
    }
    /// How many bytes of silence (at most `max`, whole frames) to hand out now; 0 means keep waiting.
    fn silence_due(&mut self, max: usize, now: Instant) -> usize {
        let exp = self.expected(now);
        let behind = exp.saturating_sub(self.emitted);
        if behind > BYTES_PER_SEC * RESET_MS / 1000 {
            // after a long stall: restart the clock rather than send seconds of silence at once
            self.emitted = exp - BYTES_PER_SEC * BEHIND_MS / 1000;
        }
        let behind = exp.saturating_sub(self.emitted);
        if behind < BYTES_PER_SEC * BEHIND_MS / 1000 {
            return 0;
        }
        let n = (behind as usize).min(max);
        n - n % 4
    }
    /// Time to wait before silence could be due.
    fn wait(&self, now: Instant) -> Duration {
        let exp = self.expected(now);
        let need = (BYTES_PER_SEC * BEHIND_MS / 1000 + self.emitted).saturating_sub(exp);
        Duration::from_micros((need * 1_000_000 / BYTES_PER_SEC) as u64 + 500)
    }
}

struct Shared {
    q: Mutex<VecDeque<u8>>,
    ready: Condvar,
    stop: AtomicBool,
    /// Bytes the loopback has delivered so far (for the debug log).
    got: AtomicU64,
    /// Loudest sample (as 0..32767) since the last debug report.
    peak: AtomicU64,
}

pub struct Loopback {
    shared: Arc<Shared>,
    thread: Option<std::thread::JoinHandle<()>>,
    pacer: Pacer,
}

/// Linear-interpolation converter from any rate and channel count to 48 kHz stereo.
struct Convert {
    ch: usize,
    step: f64,
    t: f64,
    prev: [f32; 2],
}

impl Convert {
    fn push(&mut self, input: &[f32], out: &mut Vec<u8>) {
        for fr in input.chunks_exact(self.ch) {
            let cur = if self.ch == 1 { [fr[0], fr[0]] } else { [fr[0], fr[1]] };
            while self.t < 1.0 {
                for c in 0..2 {
                    let v = self.prev[c] + (cur[c] - self.prev[c]) * self.t as f32;
                    out.extend_from_slice(&((v.clamp(-1.0, 1.0) * 32767.0) as i16).to_le_bytes());
                }
                self.t += self.step;
            }
            self.t -= 1.0;
            self.prev = cur;
        }
    }
}

fn feed<T: Sample + cpal::SizedSample>(shared: Arc<Shared>, mut conv: Convert) -> impl FnMut(&[T], &cpal::InputCallbackInfo) + Send + 'static
where
    f32: cpal::FromSample<T>,
{
    let mut tmp = Vec::new();
    let mut bytes = Vec::new();
    move |data: &[T], _| {
        tmp.clear();
        tmp.extend(data.iter().map(|s| s.to_sample::<f32>()));
        bytes.clear();
        conv.push(&tmp, &mut bytes);
        shared.got.fetch_add(bytes.len() as u64, Ordering::Relaxed);
        let pk = tmp.iter().fold(0f32, |m, v| m.max(v.abs())) * 32767.0;
        shared.peak.fetch_max(pk as u64, Ordering::Relaxed);
        push(&shared, &bytes);
    }
}

/// Queue captured audio for `read`, dropping the oldest if the sender has fallen behind.
fn push(shared: &Shared, bytes: &[u8]) {
    let mut q = shared.q.lock().unwrap();
    q.extend(bytes.iter());
    let over = q.len().saturating_sub(MAX_BYTES);
    if over > 0 {
        q.drain(..over - over % 4);
    }
    drop(q);
    shared.ready.notify_all();
}

/// How often the capture thread looks at which output is the default.
const FOLLOW_EVERY: Duration = Duration::from_secs(1);

/// Open a loopback capture of `dev`, feeding `shared`.
fn open_stream(dev: &cpal::Device, shared: &Arc<Shared>) -> Result<cpal::Stream, String> {
    let cfg = dev.default_output_config().map_err(|e| e.to_string())?;
    let conv = Convert { ch: cfg.channels() as usize, step: cfg.sample_rate().0 as f64 / 48_000.0, t: 0.0, prev: [0.0; 2] };
    let err = |e| eprintln!("audio capture error: {e}");
    // Building an input stream on an output device is how cpal asks WASAPI for loopback.
    let stream = match cfg.sample_format() {
        SampleFormat::F32 => dev.build_input_stream(&cfg.config(), feed::<f32>(shared.clone(), conv), err, None),
        SampleFormat::I16 => dev.build_input_stream(&cfg.config(), feed::<i16>(shared.clone(), conv), err, None),
        SampleFormat::I32 => dev.build_input_stream(&cfg.config(), feed::<i32>(shared.clone(), conv), err, None),
        f => return Err(format!("unsupported sample format {f:?}")),
    }
    .map_err(|e| e.to_string())?;
    stream.play().map_err(|e| e.to_string())?;
    Ok(stream)
}

impl Loopback {
    pub fn start() -> Result<Self, String> {
        let shared = Arc::new(Shared { q: Mutex::new(VecDeque::new()), ready: Condvar::new(), stop: AtomicBool::new(false), got: AtomicU64::new(0), peak: AtomicU64::new(0) });
        let (tx, rx) = mpsc::channel::<Result<(), String>>();
        let s = shared.clone();
        let thread = std::thread::spawn(move || {
            // Preferred: everything the apps play, before any output's volume (see process_loopback_windows).
            match crate::process_loopback_windows::open() {
                Ok(cap) => {
                    let _ = tx.send(Ok(()));
                    info!("capturing the audio of every app, whichever output it plays on (per-process loopback)");
                    let (mut bytes, mut last_report, mut last_got) = (Vec::new(), Instant::now(), 0u64);
                    while !s.stop.load(Ordering::Relaxed) {
                        std::thread::sleep(Duration::from_millis(5));
                        bytes.clear();
                        let pk = cap.drain(&mut bytes);
                        if !bytes.is_empty() {
                            s.got.fetch_add(bytes.len() as u64, Ordering::Relaxed);
                            s.peak.fetch_max(pk as u64, Ordering::Relaxed);
                            push(&s, &bytes);
                        }
                        if last_report.elapsed() >= Duration::from_secs(10) {
                            let g = s.got.load(Ordering::Relaxed);
                            debug!("capture: {} bytes in the last 10 s, loudest sample {}", g - last_got, s.peak.swap(0, Ordering::Relaxed));
                            last_got = g;
                            last_report = Instant::now();
                        }
                    }
                    return;
                }
                Err(e) => warn!("per-process capture is not available ({e}); capturing the default output instead"),
            }
            let host = cpal::default_host();
            let built = (|| -> Result<(cpal::Stream, String), String> {
                let dev = host.default_output_device().ok_or("no audio output device to capture")?;
                let name = dev.name().unwrap_or_default();
                Ok((open_stream(&dev, &s)?, name))
            })();
            match built {
                Ok((mut stream, mut name)) => {
                    let _ = tx.send(Ok(()));
                    let (mut last_check, mut failed_for) = (Instant::now(), String::new());
                    let (mut last_report, mut last_got) = (Instant::now(), 0u64);
                    while !s.stop.load(Ordering::Relaxed) {
                        std::thread::sleep(Duration::from_millis(50));
                        if last_report.elapsed() >= Duration::from_secs(10) {
                            let g = s.got.load(Ordering::Relaxed);
                            debug!("capture: {} bytes from {name} in the last 10 s, loudest sample {}", g - last_got, s.peak.swap(0, Ordering::Relaxed));
                            last_got = g;
                            last_report = Instant::now();
                        }
                        if last_check.elapsed() < FOLLOW_EVERY {
                            continue;
                        }
                        last_check = Instant::now();
                        // Follow the default output: capture the one that is playing now.
                        let Some(dev) = host.default_output_device() else { continue };
                        let now_name = dev.name().unwrap_or_default();
                        if now_name == name {
                            continue;
                        }
                        match open_stream(&dev, &s) {
                            Ok(new) => {
                                info!("capturing from the new default output: {now_name} (was {name})");
                                stream = new; // the old stream is dropped here
                                name = now_name;
                                failed_for.clear();
                            }
                            Err(e) => {
                                if failed_for != now_name {
                                    warn!("cannot capture the new default output {now_name} ({e}); staying on {name}");
                                    failed_for = now_name;
                                }
                            }
                        }
                    }
                    drop(stream);
                }
                Err(e) => {
                    let _ = tx.send(Err(e));
                }
            }
        });
        match rx.recv_timeout(Duration::from_secs(10)) {
            Ok(Ok(())) => Ok(Self { shared, thread: Some(thread), pacer: Pacer::new(Instant::now()) }),
            Ok(Err(e)) => Err(e),
            Err(_) => Err("the audio capture did not start in 10 s".into()),
        }
    }
}

impl Read for Loopback {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        let mut q = self.shared.q.lock().unwrap();
        loop {
            if !q.is_empty() {
                let n = buf.len().min(q.len());
                for (i, b) in q.drain(..n).enumerate() {
                    buf[i] = b;
                }
                self.pacer.sent(n, Instant::now());
                return Ok(n);
            }
            let now = Instant::now();
            let n = self.pacer.silence_due(buf.len(), now);
            if n > 0 {
                buf[..n].fill(0);
                self.pacer.sent(n, now);
                return Ok(n);
            }
            let (g, _) = self.shared.ready.wait_timeout(q, self.pacer.wait(now)).unwrap();
            q = g;
        }
    }
}

impl Drop for Loopback {
    fn drop(&mut self) {
        self.shared.stop.store(true, Ordering::Relaxed);
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn idle_output_gets_silence_at_real_time_pace() {
        let t0 = Instant::now();
        let mut p = Pacer::new(t0);
        let mut got = 0usize;
        // 2 s of an idle output, polled every millisecond, one 5 ms frame at a time
        for ms in 0..2000u64 {
            let now = t0 + Duration::from_millis(ms);
            loop {
                let n = p.silence_due(960, now);
                if n == 0 {
                    break;
                }
                p.sent(n, now);
                got += n;
            }
        }
        let secs = got as f64 / 192_000.0;
        assert!((1.95..=2.0).contains(&secs), "2 s of idle gave {secs} s of silence");
    }

    #[test]
    fn flowing_audio_gets_no_silence() {
        let t0 = Instant::now();
        let mut p = Pacer::new(t0);
        // real audio arrives every 10 ms (1,920 bytes) with 3 ms of jitter; none of it needs padding
        for k in 1..300u64 {
            let now = t0 + Duration::from_millis(k * 10 + (k % 4));
            assert_eq!(p.silence_due(960, now), 0, "silence inserted into flowing audio at block {k}");
            p.sent(1920, now);
        }
    }

    #[test]
    fn a_long_stall_is_not_made_up_with_a_burst() {
        let t0 = Instant::now();
        let mut p = Pacer::new(t0);
        let now = t0 + Duration::from_secs(5);
        let mut total = 0;
        loop {
            let n = p.silence_due(960, now);
            if n == 0 {
                break;
            }
            p.sent(n, now);
            total += n;
        }
        assert!(total <= 192_000 / 5, "{total} bytes of silence in one burst");
    }
}
