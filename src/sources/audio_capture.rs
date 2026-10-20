//! Audio input capture (microphones, line in) and audio output capture (WASAPI loopback of a
//! playback device) through cpal.

use std::sync::Arc;
use std::time::Duration;

use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};

use super::SourceShared;
use crate::model::SourceKind;

/// (id, name) of every capture device.
pub fn list_input_devices() -> Vec<(String, String)> {
    list(true)
}

/// (id, name) of every playback device (for loopback capture).
pub fn list_output_devices() -> Vec<(String, String)> {
    list(false)
}

fn list(input: bool) -> Vec<(String, String)> {
    let host = cpal::default_host();
    let devices = if input { host.input_devices().map(|d| d.collect::<Vec<_>>()) } else { host.output_devices().map(|d| d.collect::<Vec<_>>()) };
    devices
        .unwrap_or_default()
        .into_iter()
        .filter_map(|d| {
            let id = d.id().ok()?.to_string();
            let name = d.description().map(|x| x.name().to_string()).unwrap_or_else(|_| id.clone());
            Some((id, name))
        })
        .collect()
}

pub fn run(kind: SourceKind, s: Arc<SourceShared>) {
    let (device_id, loopback) = match &kind {
        SourceKind::AudioInput { device } => (device.clone(), false),
        SourceKind::AudioOutput { device } => (device.clone(), true),
        _ => return,
    };
    // Re-open on failure (device unplugged, default changed …) until the source is removed.
    while !s.stopped() {
        match open(&device_id, loopback, &s) {
            Ok(stream) => {
                s.clear_error();
                let started_default = device_id.is_none().then(|| default_name(loopback));
                // Keep the stream alive; follow the system default device when it changes.
                loop {
                    if s.sleep(Duration::from_secs(2)) {
                        drop(stream);
                        return;
                    }
                    if let Some(name) = &started_default {
                        if default_name(loopback) != *name {
                            log::info!("default audio device changed; reopening");
                            break;
                        }
                    }
                    if s.error().is_some() {
                        break;
                    }
                }
                drop(stream);
            }
            Err(e) => {
                s.set_error(e);
                if s.sleep(Duration::from_secs(3)) {
                    return;
                }
            }
        }
    }
}

fn default_name(loopback: bool) -> Option<String> {
    let host = cpal::default_host();
    let d = if loopback { host.default_output_device() } else { host.default_input_device() }?;
    d.id().ok().map(|i| i.to_string())
}

fn open(device_id: &Option<String>, loopback: bool, s: &Arc<SourceShared>) -> Result<cpal::Stream, String> {
    let host = cpal::default_host();
    let device = match device_id {
        Some(id) => {
            let parsed: cpal::DeviceId = id.parse().map_err(|e| format!("device id {id}: {e}"))?;
            host.device_by_id(&parsed).ok_or_else(|| format!("audio device not found: {id}"))?
        }
        None => if loopback { host.default_output_device() } else { host.default_input_device() }
            .ok_or_else(|| "no default audio device".to_string())?,
    };
    let supported = if loopback { device.default_output_config() } else { device.default_input_config() }
        .map_err(|e| format!("audio device config: {e}"))?;
    let channels = supported.channels() as usize;
    let rate = supported.sample_rate();
    let config = cpal::StreamConfig { channels: supported.channels(), sample_rate: rate, buffer_size: cpal::BufferSize::Default };
    let tap = Arc::clone(s.audio.as_ref().ok_or("no audio tap")?);
    let err_s = Arc::clone(s);
    let on_error = move |e: cpal::Error| {
        // Xruns are routine (loopback with nothing playing); only a lost device needs a reopen.
        if matches!(e.kind(), cpal::ErrorKind::Xrun) {
            log::debug!("audio stream: {e}");
        } else {
            err_s.set_error(format!("audio stream: {e}"));
        }
    };
    let mut scratch: Vec<f32> = Vec::new();

    macro_rules! build {
        ($t:ty, $conv:expr) => {{
            let tap = Arc::clone(&tap);
            device.build_input_stream::<$t, _, _>(
                config.clone(),
                move |data: &[$t], _: &cpal::InputCallbackInfo| {
                    scratch.clear();
                    scratch.extend(data.iter().map($conv));
                    tap.push(&scratch, channels, rate);
                },
                on_error,
                Some(Duration::from_secs(3)),
            )
        }};
    }
    use cpal::SampleFormat as F;
    let stream = match supported.sample_format() {
        F::F32 => {
            let tap = Arc::clone(&tap);
            device.build_input_stream::<f32, _, _>(
                config.clone(),
                move |data: &[f32], _: &cpal::InputCallbackInfo| tap.push(data, channels, rate),
                on_error,
                Some(Duration::from_secs(3)),
            )
        }
        F::I16 => build!(i16, |v: &i16| *v as f32 / 32768.0),
        F::U16 => build!(u16, |v: &u16| (*v as f32 - 32768.0) / 32768.0),
        F::I32 => build!(i32, |v: &i32| *v as f32 / 2_147_483_648.0),
        F::F64 => build!(f64, |v: &f64| *v as f32),
        other => return Err(format!("unsupported sample format {other:?}")),
    }
    .map_err(|e| format!("open audio stream: {e}"))?;
    stream.play().map_err(|e| format!("start audio stream: {e}"))?;
    log::info!("audio capture: {} ch @ {} Hz ({})", channels, rate, if loopback { "loopback" } else { "input" });
    Ok(stream)
}
