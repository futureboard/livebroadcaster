//! Browser source: web pages through the Chromium Embedded Framework (cef-rs), rendered
//! off-screen. CEF runs its own UI thread (`multi_threaded_message_loop`) and its helper
//! processes are `lb-browser-helper` next to the studio. Pictures arrive in `on_paint`
//! (premultiplied BGRA, converted to the studio's straight RGBA); sound arrives through the
//! audio handler and goes to the mixer instead of the speakers.

use std::path::PathBuf;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use cef::*;
use parking_lot::Mutex;

use super::SourceShared;
use crate::model::{BrowserSettings as SourceSettings, SourceKind};
use crate::video::{Change, RgbaImage};

/// Name of the helper executable (CEF's renderer / GPU / utility processes).
#[cfg(windows)]
pub const HELPER_EXE: &str = "lb-browser-helper.exe";
#[cfg(not(windows))]
pub const HELPER_EXE: &str = "lb-browser-helper";

/// The CEF shared library that has to sit next to the helper.
#[cfg(windows)]
const CEF_LIB: &str = "libcef.dll";
#[cfg(target_os = "linux")]
const CEF_LIB: &str = "libcef.so";
#[cfg(target_os = "macos")]
const CEF_LIB: &str = "Chromium Embedded Framework.framework/Chromium Embedded Framework";
#[cfg(not(any(windows, target_os = "linux", target_os = "macos")))]
const CEF_LIB: &str = "libcef.so";

/// Browsers created and not yet closed (shutdown waits for them).
static OPEN: AtomicUsize = AtomicUsize::new(0);
/// `None`: not tried yet; `Some(Err)`: CEF can't run here.
static INIT: Mutex<Option<Result<(), String>>> = Mutex::new(None);

/// Where the CEF runtime files are: the helper's directory (next to the studio, or one level up
/// for test executables in `deps/`).
fn runtime_dir() -> Option<PathBuf> {
    let exe = std::env::current_exe().ok()?;
    let dir = exe.parent()?;
    let found = [Some(dir), dir.parent()].into_iter().flatten().find(|d| d.join(HELPER_EXE).is_file() && d.join(CEF_LIB).exists()).map(PathBuf::from);
    found
}

/// Start CEF (once per process). Call on the thread that will also call [`shutdown`] — the UI
/// thread in the app.
pub fn ensure_init() -> Result<(), String> {
    let mut g = INIT.lock();
    if let Some(r) = g.as_ref() {
        return r.clone();
    }
    let r = init();
    if let Err(e) = &r {
        log::warn!("browser source unavailable: {e}");
    }
    *g = Some(r.clone());
    r
}

fn init() -> Result<(), String> {
    let dir = runtime_dir().ok_or_else(|| format!("the Chromium runtime ({CEF_LIB}, {HELPER_EXE}) is missing next to the program"))?;
    // Windows delay-loads libcef.dll: load it from the runtime directory before the first call.
    // Linux and macOS link it (an `$ORIGIN` rpath on Linux), so it is already mapped.
    #[cfg(windows)]
    unsafe {
        use windows::core::HSTRING;
        use windows::Win32::System::LibraryLoader::{LoadLibraryExW, LOAD_WITH_ALTERED_SEARCH_PATH};
        LoadLibraryExW(&HSTRING::from(dir.join(CEF_LIB).as_os_str()), None, LOAD_WITH_ALTERED_SEARCH_PATH).map_err(|e| format!("load {CEF_LIB}: {e}"))?;
    }
    let _ = api_hash(sys::CEF_API_VERSION_LAST, 0);
    let cache = crate::config::config_dir().join("browser");
    let _ = std::fs::create_dir_all(&cache);
    let path = |p: &std::path::Path| CefString::from(p.to_string_lossy().as_ref());
    let settings = Settings {
        no_sandbox: 1,
        multi_threaded_message_loop: 1,
        windowless_rendering_enabled: 1,
        browser_subprocess_path: path(&dir.join(HELPER_EXE)),
        resources_dir_path: path(&dir),
        locales_dir_path: path(&dir.join("locales")),
        root_cache_path: path(&cache),
        cache_path: path(&cache.join("default")),
        log_file: path(&cache.join("cef.log")),
        log_severity: LogSeverity::WARNING,
        background_color: 0,
        ..Default::default()
    };
    let args = args::Args::new();
    let mut app = StudioApp::new();
    if initialize(Some(args.as_main_args()), Some(&settings), Some(&mut app), std::ptr::null_mut()) != 1 {
        return Err("CEF failed to start (see browser/cef.log in the settings folder)".into());
    }
    log::info!("browser: CEF {} started", env!("CARGO_PKG_VERSION"));
    Ok(())
}

