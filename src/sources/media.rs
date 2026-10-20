//! Media source: plays any file MKFF (`mkff-codecs`) opens — MP4 / MOV, MKV / WebM, AVI, MXF,
//! TS / PS, Ogg, audio files, stills — with H.264, HEVC, VP8 / VP9, AV1, ProRes, DNx, MPEG-2,
//! AAC, Opus, AC-3, MP3, FLAC … decoded in software.
//!
//! A video thread picks the frame for the media clock; an audio thread keeps the source's tap
//! ~150 ms ahead of it, so the mixer plays the sound for the picture on screen.

use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use mkff_codecs::{MediaFile, MediaKind};
use parking_lot::Mutex;

use super::SourceShared;
use crate::audio::SAMPLE_RATE;
use crate::model::{MediaSettings, SourceKind};
use crate::video::frame_to_rgba;

/// Audio kept decoded ahead of the clock.
const AUDIO_LEAD: f64 = 0.15;
const AUDIO_CHUNK: usize = 2048;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MediaCommand {
    Play,
    Pause,
    Restart,
    Stop,
}

/// Transport controls and position, shared with the UI.
#[derive(Default)]
pub struct MediaControl {
    pending: Mutex<Vec<MediaCommand>>,
    seek_to: Mutex<Option<f64>>,
    /// Position and duration in ms.
    pub position_ms: AtomicU64,
    pub duration_ms: AtomicU64,
    pub paused: AtomicBool,
    pub ended: AtomicBool,
    pub info: Mutex<String>,
}

impl MediaControl {
    pub fn send(&self, c: MediaCommand) {
        self.pending.lock().push(c);
    }
    pub fn seek(&self, seconds: f64) {
        *self.seek_to.lock() = Some(seconds);
    }
}

/// Media time that advances with the wall clock while playing.
struct Clock {
    base: f64,
    since: Option<Instant>,
    speed: f64,
    /// Bumped on every jump (seek / loop / restart): the audio thread re-syncs.
    generation: u64,
}

impl Clock {
    fn now(&self) -> f64 {
        self.base + self.since.map_or(0.0, |s| s.elapsed().as_secs_f64() * self.speed)
    }
    fn set(&mut self, t: f64) {
        self.base = t;
        if self.since.is_some() {
            self.since = Some(Instant::now());
        }
        self.generation += 1;
    }
    fn play(&mut self) {
        if self.since.is_none() {
            self.since = Some(Instant::now());
        }
    }
    fn pause(&mut self) {
        self.base = self.now();
        self.since = None;
    }
}

pub fn run(kind: SourceKind, s: Arc<SourceShared>) {
    let SourceKind::Media(m) = kind else { return };
    if m.path.is_empty() {
        s.set_error("no media file selected");
        return;
    }
    let clip = match MediaFile::open(&m.path) {
        Ok(c) => c,
        Err(e) => {
            s.set_error(format!("{}: {e}", m.path));
            return;
        }
    };
    let ctl = s.media.as_ref().expect("media control");
    let info = clip.info().clone();
    *ctl.info.lock() = describe(&info);
    let duration = info.duration_seconds.max(0.0);
    ctl.duration_ms.store((duration * 1000.0) as u64, Ordering::Relaxed);

    let clock = Arc::new(Mutex::new(Clock {
        base: 0.0,
        since: None,
        speed: m.speed_percent.clamp(10, 400) as f64 / 100.0,
        generation: 0,
    }));
    let has_audio = m.audio && !info.audio.is_empty() && s.audio.is_some();
    let audio_thread = has_audio.then(|| {
        let (clip, clock, s) = (clip.clone(), Arc::clone(&clock), Arc::clone(&s));
        std::thread::Builder::new()
            .name("media-audio".into())
            .spawn(move || audio_loop(clip, clock, s, duration))
            .ok()
    });

    video_loop(&clip, &m, &clock, &s, duration);
    if let Some(Some(t)) = audio_thread {
        let _ = t.join();
    }
}

fn describe(i: &mkff_codecs::MediaInfo) -> String {
    let mut parts = vec![i.container.clone()];
    if let Some(v) = &i.video {
        let fps = v.frame_rate.0 as f64 / v.frame_rate.1.max(1) as f64;
        parts.push(format!("{} {}x{} {:.2} fps", v.codec, v.width, v.height, fps));
    }
    for a in &i.audio {
        parts.push(format!("{} {} Hz {} ch", a.codec, a.sample_rate, a.channels));
    }
    parts.join(" · ")
}

