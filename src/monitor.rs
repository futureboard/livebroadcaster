//! Audio monitoring: the mixer's monitor bus (sources set to "Monitor") played on a playback
//! device, so the streamer hears browser alerts and media they can't otherwise hear. The device
//! stream opens only while something is monitored and follows the chosen / default device.

use std::collections::VecDeque;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use parking_lot::Mutex;

use crate::audio::{BLOCK, CHANNELS, SAMPLE_RATE};

/// Playback starts once this much is queued (absorbs mixer / device timing jitter).
const PRIME: usize = BLOCK * 4;
/// Most audio kept: older audio is dropped so monitoring stays close to real time.
const MAX: usize = SAMPLE_RATE as usize / 5;

#[derive(Default)]
struct Queue {
    /// Interleaved stereo at 48 kHz.
    buf: VecDeque<f32>,
    primed: bool,
}

pub struct Monitor {
    queue: Mutex<Queue>,
    /// The mixer had a monitored source in its last block.
    wanted: AtomicBool,
    /// Device to use (`None`: default), as set from the settings.
    device: Mutex<Option<String>>,
    /// Last error opening / running the device, for the UI.
    pub error: Mutex<Option<String>>,
}

impl Monitor {
    pub fn new() -> Arc<Monitor> {
        Arc::new(Monitor { queue: Mutex::new(Queue::default()), wanted: AtomicBool::new(false), device: Mutex::new(None), error: Mutex::new(None) })
    }

    pub fn set_device(&self, device: Option<String>) {
        *self.device.lock() = device;
    }

    /// One mixer block of the monitor bus (or `None` when nothing is monitored).
    pub fn push(&self, block: Option<&[f32]>) {
        self.wanted.store(block.is_some(), Ordering::Relaxed);
        let Some(block) = block else { return };
        let mut q = self.queue.lock();
        q.buf.extend(block.iter().copied());
        let excess = q.buf.len().saturating_sub(MAX * CHANNELS);
        if excess > 0 {
            q.buf.drain(..excess);
        }
    }

    /// The device thread: opens the stream while monitoring is wanted, reopens it when the
    /// device changes or fails, until `quit`.
    pub fn run(self: Arc<Self>, quit: &AtomicBool) {
        let mut idle_since: Option<Instant> = None;
        while !quit.load(Ordering::Relaxed) {
            if !self.wanted.load(Ordering::Relaxed) {
                std::thread::sleep(Duration::from_millis(50));
                continue;
            }
            let device = self.device.lock().clone();
            let failed = Arc::new(AtomicBool::new(false));
            let stream = match self.open(&device, &failed) {
                Ok(s) => {
                    *self.error.lock() = None;
                    s
                }
                Err(e) => {
                    log::warn!("audio monitoring: {e}");
                    *self.error.lock() = Some(e);
                    sleep_unless(quit, Duration::from_secs(3));
                    continue;
                }
            };
            let default_at_start = default_id();
            let mut checked = Instant::now();
            loop {
                if quit.load(Ordering::Relaxed) || failed.load(Ordering::Relaxed) || *self.device.lock() != device {
                    break;
                }
                // Close the device a while after the last monitored source went away.
                if self.wanted.load(Ordering::Relaxed) {
                    idle_since = None;
                } else if idle_since.get_or_insert_with(Instant::now).elapsed() > Duration::from_secs(3) {
                    break;
                }
                if device.is_none() && checked.elapsed() > Duration::from_secs(2) {
                    checked = Instant::now();
                    if default_id() != default_at_start {
                        log::info!("audio monitoring: default device changed");
                        break;
                    }
                }
                std::thread::sleep(Duration::from_millis(50));
            }
            drop(stream);
            idle_since = None;
            let mut q = self.queue.lock();
            q.buf.clear();
            q.primed = false;
        }
    }