/// Close CEF at exit (after the sources are gone): waits briefly for the browsers to close.
pub fn shutdown() {
    if !matches!(*INIT.lock(), Some(Ok(()))) {
        return;
    }
    let t = Instant::now();
    while OPEN.load(Ordering::Acquire) > 0 && t.elapsed() < Duration::from_secs(3) {
        std::thread::sleep(Duration::from_millis(20));
    }
    cef::shutdown();
}

wrap_app! {
    struct StudioApp;
    impl App {
        fn on_before_command_line_processing(&self, _process_type: Option<&CefString>, command_line: Option<&mut CommandLine>) {
            let Some(cl) = command_line else { return };
            // No Chrome window of its own (only our off-screen browsers).
            cl.append_switch(Some(&"no-startup-window".into()));
            // Overlays and alerts play sound / video without a click.
            cl.append_switch_with_value(Some(&"autoplay-policy".into()), Some(&"no-user-gesture-required".into()));
            // Pages keep animating at full rate while "hidden" (they're off-screen by design).
            cl.append_switch(Some(&"disable-background-timer-throttling".into()));
            cl.append_switch(Some(&"disable-renderer-backgrounding".into()));
        }
    }
}

/// What the CEF callbacks of one browser share with its source thread.
struct State {
    src: Arc<SourceShared>,
    width: u32,
    height: u32,
    css: String,
    browser: Mutex<Option<Browser>>,
    channels: AtomicUsize,
    rate: AtomicUsize,
    counted: std::sync::atomic::AtomicBool,
    paint: Mutex<Paint>,
    /// Audio packet timing (`LB_PERF`): (window start, packets, frames, longest gap, last packet).
    audio_stats: Mutex<(Instant, u32, u64, Duration, Option<Instant>)>,
}

/// The page's picture, kept converted so a paint only converts what it redrew.
struct Paint {
    full: RgbaImage,
    seq: u64,
    /// What each recent paint redrew: (seq, rect).
    history: std::collections::VecDeque<(u64, [u32; 4])>,
    // Statistics (`LB_PERF`).
    frames: u32,
    converted_px: u64,
    busy: Duration,
    since: Instant,
}

