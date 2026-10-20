//! The studio window, built on Sphere UI: title chrome (menus, program scene, Go Live /
//! Record), a left sidebar (Scenes, Layers), the stage (preview + transition bar), a right
//! sidebar (Audio / Properties / Health) and the status bar.

mod inspector;
mod mixer;
#[cfg(windows)]
mod native_menu;
mod preview;
mod settings;
mod sidebar;

use std::collections::HashMap;
use std::sync::atomic::Ordering;
use std::time::{Duration, Instant};

use eframe::egui::{self, RichText};
use sphere_ui::{self as sui, Icon};

use crate::engine::Engine;
use crate::model::{Appearance, Id, SceneCollection, Settings};
use crate::output::rtmp::StreamState;

pub use preview::PreviewTexture;

#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum RenameTarget {
    Scene(Id),
}

pub(crate) struct Rename {
    pub target: RenameTarget,
    pub text: String,
    pub focus: bool,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum RightTab {
    Audio,
    Properties,
    Health,
}

pub(crate) struct Notice {
    pub text: String,
    pub error: bool,
    pub at: Instant,
}

pub struct StudioApp {
    pub(crate) engine: Engine,
    pub(crate) col: SceneCollection,
    pub(crate) settings: Settings,
    pub(crate) selected_item: Option<Id>,
    /// Source shown in the Properties tab (follows the selection; a mixer gear sets it too).
    pub(crate) inspect: Option<Id>,
    pub(crate) right_tab: RightTab,
    pub(crate) preview: PreviewTexture,
    pub(crate) inspector: inspector::InspectorState,
    pub(crate) rename: Option<Rename>,
    pub(crate) settings_dialog: Option<settings::SettingsDialog>,
    pub(crate) about_open: bool,
    pub(crate) confirm_exit: bool,
    pub(crate) exit_now: bool,
    pub(crate) notices: Vec<Notice>,
    pub(crate) drag: Option<preview::Drag>,
    /// Smoothed meter level, peak hold and when the peak was set, per source (dB).
    pub(crate) meters: HashMap<Id, (f32, f32, Instant)>,
    pub(crate) master_meter: [(f32, f32, Instant); 2],
    saved_col: SceneCollection,
    changed_at: Option<Instant>,
    sys: sysinfo::System,
    pub(crate) cpu: f32,
    cpu_at: Instant,
    pub(crate) bitrate: (Instant, u64, f32),
    last_error_seen: Option<String>,
    applied_theme: Option<Appearance>,
    /// The interface language in effect (follows the settings, or the system).
    applied_lang: Option<crate::i18n::Lang>,
    /// The GPU drawing the window ("name · API · type") and every adapter wgpu offered.
    pub(crate) gpu_label: String,
    pub(crate) gpu_adapters: Vec<String>,
    /// Preview settings the window was created with (changes apply after a restart).
    pub(crate) preview_at_start: crate::model::PreviewSettings,
    /// The Win32 menu bar (None elsewhere, or if it couldn't be built: egui menus then).
    #[cfg(windows)]
    native_menu: Option<native_menu::NativeMenu>,
}

impl StudioApp {
    pub fn new(cc: &eframe::CreationContext<'_>) -> Self {
        let col = crate::config::load_collection();
        let settings = crate::config::load_settings();
        sui::install(&cc.egui_ctx, theme_kind(settings.appearance));
        sui::set_translator(crate::i18n::t);
        // egui only has the hover inspector in debug builds.
        #[cfg(debug_assertions)]
        if std::env::var_os("LB_DEBUG_UI").is_some() {
            cc.egui_ctx.set_debug_on_hover(true);
        }
        // Composite on the window's GPU: the preview then shows the program texture directly.
        let gpu = cc.wgpu_render_state.as_ref().map(|rs| {
            let i = rs.adapter.get_info();
            crate::gpu_compositor::Gpu { device: rs.device.clone(), queue: rs.queue.clone(), name: format!("{} · {:?}", i.name, i.backend) }
        });
        let engine = Engine::start(col.clone(), settings.clone(), gpu);
        *engine.shared.repaint.lock() = Some(cc.egui_ctx.clone());
        engine.shared.ui_paced.store(settings.preview.vsync, std::sync::atomic::Ordering::Relaxed);
        let (gpu_label, gpu_adapters) = match &cc.wgpu_render_state {
            Some(rs) => {
                let i = rs.adapter.get_info();
                let label = format!("{} · {:?} · {:?}", i.name, i.backend, i.device_type);
                log::info!("preview GPU: {label}");
                // OpenGL adds "/PCIe/SSE2" to the name: keep the GPU's own name.
                let mut names: Vec<String> = rs.available_adapters.iter().map(|a| a.get_info().name.split('/').next().unwrap_or_default().trim().to_string()).collect();
                names.dedup();
                (label, names)
            }
            None => ("unknown".into(), Vec::new()),
        };
        let mut sys = sysinfo::System::new();
        sys.refresh_cpu_usage();
        let now = Instant::now();
        let preview_at_start = settings.preview.clone();
        #[cfg(windows)]
        let native_menu = native_menu::NativeMenu::install(cc);
        #[cfg(windows)]
        if let Some(m) = &native_menu {
            m.set_theme(settings.appearance == Appearance::Dark, &sui::palette());
        }
        StudioApp {
            engine,
            saved_col: col.clone(),
            col,
            applied_theme: Some(settings.appearance),
            applied_lang: None,
            settings,
            selected_item: None,
            inspect: None,
            right_tab: RightTab::Audio,
            preview: PreviewTexture::default(),
            inspector: inspector::InspectorState::default(),
            rename: None,
            settings_dialog: None,
            about_open: false,
            confirm_exit: false,
            exit_now: false,
            notices: Vec::new(),
            drag: None,
            meters: HashMap::new(),
            master_meter: [(-60.0, -60.0, now); 2],
            changed_at: None,
            sys,
            cpu: 0.0,
            cpu_at: now,
            bitrate: (now, 0, 0.0),
            last_error_seen: None,
            gpu_label,
            gpu_adapters,
            preview_at_start,
            #[cfg(windows)]
            native_menu,
        }
    }

