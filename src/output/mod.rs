//! Outputs: one encoder thread (MKFF H.264 + AAC) shared by the recording (MKFF MP4 / MOV
//! writer) and the stream (RTMP / RTMPS), like OBS's shared encoders.

pub mod amf;
pub mod rtmp;
mod video_enc;

use std::collections::VecDeque;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::thread::JoinHandle;
use std::time::Instant;

use crossbeam_channel::{Receiver, Sender};
use filmcraft_isobmff::CodecConfig;
use mkff_codecs::encode::{
    AudioEncoder, Container, H264Preset, H264Profile, H264Rate, H264Settings, MediaWriter, Packet, TrackId, VideoEncodeCodec, VideoEncoderConfig,
};
use video_enc::VideoEnc;
use parking_lot::Mutex;

use crate::audio::{Level, CHANNELS, SAMPLE_RATE};
use crate::model::{EncoderChoice, EncoderPreset, EncoderProfile, RecordFormat, Settings};
use crate::video::SharedImage;
use rtmp::{StreamInfo, StreamPacket, StreamStatus, Streamer};

/// A composited picture for the encoder: RGBA from the CPU compositor, or NV12 the GPU
/// compositor converted and read back.
pub enum VideoFrame {
    Rgba(SharedImage),
    Nv12(Arc<crate::gpu_compositor::Nv12Frame>),
}

impl VideoFrame {
    pub fn size(&self) -> (u32, u32) {
        match self {
            VideoFrame::Rgba(i) => (i.width, i.height),
            VideoFrame::Nv12(f) => (f.width, f.height),
        }
    }
}

/// A composited frame for the encoder: (compositor tick, picture).
pub type VideoMsg = (u64, VideoFrame);
/// A mixed audio block: (first sample index since the engine epoch, interleaved stereo).
pub type AudioMsg = (u64, Vec<f32>);

enum Ctl {
    StartRecording { path: PathBuf, format: RecordFormat },
    StopRecording,
    StartStream { url: String, key: String },
    StopStream,
}

pub struct RecordingInfo {
    pub path: PathBuf,
    pub since: Instant,
}

/// What the UI shows about the outputs.
#[derive(Default)]
pub struct OutputState {
    pub recording: Mutex<Option<RecordingInfo>>,
    pub stream: Mutex<Option<Arc<StreamStatus>>>,
    pub encoded_frames: AtomicU64,
    /// Average H.264 encode time per frame (ms).
    pub encode_ms: Level,
    pub last_error: Mutex<Option<String>>,
    pub encoder_desc: Mutex<String>,
}

/// Channels the compositor and mixer feed while an encoder runs.
#[derive(Default)]
pub struct OutputTaps {
    pub video: Mutex<Option<Sender<VideoMsg>>>,
    pub audio: Mutex<Option<Sender<AudioMsg>>>,
    /// Frames the compositor produced while the encoder was still busy.
    pub skipped_frames: AtomicU64,
}

pub struct OutputManager {
    ctl: Option<Sender<Ctl>>,
    thread: Option<JoinHandle<()>>,
    pub state: Arc<OutputState>,
    taps: Arc<OutputTaps>,
}

impl OutputManager {
    pub fn new(taps: Arc<OutputTaps>) -> Self {
        OutputManager { ctl: None, thread: None, state: Arc::new(OutputState::default()), taps }
    }

    pub fn is_recording(&self) -> bool {
        self.state.recording.lock().is_some()
    }

    pub fn is_streaming(&self) -> bool {
        self.state.stream.lock().is_some()
    }

    /// Any output running (video settings are locked meanwhile).
    pub fn active(&self) -> bool {
        self.thread.as_ref().is_some_and(|t| !t.is_finished())
    }

    /// Reap a finished encoder thread.
    pub fn poll(&mut self) {
        if self.thread.as_ref().is_some_and(|t| t.is_finished()) {
            if let Some(t) = self.thread.take() {
                let _ = t.join();
            }
            self.ctl = None;
            *self.taps.video.lock() = None;
            *self.taps.audio.lock() = None;
            *self.state.recording.lock() = None;
            *self.state.stream.lock() = None;
        }
    }