wrap_render_handler! {
    struct Render { st: Arc<State> }
    impl RenderHandler {
        fn view_rect(&self, _browser: Option<&mut Browser>, rect: Option<&mut Rect>) {
            if let Some(r) = rect {
                *r = Rect { x: 0, y: 0, width: self.st.width as _, height: self.st.height as _ };
            }
        }
        fn screen_info(&self, _browser: Option<&mut Browser>, screen_info: Option<&mut ScreenInfo>) -> ::std::os::raw::c_int {
            match screen_info {
                Some(si) => {
                    si.device_scale_factor = 1.0;
                    1
                }
                None => 0,
            }
        }
        fn on_paint(&self, _browser: Option<&mut Browser>, type_: PaintElementType, dirty_rects: Option<&[Rect]>, buffer: *const u8, width: ::std::os::raw::c_int, height: ::std::os::raw::c_int) {
            if type_ != PaintElementType::VIEW || buffer.is_null() || width <= 0 || height <= 0 {
                return;
            }
            let t0 = Instant::now();
            let (w, h) = (width as u32, height as u32);
            let src = unsafe { std::slice::from_raw_parts(buffer, w as usize * h as usize * 4) };
            let mut p = self.st.paint.lock();
            let resized = p.full.width != w || p.full.height != h;
            if resized {
                p.full = RgbaImage::new(w, h);
            }
            let rects: Vec<[u32; 4]> = match dirty_rects {
                Some(r) if !resized && !r.is_empty() => r.iter().filter_map(|r| clamp_rect(r, w, h)).collect(),
                _ => vec![[0, 0, w, h]],
            };
            let mut px = 0u64;
            for r in &rects {
                unpremultiply_rect(src, &mut p.full.data, w, *r);
                px += r[2] as u64 * r[3] as u64;
            }
            let union = rects.iter().copied().reduce(|a, b| {
                let (x0, y0) = (a[0].min(b[0]), a[1].min(b[1]));
                let (x1, y1) = ((a[0] + a[2]).max(b[0] + b[2]), (a[1] + a[3]).max(b[1] + b[3]));
                [x0, y0, x1 - x0, y1 - y0]
            });
            let Some(union) = union else { return };
            let prev = p.seq;
            p.seq += 1;
            let seq = p.seq;
            if resized {
                p.history.clear();
            }
            p.history.push_back((seq, union));
            if p.history.len() > 8 {
                p.history.pop_front();
            }
            // The reused buffer holds an older paint: bring over only what changed since.
            let mut out = self.st.src.video.recycle_as_is(w, h);
            let held = out.change.map(|c| c.seq).filter(|s| !resized && p.history.front().is_some_and(|(first, _)| *first <= s + 1));
            match held {
                Some(s) => {
                    let row = w as usize * 4;
                    for (_, [x, y, rw, rh]) in p.history.iter().filter(|(q, _)| *q > s) {
                        let (x, y, rw, rh) = (*x as usize, *y as usize, *rw as usize, *rh as usize);
                        for yy in y..y + rh {
                            let o = yy * row + x * 4;
                            out.data[o..o + rw * 4].copy_from_slice(&p.full.data[o..o + rw * 4]);
                        }
                    }
                }
                None => out.data.copy_from_slice(&p.full.data),
            }
            out.change = (!resized).then_some(Change { seq, prev, rect: union });
            self.st.src.video.set(out);
            p.frames += 1;
            p.converted_px += px;
            p.busy += t0.elapsed();
            if p.since.elapsed() >= Duration::from_secs(2) {
                if std::env::var_os("LB_PERF").is_some() {
                    let el = p.since.elapsed().as_secs_f32();
                    log::info!(
                        "browser: {:.1} paints/s, {:.2} ms each, {:.0}% of the page redrawn",
                        p.frames as f32 / el,
                        p.busy.as_secs_f32() * 1000.0 / p.frames.max(1) as f32,
                        p.converted_px as f64 * 100.0 / (p.frames.max(1) as f64 * w as f64 * h as f64)
                    );
                }
                p.frames = 0;
                p.converted_px = 0;
                p.busy = Duration::ZERO;
                p.since = Instant::now();
            }
        }
    }
}

