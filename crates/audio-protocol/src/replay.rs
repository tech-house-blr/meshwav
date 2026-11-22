//! Sliding replay window (1024 packets) over u32 sequence numbers.

const WORDS: usize = 16;
pub const WINDOW: u32 = (WORDS as u32) * 64;

pub struct ReplayWindow {
    highest: u32,
    seen_any: bool,
    bits: [u64; WORDS],
}

impl Default for ReplayWindow {
    fn default() -> Self {
        Self::new()
    }
}

impl ReplayWindow {
    pub fn new() -> Self {
        Self { highest: 0, seen_any: false, bits: [0; WORDS] }
    }

    fn get(&self, off: u32) -> bool {
        self.bits[(off / 64) as usize] >> (off % 64) & 1 == 1
    }

    fn set(&mut self, off: u32) {
        self.bits[(off / 64) as usize] |= 1 << (off % 64);
    }

    /// Non-mutating check (used before authentication).
    pub fn would_accept(&self, seq: u32) -> bool {
        if !self.seen_any || seq > self.highest {
            return true;
        }
        let off = self.highest - seq;
        off < WINDOW && !self.get(off)
    }

    /// Record an authenticated sequence number. Returns false if it was a replay.
    pub fn accept(&mut self, seq: u32) -> bool {
        if !self.seen_any {
            self.seen_any = true;
            self.highest = seq;
            self.bits = [0; WORDS];
            self.set(0);
            return true;
        }
        if seq > self.highest {
            let shift = seq - self.highest;
            if shift >= WINDOW {
                self.bits = [0; WORDS];
            } else {
                for i in (0..WORDS).rev() {
                    let ws = (shift / 64) as usize;
                    let bs = shift % 64;
                    let mut v = if i >= ws { self.bits[i - ws] << bs } else { 0 };
                    if bs > 0 && i > ws {
                        v |= self.bits[i - ws - 1] >> (64 - bs);
                    }
                    self.bits[i] = v;
                }
            }
            self.highest = seq;
            self.set(0);
            return true;
        }
        let off = self.highest - seq;
        if off >= WINDOW || self.get(off) {
            return false;
        }
        self.set(off);
        true
    }
}
