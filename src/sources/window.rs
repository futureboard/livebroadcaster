//! Window capture of the client area: Windows Graphics Capture (see [`super::wgc`]) by default;
//! `PrintWindow(PW_RENDERFULLCONTENT)` into a DIB section as the fallback for old Windows.

use std::sync::Arc;
use std::time::{Duration, Instant};

use windows::core::BOOL;
use windows::Win32::Foundation::{CloseHandle, HWND, LPARAM, POINT, RECT};
use windows::Win32::Graphics::Dwm::{DwmGetWindowAttribute, DWMWA_CLOAKED, DWMWA_EXTENDED_FRAME_BOUNDS};
use windows::Win32::Graphics::Gdi::*;
use windows::Win32::Storage::Xps::{PrintWindow, PRINT_WINDOW_FLAGS};
use windows::Win32::System::Threading::{OpenProcess, QueryFullProcessImageNameW, PROCESS_NAME_FORMAT, PROCESS_QUERY_LIMITED_INFORMATION};
use windows::Win32::UI::WindowsAndMessaging::*;

use super::wgc::WgcCapture;
use super::SourceShared;
use crate::model::{CaptureMethod, SourceKind, WindowTarget};
use crate::video::RgbaImage;

/// A capturable top-level window.
#[derive(Clone, Debug)]
pub struct WindowInfo {
    pub hwnd: isize,
    pub target: WindowTarget,
}

impl WindowInfo {
    pub fn label(&self) -> String {
        format!("[{}] {}", self.target.exe, self.target.title)
    }
}

pub fn list_windows() -> Vec<WindowInfo> {
    let mut out: Vec<WindowInfo> = Vec::new();
    unsafe extern "system" fn cb(hwnd: HWND, lp: LPARAM) -> BOOL {
        let out = &mut *(lp.0 as *mut Vec<WindowInfo>);
        if let Some(info) = describe(hwnd) {
            out.push(info);
        }
        BOOL(1)
    }
    unsafe {
        let _ = EnumWindows(Some(cb), LPARAM(&mut out as *mut _ as isize));
    }
    let me = std::process::id();
    out.retain(|w| unsafe {
        let mut pid = 0u32;
        GetWindowThreadProcessId(HWND(w.hwnd as *mut _), Some(&mut pid));
        pid != me
    });
    out
}

unsafe fn describe(hwnd: HWND) -> Option<WindowInfo> {
    if !IsWindowVisible(hwnd).as_bool() {
        return None;
    }
    let ex = GetWindowLongW(hwnd, GWL_EXSTYLE) as u32;
    if ex & WS_EX_TOOLWINDOW.0 != 0 {
        return None;
    }
    if GetWindow(hwnd, GW_OWNER).is_ok_and(|o| !o.0.is_null()) {
        return None;
    }
    let mut cloaked = 0u32;
    if DwmGetWindowAttribute(hwnd, DWMWA_CLOAKED, &mut cloaked as *mut _ as *mut _, 4).is_ok() && cloaked != 0 {
        return None;
    }
    let mut title = [0u16; 512];
    let n = GetWindowTextW(hwnd, &mut title);
    if n <= 0 {
        return None;
    }
    let title = String::from_utf16_lossy(&title[..n as usize]);
    let mut class = [0u16; 256];
    let n = GetClassNameW(hwnd, &mut class);
    let class = String::from_utf16_lossy(&class[..n.max(0) as usize]);
    if class == "Progman" || class == "Windows.UI.Core.CoreWindow" {
        return None;
    }
    let mut pid = 0u32;
    GetWindowThreadProcessId(hwnd, Some(&mut pid));
    let exe = process_name(pid).unwrap_or_default();
    Some(WindowInfo { hwnd: hwnd.0 as isize, target: WindowTarget { title, class, exe } })
}

