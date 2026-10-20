//! Source runtimes: the live half of a [`SourceConfig`]. Each runtime owns whatever thread
//! produces its pictures / sound and stops it when dropped.

#[cfg(windows)]
pub mod app_audio;
mod audio_capture;
#[cfg(feature = "browser")]
pub mod browser;
#[cfg(windows)]
pub mod camera;
#[cfg(windows)]
pub mod display;
mod image_file;
pub mod media;
mod text;
#[cfg(windows)]
mod wgc;
#[cfg(windows)]
pub mod window;

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::thread::JoinHandle;

use parking_lot::Mutex;

use crate::audio::AudioTap;
use crate::model::SourceKind;
use crate::video::{RgbaImage, SharedImage};

pub use audio_capture::{list_input_devices, list_output_devices};

/// The latest picture of a source, swapped in by its producer.
#[derive(Default)]
pub struct VideoSlot {
    frame: Mutex<Option<SharedImage>>,
    /// The picture before the current one, for [`VideoSlot::recycle`].
    spare: Mutex<Option<SharedImage>>,
}

impl VideoSlot {
    pub fn set(&self, img: RgbaImage) {
        let old = self.frame.lock().replace(Arc::new(img));
        *self.spare.lock() = old;
    }

    /// A `w × h` picture to fill next: the previous picture's memory once nobody holds it any
    /// more (capture sources publish 60 large frames a second), else a new one.
    #[cfg_attr(not(windows), allow(dead_code))]
    pub fn recycle(&self, w: u32, h: u32) -> RgbaImage {
        let mut img = self.recycle_as_is(w, h);
        img.change = None;
        img
    }

    /// [`VideoSlot::recycle`], keeping the old picture's `change` (which says which picture of the
    /// source the memory still holds).
    #[cfg_attr(not(all(windows, feature = "browser")), allow(dead_code))]
    pub fn recycle_as_is(&self, w: u32, h: u32) -> RgbaImage {
        if let Some(a) = self.spare.lock().take() {
            if let Ok(img) = Arc::try_unwrap(a) {
                if img.width == w && img.height == h {
                    return img;
                }
            }
        }
        RgbaImage::new(w, h)
    }
    pub fn clear(&self) {
        *self.frame.lock() = None;
    }
    pub fn get(&self) -> Option<SharedImage> {
        self.frame.lock().clone()
    }
    pub fn size(&self) -> Option<(u32, u32)> {
        self.frame.lock().as_ref().map(|f| f.size())
    }
}

/// State a producer thread shares with the studio.
pub struct SourceShared {
    pub video: VideoSlot,
    pub audio: Option<Arc<AudioTap>>,
    /// Shown on air (in the program scene, visible) — set by the compositor every frame.
    pub active: AtomicBool,
    pub stop: AtomicBool,
    /// Last error, for the properties dialog / source list.
    pub status: Mutex<Option<String>>,
    /// Media sources: transport controls and position.
    pub media: Option<media::MediaControl>,
}

impl SourceShared {
    fn new(audio: Option<Arc<AudioTap>>, media: Option<media::MediaControl>) -> Arc<Self> {
        Arc::new(SourceShared {
            video: VideoSlot::default(),
            audio,
            active: AtomicBool::new(false),
            stop: AtomicBool::new(false),
            status: Mutex::new(None),
            media,
        })
    }

    pub fn stopped(&self) -> bool {
        self.stop.load(Ordering::Relaxed)
    }

    pub fn is_active(&self) -> bool {
        self.active.load(Ordering::Relaxed)
    }

    pub fn set_error(&self, e: impl std::fmt::Display) {
        let msg = e.to_string();
        log::warn!("source: {msg}");
        *self.status.lock() = Some(msg);
    }

    pub fn clear_error(&self) {
        *self.status.lock() = None;
    }

    pub fn error(&self) -> Option<String> {
        self.status.lock().clone()
    }