    fn open(self: &Arc<Self>, device_id: &Option<String>, failed: &Arc<AtomicBool>) -> Result<cpal::Stream, String> {
        let host = cpal::default_host();
        let device = match device_id {
            Some(id) => {
                let parsed: cpal::DeviceId = id.parse().map_err(|e| format!("device id {id}: {e}"))?;
                host.device_by_id(&parsed).ok_or_else(|| format!("monitoring device not found: {id}"))?
            }
            None => host.default_output_device().ok_or("no playback device")?,
        };
        let supported = device.default_output_config().map_err(|e| format!("playback device config: {e}"))?;
        let channels = supported.channels() as usize;
        let rate = supported.sample_rate();
        let config = cpal::StreamConfig { channels: supported.channels(), sample_rate: rate, buffer_size: cpal::BufferSize::Default };
        let f = Arc::clone(failed);
        let on_error = move |e: cpal::Error| {
            if !matches!(e.kind(), cpal::ErrorKind::Xrun) {
                log::warn!("audio monitoring: {e}");
                f.store(true, Ordering::Relaxed);
            }
        };
        let mut reader = Reader::new(rate);
        let me = Arc::clone(self);
        let mut scratch: Vec<f32> = Vec::new();
        macro_rules! build {
            ($t:ty, $conv:expr) => {{
                device.build_output_stream::<$t, _, _>(
                    config.clone(),
                    move |out: &mut [$t], _: &cpal::OutputCallbackInfo| {
                        scratch.resize(out.len(), 0.0);
                        reader.fill(&me, &mut scratch, channels);
                        for (o, s) in out.iter_mut().zip(&scratch) {
                            *o = $conv(*s);
                        }
                    },
                    on_error,
                    None,
                )
            }};
        }
        use cpal::SampleFormat as F;
        let stream = match supported.sample_format() {
            F::F32 => build!(f32, |s: f32| s),
            F::I16 => build!(i16, |s: f32| (s.clamp(-1.0, 1.0) * 32767.0) as i16),
            F::I32 => build!(i32, |s: f32| (s.clamp(-1.0, 1.0) as f64 * 2_147_483_647.0) as i32),
            F::U16 => build!(u16, |s: f32| ((s.clamp(-1.0, 1.0) + 1.0) * 32767.5) as u16),
            other => return Err(format!("unsupported playback format {other:?}")),
        }
        .map_err(|e| format!("open playback stream: {e}"))?;
        stream.play().map_err(|e| format!("start playback: {e}"))?;
        log::info!("audio monitoring: {} ch @ {rate} Hz", channels);
        Ok(stream)
    }
}

fn default_id() -> Option<String> {
    cpal::default_host().default_output_device().and_then(|d| d.id().ok()).map(|i| i.to_string())
}

fn sleep_unless(quit: &AtomicBool, d: Duration) {
    let end = Instant::now() + d;
    while Instant::now() < end && !quit.load(Ordering::Relaxed) {
        std::thread::sleep(Duration::from_millis(50));
    }
}

/// Reads the 48 kHz stereo queue at the device rate (linear interpolation) into any channel
/// layout (stereo to the first two channels; mono gets the average).
struct Reader {
    step: f64,
    frac: f64,
    a: [f32; 2],
    b: [f32; 2],
}

impl Reader {
    fn new(rate: u32) -> Reader {
        Reader { step: SAMPLE_RATE as f64 / rate.max(1) as f64, frac: 0.0, a: [0.0; 2], b: [0.0; 2] }
    }

    fn fill(&mut self, m: &Monitor, out: &mut [f32], channels: usize) {
        let channels = channels.max(1);
        let mut q = m.queue.lock();
        if !q.primed && q.buf.len() >= PRIME * CHANNELS {
            q.primed = true;
        }
        for f in out.chunks_mut(channels) {
            while self.frac >= 1.0 {
                self.a = self.b;
                self.b = if q.primed && q.buf.len() >= 2 {
                    [q.buf.pop_front().unwrap(), q.buf.pop_front().unwrap()]
                } else {
                    // Ran dry: silence, and wait for a little audio again.
                    q.primed = false;
                    [0.0; 2]
                };
                self.frac -= 1.0;
            }
            let t = self.frac as f32;
            let l = self.a[0] + (self.b[0] - self.a[0]) * t;
            let r = self.a[1] + (self.b[1] - self.a[1]) * t;
            self.frac += self.step;
            match f.len() {
                1 => f[0] = (l + r) * 0.5,
                _ => {
                    f[0] = l;
                    f[1] = r;
                    f[2..].fill(0.0);
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reader_resamples_and_primes() {
        let m = Monitor::new();
        // 30 ms of a ramp: not enough to start.
        let block: Vec<f32> = (0..BLOCK * 3).flat_map(|i| [i as f32 / 10_000.0; 2]).collect();
        m.push(Some(&block));
        let mut r = Reader::new(44_100);
        let mut out = vec![1.0f32; 441 * 2];
        r.fill(&m, &mut out, 2);
        assert!(out.iter().all(|v| *v == 0.0), "played before priming");
        m.push(Some(&block));
        let mut out = vec![0.0f32; 441 * 2];
        r.fill(&m, &mut out, 2);
        // 441 frames at 44.1 kHz consume ~480 input frames.
        let left = m.queue.lock().buf.len() / 2;
        assert!((BLOCK * 6 - left).abs_diff(480) <= 2, "consumed {}", BLOCK * 6 - left);
        assert!(out.windows(4).step_by(2).all(|w| w[2] >= w[0]), "ramp not monotonic");
    }
}