unsafe fn process_name(pid: u32) -> Option<String> {
    let h = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid).ok()?;
    let mut buf = [0u16; 1024];
    let mut len = buf.len() as u32;
    let ok = QueryFullProcessImageNameW(h, PROCESS_NAME_FORMAT(0), windows::core::PWSTR(buf.as_mut_ptr()), &mut len).is_ok();
    let _ = CloseHandle(h);
    if !ok {
        return None;
    }
    let path = String::from_utf16_lossy(&buf[..len as usize]);
    path.rsplit(['\\', '/']).next().map(str::to_string)
}

/// Find the window again: exact title first, then same executable + class (titles change, e.g.
/// a browser tab).
fn find(target: &WindowTarget) -> Option<HWND> {
    let all = list_windows();
    all.iter()
        .find(|w| w.target.title == target.title && w.target.exe == target.exe)
        .or_else(|| all.iter().find(|w| w.target.exe == target.exe && w.target.class == target.class && !target.exe.is_empty()))
        .map(|w| HWND(w.hwnd as *mut _))
}

pub fn run(kind: SourceKind, s: Arc<SourceShared>) {
    let SourceKind::Window { target, cursor, method } = kind else { return };
    if target.title.is_empty() && target.exe.is_empty() {
        s.set_error("no window selected");
        return;
    }
    let use_wgc = match method {
        CaptureMethod::Gdi => false,
        CaptureMethod::Wgc => true,
        CaptureMethod::Auto => super::wgc::supported(),
    };
    let mut hwnd: Option<HWND> = None;
    let mut last_lookup = Instant::now() - Duration::from_secs(10);
    let mut gdi = Gdi::default();
    let mut wgc: Option<WgcCapture> = None;
    // WGC failed for this window (e.g. protected content): GDI until the window changes.
    let mut wgc_failed = false;
    let gdi_interval = Duration::from_millis(33);
    let mut fps = (Instant::now(), 0u32);
    while !s.stopped() {
        let t0 = Instant::now();
        let alive = hwnd.is_some_and(|h| unsafe { IsWindow(Some(h)).as_bool() });
        if !alive {
            hwnd = None;
            wgc = None;
            if last_lookup.elapsed() > Duration::from_secs(2) {
                hwnd = find(&target);
                last_lookup = Instant::now();
                wgc_failed = false;
                if hwnd.is_none() {
                    s.set_error(format!("window not found: {}", target.title));
                    s.video.clear();
                }
            }
        }
        let Some(h) = hwnd else {
            if s.sleep(Duration::from_millis(200)) {
                return;
            }
            continue;
        };
        if use_wgc && !wgc_failed {
            if wgc.is_none() {
                match WgcCapture::window(h, cursor) {
                    Ok(c) => {
                        log::info!("window capture: Windows Graphics Capture of \"{}\"", target.title);
                        wgc = Some(c);
                    }
                    Err(e) => {
                        log::warn!("window capture: {e}; using GDI");
                        wgc_failed = method == CaptureMethod::Auto;
                        if !wgc_failed {
                            s.set_error(e);
                            if s.sleep(Duration::from_secs(2)) {
                                return;
                            }
                            continue;
                        }
                    }
                }
            }
            if let Some(c) = wgc.as_mut() {
                // Frames come at the window's own rate; nothing new while it's minimised or idle.
                if !c.wait(Duration::from_millis(100)) {
                    continue;
                }
                let mut img = std::mem::take(&mut gdi.scratch);
                match c.grab(|fw, fh| client_region(h, fw, fh), &mut img) {
                    Ok(true) => {
                        let mut out = s.video.recycle(img.width, img.height);
                        std::mem::swap(&mut out, &mut img);
                        s.video.set(out);
                        s.clear_error();
                        fps.1 += 1;
                    }
                    Ok(false) => {}
                    Err(e) => {
                        log::warn!("window capture: {e}");
                        wgc = None;
                    }
                }
                gdi.scratch = img;
                if fps.0.elapsed() >= Duration::from_secs(5) {
                    log::debug!("window capture: {:.1} fps", fps.1 as f32 / fps.0.elapsed().as_secs_f32());
                    fps = (Instant::now(), 0);
                }
                continue;
            }
        }
        match unsafe { gdi.capture(h, cursor, &s) } {
            Ok(Some(img)) => {
                s.video.set(img);
                s.clear_error();
            }
            Ok(None) => {}
            Err(e) => s.set_error(e),
        }
        let spent = t0.elapsed();
        if s.sleep(gdi_interval.saturating_sub(spent).max(Duration::from_millis(5))) {
            return;
        }
    }
}

