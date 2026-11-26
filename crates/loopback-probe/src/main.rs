//! Throwaway probe: does a per-process loopback capture hear audio while the output volume is 0,
//! when a plain loopback capture of the output does not? Prints the loudest sample of each, per second.
#[cfg(not(windows))]
fn main() {}

#[cfg(windows)]
fn main() {
    probe::run();
}

#[cfg(windows)]
mod probe {
    use std::sync::{Arc, Condvar, Mutex};
    use std::time::{Duration, Instant};
    use windows::core::{implement, w, Interface, Result, GUID};
    use windows::Win32::Foundation::HANDLE;
    use windows::Win32::Media::Audio::Endpoints::IAudioEndpointVolume;
    use windows::Win32::Media::Audio::*;
    use windows::Win32::System::Com::*;

    #[implement(IActivateAudioInterfaceCompletionHandler)]
    struct Handler(Arc<(Mutex<bool>, Condvar)>);

    impl IActivateAudioInterfaceCompletionHandler_Impl for Handler {
        fn ActivateCompleted(&self, _op: Option<&IActivateAudioInterfaceAsyncOperation>) -> Result<()> {
            *self.0 .0.lock().unwrap() = true;
            self.0 .1.notify_all();
            Ok(())
        }
    }

    fn format() -> WAVEFORMATEX {
        WAVEFORMATEX { wFormatTag: 1, nChannels: 2, nSamplesPerSec: 48000, nAvgBytesPerSec: 48000 * 4, nBlockAlign: 4, wBitsPerSample: 16, cbSize: 0 }
    }

    /// Per-process loopback of everything except this process.
    unsafe fn process_loopback() -> Result<IAudioClient> {
        let mut params = AUDIOCLIENT_ACTIVATION_PARAMS {
            ActivationType: AUDIOCLIENT_ACTIVATION_TYPE_PROCESS_LOOPBACK,
            Anonymous: AUDIOCLIENT_ACTIVATION_PARAMS_0 {
                ProcessLoopbackParams: AUDIOCLIENT_PROCESS_LOOPBACK_PARAMS {
                    TargetProcessId: std::process::id(),
                    ProcessLoopbackMode: PROCESS_LOOPBACK_MODE_EXCLUDE_TARGET_PROCESS_TREE,
                },
            },
        };
        // a PROPVARIANT holding a blob, laid out by hand (vt = VT_BLOB, then the BLOB)
        #[repr(C)]
        struct RawPv {
            vt: u16,
            r: [u16; 3],
            cb: u32,
            ptr: *mut u8,
        }
        let raw = RawPv { vt: 65, r: [0; 3], cb: std::mem::size_of::<AUDIOCLIENT_ACTIVATION_PARAMS>() as u32, ptr: &mut params as *mut _ as *mut u8 };
        let pv: windows::core::PROPVARIANT = std::mem::transmute(raw);
        let state = Arc::new((Mutex::new(false), Condvar::new()));
        let handler: IActivateAudioInterfaceCompletionHandler = Handler(state.clone()).into();
        let op = ActivateAudioInterfaceAsync(w!("VAD\\Process_Loopback"), &IAudioClient::IID, Some(&pv as *const _), &handler)?;
        let (m, cv) = &*state;
        std::mem::forget(pv);
        let (done, _) = cv.wait_timeout_while(m.lock().unwrap(), Duration::from_secs(10), |d| !*d).unwrap();
        if !*done {
            return Err(windows::core::Error::new(windows::Win32::Foundation::E_FAIL, "activation timed out"));
        }
        let mut hr = windows::core::HRESULT(0);
        let mut unk: Option<windows::core::IUnknown> = None;
        op.GetActivateResult(&mut hr, &mut unk)?;
        hr.ok()?;
        unk.unwrap().cast::<IAudioClient>()
    }

    unsafe fn start(client: &IAudioClient, extra: u32) -> Result<IAudioCaptureClient> {
        let fmt = format();
        client.Initialize(AUDCLNT_SHAREMODE_SHARED, AUDCLNT_STREAMFLAGS_LOOPBACK | extra, 2_000_000, 0, &fmt, None)?;
        let cap: IAudioCaptureClient = client.GetService()?;
        client.Start()?;
        Ok(cap)
    }

