//! Audio plumbing: per-source taps (48 kHz stereo FIFOs fed by capture callbacks and media
//! decoders) and the mixer that sums them in real time.

use std::collections::VecDeque;
use std::sync::atomic::{AtomicU32, Ordering};

use parking_lot::Mutex;

/// The studio's audio format: 48 kHz, stereo, f32.
pub const SAMPLE_RATE: u32 = 48_000;
pub const CHANNELS: usize = 2;
/// Mixer block: 10 ms.
pub const BLOCK: usize = 480;

/// A peak level shared with the UI (linear amplitude stored as f32 bits).
#[derive(Default)]
pub struct Level(AtomicU32);

impl Level {
    pub fn set(&self, v: f32) {
        self.0.store(v.to_bits(), Ordering::Relaxed);
    }
    pub fn get(&self) -> f32 {
        f32::from_bits(self.0.load(Ordering::Relaxed))
    }
}

/// Linear-interpolating resampler to 48 kHz stereo.
struct Resampler {
    rate: u32,
    /// Position of the next output frame, in input frames, relative to `prev`.
    pos: f64,
    prev: [f32; 2],
}

impl Resampler {
    fn new() -> Self {
        Resampler { rate: SAMPLE_RATE, pos: 0.0, prev: [0.0; 2] }
    }

    /// Push interleaved `input` with `channels` channels at `rate` Hz; appends stereo to `out`.
    fn process(&mut self, input: &[f32], channels: usize, rate: u32, out: &mut VecDeque<f32>) {
        if channels == 0 || rate == 0 {
            return;
        }
        let frames = input.len() / channels;
        let stereo = |i: usize| -> [f32; 2] {
            let f = &input[i * channels..i * channels + channels];
            match channels {
                1 => [f[0], f[0]],
                2 => [f[0], f[1]],
                // Surround (L R C LFE Ls Rs …): fold centre and surrounds in (ITU-R BS.775).
                _ => {
                    let c = f.get(2).copied().unwrap_or(0.0) * 0.707;
                    let ls = f.get(4).copied().unwrap_or(0.0) * 0.707;
                    let rs = f.get(5).copied().unwrap_or(0.0) * 0.707;
                    [f[0] + c + ls, f[1] + c + rs]
                }
            }
        };
        if rate == SAMPLE_RATE {
            for i in 0..frames {
                let s = stereo(i);
                out.push_back(s[0]);
                out.push_back(s[1]);
            }
            if frames > 0 {
                self.prev = stereo(frames - 1);
            }
            return;
        }
        if self.rate != rate {
            self.rate = rate;
            self.pos = 0.0;
        }
        let step = rate as f64 / SAMPLE_RATE as f64;
        // Input frame k (k = -1 is `prev`); output frames between k and k+1.
        while self.pos < frames as f64 - 1.0 + 1.0 {
            let k = self.pos.floor();
            let t = (self.pos - k) as f32;
            let a = if k < 0.0 { self.prev } else { stereo(k as usize) };
            let bi = k + 1.0;
            if bi >= frames as f64 {
                break;
            }
            let b = stereo(bi as usize);
            out.push_back(a[0] + (b[0] - a[0]) * t);
            out.push_back(a[1] + (b[1] - a[1]) * t);
            self.pos += step;
        }
        if frames > 0 {
            self.prev = stereo(frames - 1);
            self.pos -= frames as f64;
        }
    }
}

struct TapInner {
    /// Interleaved stereo at 48 kHz.
    queue: VecDeque<f32>,
    resampler: Resampler,
    /// Consumption starts once a little audio is buffered, so callback jitter doesn't click.
    primed: bool,
    /// Frames to buffer before (re)starting: grows when audio arrives too unevenly (each
    /// mid-sound underrun would otherwise be an audible gap), shrinks back once it's steady.
    prime: usize,
    /// When the last underrun happened (while sound was coming in).
    last_underrun: Option<std::time::Instant>,
    /// When audio last arrived.
    last_push: Option<std::time::Instant>,
}

/// The adaptive buffer's limits: grows by a block per underrun up to this.
const MAX_PRIME: usize = BLOCK * 10;
/// Steady this long: the buffer shrinks by a block.
const SHRINK_AFTER: std::time::Duration = std::time::Duration::from_secs(10);

/// Where one source's sound waits for the mixer.
pub struct AudioTap {
    inner: Mutex<TapInner>,
    /// Most audio kept (frames); older audio is dropped so latency stays bounded.
    max_frames: usize,
    prime_frames: usize,
    /// Post-fader peak of the last mixer block, for the meters.
    pub level: Level,
}

impl AudioTap {
    /// A tap for a live device (low latency).
    pub fn live() -> Self {
        Self::with_limits(SAMPLE_RATE as usize / 4, BLOCK * 3)
    }

    /// A tap for a decoder that runs ahead of real time.
    pub fn buffered() -> Self {
        Self::with_limits(SAMPLE_RATE as usize, BLOCK * 2)
    }

    fn with_limits(max_frames: usize, prime_frames: usize) -> Self {
        AudioTap {
            inner: Mutex::new(TapInner {
                queue: VecDeque::new(),
                resampler: Resampler::new(),
                primed: false,
                prime: prime_frames,
                last_underrun: None,
                last_push: None,
            }),
            max_frames,
            prime_frames,
            level: Level::default(),
        }
    }