    pub(crate) fn notify(&mut self, text: impl Into<String>, error: bool) {
        let text = text.into();
        if error {
            log::warn!("{text}");
        }
        self.notices.push(Notice { text, error, at: Instant::now() });
        if self.notices.len() > 4 {
            self.notices.remove(0);
        }
    }

    /// Select a scene item and show its source in Properties.
    pub(crate) fn select_item(&mut self, item: Option<Id>) {
        self.selected_item = item;
        if let Some(src) = item.and_then(|i| self.item_source(i)) {
            if self.inspect != Some(src) {
                self.inspect = Some(src);
                self.inspector.devices_for = None;
            }
        }
    }

    pub(crate) fn item_source(&self, item: Id) -> Option<Id> {
        self.col.scene(self.col.current_scene)?.items.iter().find(|i| i.id == item).map(|i| i.source)
    }

    /// Open a source's properties in the right sidebar.
    pub(crate) fn open_properties(&mut self, source: Id) {
        self.inspect = Some(source);
        self.right_tab = RightTab::Properties;
        self.inspector.devices_for = None;
    }

    fn sync_engine(&mut self) {
        // Place newly added items once their source knows its size.
        let (cw, ch) = (self.settings.video.width as f32, self.settings.video.height as f32);
        let sizes: HashMap<Id, (u32, u32)> =
            self.engine.shared.sources.read().iter().filter_map(|(id, r)| r.shared.video.size().map(|s| (*id, s))).collect();
        for scene in &mut self.col.scenes {
            for item in &mut scene.items {
                if !item.needs_layout {
                    continue;
                }
                let Some(&(w, h)) = sizes.get(&item.source) else { continue };
                if w as f32 > cw || h as f32 > ch {
                    preview::fit_item(item, (w, h), (cw, ch));
                } else {
                    item.scale = [1.0, 1.0];
                    item.pos = [((cw - w as f32) / 2.0).round(), ((ch - h as f32) / 2.0).round()];
                }
                item.needs_layout = false;
            }
        }
        if self.inspect.is_some_and(|id| self.col.source(id).is_none()) {
            self.inspect = None;
        }
        if self.col != *self.engine.shared.collection.lock() {
            self.engine.set_collection(&self.col);
        }
        if self.col != self.saved_col {
            let at = *self.changed_at.get_or_insert_with(Instant::now);
            if at.elapsed() > Duration::from_millis(1500) {
                self.save_collection();
            }
        }
    }

    pub(crate) fn save_collection(&mut self) {
        if let Err(e) = crate::config::save_collection(&self.col) {
            self.notify(trf!("Could not save scenes: {e}", e), true);
        }
        self.saved_col = self.col.clone();
        self.changed_at = None;
    }

    pub(crate) fn save_settings(&mut self) {
        self.engine.set_settings(&self.settings);
        if let Err(e) = crate::config::save_settings(&self.settings) {
            self.notify(trf!("Could not save settings: {e}", e), true);
        }
    }

    // -- outputs ------------------------------------------------------------------------------

