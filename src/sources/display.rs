//! Display capture: DXGI Desktop Duplication on the adapter that drives the monitor, copied to a
//! staging texture and read back as RGBA, with the mouse pointer drawn in.

use std::sync::Arc;
use std::time::{Duration, Instant};

use windows::core::Interface;
use windows::Win32::Foundation::HMODULE;
use windows::Win32::Graphics::Direct3D::D3D_DRIVER_TYPE_UNKNOWN;
use windows::Win32::Graphics::Direct3D11::*;
use windows::Win32::Graphics::Dxgi::Common::*;
use windows::Win32::Graphics::Dxgi::*;

use super::SourceShared;
use crate::model::SourceKind;
use crate::video::RgbaImage;

/// A monitor as listed in the properties dialog.
#[derive(Clone, Debug)]
pub struct MonitorInfo {
    pub name: String,
    pub x: i32,
    pub y: i32,
    pub width: u32,
    pub height: u32,
    pub primary: bool,
}

fn outputs() -> windows::core::Result<Vec<(IDXGIAdapter1, IDXGIOutput, MonitorInfo)>> {
    let mut out = Vec::new();
    unsafe {
        let factory: IDXGIFactory1 = CreateDXGIFactory1()?;
        let mut ai = 0;
        while let Ok(adapter) = factory.EnumAdapters1(ai) {
            let mut oi = 0;
            while let Ok(output) = adapter.EnumOutputs(oi) {
                if let Ok(desc) = output.GetDesc() {
                    if desc.AttachedToDesktop.as_bool() {
                        let r = desc.DesktopCoordinates;
                        let dev = String::from_utf16_lossy(&desc.DeviceName);
                        let dev = dev.trim_end_matches('\0').trim_start_matches("\\\\.\\").to_string();
                        out.push((
                            adapter.clone(),
                            output.clone(),
                            MonitorInfo {
                                name: dev,
                                x: r.left,
                                y: r.top,
                                width: (r.right - r.left) as u32,
                                height: (r.bottom - r.top) as u32,
                                primary: r.left == 0 && r.top == 0,
                            },
                        ));
                    }
                }
                oi += 1;
            }
            ai += 1;
        }
    }
    Ok(out)
}

pub fn list_monitors() -> Vec<MonitorInfo> {
    outputs().map(|o| o.into_iter().map(|(_, _, m)| m).collect()).unwrap_or_default()
}

struct Duplicator {
    ctx: ID3D11DeviceContext,
    dupl: IDXGIOutputDuplication,
    staging: Option<(ID3D11Texture2D, u32, u32)>,
    device: ID3D11Device,
}

fn open(monitor: usize) -> Result<(Duplicator, MonitorInfo), String> {
    let list = outputs().map_err(|e| format!("DXGI: {e}"))?;
    let (adapter, output, info) = list.into_iter().nth(monitor).ok_or_else(|| format!("monitor {} not found", monitor + 1))?;
    unsafe {
        let mut device = None;
        let mut ctx = None;
        D3D11CreateDevice(
            &adapter,
            D3D_DRIVER_TYPE_UNKNOWN,
            HMODULE::default(),
            D3D11_CREATE_DEVICE_BGRA_SUPPORT,
            None,
            D3D11_SDK_VERSION,
            Some(&mut device),
            None,
            Some(&mut ctx),
        )
        .map_err(|e| format!("D3D11CreateDevice: {e}"))?;
        let device: ID3D11Device = device.ok_or("no D3D11 device")?;
        let ctx = ctx.ok_or("no D3D11 context")?;
        let output1: IDXGIOutput1 = output.cast().map_err(|e| format!("IDXGIOutput1: {e}"))?;
        let dupl = output1.DuplicateOutput(&device).map_err(|e| format!("DuplicateOutput: {e}"))?;
        Ok((Duplicator { ctx, dupl, staging: None, device }, info))
    }
}

#[derive(Default)]
struct Pointer {
    visible: bool,
    x: i32,
    y: i32,
    shape: Vec<u8>,
    info: DXGI_OUTDUPL_POINTER_SHAPE_INFO,
}

