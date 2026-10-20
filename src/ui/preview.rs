//! The preview: the program picture on a wgpu texture, with OBS-style editing — click to select,
//! drag to move (snaps to edges / centre, Ctrl disables), drag handles to resize (corners keep the
//! aspect ratio, Shift frees it), arrows nudge, right-click for the item menu.

use std::sync::atomic::Ordering;

use eframe::egui::{self, pos2, vec2, Color32, Pos2, Rect, Sense, Stroke};
use eframe::egui_wgpu::wgpu;

use sphere_ui::{self as sui, Icon};

use super::StudioApp;
use crate::engine::Shared;
use crate::model::{Id, SceneItem};

/// Preview pacing, measured over one-second windows.
#[derive(Clone, Copy, Debug, Default)]
pub struct PreviewReport {
    /// Program frames shown per second.
    pub fps: f32,
    /// UI frames per second.
    pub ui_fps: f32,
    /// Compositor frames the UI never showed.
    pub missed: u64,
    /// Longest wait between two shown frames.
    pub worst_gap_ms: f32,
    pub upload_ms: f32,
    pub ui_ms: f32,
    /// Longest gap between two UI frames (includes waiting for vsync / present).
    pub worst_ui_gap_ms: f32,
}

pub struct PreviewStats {
    window: std::time::Instant,
    ui_frames: u32,
    uploads: u32,
    missed: u64,
    worst_gap: f32,
    worst_ui_gap: f32,
    upload_ms: f32,
    ui_ms: f32,
    last_upload: std::time::Instant,
    last_ui: std::time::Instant,
    pub report: PreviewReport,
}

impl Default for PreviewStats {
    fn default() -> Self {
        let now = std::time::Instant::now();
        PreviewStats {
            window: now,
            ui_frames: 0,
            uploads: 0,
            missed: 0,
            worst_gap: 0.0,
            worst_ui_gap: 0.0,
            upload_ms: 0.0,
            ui_ms: 0.0,
            last_upload: now,
            last_ui: now,
            report: PreviewReport::default(),
        }
    }
}

impl PreviewStats {
    /// Call once per UI frame with the time the frame's work took.
    pub fn ui_frame(&mut self, ui_ms: f32) {
        let now = std::time::Instant::now();
        self.worst_ui_gap = self.worst_ui_gap.max((now - self.last_ui).as_secs_f32() * 1000.0);
        self.last_ui = now;
        self.ui_frames += 1;
        self.ui_ms += ui_ms;
        let el = (now - self.window).as_secs_f32();
        if el >= 1.0 {
            self.report = PreviewReport {
                fps: self.uploads as f32 / el,
                ui_fps: self.ui_frames as f32 / el,
                missed: self.missed,
                worst_gap_ms: self.worst_gap,
                upload_ms: if self.uploads > 0 { self.upload_ms / self.uploads as f32 } else { 0.0 },
                ui_ms: self.ui_ms / self.ui_frames.max(1) as f32,
                worst_ui_gap_ms: self.worst_ui_gap,
            };
            if std::env::var_os("LB_PERF").is_some() {
                log::info!("preview: {:?}", self.report);
            }
            *self = PreviewStats { report: self.report, last_upload: self.last_upload, last_ui: now, ..PreviewStats::default() };
        }
    }

    fn uploaded(&mut self, skipped: u64, ms: f32) {
        let now = std::time::Instant::now();
        self.worst_gap = self.worst_gap.max((now - self.last_upload).as_secs_f32() * 1000.0);
        self.last_upload = now;
        self.uploads += 1;
        self.missed += skipped;
        self.upload_ms += ms;
    }
}

/// The program picture as an egui texture: the GPU compositor's texture itself, or (CPU
/// compositor) a texture refreshed from its frames.
#[derive(Default)]
pub struct PreviewTexture {
    tex: Option<(wgpu::Texture, egui::TextureId, u32, u32)>,
    /// The GPU compositor's program texture registered with egui: (generation, id).
    gpu: Option<(u64, egui::TextureId)>,
    seq: u64,
    pub stats: PreviewStats,
}