wrap_audio_handler! {
    struct Audio { st: Arc<State> }
    impl AudioHandler {
        fn audio_parameters(&self, _browser: Option<&mut Browser>, params: Option<&mut AudioParameters>) -> ::std::os::raw::c_int {
            if let Some(p) = params {
                p.sample_rate = crate::audio::SAMPLE_RATE as _;
                p.channel_layout = ChannelLayout::LAYOUT_STEREO;
                p.frames_per_buffer = 480;
            }
            1
        }
        fn on_audio_stream_started(&self, _browser: Option<&mut Browser>, params: Option<&AudioParameters>, channels: ::std::os::raw::c_int) {
            self.st.channels.store(channels.max(1) as usize, Ordering::Relaxed);
            let rate = params.map(|p| p.sample_rate).unwrap_or(crate::audio::SAMPLE_RATE as _);
            self.st.rate.store(rate.max(1) as usize, Ordering::Relaxed);
            log::info!("browser audio: stream started, {channels} ch @ {rate} Hz, {} frames per packet", params.map(|p| p.frames_per_buffer).unwrap_or(0));
        }
        fn on_audio_stream_packet(&self, _browser: Option<&mut Browser>, data: *mut *const f32, frames: ::std::os::raw::c_int, _pts: i64) {
            let Some(tap) = self.st.src.audio.as_ref() else { return };
            let ch = self.st.channels.load(Ordering::Relaxed);
            if data.is_null() || frames <= 0 || ch == 0 {
                return;
            }
            let frames = frames as usize;
            let planes = unsafe { std::slice::from_raw_parts(data as *const *const f32, ch) };
            let mut interleaved = vec![0f32; frames * ch];
            for (c, p) in planes.iter().enumerate() {
                if p.is_null() {
                    continue;
                }
                let plane = unsafe { std::slice::from_raw_parts(*p, frames) };
                for (i, v) in plane.iter().enumerate() {
                    interleaved[i * ch + c] = *v;
                }
            }
            tap.push(&interleaved, ch, self.st.rate.load(Ordering::Relaxed) as u32);
            let now = Instant::now();
            let mut a = self.st.audio_stats.lock();
            if let Some(last) = a.4 {
                a.3 = a.3.max(now - last);
            }
            a.4 = Some(now);
            a.1 += 1;
            a.2 += frames as u64;
            if a.0.elapsed() >= Duration::from_secs(2) {
                if std::env::var_os("LB_PERF").is_some() {
                    let el = a.0.elapsed().as_secs_f64();
                    log::info!(
                        "browser audio: {:.1} packets/s, {:.0} frames/s, {} frames each, longest gap {:.1} ms",
                        a.1 as f64 / el,
                        a.2 as f64 / el,
                        a.2 / a.1.max(1) as u64,
                        a.3.as_secs_f64() * 1000.0
                    );
                }
                *a = (now, 0, 0, Duration::ZERO, a.4);
            }
        }
        fn on_audio_stream_stopped(&self, _browser: Option<&mut Browser>) {
            log::info!("browser audio: stream stopped");
        }
        fn on_audio_stream_error(&self, _browser: Option<&mut Browser>, message: Option<&CefString>) {
            log::warn!("browser audio: {}", message.map(|m| m.to_string()).unwrap_or_default());
        }
    }
}

wrap_load_handler! {
    struct Load { st: Arc<State> }
    impl LoadHandler {
        fn on_load_end(&self, _browser: Option<&mut Browser>, frame: Option<&mut Frame>, http_status_code: ::std::os::raw::c_int) {
            let Some(frame) = frame else { return };
            if frame.is_main() != 1 {
                return;
            }
            if http_status_code >= 400 {
                self.st.src.set_error(format!("page returned HTTP {http_status_code}"));
            } else {
                self.st.src.clear_error();
            }
            if !self.st.css.trim().is_empty() {
                let css = serde_json::to_string(&self.st.css).unwrap_or_default();
                let js = format!("(function(){{var s=document.createElement('style');s.textContent={css};(document.head||document.documentElement).appendChild(s);}})();");
                frame.execute_java_script(Some(&CefString::from(js.as_str())), Some(&CefString::from("livebroadcaster://css")), 0);
            }
        }
        fn on_load_error(&self, _browser: Option<&mut Browser>, frame: Option<&mut Frame>, error_code: Errorcode, error_text: Option<&CefString>, failed_url: Option<&CefString>) {
            if frame.is_some_and(|f| f.is_main() == 1) {
                let _ = error_code;
                self.st.src.set_error(format!(
                    "could not load {}: {}",
                    failed_url.map(|u| u.to_string()).unwrap_or_default(),
                    error_text.map(|t| t.to_string()).unwrap_or_default()
                ));
            }
        }
    }
}

wrap_life_span_handler! {
    struct Life { st: Arc<State> }
    impl LifeSpanHandler {
        fn on_after_created(&self, browser: Option<&mut Browser>) {
            let Some(b) = browser else { return };
            if self.st.src.stopped() {
                // Removed while it was being created.
                if let Some(h) = b.host() {
                    h.close_browser(1);
                }
                return;
            }
            *self.st.browser.lock() = Some(b.clone());
        }
        // Pages can't open windows (target=_blank links, window.open): they would show up on
        // the desktop as real Chrome windows.
        fn on_before_popup(&self, _browser: Option<&mut Browser>, _frame: Option<&mut Frame>, _popup_id: ::std::os::raw::c_int, target_url: Option<&CefString>, _target_frame_name: Option<&CefString>, _target_disposition: WindowOpenDisposition, _user_gesture: ::std::os::raw::c_int, _popup_features: Option<&PopupFeatures>, _window_info: Option<&mut WindowInfo>, _client: Option<&mut Option<Client>>, _settings: Option<&mut BrowserSettings>, _extra_info: Option<&mut Option<DictionaryValue>>, _no_javascript_access: Option<&mut ::std::os::raw::c_int>) -> ::std::os::raw::c_int {
            log::info!("browser: blocked a pop-up to {}", target_url.map(|u| u.to_string()).unwrap_or_default());
            1
        }
        fn on_before_close(&self, _browser: Option<&mut Browser>) {
            self.st.browser.lock().take();
            if self.st.counted.swap(false, Ordering::AcqRel) {
                OPEN.fetch_sub(1, Ordering::AcqRel);
            }
        }
    }
}

