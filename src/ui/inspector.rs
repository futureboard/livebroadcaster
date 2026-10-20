//! The Properties tab: the inspected source's settings (edits apply live — the engine
//! restarts a source whenever its settings change), media transport, and the selected layer's
//! transform.

use std::sync::atomic::Ordering;

use eframe::egui::{self, RichText};
use sphere_ui::{self as sui, Icon};

use super::preview::ItemAction;
use super::StudioApp;
use crate::model::{Id, SourceKind, TextAlign};
use crate::sources::media::MediaCommand;

const LABEL_W: f32 = 92.0;

/// Device lists, read when a capture source is inspected (Refresh re-reads them).
#[derive(Default)]
pub struct Devices {
    #[cfg(windows)]
    monitors: Vec<crate::sources::display::MonitorInfo>,
    #[cfg(windows)]
    windows: Vec<crate::sources::window::WindowInfo>,
    #[cfg(windows)]
    cameras: Vec<crate::sources::camera::CameraInfo>,
    inputs: Vec<(String, String)>,
    outputs: Vec<(String, String)>,
    #[cfg(windows)]
    apps: Vec<crate::sources::app_audio::AppInfo>,
}

impl Devices {
    fn load(kind: &SourceKind) -> Devices {
        let mut d = Devices::default();
        match kind {
            #[cfg(windows)]
            SourceKind::Display { .. } => d.monitors = crate::sources::display::list_monitors(),
            #[cfg(windows)]
            SourceKind::Window { .. } => d.windows = crate::sources::window::list_windows(),
            #[cfg(windows)]
            SourceKind::Camera { .. } => d.cameras = crate::sources::camera::list_cameras(),
            SourceKind::AudioInput { .. } => d.inputs = crate::sources::list_input_devices(),
            SourceKind::AudioOutput { .. } => d.outputs = crate::sources::list_output_devices(),
            #[cfg(windows)]
            SourceKind::AppAudio { .. } => d.apps = crate::sources::app_audio::list_apps(),
            _ => {}
        }
        d
    }
}

#[derive(Default)]
pub struct InspectorState {
    devices: Devices,
    /// The source `devices` was loaded for (None: reload).
    pub devices_for: Option<Id>,
    seek: Option<f32>,
}

fn pick_file(title: &str, filters: &[(&str, Vec<String>)]) -> Option<String> {
    let mut d = rfd::FileDialog::new().set_title(title);
    for (name, exts) in filters {
        d = d.add_filter(*name, exts);
    }
    d.pick_file().map(|p| p.to_string_lossy().into_owned())
}

fn exts(list: &[&str]) -> Vec<String> {
    list.iter().map(|s| s.to_string()).collect()
}

/// A file path shown as a field with a Browse button.
fn path_field(ui: &mut egui::Ui, path: &mut String, title: &str, filters: &[(&str, Vec<String>)]) {
    let p = sui::palette();
    ui.horizontal(|ui| {
        let shown = if path.is_empty() {
            tr!("No file selected").to_string()
        } else {
            std::path::Path::new(path.as_str()).file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or(path.clone())
        };
        let w = (ui.available_width() - 34.0).max(60.0);
        let (rect, resp) = ui.allocate_exact_size(egui::vec2(w, sui::size::COMFORTABLE), egui::Sense::hover());
        ui.painter().rect_filled(rect, sui::radius::cr(sui::radius::CONTROL), p.surface_input);
        ui.painter().rect_stroke(rect, sui::radius::cr(sui::radius::CONTROL), egui::Stroke::new(1.0f32, p.border_subtle), egui::StrokeKind::Inside);
        let job = egui::text::LayoutJob {
            wrap: egui::text::TextWrapping::truncate_at_width(w - 16.0),
            ..egui::text::LayoutJob::simple_singleline(shown, sui::regular(sui::typography::UI_SM), if path.is_empty() { p.text_faint } else { p.text_primary })
        };
        let g = ui.painter().layout_job(job);
        ui.painter().galley(egui::pos2(rect.left() + 8.0, rect.center().y - g.size().y / 2.0), g, p.text_primary);
        if !path.is_empty() {
            resp.on_hover_text(path.as_str());
        }
        if ui.add(sui::IconButton::new(Icon::FolderOpen).size(sui::size::COMFORTABLE).framed(true).tooltip(tr!("Browse…"))).clicked() {
            if let Some(f) = pick_file(title, filters) {
                *path = f;
            }
        }
    });
}

