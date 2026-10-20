//! Settings: a modal with a section nav — Stream, Output, Video, Appearance.

use eframe::egui::{self, RichText};
use sphere_ui::{self as sui, Icon};

use super::StudioApp;
use crate::model::{Appearance, EncoderChoice, GpuBackend, EncoderPreset, EncoderProfile, RecordFormat, Settings, StreamService, FRAME_RATES, RESOLUTIONS};

#[derive(Clone, Copy, PartialEq, Eq)]
enum Section {
    Stream,
    Output,
    Video,
    Preview,
    Audio,
    Appearance,
}

pub struct SettingsDialog {
    draft: Settings,
    section: Section,
    show_key: bool,
    /// Hardware encoders found when the dialog opened.
    hw: Vec<mkff_hwenc::EncoderInfo>,
    /// wgpu adapters and the one drawing the window now.
    gpus: Vec<String>,
    gpu_now: String,
    restart: bool,
    /// Playback devices (id, name) for monitoring.
    outputs: Vec<(String, String)>,
}

const LABEL_W: f32 = 140.0;

impl StudioApp {
    pub(crate) fn open_settings(&mut self) {
        self.settings_dialog = Some(SettingsDialog { draft: self.settings.clone(), section: Section::Stream, show_key: false,
            hw: mkff_hwenc::h264_encoders(),
            gpus: self.gpu_adapters.clone(),
            gpu_now: self.gpu_label.clone(),
            restart: false,
            outputs: crate::sources::list_output_devices(),
        });
    }

