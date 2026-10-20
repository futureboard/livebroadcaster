//! Right sidebar: Audio (a channel card per audio source + master bus), Properties (the
//! inspector) and Health (stream / recording / performance).

use std::sync::atomic::Ordering;
use std::time::{Duration, Instant};

use eframe::egui::{self, pos2, vec2, RichText};
use sphere_ui::{self as sui, Icon};

use super::{hms, RightTab, StudioApp};
use crate::audio::{fader_to_gain, gain_to_db, gain_to_fader};
use crate::model::{Id, SourceKind};
use crate::output::rtmp::StreamState;

const FLOOR_DB: f32 = -60.0;
/// Peak hold before it falls.
const HOLD: Duration = Duration::from_millis(1200);

/// Meter ballistics: instant attack, ~25 dB/s release, peak hold.
fn ballistics(state: &mut (f32, f32, Instant), level: f32, dt: f32) -> (f32, f32) {
    let db = gain_to_db(level).max(FLOOR_DB);
    state.0 = if db > state.0 { db } else { (state.0 - 25.0 * dt).max(db) };
    if db >= state.1 || state.2.elapsed() > HOLD {
        state.1 = if db >= state.1 { db } else { (state.1 - 15.0 * dt).max(db) };
        if db >= state.1 {
            state.2 = Instant::now();
        }
    }
    (state.0, state.1)
}

impl StudioApp {
    pub(crate) fn right_sidebar(&mut self, ui: &mut egui::Ui) {
        let p = sui::palette();
        egui::Frame::new()
            .fill(p.surface_panel)
            .stroke(egui::Stroke::new(1.0f32, p.border_panel))
            .corner_radius(sui::radius::cr(sui::radius::SURFACE))
            .show(ui, |ui| {
                ui.set_width(ui.available_width());
                ui.set_min_height(ui.available_height());
                ui.spacing_mut().item_spacing.y = 0.0;
                sui::tabs(
                    ui,
                    &mut self.right_tab,
                    &[(RightTab::Audio, Icon::AudioLines, tr!("Audio")), (RightTab::Properties, Icon::SlidersHorizontal, tr!("Properties")), (RightTab::Health, Icon::Activity, tr!("Health"))],
                );
                ui.spacing_mut().item_spacing.y = sui::space::SNUG;
                egui::Frame::new().inner_margin(egui::Margin::same(sui::space::BASE as i8)).show(ui, |ui| {
                    ui.set_width(ui.available_width());
                    match self.right_tab {
                        RightTab::Audio => self.audio_tab(ui),
                        RightTab::Properties => self.properties_tab(ui),
                        RightTab::Health => self.health_tab(ui),
                    }
                });
            });
    }

    // -- Audio --------------------------------------------------------------------------------

