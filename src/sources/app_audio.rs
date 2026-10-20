//! Application audio capture: WASAPI process loopback (Windows 10 2004+). Captures what one
//! application (with its child processes) plays, everything but one application, or the whole
//! system's sound minus LiveBroadcaster itself — independent of which playback device is used.

use std::sync::Arc;
use std::time::{Duration, Instant};

use windows::core::{implement, Interface, Ref};
use windows::Win32::Foundation::{CloseHandle, HANDLE, WAIT_OBJECT_0};
use windows::Win32::Media::Audio::*;
use windows::Win32::System::Com::StructuredStorage::PROPVARIANT;
use windows::Win32::System::Com::{CoInitializeEx, BLOB, COINIT_MULTITHREADED};
use windows::Win32::System::Threading::{CreateEventW, WaitForSingleObject};
use windows::Win32::System::Variant::VT_BLOB;

use super::SourceShared;
use crate::audio::{CHANNELS, SAMPLE_RATE};
use crate::model::{AppAudioMode, SourceKind};

/// An application that can be captured, for the properties dialog.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AppInfo {
    pub exe: String,
    pub title: String,
}

/// Applications with a window, one entry per executable.
pub fn list_apps() -> Vec<AppInfo> {
    let mut out: Vec<AppInfo> = Vec::new();
    for w in super::window::list_windows() {
        if !w.target.exe.is_empty() && !out.iter().any(|a| a.exe.eq_ignore_ascii_case(&w.target.exe)) {
            out.push(AppInfo { exe: w.target.exe, title: w.target.title });
        }
    }
    out.sort_by_key(|a| a.exe.to_lowercase());
    out
}

/// The root process running `exe` (the one whose parent isn't the same program), so its whole
/// process tree is captured (browsers play sound from a child process).
fn find_process(exe: &str) -> Option<u32> {
    use sysinfo::{ProcessRefreshKind, ProcessesToUpdate, System};
    let mut sys = System::new();
    sys.refresh_processes_specifics(ProcessesToUpdate::All, true, ProcessRefreshKind::nothing());
    let procs = sys.processes();
    let same = |p: &sysinfo::Process| p.name().to_string_lossy().eq_ignore_ascii_case(exe);
    procs
        .values()
        .filter(|p| same(p))
        .filter(|p| p.parent().and_then(|pp| procs.get(&pp)).is_none_or(|pp| !same(pp)))
        .map(|p| p.pid().as_u32())
        .min()
}

fn process_alive(pid: u32) -> bool {
    use windows::Win32::System::Threading::{GetExitCodeProcess, OpenProcess, PROCESS_QUERY_LIMITED_INFORMATION};
    unsafe {
        let Ok(h) = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid) else { return false };
        let mut code = 0u32;
        let ok = GetExitCodeProcess(h, &mut code).is_ok();
        let _ = CloseHandle(h);
        ok && code == 259 // STILL_ACTIVE
    }
}

pub fn run(kind: SourceKind, s: Arc<SourceShared>) {
    let SourceKind::AppAudio { mode, exe, .. } = kind else { return };
    unsafe {
        let _ = CoInitializeEx(None, COINIT_MULTITHREADED);
    }
    while !s.stopped() {
        let (pid, loopback_mode) = match mode {
            AppAudioMode::System => (std::process::id(), PROCESS_LOOPBACK_MODE_EXCLUDE_TARGET_PROCESS_TREE),
            AppAudioMode::Only | AppAudioMode::AllExcept if exe.is_empty() => {
                s.set_error("no application selected");
                return;
            }
            AppAudioMode::Only | AppAudioMode::AllExcept => match find_process(&exe) {
                Some(pid) => (
                    pid,
                    if mode == AppAudioMode::Only { PROCESS_LOOPBACK_MODE_INCLUDE_TARGET_PROCESS_TREE } else { PROCESS_LOOPBACK_MODE_EXCLUDE_TARGET_PROCESS_TREE },
                ),
                None if mode == AppAudioMode::AllExcept => (std::process::id(), PROCESS_LOOPBACK_MODE_EXCLUDE_TARGET_PROCESS_TREE),
                None => {
                    s.set_error(format!("{exe} is not running"));
                    if s.sleep(Duration::from_secs(2)) {
                        return;
                    }
                    continue;
                }
            },
        };
        match unsafe { capture(pid, loopback_mode, &s, mode != AppAudioMode::System) } {
            Ok(()) => {}
            Err(e) => {
                s.set_error(e);
                if s.sleep(Duration::from_secs(2)) {
                    return;
                }
            }
        }
    }
}