    pub(crate) fn settings_window(&mut self, ctx: &egui::Context) {
        let Some(mut dlg) = self.settings_dialog.take() else { return };
        let locked = self.engine.outputs.active();
        let p = sui::palette();
        let mut apply = false;
        let mut close = false;
        sui::scrim(ctx);
        egui::Window::new("settings")
            .title_bar(false)
            .collapsible(false)
            .resizable(false)
            .order(egui::Order::Foreground)
            .frame(sui::dialog_frame().inner_margin(egui::Margin::same(0)))
            .anchor(egui::Align2::CENTER_CENTER, egui::vec2(0.0, 0.0))
            .show(ctx, |ui| {
                ui.set_width(760.0);
                ui.set_height(500.0);
                ui.spacing_mut().item_spacing = egui::vec2(0.0, 0.0);
                ui.horizontal_top(|ui| {
                    // Nav.
                    egui::Frame::new()
                        .fill(p.surface_sidebar)
                        .corner_radius(egui::CornerRadius { nw: sui::radius::DIALOG as u8, sw: sui::radius::DIALOG as u8, ne: 0, se: 0 })
                        .inner_margin(egui::Margin::same(sui::space::BASE as i8))
                        .show(ui, |ui| ui.vertical(|ui| {
                            ui.set_width(180.0);
                            ui.set_height(500.0 - 16.0);
                            ui.spacing_mut().item_spacing.y = sui::space::HAIR;
                            ui.add_space(sui::space::TIGHT);
                            ui.horizontal(|ui| {
                                ui.add_space(sui::space::TIGHT);
                                sui::icons::icon(ui, Icon::Settings, 16.0, p.accent_primary);
                                ui.add_space(sui::space::SNUG);
                                ui.label(RichText::new(tr!("Settings")).font(sui::semibold(sui::typography::UI_MD)).color(p.text_primary));
                            });
                            ui.add_space(sui::space::LOOSE);
                            for (s, icon, label) in [
                                (Section::Stream, Icon::Radio, tr!("Stream")),
                                (Section::Output, Icon::HardDrive, tr!("Output")),
                                (Section::Video, Icon::Monitor, tr!("Video")),
                                (Section::Preview, Icon::MonitorPlay, tr!("Preview & GPU")),
                                (Section::Audio, Icon::Headphones, tr!("Audio")),
                                (Section::Appearance, Icon::Palette, tr!("Appearance")),
                            ] {
                                if sui::ListRow::new(label).icon(icon).selected(dlg.section == s).show(ui, |_| {}).clicked() {
                                    dlg.section = s;
                                }
                            }
                        }));
                    // Content.
                    ui.vertical(|ui| {
                        ui.set_width(580.0);
                        egui::Frame::new().inner_margin(egui::Margin::same(sui::space::SECTION as i8)).show(ui, |ui| {
                            ui.set_width(548.0);
                            ui.set_height(500.0 - 32.0 - 52.0);
                            ui.spacing_mut().item_spacing = egui::vec2(6.0, 8.0);
                            ui.horizontal(|ui| {
                                let title = match dlg.section {
                                    Section::Stream => tr!("Stream"),
                                    Section::Output => tr!("Output"),
                                    Section::Video => tr!("Video"),
                                    Section::Preview => tr!("Preview & GPU"),
                                    Section::Audio => tr!("Audio"),
                                    Section::Appearance => tr!("Appearance"),
                                };
                                ui.label(RichText::new(title).font(sui::semibold(sui::typography::UI_LG)).color(p.text_primary));
                                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                                    if ui.add(sui::IconButton::new(Icon::X).tooltip(tr!("Close"))).clicked() {
                                        close = true;
                                    }
                                });
                            });
                            egui::ScrollArea::vertical().auto_shrink([false, false]).show(ui, |ui| match dlg.section {
                                Section::Stream => stream_section(ui, &mut dlg),
                                Section::Output => {
                                    let hw = dlg.hw.clone();
                                    ui.add_enabled_ui(!locked, |ui| output_section(ui, &mut dlg.draft, &hw));
                                }
                                Section::Video => {
                                    ui.add_enabled_ui(!locked, |ui| video_section(ui, &mut dlg.draft));
                                }
                                Section::Appearance => appearance_section(ui, &mut dlg.draft),
                                Section::Audio => {
                                    let outputs = dlg.outputs.clone();
                                    let err = self.engine.shared.monitor.error.lock().clone();
                                    audio_section(ui, &mut dlg.draft, &outputs, err);
                                }
                                Section::Preview => {
                                    let changed = dlg.draft.preview != self.preview_at_start;
                                    let (gpus, now) = (dlg.gpus.clone(), dlg.gpu_now.clone());
                                    if preview_section(ui, &mut dlg.draft, &gpus, &now, changed, locked) {
                                        dlg.restart = true;
                                    }
                                }
                            });
                        });
                        // Footer.
                        let (rect, _) = ui.allocate_exact_size(egui::vec2(580.0, 52.0), egui::Sense::hover());
                        ui.painter().hline(rect.x_range(), rect.top(), egui::Stroke::new(1.0f32, p.divider));
                        let mut f = ui.new_child(egui::UiBuilder::new().max_rect(rect.shrink2(egui::vec2(sui::space::SECTION, 0.0))).layout(egui::Layout::right_to_left(egui::Align::Center)));
                        f.spacing_mut().item_spacing.x = sui::space::BASE;
                        if f.add(sui::Button::primary(tr!("Save")).min_width(88.0).height(sui::size::PROMINENT)).clicked() {
                            apply = true;
                            close = true;
                        }
                        if f.add(sui::Button::new(tr!("Apply")).min_width(72.0).height(sui::size::PROMINENT)).clicked() {
                            apply = true;
                        }
                        if f.add(sui::Button::ghost(tr!("Cancel")).height(sui::size::PROMINENT)).clicked() {
                            close = true;
                        }
                        if locked && dlg.section != Section::Stream && dlg.section != Section::Appearance {
                            f.with_layout(egui::Layout::left_to_right(egui::Align::Center), |ui| {
                                sui::icons::icon(ui, Icon::Lock, 13.0, p.status_warning);
                                ui.label(RichText::new(tr!("Locked while live or recording")).color(p.status_warning));
                            });
                        }
                    });
                });
            });
        if ctx.input(|i| i.key_pressed(egui::Key::Escape)) {
            close = true;
        }
        if apply {
            let mut new = dlg.draft.clone();
            if locked {
                new.output = self.settings.output.clone();
                new.video = self.settings.video.clone();
            }
            self.settings = new;
            self.save_settings();
        }
        if dlg.restart {
            // Save, start a new instance, close this one.
            let mut new = dlg.draft.clone();
            if locked {
                new.output = self.settings.output.clone();
                new.video = self.settings.video.clone();
            }
            self.settings = new;
            self.save_settings();
            self.save_collection();
            match std::env::current_exe().and_then(|exe| std::process::Command::new(exe).spawn()) {
                Ok(_) => ctx.send_viewport_cmd(egui::ViewportCommand::Close),
                Err(e) => self.notify(trf!("Could not restart: {e}", e), true),
            }
            return;
        }
        if !close {
            self.settings_dialog = Some(dlg);
        }
    }
}