    fn audio_tab(&mut self, ui: &mut egui::Ui) {
        let p = sui::palette();
        let mut ids: Vec<Id> = self.col.global_audio.clone();
        if let Some(scene) = self.col.scene(self.col.current_scene) {
            for it in &scene.items {
                if !ids.contains(&it.source) && self.col.source(it.source).is_some_and(|s| s.kind.has_audio()) {
                    ids.push(it.source);
                }
            }
        }
        let dt = ui.input(|i| i.stable_dt).min(0.1);
        let mut open_props = None;
        let mut add_global = None;
        let master_h = 92.0;
        egui::ScrollArea::vertical().id_salt("mixer").max_height((ui.available_height() - master_h).max(80.0)).auto_shrink([false, false]).show(ui, |ui| {
            if ids.is_empty() {
                sui::empty_state(ui, Icon::VolumeX, tr!("No audio"), tr!("Add a microphone or desktop audio below"));
            }
            for id in &ids {
                let rt = self.engine.runtime(*id);
                let level = rt.as_ref().and_then(|r| r.shared.audio.as_ref().map(|t| t.level.get())).unwrap_or(0.0);
                let active = rt.as_ref().is_some_and(|r| r.shared.is_active());
                let err = rt.as_ref().and_then(|r| r.shared.error());
                let st = self.meters.entry(*id).or_insert((FLOOR_DB, FLOOR_DB, Instant::now()));
                let (lvl, peak) = ballistics(st, if active { level } else { 0.0 }, dt);
                let global = self.col.global_audio.contains(id);
                let Some(src) = self.col.source_mut(*id) else { continue };
                channel_card(ui, src, global, lvl, peak, err.as_deref(), &mut open_props);
                ui.add_space(sui::space::SNUG);
            }
        });
        // Master bus.
        ui.add_space(sui::space::TIGHT);
        let card = egui::Frame::new()
            .fill(p.surface_panel_raised)
            .stroke(egui::Stroke::new(1.0f32, p.border_subtle))
            .corner_radius(sui::radius::cr(sui::radius::SURFACE))
            .inner_margin(egui::Margin::same(sui::space::BASE as i8));
        card.show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.horizontal(|ui| {
                sui::icons::icon(ui, Icon::AudioLines, 14.0, p.accent_primary);
                ui.label(RichText::new(tr!("Master")).font(sui::semibold(sui::typography::UI_SM)).color(p.text_primary));
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    let add = ui.add(sui::IconButton::new(Icon::Plus).tooltip(tr!("Add a global audio device")));
                    sui::popover(ui, &add, egui::Id::new("add-global-audio"), |ui| {
                        sui::menu_header(ui, tr!("Always-on audio"));
                        if sui::menu_item(ui, Some(Icon::Speaker), tr!("Desktop audio (output capture)"), None).clicked() {
                            add_global = Some(SourceKind::AudioOutput { device: None });
                        }
                        if sui::menu_item(ui, Some(Icon::AudioLines), tr!("System sound (all applications)"), None).clicked() {
                            add_global = Some(SourceKind::AppAudio { mode: crate::model::AppAudioMode::System, exe: String::new(), title: String::new() });
                        }
                        if sui::menu_item(ui, Some(Icon::AppWindow), tr!("Application audio"), None).clicked() {
                            add_global = Some(SourceKind::AppAudio { mode: crate::model::AppAudioMode::Only, exe: String::new(), title: String::new() });
                        }
                        if sui::menu_item(ui, Some(Icon::Mic), tr!("Microphone (input capture)"), None).clicked() {
                            add_global = Some(SourceKind::AudioInput { device: None });
                        }
                    });
                    let db = gain_to_db(self.col.master_volume);
                    ui.label(RichText::new(if db.is_finite() { format!("{db:+.1} dB") } else { "−∞ dB".into() }).font(sui::mono(sui::typography::UI_XS)).color(p.text_muted));
                });
            });
            let levels: Vec<(f32, f32)> = (0..2)
                .map(|c| {
                    let lvl = self.engine.shared.master_level[c].get();
                    ballistics(&mut self.master_meter[c], lvl, dt)
                })
                .collect();
            let w = ui.available_width();
            sui::meter(ui, &[levels[0].0, levels[1].0], &[levels[0].1, levels[1].1], w, false);
            let mut pos = gain_to_fader(self.col.master_volume);
            if ui.add(sui::Fader::new(&mut pos).default_value(gain_to_fader(1.0))).changed() {
                self.col.master_volume = fader_to_gain(pos);
            }
        });
        if let Some(id) = open_props {
            self.open_properties(id);
        }
        if let Some(kind) = add_global {
            let name = match kind {
                SourceKind::AudioOutput { .. } => tr!("Desktop Audio"),
                SourceKind::AppAudio { mode: crate::model::AppAudioMode::System, .. } => tr!("System Sound"),
                SourceKind::AppAudio { .. } => tr!("Application Audio"),
                _ => tr!("Microphone"),
            };
            let id = self.col.add_source(name, kind);
            self.col.global_audio.push(id);
            self.open_properties(id);
        }
    }

    // -- Health -------------------------------------------------------------------------------

    fn health_tab(&mut self, ui: &mut egui::Ui) {
        let p = sui::palette();
        egui::ScrollArea::vertical().id_salt("health").auto_shrink([false, false]).show(ui, |ui| {
            let out = &self.engine.outputs.state;
            let stats = &self.engine.shared.stats;
            // Stream.
            let stream = out.stream.lock().clone();
            health_card(ui, Icon::Radio, tr!("Stream"), |ui| match &stream {
                Some(s) => {
                    let state = s.state.lock().clone();
                    let (txt, c) = match &state {
                        StreamState::Live => (tr!("Live").to_string(), p.status_error),
                        StreamState::Connecting => (tr!("Connecting").to_string(), p.status_warning),
                        StreamState::Reconnecting { attempt } => (trf!("Reconnecting #{attempt}", attempt), p.status_warning),
                        StreamState::Failed(e) => (trf!("Failed: {e}", e), p.status_error),
                        StreamState::Stopped => (tr!("Stopped").to_string(), p.text_muted),
                    };
                    sui::chip(ui, &txt, c, if state == StreamState::Live { sui::ChipStyle::Solid } else { sui::ChipStyle::Soft }, None, true);
                    let sent = s.frames_sent.load(Ordering::Relaxed);
                    let dropped = s.frames_dropped.load(Ordering::Relaxed);
                    let pct = if sent + dropped > 0 { dropped as f32 * 100.0 / (sent + dropped) as f32 } else { 0.0 };
                    stat_grid(ui, &[
                        (tr!("Uptime"), hms(s.live_since.lock().map(|t| t.elapsed()).unwrap_or_default())),
                        (tr!("Bitrate"), format!("{:.0} kb/s", self.bitrate.2)),
                        (tr!("Data sent"), format!("{:.1} MB", s.bytes_sent.load(Ordering::Relaxed) as f64 / 1e6)),
                        (tr!("Dropped"), format!("{dropped} ({pct:.1}%)")),
                        (tr!("Server"), self.settings.stream.server_url()),
                    ]);
                }
                None => {
                    sui::hint(ui, tr!("Not streaming. Set the service and key in Settings → Stream, then Go Live."));
                }
            });
            // Recording.
            let rec = out.recording.lock().as_ref().map(|r| (r.path.clone(), r.since.elapsed()));
            health_card(ui, Icon::CircleDot, tr!("Recording"), |ui| match &rec {
                Some((path, d)) => {
                    let size = std::fs::metadata(path).map(|m| m.len()).unwrap_or(0);
                    stat_grid(ui, &[
                        (tr!("Duration"), hms(*d)),
                        (tr!("Size"), format!("{:.1} MB", size as f64 / 1e6)),
                        (tr!("File"), path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default()),
                    ]);
                }
                None => {
                    sui::hint(ui, trf!("Not recording. Files go to {path}", path = self.settings.output.record_path));
                }
            });
            // Performance.
            health_card(ui, Icon::Gauge, tr!("Performance"), |ui| {
                let enc = out.encoded_frames.load(Ordering::Relaxed);
                let skipped = self.engine.shared.taps.skipped_frames.load(Ordering::Relaxed);
                stat_grid(ui, &[
                    (tr!("CPU"), format!("{:.1} %", self.cpu)),
                    (tr!("Canvas"), format!("{}×{} @ {}", self.settings.video.width, self.settings.video.height, self.settings.video.fps)),
                    (tr!("Render rate"), format!("{:.2} fps", stats.fps.get())),
                    (tr!("Render time"), format!("{:.2} ms", stats.render_ms.get())),
                    (tr!("Missed frames"), stats.lagged.load(Ordering::Relaxed).to_string()),
                    (tr!("Encode time"), format!("{:.2} ms", out.encode_ms.get())),
                    (tr!("Encoded / skipped"), format!("{enc} / {skipped}")),
                    (tr!("Audio resets"), stats.mixer_underruns.load(Ordering::Relaxed).to_string()),
                ]);
                let pr = self.preview.stats.report;
                stat_grid(ui, &[
                    (tr!("Preview"), format!("{:.1} fps (UI {:.1})", pr.fps, pr.ui_fps)),
                    (tr!("Preview gaps"), trf!("worst {ms} ms, missed {missed}", ms = format!("{:.1}", pr.worst_gap_ms), missed = pr.missed)),
                    (tr!("UI frame"), trf!("{ms} ms (+ upload {upload} ms), worst gap {gap} ms", ms = format!("{:.2}", pr.ui_ms), upload = format!("{:.2}", pr.upload_ms), gap = format!("{:.1}", pr.worst_ui_gap_ms))),
                    (tr!("Preview GPU"), self.gpu_label.clone()),
                ]);
                let desc = out.encoder_desc.lock().clone();
                if !desc.is_empty() {
                    sui::hint(ui, desc);
                }
            });
        });
    }
}

