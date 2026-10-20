//! End-to-end checks of the engine: sources → compositor / mixer → MKFF encoder → outputs.

use std::path::PathBuf;
use std::time::{Duration, Instant};

use crate::engine::Engine;
use crate::model::{EncoderChoice, MediaSettings, RecordFormat, SceneCollection, Settings, SourceKind, StreamService, TextSettings};

/// MKFF's test clips (`testdata/codecs` of a mediakit-foundation checkout); `MKFF_TESTDATA`
/// points elsewhere.
fn testdata() -> String {
    std::env::var("MKFF_TESTDATA").unwrap_or_else(|_| "W:/works/mediakit-foundation/testdata/codecs".into())
}

fn scene_with_sources() -> SceneCollection {
    let mut c = SceneCollection::new_default();
    // No capture devices in tests: drop the global audio devices.
    c.sources.clear();
    c.global_audio.clear();
    let scene = c.current_scene;
    let bg = c.add_source("Background", SourceKind::Color { color: [200, 30, 30, 255], width: 640, height: 360 });
    let media = c.add_source(
        "Clip",
        SourceKind::Media(MediaSettings { path: format!("{}/vp9_opus.webm", testdata()), ..Default::default() }),
    );
    let text = c.add_source("Title", SourceKind::Text(TextSettings { size: 32.0, ..Default::default() }));
    for s in [bg, media, text] {
        c.add_item(scene, s);
    }
    for it in &mut c.scenes[0].items {
        it.needs_layout = false;
    }
    c.scenes[0].items[1].scale = [2.0, 2.0];
    c
}

fn settings(dir: &std::path::Path) -> Settings {
    let mut s = Settings::default();
    s.video.width = 640;
    s.video.height = 360;
    s.video.fps = 30;
    s.output.video_kbps = 1200;
    s.output.record_path = dir.to_string_lossy().into_owned();
    s
}

