//! System-audio capture on Windows: WASAPI loopback of the default output, through `cpal`.
//!
//! The sender reads fixed-size frames of 48 kHz stereo s16le, as `parec` gives them on Linux. Here the
//! device's own mix format (any rate, any channel count) is converted into that. Loopback delivers
//! nothing while the output is silent, so `read` fills the gap with silence at real-time pace, which
//! keeps the frame clock running the way a PulseAudio monitor does.
use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use cpal::{Sample, SampleFormat};
use std::collections::VecDeque;
use std::io::{self, Read};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{mpsc, Arc, Condvar, Mutex};
use std::time::Duration;

/// Never hold more than this much captured audio: if the sender stalls, old audio is dropped, not queued.
const MAX_BYTES: usize = 48_000 * 4 / 5; // 200 ms
const SILENCE_AFTER: Duration = Duration::from_millis(10);

struct Shared {
    q: Mutex<VecDeque<u8>>,
    ready: Condvar,
    stop: AtomicBool,
}

pub struct Loopback {
    shared: Arc<Shared>,
    thread: Option<std::thread::JoinHandle<()>>,
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
        let mut q = shared.q.lock().unwrap();
        q.extend(bytes.iter());
        let over = q.len().saturating_sub(MAX_BYTES);
        if over > 0 {
            q.drain(..over - over % 4);
        }
        drop(q);
        shared.ready.notify_all();
    }
}

impl Loopback {
    pub fn start() -> Result<Self, String> {
        let shared = Arc::new(Shared { q: Mutex::new(VecDeque::new()), ready: Condvar::new(), stop: AtomicBool::new(false) });
        let (tx, rx) = mpsc::channel::<Result<(), String>>();
        let s = shared.clone();
        let thread = std::thread::spawn(move || {
            let built = (|| -> Result<cpal::Stream, String> {
                let dev = cpal::default_host().default_output_device().ok_or("no audio output device to capture")?;
                let cfg = dev.default_output_config().map_err(|e| e.to_string())?;
                let ch = cfg.channels() as usize;
                let conv = Convert { ch, step: cfg.sample_rate().0 as f64 / 48_000.0, t: 0.0, prev: [0.0; 2] };
                let err = |e| eprintln!("audio capture error: {e}");
                // Building an input stream on an output device is how cpal asks WASAPI for loopback.
                let stream = match cfg.sample_format() {
                    SampleFormat::F32 => dev.build_input_stream(&cfg.config(), feed::<f32>(s.clone(), conv), err, None),
                    SampleFormat::I16 => dev.build_input_stream(&cfg.config(), feed::<i16>(s.clone(), conv), err, None),
                    SampleFormat::I32 => dev.build_input_stream(&cfg.config(), feed::<i32>(s.clone(), conv), err, None),
                    f => return Err(format!("unsupported sample format {f:?}")),
                }
                .map_err(|e| e.to_string())?;
                stream.play().map_err(|e| e.to_string())?;
                Ok(stream)
            })();
            match built {
                Ok(stream) => {
                    let _ = tx.send(Ok(()));
                    while !s.stop.load(Ordering::Relaxed) {
                        std::thread::sleep(Duration::from_millis(50));
                    }
                    drop(stream);
                }
                Err(e) => {
                    let _ = tx.send(Err(e));
                }
            }
        });
        match rx.recv_timeout(Duration::from_secs(10)) {
            Ok(Ok(())) => Ok(Self { shared, thread: Some(thread) }),
            Ok(Err(e)) => Err(e),
            Err(_) => Err("the audio capture did not start in 10 s".into()),
        }
    }
}

impl Read for Loopback {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        let mut q = self.shared.q.lock().unwrap();
        if q.is_empty() {
            let (g, timeout) = self.shared.ready.wait_timeout(q, SILENCE_AFTER).unwrap();
            q = g;
            if timeout.timed_out() && q.is_empty() {
                let n = buf.len().min(48 * 10 * 4); // 10 ms of silence
                buf[..n].fill(0);
                return Ok(n);
            }
        }
        let n = buf.len().min(q.len());
        for (i, b) in q.drain(..n).enumerate() {
            buf[i] = b;
        }
        Ok(n)
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