    fn ensure(&mut self, settings: &Settings, for_stream: bool) -> Result<&Sender<Ctl>, String> {
        self.poll();
        if self.ctl.is_none() {
            let cfg = EncoderConfig::from_settings(settings, for_stream);
            let (ctl_tx, ctl_rx) = crossbeam_channel::unbounded();
            // Two frames of slack: beyond that the compositor skips frames rather than queueing.
            let (vtx, vrx) = crossbeam_channel::bounded(2);
            let (atx, arx) = crossbeam_channel::unbounded();
            self.state.encoded_frames.store(0, Ordering::Relaxed);
            self.taps.skipped_frames.store(0, Ordering::Relaxed);
            *self.state.last_error.lock() = None;
            let state = Arc::clone(&self.state);
            // Hardware encoders are COM objects: create them on the thread that uses them, and
            // wait here for the outcome so a failure reaches the caller.
            let (ready_tx, ready_rx) = crossbeam_channel::bounded::<Result<String, String>>(1);
            let thread = std::thread::Builder::new()
                .name("encoder".into())
                .spawn(move || {
                    let opened = VideoEnc::open(cfg.choice, &cfg.video, &cfg.hw).and_then(|(v, desc)| {
                        let a = AudioEncoder::aac(SAMPLE_RATE, CHANNELS, cfg.audio_kbps * 1000).map_err(|e| format!("audio encoder: {e}"))?;
                        Ok((v, a, desc))
                    });
                    match opened {
                        Ok((venc, aenc, desc)) => {
                            let _ = ready_tx.send(Ok(format!("{desc} · {}", cfg.describe())));
                            let mut e = Encoder::new(cfg, venc, aenc, state);
                            e.run(ctl_rx, vrx, arx);
                        }
                        Err(e) => {
                            let _ = ready_tx.send(Err(e));
                        }
                    }
                })
                .map_err(|e| e.to_string())?;
            match ready_rx.recv_timeout(std::time::Duration::from_secs(15)) {
                Ok(Ok(desc)) => {
                    *self.state.encoder_desc.lock() = desc;
                    // Feed frames only once the encoder is ready (opening a GPU encoder takes a
                    // few hundred ms; frames sent meanwhile would count as skipped).
                    *self.taps.video.lock() = Some(vtx);
                    *self.taps.audio.lock() = Some(atx);
                    self.taps.skipped_frames.store(0, Ordering::Relaxed);
                }
                Ok(Err(e)) => {
                    let _ = thread.join();
                    *self.taps.video.lock() = None;
                    *self.taps.audio.lock() = None;
                    return Err(e);
                }
                Err(_) => {
                    *self.taps.video.lock() = None;
                    *self.taps.audio.lock() = None;
                    return Err("the video encoder did not start".into());
                }
            }
            self.ctl = Some(ctl_tx);
            self.thread = Some(thread);
        }
        Ok(self.ctl.as_ref().unwrap())
    }

    pub fn start_recording(&mut self, settings: &Settings) -> Result<PathBuf, String> {
        let dir = PathBuf::from(&settings.output.record_path);
        std::fs::create_dir_all(&dir).map_err(|e| format!("{}: {e}", dir.display()))?;
        let name = chrono::Local::now().format("%Y-%m-%d %H-%M-%S").to_string();
        let path = dir.join(format!("{name}.{}", settings.output.record_format.extension()));
        let format = settings.output.record_format;
        let ctl = self.ensure(settings, false)?;
        ctl.send(Ctl::StartRecording { path: path.clone(), format }).map_err(|e| e.to_string())?;
        Ok(path)
    }

    pub fn stop_recording(&mut self) {
        if let Some(c) = &self.ctl {
            let _ = c.send(Ctl::StopRecording);
        }
    }

    pub fn start_stream(&mut self, settings: &Settings) -> Result<(), String> {
        let url = settings.stream.server_url();
        rtmp::parse_url(&url)?;
        if settings.stream.key.trim().is_empty() {
            return Err("Enter a stream key in Settings → Stream.".into());
        }
        let key = settings.stream.key.trim().to_string();
        let ctl = self.ensure(settings, true)?;
        ctl.send(Ctl::StartStream { url, key }).map_err(|e| e.to_string())
    }

    pub fn stop_stream(&mut self) {
        if let Some(c) = &self.ctl {
            let _ = c.send(Ctl::StopStream);
        }
    }