    /// Loudest sample (0..32767) waiting in the capture buffer; drains it.
    unsafe fn drain(cap: &IAudioCaptureClient) -> u32 {
        let mut peak = 0i32;
        while let Ok(n) = cap.GetNextPacketSize() {
            if n == 0 {
                break;
            }
            let mut data = std::ptr::null_mut();
            let (mut frames, mut flags, mut p1, mut p2) = (0u32, 0u32, 0u64, 0u64);
            if cap.GetBuffer(&mut data, &mut frames, &mut flags, Some(&mut p1), Some(&mut p2)).is_err() {
                break;
            }
            if flags & (AUDCLNT_BUFFERFLAGS_SILENT.0 as u32) == 0 {
                let s = std::slice::from_raw_parts(data as *const i16, frames as usize * 2);
                for v in s {
                    peak = peak.max((*v as i32).abs());
                }
            }
            let _ = cap.ReleaseBuffer(frames);
        }
        peak as u32
    }

    struct Restore(IAudioEndpointVolume, f32, bool);
    impl Drop for Restore {
        fn drop(&mut self) {
            unsafe {
                let _ = self.0.SetMute(self.2, std::ptr::null());
                let _ = self.0.SetMasterVolumeLevelScalar(self.1, std::ptr::null());
            }
            println!("volume restored to {:.0}%", self.1 * 100.0);
        }
    }

    pub fn run() {
        unsafe {
            CoInitializeEx(None, COINIT_MULTITHREADED).ok().unwrap();
            let en: IMMDeviceEnumerator = CoCreateInstance(&MMDeviceEnumerator, None, CLSCTX_ALL).unwrap();
            let dev = en.GetDefaultAudioEndpoint(eRender, eConsole).unwrap();
            let vol: IAudioEndpointVolume = dev.Activate(CLSCTX_ALL, None).unwrap();
            let name = dev.GetId().unwrap().to_string().unwrap_or_default();
            let orig = vol.GetMasterVolumeLevelScalar().unwrap();
            let muted = vol.GetMute().unwrap().as_bool();
            println!("default output id {name}; volume {:.0}%, muted {muted}", orig * 100.0);

            let out_client: IAudioClient = dev.Activate(CLSCTX_ALL, None).unwrap();
            let out_cap = start(&out_client, 0).map_err(|e| println!("output loopback failed: {e}")).ok();
            let proc_client = process_loopback().map_err(|e| println!("process loopback activation failed: {e}")).ok();
            let proc_cap = proc_client.as_ref().and_then(|c| {
                start(c, AUDCLNT_STREAMFLAGS_AUTOCONVERTPCM | AUDCLNT_STREAMFLAGS_SRC_DEFAULT_QUALITY)
                    .map_err(|e| println!("process loopback start failed: {e}"))
                    .ok()
            });

            println!("phase 1 (6 s): original volume. Play music now.");
            let t0 = Instant::now();
            let mut _guard = None;
            let mut phase = 1;
            let (mut po, mut pp) = (0u32, 0u32);
            let mut last = Instant::now();
            while t0.elapsed() < Duration::from_secs(20) {
                std::thread::sleep(Duration::from_millis(10));
                if let Some(c) = &out_cap {
                    po = po.max(drain(c));
                }
                if let Some(c) = &proc_cap {
                    pp = pp.max(drain(c));
                }
                if phase == 1 && t0.elapsed() > Duration::from_secs(6) {
                    phase = 2;
                    _guard = Some(Restore(vol.clone(), orig, muted));
                    let _ = vol.SetMasterVolumeLevelScalar(0.0, std::ptr::null());
                    println!("phase 2 (14 s): volume set to 0. Keep playing.");
                }
                if last.elapsed() >= Duration::from_secs(1) {
                    last = Instant::now();
                    let v = vol.GetMasterVolumeLevelScalar().unwrap_or(-1.0);
                    println!("phase {phase} vol {:>3.0}%  output-loopback peak {po:>5}  process-loopback peak {pp:>5}", v * 100.0);
                    po = 0;
                    pp = 0;
                }
            }
            let _ = GUID::zeroed();
            let _ = HANDLE::default();
        }
    }
}
