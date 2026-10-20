//! Windows.Graphics.Capture: the picture DWM composes for a window — GPU-rendered content
//! (browsers, games, DirectComposition) included, at the window's own frame rate, without asking
//! the window to repaint (unlike `PrintWindow`). Frames are copied to a staging texture and read
//! back as RGBA.

use std::sync::Arc;
use std::time::Duration;

use parking_lot::{Condvar, Mutex};
use windows::core::Interface;
use windows::Foundation::TypedEventHandler;
use windows::Graphics::Capture::{Direct3D11CaptureFramePool, GraphicsCaptureItem, GraphicsCaptureSession};
use windows::Graphics::DirectX::Direct3D11::IDirect3DDevice;
use windows::Graphics::DirectX::DirectXPixelFormat;
use windows::Graphics::SizeInt32;
use windows::Win32::Foundation::{HMODULE, HWND};
use windows::Win32::Graphics::Direct3D::D3D_DRIVER_TYPE_HARDWARE;
use windows::Win32::Graphics::Direct3D11::*;
use windows::Win32::Graphics::Dxgi::Common::DXGI_SAMPLE_DESC;
use windows::Win32::Graphics::Dxgi::IDXGIDevice;
use windows::Win32::System::WinRT::Direct3D11::{CreateDirect3D11DeviceFromDXGIDevice, IDirect3DDxgiInterfaceAccess};
use windows::Win32::System::WinRT::Graphics::Capture::IGraphicsCaptureItemInterop;
use windows::Win32::System::WinRT::{RoInitialize, RO_INIT_MULTITHREADED};

use crate::video::RgbaImage;

/// Whether this Windows has the capture API (Windows 10 1803+).
pub fn supported() -> bool {
    unsafe {
        let _ = RoInitialize(RO_INIT_MULTITHREADED);
    }
    GraphicsCaptureSession::IsSupported().unwrap_or(false)
}

/// A pixel rectangle: x, y, width, height.
pub type Region = (u32, u32, u32, u32);

pub struct WgcCapture {
    device: ID3D11Device,
    ctx: ID3D11DeviceContext,
    d3d: IDirect3DDevice,
    pool: Direct3D11CaptureFramePool,
    session: GraphicsCaptureSession,
    pool_size: SizeInt32,
    staging: Option<(ID3D11Texture2D, u32, u32)>,
    arrived: Arc<(Mutex<bool>, Condvar)>,
    token: i64,
}

// The COM objects are free-threaded (MTA, free-threaded frame pool); the capture thread owns it.
unsafe impl Send for WgcCapture {}

const FORMAT: DirectXPixelFormat = DirectXPixelFormat::B8G8R8A8UIntNormalized;

fn err(what: &str) -> impl Fn(windows::core::Error) -> String + '_ {
    move |e| format!("{what}: {e}")
}

impl WgcCapture {
    pub fn window(hwnd: HWND, cursor: bool) -> Result<WgcCapture, String> {
        unsafe {
            let _ = RoInitialize(RO_INIT_MULTITHREADED);
            let interop = windows::core::factory::<GraphicsCaptureItem, IGraphicsCaptureItemInterop>().map_err(err("capture interop"))?;
            let item: GraphicsCaptureItem = interop.CreateForWindow(hwnd).map_err(err("capture item"))?;
            Self::start(item, cursor)
        }
    }

    unsafe fn start(item: GraphicsCaptureItem, cursor: bool) -> Result<WgcCapture, String> {
        let mut device = None;
        let mut ctx = None;
        D3D11CreateDevice(
            None,
            D3D_DRIVER_TYPE_HARDWARE,
            HMODULE::default(),
            D3D11_CREATE_DEVICE_BGRA_SUPPORT,
            None,
            D3D11_SDK_VERSION,
            Some(&mut device),
            None,
            Some(&mut ctx),
        )
        .map_err(err("D3D11CreateDevice"))?;
        let device: ID3D11Device = device.ok_or("no D3D11 device")?;
        let ctx = ctx.ok_or("no D3D11 context")?;
        let dxgi: IDXGIDevice = device.cast().map_err(err("IDXGIDevice"))?;
        let d3d: IDirect3DDevice = CreateDirect3D11DeviceFromDXGIDevice(&dxgi).and_then(|i| i.cast()).map_err(err("WinRT D3D device"))?;
        let size = item.Size().map_err(err("item size"))?;
        let pool = Direct3D11CaptureFramePool::CreateFreeThreaded(&d3d, FORMAT, 2, size).map_err(err("frame pool"))?;
        let arrived = Arc::new((Mutex::new(false), Condvar::new()));
        let a = Arc::clone(&arrived);
        let token = pool
            .FrameArrived(&TypedEventHandler::new(move |_, _| {
                *a.0.lock() = true;
                a.1.notify_one();
                Ok(())
            }))
            .map_err(err("FrameArrived"))?;
        let session = pool.CreateCaptureSession(&item).map_err(err("capture session"))?;
        let _ = session.SetIsCursorCaptureEnabled(cursor);
        // No yellow border (Windows 11; older versions just keep it).
        let _ = session.SetIsBorderRequired(false);
        session.StartCapture().map_err(err("StartCapture"))?;
        Ok(WgcCapture { device, ctx, d3d, pool, session, pool_size: size, staging: None, arrived, token })
    }