    pub fn stop_all(&mut self) {
        self.stop_stream();
        self.stop_recording();
        if let Some(t) = self.thread.take() {
            let deadline = Instant::now() + std::time::Duration::from_secs(5);
            while !t.is_finished() && Instant::now() < deadline {
                std::thread::sleep(std::time::Duration::from_millis(20));
            }
            if t.is_finished() {
                let _ = t.join();
            }
        }
        self.ctl = None;
    }
}

#[derive(Clone)]
struct EncoderConfig {
    choice: EncoderChoice,
    video: VideoEncoderConfig,
    hw: mkff_hwenc::HwEncoderConfig,
    fps: u32,
    video_kbps: u32,
    audio_kbps: u32,
}

impl EncoderConfig {
    fn from_settings(s: &Settings, for_stream: bool) -> Self {
        let o = &s.output;
        let fps = s.video.fps.max(1);
        let cbr = for_stream || o.record_same_as_stream;
        let rate = if cbr { H264Rate::Cbr { kbps: o.video_kbps.max(200) } } else { H264Rate::Crf(o.record_crf) };
        let (w, h) = (s.video.width & !1, s.video.height & !1);
        let hw = mkff_hwenc::HwEncoderConfig {
            width: w,
            height: h,
            fps_num: fps,
            fps_den: 1,
            profile: match o.profile {
                EncoderProfile::Baseline => mkff_hwenc::Profile::Baseline,
                EncoderProfile::Main => mkff_hwenc::Profile::Main,
                EncoderProfile::High => mkff_hwenc::Profile::High,
            },
            rate: if cbr {
                mkff_hwenc::RateControl::Cbr { kbps: o.video_kbps.max(200) }
            } else {
                // CRF 14 (near lossless) … 35 (small) → quality 95 … 25.
                mkff_hwenc::RateControl::Quality((95.0 - (o.record_crf - 14.0) * 3.3).clamp(10.0, 100.0) as u8)
            },
            keyint: fps * o.keyint_secs.clamp(1, 10),
            quality_vs_speed: match o.preset {
                EncoderPreset::Speed => 20,
                EncoderPreset::Balanced => 50,
                EncoderPreset::Quality => 85,
            },
            low_latency: true,
        };
        let h264 = H264Settings {
            profile: match o.profile {
                EncoderProfile::Baseline => H264Profile::Baseline,
                EncoderProfile::Main => H264Profile::Main,
                EncoderProfile::High => H264Profile::High,
            },
            preset: match o.preset {
                EncoderPreset::Speed => H264Preset::Speed,
                EncoderPreset::Balanced => H264Preset::Balanced,
                EncoderPreset::Quality => H264Preset::Quality,
            },
            rate,
            keyint: Some(fps * o.keyint_secs.clamp(1, 10)),
            bframes: 0,
            threads: 0,
        };
        EncoderConfig {
            choice: o.encoder,
            hw,
            video: VideoEncoderConfig {
                width: w,
                height: h,
                fps_num: fps,
                fps_den: 1,
                codec: VideoEncodeCodec::H264(h264),
            },
            fps,
            video_kbps: o.video_kbps,
            audio_kbps: o.audio_kbps.clamp(32, 512),
        }
    }

    fn describe(&self) -> String {
        let rate = match &self.video.codec {
            VideoEncodeCodec::H264(h) => match h.rate {
                H264Rate::Cbr { kbps } => format!("CBR {kbps} kb/s"),
                H264Rate::Crf(q) => format!("CRF {q}"),
                H264Rate::Vbr { target_kbps, .. } => format!("VBR {target_kbps} kb/s"),
            },
            _ => String::new(),
        };
        format!("H.264 {}x{}@{} {rate} · AAC {} kb/s", self.video.width, self.video.height, self.fps, self.audio_kbps)
    }
}

struct Recorder {
    writer: MediaWriter,
    v: TrackId,
    a: TrackId,
    /// Waiting for a keyframe to start on.
    started: bool,
    v_origin: i64,
    a_origin: i64,
    /// The previous video packet, written once the next one tells its true duration (frames the
    /// compositor skipped stretch the previous picture, keeping A/V in sync).
    held: Option<Packet>,
    /// Audio encoded before the first keyframe arrived (written from the keyframe's time on).
    early_audio: VecDeque<Packet>,
}

struct Stream {
    streamer: Streamer,
    started: bool,
    v_origin_ms: i64,
    early_audio: VecDeque<Packet>,
}