/// Returns true when "Restart now" was clicked.
fn preview_section(ui: &mut egui::Ui, d: &mut Settings, gpus: &[String], now: &str, changed: bool, locked: bool) -> bool {
    let p = sui::palette();
    let mut restart = false;
    sui::section_header(ui, tr!("Renderer (wgpu)"));
    sui::property_row(ui, tr!("In use"), LABEL_W, |ui| {
        ui.label(RichText::new(now).font(sui::mono(sui::typography::UI_XS)).color(p.text_secondary));
    });
    sui::property_row(ui, tr!("Preview GPU"), LABEL_W, |ui| {
        let cur = d.preview.adapter.clone().unwrap_or_else(|| tr!("Auto (fastest GPU)").into());
        egui::ComboBox::from_id_salt("gpu").width(320.0).selected_text(cur).show_ui(ui, |ui| {
            ui.selectable_value(&mut d.preview.adapter, None, tr!("Auto (fastest GPU)"));
            for g in gpus {
                ui.selectable_value(&mut d.preview.adapter, Some(g.clone()), g);
            }
        });
        sui::hint(ui, tr!("Pick the GPU your monitor is plugged into: drawing on another GPU adds a copy per frame."));
    });
    sui::property_row(ui, tr!("Graphics API"), LABEL_W, |ui| {
        let choices: Vec<_> = GpuBackend::choices().iter().copied().map(|b| (b, None, b.name())).collect();
        sui::segmented(ui, &mut d.preview.backend, &choices, false);
    });
    if d.preview.backend == GpuBackend::Gl {
        ui.horizontal_wrapped(|ui| {
            sui::icons::icon(ui, Icon::TriangleAlert, 13.0, p.status_warning);
            let hint = if cfg!(windows) {
                tr!("OpenGL is the slowest choice here: the compositor and the window share one GL context, and uploads stall it. Use Auto (DirectX 12) unless the others fail.")
            } else {
                tr!("OpenGL is the slowest choice here: the compositor and the window share one GL context, and uploads stall it. Use Auto (Vulkan) unless the others fail.")
            };
            ui.label(RichText::new(hint).color(p.status_warning));
        });
    }
    sui::property_row(ui, tr!("VSync"), LABEL_W, |ui| {
        ui.add(sui::Toggle::new(&mut d.preview.vsync).label(tr!("Redraw on every vertical blank (smoothest)")));
    });
    if changed {
        ui.add_space(sui::space::BASE);
        ui.horizontal(|ui| {
            sui::icons::icon(ui, Icon::RefreshCw, 13.0, p.status_warning);
            ui.label(RichText::new(tr!("These apply when LiveBroadcaster restarts.")).color(p.status_warning));
            if ui.add_enabled(!locked, sui::Button::primary(tr!("Restart now")).icon(Icon::RotateCcw)).clicked() {
                restart = true;
            }
        });
        if locked {
            sui::hint(ui, tr!("Stop streaming and recording first."));
        }
    }
    restart
}