#[implement(IActivateAudioInterfaceCompletionHandler)]
struct Activated(crossbeam_channel::Sender<()>);

impl IActivateAudioInterfaceCompletionHandler_Impl for Activated_Impl {
    fn ActivateCompleted(&self, _op: Ref<IActivateAudioInterfaceAsyncOperation>) -> windows::core::Result<()> {
        let _ = self.0.send(());
        Ok(())
    }
}


unsafe fn activate(pid: u32, mode: PROCESS_LOOPBACK_MODE) -> Result<IAudioClient, String> {
    let params = AUDIOCLIENT_ACTIVATION_PARAMS {
        ActivationType: AUDIOCLIENT_ACTIVATION_TYPE_PROCESS_LOOPBACK,
        Anonymous: AUDIOCLIENT_ACTIVATION_PARAMS_0 {
            ProcessLoopbackParams: AUDIOCLIENT_PROCESS_LOOPBACK_PARAMS { TargetProcessId: pid, ProcessLoopbackMode: mode },
        },
    };
    // ManuallyDrop: dropping a PROPVARIANT runs PropVariantClear, which would CoTaskMemFree
    // the (stack) blob.
    let mut pv = std::mem::ManuallyDrop::new(PROPVARIANT::default());
    {
        let inner = &mut *pv.Anonymous.Anonymous;
        inner.vt = VT_BLOB;
        inner.Anonymous.blob = BLOB { cbSize: std::mem::size_of::<AUDIOCLIENT_ACTIVATION_PARAMS>() as u32, pBlobData: &params as *const _ as *mut u8 };
    }
    let (tx, rx) = crossbeam_channel::bounded(1);
    let handler: IActivateAudioInterfaceCompletionHandler = Activated(tx).into();
    let op = ActivateAudioInterfaceAsync(VIRTUAL_AUDIO_DEVICE_PROCESS_LOOPBACK, &IAudioClient::IID, Some(&*pv), &handler)
        .map_err(|e| format!("process loopback needs Windows 10 2004 or later ({e})"))?;
    rx.recv_timeout(Duration::from_secs(5)).map_err(|_| "audio activation timed out".to_string())?;
    let mut hr = windows::core::HRESULT(0);
    let mut unk = None;
    op.GetActivateResult(&mut hr, &mut unk).map_err(|e| format!("activate: {e}"))?;
    hr.ok().map_err(|e| format!("activate: {e}"))?;
    unk.ok_or("no audio client")?.cast().map_err(|e| format!("IAudioClient: {e}"))
}