    pub(crate) fn toggle_stream(&mut self) {
        if self.engine.outputs.is_streaming() {
            self.engine.outputs.stop_stream();
            self.notify(tr!("Stream ended"), false);
        } else {
            match self.engine.outputs.start_stream(&self.settings) {
                Ok(()) => self.notify(trf!("Connecting to {server} …", server = self.settings.stream.server_url()), false),
                Err(e) => self.notify(trf!("Can't go live: {e}", e), true),
            }
        }
    }

    pub(crate) fn toggle_recording(&mut self) {
        if self.engine.outputs.is_recording() {
            let path = self.engine.outputs.state.recording.lock().as_ref().map(|r| r.path.clone());
            self.engine.outputs.stop_recording();
            if let Some(p) = path {
                self.notify(trf!("Recording saved: {path}", path = p.display()), false);
            }
        } else {
            match self.engine.outputs.start_recording(&self.settings) {
                Ok(p) => self.notify(trf!("Recording to {path}", path = p.display()), false),
                Err(e) => self.notify(trf!("Can't start recording: {e}", e), true),
            }
        }
    }

    fn poll_outputs(&mut self) {
        self.engine.outputs.poll();
        let err = self.engine.outputs.state.last_error.lock().clone();
        if err.is_some() && err != self.last_error_seen {
            self.notify(err.clone().unwrap(), true);
        }
        self.last_error_seen = err;
        let bytes = self.engine.outputs.state.stream.lock().as_ref().map(|s| s.bytes_sent.load(Ordering::Relaxed)).unwrap_or(0);
        let el = self.bitrate.0.elapsed();
        if el >= Duration::from_secs(1) {
            let kbps = bytes.saturating_sub(self.bitrate.1) as f32 * 8.0 / el.as_secs_f32() / 1000.0;
            self.bitrate = (Instant::now(), bytes, if bytes == 0 { 0.0 } else { kbps });
        }
        if self.cpu_at.elapsed() >= Duration::from_secs(1) {
            self.sys.refresh_cpu_usage();
            self.cpu = self.sys.global_cpu_usage();
            self.cpu_at = Instant::now();
        }
    }

    pub(crate) fn stream_state(&self) -> Option<StreamState> {
        self.engine.outputs.state.stream.lock().as_ref().map(|s| s.state.lock().clone())
    }

    // -- chrome -------------------------------------------------------------------------------