fn temp_dir(name: &str) -> PathBuf {
    let _ = env_logger::builder().is_test(true).filter_level(log::LevelFilter::Info).try_init();
    let d = std::env::temp_dir().join(format!("live-broadcaster-test-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    d
}

fn wait_idle(engine: &mut Engine) {
    let t = Instant::now();
    while engine.outputs.active() && t.elapsed() < Duration::from_secs(10) {
        std::thread::sleep(Duration::from_millis(50));
    }
    engine.outputs.poll();
    assert!(!engine.outputs.active(), "encoder did not stop");
}

fn headless_gpu() -> Option<crate::gpu_compositor::Gpu> {
    crate::gpu_compositor::Gpu::headless(eframe::egui_wgpu::wgpu::Backends::PRIMARY)
}

/// The encoders this machine has: software always, plus each GPU encoder found.
fn encoders() -> Vec<EncoderChoice> {
    let mut v = vec![EncoderChoice::Software];
    for e in mkff_hwenc::h264_encoders() {
        let c = match e.vendor {
            mkff_hwenc::Vendor::Nvidia => EncoderChoice::Nvenc,
            mkff_hwenc::Vendor::Intel => EncoderChoice::Qsv,
            mkff_hwenc::Vendor::Amd => EncoderChoice::Amf,
            mkff_hwenc::Vendor::Other => continue,
        };
        if !v.contains(&c) {
            v.push(c);
        }
    }
    v
}

#[test]
fn record_scene_to_mp4() {
    let mut runs = vec![(EncoderChoice::Software, RecordFormat::Mp4)];
    runs.extend(encoders().into_iter().map(|e| (e, RecordFormat::FragmentedMp4)));
    for (encoder, format) in runs {
        let dir = temp_dir(&format!("{encoder:?}-{format:?}"));
        let mut st = settings(&dir);
        st.output.encoder = encoder;
        st.output.record_format = format;
        st.output.record_same_as_stream = false;
        // The software run composites on the CPU, the GPU encoders on the GPU.
        let gpu = if encoder == EncoderChoice::Software { None } else { headless_gpu() };
        let mut engine = Engine::start(scene_with_sources(), st.clone(), gpu);
        // Let the media source open and start.
        std::thread::sleep(Duration::from_millis(500));
        let path = engine.outputs.start_recording(&st).expect("start recording");
        std::thread::sleep(Duration::from_secs(3));
        engine.outputs.stop_recording();
        wait_idle(&mut engine);
        let err = engine.outputs.state.last_error.lock().clone();
        assert!(err.is_none(), "output error: {err:?}");
        let desc = engine.outputs.state.encoder_desc.lock().clone();
        eprintln!("encoder: {desc}; compositor: {}", engine.shared.compositor_name.lock());
        let rendered = engine.shared.stats.rendered.load(std::sync::atomic::Ordering::Relaxed);
        let skipped = engine.shared.taps.skipped_frames.load(std::sync::atomic::Ordering::Relaxed);
        engine.shutdown();

        let clip = mkff_codecs::MediaFile::open(&path).expect("open recording with MKFF");
        let info = clip.info().clone();
        eprintln!("{encoder:?} {format:?}: {:?} {:.2}s (rendered {rendered}, skipped {skipped})", info.video.as_ref().map(|v| v.codec.clone()), info.duration_seconds);
        let v = info.video.as_ref().expect("video track");
        assert_eq!((v.width, v.height), (640, 360));
        assert!(!info.audio.is_empty(), "audio track");
        assert!((2.4..=3.8).contains(&info.duration_seconds), "duration {}", info.duration_seconds);
        // The red background shows at the bottom-right corner (clip and title sit top-left).
        let f = clip.video_frame(10).expect("decode frame");
        let rgba = f.to_rgba8();
        let i = ((f.height() as usize - 4) * f.width() as usize + f.width() as usize - 4) * 4;
        let (r, g, b) = (rgba[i] as i32, rgba[i + 1] as i32, rgba[i + 2] as i32);
        assert!(r > 150 && g < 80 && b < 80, "corner pixel {r},{g},{b}");
        // The clip has sound: the recording isn't silent.
        let pcm = clip.audio(0, 48_000 * 2, 48_000).expect("decode audio");
        let peak = pcm.channels.iter().flatten().fold(0f32, |m, s| m.max(s.abs()));
        assert!(peak > 0.01, "audio peak {peak}");
        let _ = std::fs::remove_dir_all(&dir);
    }
}

/// Streams to ffmpeg acting as an RTMP server, then checks what it received with ffprobe.
/// `cargo test -- --ignored rtmp` (needs ffmpeg / ffprobe on PATH).
#[test]
#[ignore]
fn rtmp_to_ffmpeg() {
    let dir = temp_dir("rtmp");
    let out = dir.join("received.flv");
    let port = 19350;
    let mut server = std::process::Command::new("ffmpeg")
        .args(["-hide_banner", "-loglevel", "warning", "-y", "-listen", "1", "-i"])
        .arg(format!("rtmp://127.0.0.1:{port}/live/testkey"))
        .args(["-c", "copy"])
        .arg(&out)
        .spawn()
        .expect("run ffmpeg");
    std::thread::sleep(Duration::from_millis(1500));

    let mut st = settings(&dir);
    st.stream.service = StreamService::Custom;
    st.stream.server = format!("rtmp://127.0.0.1:{port}/live");
    st.stream.key = "testkey".into();
    // The GPU encoder when there is one (the path live streaming normally takes).
    st.output.encoder = EncoderChoice::Auto;
    let mut engine = Engine::start(scene_with_sources(), st.clone(), headless_gpu());
    engine.outputs.start_stream(&st).expect("start stream");
    let t = Instant::now();
    loop {
        let state = engine.outputs.state.stream.lock().as_ref().map(|s| s.state.lock().clone());
        if matches!(state, Some(crate::output::rtmp::StreamState::Live)) {
            break;
        }
        assert!(t.elapsed() < Duration::from_secs(10), "never went live: {state:?}");
        std::thread::sleep(Duration::from_millis(50));
    }
    std::thread::sleep(Duration::from_secs(4));
    let (sent, frames, dropped) = engine
        .outputs
        .state
        .stream
        .lock()
        .as_ref()
        .map(|s| {
            use std::sync::atomic::Ordering::Relaxed;
            (s.bytes_sent.load(Relaxed), s.frames_sent.load(Relaxed), s.frames_dropped.load(Relaxed))
        })
        .unwrap();
    let encoded = engine.outputs.state.encoded_frames.load(std::sync::atomic::Ordering::Relaxed);
    eprintln!("encoded {encoded}, frames sent {frames}, dropped {dropped}");
    engine.outputs.stop_stream();
    wait_idle(&mut engine);
    engine.shutdown();
    let deadline = Instant::now() + Duration::from_secs(10);
    while server.try_wait().unwrap().is_none() && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(100));
    }
    let _ = server.kill();
    eprintln!("sent {sent} bytes");

    let probe = std::process::Command::new("ffprobe")
        .args(["-v", "error", "-show_entries", "stream=codec_name,width,height,sample_rate:format=duration", "-of", "compact"])
        .arg(&out)
        .output()
        .expect("run ffprobe");
    let text = String::from_utf8_lossy(&probe.stdout);
    eprintln!("{text}");
    assert!(text.contains("codec_name=h264"), "{text}");
    assert!(text.contains("width=640") && text.contains("height=360"), "{text}");
    assert!(text.contains("codec_name=aac"), "{text}");
    let dur: f64 = text.split("duration=").nth(1).and_then(|s| s.trim().parse().ok()).unwrap_or(0.0);
    assert!(dur > 3.0, "duration {dur}");
    let _ = std::fs::remove_dir_all(&dir);
}