struct Encoder {
    cfg: EncoderConfig,
    venc: VideoEnc,
    aenc: AudioEncoder,
    /// Outputs asked for before the encoder knew its parameter sets.
    pending_record: Option<(PathBuf, RecordFormat)>,
    pending_stream: Option<(String, String)>,
    state: Arc<OutputState>,
    recorder: Option<Recorder>,
    stream: Option<Stream>,
    tick0: Option<u64>,
    /// Next audio sample (engine timeline) the AAC encoder expects.
    audio_next: Option<u64>,
    early_audio: VecDeque<AudioMsg>,
    avcc: Option<Vec<u8>>,
    asc: Vec<u8>,
    encode_avg: f32,
}

impl Encoder {
    fn new(cfg: EncoderConfig, venc: VideoEnc, aenc: AudioEncoder, state: Arc<OutputState>) -> Self {
        let avcc = venc.avcc();
        let asc = match aenc.sample_entry().codec {
            CodecConfig::Aac(c) => c.asc.clone(),
            _ => Vec::new(),
        };
        Encoder {
            cfg,
            venc,
            aenc,
            pending_record: None,
            pending_stream: None,
            state,
            recorder: None,
            stream: None,
            tick0: None,
            audio_next: None,
            early_audio: VecDeque::new(),
            avcc,
            asc,
            encode_avg: 0.0,
        }
    }

    fn run(&mut self, ctl: Receiver<Ctl>, video: Receiver<VideoMsg>, audio: Receiver<AudioMsg>) {
        let mut had_sink = false;
        loop {
            crossbeam_channel::select! {
                recv(ctl) -> m => match m {
                    Ok(c) => self.control(c),
                    Err(_) => break,
                },
                recv(video) -> m => match m {
                    Ok((tick, img)) => self.video(tick, &img),
                    Err(_) => break,
                },
                recv(audio) -> m => match m {
                    Ok(a) => self.audio(a),
                    Err(_) => break,
                },
            }
            if let Some(s) = &self.stream {
                if s.streamer.is_finished() {
                    // Gave up (bad key, too many reconnects): report and drop it.
                    if let Some(st) = self.state.stream.lock().as_ref() {
                        if let rtmp::StreamState::Failed(e) = &*st.state.lock() {
                            *self.state.last_error.lock() = Some(format!("Stream: {e}"));
                        }
                    }
                    self.stream = None;
                    *self.state.stream.lock() = None;
                }
            }
            let has = self.recorder.is_some() || self.stream.is_some() || self.pending_record.is_some() || self.pending_stream.is_some();
            had_sink |= has;
            if had_sink && !has {
                break;
            }
        }
        if let Some(r) = self.recorder.take() {
            self.finish_recording(r);
        }
        if let Some(s) = self.stream.take() {
            s.streamer.stop();
        }
        *self.state.recording.lock() = None;
        *self.state.stream.lock() = None;
    }

    fn control(&mut self, c: Ctl) {
        match c {
            Ctl::StartRecording { path, format } => {
                if self.recorder.is_some() || self.pending_record.is_some() {
                    return;
                }
                *self.state.recording.lock() = Some(RecordingInfo { path: path.clone(), since: Instant::now() });
                self.pending_record = Some((path, format));
                self.start_pending();
            }
            Ctl::StopRecording => {
                self.pending_record = None;
                if let Some(r) = self.recorder.take() {
                    self.finish_recording(r);
                }
                *self.state.recording.lock() = None;
            }
            Ctl::StartStream { url, key } => {
                if self.stream.is_some() || self.pending_stream.is_some() {
                    return;
                }
                self.pending_stream = Some((url, key));
                self.start_pending();
            }
            Ctl::StopStream => {
                self.pending_stream = None;
                if let Some(s) = self.stream.take() {
                    s.streamer.stop();
                }
                *self.state.stream.lock() = None;
            }
        }
    }