impl PreviewTexture {
    pub fn update(&mut self, frame: &eframe::Frame, shared: &Shared) {
        let seq = shared.program_seq.load(Ordering::Relaxed);
        if seq == self.seq {
            return;
        }
        let skipped = seq.saturating_sub(self.seq + 1);
        self.seq = seq;
        let t0 = std::time::Instant::now();
        let Some(rs) = frame.wgpu_render_state() else { return };
        if let Some(pt) = shared.program_gpu.lock().clone() {
            if self.gpu.is_none_or(|(g, _)| g != pt.generation) {
                let mut renderer = rs.renderer.write();
                let id = match self.gpu {
                    Some((_, id)) => {
                        renderer.update_egui_texture_from_wgpu_texture(&rs.device, &pt.view, wgpu::FilterMode::Linear, id);
                        id
                    }
                    None => renderer.register_native_texture(&rs.device, &pt.view, wgpu::FilterMode::Linear),
                };
                self.gpu = Some((pt.generation, id));
            }
            self.stats.uploaded(skipped, t0.elapsed().as_secs_f32() * 1000.0);
            return;
        }
        let Some(img) = shared.program.lock().clone() else { return };
        let (w, h) = (img.width, img.height);
        if !matches!(self.tex, Some((_, _, tw, th)) if tw == w && th == h) {
            let texture = rs.device.create_texture(&wgpu::TextureDescriptor {
                label: Some("program"),
                size: wgpu::Extent3d { width: w, height: h, depth_or_array_layers: 1 },
                mip_level_count: 1,
                sample_count: 1,
                dimension: wgpu::TextureDimension::D2,
                format: wgpu::TextureFormat::Rgba8UnormSrgb,
                usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
                view_formats: &[],
            });
            let view = texture.create_view(&wgpu::TextureViewDescriptor::default());
            let mut renderer = rs.renderer.write();
            let id = match &self.tex {
                Some((_, id, _, _)) => {
                    renderer.update_egui_texture_from_wgpu_texture(&rs.device, &view, wgpu::FilterMode::Linear, *id);
                    *id
                }
                None => renderer.register_native_texture(&rs.device, &view, wgpu::FilterMode::Linear),
            };
            self.tex = Some((texture, id, w, h));
        }
        let (texture, ..) = self.tex.as_ref().unwrap();
        rs.queue.write_texture(
            wgpu::TexelCopyTextureInfo { texture, mip_level: 0, origin: wgpu::Origin3d::ZERO, aspect: wgpu::TextureAspect::All },
            &img.data,
            wgpu::TexelCopyBufferLayout { offset: 0, bytes_per_row: Some(4 * w), rows_per_image: Some(h) },
            wgpu::Extent3d { width: w, height: h, depth_or_array_layers: 1 },
        );
        self.stats.uploaded(skipped, t0.elapsed().as_secs_f32() * 1000.0);
    }

    pub fn texture_id(&self) -> Option<egui::TextureId> {
        self.gpu.map(|g| g.1).or(self.tex.as_ref().map(|t| t.1))
    }
}

#[derive(Clone, Copy, Debug)]
pub enum DragMode {
    Move,
    /// Handle position as fractions of the rectangle (0, ½, 1).
    Resize { hx: f32, hy: f32 },
}

#[derive(Clone, Debug)]
pub struct Drag {
    item: Id,
    mode: DragMode,
    start_mouse: Pos2,
    start_pos: [f32; 2],
    start_size: [f32; 2],
    src: (u32, u32),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ItemAction {
    Reset,
    Fit,
    Stretch,
    Center,
    Up,
    Down,
    Top,
    Bottom,
    ToggleVisible,
    ToggleLock,
    Remove,
}

const HANDLES: [(f32, f32); 8] = [(0.0, 0.0), (0.5, 0.0), (1.0, 0.0), (1.0, 0.5), (1.0, 1.0), (0.5, 1.0), (0.0, 1.0), (0.0, 0.5)];

/// Fit the item inside the canvas keeping its aspect ratio, centred.
pub fn fit_item(item: &mut SceneItem, src: (u32, u32), canvas: (f32, f32)) {
    let w = src.0.saturating_sub(item.crop[0] + item.crop[2]).max(1) as f32;
    let h = src.1.saturating_sub(item.crop[1] + item.crop[3]).max(1) as f32;
    let s = (canvas.0 / w).min(canvas.1 / h);
    item.scale = [s, s];
    item.pos = [((canvas.0 - w * s) / 2.0).round(), ((canvas.1 - h * s) / 2.0).round()];
}

impl StudioApp {
    pub(crate) fn source_size(&self, source: Id) -> Option<(u32, u32)> {
        self.engine.runtime(source).and_then(|r| r.shared.video.size())
    }