fn video_loop(clip: &MediaFile, m: &MediaSettings, clock: &Mutex<Clock>, s: &SourceShared, duration: f64) {
    let ctl = s.media.as_ref().unwrap();
    let info = clip.info();
    let fps = info
        .video
        .as_ref()
        .map(|v| v.frame_rate.0 as f64 / v.frame_rate.1.max(1) as f64)
        .filter(|f| f.is_finite() && *f > 0.0)
        .unwrap_or(30.0);
    let frame_count = clip.frame_count();
    let still = matches!(info.kind, MediaKind::Still) || frame_count <= 1;
    let has_video = info.video.is_some();
    let mut last_index: Option<u64> = None;
    let mut was_active = false;
    let mut user_paused = false;
    let mut ended = false;

    while !s.stopped() {
        // Activation: OBS-style restart when the source goes on air; pause while off air.
        let active = s.is_active();
        if active != was_active {
            let mut c = clock.lock();
            if active {
                if m.restart_on_activate || ended {
                    c.set(0.0);
                    ended = false;
                    last_index = None;
                }
                if !user_paused {
                    c.play();
                }
            } else {
                c.pause();
            }
            was_active = active;
        }
        for cmd in std::mem::take(&mut *ctl.pending.lock()) {
            let mut c = clock.lock();
            match cmd {
                MediaCommand::Play => {
                    user_paused = false;
                    if ended {
                        c.set(0.0);
                        ended = false;
                        last_index = None;
                    }
                    if active {
                        c.play();
                    }
                }
                MediaCommand::Pause => {
                    user_paused = true;
                    c.pause();
                }
                MediaCommand::Restart => {
                    user_paused = false;
                    ended = false;
                    c.set(0.0);
                    last_index = None;
                    if active {
                        c.play();
                    }
                }
                MediaCommand::Stop => {
                    user_paused = true;
                    c.pause();
                    c.set(0.0);
                    last_index = None;
                }
            }
        }
        if let Some(t) = ctl.seek_to.lock().take() {
            let mut c = clock.lock();
            c.set(t.clamp(0.0, duration));
            ended = false;
            last_index = None;
        }

        // End of file: loop or stop.
        let mut t = clock.lock().now();
        if duration > 0.0 && t >= duration && !still {
            if m.looping {
                let mut c = clock.lock();
                c.set(0.0);
                t = 0.0;
                last_index = None;
            } else if !ended {
                ended = true;
                clock.lock().pause();
                if !m.hold_last_frame {
                    s.video.clear();
                }
            }
        }
        ctl.position_ms.store((t.min(duration) * 1000.0) as u64, Ordering::Relaxed);
        ctl.paused.store(user_paused, Ordering::Relaxed);
        ctl.ended.store(ended, Ordering::Relaxed);

        if has_video && !(ended && !m.hold_last_frame) {
            let index = if still { 0 } else { ((t * fps).floor() as u64).min(frame_count.saturating_sub(1)) };
            if last_index != Some(index) {
                let started = Instant::now();
                match clip.video_frame(index) {
                    Ok(f) => {
                        s.video.set(frame_to_rgba(&f));
                        s.clear_error();
                    }
                    Err(e) => s.set_error(format!("decode frame {index}: {e}")),
                }
                last_index = Some(index);
                log::trace!("media frame {index} in {:?}", started.elapsed());
            }
        }
        // Wake for the next frame boundary (or soon, for controls).
        let speed = clock.lock().speed;
        let next = if has_video && !still {
            let next_t = ((t * fps).floor() + 1.0) / fps;
            ((next_t - t) / speed).clamp(0.001, 0.05)
        } else {
            0.02
        };
        std::thread::sleep(Duration::from_secs_f64(next));
    }
}

fn audio_loop(clip: MediaFile, clock: Arc<Mutex<Clock>>, s: Arc<SourceShared>, duration: f64) {
    let tap = s.audio.as_ref().unwrap();
    let mut generation = u64::MAX;
    let mut cursor: i64 = 0;
    let total = (duration * SAMPLE_RATE as f64) as i64;
    while !s.stopped() {
        let (now, gen, playing, speed) = {
            let c = clock.lock();
            (c.now(), c.generation, c.since.is_some(), c.speed)
        };
        if gen != generation {
            generation = gen;
            tap.clear();
            cursor = (now * SAMPLE_RATE as f64) as i64;
        }
        if !playing || !s.is_active() {
            std::thread::sleep(Duration::from_millis(10));
            continue;
        }
        let target = ((now + AUDIO_LEAD * speed) * SAMPLE_RATE as f64) as i64;
        let mut produced = false;
        while cursor < target && (total == 0 || cursor < total) && !s.stopped() {
            // `cursor` counts media time in 48 kHz samples. Reading at 48 kHz / speed and playing
            // at 48 kHz gives the requested speed (pitch follows, as in MKFF Player).
            let read_rate = ((SAMPLE_RATE as f64 / speed).round() as u32).max(1000);
            let frames = AUDIO_CHUNK;
            let start = (cursor as f64 / SAMPLE_RATE as f64 * read_rate as f64) as i64;
            match clip.audio(start, frames, read_rate) {
                Ok(buf) => {
                    let inter = buf.interleaved();
                    let ch = buf.channel_count().max(1);
                    tap.push(&inter, ch, SAMPLE_RATE);
                }
                Err(e) => {
                    s.set_error(format!("audio: {e}"));
                    std::thread::sleep(Duration::from_millis(200));
                    break;
                }
            }
            cursor += (frames as f64 * speed) as i64;
            produced = true;
        }
        if !produced {
            std::thread::sleep(Duration::from_millis(10));
        }
    }
}