    /// Open the outputs that were waiting for the encoder's parameter sets.
    fn start_pending(&mut self) {
        let Some(avcc) = self.avcc.clone() else { return };
        let (w, h) = (self.cfg.video.width, self.cfg.video.height);
        if let Some((path, format)) = self.pending_record.take() {
            let container = if format == RecordFormat::Mov { Container::Mov } else { Container::Mp4 };
            let fragmented = format == RecordFormat::FragmentedMp4;
            let timescale = self.venc.timescale();
            let r = MediaWriter::create(&path, container, fragmented).and_then(|mut wr| {
                let v = wr.add_h264_track(&avcc, w as u16, h as u16, timescale)?;
                let a = wr.add_audio_track(&self.aenc)?;
                Ok((wr, v, a))
            });
            match r {
                Ok((writer, v, a)) => {
                    log::info!("recording to {}", path.display());
                    self.recorder = Some(Recorder { writer, v, a, started: false, v_origin: 0, a_origin: 0, held: None, early_audio: VecDeque::new() });
                }
                Err(e) => {
                    *self.state.last_error.lock() = Some(format!("Recording: {}: {e}", path.display()));
                    *self.state.recording.lock() = None;
                }
            }
        }
        if let Some((url, key)) = self.pending_stream.take() {
            let info = StreamInfo {
                width: w,
                height: h,
                fps: self.cfg.fps,
                video_kbps: self.cfg.video_kbps,
                audio_kbps: self.cfg.audio_kbps,
                avcc,
                asc: self.asc.clone(),
                sample_rate: SAMPLE_RATE,
                channels: CHANNELS as u32,
            };
            let streamer = Streamer::start(url, key, info);
            *self.state.stream.lock() = Some(Arc::clone(&streamer.status));
            self.stream = Some(Stream { streamer, started: false, v_origin_ms: 0, early_audio: VecDeque::new() });
        }
    }

    fn finish_recording(&mut self, mut r: Recorder) {
        if let Some(mut p) = r.held.take() {
            p.duration = self.venc.frame_duration();
            let _ = r.writer.write(r.v, &p);
        }
        if let Ok(ps) = self.aenc.flush() {
            for p in ps {
                if r.started && p.pts >= r.a_origin {
                    let _ = r.writer.write(r.a, &shift(&p, r.a_origin));
                }
            }
        }
        if let Err(e) = r.writer.finish() {
            *self.state.last_error.lock() = Some(format!("Recording: {e}"));
        }
        log::info!("recording finished");
    }

    fn video(&mut self, tick: u64, img: &VideoFrame) {
        if self.recorder.is_none() && self.stream.is_none() && self.pending_record.is_none() && self.pending_stream.is_none() {
            return;
        }
        let (w, h) = (self.cfg.video.width, self.cfg.video.height);
        let (iw, ih) = img.size();
        if iw < w || ih < h {
            return; // canvas resized under us; outputs lock video settings, so this is transient
        }
        let tick0 = *self.tick0.get_or_insert(tick);
        if tick < tick0 {
            return;
        }
        if self.audio_next.is_none() {
            self.audio_next = Some(tick0 * SAMPLE_RATE as u64 / self.cfg.fps as u64);
            for a in std::mem::take(&mut self.early_audio) {
                self.audio(a);
            }
        }
        let t = Instant::now();
        let encoded = match img {
            VideoFrame::Rgba(img) => {
                let rgba: std::borrow::Cow<[u8]> = if img.width == w && img.height == h {
                    std::borrow::Cow::Borrowed(&img.data)
                } else {
                    // Odd canvas sizes: crop to the even encode size.
                    let mut v = Vec::with_capacity((w * h * 4) as usize);
                    for y in 0..h as usize {
                        let row = y * img.width as usize * 4;
                        v.extend_from_slice(&img.data[row..row + w as usize * 4]);
                    }
                    std::borrow::Cow::Owned(v)
                };
                self.venc.encode_rgba(&rgba, tick - tick0)
            }
            VideoFrame::Nv12(f) if f.width == w && f.height == h => self.venc.encode_nv12(&f.data, w, h, tick - tick0),
            VideoFrame::Nv12(_) => return,
        };
        let packets = match encoded {
            Ok(p) => p,
            Err(e) => {
                *self.state.last_error.lock() = Some(format!("Encoder: {e}"));
                return;
            }
        };
        let ms = t.elapsed().as_secs_f32() * 1000.0;
        self.encode_avg = if self.encode_avg == 0.0 { ms } else { self.encode_avg * 0.95 + ms * 0.05 };
        self.state.encode_ms.set(self.encode_avg);
        self.state.encoded_frames.fetch_add(1, Ordering::Relaxed);
        if self.avcc.is_none() {
            self.avcc = self.venc.avcc();
        }
        // Outputs requested before the parameter sets were known start on this (key)frame.
        self.start_pending();
        let timescale = self.venc.timescale() as i64;
        let priming = self.aenc.priming() as i64;
        for p in packets {
            if let Some(r) = &mut self.recorder {
                if !r.started && p.keyframe {
                    r.started = true;
                    r.v_origin = p.dts;
                    // The audio track's edit skips the encoder priming, so packet pts (not
                    // pts − priming) lines up with the picture.
                    r.a_origin = p.pts * SAMPLE_RATE as i64 / timescale;
                    for a in std::mem::take(&mut r.early_audio) {
                        if a.pts >= r.a_origin {
                            let _ = r.writer.write(r.a, &shift(&a, r.a_origin));
                        }
                    }
                }
                if r.started {
                    let p2 = shift(&p, r.v_origin);
                    if let Some(mut prev) = r.held.replace(p2.clone()) {
                        prev.duration = (p2.dts - prev.dts).clamp(1, u32::MAX as i64) as u32;
                        if let Err(e) = r.writer.write(r.v, &prev) {
                            *self.state.last_error.lock() = Some(format!("Recording: {e}"));
                        }
                    }
                }
            }
            if let Some(s) = &mut self.stream {
                if !s.started && p.keyframe {
                    s.started = true;
                    s.v_origin_ms = p.dts * 1000 / timescale;
                    for a in std::mem::take(&mut s.early_audio) {
                        let ms = (a.pts - priming) * 1000 / SAMPLE_RATE as i64 - s.v_origin_ms;
                        if ms >= 0 {
                            s.streamer.send(StreamPacket::Audio { ts: ms as u32, data: Arc::new(a.data) });
                        }
                    }
                }
                if s.started {
                    let ts = (p.dts * 1000 / timescale - s.v_origin_ms).max(0) as u32;
                    let cts = ((p.pts - p.dts) * 1000 / timescale) as i32;
                    s.streamer.send(StreamPacket::Video { ts, cts, key: p.keyframe, data: Arc::new(p.data.clone()) });
                }
            }
        }
    }

