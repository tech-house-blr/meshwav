//! Opus, 48 kHz stereo, configurable frame size and bitrate.
use crate::CodecError;
use audiopus::coder::{Decoder, Encoder};
use audiopus::{Application, Bitrate, Channels, MutSignals, SampleRate};

pub const RATE: u32 = 48_000;
pub const CHANNELS: usize = 2;
pub const MIN_KBPS: u32 = 64;
pub const MAX_KBPS: u32 = 256;
pub const DEFAULT_KBPS: u32 = 160;
/// Allowed frame durations in microseconds (Opus supports 2.5 ms to 60 ms; we cap at 20 ms).
pub const FRAME_US: [u32; 4] = [2_500, 5_000, 10_000, 20_000];

fn e(err: audiopus::Error) -> CodecError { CodecError::Opus(err.to_string()) }

pub fn frame_samples(frame_us: u32) -> Result<usize, CodecError> {
    if !FRAME_US.contains(&frame_us) { return Err(CodecError::Params("unsupported opus frame size")) }
    Ok((RATE as usize * frame_us as usize) / 1_000_000)
}

pub struct OpusEncoder {
    enc: Encoder,
    frame_samples: usize,
    buf: Vec<u8>,
}

impl OpusEncoder {
    /// `low_delay` selects the restricted low-delay application (less lookahead, lower fidelity).
    pub fn new(frame_us: u32, kbps: u32, low_delay: bool) -> Result<Self, CodecError> {
        if !(MIN_KBPS..=MAX_KBPS).contains(&kbps) { return Err(CodecError::Params("bitrate out of range")) }
        let frame_samples = frame_samples(frame_us)?;
        let app = if low_delay { Application::LowDelay } else { Application::Audio };
        let mut enc = Encoder::new(SampleRate::Hz48000, Channels::Stereo, app).map_err(e)?;
        enc.set_bitrate(Bitrate::BitsPerSecond((kbps * 1000) as i32)).map_err(e)?;
        enc.set_vbr(true).map_err(e)?;
        enc.set_vbr_constraint(true).map_err(e)?;
        Ok(Self { enc, frame_samples, buf: vec![0; 1275] })
    }

    pub fn set_bitrate_kbps(&mut self, kbps: u32) -> Result<(), CodecError> {
        if !(MIN_KBPS..=MAX_KBPS).contains(&kbps) { return Err(CodecError::Params("bitrate out of range")) }
        self.enc.set_bitrate(Bitrate::BitsPerSecond((kbps * 1000) as i32)).map_err(e)
    }

    pub fn frame_samples(&self) -> usize { self.frame_samples }

    /// Encode exactly one frame of interleaved stereo i16 (`frame_samples * 2` values).
    pub fn encode(&mut self, pcm: &[i16]) -> Result<&[u8], CodecError> {
        if pcm.len() != self.frame_samples * CHANNELS { return Err(CodecError::Params("wrong frame length")) }
        let n = self.enc.encode(pcm, &mut self.buf).map_err(e)?;
        Ok(&self.buf[..n])
    }
}

pub struct OpusDecoder {
    dec: Decoder,
    frame_samples: usize,
}

impl OpusDecoder {
    pub fn new(frame_us: u32) -> Result<Self, CodecError> {
        Ok(Self {
            dec: Decoder::new(SampleRate::Hz48000, Channels::Stereo).map_err(e)?,
            frame_samples: frame_samples(frame_us)?,
        })
    }

    /// Decode one packet, or run packet-loss concealment when `packet` is `None`.
    /// Returns interleaved stereo samples for exactly one frame.
    pub fn decode(&mut self, packet: Option<&[u8]>) -> Result<Vec<i16>, CodecError> {
        let mut out = vec![0i16; self.frame_samples * CHANNELS];
        let sig = MutSignals::try_from(&mut out[..]).map_err(e)?;
        let n = match packet {
            Some(p) => {
                let pk = audiopus::packet::Packet::try_from(p).map_err(e)?;
                self.dec.decode(Some(pk), sig, false).map_err(e)?
            }
            None => self.dec.decode(None, sig, false).map_err(e)?,
        };
        out.truncate(n * CHANNELS);
        Ok(out)
    }
}