fn channel_card(ui: &mut egui::Ui, src: &mut crate::model::SourceConfig, global: bool, lvl: f32, peak: f32, err: Option<&str>, open_props: &mut Option<Id>) {
    let p = sui::palette();
    egui::Frame::new()
        .fill(p.surface_card)
        .stroke(egui::Stroke::new(1.0f32, p.border_subtle))
        .corner_radius(sui::radius::cr(sui::radius::SURFACE))
        .inner_margin(egui::Margin::same(sui::space::BASE as i8))
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.spacing_mut().item_spacing.y = sui::space::SNUG;
            ui.horizontal(|ui| {
                // Global devices (always mixed) carry the accent on their icon.
                let icon_c = if err.is_some() {
                    p.status_warning
                } else if src.muted {
                    p.text_faint
                } else if global {
                    p.accent_primary
                } else {
                    p.text_muted
                };
                sui::icons::icon(ui, if err.is_some() { Icon::TriangleAlert } else { src.kind.icon() }, 14.0, icon_c);
                let name = ui.add(egui::Label::new(RichText::new(&src.name).font(sui::semibold(sui::typography::UI_SM)).color(p.text_primary)).truncate());
                if let Some(e) = err {
                    name.on_hover_text(e);
                }
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if ui.add(sui::IconButton::new(Icon::SlidersHorizontal).size(22.0).tooltip(tr!("Properties"))).clicked() {
                        *open_props = Some(src.id);
                    }
                    // Monitoring: click cycles Output only → Monitor + output → Monitor only.
                    use crate::model::MonitorMode as M;
                    let mode = src.monitor_mode();
                    let mon = sui::IconButton::new(if mode.heard() { Icon::Headphones } else { Icon::HeadphoneOff })
                        .size(22.0)
                        .active(mode.heard())
                        .color(if mode.heard() { p.accent_primary } else { p.text_faint })
                        .tooltip(trf!("Monitoring: {mode} (click to change)", mode = mode.name()));
                    if ui.add(mon).clicked() {
                        src.monitor = Some(match mode {
                            M::Off => M::MonitorAndOutput,
                            M::MonitorAndOutput => M::MonitorOnly,
                            M::MonitorOnly => M::Off,
                        });
                    }
                    let mute = sui::IconButton::new(if src.muted { Icon::VolumeX } else { Icon::Volume2 })
                        .size(22.0)
                        .active(src.muted)
                        .color(if src.muted { p.state_mute } else { p.text_muted })
                        .tooltip(if src.muted { tr!("Unmute") } else { tr!("Mute") });
                    if ui.add(mute).clicked() {
                        src.muted ^= true;
                    }
                });
            });
            let w = ui.available_width();
            sui::meter(ui, &[lvl], &[peak], w, src.muted);
            let db = gain_to_db(src.volume);
            let vol = if db.is_finite() { format!("{db:+.1} dB") } else { "−∞ dB".into() };
            let mut pos = gain_to_fader(src.volume);
            ui.horizontal(|ui| {
                let fader = sui::Fader::new(&mut pos).default_value(gain_to_fader(1.0)).width(ui.available_width() - 62.0);
                let fader = if src.muted { fader.color(p.text_faint) } else { fader };
                if ui.add(fader).changed() {
                    src.volume = fader_to_gain(pos);
                }
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    ui.label(RichText::new(vol).font(sui::mono(sui::typography::UI_XS)).color(p.text_muted));
                });
            });

        });
}