    fn menus(&mut self, ui: &mut egui::Ui) {
        let p = sui::palette();
        // Frameless menu buttons (not `menu::bar`, which claims the whole row and would hide
        // the title's centre slot).
        ui.scope(|ui| {
            ui.visuals_mut().button_frame = false;
            ui.spacing_mut().button_padding = egui::vec2(6.0, 3.0);
            ui.menu_button(RichText::new(tr!("File")).color(p.text_secondary), |ui| {
                if sui::menu_item(ui, Some(Icon::FolderOpen), tr!("Show Recordings"), None).clicked() {
                    let _ = open_path(&self.settings.output.record_path);
                    ui.close_menu();
                }
                if sui::menu_item(ui, Some(Icon::Folder), tr!("Show Settings Folder"), None).clicked() {
                    let _ = open_path(&crate::config::config_dir().to_string_lossy());
                    ui.close_menu();
                }
                ui.separator();
                if sui::menu_item(ui, Some(Icon::Settings), tr!("Settings…"), Some("Ctrl+,")).clicked() {
                    self.open_settings();
                    ui.close_menu();
                }
                ui.separator();
                if sui::menu_item(ui, Some(Icon::Power), tr!("Exit"), None).clicked() {
                    ui.ctx().send_viewport_cmd(egui::ViewportCommand::Close);
                    ui.close_menu();
                }
            });
            ui.menu_button(RichText::new(tr!("Edit")).color(p.text_secondary), |ui| {
                let sel = self.selected_item;
                for (icon, label, action) in [
                    (Icon::Maximize, tr!("Fit to Canvas"), preview::ItemAction::Fit),
                    (Icon::Scan, tr!("Stretch to Canvas"), preview::ItemAction::Stretch),
                    (Icon::Move, tr!("Center"), preview::ItemAction::Center),
                    (Icon::RotateCcw, tr!("Reset Transform"), preview::ItemAction::Reset),
                ] {
                    if sui::menu_item_colored(ui, Some(icon), label, None, None, sel.is_some()).clicked() {
                        self.item_action(sel.unwrap(), action);
                        ui.close_menu();
                    }
                }
                ui.separator();
                if sui::menu_item_colored(ui, Some(Icon::Trash2), tr!("Remove Layer"), Some("Del"), None, sel.is_some()).clicked() {
                    self.item_action(sel.unwrap(), preview::ItemAction::Remove);
                    ui.close_menu();
                }
            });
            ui.menu_button(RichText::new(tr!("Scenes")).color(p.text_secondary), |ui| {
                if sui::menu_item(ui, Some(Icon::Plus), tr!("New Scene"), None).clicked() {
                    let id = self.col.add_scene(tr!("Scene"));
                    self.col.current_scene = id;
                    ui.close_menu();
                }
                if sui::menu_item(ui, Some(Icon::Save), tr!("Save Now"), Some("Ctrl+S")).clicked() {
                    self.save_collection();
                    self.notify(tr!("Scenes saved"), false);
                    ui.close_menu();
                }
                ui.separator();
                if sui::menu_item(ui, Some(Icon::Share), tr!("Export Collection…"), None).clicked() {
                    ui.close_menu();
                    self.export_collection();
                }
                if sui::menu_item(ui, Some(Icon::Import), tr!("Import Collection…"), None).clicked() {
                    ui.close_menu();
                    self.import_collection();
                }
            });
            ui.menu_button(RichText::new(tr!("View")).color(p.text_secondary), |ui| {
                for (tab, icon, label) in [(RightTab::Audio, Icon::AudioLines, tr!("Audio")), (RightTab::Properties, Icon::SlidersHorizontal, tr!("Properties")), (RightTab::Health, Icon::Activity, tr!("Health"))] {
                    if sui::menu_item(ui, Some(icon), label, None).clicked() {
                        self.right_tab = tab;
                        ui.close_menu();
                    }
                }
                ui.separator();
                let (icon, label) = if self.settings.appearance == Appearance::Dark { (Icon::Sun, tr!("Light Theme")) } else { (Icon::Moon, tr!("Dark Theme")) };
                if sui::menu_item(ui, Some(icon), label, None).clicked() {
                    self.settings.appearance = if self.settings.appearance == Appearance::Dark { Appearance::Light } else { Appearance::Dark };
                    self.save_settings();
                    ui.close_menu();
                }
            });
            ui.menu_button(RichText::new(tr!("Help")).color(p.text_secondary), |ui| {
                if sui::menu_item(ui, Some(Icon::Info), tr!("About LiveBroadcaster"), None).clicked() {
                    self.about_open = true;
                    ui.close_menu();
                }
            });
        });
    }

    fn export_collection(&mut self) {
        if let Some(p) = rfd::FileDialog::new().add_filter("Scene collection", &["json"]).set_file_name("scenes.json").save_file() {
            match serde_json::to_string_pretty(&self.col).map_err(|e| e.to_string()).and_then(|t| std::fs::write(&p, t).map_err(|e| e.to_string())) {
                Ok(()) => self.notify(trf!("Exported to {path}", path = p.display()), false),
                Err(e) => self.notify(trf!("Export failed: {e}", e), true),
            }
        }
    }

    fn import_collection(&mut self) {
        if let Some(p) = rfd::FileDialog::new().add_filter("Scene collection", &["json"]).pick_file() {
            match std::fs::read_to_string(&p).map_err(|e| e.to_string()).and_then(|t| serde_json::from_str::<SceneCollection>(&t).map_err(|e| e.to_string())) {
                Ok(mut c) => {
                    c.repair();
                    self.col = c;
                    self.selected_item = None;
                    self.notify(tr!("Scene collection imported"), false);
                }
                Err(e) => self.notify(trf!("Import failed: {e}", e), true),
            }
        }
    }