wrap_display_handler! {
    struct Display;
    impl DisplayHandler {
        // The page's console, in the studio log (`RUST_LOG=live_broadcaster=debug`).
        fn on_console_message(&self, _browser: Option<&mut Browser>, level: LogSeverity, message: Option<&CefString>, source: Option<&CefString>, line: ::std::os::raw::c_int) -> ::std::os::raw::c_int {
            let msg = message.map(|m| m.to_string()).unwrap_or_default();
            let src = source.map(|s| s.to_string()).unwrap_or_default();
            let src = src.rsplit('/').next().unwrap_or_default();
            if level == LogSeverity::ERROR {
                log::warn!("page console: {msg} ({src}:{line})");
            } else {
                log::debug!("page console: {msg} ({src}:{line})");
            }
            0
        }
    }
}

wrap_client! {
    struct Client_ { render: RenderHandler, audio: Option<AudioHandler>, load: LoadHandler, life: LifeSpanHandler, display: DisplayHandler }
    impl Client {
        fn display_handler(&self) -> Option<DisplayHandler> {
            Some(self.display.clone())
        }
        fn render_handler(&self) -> Option<RenderHandler> {
            Some(self.render.clone())
        }
        fn audio_handler(&self) -> Option<AudioHandler> {
            self.audio.clone()
        }
        fn load_handler(&self) -> Option<LoadHandler> {
            Some(self.load.clone())
        }
        fn life_span_handler(&self) -> Option<LifeSpanHandler> {
            Some(self.life.clone())
        }
    }
}

fn clamp_rect(r: &Rect, w: u32, h: u32) -> Option<[u32; 4]> {
    let x0 = r.x.clamp(0, w as i32) as u32;
    let y0 = r.y.clamp(0, h as i32) as u32;
    let x1 = (r.x + r.width).clamp(0, w as i32) as u32;
    let y1 = (r.y + r.height).clamp(0, h as i32) as u32;
    (x1 > x0 && y1 > y0).then_some([x0, y0, x1 - x0, y1 - y0])
}

/// Convert the `rect` (x, y, w, h) part of a `stride`-pixel-wide premultiplied BGRA picture
/// into straight RGBA in `dst` (same layout).
fn unpremultiply_rect(src: &[u8], dst: &mut [u8], stride: u32, rect: [u32; 4]) {
    use rayon::prelude::*;
    let row = stride as usize * 4;
    let [x, y, w, h] = rect.map(|v| v as usize);
    let work = |(d, s): (&mut [u8], &[u8])| unpremultiply_bgra(&s[x * 4..(x + w) * 4], &mut d[x * 4..(x + w) * 4]);
    let (d, s) = (&mut dst[y * row..(y + h) * row], &src[y * row..(y + h) * row]);
    // Small regions aren't worth handing to other threads.
    if w * h < 64 * 1024 {
        d.chunks_mut(row).zip(s.chunks(row)).for_each(work);
    } else {
        d.par_chunks_mut(row).zip(s.par_chunks(row)).for_each(work);
    }
}

/// CEF paints premultiplied BGRA; the studio's pictures are straight RGBA.
fn unpremultiply_bgra(src: &[u8], dst: &mut [u8]) {
    {
        for (o, p) in dst.chunks_exact_mut(4).zip(src.chunks_exact(4)) {
            let a = p[3] as u32;
            if a == 255 {
                o[0] = p[2];
                o[1] = p[1];
                o[2] = p[0];
            } else if a == 0 {
                o[0] = 0;
                o[1] = 0;
                o[2] = 0;
            } else {
                o[0] = ((p[2] as u32 * 255 + a / 2) / a).min(255) as u8;
                o[1] = ((p[1] as u32 * 255 + a / 2) / a).min(255) as u8;
                o[2] = ((p[0] as u32 * 255 + a / 2) / a).min(255) as u8;
            }
            o[3] = a as u8;
        }
    }
}

