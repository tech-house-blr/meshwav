//! Adaptive jitter buffer. Pure logic: no threads, no sockets, the clock is passed in, so it is
//! unit-tested with simulated loss, reordering and delay. This is a port of the Android app's
//! `JitterBuffer.kt` and must behave the same; the tests mirror `JitterBufferTest.kt`.
//!
//! Target depth follows an RFC 3550 style interarrival jitter estimate: it grows at once when jitter
//! or late packets appear and shrinks by one frame per second. Total depth is capped so latency stays
//! bounded (stale audio is worthless). The caller serialises access.
use std::collections::BTreeMap;

pub const RESYNC_GAP: i64 = 1000; // a sequence jump this large means the sender restarted
pub const REBUFFER_MS: u64 = 150; // empty this long => underrun, rebuffer
pub const SHRINK_EVERY_MS: u64 = 1000; // the target shrinks at most one frame per second

#[derive(Debug, PartialEq, Eq)]
pub enum Out {
    Frame(Vec<u8>),
    Lost,
    Wait,
}

#[derive(Default, Clone, Copy, Debug)]
pub struct Counters {
    pub received: u64,
    pub lost: u64,
    pub late: u64,
    pub duplicates: u64,
    pub reordered: u64,
    pub trimmed: u64,
    pub underruns: u64,
    pub resyncs: u64,
}

impl Counters {
    pub fn add(&mut self, o: &Counters) {
        self.received += o.received;
        self.lost += o.lost;
        self.late += o.late;
        self.duplicates += o.duplicates;
        self.reordered += o.reordered;
        self.trimmed += o.trimmed;
        self.underruns += o.underruns;
        self.resyncs += o.resyncs;
    }
}

pub struct JitterBuffer {
    pub frame_ms: u32,
    pub min_frames: usize,
    pub max_frames: usize, // hard cap, 200 ms
    pub loss_gap: i64,     // this many later frames present => the missing one is declared lost
    buf: BTreeMap<i64, Vec<u8>>,
    next: i64,
    max_seq: i64,
    empty_since: u64,
    hole_since: u64,
    last_shrink: u64,
    prev_transit: Option<f64>,
    pub jitter_ms: f64,
    pub target_frames: usize,
    pub counters: Counters,
}

impl Default for JitterBuffer {
    fn default() -> Self {
        Self::new(10, 2, 20, 2)
    }
}

impl JitterBuffer {
    pub fn new(frame_ms: u32, min_frames: usize, max_frames: usize, loss_gap: i64) -> Self {
        Self {
            frame_ms,
            min_frames,
            max_frames,
            loss_gap,
            buf: BTreeMap::new(),
            next: -1,
            max_seq: -1,
            empty_since: 0,
            hole_since: 0,
            last_shrink: 0,
            prev_transit: None,
            jitter_ms: 0.0,
            target_frames: min_frames,
            counters: Counters::default(),
        }
    }

    /// Same 20 ms floor and 200 ms hard cap for every frame size.
    pub fn for_frame_ms(ms: u32) -> Self {
        Self::new(ms, 2.max(((20 + ms - 1) / ms) as usize), (200 / ms) as usize, 2)
    }

    pub fn depth_frames(&self) -> usize {
        self.buf.len()
    }

    pub fn push(&mut self, seq: i64, payload: Vec<u8>, now_ms: u64) {
        let r = if self.next >= 0 {
            self.next
        } else if self.buf.is_empty() {
            self.max_seq
        } else {
            *self.buf.keys().next().unwrap()
        };
        if self.max_seq >= 0 && (seq + RESYNC_GAP < r || seq > self.max_seq + RESYNC_GAP) {
            self.resync();
        }
        if self.next >= 0 && seq < self.next {
            self.counters.late += 1;
            self.target_frames = self.max_frames.min(self.target_frames + 1);
            self.last_shrink = now_ms;
            return;
        }
        if self.buf.contains_key(&seq) {
            self.counters.duplicates += 1;
            return;
        }
        if self.max_seq >= 0 && seq < self.max_seq {
            self.counters.reordered += 1;
        }
        self.counters.received += 1;

        let transit = now_ms as f64 - seq as f64 * self.frame_ms as f64;
        if let Some(p) = self.prev_transit {
            self.jitter_ms += ((transit - p).abs() - self.jitter_ms) / 16.0;
        }
        self.prev_transit = Some(transit);

        self.buf.insert(seq, payload);
        if seq > self.max_seq {
            self.max_seq = seq;
        }

        let want = ((4.0 * self.jitter_ms / self.frame_ms as f64).ceil() as usize + self.min_frames)
            .clamp(self.min_frames, self.max_frames);
        if want > self.target_frames {
            self.target_frames = want;
            self.last_shrink = now_ms;
        } else if want < self.target_frames && now_ms.saturating_sub(self.last_shrink) >= SHRINK_EVERY_MS {
            self.target_frames -= 1;
            self.last_shrink = now_ms;
        }
    }