    /// Canvas rectangle of an item, when its source has a picture.
    fn item_rect(&self, item: &SceneItem) -> Option<Rect> {
        let size = self.source_size(item.source)?;
        let [w, h] = item.display_size(size);
        Some(Rect::from_min_size(pos2(item.pos[0], item.pos[1]), vec2(w, h)))
    }

    pub(crate) fn item_action(&mut self, item_id: Id, action: ItemAction) {
        let canvas = (self.settings.video.width as f32, self.settings.video.height as f32);
        let scene_id = self.col.current_scene;
        let size = self
            .col
            .scene(scene_id)
            .and_then(|s| s.items.iter().find(|i| i.id == item_id))
            .and_then(|i| self.source_size(i.source));
        let Some(scene) = self.col.scene_mut(scene_id) else { return };
        let Some(idx) = scene.items.iter().position(|i| i.id == item_id) else { return };
        match action {
            ItemAction::Reset => {
                let it = &mut scene.items[idx];
                it.pos = [0.0, 0.0];
                it.scale = [1.0, 1.0];
                it.crop = [0; 4];
            }
            ItemAction::Fit => {
                if let Some(sz) = size {
                    fit_item(&mut scene.items[idx], sz, canvas);
                }
            }
            ItemAction::Stretch => {
                if let Some(sz) = size {
                    let it = &mut scene.items[idx];
                    let w = sz.0.saturating_sub(it.crop[0] + it.crop[2]).max(1) as f32;
                    let h = sz.1.saturating_sub(it.crop[1] + it.crop[3]).max(1) as f32;
                    it.scale = [canvas.0 / w, canvas.1 / h];
                    it.pos = [0.0, 0.0];
                }
            }
            ItemAction::Center => {
                if let Some(sz) = size {
                    let it = &mut scene.items[idx];
                    let [w, h] = it.display_size(sz);
                    it.pos = [((canvas.0 - w) / 2.0).round(), ((canvas.1 - h) / 2.0).round()];
                }
            }
            // The list is bottom → top, so "up" moves towards the end.
            ItemAction::Up => {
                if idx + 1 < scene.items.len() {
                    scene.items.swap(idx, idx + 1);
                }
            }
            ItemAction::Down => {
                if idx > 0 {
                    scene.items.swap(idx, idx - 1);
                }
            }
            ItemAction::Top => {
                let it = scene.items.remove(idx);
                scene.items.push(it);
            }
            ItemAction::Bottom => {
                let it = scene.items.remove(idx);
                scene.items.insert(0, it);
            }
            ItemAction::ToggleVisible => scene.items[idx].visible ^= true,
            ItemAction::ToggleLock => scene.items[idx].locked ^= true,
            ItemAction::Remove => {
                scene.items.remove(idx);
                if self.selected_item == Some(item_id) {
                    self.selected_item = None;
                }
                self.col.prune_orphans();
            }
        }
    }

