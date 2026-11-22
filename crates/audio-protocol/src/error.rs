use thiserror::Error;

#[derive(Debug, Error, PartialEq, Eq)]
pub enum ProtocolError {
    #[error("datagram too short")]
    TooShort,
    #[error("datagram exceeds maximum size")]
    TooLong,
    #[error("bad magic")]
    BadMagic,
    #[error("unsupported protocol version {0}")]
    BadVersion(u8),
    #[error("unknown packet type {0}")]
    BadType(u8),
    #[error("invalid fragment fields")]
    BadFragment,
    #[error("authentication failed")]
    AuthFailed,
    #[error("replayed or too-old packet")]
    Replay,
    #[error("sequence space exhausted, rekey required")]
    SeqExhausted,
    #[error("malformed control message: {0}")]
    BadControl(&'static str),
    #[error("handshake failed: {0}")]
    Handshake(&'static str),
    #[error("peer static key does not match the pinned key")]
    PeerMismatch,
}