    /// Called once per playout slot (one frame of audio needed).
    pub fn pop(&mut self, now_ms: u64) -> Out {
        if self.next < 0 {
            if self.buf.len() >= self.target_frames.max(1) {
                self.next = *self.buf.keys().next().unwrap();
                self.empty_since = 0;
                self.hole_since = 0;
            } else {
                return Out::Wait;
            }
        }
        let cap = self.max_frames.min(self.target_frames * 2 + 2) as i64;
        if self.max_seq - self.next + 1 > cap {
            // too far behind: skip ahead, bounded latency
            let n2 = self.max_seq - self.target_frames as i64 + 1;
            self.counters.trimmed += (n2 - self.next) as u64;
            self.buf = self.buf.split_off(&n2);
            self.next = n2;
        }
        if let Some(f) = self.buf.remove(&self.next) {
            self.next += 1;
            self.empty_since = 0;
            self.hole_since = 0;
            return Out::Frame(f);
        }
        if self.max_seq > self.next {
            // hole with later audio queued
            if self.hole_since == 0 {
                self.hole_since = now_ms;
            }
            if self.max_seq - self.next >= self.loss_gap || now_ms - self.hole_since >= 2 * self.frame_ms as u64 {
                self.next += 1;
                self.counters.lost += 1;
                self.hole_since = 0;
                return Out::Lost;
            }
            return Out::Wait;
        }
        // nothing buffered at all
        if self.empty_since == 0 {
            self.empty_since = now_ms;
        }
        if now_ms - self.empty_since > REBUFFER_MS {
            self.counters.underruns += 1;
            self.target_frames = self.max_frames.min(self.target_frames + 1);
            self.last_shrink = now_ms;
            // Start from scratch: whatever arrives next defines the new base (the sender may have restarted).
            self.next = -1;
            self.max_seq = -1;
            self.empty_since = 0;
            self.prev_transit = None;
        }
        Out::Wait
    }