    fn chrome(&mut self, ctx: &egui::Context) {
        let p = sui::palette();
        let scene_name = self.col.scene(self.col.current_scene).map(|s| s.name.clone()).unwrap_or_default();
        let mut stream_click = false;
        let mut rec_click = false;
        let mut settings_click = false;
        let state = self.stream_state();
        let live_for = self.engine.outputs.state.stream.lock().as_ref().and_then(|s| *s.live_since.lock()).map(|t| t.elapsed());
        let rec_for = self.engine.outputs.state.recording.lock().as_ref().map(|r| r.since.elapsed());
        let native = self.has_native_menu();
        let mut this = Some(self);
        sui::chrome_row(
            ctx,
            !native,
            |ui| {
                ui.add_space(sui::space::TIGHT);
                sui::icons::icon(ui, Icon::RadioTower, 16.0, p.accent_primary);
                if native {
                    return;
                }
                ui.label(RichText::new(tr!("LiveBroadcaster")).font(sui::semibold(sui::typography::UI_SM)).color(p.text_primary));
                ui.add_space(sui::space::BASE);
                if let Some(app) = this.as_mut() {
                    app.menus(ui);
                }
            },
            |ui| {
                // Centre the chip + name within the slot.
                let p2 = sui::palette();
                let name_w = ui.painter().layout_no_wrap(scene_name.clone(), sui::semibold(sui::typography::UI_SM), p2.text_primary).size().x;
                let total = name_w + 96.0;
                ui.add_space(((ui.available_width() - total) / 2.0).max(0.0));
                ui.horizontal(|ui| {
                    let (txt, color) = match &state {
                        Some(StreamState::Live) => (tr!("ON AIR"), p.status_error),
                        Some(StreamState::Connecting) | Some(StreamState::Reconnecting { .. }) => (tr!("CONNECTING"), p.status_warning),
                        _ => (tr!("PROGRAM"), p.text_muted),
                    };
                    let style = if matches!(state, Some(StreamState::Live)) { sui::ChipStyle::Solid } else { sui::ChipStyle::Soft };
                    sui::chip(ui, txt, color, style, None, true);
                    ui.label(RichText::new(&scene_name).font(sui::semibold(sui::typography::UI_SM)).color(p.text_primary));
                });
            },
            |ui| {
                if ui.add(sui::IconButton::new(Icon::Settings).tooltip(tr!("Settings"))).clicked() {
                    settings_click = true;
                }
                sui::vdivider(ui, 18.0);
                let h = sui::size::DEFAULT + 4.0;
                let live_btn = match &state {
                    Some(StreamState::Live) => sui::Button::danger(trf!("End  {time}", time = hms(live_for.unwrap_or_default()))).icon(Icon::CircleStop),
                    Some(StreamState::Connecting) => sui::Button::new(tr!("Connecting…")).icon(Icon::Wifi).selected(true),
                    Some(StreamState::Reconnecting { attempt }) => sui::Button::new(trf!("Reconnecting #{attempt}", attempt)).icon(Icon::RefreshCw).selected(true),
                    _ => sui::Button::primary(tr!("Go Live")).icon(Icon::Radio),
                };
                if ui.add(live_btn.height(h).min_width(104.0)).clicked() {
                    stream_click = true;
                }
                let rec_btn = match rec_for {
                    Some(d) => sui::Button::new(trf!("Stop  {time}", time = hms(d))).icon(Icon::Square).selected(true),
                    None => sui::Button::new(tr!("Record")).icon(Icon::CircleDot),
                };
                if ui.add(rec_btn.height(h).min_width(92.0)).clicked() {
                    rec_click = true;
                }
            },
        );
        let app = this.take().unwrap();
        if stream_click {
            app.toggle_stream();
        }
        if rec_click {
            app.toggle_recording();
        }
        if settings_click {
            app.open_settings();
        }
    }

    fn status(&mut self, ctx: &egui::Context) {
        let p = sui::palette();
        self.notices.retain(|n| n.at.elapsed() < Duration::from_secs(if n.error { 12 } else { 6 }));
        let notice = self.notices.last().map(|n| (n.text.clone(), n.error));
        let st = &self.engine.shared.stats;
        let fps = st.fps.get();
        let render = st.render_ms.get();
        let out_active = self.engine.outputs.active();
        let enc_ms = self.engine.outputs.state.encode_ms.get();
        let skipped = self.engine.shared.taps.skipped_frames.load(Ordering::Relaxed);
        let stream = self.engine.outputs.state.stream.lock().clone();
        let kbps = self.bitrate.2;
        let cpu = self.cpu;
        let target_fps = self.settings.video.fps;
        sui::status_bar(
            ctx,
            |ui| match &notice {
                Some((t, err)) => {
                    let (icon, c) = if *err { (Icon::TriangleAlert, p.status_error) } else { (Icon::Info, p.statusbar_text) };
                    sui::status_item(ui, Some(icon), t, Some(c));
                }
                None => {
                    sui::status_item(ui, Some(Icon::Check), tr!("Ready"), Some(p.statusbar_text_muted));
                }
            },
            |ui| {
                let fps_c = if fps + 0.5 < target_fps as f32 { Some(p.status_warning) } else { None };
                sui::status_item(ui, Some(Icon::Gauge), &format!("{fps:.1} / {target_fps} fps"), fps_c);
                sui::status_item(ui, Some(Icon::Timer), &format!("{render:.1} ms"), None);
                sui::status_item(ui, Some(Icon::Cpu), &format!("CPU {cpu:.0}%"), None);
                if out_active {
                    let c = if skipped > 0 { Some(p.status_warning) } else { None };
                    sui::status_item(ui, Some(Icon::Activity), &trf!("enc {ms} ms · lag {skipped}", ms = format!("{enc_ms:.1}"), skipped), c);
                }
                if let Some(s) = &stream {
                    let dropped = s.frames_dropped.load(Ordering::Relaxed);
                    let c = if dropped > 0 { Some(p.status_warning) } else { Some(p.status_success) };
                    sui::status_item(ui, Some(Icon::Upload), &trf!("{kbps} kb/s · dropped {dropped}", kbps = format!("{kbps:.0}"), dropped), c);
                }
            },
        );
    }

