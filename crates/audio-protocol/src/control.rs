//! Control-channel messages. Wire form (after the transport's own encryption):
//! `u16 length | u8 type | body`, where `length` covers type + body. Strict decoding:
//! unknown types, truncation, trailing bytes and oversized fields are rejected.
use crate::ProtocolError;

pub const MAX_CONTROL_LEN: usize = 1024;
pub const MAX_NAME: usize = 64;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u8)]
pub enum Codec {
    Opus = 1,
    Pcm = 2,
    Flac = 3,
}

impl Codec {
    fn from_u8(v: u8) -> Result<Self, ProtocolError> {
        match v {
            1 => Ok(Self::Opus),
            2 => Ok(Self::Pcm),
            3 => Ok(Self::Flac),
            _ => Err(ProtocolError::BadControl("unknown codec")),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Message {
    Hello { version: u8, name: String, codecs: Vec<Codec>, max_rate: u32, max_depth: u8 },
    Offer { name: String, rate: u32, channels: u8, depth: u8, codecs: Vec<Codec>, min_kbps: u32, max_kbps: u32 },
    Accept { codec: Codec, rate: u32, depth: u8, frame_us: u32, bitrate_kbps: u32, udp_port: u16 },
    StreamKey { stream_id: u32, key: [u8; 32], start_seq: u32 },
    Ping { t_us: u64 },
    Pong { echo_us: u64, t_us: u64 },
    Stats { received: u32, lost: u32, late: u32, concealed: u32, buffer_ms: u16, relayed: bool },
    Bye { reason: u8 },
    /// Receiver asks the agent to change the encoder at runtime. 0 means "keep the current value".
    /// `codec` is 0 (keep), 1 (Opus) or 2 (lossless PCM, always sent as 5 ms packets).
    SetConfig { req: u16, kbps: u32, frame_ms: u8, codec: u8 },
    /// Agent confirms; carries the values now in force. Repeated verbatim for a repeated `req`.
    ConfigApplied { req: u16, kbps: u32, frame_ms: u8, codec: u8 },
    /// Agent refuses; see `REJECT_*`.
    ConfigRejected { req: u16, reason: u8 },
    /// Agent tells a receiver whether it is the one getting audio (`true`) or on standby (`false`).
    /// Repeated about once a second, so a lost packet only delays the news.
    ActiveState { active: bool },
    /// Agent says: the audio frame stamped `ts` (the header's sample clock) was delivered by the capture
    /// stage at agent-clock time `t_us` (microseconds on the clock `Pong.t_us` reports). Sent about twice a
    /// second to the active receiver, so it can turn clock-offset + sample clock into a per-frame delay.
    TimeRef { ts: u32, t_us: u64 },
    /// Receiver asks the agent to send it the audio, and to keep doing so (same as `meshwav use`).
    PlayHere,
    /// Agent tells receivers it understands `PlayHere`. Repeated with the keepalive, so a receiver only offers
    /// the action to agents that support it. Older receivers fail to decode it and ignore it.
    PlayHereSupported,
    /// Set the output level (0..=100) of the receiver that is playing. Receiver to agent, then agent to
    /// the active receiver.
    SetVolume { percent: u8 },
    /// The playing receiver's current level. Receiver to agent, then agent to the other receivers.
    VolumeLevel { percent: u8 },
}

pub const REJECT_BITRATE: u8 = 1;
pub const REJECT_FRAME: u8 = 2;
pub const REJECT_ENCODER: u8 = 3;
pub const REJECT_CODEC: u8 = 4;
/// `codec` values of `SetConfig` / `ConfigApplied`.
pub const CODEC_KEEP: u8 = 0;
pub const CODEC_OPUS: u8 = 1;
pub const CODEC_PCM: u8 = 2;
/// Bytes of one lossless packet: 5 ms of 48 kHz stereo 16-bit PCM. Fits one datagram, so no fragmentation.
pub const PCM_FRAME_BYTES: usize = 960;
/// Bitrate range the agent accepts at runtime (kbit/s) and the frame sizes it can switch between (ms).
pub const MIN_KBPS: u32 = 64;
pub const MAX_KBPS: u32 = 256;
pub const FRAME_CHOICES_MS: [u8; 3] = [5, 10, 20];

struct W(Vec<u8>);
impl W {
    fn u8(&mut self, v: u8) { self.0.push(v) }
    fn u16(&mut self, v: u16) { self.0.extend_from_slice(&v.to_be_bytes()) }
    fn u32(&mut self, v: u32) { self.0.extend_from_slice(&v.to_be_bytes()) }
    fn u64(&mut self, v: u64) { self.0.extend_from_slice(&v.to_be_bytes()) }
    fn str(&mut self, s: &str) {
        let b = &s.as_bytes()[..s.len().min(MAX_NAME)];
        // never split a UTF-8 sequence
        let b = match std::str::from_utf8(b) {
            Ok(_) => b,
            Err(e) => &b[..e.valid_up_to()],
        };
        self.u8(b.len() as u8);
        self.0.extend_from_slice(b);
    }
    fn codecs(&mut self, c: &[Codec]) {
        let c = &c[..c.len().min(8)];
        self.u8(c.len() as u8);
        for x in c { self.u8(*x as u8) }
    }
}

struct R<'a>(&'a [u8]);
impl<'a> R<'a> {
    fn take(&mut self, n: usize) -> Result<&'a [u8], ProtocolError> {
        if self.0.len() < n { return Err(ProtocolError::BadControl("truncated")) }
        let (a, b) = self.0.split_at(n);
        self.0 = b;
        Ok(a)
    }
    fn u8(&mut self) -> Result<u8, ProtocolError> { Ok(self.take(1)?[0]) }
    fn pct(&mut self) -> Result<u8, ProtocolError> {
        match self.u8()? { v @ 0..=100 => Ok(v), _ => Err(ProtocolError::BadControl("bad percent")) }
    }
    fn u16(&mut self) -> Result<u16, ProtocolError> { Ok(u16::from_be_bytes(self.take(2)?.try_into().unwrap())) }
    fn u32(&mut self) -> Result<u32, ProtocolError> { Ok(u32::from_be_bytes(self.take(4)?.try_into().unwrap())) }
    fn u64(&mut self) -> Result<u64, ProtocolError> { Ok(u64::from_be_bytes(self.take(8)?.try_into().unwrap())) }
    fn str(&mut self) -> Result<String, ProtocolError> {
        let n = self.u8()? as usize;
        if n > MAX_NAME { return Err(ProtocolError::BadControl("name too long")) }
        String::from_utf8(self.take(n)?.to_vec()).map_err(|_| ProtocolError::BadControl("bad utf8"))
    }
    fn codecs(&mut self) -> Result<Vec<Codec>, ProtocolError> {
        let n = self.u8()? as usize;
        if n > 8 { return Err(ProtocolError::BadControl("too many codecs")) }
        (0..n).map(|_| Codec::from_u8(self.u8()?)).collect()
    }
}

impl Message {
    pub fn encode(&self) -> Vec<u8> {
        let mut w = W(vec![0, 0]);
        match self {
            Message::Hello { version, name, codecs, max_rate, max_depth } => {
                w.u8(1); w.u8(*version); w.str(name); w.codecs(codecs); w.u32(*max_rate); w.u8(*max_depth);
            }
            Message::Offer { name, rate, channels, depth, codecs, min_kbps, max_kbps } => {
                w.u8(2); w.str(name); w.u32(*rate); w.u8(*channels); w.u8(*depth); w.codecs(codecs);
                w.u32(*min_kbps); w.u32(*max_kbps);
            }
            Message::Accept { codec, rate, depth, frame_us, bitrate_kbps, udp_port } => {
                w.u8(3); w.u8(*codec as u8); w.u32(*rate); w.u8(*depth); w.u32(*frame_us);
                w.u32(*bitrate_kbps); w.u16(*udp_port);
            }
            Message::StreamKey { stream_id, key, start_seq } => {
                w.u8(4); w.u32(*stream_id); w.0.extend_from_slice(key); w.u32(*start_seq);
            }
            Message::Ping { t_us } => { w.u8(6); w.u64(*t_us); }
            Message::Pong { echo_us, t_us } => { w.u8(7); w.u64(*echo_us); w.u64(*t_us); }
            Message::Stats { received, lost, late, concealed, buffer_ms, relayed } => {
                w.u8(8); w.u32(*received); w.u32(*lost); w.u32(*late); w.u32(*concealed);
                w.u16(*buffer_ms); w.u8(*relayed as u8);
            }
            Message::Bye { reason } => { w.u8(9); w.u8(*reason); }
            Message::SetConfig { req, kbps, frame_ms, codec } => { w.u8(11); w.u16(*req); w.u32(*kbps); w.u8(*frame_ms); w.u8(*codec); }
            Message::ConfigApplied { req, kbps, frame_ms, codec } => { w.u8(12); w.u16(*req); w.u32(*kbps); w.u8(*frame_ms); w.u8(*codec); }
            Message::ConfigRejected { req, reason } => { w.u8(13); w.u16(*req); w.u8(*reason); }
            Message::ActiveState { active } => { w.u8(14); w.u8(*active as u8); }
            Message::TimeRef { ts, t_us } => { w.u8(15); w.u32(*ts); w.u64(*t_us); }
            Message::PlayHere => w.u8(16),
            Message::PlayHereSupported => w.u8(17),
            Message::SetVolume { percent } => { w.u8(18); w.u8(*percent); }
            Message::VolumeLevel { percent } => { w.u8(19); w.u8(*percent); }
        }
        let len = (w.0.len() - 2) as u16;
        w.0[0..2].copy_from_slice(&len.to_be_bytes());
        w.0
    }

