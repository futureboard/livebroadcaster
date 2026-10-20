//! Buttons, after Sphere's `fb_button` / `icon_button`: four kinds, the state layers composited
//! over each kind's rest fill, CONTROL radius, focus ring.

use egui::{vec2, Color32, Response, Sense, Stroke, StrokeKind, Ui, Widget, WidgetInfo, WidgetType};

use crate::icons::{self, Icon};
use crate::theme::{palette, regular, semibold};
use crate::tokens::{composite, radius, size, space, state, typography, with_alpha, Palette};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum ButtonKind {
    /// Bordered neutral button.
    #[default]
    Default,
    /// The one main action of a surface (accent fill, inverse text).
    Primary,
    /// Destructive / "on air" action.
    Danger,
    /// No fill until hovered (toolbars).
    Ghost,
}

struct Paint {
    rest: Color32,
    hover: Color32,
    pressed: Color32,
    border: Option<Color32>,
    text: Color32,
}

fn paint_for(kind: ButtonKind, p: &Palette, enabled: bool, selected: bool) -> Paint {
    match kind {
        ButtonKind::Primary => Paint { rest: p.accent_primary, hover: p.accent_hover, pressed: p.accent_pressed, border: None, text: p.on_accent },
        ButtonKind::Danger => {
            let rest = p.status_error;
            Paint {
                rest,
                hover: composite(rest, with_alpha(p.text_primary, 0.12)),
                pressed: composite(rest, p.state_recessed),
                border: None,
                text: p.text_inverse,
            }
        }
        ButtonKind::Ghost => {
            let base = p.surface_panel;
            let rest = if selected { p.tab_bg_active } else { Color32::TRANSPARENT };
            Paint {
                rest,
                hover: composite(if selected { composite(base, p.tab_bg_active) } else { base }, p.state_hover),
                pressed: composite(base, p.state_recessed),
                border: None,
                text: if selected { p.accent_primary } else { p.text_secondary },
            }
        }
        ButtonKind::Default => {
            let rest = if selected { p.button_bg_active } else { p.button_bg };
            Paint {
                rest,
                hover: composite(rest, p.state_hover),
                pressed: composite(rest, p.state_recessed),
                border: Some(if selected { p.border_accent } else { p.button_border }),
                text: if enabled { p.button_text } else { p.text_disabled },
            }
        }
    }
}

/// A text (and optional icon) button.
#[must_use = "add it with ui.add(…)"]
pub struct Button {
    label: String,
    icon: Option<Icon>,
    kind: ButtonKind,
    height: f32,
    min_width: f32,
    fill_width: bool,
    enabled: bool,
    selected: bool,
    strong: bool,
}

impl Button {
    pub fn new(label: impl Into<String>) -> Self {
        Button {
            label: label.into(),
            icon: None,
            kind: ButtonKind::Default,
            height: size::DEFAULT,
            min_width: 0.0,
            fill_width: false,
            enabled: true,
            selected: false,
            strong: false,
        }
    }
    pub fn primary(label: impl Into<String>) -> Self {
        Self::new(label).kind(ButtonKind::Primary).strong(true)
    }
    pub fn danger(label: impl Into<String>) -> Self {
        Self::new(label).kind(ButtonKind::Danger).strong(true)
    }
    pub fn ghost(label: impl Into<String>) -> Self {
        Self::new(label).kind(ButtonKind::Ghost)
    }
    pub fn kind(mut self, k: ButtonKind) -> Self {
        self.kind = k;
        self
    }
    pub fn icon(mut self, i: Icon) -> Self {
        self.icon = Some(i);
        self
    }
    /// Control height from the ladder (`size::DEFAULT`, `COMFORTABLE`, `PROMINENT`).
    pub fn height(mut self, h: f32) -> Self {
        self.height = h;
        self
    }
    pub fn min_width(mut self, w: f32) -> Self {
        self.min_width = w;
        self
    }
    pub fn fill_width(mut self, b: bool) -> Self {
        self.fill_width = b;
        self
    }
    pub fn enabled(mut self, b: bool) -> Self {
        self.enabled = b;
        self
    }
    pub fn selected(mut self, b: bool) -> Self {
        self.selected = b;
        self
    }
    pub fn strong(mut self, b: bool) -> Self {
        self.strong = b;
        self
    }
}