/// The client area inside a captured frame of `fw × fh` (WGC frames are the window's visible
/// bounds: title bar and borders included).
fn client_region(hwnd: HWND, fw: u32, fh: u32) -> super::wgc::Region {
    unsafe {
        let mut frame = RECT::default();
        let mut client = RECT::default();
        let mut origin = POINT::default();
        let ok = DwmGetWindowAttribute(hwnd, DWMWA_EXTENDED_FRAME_BOUNDS, &mut frame as *mut _ as *mut _, std::mem::size_of::<RECT>() as u32).is_ok()
            && GetClientRect(hwnd, &mut client).is_ok()
            && ClientToScreen(hwnd, &mut origin).as_bool();
        if !ok {
            return (0, 0, fw, fh);
        }
        let x = (origin.x - frame.left).max(0) as u32;
        let y = (origin.y - frame.top).max(0) as u32;
        let w = (client.right - client.left).max(0) as u32;
        let h = (client.bottom - client.top).max(0) as u32;
        if w == 0 || h == 0 {
            return (0, 0, fw, fh);
        }
        (x, y, w, h)
    }
}

#[derive(Default)]
struct Gdi {
    dc: Option<HDC>,
    bmp: Option<(HBITMAP, *mut u8, i32, i32)>,
    /// A picture buffer kept between frames.
    scratch: RgbaImage,
}

// The DC and bitmap live on the capture thread only.
unsafe impl Send for Gdi {}

impl Drop for Gdi {
    fn drop(&mut self) {
        unsafe {
            if let Some((b, ..)) = self.bmp.take() {
                let _ = DeleteObject(b.into());
            }
            if let Some(dc) = self.dc.take() {
                let _ = DeleteDC(dc);
            }
        }
    }
}

impl Gdi {
    unsafe fn capture(&mut self, hwnd: HWND, cursor: bool, s: &SourceShared) -> Result<Option<RgbaImage>, String> {
        if IsIconic(hwnd).as_bool() {
            return Ok(None); // minimised: keep the last picture
        }
        let mut rc = RECT::default();
        GetClientRect(hwnd, &mut rc).map_err(|e| format!("GetClientRect: {e}"))?;
        let (w, h) = (rc.right - rc.left, rc.bottom - rc.top);
        if w <= 0 || h <= 0 {
            return Ok(None);
        }
        if self.dc.is_none() {
            let screen = GetDC(None);
            self.dc = Some(CreateCompatibleDC(Some(screen)));
            ReleaseDC(None, screen);
        }
        let dc = self.dc.unwrap();
        if !matches!(self.bmp, Some((_, _, bw, bh)) if bw == w && bh == h) {
            if let Some((b, ..)) = self.bmp.take() {
                let _ = DeleteObject(b.into());
            }
            let bmi = BITMAPINFO {
                bmiHeader: BITMAPINFOHEADER {
                    biSize: std::mem::size_of::<BITMAPINFOHEADER>() as u32,
                    biWidth: w,
                    biHeight: -h,
                    biPlanes: 1,
                    biBitCount: 32,
                    biCompression: BI_RGB.0,
                    ..Default::default()
                },
                ..Default::default()
            };
            let mut bits: *mut std::ffi::c_void = std::ptr::null_mut();
            let bmp = CreateDIBSection(Some(dc), &bmi, DIB_RGB_COLORS, &mut bits, None, 0).map_err(|e| format!("CreateDIBSection: {e}"))?;
            SelectObject(dc, bmp.into());
            self.bmp = Some((bmp, bits as *mut u8, w, h));
        }
        let (_, bits, _, _) = self.bmp.unwrap();
        // PW_CLIENTONLY | PW_RENDERFULLCONTENT
        if !PrintWindow(hwnd, dc, PRINT_WINDOW_FLAGS(1 | 2)).as_bool() {
            return Err("PrintWindow failed".into());
        }
        let _ = GdiFlush();
        if cursor {
            draw_cursor(dc, hwnd);
        }
        let (wu, hu) = (w as usize, h as usize);
        let src = std::slice::from_raw_parts(bits, wu * hu * 4);
        let mut img = s.video.recycle(w as u32, h as u32);
        super::display::bgra_to_rgba(src, wu * 4, &mut img.data, wu, hu);
        Ok(Some(img))
    }
}