fn color_field(ui: &mut egui::Ui, c: &mut [u8; 4]) {
    ui.horizontal(|ui| {
        ui.color_edit_button_srgba_unmultiplied(c);
        ui.label(RichText::new(format!("#{:02X}{:02X}{:02X}{}", c[0], c[1], c[2], if c[3] < 255 { format!("{:02X}", c[3]) } else { String::new() })).font(sui::mono(sui::typography::UI_XS)).color(sui::palette().text_muted));
    });
}

fn device_combo(ui: &mut egui::Ui, id: &str, device: &mut Option<String>, list: &[(String, String)]) {
    let cur = match device {
        None => tr!("System default").to_string(),
        Some(d) => list.iter().find(|(i, _)| i == d).map(|(_, n)| n.clone()).unwrap_or_else(|| tr!("Unavailable device").to_string()),
    };
    egui::ComboBox::from_id_salt(id).width(ui.available_width()).selected_text(cur).show_ui(ui, |ui| {
        ui.selectable_value(device, None, tr!("System default"));
        for (i, name) in list {
            ui.selectable_value(device, Some(i.clone()), name);
        }
    });
}

fn mmss(s: f32) -> String {
    let s = s.max(0.0) as u64;
    if s >= 3600 {
        format!("{}:{:02}:{:02}", s / 3600, s / 60 % 60, s % 60)
    } else {
        format!("{:02}:{:02}", s / 60, s % 60)
    }
}

