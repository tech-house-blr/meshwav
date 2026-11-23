//! Packet loss concealment for interleaved stereo i16. A lost frame repeats the last good one with a
//! fade that reaches silence after `MAX_MISSES` consecutive losses; the first real frame after a
//! loss fades in over 2 ms to avoid a click. Port of the Android app's `Concealer.kt`.

pub const MAX_MISSES: u32 = 5;
pub const FADE_IN_SAMPLES: usize = 192; // 2 ms stereo at 48 kHz
const DECAY: f32 = 0.6;

#[derive(Default)]
pub struct Concealer {
    last: Vec<i16>,
    misses: u32,
}

impl Concealer {
    /// Record a decoded frame; returns it (with a short fade-in if it follows a loss).
    pub fn accept(&mut self, mut pcm: Vec<i16>) -> Vec<i16> {
        if pcm.is_empty() {
            return pcm;
        }
        self.last = pcm.clone();
        if self.misses > 0 {
            let n = FADE_IN_SAMPLES.min(pcm.len());
            for (i, s) in pcm.iter_mut().take(n).enumerate() {
                *s = (*s as f32 * (i as f32 / n as f32)) as i16;
            }
        }
        self.misses = 0;
        pcm
    }

    /// Forget the last frame (the frame size changed, so it no longer fits).
    pub fn reset(&mut self) {
        self.last.clear();
        self.misses = 0;
    }

    /// Synthesize a frame for a lost packet.
    pub fn conceal(&mut self, fallback_samples: usize) -> Vec<i16> {
        self.misses += 1;
        if self.last.is_empty() {
            return vec![0; fallback_samples];
        }
        let mut out = vec![0i16; self.last.len()];
        if self.misses <= MAX_MISSES {
            let g0 = DECAY.powi(self.misses as i32 - 1);
            let g1 = if self.misses == MAX_MISSES { 0.0 } else { g0 * DECAY };
            let len = out.len() as f32;
            for (i, o) in out.iter_mut().enumerate() {
                let g = g0 + (g1 - g0) * (i as f32 / len);
                *o = (self.last[i] as f32 * g) as i16;
            }
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fades_to_silence_and_keeps_length() {
        let mut c = Concealer::default();
        c.accept(vec![10000; 960]);
        let mut prev_peak = 10000i32;
        let mut last = vec![];
        for _ in 1..=6 {
            last = c.conceal(960);
            assert_eq!(last.len(), 960);
            let peak = last.iter().map(|s| (*s as i32).abs()).max().unwrap();
            assert!(peak <= prev_peak, "peak {peak} must not rise above {prev_peak}");
            prev_peak = peak;
        }
        assert!(last.iter().all(|s| *s == 0));
    }

    #[test]
    fn fades_in_after_loss() {
        let mut c = Concealer::default();
        c.accept(vec![10000; 960]);
        c.conceal(960);
        let out = c.accept(vec![10000; 960]);
        assert_eq!(out[0], 0);
        assert_eq!(out[FADE_IN_SAMPLES + 10], 10000);
    }

    #[test]
    fn nothing_to_repeat_gives_silence_of_the_asked_length() {
        let mut c = Concealer::default();
        assert_eq!(c.conceal(480), vec![0i16; 480]);
    }
}
