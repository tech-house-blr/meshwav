//! Receiver-side clock mapping for end-to-end latency: pure arithmetic, no I/O, no clock reads.
//!
//! The receiver pings the agent (`Message::Ping`), the agent answers with its own clock (`Pong`), and the
//! agent also says which of its clock readings belongs to which audio timestamp (`TimeRef`). Together they
//! tell the receiver, in its own clock, when each frame was captured, so `now - captured` is the delay.
//!
//! All times are microseconds. The receiver passes its own monotonic clock in; the agent's clock is
//! whatever the agent reports (only differences matter). The Kotlin port in the Android app
//! (`ClockSync.kt`) follows this file and is checked against the same vectors.

use std::collections::VecDeque;

/// Audio sample clock rate behind `Header::timestamp`.
pub const RATE: i64 = 48_000;
/// How many recent ping answers are kept; the one with the shortest round trip is trusted.
const KEEP: usize = 8;

#[derive(Default, Debug)]
pub struct ClockSync {
    /// (local - agent offset in us, round trip in us)
    samples: VecDeque<(i64, u64)>,
    timeref: Option<(u32, u64)>,
}

impl ClockSync {
    pub fn new() -> Self { Self::default() }

    /// A `Pong` arrived: `echo_us` is our own send time, `agent_us` the agent's clock when it answered.
    /// Assumes the answer was made half way through the round trip (error at most half the round trip).
    pub fn on_pong(&mut self, echo_us: u64, agent_us: u64, now_us: u64) {
        let Some(rtt) = now_us.checked_sub(echo_us) else { return };
        // Peer-supplied values can be anything; saturate rather than overflow.
        let mid_local = (echo_us as i64).saturating_add((rtt / 2).min(i64::MAX as u64) as i64);
        self.samples.push_back((mid_local.saturating_sub(agent_us.min(i64::MAX as u64) as i64), rtt));
        if self.samples.len() > KEEP { self.samples.pop_front(); }
    }

    /// A `TimeRef` arrived: the frame stamped `ts` was captured at agent time `agent_us`.
    pub fn on_timeref(&mut self, ts: u32, agent_us: u64) { self.timeref = Some((ts, agent_us)); }

    fn best(&self) -> Option<(i64, u64)> { self.samples.iter().copied().min_by_key(|s| s.1) }

    /// True once both a clock offset and a time reference are known.
    pub fn ready(&self) -> bool { self.best().is_some() && self.timeref.is_some() }

    /// The worst-case error of the clock offset, in us (half the best round trip).
    pub fn error_us(&self) -> Option<u64> { self.best().map(|b| b.1 / 2) }

    /// When the frame stamped `ts` was captured, on the receiver's clock.
    pub fn captured_at_local_us(&self, ts: u32) -> Option<i64> {
        let (offset, _) = self.best()?;
        let (ref_ts, ref_agent) = self.timeref?;
        let dts = i64::from(ts.wrapping_sub(ref_ts) as i32); // signed, survives the 32-bit wrap
        Some((ref_agent.min(i64::MAX as u64) as i64).saturating_add(dts * 1_000_000 / RATE).saturating_add(offset))
    }

    /// Forget everything (new connection: the agent may have restarted, so its clock too).
    pub fn reset(&mut self) { *self = Self::default(); }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A fake agent whose clock reads `local - OFFSET`, with a given one-way delay each way.
    const OFFSET: i64 = 5_000_000;

    fn pong(c: &mut ClockSync, send_local: u64, there: u64, back: u64) {
        let agent_at = (send_local + there) as i64 - OFFSET;
        c.on_pong(send_local, agent_at as u64, send_local + there + back);
    }

    #[test]
    fn not_ready_until_both_pieces_arrive() {
        let mut c = ClockSync::new();
        assert!(!c.ready() && c.captured_at_local_us(0).is_none());
        pong(&mut c, 10_000_000, 2500, 2500);
        assert!(!c.ready());
        c.on_timeref(48_000, 1_000_000);
        assert!(c.ready());
    }

    #[test]
    fn symmetric_delay_gives_the_exact_capture_time() {
        let mut c = ClockSync::new();
        pong(&mut c, 10_000_000, 2500, 2500);
        // the frame stamped 48_000 was captured at agent time 20_000_000, i.e. local 25_000_000
        c.on_timeref(48_000, 20_000_000);
        assert_eq!(c.captured_at_local_us(48_000), Some(25_000_000));
        // 480 samples (10 ms) later / earlier
        assert_eq!(c.captured_at_local_us(48_480), Some(25_010_000));
        assert_eq!(c.captured_at_local_us(47_520), Some(24_990_000));
        assert_eq!(c.error_us(), Some(2500));
    }

    #[test]
    fn asymmetric_delay_errs_by_less_than_half_the_round_trip() {
        let mut c = ClockSync::new();
        pong(&mut c, 10_000_000, 1000, 9000); // rtt 10 ms
        c.on_timeref(0, 20_000_000);
        let got = c.captured_at_local_us(0).unwrap();
        let truth = 25_000_000;
        assert!((got - truth).unsigned_abs() <= c.error_us().unwrap(), "{got} vs {truth}");
    }

    #[test]
    fn the_shortest_round_trip_wins_over_a_congested_one() {
        let mut c = ClockSync::new();
        pong(&mut c, 10_000_000, 40_000, 2000); // congested, very asymmetric
        pong(&mut c, 11_000_000, 2500, 2500);
        pong(&mut c, 12_000_000, 30_000, 1000);
        c.on_timeref(0, 20_000_000);
        assert_eq!(c.captured_at_local_us(0), Some(25_000_000));
        assert_eq!(c.error_us(), Some(2500));
    }

    #[test]
    fn the_sample_clock_wrapping_does_not_matter() {
        let mut c = ClockSync::new();
        pong(&mut c, 10_000_000, 2500, 2500);
        c.on_timeref(u32::MAX - 239, 20_000_000); // 240 samples before the wrap
        assert_eq!(c.captured_at_local_us(240), Some(25_000_000 + 10_000)); // 480 samples later = 10 ms
        assert_eq!(c.captured_at_local_us(u32::MAX - 719), Some(25_000_000 - 10_000));
    }

    #[test]
    fn old_samples_age_out_and_reset_forgets() {
        let mut c = ClockSync::new();
        pong(&mut c, 1_000_000, 100, 100); // best rtt 200 us, then 8 worse ones push it out
        for i in 0..8 { pong(&mut c, 2_000_000 + i * 1_000_000, 5000, 5000); }
        assert_eq!(c.error_us(), Some(5000));
        c.reset();
        assert!(c.error_us().is_none());
    }

    #[test]
    fn a_pong_from_the_future_is_ignored() {
        let mut c = ClockSync::new();
        c.on_pong(10_000_000, 1, 9_000_000);
        assert!(c.error_us().is_none());
    }
}
