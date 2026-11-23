//! Meshwav Linux receiver: connects to a paired agent over the tailnet and plays its audio.
//! The receiving side mirrors the Android app: Noise handshake (pairing code once, pinned agent key
//! afterwards), adaptive jitter buffer, Opus or lossless PCM, loss concealment.

pub mod cli;
pub mod conceal;
pub mod discover;
pub mod jitter;
pub mod session;
pub mod sink;
pub mod store;

pub use session::{pair, run, Settings, Snapshot, Status};
