//! Where decoded audio goes. The real sink is `pacat` (PulseAudio and PipeWire-pulse both provide
//! it), the same way the agent captures with `parec`; tests use a closure.
use std::io::{self, Write};
use std::process::{Child, ChildStdin, Command, Stdio};

pub trait Sink: Send {
    /// Play interleaved 48 kHz stereo i16. May block until the sound server wants more.
    fn write(&mut self, pcm: &[i16]) -> io::Result<()>;
}

impl<F: FnMut(&[i16]) -> io::Result<()> + Send> Sink for F {
    fn write(&mut self, pcm: &[i16]) -> io::Result<()> {
        self(pcm)
    }
}

pub struct Pacat {
    child: Child,
    stdin: ChildStdin,
    bytes: Vec<u8>,
}

impl Pacat {
    pub fn start(latency_ms: u32) -> io::Result<Self> {
        let mut child = Command::new("pacat")
            .args(["--playback", "--raw", "--format=s16le", "--rate=48000", "--channels=2"])
            .arg(format!("--latency-msec={latency_ms}"))
            .args(["--client-name=Meshwav", "--stream-name=Meshwav stream"])
            .stdin(Stdio::piped())
            .spawn()
            .map_err(|e| io::Error::new(e.kind(), format!("cannot start pacat (install pulseaudio-utils): {e}")))?;
        let stdin = child.stdin.take().expect("piped");
        Ok(Self { child, stdin, bytes: Vec::new() })
    }
}

impl Sink for Pacat {
    fn write(&mut self, pcm: &[i16]) -> io::Result<()> {
        self.bytes.clear();
        for s in pcm {
            self.bytes.extend_from_slice(&s.to_le_bytes());
        }
        self.stdin.write_all(&self.bytes)
    }
}

impl Drop for Pacat {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

/// Raw s16le 48 kHz stereo into a file (or stdout for `-`): for machines without a sound server and for
/// checking what was received. Paced like real playback so the jitter buffer sees a realistic consumer.
pub struct RawFile {
    out: Box<dyn Write + Send>,
    bytes: Vec<u8>,
}

impl RawFile {
    pub fn create(path: &str) -> io::Result<Self> {
        let out: Box<dyn Write + Send> = if path == "-" { Box::new(io::stdout()) } else { Box::new(std::fs::File::create(path)?) };
        Ok(Self { out, bytes: Vec::new() })
    }
}

impl Sink for RawFile {
    fn write(&mut self, pcm: &[i16]) -> io::Result<()> {
        self.bytes.clear();
        for s in pcm {
            self.bytes.extend_from_slice(&s.to_le_bytes());
        }
        self.out.write_all(&self.bytes)?;
        self.out.flush()?;
        std::thread::sleep(std::time::Duration::from_micros(pcm.len() as u64 * 1_000_000 / (48_000 * 2)));
        Ok(())
    }
}
