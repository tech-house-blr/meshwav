//! Meshwav codecs. All codecs work on interleaved samples and are frame-oriented: one call
//! encodes exactly one packet-duration worth of audio. FLAC frames are self-contained so a lost
//! packet never corrupts the next one.

pub mod flac;
pub mod opus;
pub mod pcm;

use thiserror::Error;

#[derive(Debug, Error)]
pub enum CodecError {
    #[error("opus: {0}")]
    Opus(String),
    #[error("flac: {0}")]
    Flac(String),
    #[error("invalid parameters: {0}")]
    Params(&'static str),
    #[error("malformed input: {0}")]
    Malformed(&'static str),
}
