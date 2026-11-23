//! Where decoded audio goes. The real sink is `pacat` (PulseAudio and PipeWire-pulse both provide
//! it), the same way the agent captures with `parec`; tests use a closure.
use std::io::{self, Write};
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::Arc;
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

/// Output level, 0 to 100 percent. Shared between the window and the playback thread.
pub struct Volume(AtomicU32);

impl Volume {
    pub fn new(percent: u32) -> Arc<Self> {
        Arc::new(Self(AtomicU32::new(percent.min(100))))
    }
    pub fn set(&self, percent: u32) {
        self.0.store(percent.min(100), Ordering::Relaxed);
    }
    pub fn percent(&self) -> u32 {
        self.0.load(Ordering::Relaxed)
    }
    /// Linear gain for the slider position. Squared so the lower half of the slider is usable;
    /// exactly 1.0 at 100 so full volume leaves the samples untouched.
    pub fn gain(&self) -> f32 {
        let p = self.percent() as f32 / 100.0;
        p * p
    }
}

/// Applies a [`Volume`] before the samples reach the real sink.
pub struct Gain<S> {
    inner: S,
    volume: Arc<Volume>,
    scaled: Vec<i16>,
}

impl<S: Sink> Gain<S> {
    pub fn new(inner: S, volume: Arc<Volume>) -> Self {
        Self { inner, volume, scaled: Vec::new() }
    }
}

impl<S: Sink> Sink for Gain<S> {
    fn write(&mut self, pcm: &[i16]) -> io::Result<()> {
        let g = self.volume.gain();
        if g >= 1.0 {
            return self.inner.write(pcm);
        }
        self.scaled.clear();
        self.scaled.extend(pcm.iter().map(|s| (*s as f32 * g).round() as i16));
        self.inner.write(&self.scaled)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    fn capture(volume: &Arc<Volume>, input: &[i16]) -> Vec<i16> {
        let got = Arc::new(Mutex::new(Vec::new()));
        let g2 = got.clone();
        let mut sink = Gain::new(move |p: &[i16]| { g2.lock().unwrap().extend_from_slice(p); Ok(()) }, volume.clone());
        sink.write(input).unwrap();
        let v = got.lock().unwrap().clone();
        v
    }

    #[test]
    fn full_volume_is_untouched() {
        let input = [i16::MIN, -1, 0, 1, 12345, i16::MAX];
        assert_eq!(capture(&Volume::new(100), &input), input);
    }

    #[test]
    fn zero_is_silence_and_half_slider_is_a_quarter_of_the_amplitude() {
        assert_eq!(capture(&Volume::new(0), &[1000, -1000]), [0, 0]);
        assert_eq!(capture(&Volume::new(50), &[1000, -1000, i16::MIN]), [250, -250, -8192]);
    }

    #[test]
    fn changes_apply_to_the_next_write() {
        let v = Volume::new(100);
        let got = Arc::new(Mutex::new(Vec::new()));
        let g2 = got.clone();
        let mut sink = Gain::new(move |p: &[i16]| { g2.lock().unwrap().extend_from_slice(p); Ok(()) }, v.clone());
        sink.write(&[400]).unwrap();
        v.set(50);
        sink.write(&[400]).unwrap();
        assert_eq!(*got.lock().unwrap(), [400, 100]);
    }
}