pub fn run(kind: SourceKind, s: Arc<SourceShared>) {
    let SourceKind::Display { monitor, cursor } = kind else { return };
    while !s.stopped() {
        let (mut d, info) = match open(monitor) {
            Ok(v) => v,
            Err(e) => {
                s.set_error(e);
                if s.sleep(Duration::from_secs(2)) {
                    return;
                }
                continue;
            }
        };
        s.clear_error();
        log::info!("display capture: {} {}x{}", info.name, info.width, info.height);
        let mut pointer = Pointer::default();
        let mut desktop: Option<RgbaImage> = None;
        let mut last_publish = Instant::now() - Duration::from_secs(1);
        let interval = Duration::from_millis(1000 / 60);
        let mut dirty = false;
        loop {
            if s.stopped() {
                return;
            }
            match unsafe { d.acquire(&mut pointer, cursor, &mut desktop, last_publish.elapsed() >= interval) } {
                Ok(changed) => dirty |= changed,
                Err(e) => {
                    // ACCESS_LOST: mode change, UAC / lock screen, full-screen switch — re-duplicate.
                    log::debug!("display capture: {e}");
                    break;
                }
            }
            if dirty && last_publish.elapsed() >= interval {
                if let Some(img) = &desktop {
                    let mut frame = s.video.recycle(img.width, img.height);
                    frame.data.copy_from_slice(&img.data);
                    if cursor && pointer.visible {
                        draw_pointer(&mut frame, &pointer);
                    }
                    s.video.set(frame);
                    last_publish = Instant::now();
                    dirty = false;
                }
            }
        }
        if s.sleep(Duration::from_millis(200)) {
            return;
        }
    }
}

impl Duplicator {
    /// Wait up to 20 ms for a desktop update. Returns whether the picture or pointer changed.
    unsafe fn acquire(&mut self, pointer: &mut Pointer, cursor: bool, desktop: &mut Option<RgbaImage>, want_pixels: bool) -> Result<bool, String> {
        let mut fi = DXGI_OUTDUPL_FRAME_INFO::default();
        let mut res: Option<IDXGIResource> = None;
        match self.dupl.AcquireNextFrame(20, &mut fi, &mut res) {
            Ok(()) => {}
            Err(e) if e.code() == DXGI_ERROR_WAIT_TIMEOUT => return Ok(false),
            Err(e) => return Err(format!("AcquireNextFrame: {e}")),
        }
        let mut changed = false;
        let result = (|| -> Result<(), String> {
            if cursor && fi.LastMouseUpdateTime != 0 {
                pointer.visible = fi.PointerPosition.Visible.as_bool();
                pointer.x = fi.PointerPosition.Position.x;
                pointer.y = fi.PointerPosition.Position.y;
                changed = true;
                if fi.PointerShapeBufferSize > 0 {
                    pointer.shape.resize(fi.PointerShapeBufferSize as usize, 0);
                    let mut needed = 0u32;
                    let _ = self.dupl.GetFramePointerShape(
                        fi.PointerShapeBufferSize,
                        pointer.shape.as_mut_ptr() as *mut _,
                        &mut needed,
                        &mut pointer.info,
                    );
                }
            }
            if (fi.LastPresentTime != 0 || desktop.is_none()) && (want_pixels || desktop.is_none()) {
                let Some(res) = res.as_ref() else { return Ok(()) };
                let tex: ID3D11Texture2D = res.cast().map_err(|e| format!("texture: {e}"))?;
                let mut desc = D3D11_TEXTURE2D_DESC::default();
                tex.GetDesc(&mut desc);
                let staging = match &self.staging {
                    Some((t, w, h)) if *w == desc.Width && *h == desc.Height => t.clone(),
                    _ => {
                        let sd = D3D11_TEXTURE2D_DESC {
                            Width: desc.Width,
                            Height: desc.Height,
                            MipLevels: 1,
                            ArraySize: 1,
                            Format: desc.Format,
                            SampleDesc: DXGI_SAMPLE_DESC { Count: 1, Quality: 0 },
                            Usage: D3D11_USAGE_STAGING,
                            BindFlags: 0,
                            CPUAccessFlags: D3D11_CPU_ACCESS_READ.0 as u32,
                            MiscFlags: 0,
                        };
                        let mut t = None;
                        self.device.CreateTexture2D(&sd, None, Some(&mut t)).map_err(|e| format!("staging texture: {e}"))?;
                        let t = t.ok_or("no staging texture")?;
                        self.staging = Some((t.clone(), desc.Width, desc.Height));
                        t
                    }
                };
                self.ctx.CopyResource(&staging, &tex);
                let mut mapped = D3D11_MAPPED_SUBRESOURCE::default();
                self.ctx.Map(&staging, 0, D3D11_MAP_READ, 0, Some(&mut mapped)).map_err(|e| format!("Map: {e}"))?;
                let (w, h) = (desc.Width as usize, desc.Height as usize);
                let img = desktop.get_or_insert_with(|| RgbaImage::new(w as u32, h as u32));
                if img.width as usize != w || img.height as usize != h {
                    *img = RgbaImage::new(w as u32, h as u32);
                }
                let pitch = mapped.RowPitch as usize;
                let src = std::slice::from_raw_parts(mapped.pData as *const u8, pitch * h);
                bgra_to_rgba(src, pitch, &mut img.data, w, h);
                self.ctx.Unmap(&staging, 0);
                changed = true;
            }
            Ok(())
        })();
        let _ = self.dupl.ReleaseFrame();
        result.map(|_| changed)
    }
}