impl Widget for Button {
    fn ui(self, ui: &mut Ui) -> Response {
        let p = palette();
        let paint = paint_for(self.kind, &p, self.enabled, self.selected);
        let font = if self.strong { semibold(typography::UI_SM) } else { regular(typography::UI_SM) };
        let galley = ui.painter().layout_no_wrap(self.label.clone(), font, paint.text);
        let icon_size = if self.height >= size::PROMINENT { 16.0 } else { 14.0 };
        let pad = if self.height >= size::PROMINENT { space::LOOSE } else { space::BASE + 2.0 };
        let has_text = !self.label.is_empty();
        let content = galley.size().x + self.icon.map_or(0.0, |_| icon_size + if has_text { space::SNUG } else { 0.0 });
        let mut w = (content + 2.0 * pad).max(self.min_width);
        if !has_text {
            w = w.max(self.height);
        }
        if self.fill_width {
            w = w.max(ui.available_width());
        }
        let sense = if self.enabled { Sense::click() } else { Sense::hover() };
        let (rect, resp) = ui.allocate_exact_size(vec2(w, self.height), sense);
        resp.widget_info(|| WidgetInfo::labeled(WidgetType::Button, self.enabled, &self.label));
        if ui.is_rect_visible(rect) {
            let fill = if !self.enabled {
                paint.rest
            } else if resp.is_pointer_button_down_on() {
                paint.pressed
            } else if resp.hovered() {
                paint.hover
            } else {
                paint.rest
            };
            let painter = ui.painter();
            let cr = radius::cr(radius::CONTROL);
            painter.rect_filled(rect, cr, fill);
            if let Some(b) = paint.border {
                let b = if resp.hovered() && self.enabled && !self.selected { p.button_border_hover } else { b };
                painter.rect_stroke(rect, cr, Stroke::new(1.0f32, b), StrokeKind::Inside);
            }
            if resp.has_focus() {
                painter.rect_stroke(rect.expand(1.0), radius::cr(radius::CONTROL + 1.0), Stroke::new(state::FOCUS_RING_PX, p.state_focus_ring), StrokeKind::Outside);
            }
            let text_color = if self.enabled { paint.text } else { with_alpha(paint.text, state::DISABLED_CONTENT + 0.2) };
            let mut x = rect.center().x - content / 2.0;
            if let Some(i) = self.icon {
                icons::paint(ui, i, egui::pos2(x + icon_size / 2.0, rect.center().y), icon_size, text_color);
                x += icon_size + if has_text { space::SNUG } else { 0.0 };
            }
            if has_text {
                let pos = egui::pos2(x, rect.center().y - galley.size().y / 2.0);
                ui.painter().galley(pos, galley, text_color);
            }
        }
        if self.enabled {
            resp.on_hover_cursor(egui::CursorIcon::PointingHand)
        } else {
            resp
        }
    }
}

/// A square icon-only button (toolbars, rows). Ghost until hovered; `active` latches it in the
/// accent (or `color`) with wash + border, as Sphere's latched toggles.
#[must_use = "add it with ui.add(…)"]
pub struct IconButton {
    icon: Icon,
    size: f32,
    icon_size: f32,
    active: bool,
    color: Option<Color32>,
    tooltip: Option<String>,
    enabled: bool,
    frame: bool,
}

impl IconButton {
    pub fn new(icon: Icon) -> Self {
        IconButton { icon, size: size::DEFAULT, icon_size: 14.0, active: false, color: None, tooltip: None, enabled: true, frame: false }
    }
    pub fn size(mut self, s: f32) -> Self {
        self.size = s;
        self.icon_size = (s * 0.58).round().clamp(10.0, 20.0);
        self
    }
    pub fn icon_size(mut self, s: f32) -> Self {
        self.icon_size = s;
        self
    }
    pub fn active(mut self, b: bool) -> Self {
        self.active = b;
        self
    }
    /// Glyph colour (and latch colour when active).
    pub fn color(mut self, c: Color32) -> Self {
        self.color = Some(c);
        self
    }
    pub fn tooltip(mut self, t: impl Into<String>) -> Self {
        self.tooltip = Some(t.into());
        self
    }
    pub fn enabled(mut self, b: bool) -> Self {
        self.enabled = b;
        self
    }
    /// Draw the bordered button surface at rest too.
    pub fn framed(mut self, b: bool) -> Self {
        self.frame = b;
        self
    }
}

impl Widget for IconButton {
    fn ui(self, ui: &mut Ui) -> Response {
        let p = palette();
        let sense = if self.enabled { Sense::click() } else { Sense::hover() };
        let (rect, resp) = ui.allocate_exact_size(vec2(self.size, self.size), sense);
        resp.widget_info(|| WidgetInfo::labeled(WidgetType::Button, self.enabled, self.tooltip.as_deref().unwrap_or(self.icon.name())));
        if ui.is_rect_visible(rect) {
            let cr = radius::cr(if self.size <= 20.0 { radius::CONTROL_SM } else { radius::CONTROL });
            let base = if self.frame { p.button_bg } else { Color32::TRANSPARENT };
            let semantic = self.color.unwrap_or(p.accent_primary);
            let (mut fill, border) = if self.active {
                let (f, b) = p.latched(p.surface_panel, semantic);
                (f, Some(b))
            } else {
                (base, self.frame.then_some(p.button_border))
            };
            if self.enabled && resp.is_pointer_button_down_on() {
                fill = composite(composite(p.surface_panel, fill), p.state_recessed);
            } else if self.enabled && resp.hovered() {
                fill = composite(composite(p.surface_panel, fill), p.state_hover);
            }
            ui.painter().rect_filled(rect, cr, fill);
            if let Some(b) = border {
                ui.painter().rect_stroke(rect, cr, Stroke::new(1.0f32, b), StrokeKind::Inside);
            }
            let glyph = if !self.enabled {
                p.text_disabled
            } else if self.active {
                semantic
            } else if let Some(c) = self.color {
                c
            } else if resp.hovered() {
                p.text_primary
            } else {
                p.text_muted
            };
            icons::paint(ui, self.icon, rect.center(), self.icon_size, glyph);
        }
        let resp = if self.enabled { resp.on_hover_cursor(egui::CursorIcon::PointingHand) } else { resp };
        match self.tooltip {
            Some(t) => resp.on_hover_text(t),
            None => resp,
        }
    }
}