    fn about_window(&mut self, ctx: &egui::Context) {
        if !self.about_open {
            return;
        }
        let p = sui::palette();
        let mut close = false;
        egui::Window::new("about")
            .title_bar(false)
            .resizable(false)
            .collapsible(false)
            .frame(sui::dialog_frame())
            .anchor(egui::Align2::CENTER_CENTER, egui::vec2(0.0, 0.0))
            .show(ctx, |ui| {
                ui.set_width(400.0);
                close = sui::dialog_header(ui, Icon::RadioTower, tr!("About LiveBroadcaster"));
                ui.label(RichText::new(trf!("Version {version}", version = env!("CARGO_PKG_VERSION"))).color(p.text_muted));
                ui.add_space(sui::space::BASE);
                for (icon, text) in [
                    (Icon::Film, tr!("Decode, H.264 / AAC encode and MP4 / MOV mux: MediaKit Foundation (MKFF)")),
                    (Icon::LayoutDashboard, tr!("Interface: Sphere UI (Futureboard design system) on egui / wgpu")),
                    (Icon::Radio, tr!("Output: RTMP / RTMPS — YouTube, Twitch, Facebook, Kick, custom")),
                    (Icon::Monitor, if cfg!(windows) {
                        tr!("Capture: DXGI displays, windows, Media Foundation cameras, WASAPI audio")
                    } else {
                        tr!("Capture: media files, images, text, microphones and system playback (screen, window and camera capture need Windows)")
                    }),
                    (Icon::Sparkles, tr!("Icons: Lucide (ISC)")),
                ] {
                    ui.horizontal(|ui| {
                        sui::icons::icon(ui, icon, 14.0, p.text_muted);
                        ui.label(RichText::new(text).color(p.text_secondary));
                    });
                }
                ui.add_space(sui::space::LOOSE);
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if ui.add(sui::Button::primary(tr!("Close")).min_width(80.0)).clicked() {
                        close = true;
                    }
                });
            });
        if close {
            self.about_open = false;
        }
    }

    fn exit_dialog(&mut self, ctx: &egui::Context) {
        if !self.confirm_exit {
            return;
        }
        let p = sui::palette();
        sui::scrim(ctx);
        egui::Window::new("confirm-exit")
            .title_bar(false)
            .resizable(false)
            .collapsible(false)
            .frame(sui::dialog_frame())
            .anchor(egui::Align2::CENTER_CENTER, egui::vec2(0.0, 0.0))
            .order(egui::Order::Foreground)
            .show(ctx, |ui| {
                ui.set_width(360.0);
                if sui::dialog_header(ui, Icon::TriangleAlert, tr!("You're still on air")) {
                    self.confirm_exit = false;
                }
                ui.label(RichText::new(tr!("Streaming or recording is running. Stop everything and quit?")).color(p.text_secondary));
                ui.add_space(sui::space::LOOSE);
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if ui.add(sui::Button::danger(tr!("Stop and Quit")).icon(Icon::Power)).clicked() {
                        self.engine.outputs.stop_all();
                        self.exit_now = true;
                        self.confirm_exit = false;
                        ui.ctx().send_viewport_cmd(egui::ViewportCommand::Close);
                    }
                    if ui.add(sui::Button::new(tr!("Keep Running"))).clicked() {
                        self.confirm_exit = false;
                    }
                });
            });
    }

    fn has_native_menu(&self) -> bool {
        #[cfg(windows)]
        {
            self.native_menu.is_some()
        }
        #[cfg(not(windows))]
        {
            false
        }
    }

    /// Run what was picked in the Win32 menu, and keep its items in step with the app.
    #[cfg(windows)]
    fn native_menu_frame(&mut self, ctx: &egui::Context) {
        use native_menu::MenuCmd as M;
        let Some(menu) = self.native_menu.as_mut() else { return };
        menu.follow_language();
        let cmds = menu.take_commands();
        // State first (labels / enabled / checks), then act.
        let streaming = self.engine.outputs.is_streaming();
        let recording = self.engine.outputs.is_recording();
        let sel = self.selected_item.is_some();
        let dark = self.settings.appearance == Appearance::Dark;
        let tab = self.right_tab;
        let menu = self.native_menu.as_mut().unwrap();
        menu.set_label(M::ToggleStream, if streaming { tr!("End &Stream") } else { tr!("&Go Live") });
        menu.set_label(M::ToggleRecord, if recording { tr!("Stop &Recording") } else { tr!("Start &Recording") });
        for c in [M::Fit, M::Stretch, M::Center, M::ResetTransform, M::RemoveLayer] {
            menu.set_enabled(c, sel);
        }
        menu.set_checked(M::ViewAudio, tab == RightTab::Audio);
        menu.set_checked(M::ViewProperties, tab == RightTab::Properties);
        menu.set_checked(M::ViewHealth, tab == RightTab::Health);
        menu.set_checked(M::DarkTheme, dark);
        for c in cmds {
            let item = self.selected_item;
            match c {
                M::ToggleStream => self.toggle_stream(),
                M::ToggleRecord => self.toggle_recording(),
                M::ShowRecordings => {
                    let _ = open_path(&self.settings.output.record_path);
                }
                M::ShowSettingsFolder => {
                    let _ = open_path(&crate::config::config_dir().to_string_lossy());
                }
                M::Settings => self.open_settings(),
                M::Exit => ctx.send_viewport_cmd(egui::ViewportCommand::Close),
                M::Fit | M::Stretch | M::Center | M::ResetTransform | M::RemoveLayer => {
                    if let Some(id) = item {
                        let a = match c {
                            M::Fit => preview::ItemAction::Fit,
                            M::Stretch => preview::ItemAction::Stretch,
                            M::Center => preview::ItemAction::Center,
                            M::ResetTransform => preview::ItemAction::Reset,
                            _ => preview::ItemAction::Remove,
                        };
                        self.item_action(id, a);
                    }
                }
                M::NewScene => {
                    let id = self.col.add_scene(tr!("Scene"));
                    self.col.current_scene = id;
                    self.select_item(None);
                }
                M::SaveScenes => {
                    self.save_collection();
                    self.notify(tr!("Scenes saved"), false);
                }
                M::ExportScenes => self.export_collection(),
                M::ImportScenes => self.import_collection(),
                M::ViewAudio => self.right_tab = RightTab::Audio,
                M::ViewProperties => self.right_tab = RightTab::Properties,
                M::ViewHealth => self.right_tab = RightTab::Health,
                M::DarkTheme => {
                    self.settings.appearance = if dark { Appearance::Light } else { Appearance::Dark };
                    self.save_settings();
                }
                M::About => self.about_open = true,
            }
        }
    }

    #[cfg(not(windows))]
    fn native_menu_frame(&mut self, _ctx: &egui::Context) {}

    fn shortcuts(&mut self, ctx: &egui::Context) {
        if ctx.wants_keyboard_input() {
            return;
        }
        let (save, settings) = ctx.input_mut(|i| (i.consume_key(egui::Modifiers::COMMAND, egui::Key::S), i.consume_key(egui::Modifiers::COMMAND, egui::Key::Comma)));
        if save {
            self.save_collection();
            self.notify(tr!("Scenes saved"), false);
        }
        if settings {
            self.open_settings();
        }
    }
}