/// Live load at 1080p60: every encoder records 5 s of a moving scene; prints the encode time
/// and how many frames the encoder couldn't take. `cargo test --release -- --ignored perf_1080p60 --nocapture`
#[test]
#[ignore]
fn perf_1080p60() {
    for encoder in encoders() {
        let dir = temp_dir(&format!("perf-{encoder:?}"));
        let mut st = settings(&dir);
        st.video.width = 1920;
        st.video.height = 1080;
        st.video.fps = 60;
        st.output.encoder = encoder;
        st.output.video_kbps = 6000;
        st.output.record_same_as_stream = true;
        let mut col = scene_with_sources();
        if let Some(crate::model::SourceKind::Color { width, height, .. }) = col.sources.iter_mut().find(|s| s.name == "Background").map(|s| &mut s.kind) {
            *width = 1920;
            *height = 1080;
        }
        let mut engine = Engine::start(col, st.clone(), headless_gpu());
        std::thread::sleep(Duration::from_millis(500));
        engine.outputs.start_recording(&st).expect("start recording");
        std::thread::sleep(Duration::from_secs(5));
        let out = &engine.outputs.state;
        let enc = out.encoded_frames.load(std::sync::atomic::Ordering::Relaxed);
        let skipped = engine.shared.taps.skipped_frames.load(std::sync::atomic::Ordering::Relaxed);
        let ms = out.encode_ms.get();
        let render = engine.shared.stats.render_ms.get();
        let fps = engine.shared.stats.fps.get();
        let desc = out.encoder_desc.lock().clone();
        engine.outputs.stop_recording();
        wait_idle(&mut engine);
        engine.shutdown();
        eprintln!("{encoder:?}: encode {ms:.2} ms/frame, {enc} encoded, {skipped} skipped, compositor {fps:.1} fps / {render:.2} ms — {desc}");
        let _ = std::fs::remove_dir_all(&dir);
    }
}