pub fn bgra_to_rgba(src: &[u8], pitch: usize, dst: &mut [u8], w: usize, h: usize) {
    use rayon::prelude::*;
    dst.par_chunks_mut(w * 4).take(h).enumerate().for_each(|(y, row)| {
        let s = &src[y * pitch..y * pitch + w * 4];
        for (d, p) in row.chunks_exact_mut(4).zip(s.chunks_exact(4)) {
            d[0] = p[2];
            d[1] = p[1];
            d[2] = p[0];
            d[3] = 255;
        }
    });
}

fn draw_pointer(img: &mut RgbaImage, p: &Pointer) {
    let (iw, ih) = (img.width as i32, img.height as i32);
    let info = &p.info;
    let pitch = info.Pitch as usize;
    let w = info.Width as i32;
    if p.shape.is_empty() || w == 0 {
        return;
    }
    let ty = info.Type as i32;
    let put = |img: &mut RgbaImage, x: i32, y: i32, f: &mut dyn FnMut(&mut [u8])| {
        if x >= 0 && y >= 0 && x < iw && y < ih {
            let i = (y as usize * iw as usize + x as usize) * 4;
            f(&mut img.data[i..i + 4]);
        }
    };
    if ty == DXGI_OUTDUPL_POINTER_SHAPE_TYPE_MONOCHROME.0 {
        // AND mask then XOR mask, 1 bpp, each `Height / 2` rows.
        let h = info.Height as i32 / 2;
        for y in 0..h {
            for x in 0..w {
                let byte = (x / 8) as usize;
                let bit = 0x80u8 >> (x % 8);
                let and = p.shape.get(y as usize * pitch + byte).is_some_and(|b| b & bit != 0);
                let xor = p.shape.get((y + h) as usize * pitch + byte).is_some_and(|b| b & bit != 0);
                put(img, p.x + x, p.y + y, &mut |px| {
                    for c in &mut px[..3] {
                        let v = if and { *c } else { 0 };
                        *c = if xor { v ^ 0xFF } else { v };
                    }
                });
            }
        }
    } else {
        let masked = ty == DXGI_OUTDUPL_POINTER_SHAPE_TYPE_MASKED_COLOR.0;
        let h = info.Height as i32;
        for y in 0..h {
            for x in 0..w {
                let i = y as usize * pitch + x as usize * 4;
                let Some(s) = p.shape.get(i..i + 4) else { continue };
                let (b, g, r, a) = (s[0], s[1], s[2], s[3]);
                put(img, p.x + x, p.y + y, &mut |px| {
                    if masked {
                        if a == 0 {
                            px[0] = r;
                            px[1] = g;
                            px[2] = b;
                        } else {
                            px[0] ^= r;
                            px[1] ^= g;
                            px[2] ^= b;
                        }
                    } else if a > 0 {
                        let a = a as u32;
                        px[0] = ((r as u32 * a + px[0] as u32 * (255 - a)) / 255) as u8;
                        px[1] = ((g as u32 * a + px[1] as u32 * (255 - a)) / 255) as u8;
                        px[2] = ((b as u32 * a + px[2] as u32 * (255 - a)) / 255) as u8;
                    }
                });
            }
        }
    }
}