    /// Sleep up to `d`, returning early (true) when asked to stop.
    pub fn sleep(&self, d: std::time::Duration) -> bool {
        let end = std::time::Instant::now() + d;
        loop {
            if self.stopped() {
                return true;
            }
            let now = std::time::Instant::now();
            if now >= end {
                return false;
            }
            std::thread::sleep((end - now).min(std::time::Duration::from_millis(20)));
        }
    }
}

/// A running source.
pub struct SourceRuntime {
    /// The settings it was started with (a change restarts it).
    pub kind: SourceKind,
    pub shared: Arc<SourceShared>,
    thread: Option<JoinHandle<()>>,
}

impl Drop for SourceRuntime {
    fn drop(&mut self) {
        self.shared.stop.store(true, Ordering::Relaxed);
        if let Some(t) = self.thread.take() {
            // Don't block the UI on a stuck device; the thread exits on its own.
            std::thread::spawn(move || {
                let _ = t.join();
            });
        }
    }
}

impl SourceRuntime {
    pub fn start(kind: &SourceKind) -> SourceRuntime {
        let audio = kind.has_audio().then(|| {
            Arc::new(match kind {
                SourceKind::Media(_) => AudioTap::buffered(),
                _ => AudioTap::live(),
            })
        });
        let media_ctl = matches!(kind, SourceKind::Media(_)).then(media::MediaControl::default);
        let shared = SourceShared::new(audio, media_ctl);
        let s = Arc::clone(&shared);
        let k = kind.clone();
        let name = format!("src-{}", kind.type_name());
        let thread: Option<JoinHandle<()>> = match kind {
            SourceKind::Color { color, width, height } => {
                shared.video.set(RgbaImage::filled((*width).max(1), (*height).max(1), *color));
                None
            }
            SourceKind::Image { .. } => spawn(&name, move || image_file::run(k, s)),
            SourceKind::Text(_) => spawn(&name, move || text::run(k, s)),
            SourceKind::Media(_) => spawn(&name, move || media::run(k, s)),
            #[cfg(windows)]
            SourceKind::Display { .. } => spawn(&name, move || display::run(k, s)),
            #[cfg(windows)]
            SourceKind::Window { .. } => spawn(&name, move || window::run(k, s)),
            #[cfg(windows)]
            SourceKind::Camera { .. } => spawn(&name, move || camera::run(k, s)),
            #[cfg(not(windows))]
            SourceKind::Display { .. } | SourceKind::Window { .. } | SourceKind::Camera { .. } => {
                shared.set_error("screen and camera capture are only implemented on Windows");
                None
            }
            SourceKind::AudioInput { .. } | SourceKind::AudioOutput { .. } => {
                spawn(&name, move || audio_capture::run(k, s))
            }
            #[cfg(windows)]
            SourceKind::AppAudio { .. } => spawn(&name, move || app_audio::run(k, s)),
            #[cfg(feature = "browser")]
            SourceKind::Browser(_) => {
                // CEF starts on the calling (UI) thread, which also shuts it down.
                let _ = browser::ensure_init();
                spawn(&name, move || browser::run(k, s))
            }
            #[cfg(not(feature = "browser"))]
            SourceKind::Browser(_) => {
                shared.set_error("this build has no browser source (the \"browser\" feature is off)");
                None
            }
            #[cfg(not(windows))]
            SourceKind::AppAudio { .. } => {
                shared.set_error("application audio capture is only implemented on Windows");
                None
            }
        };
        SourceRuntime { kind: kind.clone(), shared, thread }
    }
}

fn spawn(name: &str, f: impl FnOnce() + Send + 'static) -> Option<JoinHandle<()>> {
    std::thread::Builder::new()
        .name(name.to_string())
        .spawn(move || {
            if let Err(e) = std::panic::catch_unwind(std::panic::AssertUnwindSafe(f)) {
                let msg = e
                    .downcast_ref::<String>()
                    .cloned()
                    .or_else(|| e.downcast_ref::<&str>().map(|s| s.to_string()))
                    .unwrap_or_else(|| "panic".into());
                log::error!("source thread panicked: {msg}");
            }
        })
        .ok()
}