fn stream_section(ui: &mut egui::Ui, dlg: &mut SettingsDialog) {
    let d = &mut dlg.draft;
    sui::section_header(ui, tr!("Destination"));
    sui::property_row(ui, tr!("Service"), LABEL_W, |ui| {
        egui::ComboBox::from_id_salt("svc").width(300.0).selected_text(d.stream.service.name()).show_ui(ui, |ui| {
            for s in StreamService::ALL {
                ui.selectable_value(&mut d.stream.service, s, s.name());
            }
        });
    });
    sui::property_row(ui, tr!("Server"), LABEL_W, |ui| match d.stream.service.server() {
        Some(url) => {
            ui.label(RichText::new(url).font(sui::mono(sui::typography::UI_XS)).color(sui::palette().text_secondary));
        }
        None => {
            ui.add(egui::TextEdit::singleline(&mut d.stream.server).hint_text(tr!("rtmp://host[:port]/app")).desired_width(300.0));
        }
    });
    sui::property_row(ui, tr!("Stream key"), LABEL_W, |ui| {
        ui.horizontal(|ui| {
            ui.add(egui::TextEdit::singleline(&mut d.stream.key).password(!dlg.show_key).desired_width(264.0));
            let eye = sui::IconButton::new(if dlg.show_key { Icon::EyeOff } else { Icon::Eye }).framed(true).size(sui::size::COMFORTABLE).tooltip(if dlg.show_key { tr!("Hide key") } else { tr!("Show key") });
            if ui.add(eye).clicked() {
                dlg.show_key ^= true;
            }
        });
    });
    sui::hint(ui, tr!("RTMPS (TLS) is supported, e.g. Facebook and Kick. Local test server: rtmp://127.0.0.1/live"));
}

