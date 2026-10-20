//! Image source: a still picture (PNG / JPEG / BMP / GIF / WebP), reloaded when the file changes.

use std::sync::Arc;
use std::time::{Duration, SystemTime};

use super::SourceShared;
use crate::model::SourceKind;
use crate::video::RgbaImage;

pub fn run(kind: SourceKind, s: Arc<SourceShared>) {
    let SourceKind::Image { path } = kind else { return };
    if path.is_empty() {
        s.set_error("no image file selected");
        return;
    }
    let mut loaded: Option<SystemTime> = None;
    loop {
        let mtime = std::fs::metadata(&path).and_then(|m| m.modified()).ok();
        if mtime.is_some() && mtime != loaded {
            match image::open(&path) {
                Ok(img) => {
                    let rgba = img.to_rgba8();
                    let (width, height) = rgba.dimensions();
                    s.video.set(RgbaImage::from_data(width, height, rgba.into_raw()));
                    s.clear_error();
                }
                Err(e) => s.set_error(format!("{path}: {e}")),
            }
            loaded = mtime;
        } else if mtime.is_none() {
            s.set_error(format!("{path}: file not found"));
        }
        if s.sleep(Duration::from_secs(1)) {
            return;
        }
    }
}
