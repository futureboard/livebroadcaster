//! The active theme: a global palette (like Sphere's `Colors`) plus the egui style derived from
//! it, so plain egui widgets (text edits, combo boxes, sliders, menus) match the components.

use std::sync::{Arc, OnceLock};

use egui::{epaint::Shadow, Color32, FontFamily, FontId, Margin, Stroke, TextStyle};
use parking_lot::RwLock;

use crate::tokens::{composite, radius, size, typography, with_alpha, Palette};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum ThemeKind {
    #[default]
    Dark,
    Light,
}

fn store() -> &'static RwLock<Arc<Palette>> {
    static P: OnceLock<RwLock<Arc<Palette>>> = OnceLock::new();
    P.get_or_init(|| RwLock::new(Arc::new(Palette::dark())))
}

/// The active palette.
pub fn palette() -> Arc<Palette> {
    store().read().clone()
}

/// Semibold UI font.
pub fn semibold(size: f32) -> FontId {
    FontId::new(size, FontFamily::Name(crate::fonts::SEMIBOLD.into()))
}

pub fn regular(size: f32) -> FontId {
    FontId::proportional(size)
}

pub fn mono(size: f32) -> FontId {
    FontId::monospace(size)
}

/// Install fonts and a theme (call once at start-up; again to switch theme).
pub fn install(ctx: &egui::Context, kind: ThemeKind) {
    static FONTS: OnceLock<()> = OnceLock::new();
    FONTS.get_or_init(|| {
        crate::fonts::install(ctx);
    });
    let p = match kind {
        ThemeKind::Dark => Palette::dark(),
        ThemeKind::Light => Palette::light(),
    };
    set_palette(ctx, p);
}

/// Use a custom palette (e.g. a Futureboard theme file from disk).
pub fn set_palette(ctx: &egui::Context, p: Palette) {
    apply_style(ctx, &p);
    *store().write() = Arc::new(p);
}

fn apply_style(ctx: &egui::Context, p: &Palette) {
    let mut v = if p.dark { egui::Visuals::dark() } else { egui::Visuals::light() };
    v.override_text_color = None;
    v.panel_fill = p.surface_window;
    v.window_fill = p.surface_panel_raised;
    v.window_stroke = Stroke::new(1.0f32, p.border_normal);
    v.window_corner_radius = radius::cr(radius::DIALOG);
    v.window_shadow = Shadow { offset: [0, 8], blur: 24, spread: 0, color: with_alpha(Color32::BLACK, 0.5) };
    v.popup_shadow = Shadow { offset: [0, 4], blur: 12, spread: 0, color: with_alpha(Color32::BLACK, 0.45) };
    v.menu_corner_radius = radius::cr(radius::SURFACE);
    v.extreme_bg_color = p.surface_input;
    v.faint_bg_color = p.lift(p.surface_panel, 0.03);
    v.code_bg_color = p.surface_input;
    v.hyperlink_color = p.accent_primary;
    v.warn_fg_color = p.status_warning;
    v.error_fg_color = p.status_error;
    v.selection.bg_fill = with_alpha(p.accent_primary, 0.35);
    v.selection.stroke = Stroke::new(1.0f32, p.accent_primary);
    v.text_cursor.stroke = Stroke::new(2.0f32, p.accent_primary);
    v.slider_trailing_fill = true;
    v.handle_shape = egui::style::HandleShape::Circle;
    v.striped = false;

    let cr = radius::cr(radius::CONTROL);
    let w = &mut v.widgets;
    w.noninteractive.bg_fill = p.surface_panel;
    w.noninteractive.weak_bg_fill = p.surface_panel;
    w.noninteractive.bg_stroke = Stroke::new(1.0f32, p.divider);
    w.noninteractive.fg_stroke = Stroke::new(1.0f32, p.text_secondary);
    w.noninteractive.corner_radius = cr;

    w.inactive.bg_fill = p.button_bg;
    w.inactive.weak_bg_fill = p.button_bg;
    w.inactive.bg_stroke = Stroke::new(1.0f32, p.button_border);
    w.inactive.fg_stroke = Stroke::new(1.0f32, p.text_primary);
    w.inactive.corner_radius = cr;
    w.inactive.expansion = 0.0;

    let hover = composite(p.button_bg, p.state_hover);
    w.hovered.bg_fill = hover;
    w.hovered.weak_bg_fill = hover;
    w.hovered.bg_stroke = Stroke::new(1.0f32, p.button_border_hover);
    w.hovered.fg_stroke = Stroke::new(1.0f32, p.text_primary);
    w.hovered.corner_radius = cr;
    w.hovered.expansion = 0.0;

    w.active.bg_fill = p.accent_primary;
    w.active.weak_bg_fill = composite(p.button_bg, p.state_recessed);
    w.active.bg_stroke = Stroke::new(1.0f32, p.border_focus);
    w.active.fg_stroke = Stroke::new(1.0f32, p.text_primary);
    w.active.corner_radius = cr;
    w.active.expansion = 0.0;

    w.open.bg_fill = p.surface_panel_raised;
    w.open.weak_bg_fill = p.surface_panel_raised;
    w.open.bg_stroke = Stroke::new(1.0f32, p.border_accent);
    w.open.fg_stroke = Stroke::new(1.0f32, p.text_primary);
    w.open.corner_radius = cr;

    ctx.set_visuals(v);
    ctx.style_mut(|s| {
        s.text_styles = [
            (TextStyle::Small, regular(typography::UI_XS)),
            (TextStyle::Body, regular(typography::UI_SM)),
            (TextStyle::Button, regular(typography::UI_SM)),
            (TextStyle::Monospace, mono(typography::UI_XS)),
            (TextStyle::Heading, semibold(typography::UI_LG)),
        ]
        .into();
        let sp = &mut s.spacing;
        sp.item_spacing = egui::vec2(6.0, 6.0);
        sp.button_padding = egui::vec2(8.0, 3.0);
        sp.interact_size = egui::vec2(24.0, size::DEFAULT);
        sp.icon_width = 14.0;
        sp.icon_width_inner = 8.0;
        sp.slider_rail_height = 4.0;
        sp.combo_height = 240.0;
        sp.menu_margin = Margin::same(4);
        sp.window_margin = Margin::same(12);
        sp.scroll = egui::style::ScrollStyle::thin();
        s.interaction.selectable_labels = false;
    });
}