fn output_section(ui: &mut egui::Ui, d: &mut Settings, hw: &[mkff_hwenc::EncoderInfo]) {
    let p = sui::palette();
    sui::section_header(ui, tr!("Video encoder"));
    let found = |c: EncoderChoice| -> Option<&mkff_hwenc::EncoderInfo> {
        let v = match c {
            EncoderChoice::Nvenc => mkff_hwenc::Vendor::Nvidia,
            EncoderChoice::Qsv => mkff_hwenc::Vendor::Intel,
            EncoderChoice::Amf => mkff_hwenc::Vendor::Amd,
            _ => return None,
        };
        hw.iter().find(|e| e.vendor == v)
    };
    sui::property_row(ui, tr!("Encoder"), LABEL_W, |ui| {
        egui::ComboBox::from_id_salt("encoder").width(300.0).selected_text(d.output.encoder.name()).show_ui(ui, |ui| {
            for c in EncoderChoice::ALL {
                let hardware = matches!(c, EncoderChoice::Nvenc | EncoderChoice::Qsv | EncoderChoice::Amf);
                let available = !hardware || found(c).is_some();
                let label = if available { c.name().to_string() } else { trf!("{name} — not found", name = c.name()) };
                ui.add_enabled_ui(available, |ui| {
                    ui.selectable_value(&mut d.output.encoder, c, label);
                });
            }
        });
        let detail = match d.output.encoder {
            EncoderChoice::Auto => match hw.first() {
                Some(e) => trf!("Will use {api} ({name})", api = e.vendor.api_name(), name = e.name),
                None => tr!("No GPU encoder found: software encoding").to_string(),
            },
            EncoderChoice::Software => tr!("Encodes on the CPU (slow at 1080p / 60 fps)").to_string(),
            c => found(c).map(|e| e.name.clone()).unwrap_or_else(|| tr!("Not available on this PC").into()),
        };
        ui.horizontal(|ui| {
            let ok = d.output.encoder == EncoderChoice::Software || hw.first().is_some() && (d.output.encoder == EncoderChoice::Auto || found(d.output.encoder).is_some());
            sui::icons::icon(ui, if ok { Icon::Cpu } else { Icon::TriangleAlert }, 12.0, if ok { p.status_success } else { p.status_warning });
            sui::hint(ui, detail);
        });
    });
    sui::section_header(ui, tr!("Streaming"));
    sui::property_row(ui, tr!("Video bitrate"), LABEL_W, |ui| ui.add(egui::DragValue::new(&mut d.output.video_kbps).range(300..=50_000).speed(50.0).suffix(" kb/s")));
    sui::property_row(ui, tr!("Audio bitrate"), LABEL_W, |ui| {
        egui::ComboBox::from_id_salt("ab").selected_text(format!("{} kb/s AAC", d.output.audio_kbps)).show_ui(ui, |ui| {
            for b in [64, 96, 128, 160, 192, 256, 320] {
                ui.selectable_value(&mut d.output.audio_kbps, b, format!("{b} kb/s"));
            }
        });
    });
    sui::property_row(ui, tr!("Keyframe interval"), LABEL_W, |ui| ui.add(egui::DragValue::new(&mut d.output.keyint_secs).range(1..=10).suffix(" s")));
    sui::property_row(ui, tr!("Preset"), LABEL_W, |ui| {
        sui::segmented(ui, &mut d.output.preset, &[(EncoderPreset::Speed, None, tr!("Speed")), (EncoderPreset::Balanced, None, tr!("Balanced")), (EncoderPreset::Quality, None, tr!("Quality"))], false);
    });
    sui::property_row(ui, tr!("Profile"), LABEL_W, |ui| {
        sui::segmented(ui, &mut d.output.profile, &[(EncoderProfile::Baseline, None, tr!("Baseline")), (EncoderProfile::Main, None, tr!("Main")), (EncoderProfile::High, None, tr!("High"))], false);
    });
    sui::section_header(ui, tr!("Recording"));
    sui::property_row(ui, tr!("Folder"), LABEL_W, |ui| {
        ui.horizontal(|ui| {
            ui.add(egui::TextEdit::singleline(&mut d.output.record_path).desired_width(264.0));
            if ui.add(sui::IconButton::new(Icon::FolderOpen).framed(true).size(sui::size::COMFORTABLE).tooltip(tr!("Browse…"))).clicked() {
                if let Some(p) = rfd::FileDialog::new().set_directory(&d.output.record_path).pick_folder() {
                    d.output.record_path = p.to_string_lossy().into_owned();
                }
            }
        });
    });
    sui::property_row(ui, tr!("Format"), LABEL_W, |ui| {
        egui::ComboBox::from_id_salt("fmt").width(300.0).selected_text(d.output.record_format.name()).show_ui(ui, |ui| {
            for f in [RecordFormat::FragmentedMp4, RecordFormat::Mp4, RecordFormat::Mov] {
                ui.selectable_value(&mut d.output.record_format, f, f.name());
            }
        });
    });
    sui::property_row(ui, tr!("Quality"), LABEL_W, |ui| {
        ui.add(sui::Toggle::new(&mut d.output.record_same_as_stream).label(tr!("Same as stream (shares the encoder)")));
        ui.add_enabled_ui(!d.output.record_same_as_stream, |ui| {
            ui.horizontal(|ui| {
                ui.label(tr!("CRF"));
                ui.add(egui::Slider::new(&mut d.output.record_crf, 14.0..=35.0).fixed_decimals(0));
            });
        });
    });
    sui::hint(ui, tr!("A recording started on its own uses its own quality; started while live, it shares the stream's encoder."));
}

