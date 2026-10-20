//! Lucide icons for egui: each SVG is rasterised once per pixel size (resvg) in white and
//! tinted when painted, so one texture serves every colour.

use std::collections::HashMap;
use std::sync::Arc;

use egui::{Color32, Pos2, Rect, Response, Sense, Ui, Vec2};
use parking_lot::Mutex;

pub use crate::icon_set::Icon;

#[derive(Clone, Default)]
struct Cache(Arc<Mutex<HashMap<(Icon, u32), egui::TextureHandle>>>);

fn rasterize(icon: Icon, px: u32) -> Option<egui::ColorImage> {
    use resvg::{tiny_skia, usvg};
    // Lucide strokes with currentColor; draw white and tint at paint time.
    let svg = icon.svg().replace("currentColor", "#ffffff");
    let tree = usvg::Tree::from_str(&svg, &usvg::Options::default()).ok()?;
    let mut pixmap = tiny_skia::Pixmap::new(px, px)?;
    let s = px as f32 / tree.size().width().max(1.0);
    resvg::render(&tree, tiny_skia::Transform::from_scale(s, s), &mut pixmap.as_mut());
    Some(egui::ColorImage::from_rgba_premultiplied([px as usize, px as usize], pixmap.data()))
}

/// The texture for `icon` at `size` logical px on this display.
pub fn texture(ctx: &egui::Context, icon: Icon, size: f32) -> Option<egui::TextureId> {
    let px = (size * ctx.pixels_per_point()).round().clamp(4.0, 512.0) as u32;
    let cache = ctx.data_mut(|d| d.get_temp_mut_or_default::<Cache>(egui::Id::new("sphere-ui-icons")).clone());
    let mut map = cache.0.lock();
    if let Some(t) = map.get(&(icon, px)) {
        return Some(t.id());
    }
    let img = rasterize(icon, px)?;
    let tex = ctx.load_texture(format!("lucide-{}-{px}", icon.name()), img, egui::TextureOptions::LINEAR);
    let id = tex.id();
    map.insert((icon, px), tex);
    Some(id)
}

/// Paint `icon` centred in `rect` (square of `size`).
pub fn paint(ui: &Ui, icon: Icon, center: Pos2, size: f32, color: Color32) {
    if let Some(id) = texture(ui.ctx(), icon, size) {
        let rect = Rect::from_center_size(snap(ui, center, size), Vec2::splat(size));
        ui.painter().image(id, rect, Rect::from_min_max(Pos2::ZERO, Pos2::new(1.0, 1.0)), color);
    }
}

/// Snap so the rasterised pixels land on the pixel grid (crisp strokes).
fn snap(ui: &Ui, center: Pos2, size: f32) -> Pos2 {
    let ppp = ui.ctx().pixels_per_point();
    let half = size / 2.0;
    let min = ((center - Vec2::splat(half)).to_vec2() * ppp).round() / ppp;
    Pos2::new(min.x + half, min.y + half)
}

/// An icon as a (non-interactive) widget.
pub fn icon(ui: &mut Ui, icon: Icon, size: f32, color: Color32) -> Response {
    let (rect, resp) = ui.allocate_exact_size(Vec2::splat(size), Sense::hover());
    paint(ui, icon, rect.center(), size, color);
    resp
}