fn theme_kind(a: Appearance) -> sui::ThemeKind {
    match a {
        Appearance::Dark => sui::ThemeKind::Dark,
        Appearance::Light => sui::ThemeKind::Light,
    }
}

impl eframe::App for StudioApp {
    fn update(&mut self, ctx: &egui::Context, frame: &mut eframe::Frame) {
        let frame_start = Instant::now();
        let lang = self.settings.language.unwrap_or_else(system_lang);
        if self.applied_lang != Some(lang) {
            crate::i18n::set_lang(lang);
            let han = if lang == crate::i18n::Lang::Ja { sui::fonts::HanPreference::Japanese } else { sui::fonts::HanPreference::Chinese };
            sui::fonts::install_for(ctx, han);
            self.applied_lang = Some(lang);
        }
        if self.applied_theme != Some(self.settings.appearance) {
            sui::install(ctx, theme_kind(self.settings.appearance));
            self.applied_theme = Some(self.settings.appearance);
            #[cfg(windows)]
            if let Some(m) = &self.native_menu {
                m.set_theme(self.settings.appearance == Appearance::Dark, &sui::palette());
            }
        }
        if ctx.input(|i| i.viewport().close_requested()) && !self.exit_now && self.engine.outputs.active() {
            ctx.send_viewport_cmd(egui::ViewportCommand::CancelClose);
            self.confirm_exit = true;
        }
        if !self.has_native_menu() {
            sui::window_resize(ctx);
        }
        self.native_menu_frame(ctx);
        self.shortcuts(ctx);
        self.poll_outputs();
        self.preview.update(frame, &self.engine.shared);
        let p = sui::palette();

        self.chrome(ctx);
        self.status(ctx);
        let side = egui::Frame::new().fill(p.surface_window).inner_margin(egui::Margin::same(sui::space::BASE as i8));
        egui::SidePanel::left("left")
            .resizable(true)
            .default_width(268.0)
            .width_range(220.0..=420.0)
            .frame(side)
            .show_separator_line(false)
            .show(ctx, |ui| self.left_sidebar(ui));
        egui::SidePanel::right("right")
            .resizable(true)
            .default_width(330.0)
            .width_range(280.0..=520.0)
            .frame(side)
            .show_separator_line(false)
            .show(ctx, |ui| self.right_sidebar(ui));
        egui::CentralPanel::default()
            .frame(egui::Frame::new().fill(p.surface_window).inner_margin(egui::Margin { left: 0, right: 0, top: sui::space::BASE as i8, bottom: sui::space::BASE as i8 }))
            .show(ctx, |ui| self.stage(ui));

        self.settings_window(ctx);
        self.about_window(ctx);
        self.exit_dialog(ctx);
        if !self.has_native_menu() {
            sui::window_outline(ctx);
        }

        self.sync_engine();
        if self.preview_at_start.vsync {
            // Redraw every vsync and show the newest program frame each time.
            ctx.request_repaint();
        } else {
            ctx.request_repaint_after(Duration::from_millis(50));
        }
        self.preview.stats.ui_frame(frame_start.elapsed().as_secs_f32() * 1000.0);
    }