unsafe fn draw_cursor(dc: HDC, hwnd: HWND) {
    let mut ci = CURSORINFO { cbSize: std::mem::size_of::<CURSORINFO>() as u32, ..Default::default() };
    if GetCursorInfo(&mut ci).is_err() || ci.flags.0 & CURSOR_SHOWING.0 == 0 {
        return;
    }
    let mut p = POINT { x: ci.ptScreenPos.x, y: ci.ptScreenPos.y };
    let _ = ScreenToClient(hwnd, &mut p);
    let mut ii = ICONINFO::default();
    if GetIconInfo(ci.hCursor.into(), &mut ii).is_ok() {
        let (hx, hy) = (ii.xHotspot as i32, ii.yHotspot as i32);
        if !ii.hbmColor.is_invalid() {
            let _ = DeleteObject(ii.hbmColor.into());
        }
        if !ii.hbmMask.is_invalid() {
            let _ = DeleteObject(ii.hbmMask.into());
        }
        let _ = DrawIconEx(dc, p.x - hx, p.y - hy, ci.hCursor.into(), 0, 0, 0, None, DI_NORMAL);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Captures a desktop window both ways and prints the cost per frame.
    /// `cargo test --release -- --ignored window_capture_methods --nocapture`
    #[test]
    #[ignore]
    fn window_capture_methods() {
        let want = std::env::var("LB_TEST_WINDOW").unwrap_or_else(|_| "notepad.exe".into());
        let not_minimised = |w: &WindowInfo| unsafe { !IsIconic(HWND(w.hwnd as *mut _)).as_bool() };
        let all = list_windows();
        for w in &all {
            eprintln!("  {} minimised={}", w.label(), !not_minimised(w));
        }
        let Some(w) = all.into_iter().filter(not_minimised).find(|w| w.target.exe.eq_ignore_ascii_case(&want) || want == "*") else {
            eprintln!("no {want} window; skipped");
            return;
        };
        let hwnd = HWND(w.hwnd as *mut _);
        eprintln!("window: {}", w.label());
        let s = super::super::SourceShared::new(None, None);
        let mut gdi = Gdi::default();
        let t = Instant::now();
        let mut size = (0, 0);
        for _ in 0..20 {
            let img = unsafe { gdi.capture(hwnd, false, &s) }.unwrap().unwrap();
            size = (img.width, img.height);
        }
        eprintln!("GDI PrintWindow: {:.2} ms/frame, {size:?}", t.elapsed().as_secs_f32() * 1000.0 / 20.0);
        let mut c = WgcCapture::window(hwnd, false).expect("WGC");
        assert!(c.wait(Duration::from_secs(2)), "no WGC frame");
        let mut img = RgbaImage::default();
        let t = Instant::now();
        assert!(c.grab(|fw, fh| client_region(hwnd, fw, fh), &mut img).unwrap());
        eprintln!("WGC grab: {:.2} ms, {:?}", t.elapsed().as_secs_f32() * 1000.0, (img.width, img.height));
        assert_eq!((img.width, img.height), size, "client area differs between methods");
    }
}
