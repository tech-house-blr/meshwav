//! Audio output through AAudio, the platform's native low-latency path.
//!
//! One shared output stream, 48 kHz stereo 16-bit. The buffer starts at a few bursts and grows by one burst
//! whenever the stream underruns, so a device that needs more headroom gets it instead of crackling. After a
//! quiet minute it gives one burst back, so a single hiccup does not cost latency for the rest of the session.
use ndk::audio::{
    AudioContentType, AudioDirection, AudioFormat, AudioPerformanceMode, AudioSharingMode, AudioStream, AudioStreamBuilder,
    AudioUsage, Clockid,
};
use std::sync::atomic::{AtomicI32, AtomicI64, Ordering};
use std::time::{Duration, Instant};

const RATE: i64 = 48_000;
const START_BURSTS: i32 = 4;
const WRITE_TIMEOUT: Duration = Duration::from_millis(200);
const LATE_WRITE_NS: i64 = 20_000_000;
const SHRINK_AFTER_NS: i64 = 60_000_000_000;

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
    start: i32,
    baseline: AtomicI32,
    last_xruns: AtomicI32,
    first: AtomicI32,
    // The raw underrun counter counts every burst period without data, so one stall of a second adds hundreds.
    // `stalls` counts the events instead; `max_gap_ns` is the longest time between two writes, which shows
    // whether the app stopped feeding the stream or Android stopped reading it.
    stalls: AtomicI32,
    max_gap_ns: AtomicI64,
    late_writes: AtomicI32,
    late_stalls: AtomicI32,
    last_write_ns: AtomicI64,
    last_change_ns: AtomicI64,
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
        let start = (burst * START_BURSTS).min(stream.buffer_capacity_in_frames());
        Ok(Out {
            stream,
            burst,
            start,
            baseline: AtomicI32::new(0),
            last_xruns: AtomicI32::new(0),
            first: AtomicI32::new(1),
            stalls: AtomicI32::new(0),
            max_gap_ns: AtomicI64::new(0),
            late_writes: AtomicI32::new(0),
            late_stalls: AtomicI32::new(0),
            last_write_ns: AtomicI64::new(0),
            last_change_ns: AtomicI64::new(now_ns()),
        })
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
        let t0 = now_ns();
        let prev = self.last_write_ns.swap(t0, Ordering::Relaxed);
        let gap = if prev > 0 { t0 - prev } else { 0 };
        if gap > 0 {
            self.max_gap_ns.fetch_max(gap, Ordering::Relaxed);
        }
        let late = gap > LATE_WRITE_NS;
        if late {
            self.late_writes.fetch_add(1, Ordering::Relaxed);
        }
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
        self.after_write(late);
        done as i32
    }

    fn after_write(&self, late: bool) {
        let x = self.stream.x_run_count();
        if self.first.swap(0, Ordering::Relaxed) == 1 {
            // Underruns before the first data arrived are not the stream's fault.
            self.baseline.store(x, Ordering::Relaxed);
            self.last_xruns.store(x, Ordering::Relaxed);
            return;
        }
        let now = now_ns();
        let cur = self.stream.buffer_size_in_frames();
        let cap = self.stream.buffer_capacity_in_frames();
        if x > self.last_xruns.swap(x, Ordering::Relaxed) {
            self.stalls.fetch_add(1, Ordering::Relaxed);
            if late {
                self.late_stalls.fetch_add(1, Ordering::Relaxed);
            }
            self.last_change_ns.store(now, Ordering::Relaxed);
            let _ = self.stream.set_buffer_size_in_frames((cur + self.burst).min(cap));
        } else if cur > self.start && now - self.last_change_ns.load(Ordering::Relaxed) > SHRINK_AFTER_NS {
            self.last_change_ns.store(now, Ordering::Relaxed);
            let _ = self.stream.set_buffer_size_in_frames((cur - self.burst).max(self.start));
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

    /// Underrun events since the stream started, a stall counting once however long it lasted.
    pub fn stalls(&self) -> i32 {
        self.stalls.load(Ordering::Relaxed)
    }

    /// Longest time between two writes since the stream started, in ms.
    pub fn max_gap_ms(&self) -> i32 {
        (self.max_gap_ns.load(Ordering::Relaxed) / 1_000_000) as i32
    }

    /// Writes that came more than 20 ms after the previous one, and stalls that happened right after such a write.
    pub fn late_writes(&self) -> i32 {
        self.late_writes.load(Ordering::Relaxed)
    }

    pub fn late_stalls(&self) -> i32 {
        self.late_stalls.load(Ordering::Relaxed)
    }

    pub fn buffer_ms(&self) -> i32 {
        (self.stream.buffer_size_in_frames() as i64 * 1000 / RATE) as i32
    }
}