    fn resync(&mut self) {
        self.buf.clear();
        self.next = -1;
        self.max_seq = -1;
        self.empty_since = 0;
        self.hole_since = 0;
        self.prev_transit = None;
        self.counters.resyncs += 1;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Arrival {
        t: u64,
        seq: i64,
    }

    struct Run {
        frames: usize,
        lost: usize,
        waits: usize,
        max_depth: usize,
        max_cap: usize,
    }

    /// Feed the arrivals in time order and pop once per 10 ms, like the playback thread does.
    fn run(jb: &mut JitterBuffer, mut arrivals: Vec<Arrival>, total_ms: u64) -> Run {
        arrivals.sort_by_key(|a| a.t);
        let (mut ai, mut frames, mut lost, mut waits, mut max_depth) = (0, 0, 0, 0, 0);
        let mut t = 0;
        while t <= total_ms {
            while ai < arrivals.len() && arrivals[ai].t <= t {
                jb.push(arrivals[ai].seq, vec![1], arrivals[ai].t);
                ai += 1;
            }
            match jb.pop(t) {
                Out::Frame(_) => frames += 1,
                Out::Lost => lost += 1,
                Out::Wait => waits += 1,
            }
            max_depth = max_depth.max(jb.depth_frames());
            t += 10;
        }
        Run { frames, lost, waits, max_depth, max_cap: jb.max_frames }
    }

    fn sender(n: i64, start_seq: i64, start_t: u64, mut jitter: impl FnMut(i64) -> u64, drop: impl Fn(i64) -> bool) -> Vec<Arrival> {
        (0..n).filter(|&i| !drop(i)).map(|i| Arrival { t: start_t + i as u64 * 10 + jitter(i), seq: start_seq + i }).collect()
    }

    fn plain(n: i64) -> Vec<Arrival> {
        sender(n, 0, 0, |_| 0, |_| false)
    }

    /// Small deterministic generator, so the tests need no extra crate.
    struct Rng(u64);
    impl Rng {
        fn next(&mut self) -> f64 {
            self.0 ^= self.0 << 13;
            self.0 ^= self.0 >> 7;
            self.0 ^= self.0 << 17;
            (self.0 >> 11) as f64 / (1u64 << 53) as f64
        }
    }

    #[test]
    fn clean_link_plays_everything_after_prebuffer() {
        let mut jb = JitterBuffer::default();
        let r = run(&mut jb, plain(500), 5030);
        assert_eq!(jb.counters.lost, 0);
        assert_eq!(r.frames, 500);
        assert!(r.waits <= 8, "waits {}", r.waits);
        assert_eq!(jb.counters.underruns, 0);
    }

    #[test]
    fn ten_percent_random_loss_is_concealed_not_stalled() {
        let mut rng = Rng(42);
        let dropped: Vec<bool> = (0..1000).map(|i| (11..=994).contains(&i) && rng.next() < 0.10).collect();
        let mut jb = JitterBuffer::default();
        let r = run(&mut jb, sender(1000, 0, 0, |_| 0, |i| dropped[i as usize]), 10_100);
        let n = dropped.iter().filter(|d| **d).count();
        assert_eq!(r.lost, n, "every dropped frame concealed exactly once");
        assert_eq!(r.frames, 1000 - n);
        assert_eq!(jb.counters.underruns, 0);
    }

    #[test]
    fn burst_loss_of_fifty_ms_is_concealed_and_recovers() {
        let mut jb = JitterBuffer::default();
        let r = run(&mut jb, sender(600, 0, 0, |_| 0, |i| (200..=204).contains(&i)), 6300);
        assert_eq!(r.lost, 5);
        assert_eq!(r.frames, 595);
    }

    #[test]
    fn adjacent_reordering_loses_nothing() {
        let mut jb = JitterBuffer::default();
        // every 7th packet is delayed by 12 ms so it arrives after its successor
        let r = run(&mut jb, sender(700, 0, 0, |i| if i % 7 == 3 { 12 } else { 0 }, |_| false), 7300);
        assert_eq!(r.lost, 0);
        assert_eq!(r.frames, 700);
        assert!(jb.counters.reordered > 50);
    }

    #[test]
    fn duplicates_are_ignored() {
        let mut jb = JitterBuffer::default();
        let a = plain(300);
        let mut all: Vec<Arrival> = a.iter().map(|x| Arrival { t: x.t, seq: x.seq }).collect();
        all.extend(a.iter().filter(|x| x.seq % 3 == 0).map(|x| Arrival { t: x.t + 1, seq: x.seq }));
        let r = run(&mut jb, all, 3300);
        assert_eq!(r.frames, 300);
        assert_eq!(jb.counters.duplicates, 100);
    }

    #[test]
    fn delay_spikes_grow_target_and_keep_depth_bounded() {
        let mut rng = Rng(7);
        let mut jb = JitterBuffer::default();
        let r = run(&mut jb, sender(1500, 0, 0, |_| if rng.next() < 0.1 { 60 } else { 0 }, |_| false), 15_500);
        assert!(jb.target_frames > jb.min_frames, "target should have grown, was {}", jb.target_frames);
        assert!(r.max_depth <= r.max_cap, "depth {} must stay under the hard cap", r.max_depth);
    }

    #[test]
    fn target_shrinks_back_after_link_calms() {
        let mut rng = Rng(9);
        let mut jb = JitterBuffer::default();
        run(&mut jb, sender(400, 0, 0, |_| if rng.next() < 0.2 { 80 } else { 0 }, |_| false), 4300);
        let grown = jb.target_frames;
        assert!(grown > jb.min_frames);
        run(&mut jb, sender(3000, 400, 4300, |_| 0, |_| false), 4300 + 31_000); // 30 s of clean link
        assert!(jb.target_frames < grown, "target {} -> {}", grown, jb.target_frames);
    }

    #[test]
    fn one_second_outage_rebuffers_and_resumes_with_bounded_depth() {
        let mut jb = JitterBuffer::default();
        let r = run(&mut jb, sender(300, 0, 0, |_| 0, |i| (100..=199).contains(&i)), 3300);
        assert_eq!(r.frames, 200, "every frame that was sent is eventually played");
        assert!(jb.counters.underruns >= 1);
        assert!(r.max_depth <= r.max_cap);
    }

    #[test]
    fn quick_sender_restart_recovers() {
        let mut jb = JitterBuffer::default();
        let mut all = plain(500);
        all.extend(sender(500, 0, 5030, |_| 0, |_| false)); // agent restarted fast: seq back to 0
        let r = run(&mut jb, all, 10_100);
        assert!(r.frames >= 960, "frames {}", r.frames);
    }

    #[test]
    fn slow_sender_restart_recovers_after_rebuffer() {
        let mut jb = JitterBuffer::default();
        let mut all = plain(500);
        all.extend(sender(500, 0, 5400, |_| 0, |_| false)); // agent restarted after a 400 ms gap
        let r = run(&mut jb, all, 10_500);
        assert!(r.frames >= 995, "frames {}", r.frames);
        assert!(jb.counters.underruns >= 1);
    }

    #[test]
    fn late_packets_are_counted_and_dropped() {
        let mut jb = JitterBuffer::default();
        run(&mut jb, sender(300, 0, 0, |i| if i == 100 { 400 } else { 0 }, |_| false), 3300);
        assert!(jb.counters.late >= 1);
    }

    #[test]
    fn frame_size_sets_floor_and_cap() {
        for (ms, min, max) in [(5, 4, 40), (10, 2, 20), (20, 2, 10)] {
            let jb = JitterBuffer::for_frame_ms(ms);
            assert_eq!((jb.min_frames, jb.max_frames), (min, max), "{ms} ms");
        }
    }
}