    /// Decode one message from the front of `buf`. Returns the message and bytes consumed,
    /// or `Ok(None)` when more bytes are needed.
    pub fn decode(buf: &[u8]) -> Result<Option<(Message, usize)>, ProtocolError> {
        if buf.len() < 2 { return Ok(None) }
        let len = u16::from_be_bytes([buf[0], buf[1]]) as usize;
        if len == 0 || len > MAX_CONTROL_LEN { return Err(ProtocolError::BadControl("bad length")) }
        if buf.len() < 2 + len { return Ok(None) }
        let mut r = R(&buf[2..2 + len]);
        let msg = match r.u8()? {
            1 => Message::Hello { version: r.u8()?, name: r.str()?, codecs: r.codecs()?, max_rate: r.u32()?, max_depth: r.u8()? },
            2 => Message::Offer { name: r.str()?, rate: r.u32()?, channels: r.u8()?, depth: r.u8()?, codecs: r.codecs()?, min_kbps: r.u32()?, max_kbps: r.u32()? },
            3 => Message::Accept { codec: Codec::from_u8(r.u8()?)?, rate: r.u32()?, depth: r.u8()?, frame_us: r.u32()?, bitrate_kbps: r.u32()?, udp_port: r.u16()? },
            4 => Message::StreamKey { stream_id: r.u32()?, key: r.take(32)?.try_into().unwrap(), start_seq: r.u32()? },
            6 => Message::Ping { t_us: r.u64()? },
            7 => Message::Pong { echo_us: r.u64()?, t_us: r.u64()? },
            8 => Message::Stats { received: r.u32()?, lost: r.u32()?, late: r.u32()?, concealed: r.u32()?, buffer_ms: r.u16()?, relayed: match r.u8()? { 0 => false, 1 => true, _ => return Err(ProtocolError::BadControl("bad bool")) } },
            9 => Message::Bye { reason: r.u8()? },
            11 => Message::SetConfig { req: r.u16()?, kbps: r.u32()?, frame_ms: r.u8()?, codec: r.u8()? },
            12 => Message::ConfigApplied { req: r.u16()?, kbps: r.u32()?, frame_ms: r.u8()?, codec: r.u8()? },
            13 => Message::ConfigRejected { req: r.u16()?, reason: r.u8()? },
            14 => Message::ActiveState { active: match r.u8()? { 0 => false, 1 => true, _ => return Err(ProtocolError::BadControl("bad bool")) } },
            15 => Message::TimeRef { ts: r.u32()?, t_us: r.u64()? },
            16 => Message::PlayHere,
            17 => Message::PlayHereSupported,
            18 => Message::SetVolume { percent: r.pct()? },
            19 => Message::VolumeLevel { percent: r.pct()? },
            _ => return Err(ProtocolError::BadControl("unknown type")),
        };
        if !r.0.is_empty() { return Err(ProtocolError::BadControl("trailing bytes")) }
        Ok(Some((msg, 2 + len)))
    }
}
