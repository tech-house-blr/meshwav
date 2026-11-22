//! UDP audio packet header (20 bytes, big-endian).
use crate::ProtocolError;

pub const MAGIC: [u8; 2] = *b"MW";
pub const VERSION: u8 = 1;
pub const HEADER_LEN: usize = 20;
pub const TAG_LEN: usize = 16;
/// Max UDP payload: Tailscale MTU 1280 minus IPv6 (40) and UDP (8) headers, with margin.
pub const MAX_DATAGRAM: usize = 1200;
/// Max codec bytes in one packet.
pub const MAX_PAYLOAD: usize = MAX_DATAGRAM - HEADER_LEN - TAG_LEN;

pub const FLAG_KEYFRAME: u16 = 1 << 0;
pub const FLAG_SILENCE: u16 = 1 << 1;
/// Bits 2..=4 of `flags` carry the frame duration of an audio packet as `frame_ms / 5`
/// (1 = 5 ms, 2 = 10 ms, 4 = 20 ms). The receiver follows it, so a runtime frame-size switch
/// needs no extra synchronisation and survives a lost control message. 0 = not stated.
pub const FLAG_FRAME_SHIFT: u16 = 2;
pub const FLAG_FRAME_MASK: u16 = 0b111 << FLAG_FRAME_SHIFT;

/// Payload is raw 48 kHz stereo s16le PCM (lossless) instead of Opus. Always 5 ms (960 bytes).
pub const FLAG_PCM: u16 = 1 << 5;

/// Flag bits announcing `frame_ms` (5, 10 or 20); 0 for anything else.
pub fn frame_flags(frame_ms: u32) -> u16 {
    match frame_ms {
        5 | 10 | 20 => ((frame_ms / 5) as u16) << FLAG_FRAME_SHIFT,
        _ => 0,
    }
}

/// Frame duration announced by `flags` (5, 10 or 20), or 0 if absent or invalid.
pub fn frame_ms_from_flags(flags: u16) -> u32 {
    match (flags & FLAG_FRAME_MASK) >> FLAG_FRAME_SHIFT {
        1 => 5,
        2 => 10,
        4 => 20,
        _ => 0,
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u8)]
pub enum PacketType {
    Audio = 1,
    Punch = 2,
    Parity = 3,
    /// Sealed control message (see `control::Message`), on stream id 2 in both directions.
    Control = 4,
}

impl PacketType {
    fn from_u8(v: u8) -> Result<Self, ProtocolError> {
        match v {
            1 => Ok(Self::Audio),
            2 => Ok(Self::Punch),
            3 => Ok(Self::Parity),
            4 => Ok(Self::Control),
            x => Err(ProtocolError::BadType(x)),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Header {
    pub ptype: PacketType,
    pub stream_id: u32,
    pub seq: u32,
    /// Sample-clock timestamp of the first sample of the frame; identical across fragments.
    pub timestamp: u32,
    pub frag_idx: u8,
    pub frag_cnt: u8,
    pub flags: u16,
}

impl Header {
    pub fn encode(&self) -> [u8; HEADER_LEN] {
        let mut b = [0u8; HEADER_LEN];
        b[0..2].copy_from_slice(&MAGIC);
        b[2] = VERSION;
        b[3] = self.ptype as u8;
        b[4..8].copy_from_slice(&self.stream_id.to_be_bytes());
        b[8..12].copy_from_slice(&self.seq.to_be_bytes());
        b[12..16].copy_from_slice(&self.timestamp.to_be_bytes());
        b[16] = self.frag_idx;
        b[17] = self.frag_cnt;
        b[18..20].copy_from_slice(&self.flags.to_be_bytes());
        b
    }

    pub fn decode(b: &[u8]) -> Result<Self, ProtocolError> {
        if b.len() < HEADER_LEN {
            return Err(ProtocolError::TooShort);
        }
        if b[0..2] != MAGIC {
            return Err(ProtocolError::BadMagic);
        }
        if b[2] != VERSION {
            return Err(ProtocolError::BadVersion(b[2]));
        }
        let h = Header {
            ptype: PacketType::from_u8(b[3])?,
            stream_id: u32::from_be_bytes(b[4..8].try_into().unwrap()),
            seq: u32::from_be_bytes(b[8..12].try_into().unwrap()),
            timestamp: u32::from_be_bytes(b[12..16].try_into().unwrap()),
            frag_idx: b[16],
            frag_cnt: b[17],
            flags: u16::from_be_bytes(b[18..20].try_into().unwrap()),
        };
        if h.frag_cnt == 0 || h.frag_idx >= h.frag_cnt {
            return Err(ProtocolError::BadFragment);
        }
        Ok(h)
    }
}

/// Split a codec frame into at most 255 chunks of `<= MAX_PAYLOAD` bytes.
/// Returns `None` if the frame is empty or needs more than 255 fragments.
pub fn fragment(frame: &[u8]) -> Option<impl Iterator<Item = &[u8]>> {
    if frame.is_empty() || frame.len().div_ceil(MAX_PAYLOAD) > 255 {
        return None;
    }
    Some(frame.chunks(MAX_PAYLOAD))
}