/// The browser source's sound through the whole studio: a page (`LB_TEST_PAGE`) on the program
/// scene at 1080p60, recorded with the GPU encoder; prints when sound is heard in the recording.
/// `cargo test --release -- --ignored browser_sound_recording --nocapture`
#[cfg(all(windows, feature = "browser"))]
#[test]
#[ignore]
fn browser_sound_recording() {
    let Ok(url) = std::env::var("LB_TEST_PAGE") else {
        eprintln!("set LB_TEST_PAGE");
        return;
    };
    let secs: u64 = std::env::var("LB_TEST_SECS").ok().and_then(|s| s.parse().ok()).unwrap_or(10);
    let dir = temp_dir("browser-sound");
    std::env::set_var("LB_CONFIG_DIR", &dir);
    crate::sources::browser::ensure_init().expect("CEF");
    let mut st = settings(&dir);
    st.video.width = 1920;
    st.video.height = 1080;
    st.video.fps = 60;
    st.output.encoder = EncoderChoice::Auto;
    st.output.video_kbps = 6000;
    let mut col = SceneCollection::new_default();
    col.sources.clear();
    col.global_audio.clear();
    let scene = col.current_scene;
    let b = col.add_source("Browser", SourceKind::Browser(crate::model::BrowserSettings { url, width: 1920, height: 1080, fps: 60, ..Default::default() }));
    col.add_item(scene, b);
    col.scenes[0].items[0].needs_layout = false;
    let mut engine = Engine::start(col, st.clone(), headless_gpu());
    let path = engine.outputs.start_recording(&st).expect("start recording");
    std::thread::sleep(Duration::from_secs(secs));
    engine.outputs.stop_recording();
    wait_idle(&mut engine);
    let underruns = engine.shared.stats.mixer_underruns.load(std::sync::atomic::Ordering::Relaxed);
    engine.shutdown();
    let clip = mkff_codecs::MediaFile::open(&path).expect("open recording");
    let pcm = clip.audio(0, 48_000 * secs as usize, 48_000).expect("decode audio");
    let ch = &pcm.channels[0];
    let mut cur: Option<usize> = None;
    let block = 480;
    let mut segs = Vec::new();
    for (i, b) in ch.chunks(block).enumerate() {
        let loud = b.iter().fold(0f32, |m, s| m.max(s.abs())) >= 0.02;
        match (cur, loud) {
            (None, true) => cur = Some(i),
            (Some(s), false) => {
                segs.push((s * 10, i * 10));
                cur = None;
            }
            _ => {}
        }
    }
    if let Some(s) = cur {
        segs.push((s * 10, ch.len() / 48));
    }
    for (s, e) in &segs {
        eprintln!("recorded sound {s:>6} ms .. {e:>6} ms ({:>5} ms)", e - s);
    }
    eprintln!("mixer resets {underruns}");
    let _ = std::fs::remove_dir_all(&dir);
}

/// Monitoring: a browser source (`LB_TEST_PAGE`) on the program scene is heard on the playback
/// device — measured by capturing this process's own playback. Plays the page's sound aloud.
/// `cargo test -- --ignored monitor_plays_browser_sound --nocapture`
#[cfg(all(windows, feature = "browser"))]
#[test]
#[ignore]
fn monitor_plays_browser_sound() {
    let Ok(url) = std::env::var("LB_TEST_PAGE") else {
        eprintln!("set LB_TEST_PAGE");
        return;
    };
    let dir = temp_dir("monitor");
    std::env::set_var("LB_CONFIG_DIR", &dir);
    crate::sources::browser::ensure_init().expect("CEF");
    let exe = std::env::current_exe().unwrap().file_name().unwrap().to_string_lossy().into_owned();
    let speakers = crate::sources::SourceRuntime::start(&SourceKind::AppAudio { mode: crate::model::AppAudioMode::Only, exe, title: String::new() });
    let spk = speakers.shared.audio.clone().unwrap();
    std::thread::sleep(Duration::from_millis(500));
    let mut col = SceneCollection::new_default();
    col.sources.clear();
    col.global_audio.clear();
    let scene = col.current_scene;
    let b = col.add_source("Browser", SourceKind::Browser(crate::model::BrowserSettings { url, width: 320, height: 240, ..Default::default() }));
    col.add_item(scene, b);
    col.scenes[0].items[0].needs_layout = false;
    let st = settings(&dir);
    let mut engine = Engine::start(col, st, headless_gpu());
    let t = Instant::now();
    let mut heard_ms = 0;
    let mut buf = vec![0f32; crate::audio::BLOCK * 2];
    while t.elapsed() < Duration::from_secs(7) {
        std::thread::sleep(Duration::from_millis(10));
        while spk.buffered_frames() >= crate::audio::BLOCK {
            spk.pull(&mut buf);
            if crate::audio::peak(&buf) >= 0.02 {
                heard_ms += 10;
            }
        }
    }
    let err = engine.shared.monitor.error.lock().clone();
    engine.shutdown();
    drop(speakers);
    eprintln!("heard on the playback device: {heard_ms} ms (monitor error: {err:?})");
    assert!(heard_ms >= 2000, "monitoring played only {heard_ms} ms");
    let _ = std::fs::remove_dir_all(&dir);
}