    /// The layer context menu (stage and Layers list).
    pub(crate) fn item_menu(&mut self, ui: &mut egui::Ui, item_id: Id) {
        let Some(item) = self.col.scene(self.col.current_scene).and_then(|s| s.items.iter().find(|i| i.id == item_id)).cloned() else {
            ui.close_menu();
            return;
        };
        let p = sui::palette();
        let has_video = self.col.source(item.source).is_some_and(|s| s.kind.has_video());
        let act = |ui: &mut egui::Ui, icon: Icon, label: &str, a: ItemAction, app: &mut StudioApp| {
            if sui::menu_item(ui, Some(icon), label, None).clicked() {
                app.item_action(item_id, a);
                ui.close_menu();
            }
        };
        if sui::menu_item(ui, Some(Icon::SlidersHorizontal), tr!("Properties"), None).clicked() {
            self.select_item(Some(item_id));
            self.open_properties(item.source);
            ui.close_menu();
        }
        ui.separator();
        if has_video {
            act(ui, Icon::Maximize, tr!("Fit to Canvas"), ItemAction::Fit, self);
            act(ui, Icon::Scan, tr!("Stretch to Canvas"), ItemAction::Stretch, self);
            act(ui, Icon::Move, tr!("Center"), ItemAction::Center, self);
            act(ui, Icon::RotateCcw, tr!("Reset Transform"), ItemAction::Reset, self);
            ui.separator();
        }
        act(ui, Icon::ArrowUpToLine, tr!("Bring to Front"), ItemAction::Top, self);
        act(ui, Icon::ChevronUp, tr!("Move Up"), ItemAction::Up, self);
        act(ui, Icon::ChevronDown, tr!("Move Down"), ItemAction::Down, self);
        act(ui, Icon::ArrowDownToLine, tr!("Send to Back"), ItemAction::Bottom, self);
        ui.separator();
        act(ui, if item.visible { Icon::EyeOff } else { Icon::Eye }, if item.visible { tr!("Hide") } else { tr!("Show") }, ItemAction::ToggleVisible, self);
        act(ui, if item.locked { Icon::LockOpen } else { Icon::Lock }, if item.locked { tr!("Unlock") } else { tr!("Lock") }, ItemAction::ToggleLock, self);
        ui.separator();
        if sui::menu_item_colored(ui, Some(Icon::Trash2), tr!("Remove"), Some("Del"), Some(p.status_error), true).clicked() {
            self.item_action(item_id, ItemAction::Remove);
            ui.close_menu();
        }
    }

