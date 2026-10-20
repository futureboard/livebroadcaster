//! LiveBroadcaster — an OBS-style live streaming and recording studio on egui / wgpu, built on
//! MediaKit Foundation (MKFF): MKFF decodes media sources and encodes / muxes the output.
#![cfg_attr(all(windows, not(debug_assertions)), windows_subsystem = "windows")]

#[macro_use]
mod i18n;
mod audio;
mod config;
mod engine;
mod gpu;
mod gpu_compositor;
mod model;
mod monitor;
mod output;
mod sources;
#[cfg(test)]
mod tests;
mod ui;
mod video;

use eframe::egui;

fn main() -> eframe::Result<()> {
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("warn,live_broadcaster=info")).init();
    // The renderer's GPU / API / vsync are fixed when the window is created.
    let preview = config::load_settings().preview;
    match run(&preview) {
        Err(e) if preview != model::PreviewSettings::default() => {
            // A GPU / API that can't drive this window must not lock the user out: go back to
            // automatic selection, save that, and start again.
            log::error!("could not start with preview GPU {:?} / {:?}: {e}; falling back to Auto", preview.adapter, preview.backend);
            let mut s = config::load_settings();
            s.preview = model::PreviewSettings { vsync: preview.vsync, ..Default::default() };
            let _ = config::save_settings(&s);
            // winit allows one event loop per process: relaunch rather than retry here.
            if let Ok(exe) = std::env::current_exe() {
                let _ = std::process::Command::new(exe).spawn();
            }
            Ok(())
        }
        r => r,
    }
}

fn run(preview: &model::PreviewSettings) -> eframe::Result<()> {
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_title("LiveBroadcaster")
            .with_app_id("live-broadcaster")
            .with_inner_size([1440.0, 900.0])
            .with_min_inner_size([960.0, 600.0])
            .with_icon(ui::app_icon()),
        persist_window: true,
        renderer: eframe::Renderer::Wgpu,
        wgpu_options: gpu::wgpu_options(preview),
        ..Default::default()
    };
    let r = eframe::run_native("LiveBroadcaster", options, Box::new(|cc| Ok(Box::new(ui::StudioApp::new(cc)))));
    // The sources (and their browsers) are gone with the app.
    #[cfg(all(windows, feature = "browser"))]
    sources::browser::shutdown();
    r
}