fn video_section(ui: &mut egui::Ui, d: &mut Settings) {
    sui::section_header(ui, tr!("Canvas"));
    sui::property_row(ui, tr!("Resolution"), LABEL_W, |ui| {
        egui::ComboBox::from_id_salt("res").selected_text(format!("{} × {}", d.video.width, d.video.height)).show_ui(ui, |ui| {
            for (w, h) in RESOLUTIONS {
                if ui.selectable_label(d.video.width == w && d.video.height == h, format!("{w} × {h}")).clicked() {
                    d.video.width = w;
                    d.video.height = h;
                }
            }
        });
    });
    sui::property_row(ui, tr!("Custom"), LABEL_W, |ui| {
        ui.horizontal(|ui| {
            ui.add(egui::DragValue::new(&mut d.video.width).range(160..=7680).speed(2.0).prefix("W "));
            ui.add(egui::DragValue::new(&mut d.video.height).range(90..=4320).speed(2.0).prefix("H "));
        });
    });
    d.video.width &= !1;
    d.video.height &= !1;
    sui::property_row(ui, tr!("Frame rate"), LABEL_W, |ui| {
        let opts: Vec<(u32, Option<Icon>, String)> = FRAME_RATES.iter().map(|f| (*f, None, f.to_string())).collect();
        let refs: Vec<(u32, Option<Icon>, &str)> = opts.iter().map(|(v, i, l)| (*v, *i, l.as_str())).collect();
        sui::segmented(ui, &mut d.video.fps, &refs, false);
    });
    sui::hint(ui, tr!("MKFF encodes in software: 1280 × 720 @ 30 is the safe default; 1080p60 needs a fast CPU."));
}

fn audio_section(ui: &mut egui::Ui, d: &mut Settings, outputs: &[(String, String)], err: Option<String>) {
    let p = sui::palette();
    sui::section_header(ui, tr!("Monitoring"));
    sui::property_row(ui, tr!("Device"), LABEL_W, |ui| {
        let cur = match &d.audio.monitor_device {
            None => tr!("Default playback device").to_string(),
            Some(id) => outputs.iter().find(|(i, _)| i == id).map(|(_, n)| n.clone()).unwrap_or_else(|| id.clone()),
        };
        egui::ComboBox::from_id_salt("monitor-device").width(ui.available_width()).selected_text(cur).show_ui(ui, |ui| {
            ui.selectable_value(&mut d.audio.monitor_device, None, tr!("Default playback device"));
            for (id, name) in outputs {
                ui.selectable_value(&mut d.audio.monitor_device, Some(id.clone()), name);
            }
        });
    });
    sui::hint(
        ui,
        tr!("Sources set to Monitor (the headphones button on an audio card) play here. Browser sources are monitored by default, so you hear alerts while live. System Sound capture leaves this out; a playback-device capture of the same device would pick it up again."),
    );
    if let Some(e) = err {
        ui.horizontal_wrapped(|ui| {
            sui::icons::icon(ui, Icon::TriangleAlert, 13.0, p.status_warning);
            ui.label(RichText::new(e).color(p.status_warning));
        });
    }
}

fn appearance_section(ui: &mut egui::Ui, d: &mut Settings) {
    use crate::i18n::Lang;
    sui::section_header(ui, tr!("Language"));
    sui::property_row(ui, tr!("Language"), LABEL_W, |ui| {
        let system = Lang::system();
        let auto = trf!("System ({lang})", lang = system.native_name());
        let cur = d.language.map(|l| l.native_name().to_string()).unwrap_or_else(|| auto.clone());
        egui::ComboBox::from_id_salt("language").width(ui.available_width()).selected_text(cur).show_ui(ui, |ui| {
            ui.selectable_value(&mut d.language, None, auto);
            for l in Lang::ALL {
                ui.selectable_value(&mut d.language, Some(l), l.native_name());
            }
        });
    });
    sui::hint(ui, tr!("English, Thai, Simplified Chinese, Japanese and Indonesian. Applies when you save."));
    sui::section_header(ui, tr!("Theme"));
    sui::property_row(ui, tr!("Theme"), LABEL_W, |ui| {
        sui::segmented(ui, &mut d.appearance, &[(Appearance::Dark, Some(Icon::Moon), tr!("Dark")), (Appearance::Light, Some(Icon::Sun), tr!("Light"))], false);
    });
    sui::hint(ui, tr!("Sphere UI themes from Futureboard Studio (Default / Light). Fonts follow the system UI font."));
}