    /// Wait up to `timeout` for a new frame; true when one arrived.
    pub fn wait(&self, timeout: Duration) -> bool {
        let (lock, cv) = &*self.arrived;
        let mut ready = lock.lock();
        if !*ready {
            cv.wait_for(&mut ready, timeout);
        }
        std::mem::replace(&mut *ready, false)
    }

    /// Copy the newest frame (the part `region` picks from the frame's size) into `out`.
    /// Returns false when no frame was waiting.
    pub fn grab(&mut self, region: impl Fn(u32, u32) -> Region, out: &mut RgbaImage) -> Result<bool, String> {
        let mut newest = None;
        while let Ok(f) = self.pool.TryGetNextFrame() {
            if let Some(old) = newest.replace(f) {
                let _ = old.Close();
            }
        }
        let Some(frame) = newest else { return Ok(false) };
        let r = unsafe { self.copy(&frame, region, out) };
        let content = frame.ContentSize().ok();
        let _ = frame.Close();
        // The window was resized: frames now come at the new size.
        if let Some(c) = content {
            if (c.Width != self.pool_size.Width || c.Height != self.pool_size.Height) && c.Width > 0 && c.Height > 0 {
                self.pool.Recreate(&self.d3d, FORMAT, 2, c).map_err(err("Recreate"))?;
                self.pool_size = c;
            }
        }
        r
    }

    unsafe fn copy(&mut self, frame: &windows::Graphics::Capture::Direct3D11CaptureFrame, region: impl Fn(u32, u32) -> Region, out: &mut RgbaImage) -> Result<bool, String> {
        let access: IDirect3DDxgiInterfaceAccess = frame.Surface().and_then(|s| s.cast()).map_err(err("frame surface"))?;
        let tex: ID3D11Texture2D = access.GetInterface().map_err(err("frame texture"))?;
        let mut desc = D3D11_TEXTURE2D_DESC::default();
        tex.GetDesc(&mut desc);
        let content = frame.ContentSize().map_err(err("ContentSize"))?;
        let (fw, fh) = ((content.Width.max(0) as u32).min(desc.Width), (content.Height.max(0) as u32).min(desc.Height));
        let (x, y, w, h) = region(fw, fh);
        let (x, y) = (x.min(fw.saturating_sub(1)), y.min(fh.saturating_sub(1)));
        let (w, h) = (w.min(fw - x), h.min(fh - y));
        if w == 0 || h == 0 {
            return Ok(false);
        }
        let staging = match &self.staging {
            Some((t, sw, sh)) if *sw == w && *sh == h => t.clone(),
            _ => {
                let sd = D3D11_TEXTURE2D_DESC {
                    Width: w,
                    Height: h,
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
                self.device.CreateTexture2D(&sd, None, Some(&mut t)).map_err(err("staging texture"))?;
                let t = t.ok_or("no staging texture")?;
                self.staging = Some((t.clone(), w, h));
                t
            }
        };
        let bx = D3D11_BOX { left: x, top: y, front: 0, right: x + w, bottom: y + h, back: 1 };
        self.ctx.CopySubresourceRegion(&staging, 0, 0, 0, 0, &tex, 0, Some(&bx));
        let mut mapped = D3D11_MAPPED_SUBRESOURCE::default();
        self.ctx.Map(&staging, 0, D3D11_MAP_READ, 0, Some(&mut mapped)).map_err(err("Map"))?;
        if out.width != w || out.height != h {
            *out = RgbaImage::new(w, h);
        }
        let pitch = mapped.RowPitch as usize;
        let src = std::slice::from_raw_parts(mapped.pData as *const u8, pitch * h as usize);
        super::display::bgra_to_rgba(src, pitch, &mut out.data, w as usize, h as usize);
        self.ctx.Unmap(&staging, 0);
        Ok(true)
    }
}

impl Drop for WgcCapture {
    fn drop(&mut self) {
        let _ = self.pool.RemoveFrameArrived(self.token);
        let _ = self.session.Close();
        let _ = self.pool.Close();
    }
}
