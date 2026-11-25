//! The Windows counterpart of `redirect`: while a receiver is connected the PC's default output is
//! turned down to 0 so it goes quiet, and the previous volume comes back when the receiver is gone
//! (after a grace period), says goodbye, or the agent exits. WASAPI loopback captures the audio
//! before the output's master volume, so the stream keeps its full level.
//!
//! The volume to give back is written to a state file first; a start after a crash restores it.
use std::path::PathBuf;
use std::time::{Duration, Instant};
use windows::core::PCWSTR;
use windows::Win32::Media::Audio::Endpoints::IAudioEndpointVolume;
use windows::Win32::Media::Audio::{eConsole, eRender, IMMDevice, IMMDeviceEnumerator, MMDeviceEnumerator};
use windows::Win32::System::Com::{CoCreateInstance, CoInitializeEx, CoTaskMemFree, CLSCTX_ALL, COINIT_MULTITHREADED};

/// Network blips shorter than this keep the output silent.
const GRACE: Duration = Duration::from_secs(15);

struct Saved {
    device_id: String,
    volume: f32,
}

pub struct Duck {
    state_file: PathBuf,
    saved: Option<Saved>,
    gone_since: Option<Instant>,
}

fn enumerator() -> Result<IMMDeviceEnumerator, String> {
    unsafe {
        let _ = CoInitializeEx(None, COINIT_MULTITHREADED); // S_FALSE when this thread already did
        CoCreateInstance(&MMDeviceEnumerator, None, CLSCTX_ALL).map_err(|e| e.to_string())
    }
}

fn volume_of(dev: &IMMDevice) -> Result<IAudioEndpointVolume, String> {
    unsafe { dev.Activate::<IAudioEndpointVolume>(CLSCTX_ALL, None).map_err(|e| e.to_string()) }
}

fn default_device() -> Result<(String, IAudioEndpointVolume), String> {
    unsafe {
        let dev = enumerator()?.GetDefaultAudioEndpoint(eRender, eConsole).map_err(|e| e.to_string())?;
        let id = dev.GetId().map_err(|e| e.to_string())?;
        let s = id.to_string().map_err(|e| e.to_string());
        CoTaskMemFree(Some(id.0 as *const _));
        Ok((s?, volume_of(&dev)?))
    }
}

fn device_by_id(id: &str) -> Result<IAudioEndpointVolume, String> {
    let wide: Vec<u16> = id.encode_utf16().chain(std::iter::once(0)).collect();
    unsafe {
        let dev = enumerator()?.GetDevice(PCWSTR(wide.as_ptr())).map_err(|e| e.to_string())?;
        volume_of(&dev)
    }
}

fn set_level(v: &IAudioEndpointVolume, level: f32) -> Result<(), String> {
    unsafe { v.SetMasterVolumeLevelScalar(level, std::ptr::null()).map_err(|e| e.to_string()) }
}

fn level(v: &IAudioEndpointVolume) -> Result<f32, String> {
    unsafe { v.GetMasterVolumeLevelScalar().map_err(|e| e.to_string()) }
}

impl Duck {
    pub fn new(state_file: PathBuf) -> Self {
        Duck { state_file, saved: None, gone_since: None }
    }

    /// Give back the volume a previous run that died left at 0.
    pub fn start(&mut self) -> Result<(), String> {
        enumerator()?; // fail early if there is no audio stack
        if let Ok(text) = std::fs::read_to_string(&self.state_file) {
            let mut lines = text.lines();
            if let (Some(id), Some(vol)) = (lines.next(), lines.next().and_then(|v| v.parse::<f32>().ok())) {
                info!("redirect: restoring the volume a previous run left at 0");
                self.saved = Some(Saved { device_id: id.to_string(), volume: vol.clamp(0.0, 1.0) });
                self.release();
            }
            let _ = std::fs::remove_file(&self.state_file);
        }
        Ok(())
    }

    pub fn tick(&mut self, connected: bool, now: Instant) {
        if connected {
            self.gone_since = None;
            match &self.saved {
                None => self.engage(),
                Some(s) => {
                    // Keep it silent if a volume key or app raised it again.
                    if let Ok(v) = device_by_id(&s.device_id) {
                        if level(&v).map(|l| l > 0.0).unwrap_or(false) {
                            let _ = set_level(&v, 0.0);
                        }
                    }
                }
            }
        } else if self.saved.is_some() {
            let since = *self.gone_since.get_or_insert(now);
            if now.duration_since(since) >= GRACE {
                self.release();
            }
        }
    }

    fn engage(&mut self) {
        let (id, v) = match default_device() {
            Ok(x) => x,
            Err(e) => return warn!("redirect: cannot reach the output ({e}); will retry"),
        };
        let Ok(vol) = level(&v) else { return };
        // Save before touching anything, so a crash right after cannot lose the volume.
        let vol = if vol > 0.0 { vol } else { 0.3 }; // it was already 0 (a stale run): hand back something audible
        let _ = std::fs::write(&self.state_file, format!("{id}\n{vol}\n"));
        if set_level(&v, 0.0).is_ok() {
            info!("redirect: this PC's output is silent while the receiver plays (volume was {:.0}%)", vol * 100.0);
            self.saved = Some(Saved { device_id: id, volume: vol });
        }
    }

    /// Turn the volume back up. Safe to call when not engaged.
    pub fn release(&mut self) {
        let Some(s) = self.saved.take() else { return };
        self.gone_since = None;
        match device_by_id(&s.device_id).and_then(|v| set_level(&v, s.volume)) {
            Ok(()) => {
                info!("redirect: volume back to {:.0}%", s.volume * 100.0);
                let _ = std::fs::remove_file(&self.state_file);
            }
            Err(e) => warn!("redirect: could not restore the volume ({e}); it stays in {}", self.state_file.display()),
        }
    }

    pub fn shutdown(&mut self) {
        self.release();
    }
}
