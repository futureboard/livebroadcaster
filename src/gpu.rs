//! The wgpu device for the window and preview: graphics API, adapter and present mode from
//! [`PreviewSettings`], applied when the window is created.

use std::sync::Arc;

use eframe::egui_wgpu::{self, wgpu};

use crate::model::{GpuBackend, PreviewSettings};

fn backends(b: GpuBackend) -> wgpu::Backends {
    match b {
        GpuBackend::Auto => wgpu::Backends::PRIMARY,
        // A settings file from Windows may still say DX12. That backend does not exist elsewhere.
        GpuBackend::Dx12 => {
            if cfg!(windows) { wgpu::Backends::DX12 } else { wgpu::Backends::PRIMARY }
        }
        GpuBackend::Vulkan => wgpu::Backends::VULKAN,
        GpuBackend::Gl => wgpu::Backends::GL,
    }
}

/// Discrete before integrated before anything else; on Windows DX12 before Vulkan / GL
/// (flip-model presentation: even frame pacing with vsync, where a windowed Vulkan swapchain
/// is composited by DWM and drops to ~40 fps at 60 Hz).
fn rank(info: &wgpu::AdapterInfo) -> (u8, u8) {
    let device = match info.device_type {
        wgpu::DeviceType::DiscreteGpu => 0,
        wgpu::DeviceType::IntegratedGpu => 1,
        wgpu::DeviceType::VirtualGpu => 2,
        wgpu::DeviceType::Other => 3,
        wgpu::DeviceType::Cpu => 4,
    };
    let api = match info.backend {
        wgpu::Backend::Dx12 if cfg!(windows) => 0,
        wgpu::Backend::Vulkan | wgpu::Backend::Metal => 1,
        _ => 2,
    };
    (device, api)
}

/// A short label for an adapter: "NVIDIA GeForce GTX 1060 3GB · Vulkan".
pub fn label(info: &wgpu::AdapterInfo) -> String {
    format!("{} · {:?}", info.name, info.backend)
}

/// The same GPU under different APIs: OpenGL names it "NVIDIA GeForce GTX 1060 3GB/PCIe/SSE2",
/// DX12 / Vulkan "NVIDIA GeForce GTX 1060 3GB".
fn same_gpu(a: &str, b: &str) -> bool {
    let base = |n: &str| n.split('/').next().unwrap_or(n).trim().to_ascii_lowercase();
    base(a) == base(b)
}

pub fn wgpu_options(p: &PreviewSettings) -> egui_wgpu::WgpuConfiguration {
    let mut setup = egui_wgpu::WgpuSetupCreateNew::default();
    setup.instance_descriptor.backends = backends(p.backend);
    setup.power_preference = wgpu::PowerPreference::HighPerformance;
    let wanted = p.adapter.clone();
    setup.native_adapter_selector = Some(Arc::new(move |adapters: &[wgpu::Adapter], surface: Option<&wgpu::Surface<'_>>| {
        let usable: Vec<&wgpu::Adapter> = adapters.iter().filter(|a| surface.is_none_or(|s| a.is_surface_supported(s))).collect();
        if let Some(name) = &wanted {
            if let Some(a) = usable.iter().filter(|a| same_gpu(&a.get_info().name, name)).min_by_key(|a| rank(&a.get_info())) {
                log::info!("preview GPU (selected): {}", label(&a.get_info()));
                return Ok((*a).clone());
            }
            log::warn!("preview GPU \"{name}\" is not available; picking automatically");
        }
        let best = usable.iter().min_by_key(|a| rank(&a.get_info())).copied().ok_or_else(|| "no GPU can draw to this window".to_string())?;
        log::info!("preview GPU (auto): {}", label(&best.get_info()));
        Ok(best.clone())
    }));
    egui_wgpu::WgpuConfiguration {
        present_mode: if p.vsync { wgpu::PresentMode::AutoVsync } else { wgpu::PresentMode::AutoNoVsync },
        // One queued frame: the preview shows the newest picture, not one from two vsyncs ago.
        desired_maximum_frame_latency: Some(1),
        wgpu_setup: egui_wgpu::WgpuSetup::CreateNew(setup),
        ..Default::default()
    }
}
