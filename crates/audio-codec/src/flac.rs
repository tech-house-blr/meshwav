//! Stateless FLAC framing. Each packet carries one self-contained FLAC frame (no STREAMINFO);
//! the decoder rebuilds a STREAMINFO from the negotiated parameters. This makes every packet
//! independently decodable, so loss never propagates.
use crate::CodecError;
use flacenc::component::BitRepr;
use flacenc::error::Verify;

/// Length of "fLaC" + STREAMINFO metadata block that flacenc emits before the first frame.
const HEADER_LEN: usize = 42;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct FlacParams {
    pub rate: u32,
    pub channels: u8,
    pub depth: u8,
    /// Samples per channel in each packet.
    pub block: u16,
}

impl FlacParams {
    fn validate(&self) -> Result<(), CodecError> {
        if !(1..=8).contains(&self.channels) || !(8..=24).contains(&self.depth) || self.block < 16 || self.rate == 0 || self.rate >= (1 << 20) {
            return Err(CodecError::Params("bad flac parameters"));
        }
        Ok(())
    }
}

pub struct FlacEncoder {
    p: FlacParams,
    cfg: flacenc::error::Verified<flacenc::config::Encoder>,
}

impl FlacEncoder {
    pub fn new(p: FlacParams) -> Result<Self, CodecError> {
        p.validate()?;
        let mut cfg = flacenc::config::Encoder::default();
        cfg.block_size = p.block as usize;
        let cfg = cfg.into_verified().map_err(|e| CodecError::Flac(format!("{e:?}")))?;
        Ok(Self { p, cfg })
    }

    /// Encode one block of interleaved samples (`block * channels` values) into a bare frame.
    pub fn encode(&self, samples: &[i32]) -> Result<Vec<u8>, CodecError> {
        if samples.len() != self.p.block as usize * self.p.channels as usize {
            return Err(CodecError::Params("wrong block length"));
        }
        // flacenc caps the sample rate at 96 kHz. The rate only labels the stream and does not
        // influence prediction/coding, and we reconstruct STREAMINFO from the negotiated rate on
        // the decoder, so for higher rates we label frames with the cap. Verified bit-exact in tests.
        let enc_rate = self.p.rate.min(96_000);
        let src = flacenc::source::MemSource::from_samples(
            samples, self.p.channels as usize, self.p.depth as usize, enc_rate as usize);
        let stream = flacenc::encode_with_fixed_block_size(&self.cfg, src, self.p.block as usize)
            .map_err(|e| CodecError::Flac(format!("{e:?}")))?;
        let mut sink = flacenc::bitsink::ByteSink::new();
        stream.write(&mut sink).map_err(|e| CodecError::Flac(format!("{e:?}")))?;
        let bytes = sink.as_slice();
        if bytes.len() <= HEADER_LEN || &bytes[..4] != b"fLaC" {
            return Err(CodecError::Flac("unexpected encoder output".into()));
        }
        Ok(bytes[HEADER_LEN..].to_vec())
    }
}

pub struct FlacDecoder {
    header: Vec<u8>,
    p: FlacParams,
}

impl FlacDecoder {
    pub fn new(p: FlacParams) -> Result<Self, CodecError> {
        p.validate()?;
        let mut h = Vec::with_capacity(HEADER_LEN);
        h.extend_from_slice(b"fLaC");
        h.extend_from_slice(&[0x80, 0, 0, 34]); // last metadata block, STREAMINFO, length 34
        h.extend_from_slice(&p.block.to_be_bytes()); // min block
        h.extend_from_slice(&p.block.to_be_bytes()); // max block
        h.extend_from_slice(&[0; 6]); // min/max frame size unknown
        let packed: u64 = ((p.rate as u64) << 44) | (((p.channels - 1) as u64) << 41) | (((p.depth - 1) as u64) << 36);
        h.extend_from_slice(&packed.to_be_bytes()); // rate(20) ch(3) bps(5) total_samples(36)=0
        h.extend_from_slice(&[0; 16]); // md5 unset
        debug_assert_eq!(h.len(), HEADER_LEN);
        Ok(Self { header: h, p })
    }

    /// Decode one frame into interleaved samples. Output length must match the negotiated block.
    pub fn decode(&self, frame: &[u8]) -> Result<Vec<i32>, CodecError> {
        let mut data = Vec::with_capacity(self.header.len() + frame.len());
        data.extend_from_slice(&self.header);
        data.extend_from_slice(frame);
        let mut r = claxon::FlacReader::new(std::io::Cursor::new(data))
            .map_err(|e| CodecError::Flac(e.to_string()))?;
        let mut out = Vec::with_capacity(self.p.block as usize * self.p.channels as usize);
        for s in r.samples() {
            out.push(s.map_err(|e| CodecError::Flac(e.to_string()))?);
        }
        if out.len() != self.p.block as usize * self.p.channels as usize {
            return Err(CodecError::Malformed("decoded block has unexpected length"));
        }
        Ok(out)
    }
}