    /// The stage: the program picture in a canvas well, and the transition bar under it.
    pub(crate) fn stage(&mut self, ui: &mut egui::Ui) {
        let p = sui::palette();
        let full = ui.available_rect_before_wrap();
        let bar_h = sui::size::PROMINENT + 8.0;
        let well = Rect::from_min_max(full.min, pos2(full.max.x, full.max.y - bar_h - sui::space::BASE));
        ui.painter().rect_filled(well, sui::radius::cr(sui::radius::SURFACE), p.surface_canvas);
        ui.painter().rect_stroke(well, sui::radius::cr(sui::radius::SURFACE), Stroke::new(1.0f32, p.border_panel), egui::StrokeKind::Inside);
        self.preview_canvas(ui, well.shrink(sui::space::LOOSE));
        // Transition bar.
        let bar = Rect::from_min_max(pos2(full.min.x, well.max.y + sui::space::BASE), full.max);
        ui.painter().rect_filled(bar, sui::radius::cr(sui::radius::SURFACE), p.surface_panel);
        ui.painter().rect_stroke(bar, sui::radius::cr(sui::radius::SURFACE), Stroke::new(1.0f32, p.border_panel), egui::StrokeKind::Inside);
        let inner = bar.shrink2(vec2(sui::space::LOOSE, 0.0));
        let mut bui = ui.new_child(egui::UiBuilder::new().max_rect(inner).layout(egui::Layout::left_to_right(egui::Align::Center)));
        bui.spacing_mut().item_spacing.x = sui::space::BASE;
        sui::icons::icon(&mut bui, Icon::Blend, 14.0, p.text_muted);
        bui.label(egui::RichText::new(tr!("Transition")).color(p.text_muted));
        use crate::model::TransitionKind as T;
        sui::segmented(&mut bui, &mut self.col.transition, &[(T::Cut, Some(Icon::Scissors), tr!("Cut")), (T::Fade, Some(Icon::Blend), tr!("Fade"))], false);
        bui.add_enabled_ui(self.col.transition == T::Fade, |ui| {
            ui.add(egui::DragValue::new(&mut self.col.transition_ms).range(50..=5000).speed(10.0).suffix(" ms"));
        });
        bui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            let v = &self.settings.video;
            sui::chip(ui, &format!("{}×{} · {} fps", v.width, v.height, v.fps), p.text_muted, sui::ChipStyle::Outline, Some(Icon::Monitor), false);
            if let Some(id) = self.selected_item {
                if ui.add(sui::IconButton::new(Icon::Maximize).tooltip(tr!("Fit selected layer to canvas"))).clicked() {
                    self.item_action(id, ItemAction::Fit);
                }
                if ui.add(sui::IconButton::new(Icon::Move).tooltip(tr!("Center selected layer"))).clicked() {
                    self.item_action(id, ItemAction::Center);
                }
            }
        });
    }

    fn preview_canvas(&mut self, ui: &mut egui::Ui, area: Rect) {
        let pal = sui::palette();
        let (cw, ch) = (self.settings.video.width as f32, self.settings.video.height as f32);
        let scale = (area.width() / cw).min(area.height() / ch).max(0.01);
        let rect = Rect::from_center_size(area.center(), vec2(cw * scale, ch * scale));
        let resp = ui.interact(area, ui.id().with("preview"), Sense::click_and_drag());
        let painter = ui.painter_at(area);
        painter.rect_filled(rect, 0.0, Color32::BLACK);
        if let Some(id) = self.preview.texture_id() {
            painter.image(id, rect, Rect::from_min_max(pos2(0.0, 0.0), pos2(1.0, 1.0)), Color32::WHITE);
        }
        let to_canvas = |p: Pos2| pos2((p.x - rect.min.x) / scale, (p.y - rect.min.y) / scale);
        let to_screen = |r: Rect| Rect::from_min_max(rect.min + r.min.to_vec2() * scale, rect.min + r.max.to_vec2() * scale);

        let items: Vec<SceneItem> = self.col.scene(self.col.current_scene).map(|s| s.items.clone()).unwrap_or_default();
        let selected = self.selected_item.and_then(|id| items.iter().find(|i| i.id == id)).cloned();

        // Pointer hit tests (topmost first).
        let pointer = resp.interact_pointer_pos().or_else(|| resp.hover_pos());
        let hit_item = |app: &StudioApp, p: Pos2| -> Option<Id> {
            let c = to_canvas(p);
            items.iter().rev().find(|i| i.visible && app.item_rect(i).is_some_and(|r| r.contains(c))).map(|i| i.id)
        };
        let hit_handle = |app: &StudioApp, p: Pos2| -> Option<(f32, f32)> {
            let s = selected.as_ref()?;
            if s.locked {
                return None;
            }
            let r = to_screen(app.item_rect(s)?);
            HANDLES.iter().copied().find(|(hx, hy)| {
                let hp = pos2(r.min.x + r.width() * hx, r.min.y + r.height() * hy);
                hp.distance(p) <= 7.0
            })
        };

        // Cursor feedback.
        if let Some(p) = resp.hover_pos() {
            if let Some((hx, hy)) = hit_handle(self, p) {
                ui.ctx().set_cursor_icon(resize_cursor(hx, hy));
            } else if self.drag.as_ref().is_some_and(|d| matches!(d.mode, DragMode::Move)) {
                ui.ctx().set_cursor_icon(egui::CursorIcon::Grabbing);
            }
        }

        if resp.drag_started_by(egui::PointerButton::Primary) {
            if let Some(p) = pointer {
                let press = ui.input(|i| i.pointer.press_origin()).unwrap_or(p);
                if let Some((hx, hy)) = hit_handle(self, press) {
                    let s = selected.as_ref().unwrap();
                    let src = self.source_size(s.source).unwrap_or((1, 1));
                    self.drag = Some(Drag {
                        item: s.id,
                        mode: DragMode::Resize { hx, hy },
                        start_mouse: to_canvas(press),
                        start_pos: s.pos,
                        start_size: s.display_size(src),
                        src,
                    });
                } else if let Some(id) = hit_item(self, press) {
                    self.select_item(Some(id));
                    let it = items.iter().find(|i| i.id == id).unwrap();
                    if !it.locked {
                        let src = self.source_size(it.source).unwrap_or((1, 1));
                        self.drag = Some(Drag {
                            item: id,
                            mode: DragMode::Move,
                            start_mouse: to_canvas(press),
                            start_pos: it.pos,
                            start_size: it.display_size(src),
                            src,
                        });
                    }
                } else {
                    self.select_item(None);
                }
            }
        }
        if resp.dragged_by(egui::PointerButton::Primary) {
            if let (Some(d), Some(p)) = (self.drag.clone(), pointer) {
                let m = to_canvas(p);
                let (ctrl, shift) = ui.input(|i| (i.modifiers.ctrl, i.modifiers.shift));
                self.apply_drag(&d, m, ctrl, shift, scale);
            }
        }
        if resp.drag_stopped() {
            self.drag = None;
        }
        if resp.clicked() {
            if let Some(p) = pointer {
                let hit = hit_item(self, p);
                self.select_item(hit);
            }
        }
        if resp.secondary_clicked() {
            if let Some(p) = pointer {
                if let Some(id) = hit_item(self, p) {
                    self.select_item(Some(id));
                }
            }
        }
        if resp.double_clicked() {
            if let Some(id) = pointer.and_then(|p| hit_item(self, p)) {
                if let Some(src) = items.iter().find(|i| i.id == id).map(|i| i.source) {
                    self.open_properties(src);
                }
            }
        }
        let menu_item = self.selected_item;
        resp.context_menu(|ui| match menu_item {
            Some(id) => self.item_menu(ui, id),
            None => {
                ui.label(tr!("No source selected"));
            }
        });

        // Keyboard: nudge / delete, while the preview has the pointer and no text field has focus.
        if resp.hovered() && !ui.ctx().wants_keyboard_input() {
            if let Some(id) = self.selected_item {
                let step = if ui.input(|i| i.modifiers.shift) { 10.0 } else { 1.0 };
                let (dx, dy, del) = ui.input(|i| {
                    let k = |key| i.key_pressed(key);
                    (
                        (k(egui::Key::ArrowRight) as i32 - k(egui::Key::ArrowLeft) as i32) as f32 * step,
                        (k(egui::Key::ArrowDown) as i32 - k(egui::Key::ArrowUp) as i32) as f32 * step,
                        k(egui::Key::Delete),
                    )
                });
                if dx != 0.0 || dy != 0.0 {
                    if let Some(it) = self.item_mut(id).filter(|i| !i.locked) {
                        it.pos[0] += dx;
                        it.pos[1] += dy;
                    }
                }
                if del {
                    self.item_action(id, ItemAction::Remove);
                }
            }
        }

        // Selection overlay.
        let items: Vec<SceneItem> = self.col.scene(self.col.current_scene).map(|s| s.items.clone()).unwrap_or_default();
        let painter = ui.painter_at(area);
        if let Some(p) = resp.hover_pos() {
            if self.drag.is_none() {
                if let Some(h) = hit_item(self, p).filter(|h| Some(*h) != self.selected_item) {
                    if let Some(r) = items.iter().find(|i| i.id == h).and_then(|i| self.item_rect(i)) {
                        painter.rect_stroke(to_screen(r), 0.0, Stroke::new(1.0f32, sui::with_alpha(pal.accent_primary, 0.6)), egui::StrokeKind::Middle);
                    }
                }
            }
        }
        if let Some(sel) = self.selected_item.and_then(|id| items.iter().find(|i| i.id == id)) {
            if let Some(r) = self.item_rect(sel) {
                let sr = to_screen(r);
                let color = if sel.locked { pal.status_warning } else { pal.accent_primary };
                painter.rect_stroke(sr, 0.0, Stroke::new(1.5f32, color), egui::StrokeKind::Middle);
                if !sel.locked {
                    for (hx, hy) in HANDLES {
                        let hp = pos2(sr.min.x + sr.width() * hx, sr.min.y + sr.height() * hy);
                        let hr = Rect::from_center_size(hp, vec2(8.0, 8.0));
                        painter.rect_filled(hr, 2.0, Color32::WHITE);
                        painter.rect_stroke(hr, 2.0, Stroke::new(1.5f32, color), egui::StrokeKind::Middle);
                    }
                }
                // Size readout while dragging.
                if let Some(d) = &self.drag {
                    let txt = match d.mode {
                        DragMode::Move => format!("{:.0}, {:.0}", sel.pos[0], sel.pos[1]),
                        DragMode::Resize { .. } => format!("{:.0} × {:.0}", r.width(), r.height()),
                    };
                    painter.text(sr.left_top() + vec2(4.0, -16.0), egui::Align2::LEFT_TOP, txt, egui::FontId::monospace(12.0), Color32::WHITE);
                }
            }
        }
        // Canvas border.
        painter.rect_stroke(rect, 0.0, Stroke::new(1.0f32, Color32::from_gray(60)), egui::StrokeKind::Outside);
        if items.is_empty() {
            painter.text(
                rect.center(),
                egui::Align2::CENTER_CENTER,
                tr!("Add a layer with + in the Layers panel"),
                egui::FontId::proportional(18.0),
                pal.text_faint,
            );
        }
    }

    pub(crate) fn item_mut(&mut self, id: Id) -> Option<&mut SceneItem> {
        let scene = self.col.current_scene;
        self.col.scene_mut(scene)?.items.iter_mut().find(|i| i.id == id)
    }

    fn apply_drag(&mut self, d: &Drag, m: Pos2, no_snap: bool, free_aspect: bool, view_scale: f32) {
        let canvas = (self.settings.video.width as f32, self.settings.video.height as f32);
        let snap_dist = 10.0 / view_scale.max(0.1) * 0.5 + 4.0;
        let Some(item) = self.item_mut(d.item) else { return };
        let crop_w = d.src.0.saturating_sub(item.crop[0] + item.crop[2]).max(1) as f32;
        let crop_h = d.src.1.saturating_sub(item.crop[1] + item.crop[3]).max(1) as f32;
        let delta = m - d.start_mouse;
        match d.mode {
            DragMode::Move => {
                let mut x = d.start_pos[0] + delta.x;
                let mut y = d.start_pos[1] + delta.y;
                if !no_snap {
                    let [w, h] = d.start_size;
                    x = snap(x, w, canvas.0, snap_dist);
                    y = snap(y, h, canvas.1, snap_dist);
                }
                item.pos = [x.round(), y.round()];
            }
            DragMode::Resize { hx, hy } => {
                let [x0, y0] = d.start_pos;
                let [w0, h0] = d.start_size;
                let (left, right) = (x0, x0 + w0);
                let (top, bottom) = (y0, y0 + h0);
                let mut w = if hx == 1.0 {
                    m.x - left
                } else if hx == 0.0 {
                    right - m.x
                } else {
                    w0
                };
                let mut h = if hy == 1.0 {
                    m.y - top
                } else if hy == 0.0 {
                    bottom - m.y
                } else {
                    h0
                };
                // Snap the moving edge to the canvas edges.
                if !no_snap {
                    if hx == 1.0 && (left + w - canvas.0).abs() < snap_dist {
                        w = canvas.0 - left;
                    }
                    if hx == 0.0 && (right - w).abs() < snap_dist {
                        w = right;
                    }
                    if hy == 1.0 && (top + h - canvas.1).abs() < snap_dist {
                        h = canvas.1 - top;
                    }
                    if hy == 0.0 && (bottom - h).abs() < snap_dist {
                        h = bottom;
                    }
                }
                let corner = hx != 0.5 && hy != 0.5;
                if corner && !free_aspect && w0 > 0.0 && h0 > 0.0 {
                    let s = (w / w0).max(h / h0);
                    w = w0 * s;
                    h = h0 * s;
                }
                let w = w.max(4.0);
                let h = h.max(4.0);
                let x = if hx == 0.0 { right - w } else { left };
                let y = if hy == 0.0 { bottom - h } else { top };
                item.scale = [w / crop_w, h / crop_h];
                item.pos = [x, y];
            }
        }
    }
}

fn snap(pos: f32, size: f32, canvas: f32, d: f32) -> f32 {
    let candidates = [(pos, 0.0), (pos + size, canvas), (pos + size / 2.0, canvas / 2.0)];
    for (edge, target) in candidates {
        if (edge - target).abs() < d {
            return pos + (target - edge);
        }
    }
    pos
}

fn resize_cursor(hx: f32, hy: f32) -> egui::CursorIcon {
    match (hx, hy) {
        (0.5, _) => egui::CursorIcon::ResizeVertical,
        (_, 0.5) => egui::CursorIcon::ResizeHorizontal,
        (x, y) if x == y => egui::CursorIcon::ResizeNwSe,
        _ => egui::CursorIcon::ResizeNeSw,
    }
}