fn health_card(ui: &mut egui::Ui, icon: Icon, title: &str, body: impl FnOnce(&mut egui::Ui)) {
    let p = sui::palette();
    egui::Frame::new()
        .fill(p.surface_card)
        .stroke(egui::Stroke::new(1.0f32, p.border_subtle))
        .corner_radius(sui::radius::cr(sui::radius::SURFACE))
        .inner_margin(egui::Margin::same(sui::space::LOOSE as i8))
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.horizontal(|ui| {
                sui::icons::icon(ui, icon, 14.0, p.accent_primary);
                ui.label(RichText::new(title).font(sui::semibold(sui::typography::UI_SM)).color(p.text_primary));
            });
            body(ui);
        });
    ui.add_space(sui::space::BASE);
}

fn stat_grid(ui: &mut egui::Ui, rows: &[(&str, String)]) {
    let p = sui::palette();
    for (k, v) in rows {
        let (rect, _) = ui.allocate_exact_size(vec2(ui.available_width(), 18.0), egui::Sense::hover());
        let kg = ui.painter().layout_no_wrap(k.to_string(), sui::regular(sui::typography::UI_XS), p.text_muted);
        ui.painter().galley(pos2(rect.left(), rect.center().y - kg.size().y / 2.0), kg, p.text_muted);
        let job = egui::text::LayoutJob {
            wrap: egui::text::TextWrapping::truncate_at_width(rect.width() * 0.62),
            ..egui::text::LayoutJob::simple_singleline(v.clone(), sui::mono(sui::typography::UI_XS), p.text_primary)
        };
        let vg = ui.painter().layout_job(job);
        ui.painter().galley(pos2(rect.right() - vg.size().x, rect.center().y - vg.size().y / 2.0), vg, p.text_primary);
    }
}