    fn on_exit(&mut self) {
        self.save_collection();
        let _ = crate::config::save_settings(&self.settings);
        self.engine.shutdown();
    }
}

pub(crate) fn hms(d: Duration) -> String {
    let s = d.as_secs();
    format!("{:02}:{:02}:{:02}", s / 3600, s / 60 % 60, s % 60)
}

pub(crate) fn open_path(path: &str) -> std::io::Result<()> {
    #[cfg(windows)]
    {
        std::process::Command::new("explorer").arg(path).spawn().map(|_| ())
    }
    #[cfg(target_os = "macos")]
    {
        std::process::Command::new("open").arg(path).spawn().map(|_| ())
    }
    #[cfg(all(unix, not(target_os = "macos")))]
    {
        std::process::Command::new("xdg-open").arg(path).spawn().map(|_| ())
    }
}

/// The window icon: a broadcast mark in the Sphere accent on a rounded tile.
pub fn app_icon() -> egui::IconData {
    let (w, h) = (64u32, 64u32);
    let mut rgba = vec![0u8; (w * h * 4) as usize];
    let accent = [0x4Du8, 0x9C, 0xF8];
    for y in 0..h {
        for x in 0..w {
            let i = ((y * w + x) * 4) as usize;
            let (fx, fy) = (x as f32 + 0.5, y as f32 + 0.5);
            let (dx, dy) = ((fx - 32.0).abs() - 20.0, (fy - 32.0).abs() - 20.0);
            let out = (dx.max(0.0).powi(2) + dy.max(0.0).powi(2)).sqrt() + dx.max(dy).min(0.0) - 10.0;
            let tile = (0.5 - out).clamp(0.0, 1.0);
            let d = ((fx - 32.0).powi(2) + (fy - 32.0).powi(2)).sqrt();
            let ring = |r: f32| (1.6 - (d - r).abs()).clamp(0.0, 1.0);
            let side = (fx - 32.0).abs() > (fy - 32.0).abs() * 0.6;
            let glyph = (5.5 - d).clamp(0.0, 1.0).max(if side { ring(12.0).max(ring(19.0)) } else { 0.0 });
            let mix = |c: u8| (c as f32 * (1.0 - glyph) + 255.0 * glyph) as u8;
            rgba[i..i + 4].copy_from_slice(&[mix(accent[0]), mix(accent[1]), mix(accent[2]), (tile * 255.0) as u8]);
        }
    }
    egui::IconData { rgba, width: w, height: h }
}

/// The system language (read once).
fn system_lang() -> crate::i18n::Lang {
    static L: std::sync::OnceLock<crate::i18n::Lang> = std::sync::OnceLock::new();
    *L.get_or_init(crate::i18n::Lang::system)
}
