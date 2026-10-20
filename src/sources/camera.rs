//! Video capture devices (webcams, capture cards) through the Media Foundation source reader,
//! which decodes MJPEG / converts YUY2 / NV12 to RGB32 for us.

use std::sync::Arc;
use std::time::Duration;

use windows::core::{Interface, GUID, PWSTR};
use windows::Win32::Media::MediaFoundation::*;
use windows::Win32::System::Com::{CoInitializeEx, CoTaskMemFree, CoUninitialize, COINIT_MULTITHREADED};

use super::SourceShared;
use crate::model::{CameraFormat, SourceKind};
use crate::video::RgbaImage;

const MF_VERSION: u32 = 0x0002_0070;

/// Formats seen per device (symbolic link). A running capture fills it, since a webcam in use
/// usually can't be opened a second time just to list its formats.
pub static FORMAT_CACHE: std::sync::Mutex<Option<std::collections::HashMap<String, Vec<CameraFormat>>>> = std::sync::Mutex::new(None);

fn remember_formats(link: &str, mut f: Vec<CameraFormat>) {
    f.sort_by_key(|f| std::cmp::Reverse((f.width * f.height, f.fps_num / f.fps_den.max(1))));
    f.dedup();
    if let Ok(mut g) = FORMAT_CACHE.lock() {
        g.get_or_insert_with(Default::default).insert(link.to_string(), f);
    }
}

pub fn cached_formats(link: &str) -> Option<Vec<CameraFormat>> {
    FORMAT_CACHE.lock().ok()?.as_ref()?.get(link).cloned()
}
const FIRST_VIDEO_STREAM: u32 = 0xFFFF_FFFC;

#[derive(Clone, Debug)]
pub struct CameraInfo {
    pub name: String,
    pub link: String,
}

struct ComGuard;
impl ComGuard {
    fn new() -> Self {
        unsafe {
            let _ = CoInitializeEx(None, COINIT_MULTITHREADED);
            let _ = MFStartup(MF_VERSION, MFSTARTUP_FULL);
        }
        ComGuard
    }
}
impl Drop for ComGuard {
    fn drop(&mut self) {
        unsafe {
            let _ = MFShutdown();
            CoUninitialize();
        }
    }
}

unsafe fn devices() -> windows::core::Result<Vec<IMFActivate>> {
    let mut attrs: Option<IMFAttributes> = None;
    MFCreateAttributes(&mut attrs, 1)?;
    let attrs = attrs.unwrap();
    attrs.SetGUID(&MF_DEVSOURCE_ATTRIBUTE_SOURCE_TYPE, &MF_DEVSOURCE_ATTRIBUTE_SOURCE_TYPE_VIDCAP_GUID)?;
    let mut list: *mut Option<IMFActivate> = std::ptr::null_mut();
    let mut count = 0u32;
    MFEnumDeviceSources(&attrs, &mut list, &mut count)?;
    let mut out = Vec::new();
    if !list.is_null() {
        for i in 0..count as usize {
            if let Some(a) = (*list.add(i)).take() {
                out.push(a);
            }
        }
        CoTaskMemFree(Some(list as *const _));
    }
    Ok(out)
}

unsafe fn string_attr(a: &IMFActivate, key: &GUID) -> String {
    let mut p = PWSTR::null();
    let mut len = 0u32;
    if a.GetAllocatedString(key, &mut p, &mut len).is_ok() && !p.is_null() {
        let s = p.to_string().unwrap_or_default();
        CoTaskMemFree(Some(p.0 as *const _));
        s
    } else {
        String::new()
    }
}

pub fn list_cameras() -> Vec<CameraInfo> {
    let _com = ComGuard::new();
    unsafe {
        devices()
            .unwrap_or_default()
            .iter()
            .map(|d| CameraInfo {
                name: string_attr(d, &MF_DEVSOURCE_ATTRIBUTE_FRIENDLY_NAME),
                link: string_attr(d, &MF_DEVSOURCE_ATTRIBUTE_SOURCE_TYPE_VIDCAP_SYMBOLIC_LINK),
            })
            .collect()
    }
}

unsafe fn open_reader(link: &str) -> Result<IMFSourceReader, String> {
    let dev = devices()
        .map_err(|e| format!("enumerate cameras: {e}"))?
        .into_iter()
        .find(|d| link.is_empty() || string_attr(d, &MF_DEVSOURCE_ATTRIBUTE_SOURCE_TYPE_VIDCAP_SYMBOLIC_LINK) == link)
        .ok_or("camera not found (unplugged?)")?;
    let source: IMFMediaSource = dev.ActivateObject().map_err(|e| format!("activate camera: {e}"))?;
    let mut attrs: Option<IMFAttributes> = None;
    MFCreateAttributes(&mut attrs, 2).map_err(|e| e.to_string())?;
    let attrs = attrs.unwrap();
    let _ = attrs.SetUINT32(&MF_SOURCE_READER_ENABLE_VIDEO_PROCESSING, 1);
    let _ = attrs.SetUINT32(&MF_READWRITE_DISABLE_CONVERTERS, 0);
    MFCreateSourceReaderFromMediaSource(&source, &attrs).map_err(|e| format!("source reader: {e}"))
}