    fn audio(&mut self, (start, data): AudioMsg) {
        let Some(next) = self.audio_next else {
            // Before the first picture: keep a little, the rest is too early anyway.
            self.early_audio.push_back((start, data));
            while self.early_audio.len() > 200 {
                self.early_audio.pop_front();
            }
            return;
        };
        let frames = (data.len() / CHANNELS) as u64;
        let end = start + frames;
        if end <= next {
            return;
        }
        let mut planar = [Vec::new(), Vec::new()];
        if start > next {
            // A gap (mixer stalled): fill with silence so timestamps stay continuous.
            let gap = ((start - next) as usize).min(SAMPLE_RATE as usize);
            for c in &mut planar {
                c.resize(gap, 0.0);
            }
        }
        let skip = next.saturating_sub(start) as usize;
        for f in skip..frames as usize {
            planar[0].push(data[f * 2]);
            planar[1].push(data[f * 2 + 1]);
        }
        self.audio_next = Some(end);
        let packets = match self.aenc.encode(&[&planar[0], &planar[1]]) {
            Ok(p) => p,
            Err(e) => {
                *self.state.last_error.lock() = Some(format!("Audio encoder: {e}"));
                return;
            }
        };
        let priming = self.aenc.priming() as i64;
        for p in packets {
            if let Some(r) = &mut self.recorder {
                if r.started {
                    if p.pts >= r.a_origin {
                        if let Err(e) = r.writer.write(r.a, &shift(&p, r.a_origin)) {
                            *self.state.last_error.lock() = Some(format!("Recording: {e}"));
                        }
                    }
                } else {
                    r.early_audio.push_back(p.clone());
                    while r.early_audio.len() > 100 {
                        r.early_audio.pop_front();
                    }
                }
            }
            if let Some(s) = &mut self.stream {
                if s.started {
                    let ms = (p.pts - priming) * 1000 / SAMPLE_RATE as i64 - s.v_origin_ms;
                    if ms >= 0 {
                        s.streamer.send(StreamPacket::Audio { ts: ms as u32, data: Arc::new(p.data.clone()) });
                    }
                } else {
                    s.early_audio.push_back(p.clone());
                    while s.early_audio.len() > 100 {
                        s.early_audio.pop_front();
                    }
                }
            }
        }
    }
}

fn shift(p: &Packet, origin: i64) -> Packet {
    Packet { data: p.data.clone(), pts: p.pts - origin, dts: p.dts - origin, duration: p.duration, keyframe: p.keyframe }
}
