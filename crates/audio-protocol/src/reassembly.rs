//! Bounded fragment reassembly. Frames are keyed by timestamp. At most `cap` frames are
//! in flight; the oldest incomplete frame is evicted first (latency over completeness).
use crate::packet::Header;
use std::collections::VecDeque;

struct Slot {
    timestamp: u32,
    flags: u16,
    parts: Vec<Option<Vec<u8>>>,
    got: usize,
}

pub struct Frame {
    pub timestamp: u32,
    pub flags: u16,
    pub data: Vec<u8>,
}

pub struct Reassembler {
    slots: VecDeque<Slot>,
    cap: usize,
    pub evicted: u64,
}

impl Reassembler {
    pub fn new(cap: usize) -> Self {
        Self { slots: VecDeque::new(), cap: cap.max(1), evicted: 0 }
    }

    pub fn in_flight(&self) -> usize {
        self.slots.len()
    }

    /// Feed one fragment. Returns the complete frame once all fragments arrived.
    pub fn push(&mut self, h: &Header, payload: Vec<u8>) -> Option<Frame> {
        if h.frag_cnt == 1 {
            return Some(Frame { timestamp: h.timestamp, flags: h.flags, data: payload });
        }
        let pos = match self.slots.iter().position(|s| s.timestamp == h.timestamp) {
            Some(p) => p,
            None => {
                if self.slots.len() == self.cap {
                    self.slots.pop_front();
                    self.evicted += 1;
                }
                self.slots.push_back(Slot {
                    timestamp: h.timestamp,
                    flags: 0,
                    parts: vec![None; h.frag_cnt as usize],
                    got: 0,
                });
                self.slots.len() - 1
            }
        };
        let slot = &mut self.slots[pos];
        if slot.parts.len() != h.frag_cnt as usize {
            return None; // inconsistent fragment count; ignore
        }
        let i = h.frag_idx as usize;
        if slot.parts[i].is_some() {
            return None; // duplicate
        }
        slot.parts[i] = Some(payload);
        slot.flags |= h.flags;
        slot.got += 1;
        if slot.got < slot.parts.len() {
            return None;
        }
        let slot = self.slots.remove(pos).unwrap();
        let data = slot.parts.into_iter().flatten().flatten().collect();
        Some(Frame { timestamp: slot.timestamp, flags: slot.flags, data })
    }
}
