//! Per-process WASAPI loopback (Windows 10 version 2004 and later): "everything except this process".
//!
//! A plain loopback of an output device hears the audio after Windows has applied that device's volume,
//! and on some outputs (HDMI monitors, for one) that means silence once the volume is set to 0. The
//! per-process mode hears the apps' audio before any device's volume, and it follows the apps to
//! whichever output they play on, so muting the PC does not mute the capture.
use std::sync::{Arc, Condvar, Mutex};
use std::time::Duration;
use windows::core::{implement, w, Interface, Result as WinResult};
use windows::Win32::Media::Audio::*;
use windows::Win32::System::Com::{CoInitializeEx, COINIT_MULTITHREADED};

#[implement(IActivateAudioInterfaceCompletionHandler)]
struct Handler(Arc<(Mutex<bool>, Condvar)>);

#[allow(non_snake_case)]
impl IActivateAudioInterfaceCompletionHandler_Impl for Handler {
    fn ActivateCompleted(&self, _op: Option<&IActivateAudioInterfaceAsyncOperation>) -> WinResult<()> {
        *self.0 .0.lock().unwrap() = true;
        self.0 .1.notify_all();
        Ok(())
    }
}

pub struct ProcessCapture {
    client: IAudioClient,
    cap: IAudioCaptureClient,
}

/// Start capturing everything except this process, as 48 kHz stereo s16le. Call it on the thread that
/// will read from it.
pub fn open() -> Result<ProcessCapture, String> {
    unsafe { open_inner() }.map_err(|e| e.to_string())
}

unsafe fn open_inner() -> WinResult<ProcessCapture> {
    let _ = CoInitializeEx(None, COINIT_MULTITHREADED); // already initialised on this thread is fine
    let mut params = AUDIOCLIENT_ACTIVATION_PARAMS {
        ActivationType: AUDIOCLIENT_ACTIVATION_TYPE_PROCESS_LOOPBACK,
        Anonymous: AUDIOCLIENT_ACTIVATION_PARAMS_0 {
            ProcessLoopbackParams: AUDIOCLIENT_PROCESS_LOOPBACK_PARAMS {
                TargetProcessId: std::process::id(),
                ProcessLoopbackMode: PROCESS_LOOPBACK_MODE_EXCLUDE_TARGET_PROCESS_TREE,
            },
        },
    };
    // A PROPVARIANT holding a blob (VT_BLOB = 65), laid out by hand: windows 0.54 keeps the type opaque.
    #[repr(C)]
    struct RawPv {
        vt: u16,
        reserved: [u16; 3],
        size: u32,
        data: *mut u8,
    }
    let raw = RawPv {
        vt: 65,
        reserved: [0; 3],
        size: std::mem::size_of::<AUDIOCLIENT_ACTIVATION_PARAMS>() as u32,
        data: &mut params as *mut _ as *mut u8,
    };
    let pv: windows::core::PROPVARIANT = std::mem::transmute(raw);
    let state = Arc::new((Mutex::new(false), Condvar::new()));
    let handler: IActivateAudioInterfaceCompletionHandler = Handler(state.clone()).into();
    let op = ActivateAudioInterfaceAsync(w!("VAD\\Process_Loopback"), &IAudioClient::IID, Some(&pv as *const _), &handler);
    // `pv` only borrows `params`; it must not free the blob it points at
    std::mem::forget(pv);
    let op = op?;
    let (m, cv) = &*state;
    let (done, _) = cv.wait_timeout_while(m.lock().unwrap(), Duration::from_secs(10), |d| !*d).unwrap();
    if !*done {
        return Err(windows::core::Error::new(windows::Win32::Foundation::E_FAIL, "activation timed out"));
    }
    drop(done);
    let mut hr = windows::core::HRESULT(0);
    let mut unk: Option<windows::core::IUnknown> = None;
    op.GetActivateResult(&mut hr, &mut unk)?;
    hr.ok()?;
    let client = unk.ok_or_else(|| windows::core::Error::from(windows::Win32::Foundation::E_POINTER))?.cast::<IAudioClient>()?;
    let fmt = WAVEFORMATEX { wFormatTag: 1, nChannels: 2, nSamplesPerSec: 48_000, nAvgBytesPerSec: 48_000 * 4, nBlockAlign: 4, wBitsPerSample: 16, cbSize: 0 };
    client.Initialize(
        AUDCLNT_SHAREMODE_SHARED,
        AUDCLNT_STREAMFLAGS_LOOPBACK | AUDCLNT_STREAMFLAGS_AUTOCONVERTPCM | AUDCLNT_STREAMFLAGS_SRC_DEFAULT_QUALITY,
        2_000_000,
        0,
        &fmt,
        None,
    )?;
    let cap: IAudioCaptureClient = client.GetService()?;
    client.Start()?;
    Ok(ProcessCapture { client, cap })
}

impl ProcessCapture {
    /// Append what has been captured since the last call to `out`; returns the loudest sample (0..32767).
    pub fn drain(&self, out: &mut Vec<u8>) -> u32 {
        let mut peak = 0i32;
        unsafe {
            while let Ok(n) = self.cap.GetNextPacketSize() {
                if n == 0 {
                    break;
                }
                let mut data = std::ptr::null_mut();
                let (mut frames, mut flags) = (0u32, 0u32);
                if self.cap.GetBuffer(&mut data, &mut frames, &mut flags, None, None).is_err() {
                    break;
                }
                let len = frames as usize * 4;
                if flags & (AUDCLNT_BUFFERFLAGS_SILENT.0 as u32) != 0 || data.is_null() {
                    out.extend(std::iter::repeat(0u8).take(len));
                } else {
                    let bytes = std::slice::from_raw_parts(data, len);
                    for s in bytes.chunks_exact(2) {
                        peak = peak.max((i16::from_le_bytes([s[0], s[1]]) as i32).abs());
                    }
                    out.extend_from_slice(bytes);
                }
                let _ = self.cap.ReleaseBuffer(frames);
            }
        }
        peak as u32
    }
}

impl Drop for ProcessCapture {
    fn drop(&mut self) {
        unsafe {
            let _ = self.client.Stop();
        }
    }
}
