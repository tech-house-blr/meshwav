//! Raw PCM packing. Samples are carried as i32 left-justified to `depth` bits (value range of the
//! depth), little-endian on the wire; f32 is carried as IEEE-754 bits.
use crate::CodecError;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PcmDepth {
    S16,
    S24,
    F32,
}

impl PcmDepth {
    pub fn bytes(self) -> usize {
        match self { Self::S16 => 2, Self::S24 => 3, Self::F32 => 4 }
    }
}

/// Pack integer samples (S16/S24). For F32 use [`pack_f32`].
pub fn pack_int(samples: &[i32], depth: PcmDepth, out: &mut Vec<u8>) -> Result<(), CodecError> {
    match depth {
        PcmDepth::S16 => {
            for &s in samples {
                let v = i16::try_from(s).map_err(|_| CodecError::Params("sample out of s16 range"))?;
                out.extend_from_slice(&v.to_le_bytes());
            }
        }
        PcmDepth::S24 => {
            for &s in samples {
                if !(-(1 << 23)..(1 << 23)).contains(&s) {
                    return Err(CodecError::Params("sample out of s24 range"));
                }
                out.extend_from_slice(&s.to_le_bytes()[..3]);
            }
        }
        PcmDepth::F32 => return Err(CodecError::Params("use pack_f32")),
    }
    Ok(())
}

pub fn unpack_int(data: &[u8], depth: PcmDepth) -> Result<Vec<i32>, CodecError> {
    let n = depth.bytes();
    if data.len() % n != 0 { return Err(CodecError::Malformed("pcm length not a multiple of sample size")) }
    match depth {
        PcmDepth::S16 => Ok(data.chunks_exact(2).map(|c| i16::from_le_bytes([c[0], c[1]]) as i32).collect()),
        PcmDepth::S24 => Ok(data
            .chunks_exact(3)
            .map(|c| i32::from_le_bytes([c[0], c[1], c[2], if c[2] & 0x80 != 0 { 0xff } else { 0 }]))
            .collect()),
        PcmDepth::F32 => Err(CodecError::Params("use unpack_f32")),
    }
}

pub fn pack_f32(samples: &[f32], out: &mut Vec<u8>) {
    for s in samples { out.extend_from_slice(&s.to_le_bytes()) }
}

pub fn unpack_f32(data: &[u8]) -> Result<Vec<f32>, CodecError> {
    if data.len() % 4 != 0 { return Err(CodecError::Malformed("pcm length not a multiple of 4")) }
    Ok(data.chunks_exact(4).map(|c| f32::from_le_bytes([c[0], c[1], c[2], c[3]])).collect())
}