/// The address to open: URLs as they are, local files as file:// URLs.
fn page_url(s: &SourceSettings) -> String {
    let u = s.url.trim();
    let p = std::path::Path::new(u);
    if !u.contains("://") && p.exists() {
        let abs = std::fs::canonicalize(p).unwrap_or_else(|_| p.to_path_buf());
        let s = abs.to_string_lossy().replace('\\', "/");
        return format!("file:///{}", s.trim_start_matches("//?/"));
    }
    if !u.contains("://") && !u.starts_with("about:") && !u.starts_with("data:") {
        return format!("https://{u}");
    }
    u.to_string()
}

/// The source thread: create the browser, follow the on-air state, close it when removed.
pub fn run(kind: SourceKind, s: Arc<SourceShared>) {
    let SourceKind::Browser(cfg) = kind else { return };
    if cfg.url.trim().is_empty() {
        s.set_error("no page address");
        return;
    }
    if let Some(Err(e)) = INIT.lock().clone() {
        s.set_error(e);
        return;
    }
    let st = Arc::new(State {
        src: Arc::clone(&s),
        width: cfg.width.clamp(16, 7680),
        height: cfg.height.clamp(16, 4320),
        css: cfg.css.clone(),
        browser: Mutex::new(None),
        channels: AtomicUsize::new(2),
        rate: AtomicUsize::new(crate::audio::SAMPLE_RATE as usize),
        counted: std::sync::atomic::AtomicBool::new(true),
        audio_stats: Mutex::new((Instant::now(), 0, 0, Duration::ZERO, None)),
        paint: Mutex::new(Paint { full: RgbaImage::default(), seq: 0, history: Default::default(), frames: 0, converted_px: 0, busy: Duration::ZERO, since: Instant::now() }),
    });
    let mut client = Client_::new(
        Render::new(Arc::clone(&st)),
        cfg.audio.then(|| Audio::new(Arc::clone(&st))),
        Load::new(Arc::clone(&st)),
        Life::new(Arc::clone(&st)),
        Display::new(),
    );
    let window_info = WindowInfo::default().set_as_windowless(Default::default());
    let settings = BrowserSettings { windowless_frame_rate: cfg.fps.clamp(1, 60) as _, background_color: 0, ..Default::default() };
    let url = CefString::from(page_url(&cfg).as_str());
    OPEN.fetch_add(1, Ordering::AcqRel);
    if browser_host_create_browser(Some(&window_info), Some(&mut client), Some(&url), Some(&settings), None, None) != 1 {
        OPEN.fetch_sub(1, Ordering::AcqRel);
        st.counted.store(false, Ordering::Release);
        s.set_error("could not create the browser");
        return;
    }
    let mut hidden = false;
    while !s.sleep(Duration::from_millis(100)) {
        if cfg.pause_when_hidden {
            let want_hidden = !s.is_active();
            if want_hidden != hidden {
                if let Some(h) = st.browser.lock().as_ref().and_then(|b| b.host()) {
                    h.was_hidden(want_hidden as _);
                    hidden = want_hidden;
                }
            }
        }
    }
    let browser = st.browser.lock().take();
    if let Some(h) = browser.and_then(|b| b.host()) {
        h.close_browser(1);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unpremultiply() {
        let src = [0u8, 0, 128, 128, 10, 20, 30, 255, 1, 1, 1, 0];
        let mut dst = [0u8; 12];
        unpremultiply_bgra(&src, &mut dst);
        assert_eq!(&dst[0..4], &[255, 0, 0, 128]);
        assert_eq!(&dst[4..8], &[30, 20, 10, 255]);
        assert_eq!(dst[11], 0);
    }

    #[test]
    fn urls() {
        let mut s = SourceSettings::default();
        s.url = "example.com/overlay".into();
        assert_eq!(page_url(&s), "https://example.com/overlay");
        s.url = "http://localhost:8080/a".into();
        assert_eq!(page_url(&s), "http://localhost:8080/a");
    }

    /// A page tone pulled the way the mixer pulls it (10 ms blocks on the clock): counts the
    /// blocks that came out silent once sound had started.
    /// `cargo test -- --ignored browser_audio_continuity --nocapture`
    #[test]
    #[ignore]
    fn browser_audio_continuity() {
        let _ = env_logger::builder().is_test(true).filter_level(log::LevelFilter::Info).try_init();
        std::env::set_var("LB_PERF", "1");
        std::env::set_var("LB_CONFIG_DIR", std::env::temp_dir().join("live-broadcaster-test-browser"));
        ensure_init().expect("CEF");
        let kind = SourceKind::Browser(SourceSettings {
            url: "data:text/html,<script>var c=new AudioContext();var o=c.createOscillator();var g=c.createGain();g.gain.value=0.25;o.connect(g);g.connect(c.destination);o.start();</script>".into(),
            width: 64,
            height: 64,
            css: String::new(),
            ..Default::default()
        });
        let rt = crate::sources::SourceRuntime::start(&kind);
        let tap = rt.shared.audio.clone().unwrap();
        let mut block = vec![0f32; crate::audio::BLOCK * 2];
        let start = Instant::now();
        let (mut started, mut silent, mut total, mut runs) = (false, 0, 0, Vec::new());
        let mut run = 0;
        let mut next = Instant::now();
        while start.elapsed() < Duration::from_secs(8) {
            next += Duration::from_millis(10);
            if let Some(d) = next.checked_duration_since(Instant::now()) {
                std::thread::sleep(d);
            }
            tap.pull(&mut block);
            let quiet = crate::audio::peak(&block) < 0.05;
            if !quiet {
                started = true;
                if run > 0 {
                    runs.push(run);
                    run = 0;
                }
            }
            if started {
                total += 1;
                if quiet {
                    silent += 1;
                    run += 1;
                }
            }
        }
        eprintln!("{silent} of {total} blocks silent after the start; silent runs (blocks): {runs:?}");
        drop(rt);
        shutdown();
        assert!(started, "no sound");
        assert!(silent * 100 <= total, "{silent} of {total} blocks silent");
    }

    /// Plays `LB_TEST_PAGE` (a page that plays sounds) for `LB_TEST_SECS` and prints when sound
    /// came out, pulled the way the mixer pulls it.
    #[test]
    #[ignore]
    fn browser_audio_timeline() {
        let _ = env_logger::builder().is_test(true).filter_level(log::LevelFilter::Info).try_init();
        std::env::set_var("LB_PERF", "1");
        std::env::set_var("LB_CONFIG_DIR", std::env::temp_dir().join("live-broadcaster-test-browser"));
        let Ok(url) = std::env::var("LB_TEST_PAGE") else {
            eprintln!("set LB_TEST_PAGE");
            return;
        };
        let secs: u64 = std::env::var("LB_TEST_SECS").ok().and_then(|s| s.parse().ok()).unwrap_or(10);
        ensure_init().expect("CEF");
        // What reaches the speakers: this process tree's own playback (CEF's audio service).
        let exe = std::env::current_exe().unwrap().file_name().unwrap().to_string_lossy().into_owned();
        let speakers = crate::sources::SourceRuntime::start(&SourceKind::AppAudio { mode: crate::model::AppAudioMode::Only, exe, title: String::new() });
        let spk = speakers.shared.audio.clone().unwrap();
        std::thread::sleep(Duration::from_millis(500));
        let kind = SourceKind::Browser(SourceSettings { url, width: 320, height: 240, css: String::new(), ..Default::default() });
        let rt = crate::sources::SourceRuntime::start(&kind);
        let tap = rt.shared.audio.clone().unwrap();
        let mut spk_block = vec![0f32; crate::audio::BLOCK * 2];
        let mut spk_segs: Vec<(u128, u128)> = Vec::new();
        let mut spk_cur: Option<u128> = None;
        let mut block = vec![0f32; crate::audio::BLOCK * 2];
        let start = Instant::now();
        let mut next = Instant::now();
        let mut segs: Vec<(u128, u128, f32)> = Vec::new();
        let mut cur: Option<(u128, f32)> = None;
        while start.elapsed() < Duration::from_secs(secs) {
            next += Duration::from_millis(10);
            if let Some(d) = next.checked_duration_since(Instant::now()) {
                std::thread::sleep(d);
            }
            tap.pull(&mut block);
            let pk = crate::audio::peak(&block);
            let t = start.elapsed().as_millis();
            spk.pull(&mut spk_block);
            match (spk_cur, crate::audio::peak(&spk_block) >= 0.02) {
                (None, true) => spk_cur = Some(t),
                (Some(s), false) => {
                    spk_segs.push((s, t));
                    spk_cur = None;
                }
                _ => {}
            }
            match (&mut cur, pk >= 0.02) {
                (None, true) => cur = Some((t, pk)),
                (Some((_, m)), true) => *m = m.max(pk),
                (Some((s, m)), false) => {
                    segs.push((*s, t, *m));
                    cur = None;
                }
                (None, false) => {}
            }
        }
        if let Some((s, m)) = cur {
            segs.push((s, start.elapsed().as_millis(), m));
        }
        for (s, e, m) in &segs {
            eprintln!("sound {s:>6} ms .. {e:>6} ms ({:>5} ms, peak {m:.2})", e - s);
        }
        for (s, e) in &spk_segs {
            eprintln!("speakers {s:>6} ms .. {e:>6} ms ({:>5} ms)", e - s);
        }
        drop(speakers);
        drop(rt);
        shutdown();
    }

    /// Renders a data: page with a red background through CEF and checks the picture.
    /// Needs the CEF runtime next to the test binary: `cargo build` first, then
    /// `cargo test -- --ignored browser_renders_page --nocapture`.
    #[test]
    #[ignore]
    fn browser_renders_page() {
        // Keep CEF's cache out of the real profile.
        std::env::set_var("LB_CONFIG_DIR", std::env::temp_dir().join("live-broadcaster-test-browser"));
        ensure_init().expect("CEF");
        let kind = SourceKind::Browser(SourceSettings {
            url: "data:text/html,<html><body style='margin:0;background:rgb(255,0,0)'><h1 style='color:white'>Hi</h1></body></html>".into(),
            width: 320,
            height: 240,
            css: String::new(),
            ..Default::default()
        });
        let rt = crate::sources::SourceRuntime::start(&kind);
        let t = Instant::now();
        let img = loop {
            if let Some(img) = rt.shared.video.get() {
                break img;
            }
            assert!(t.elapsed() < Duration::from_secs(20), "no picture: {:?}", rt.shared.error());
            std::thread::sleep(Duration::from_millis(50));
        };
        assert_eq!((img.width, img.height), (320, 240));
        let px = &img.data[(200 * 320 + 300) * 4..(200 * 320 + 300) * 4 + 4];
        eprintln!("pixel {px:?} after {:?}", t.elapsed());
        assert!(px[0] > 240 && px[1] < 16 && px[2] < 16 && px[3] == 255, "{px:?}");
        drop(rt);

        // Sound: a WebAudio tone reaches the source's tap (not the speakers).
        let kind = SourceKind::Browser(SourceSettings {
            url: "data:text/html,<script>var c=new AudioContext();var o=c.createOscillator();var g=c.createGain();g.gain.value=0.25;o.connect(g);g.connect(c.destination);o.start();</script>".into(),
            width: 64,
            height: 64,
            css: String::new(),
            ..Default::default()
        });
        let rt = crate::sources::SourceRuntime::start(&kind);
        let tap = rt.shared.audio.clone().expect("audio tap");
        let t = Instant::now();
        let mut peak = 0f32;
        while t.elapsed() < Duration::from_secs(10) && peak < 0.1 {
            std::thread::sleep(Duration::from_millis(100));
            let mut buf = vec![0f32; tap.buffered_frames() * 2];
            tap.pull(&mut buf);
            peak = peak.max(crate::audio::peak(&buf));
        }
        eprintln!("browser audio peak {peak:.3} after {:?}", t.elapsed());
        assert!(peak > 0.1, "no sound from the page (peak {peak})");
        drop(rt);
        shutdown();
    }
}