impl StudioApp {
    pub(crate) fn properties_tab(&mut self, ui: &mut egui::Ui) {
        let p = sui::palette();
        let Some(id) = self.inspect else {
            sui::empty_state(ui, Icon::SlidersHorizontal, tr!("Nothing selected"), tr!("Select a layer, or the slider icon on an audio channel"));
            return;
        };
        let Some(kind) = self.col.source(id).map(|s| s.kind.clone()) else { return };
        if self.inspector.devices_for != Some(id) {
            self.inspector.devices = Devices::load(&kind);
            self.inspector.devices_for = Some(id);
            self.inspector.seek = None;
        }
        let rt = self.engine.runtime(id);
        let item = self.selected_item.and_then(|i| self.col.scene(self.col.current_scene)?.items.iter().find(|it| it.id == i && it.source == id).cloned());
        let mut restart = false;
        let mut refresh = false;
        egui::ScrollArea::vertical().id_salt("inspector").auto_shrink([false, false]).show(ui, |ui| {
            let Some(src) = self.col.source_mut(id) else { return };
            // Header: icon tile, editable name, type.
            ui.horizontal(|ui| {
                let (tile, _) = ui.allocate_exact_size(egui::vec2(32.0, 32.0), egui::Sense::hover());
                ui.painter().rect_filled(tile, sui::radius::cr(sui::radius::CONTROL + 2.0), p.accent_muted);
                sui::icons::paint(ui, src.kind.icon(), tile.center(), 16.0, p.accent_primary);
                ui.vertical(|ui| {
                    ui.spacing_mut().item_spacing.y = 2.0;
                    ui.add(egui::TextEdit::singleline(&mut src.name).font(sui::semibold(sui::typography::UI_MD)).frame(false).desired_width(ui.available_width()));
                    ui.label(RichText::new(src.kind.type_name()).font(sui::regular(sui::typography::UI_XS)).color(p.text_faint));
                });
            });
            if let Some(e) = rt.as_ref().and_then(|r| r.shared.error()) {
                ui.add_space(sui::space::TIGHT);
                egui::Frame::new()
                    .fill(sui::with_alpha(p.status_warning, 0.10))
                    .stroke(egui::Stroke::new(1.0f32, sui::with_alpha(p.status_warning, 0.35)))
                    .corner_radius(sui::radius::cr(sui::radius::CONTROL))
                    .inner_margin(egui::Margin::same(sui::space::BASE as i8))
                    .show(ui, |ui| {
                        ui.set_width(ui.available_width());
                        ui.horizontal_wrapped(|ui| {
                            sui::icons::icon(ui, Icon::TriangleAlert, 14.0, p.status_warning);
                            ui.label(RichText::new(e).color(p.status_warning));
                        });
                    });
            }
            if let Some(sz) = rt.as_ref().and_then(|r| r.shared.video.size()) {
                ui.horizontal(|ui| {
                    sui::chip(ui, &format!("{} × {}", sz.0, sz.1), p.text_muted, sui::ChipStyle::Outline, Some(Icon::Scan), false);
                    if rt.as_ref().is_some_and(|r| r.shared.is_active()) {
                        sui::chip(ui, tr!("ON AIR"), p.status_success, sui::ChipStyle::Soft, None, true);
                    }
                });
            }

            sui::section_header(ui, tr!("Source"));
            let dev = &self.inspector.devices;
            match &mut src.kind {
                SourceKind::Color { color, width, height } => {
                    sui::property_row(ui, tr!("Color"), LABEL_W, |ui| color_field(ui, color));
                    sui::property_row(ui, tr!("Size"), LABEL_W, |ui| {
                        ui.horizontal(|ui| {
                            ui.add(egui::DragValue::new(width).range(1..=8192).suffix(" px"));
                            ui.label("×");
                            ui.add(egui::DragValue::new(height).range(1..=8192).suffix(" px"));
                        });
                    });
                }
                SourceKind::Image { path } => {
                    sui::property_row(ui, tr!("Image"), LABEL_W, |ui| path_field(ui, path, tr!("Select image"), &[(tr!("Images"), exts(&["png", "jpg", "jpeg", "bmp", "gif", "webp"]))]));
                }
                SourceKind::Text(t) => {
                    sui::property_row(ui, tr!("Text"), LABEL_W, |ui| {
                        ui.add_enabled(t.from_file.is_none(), egui::TextEdit::multiline(&mut t.text).desired_rows(3).desired_width(f32::INFINITY));
                    });
                    sui::property_row(ui, tr!("From file"), LABEL_W, |ui| {
                        let mut on = t.from_file.is_some();
                        if ui.add(sui::Toggle::new(&mut on)).changed() {
                            t.from_file = if on { Some(String::new()) } else { None };
                        }
                        if let Some(f) = &mut t.from_file {
                            path_field(ui, f, tr!("Text file"), &[(tr!("Text"), exts(&["txt"]))]);
                        }
                    });
                    sui::property_row(ui, tr!("Font"), LABEL_W, |ui| {
                        let mut f = t.font_path.clone();
                        path_field(ui, &mut f, tr!("Font"), &[(tr!("Fonts"), exts(&["ttf", "otf", "ttc"]))]);
                        if f != t.font_path {
                            t.font_path = f;
                        }
                        if !t.font_path.is_empty() && ui.add(sui::Button::ghost(tr!("Use system font")).icon(Icon::RotateCcw)).clicked() {
                            t.font_path.clear();
                        }
                    });
                    sui::property_row(ui, tr!("Size"), LABEL_W, |ui| ui.add(egui::Slider::new(&mut t.size, 8.0..=300.0).suffix(" px")));
                    sui::property_row(ui, tr!("Color"), LABEL_W, |ui| color_field(ui, &mut t.color));
                    sui::property_row(ui, tr!("Align"), LABEL_W, |ui| {
                        sui::segmented(ui, &mut t.align, &[(TextAlign::Left, None, tr!("Left")), (TextAlign::Center, None, tr!("Center")), (TextAlign::Right, None, tr!("Right"))], true);
                    });
                    sui::property_row(ui, tr!("Outline"), LABEL_W, |ui| {
                        ui.horizontal(|ui| {
                            ui.add(sui::Toggle::new(&mut t.outline));
                            ui.add_enabled(t.outline, egui::DragValue::new(&mut t.outline_size).range(0.5..=24.0).speed(0.1).suffix(" px"));
                            ui.add_enabled_ui(t.outline, |ui| ui.color_edit_button_srgba_unmultiplied(&mut t.outline_color));
                        });
                    });
                    sui::property_row(ui, tr!("Background"), LABEL_W, |ui| color_field(ui, &mut t.background));
                    sui::property_row(ui, tr!("Padding"), LABEL_W, |ui| ui.add(egui::DragValue::new(&mut t.padding).range(0..=200).suffix(" px")));
                    sui::property_row(ui, tr!("Wrap"), LABEL_W, |ui| ui.add(egui::DragValue::new(&mut t.wrap_width).range(0..=8192).suffix(" px")));
                }
                SourceKind::Browser(b) => {
                    // Edits go to a draft (a browser restarts on every change): Apply / Enter.
                    let did = egui::Id::new(("browser-draft", id));
                    let (base, mut draft): (crate::model::BrowserSettings, crate::model::BrowserSettings) =
                        ui.data_mut(|d| d.get_temp(did)).filter(|(base, _): &(crate::model::BrowserSettings, _)| base == b).unwrap_or_else(|| (b.clone(), b.clone()));
                    let mut apply = false;
                    sui::property_row(ui, tr!("Address"), LABEL_W, |ui| {
                        ui.horizontal(|ui| {
                            let w = ui.available_width() - 30.0;
                            let r = ui.add(egui::TextEdit::singleline(&mut draft.url).hint_text(tr!("https://… or a local .html file")).desired_width(w));
                            apply |= r.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
                            if ui.add(sui::IconButton::new(Icon::FolderOpen).tooltip(tr!("Choose a local page"))).clicked() {
                                if let Some(f) = pick_file(tr!("Select a page"), &[(tr!("Web pages"), exts(&["html", "htm"])), (tr!("All files"), exts(&["*"]))]) {
                                    draft.url = f;
                                    apply = true;
                                }
                            }
                        });
                    });
                    sui::property_row(ui, tr!("Size"), LABEL_W, |ui| {
                        ui.horizontal(|ui| {
                            ui.add(egui::DragValue::new(&mut draft.width).range(16..=7680).suffix(" px"));
                            ui.label("×");
                            ui.add(egui::DragValue::new(&mut draft.height).range(16..=4320).suffix(" px"));
                        });
                    });
                    sui::property_row(ui, tr!("Frame rate"), LABEL_W, |ui| ui.add(egui::DragValue::new(&mut draft.fps).range(1..=60).suffix(" fps")));
                    sui::property_row(ui, tr!("Audio"), LABEL_W, |ui| ui.add(sui::Toggle::new(&mut draft.audio).label(tr!("play through the mixer"))));
                    sui::property_row(ui, tr!("Off air"), LABEL_W, |ui| ui.add(sui::Toggle::new(&mut draft.pause_when_hidden).label(tr!("pause rendering"))));
                    sui::property_row(ui, tr!("Custom CSS"), LABEL_W, |ui| {
                        ui.add(egui::TextEdit::multiline(&mut draft.css).font(sui::mono(sui::typography::UI_XS)).desired_rows(3).desired_width(f32::INFINITY))
                    });
                    let dirty = draft != base;
                    ui.horizontal(|ui| {
                        if ui.add_enabled(dirty, sui::Button::new(tr!("Apply")).icon(Icon::Check)).clicked() {
                            apply = true;
                        }
                        if ui.add_enabled(dirty, sui::Button::new(tr!("Revert"))).clicked() {
                            draft = base.clone();
                        }
                    });
                    if apply {
                        *b = draft.clone();
                    }
                    ui.data_mut(|d| d.insert_temp(did, (b.clone(), draft)));
                }
                SourceKind::Media(m) => {
                    let media_exts: Vec<String> = mkff_codecs::supported_extensions().map(str::to_string).collect();
                    sui::property_row(ui, tr!("File"), LABEL_W, |ui| path_field(ui, &mut m.path, tr!("Select media"), &[("Media (MKFF)", media_exts), (tr!("All files"), exts(&["*"]))]));
                    sui::property_row(ui, tr!("Loop"), LABEL_W, |ui| ui.add(sui::Toggle::new(&mut m.looping)));
                    sui::property_row(ui, tr!("Restart"), LABEL_W, |ui| ui.add(sui::Toggle::new(&mut m.restart_on_activate).label(tr!("when shown on air"))));
                    sui::property_row(ui, tr!("At end"), LABEL_W, |ui| ui.add(sui::Toggle::new(&mut m.hold_last_frame).label(tr!("keep last frame"))));
                    sui::property_row(ui, tr!("Audio"), LABEL_W, |ui| ui.add(sui::Toggle::new(&mut m.audio).label(tr!("play through the mixer"))));
                    sui::property_row(ui, tr!("Speed"), LABEL_W, |ui| ui.add(egui::Slider::new(&mut m.speed_percent, 25..=200).suffix(" %")));
                }
                #[cfg(windows)]
                SourceKind::Display { monitor, cursor } => {
                    sui::property_row(ui, tr!("Display"), LABEL_W, |ui| {
                        let label = |i: usize, d: &crate::sources::display::MonitorInfo| trf!("{n}: {w}×{h} at {x},{y}{primary}", n = i + 1, w = d.width, h = d.height, x = d.x, y = d.y, primary = if d.primary { tr!(" · Primary") } else { "" });
                        let cur = dev.monitors.get(*monitor).map(|d| label(*monitor, d)).unwrap_or_else(|| trf!("Display {n}", n = *monitor + 1));
                        egui::ComboBox::from_id_salt("monitor").width(ui.available_width()).selected_text(cur).show_ui(ui, |ui| {
                            for (i, d) in dev.monitors.iter().enumerate() {
                                ui.selectable_value(monitor, i, label(i, d));
                            }
                        });
                    });
                    sui::property_row(ui, tr!("Cursor"), LABEL_W, |ui| ui.add(sui::Toggle::new(cursor)));
                }
                #[cfg(windows)]
                SourceKind::Window { target, cursor, method } => {
                    sui::property_row(ui, tr!("Window"), LABEL_W, |ui| {
                        let cur = if target.title.is_empty() { tr!("Select a window").to_string() } else { format!("[{}] {}", target.exe, target.title) };
                        egui::ComboBox::from_id_salt("window").width(ui.available_width()).selected_text(cur).show_ui(ui, |ui| {
                            for w in &dev.windows {
                                if ui.selectable_label(w.target == *target, w.label()).clicked() {
                                    *target = w.target.clone();
                                }
                            }
                        });
                    });
                    sui::property_row(ui, tr!("Cursor"), LABEL_W, |ui| ui.add(sui::Toggle::new(cursor)));
                    use crate::model::CaptureMethod as M;
                    sui::property_row(ui, tr!("Method"), LABEL_W, |ui| {
                        sui::segmented(ui, method, &[(M::Auto, None, tr!("Auto")), (M::Wgc, None, tr!("Graphics Capture")), (M::Gdi, None, tr!("GDI"))], false);
                    });
                    if *method == M::Gdi {
                        sui::hint(ui, tr!("GDI makes the window redraw for every frame: slow, and GPU-heavy windows may lag."));
                    }
                }
                #[cfg(windows)]
                SourceKind::Camera { device, device_name, format } => {
                    sui::property_row(ui, tr!("Device"), LABEL_W, |ui| {
                        let cur = if device_name.is_empty() { tr!("Select a device").to_string() } else { device_name.clone() };
                        egui::ComboBox::from_id_salt("camera").width(ui.available_width()).selected_text(cur).show_ui(ui, |ui| {
                            for c in &dev.cameras {
                                if ui.selectable_label(c.link == *device, &c.name).clicked() && c.link != *device {
                                    *device = c.link.clone();
                                    *device_name = c.name.clone();
                                    *format = None;
                                }
                            }
                        });
                        if dev.cameras.is_empty() {
                            sui::hint(ui, tr!("No video capture devices found"));
                        }
                    });
                    sui::property_row(ui, tr!("Format"), LABEL_W, |ui| {
                        let formats = crate::sources::camera::cached_formats(device).unwrap_or_default();
                        let fmt = |f: &crate::model::CameraFormat| format!("{}×{} @ {:.2} fps", f.width, f.height, f.fps_num as f64 / f.fps_den.max(1) as f64);
                        let cur = format.as_ref().map(fmt).unwrap_or_else(|| tr!("Best up to 1080p").into());
                        egui::ComboBox::from_id_salt("camfmt").width(ui.available_width()).selected_text(cur).show_ui(ui, |ui| {
                            ui.selectable_value(format, None, tr!("Best up to 1080p"));
                            for f in &formats {
                                ui.selectable_value(format, Some(*f), fmt(f));
                            }
                        });
                    });
                }
                SourceKind::AudioInput { device } => {
                    sui::property_row(ui, tr!("Device"), LABEL_W, |ui| device_combo(ui, "in-dev", device, &dev.inputs));
                }
                SourceKind::AudioOutput { device } => {
                    sui::property_row(ui, tr!("Device"), LABEL_W, |ui| device_combo(ui, "out-dev", device, &dev.outputs));
                    sui::hint(ui, tr!("Captures what this playback device plays (WASAPI loopback)."));
                }
                #[cfg(windows)]
                SourceKind::AppAudio { mode, exe, title } => {
                    use crate::model::AppAudioMode as M;
                    sui::property_row(ui, tr!("Capture"), LABEL_W, |ui| {
                        sui::segmented(ui, mode, &[(M::System, None, tr!("All apps")), (M::Only, None, tr!("One app")), (M::AllExcept, None, tr!("All except"))], false);
                    });
                    if *mode != M::System {
                        sui::property_row(ui, tr!("Application"), LABEL_W, |ui| {
                            let cur = if exe.is_empty() { tr!("Select an application").to_string() } else { exe.clone() };
                            egui::ComboBox::from_id_salt("app-audio").width(ui.available_width()).selected_text(cur).show_ui(ui, |ui| {
                                for a in &dev.apps {
                                    if ui.selectable_label(a.exe.eq_ignore_ascii_case(exe), format!("{} — {}", a.exe, a.title)).clicked() {
                                        *exe = a.exe.clone();
                                        *title = a.title.clone();
                                    }
                                }
                            });
                        });
                    }
                    sui::hint(
                        ui,
                        match mode {
                            M::System => tr!("Everything the system plays except LiveBroadcaster itself, whichever device it goes to."),
                            M::Only => tr!("Only this application's sound (with its helper processes); picks it up again when it restarts."),
                            M::AllExcept => tr!("Everything except this application (and LiveBroadcaster)."),
                        },
                    );
                }
                #[cfg(not(windows))]
                _ => {
                    sui::hint(ui, tr!("Not available on this platform"));
                }
            }

            // Monitoring (sources with sound).
            if src.kind.has_audio() {
                sui::section_header(ui, tr!("Audio"));
                let mut mode = src.monitor_mode();
                sui::property_row(ui, tr!("Monitor"), LABEL_W, |ui| {
                    let opts = crate::model::MonitorMode::ALL.map(|m| (m, None, m.name()));
                    if sui::segmented(ui, &mut mode, &opts, false) {
                        src.monitor = Some(mode);
                    }
                });
                sui::hint(
                    ui,
                    match mode {
                        crate::model::MonitorMode::Off => tr!("Goes to the stream / recording only; you don't hear it here."),
                        crate::model::MonitorMode::MonitorOnly => tr!("You hear it on the monitoring device (Settings → Audio); it is left out of the stream / recording."),
                        crate::model::MonitorMode::MonitorAndOutput => tr!("You hear it on the monitoring device (Settings → Audio), and it goes to the stream / recording."),
                    },
                );
                if matches!(src.kind, SourceKind::AudioOutput { .. }) && mode.heard() {
                    sui::hint(ui, tr!("Monitoring a playback-device capture on that same device feeds it back into itself."));
                }
            }

            // Media transport.
            if let (SourceKind::Media(_), Some(ctl)) = (&src.kind, rt.as_ref().and_then(|r| r.shared.media.as_ref())) {
                sui::section_header(ui, tr!("Playback"));
                let info = ctl.info.lock().clone();
                if !info.is_empty() {
                    sui::hint(ui, info);
                }
                let pos = ctl.position_ms.load(Ordering::Relaxed) as f32 / 1000.0;
                let dur = ctl.duration_ms.load(Ordering::Relaxed) as f32 / 1000.0;
                let mut v = self.inspector.seek.unwrap_or(pos);
                let mut frac = if dur > 0.0 { v / dur } else { 0.0 };
                let r = ui.add(sui::Fader::new(&mut frac));
                v = frac * dur;
                if r.dragged() {
                    self.inspector.seek = Some(v);
                }
                if r.drag_stopped() || (r.changed() && !r.dragged()) {
                    ctl.seek(v as f64);
                    self.inspector.seek = None;
                }
                ui.horizontal(|ui| {
                    let paused = ctl.paused.load(Ordering::Relaxed);
                    if ui.add(sui::IconButton::new(if paused { Icon::Play } else { Icon::Pause }).framed(true).tooltip(if paused { tr!("Play") } else { tr!("Pause") })).clicked() {
                        ctl.send(if paused { MediaCommand::Play } else { MediaCommand::Pause });
                    }
                    if ui.add(sui::IconButton::new(Icon::Square).framed(true).tooltip(tr!("Stop"))).clicked() {
                        ctl.send(MediaCommand::Stop);
                    }
                    if ui.add(sui::IconButton::new(Icon::RotateCcw).framed(true).tooltip(tr!("Restart"))).clicked() {
                        ctl.send(MediaCommand::Restart);
                    }
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        ui.label(RichText::new(format!("{} / {}", mmss(v), mmss(dur))).font(sui::mono(sui::typography::UI_XS)).color(p.text_muted));
                    });
                });
                if !rt.as_ref().is_some_and(|r| r.shared.is_active()) {
                    sui::hint(ui, tr!("Plays while the layer is visible in the program scene."));
                }
            }

            // Transform of the selected layer.
            if let Some(item) = item.as_ref().filter(|_| kind.has_video()) {
                self.transform_section(ui, item.id);
            }

            sui::section_header(ui, tr!("Source actions"));
            ui.horizontal(|ui| {
            if matches!(kind, SourceKind::Display { .. } | SourceKind::Window { .. } | SourceKind::Camera { .. } | SourceKind::AudioInput { .. } | SourceKind::AudioOutput { .. } | SourceKind::AppAudio { .. })
                && ui.add(sui::Button::new(tr!("Refresh")).icon(Icon::RefreshCw)).clicked()
            {
                refresh = true;
            }
            if ui.add(sui::Button::new(tr!("Restart")).icon(Icon::RotateCcw)).clicked() {
                restart = true;
            }
            });
        });
        if refresh {
            self.inspector.devices_for = None;
        }
        if restart {
            self.engine.restart_source(id);
        }
    }

    fn transform_section(&mut self, ui: &mut egui::Ui, item_id: Id) {
        let src_size = self.item_source(item_id).and_then(|s| self.source_size(s));
        let mut action = None;
        if let Some(item) = self.item_mut(item_id) {
            sui::section_header(ui, tr!("Transform"));
            sui::property_row(ui, tr!("Position"), LABEL_W, |ui| {
                ui.horizontal(|ui| {
                    ui.add(egui::DragValue::new(&mut item.pos[0]).speed(1.0).prefix("X "));
                    ui.add(egui::DragValue::new(&mut item.pos[1]).speed(1.0).prefix("Y "));
                });
            });
            if let Some(sz) = src_size {
                let [mut w, mut h] = item.display_size(sz);
                let cw = sz.0.saturating_sub(item.crop[0] + item.crop[2]).max(1) as f32;
                let chh = sz.1.saturating_sub(item.crop[1] + item.crop[3]).max(1) as f32;
                sui::property_row(ui, tr!("Size"), LABEL_W, |ui| {
                    ui.horizontal(|ui| {
                        if ui.add(egui::DragValue::new(&mut w).speed(1.0).range(1.0..=16384.0).prefix("W ")).changed() {
                            item.scale[0] = w / cw;
                        }
                        if ui.add(egui::DragValue::new(&mut h).speed(1.0).range(1.0..=16384.0).prefix("H ")).changed() {
                            item.scale[1] = h / chh;
                        }
                    });
                });
            }
            sui::property_row(ui, tr!("Crop"), LABEL_W, |ui| {
                ui.horizontal_wrapped(|ui| {
                    for (i, l) in ["L ", "T ", "R ", "B "].iter().enumerate() {
                        ui.add(egui::DragValue::new(&mut item.crop[i]).speed(1.0).range(0..=8192).prefix(*l));
                    }
                });
            });
            sui::property_row(ui, tr!("Opacity"), LABEL_W, |ui| ui.add(egui::Slider::new(&mut item.opacity, 0..=100).suffix(" %")));
            sui::property_row(ui, "", LABEL_W, |ui| {
                ui.horizontal(|ui| {
                    for (icon, tip, a) in [(Icon::Maximize, tr!("Fit"), ItemAction::Fit), (Icon::Scan, tr!("Stretch"), ItemAction::Stretch), (Icon::Move, tr!("Center"), ItemAction::Center), (Icon::RotateCcw, tr!("Reset"), ItemAction::Reset)] {
                        if ui.add(sui::IconButton::new(icon).framed(true).tooltip(tip)).clicked() {
                            action = Some(a);
                        }
                    }
                });
            });
        }
        if let Some(a) = action {
            self.item_action(item_id, a);
        }
    }
}
