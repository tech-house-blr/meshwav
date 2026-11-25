//! Windows playback through WASAPI (via the `cpal` crate).
//!
//! `cpal` pulls audio with a callback, while the receiver pushes with a blocking `write`, so a small
//! ring sits between them. `write` waits while the ring holds more than the requested latency, which
//! paces the receiver the same way `pacat` does on Linux. The stream lives on its own thread because
//! `cpal` streams cannot be moved between threads.
use crate::sink::{Output, Sink};
use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use cpal::{SampleFormat, SampleRate, StreamConfig};
use std::collections::VecDeque;
use std::io;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{mpsc, Arc, Condvar, Mutex};
use std::time::Duration;

const RATE: u32 = 48_000;
const CHANNELS: u16 = 2;

struct Ring {
    q: Mutex<VecDeque<f32>>,
    /// Woken whenever the callback has taken samples out.
    drained: Condvar,
    /// Callback invocations that found the ring empty (an audible gap).
    underruns: AtomicU64,
    stop: AtomicBool,
}

pub struct Wasapi {
    ring: Arc<Ring>,
    /// Samples (not frames) the ring may hold before `write` waits.
    limit: usize,
    thread: Option<std::thread::JoinHandle<()>>,
}

fn io_err(msg: impl Into<String>) -> io::Error {
    io::Error::new(io::ErrorKind::Other, msg.into())
}

fn find_device(name: Option<&str>) -> Result<cpal::Device, String> {
    let host = cpal::default_host();
    match name.filter(|n| !n.is_empty()) {
        None => host.default_output_device().ok_or_else(|| "no audio output device found".to_string()),
        Some(n) => host
            .output_devices()
            .map_err(|e| e.to_string())?
            .find(|d| d.name().map(|dn| dn == n).unwrap_or(false))
            .ok_or_else(|| format!("no output named `{n}` (see `outputs`)")),
    }
}

pub fn list_outputs() -> Vec<Output> {
    let host = cpal::default_host();
    let default = host.default_output_device().and_then(|d| d.name().ok());
    host.output_devices()
        .map(|it| {
            it.filter_map(|d| d.name().ok())
                .map(|n| {
                    let description = if Some(&n) == default.as_ref() { "default".to_string() } else { String::new() };
                    Output { name: n, description }
                })
                .collect()
        })
        .unwrap_or_default()
}

impl Wasapi {
    pub fn start(latency_ms: u32, device: Option<&str>) -> io::Result<Self> {
        let ring = Arc::new(Ring { q: Mutex::new(VecDeque::new()), drained: Condvar::new(), underruns: AtomicU64::new(0), stop: AtomicBool::new(false) });
        let (tx, rx) = mpsc::channel::<Result<(), String>>();
        let device = device.map(String::from);
        let r = ring.clone();
        let thread = std::thread::spawn(move || {
            let built = (|| -> Result<cpal::Stream, String> {
                let dev = find_device(device.as_deref())?;
                let cfg = StreamConfig { channels: CHANNELS, sample_rate: SampleRate(RATE), buffer_size: cpal::BufferSize::Default };
                // Shared mode hands out f32; check the device accepts 48 kHz stereo before opening.
                let ok = dev
                    .supported_output_configs()
                    .map_err(|e| e.to_string())?
                    .any(|c| c.channels() == CHANNELS && c.min_sample_rate().0 <= RATE && c.max_sample_rate().0 >= RATE && c.sample_format() == SampleFormat::F32);
                if !ok {
                    return Err("this output does not offer 48 kHz stereo".to_string());
                }
                let rr = r.clone();
                let stream = dev
                    .build_output_stream(
                        &cfg,
                        move |data: &mut [f32], _| {
                            let mut q = rr.q.lock().unwrap();
                            let mut starved = false;
                            for s in data.iter_mut() {
                                *s = q.pop_front().unwrap_or_else(|| {
                                    starved = true;
                                    0.0
                                });
                            }
                            drop(q);
                            if starved {
                                rr.underruns.fetch_add(1, Ordering::Relaxed);
                            }
                            rr.drained.notify_all();
                        },
                        |e| eprintln!("audio output error: {e}"),
                        None,
                    )
                    .map_err(|e| e.to_string())?;
                stream.play().map_err(|e| e.to_string())?;
                Ok(stream)
            })();
            match built {
                Ok(stream) => {
                    let _ = tx.send(Ok(()));
                    while !r.stop.load(Ordering::Relaxed) {
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
            Ok(Ok(())) => {}
            Ok(Err(e)) => return Err(io_err(format!("cannot open the audio output: {e}"))),
            Err(_) => return Err(io_err("the audio output did not start in 10 s")),
        }
        let limit = (latency_ms.clamp(10, 500) as usize * RATE as usize / 1000) * CHANNELS as usize;
        Ok(Self { ring, limit, thread: Some(thread) })
    }

    /// Times the output ran dry since the start.
    pub fn underruns(&self) -> u64 {
        self.ring.underruns.load(Ordering::Relaxed)
    }
}

impl Sink for Wasapi {
    fn write(&mut self, pcm: &[i16]) -> io::Result<()> {
        let mut q = self.ring.q.lock().unwrap();
        // Pace: wait until the ring has room (or give up after a second so a stuck device cannot hang us).
        let mut waited = 0;
        while q.len() > self.limit && waited < 100 {
            let (g, _) = self.ring.drained.wait_timeout(q, Duration::from_millis(10)).unwrap();
            q = g;
            waited += 1;
        }
        q.extend(pcm.iter().map(|&s| s as f32 / 32768.0));
        Ok(())
    }
}

impl Drop for Wasapi {
    fn drop(&mut self) {
        self.ring.stop.store(true, Ordering::Relaxed);
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}