    /// Push interleaved samples (any channel count / rate).
    pub fn push(&self, input: &[f32], channels: usize, rate: u32) {
        let mut g = self.inner.lock();
        g.last_push = Some(std::time::Instant::now());
        let TapInner { queue, resampler, .. } = &mut *g;
        resampler.process(input, channels, rate, queue);
        let excess = queue.len().saturating_sub(self.max_frames * CHANNELS);
        if excess > 0 {
            queue.drain(..excess);
        }
    }

    /// Frames waiting.
    #[cfg(test)]
    pub fn buffered_frames(&self) -> usize {
        self.inner.lock().queue.len() / CHANNELS
    }

    pub fn clear(&self) {
        let mut g = self.inner.lock();
        g.queue.clear();
        g.primed = false;
    }

    /// Frames buffered before playback (re)starts (grows with uneven delivery).
    #[cfg(test)]
    pub fn prime_frames(&self) -> usize {
        self.inner.lock().prime
    }

    /// Fill `out` (interleaved stereo) from the queue; silence where nothing is ready.
    pub fn pull(&self, out: &mut [f32]) {
        let mut g = self.inner.lock();
        let now = std::time::Instant::now();
        if !g.primed {
            // Still waiting; but a stream that stopped (nothing left to come) plays out what's
            // there rather than holding its last few milliseconds back.
            let stalled = g.last_push.is_none_or(|t| now - t > std::time::Duration::from_millis(100));
            if g.queue.len() >= g.prime * CHANNELS || (stalled && !g.queue.is_empty()) {
                g.primed = true;
            } else {
                out.fill(0.0);
                return;
            }
        }
        let n = out.len().min(g.queue.len());
        for (o, s) in out.iter_mut().zip(g.queue.drain(..n)) {
            *o = s;
        }
        if n < out.len() {
            out[n..].fill(0.0);
            g.primed = false;
            // Ran dry while sound was still arriving: delivery is jittery, buffer more.
            let arriving = g.last_push.is_some_and(|t| now - t < std::time::Duration::from_millis(50));
            if arriving {
                g.prime = (g.prime + BLOCK).min(MAX_PRIME.max(self.prime_frames));
                g.last_underrun = Some(now);
            }
        } else if g.prime > self.prime_frames && g.last_underrun.is_none_or(|t| now - t > SHRINK_AFTER) {
            g.prime -= BLOCK.min(g.prime - self.prime_frames);
            g.last_underrun = Some(now);
        }
    }
}

/// UI fader position (0..1) ↔ gain, OBS-style: a dB scale from −96 dB to 0 dB, cubic so most
/// of the travel covers the useful range.
pub fn fader_to_gain(pos: f32) -> f32 {
    if pos <= 0.0 {
        return 0.0;
    }
    let db = -96.0 * (1.0 - pos).powi(3);
    10f32.powf(db / 20.0)
}

pub fn gain_to_fader(gain: f32) -> f32 {
    if gain <= 0.0 {
        return 0.0;
    }
    let db = (20.0 * gain.log10()).clamp(-96.0, 0.0);
    1.0 - (-db / 96.0).cbrt()
}

pub fn gain_to_db(gain: f32) -> f32 {
    if gain <= 0.0 {
        f32::NEG_INFINITY
    } else {
        20.0 * gain.log10()
    }
}

/// Peak of an interleaved block.
pub fn peak(block: &[f32]) -> f32 {
    block.iter().fold(0.0f32, |m, s| m.max(s.abs()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resample_keeps_length() {
        let tap = AudioTap::live();
        let input = vec![0.5f32; 441 * 2];
        for _ in 0..10 {
            tap.push(&input, 2, 44_100);
        }
        let n = tap.buffered_frames();
        assert!((4790..=4810).contains(&n), "{n}");
    }

    #[test]
    fn fader_roundtrip() {
        for g in [1.0f32, 0.5, 0.1, 0.01] {
            assert!((fader_to_gain(gain_to_fader(g)) - g).abs() < 1e-3);
        }
    }

    #[test]
    fn jitter_grows_the_buffer() {
        let tap = AudioTap::live();
        let base = tap.prime_frames();
        let mut out = vec![0.0; BLOCK * 2];
        // Deliveries that run dry while audio keeps coming.
        for _ in 0..3 {
            tap.push(&vec![0.5; tap.prime_frames() * 2], 2, SAMPLE_RATE);
            while tap.buffered_frames() > 0 {
                tap.pull(&mut out);
            }
            tap.pull(&mut out);
        }
        assert!(tap.prime_frames() > base, "buffer didn't grow: {}", tap.prime_frames());
        assert!(tap.prime_frames() <= MAX_PRIME);
    }

    #[test]
    fn tap_primes_and_underruns() {
        let tap = AudioTap::live();
        let mut out = vec![1.0; BLOCK * 2];
        tap.push(&vec![0.25; BLOCK * 2], 2, SAMPLE_RATE);
        tap.pull(&mut out);
        assert_eq!(out[0], 0.0, "not primed yet");
        tap.push(&vec![0.25; BLOCK * 2 * 3], 2, SAMPLE_RATE);
        tap.pull(&mut out);
        assert_eq!(out[0], 0.25);
    }
}
