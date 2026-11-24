//! Audio output through AAudio, the platform's native low-latency path.
//!
//! One shared output stream, 48 kHz stereo 16-bit. The buffer starts at a few bursts and grows by one burst
//! whenever the stream underruns, so a device that needs more headroom gets it instead of crackling.
use ndk::audio::{
    AudioContentType, AudioDirection, AudioFormat, AudioPerformanceMode, AudioSharingMode, AudioStream, AudioStreamBuilder,
    AudioUsage, Clockid,
};
use std::sync::atomic::{AtomicI32, Ordering};
use std::time::{Duration, Instant};

const RATE: i64 = 48_000;
const START_BURSTS: i32 = 4;
const WRITE_TIMEOUT: Duration = Duration::from_millis(200);

extern "C" {
    fn clock_gettime(clk: i32, ts: *mut [i64; 2]) -> i32;
}

fn now_ns() -> i64 {
    let mut t = [0i64; 2];
    unsafe { clock_gettime(1, &mut t) }; // CLOCK_MONOTONIC
    t[0] * 1_000_000_000 + t[1]
}

pub struct Out {
    stream: AudioStream,
    burst: i32,
    baseline: AtomicI32,
    last_xruns: AtomicI32,
    first: AtomicI32,
}

// AAudio streams may be used from any thread; the calls here are the thread-safe ones.
unsafe impl Send for Out {}
unsafe impl Sync for Out {}

/// The ndk crate reports a positive frame count from `write` as an error value; treat it as the count.
fn frames_from_error(e: &impl std::fmt::Debug) -> Option<usize> {
    let t = format!("{e:?}");
    t.strip_prefix("__Unknown(")?.strip_suffix(')')?.parse::<i32>().ok().filter(|n| *n > 0).map(|n| n as usize)
}

impl Out {
    pub fn open() -> Result<Out, String> {
        let stream = AudioStreamBuilder::new()
            .and_then(|b| {
                b.direction(AudioDirection::Output)
                    .format(AudioFormat::PCM_I16)
                    .sample_rate(RATE as i32)
                    .channel_count(2)
                    .performance_mode(AudioPerformanceMode::LowLatency)
                    .sharing_mode(AudioSharingMode::Shared)
                    .usage(AudioUsage::Media)
                    .content_type(AudioContentType::Music)
                    .open_stream()
            })
            .map_err(|e| format!("{e:?}"))?;
        let burst = stream.frames_per_burst().max(1);
        let _ = stream.set_buffer_size_in_frames((burst * START_BURSTS).min(stream.buffer_capacity_in_frames()));
        stream.request_start().map_err(|e| format!("start: {e:?}"))?;
        Ok(Out { stream, burst, baseline: AtomicI32::new(0), last_xruns: AtomicI32::new(0), first: AtomicI32::new(1) })
    }

    pub fn describe(&self) -> String {
        format!(
            "{:?}:{:?}:{}",
            self.stream.sharing_mode(),
            self.stream.performance_mode(),
            self.stream.device_id()
        )
    }

    /// Blocks until all of `pcm` (interleaved stereo i16 bytes) is queued. Returns frames written, or -1.
    pub fn write(&self, pcm: &[u8]) -> i32 {
        let total = pcm.len() / 4;
        let mut done = 0usize;
        let deadline = Instant::now() + WRITE_TIMEOUT;
        while done < total {
            let p = pcm[done * 4..].as_ptr();
            match unsafe { self.stream.write(p as *const _, (total - done) as i32, 50_000_000) } {
                Ok(n) => done += n as usize,
                Err(e) => match frames_from_error(&e) {
                    Some(n) => done += n,
                    None => return -1,
                },
            }
            if Instant::now() > deadline {
                return -1;
            }
        }
        self.after_write();
        done as i32
    }

    fn after_write(&self) {
        let x = self.stream.x_run_count();
        if self.first.swap(0, Ordering::Relaxed) == 1 {
            // Underruns before the first data arrived are not the stream's fault.
            self.baseline.store(x, Ordering::Relaxed);
            self.last_xruns.store(x, Ordering::Relaxed);
            return;
        }
        if x > self.last_xruns.swap(x, Ordering::Relaxed) {
            let cur = self.stream.buffer_size_in_frames();
            let _ = self.stream.set_buffer_size_in_frames((cur + self.burst).min(self.stream.buffer_capacity_in_frames()));
        }
    }

    /// Milliseconds between a frame being queued now and it reaching the output, from the stream's own timestamp; -1 if unknown.
    pub fn queue_ms(&self) -> f64 {
        match self.stream.timestamp(Clockid::Monotonic) {
            Ok(ts) => {
                let ahead = self.stream.frames_written() - ts.frame_position;
                ahead as f64 * 1000.0 / RATE as f64 - (now_ns() - ts.time_nanoseconds) as f64 / 1e6
            }
            Err(_) => -1.0,
        }
    }

    pub fn underruns(&self) -> i32 {
        (self.stream.x_run_count() - self.baseline.load(Ordering::Relaxed)).max(0)
    }

    pub fn buffer_ms(&self) -> i32 {
        (self.stream.buffer_size_in_frames() as i64 * 1000 / RATE) as i32
    }
}