unsafe fn native_types(reader: &IMFSourceReader) -> Vec<(IMFMediaType, CameraFormat)> {
    let mut out = Vec::new();
    let mut i = 0;
    while let Ok(t) = reader.GetNativeMediaType(FIRST_VIDEO_STREAM, i) {
        i += 1;
        let size = t.GetUINT64(&MF_MT_FRAME_SIZE).unwrap_or(0);
        let rate = t.GetUINT64(&MF_MT_FRAME_RATE).unwrap_or(30u64 << 32 | 1);
        let f = CameraFormat {
            width: (size >> 32) as u32,
            height: size as u32,
            fps_num: (rate >> 32) as u32,
            fps_den: (rate as u32).max(1),
        };
        if f.width > 0 && f.height > 0 {
            out.push((t, f));
        }
    }
    out
}

pub fn run(kind: SourceKind, s: Arc<SourceShared>) {
    let SourceKind::Camera { device, format, .. } = kind else { return };
    let _com = ComGuard::new();
    while !s.stopped() {
        if let Err(e) = unsafe { capture(&device, format, &s) } {
            s.set_error(e);
        }
        if s.sleep(Duration::from_secs(2)) {
            return;
        }
    }
}

unsafe fn capture(link: &str, want: Option<CameraFormat>, s: &SourceShared) -> Result<(), String> {
    let reader = open_reader(link)?;
    let types = native_types(&reader);
    remember_formats(link, types.iter().map(|(_, f)| *f).collect());
    let pick = match want {
        Some(w) => types.iter().find(|(_, f)| *f == w).or_else(|| types.iter().find(|(_, f)| f.width == w.width && f.height == w.height)),
        None => None,
    }
    .or_else(|| {
        // Largest picture up to 1080p, then the highest frame rate.
        types
            .iter()
            .filter(|(_, f)| f.width <= 1920 && f.height <= 1080)
            .max_by_key(|(_, f)| (f.width * f.height, f.fps_num / f.fps_den.max(1)))
    })
    .or(types.first());
    if let Some((t, f)) = pick {
        reader.SetCurrentMediaType(FIRST_VIDEO_STREAM, None, t).map_err(|e| format!("set camera format {}x{}: {e}", f.width, f.height))?;
    }
    let out = MFCreateMediaType().map_err(|e| e.to_string())?;
    out.SetGUID(&MF_MT_MAJOR_TYPE, &MFMediaType_Video).map_err(|e| e.to_string())?;
    out.SetGUID(&MF_MT_SUBTYPE, &MFVideoFormat_RGB32).map_err(|e| e.to_string())?;
    reader.SetCurrentMediaType(FIRST_VIDEO_STREAM, None, &out).map_err(|e| format!("RGB32 output: {e}"))?;
    let cur = reader.GetCurrentMediaType(FIRST_VIDEO_STREAM).map_err(|e| e.to_string())?;
    let size = cur.GetUINT64(&MF_MT_FRAME_SIZE).map_err(|e| e.to_string())?;
    let (w, h) = ((size >> 32) as usize, (size as u32) as usize);
    let default_stride = cur.GetUINT32(&MF_MT_DEFAULT_STRIDE).map(|v| v as i32).unwrap_or(w as i32 * 4);
    log::info!("camera: {w}x{h} RGB32 (stride {default_stride})");
    s.clear_error();

    while !s.stopped() {
        let mut flags = 0u32;
        let mut sample: Option<IMFSample> = None;
        reader
            .ReadSample(FIRST_VIDEO_STREAM, 0, None, Some(&mut flags), None, Some(&mut sample))
            .map_err(|e| format!("ReadSample: {e}"))?;
        if flags & (MF_SOURCE_READERF_ERROR.0 as u32 | MF_SOURCE_READERF_ENDOFSTREAM.0 as u32) != 0 {
            return Err("camera stream ended".into());
        }
        let Some(sample) = sample else { continue };
        let buf = sample.ConvertToContiguousBuffer().map_err(|e| e.to_string())?;
        let mut img = RgbaImage::new(w as u32, h as u32);
        if let Ok(b2) = buf.cast::<IMF2DBuffer>() {
            let mut line0: *mut u8 = std::ptr::null_mut();
            let mut pitch = 0i32;
            if b2.Lock2D(&mut line0, &mut pitch).is_ok() {
                copy_rows(line0, pitch as isize, &mut img, w, h);
                let _ = b2.Unlock2D();
                s.video.set(img);
                continue;
            }
        }
        let mut p: *mut u8 = std::ptr::null_mut();
        let mut len = 0u32;
        buf.Lock(&mut p, None, Some(&mut len)).map_err(|e| e.to_string())?;
        let stride = default_stride as isize;
        let start = if stride < 0 { p.offset(-stride * (h as isize - 1)) } else { p };
        if (len as usize) >= stride.unsigned_abs() * h {
            copy_rows(start, stride, &mut img, w, h);
        }
        let _ = buf.Unlock();
        s.video.set(img);
    }
    Ok(())
}

/// BGRX rows starting at `line0`, `pitch` bytes apart (negative = bottom-up) → RGBA.
unsafe fn copy_rows(line0: *const u8, pitch: isize, img: &mut RgbaImage, w: usize, h: usize) {
    for y in 0..h {
        let row = std::slice::from_raw_parts(line0.offset(pitch * y as isize), w * 4);
        let dst = &mut img.data[y * w * 4..(y + 1) * w * 4];
        for (d, p) in dst.chunks_exact_mut(4).zip(row.chunks_exact(4)) {
            d[0] = p[2];
            d[1] = p[1];
            d[2] = p[0];
            d[3] = 255;
        }
    }
}