/// Capture until the source stops, the stream fails, or (`watch_pid`) the process exits.
unsafe fn capture(pid: u32, mode: PROCESS_LOOPBACK_MODE, s: &Arc<SourceShared>, watch_pid: bool) -> Result<(), String> {
    let client = activate(pid, mode)?;
    // Process loopback has no mix format of its own: ask for the studio's (the engine converts).
    let fmt = WAVEFORMATEX {
        wFormatTag: 3, // WAVE_FORMAT_IEEE_FLOAT
        nChannels: CHANNELS as u16,
        nSamplesPerSec: SAMPLE_RATE,
        nAvgBytesPerSec: SAMPLE_RATE * CHANNELS as u32 * 4,
        nBlockAlign: CHANNELS as u16 * 4,
        wBitsPerSample: 32,
        cbSize: 0,
    };
    client
        .Initialize(
            AUDCLNT_SHAREMODE_SHARED,
            AUDCLNT_STREAMFLAGS_LOOPBACK | AUDCLNT_STREAMFLAGS_EVENTCALLBACK | AUDCLNT_STREAMFLAGS_AUTOCONVERTPCM | AUDCLNT_STREAMFLAGS_SRC_DEFAULT_QUALITY,
            200_000, // 20 ms
            0,
            &fmt,
            None,
        )
        .map_err(|e| format!("audio client: {e}"))?;
    let event = CreateEventW(None, false, false, None).map_err(|e| format!("event: {e}"))?;
    struct Ev(HANDLE);
    impl Drop for Ev {
        fn drop(&mut self) {
            unsafe {
                let _ = CloseHandle(self.0);
            }
        }
    }
    let event = Ev(event);
    client.SetEventHandle(event.0).map_err(|e| format!("SetEventHandle: {e}"))?;
    let cap: IAudioCaptureClient = client.GetService().map_err(|e| format!("capture client: {e}"))?;
    client.Start().map_err(|e| format!("start: {e}"))?;
    s.clear_error();
    log::info!("application audio: capturing (pid {pid}, {})", if mode == PROCESS_LOOPBACK_MODE_INCLUDE_TARGET_PROCESS_TREE { "only" } else { "excluding" });
    let tap = s.audio.as_ref().ok_or("no audio tap")?;
    let mut silence: Vec<f32> = Vec::new();
    let mut checked = Instant::now();
    let result = loop {
        if s.stopped() {
            break Ok(());
        }
        if watch_pid && checked.elapsed() > Duration::from_secs(2) {
            checked = Instant::now();
            if !process_alive(pid) {
                break Err("the application closed".to_string());
            }
        }
        if WaitForSingleObject(event.0, 100) != WAIT_OBJECT_0 {
            continue;
        }
        let mut failed = None;
        loop {
            match cap.GetNextPacketSize() {
                Ok(0) => break,
                Ok(_) => {}
                Err(e) => {
                    failed = Some(format!("capture: {e}"));
                    break;
                }
            }
            let mut data = std::ptr::null_mut();
            let mut frames = 0u32;
            let mut flags = 0u32;
            if let Err(e) = cap.GetBuffer(&mut data, &mut frames, &mut flags, None, None) {
                failed = Some(format!("capture: {e}"));
                break;
            }
            let n = frames as usize * CHANNELS;
            if flags & AUDCLNT_BUFFERFLAGS_SILENT.0 as u32 != 0 || data.is_null() {
                silence.resize(n, 0.0);
                tap.push(&silence, CHANNELS, SAMPLE_RATE);
            } else {
                tap.push(std::slice::from_raw_parts(data as *const f32, n), CHANNELS, SAMPLE_RATE);
            }
            let _ = cap.ReleaseBuffer(frames);
        }
        if let Some(e) = failed {
            break Err(e);
        }
    };
    let _ = client.Stop();
    result
}

#[cfg(test)]
mod tests {
    use super::*;
    use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};

    /// Plays a quiet tone from this process and captures it back through process loopback.
    /// `cargo test -- --ignored process_loopback --nocapture` (plays ~1.5 s of a soft beep).
    #[test]
    #[ignore]
    fn process_loopback_captures_own_tone() {
        let host = cpal::default_host();
        let Some(dev) = host.default_output_device() else {
            eprintln!("no playback device; skipped");
            return;
        };
        let cfg = dev.default_output_config().unwrap();
        let (ch, rate) = (cfg.channels() as usize, cfg.sample_rate() as f32);
        let mut t = 0f32;
        let stream = dev
            .build_output_stream::<f32, _, _>(
                cfg.config(),
                move |out: &mut [f32], _| {
                    for f in out.chunks_mut(ch) {
                        let v = (t * 440.0 * std::f32::consts::TAU).sin() * 0.02;
                        t += 1.0 / rate;
                        f.fill(v);
                    }
                },
                |e| eprintln!("{e}"),
                None,
            )
            .unwrap();
        stream.play().unwrap();
        let s = SourceShared::new(Some(Arc::new(crate::audio::AudioTap::live())), None);
        let s2 = Arc::clone(&s);
        let th = std::thread::spawn(move || unsafe {
            let _ = CoInitializeEx(None, COINIT_MULTITHREADED);
            capture(std::process::id(), PROCESS_LOOPBACK_MODE_INCLUDE_TARGET_PROCESS_TREE, &s2, false)
        });
        std::thread::sleep(Duration::from_millis(1500));
        s.stop.store(true, std::sync::atomic::Ordering::Relaxed);
        th.join().unwrap().expect("capture");
        drop(stream);
        let tap = s.audio.as_ref().unwrap();
        let frames = tap.buffered_frames();
        let mut buf = vec![0f32; frames * CHANNELS];
        tap.pull(&mut buf);
        let peak = crate::audio::peak(&buf);
        eprintln!("captured {frames} frames, peak {peak:.4}");
        assert!(frames > 4800, "too little audio: {frames}");
        assert!(peak > 0.005, "silent capture (peak {peak})");
    }
}
