//! Value controls and indicators: fader, level meter, switch, segmented control, tabs, chips.

use egui::{pos2, vec2, Color32, Rect, Response, Sense, Stroke, StrokeKind, Ui, Widget};

use crate::icons::{self, Icon};
use crate::theme::{palette, regular, semibold};
use crate::tokens::{composite, radius, size, space, typography, with_alpha};

/// A horizontal fader over `0..=1`: groove, accent fill, round thumb. Double-click resets to
/// `default`; the wheel nudges.
pub struct Fader<'a> {
    value: &'a mut f32,
    width: Option<f32>,
    default: Option<f32>,
    color: Option<Color32>,
}

impl<'a> Fader<'a> {
    pub fn new(value: &'a mut f32) -> Self {
        Fader { value, width: None, default: None, color: None }
    }
    pub fn width(mut self, w: f32) -> Self {
        self.width = Some(w);
        self
    }
    pub fn default_value(mut self, v: f32) -> Self {
        self.default = Some(v);
        self
    }
    pub fn color(mut self, c: Color32) -> Self {
        self.color = Some(c);
        self
    }
}

impl Widget for Fader<'_> {
    fn ui(self, ui: &mut Ui) -> Response {
        let p = palette();
        let w = self.width.unwrap_or_else(|| ui.available_width()).max(40.0);
        let (rect, mut resp) = ui.allocate_exact_size(vec2(w, 18.0), Sense::click_and_drag());
        let thumb_r = 6.0;
        let x0 = rect.left() + thumb_r;
        let x1 = rect.right() - thumb_r;
        let old = *self.value;
        if let Some(pos) = resp.interact_pointer_pos() {
            if resp.dragged() || resp.clicked() {
                *self.value = ((pos.x - x0) / (x1 - x0)).clamp(0.0, 1.0);
            }
        }
        if resp.double_clicked() {
            if let Some(d) = self.default {
                *self.value = d;
            }
        }
        if resp.hovered() {
            let scroll = ui.input(|i| i.raw_scroll_delta.y);
            if scroll != 0.0 {
                *self.value = (*self.value + scroll.signum() * 0.02).clamp(0.0, 1.0);
            }
        }
        if *self.value != old {
            resp.mark_changed();
        }
        let v = *self.value;
        let cy = rect.center().y;
        let groove = Rect::from_min_max(pos2(x0, cy - 2.0), pos2(x1, cy + 2.0));
        let painter = ui.painter();
        painter.rect_filled(groove, radius::cr(2.0), p.fader_groove);
        let tx = x0 + (x1 - x0) * v;
        let fill = self.color.unwrap_or(p.accent_primary);
        painter.rect_filled(Rect::from_min_max(groove.min, pos2(tx, groove.max.y)), radius::cr(2.0), fill);
        let active = resp.hovered() || resp.dragged();
        let thumb = if active { p.text_primary } else { p.fader_thumb };
        painter.circle_filled(pos2(tx, cy), if active { thumb_r + 0.5 } else { thumb_r }, thumb);
        painter.circle_stroke(pos2(tx, cy), thumb_r, Stroke::new(1.0f32, p.fader_thumb_border));
        resp.on_hover_cursor(egui::CursorIcon::PointingHand)
    }
}

/// Stereo / mono level meter, −60…0 dBFS: rail, low / mid / high zones, peak-hold tick and a
/// latched clip cap (all values in dB, `None` for a missing channel).
pub fn meter(ui: &mut Ui, levels_db: &[f32], peaks_db: &[f32], width: f32, muted: bool) -> Response {
    let p = palette();
    let ch = levels_db.len().max(1);
    let bar_h = if ch > 1 { 4.0 } else { 6.0 };
    let gap = 2.0;
    let h = ch as f32 * bar_h + (ch as f32 - 1.0) * gap;
    let (rect, resp) = ui.allocate_exact_size(vec2(width, h), Sense::hover());
    let frac = |db: f32| ((db + 60.0) / 60.0).clamp(0.0, 1.0);
    let painter = ui.painter();
    for (i, db) in levels_db.iter().enumerate() {
        let y = rect.top() + i as f32 * (bar_h + gap);
        let bar = Rect::from_min_size(pos2(rect.left(), y), vec2(rect.width(), bar_h));
        painter.rect_filled(bar, radius::cr(1.5), p.meter_rail);
        let zones = [(-60.0, -20.0, p.meter_low), (-20.0, -9.0, p.meter_mid), (-9.0, 0.0, p.meter_high)];
        for (lo, hi, c) in zones {
            let xa = bar.left() + bar.width() * frac(lo);
            let xb = bar.left() + bar.width() * frac(hi);
            // Unlit zones keep a whisper of their colour as a scale.
            painter.rect_filled(Rect::from_min_max(pos2(xa, bar.top()), pos2(xb, bar.bottom())), 0.0, with_alpha(c, 0.10));
            let lit = (bar.left() + bar.width() * frac(*db)).min(xb);
            if lit > xa && !muted {
                painter.rect_filled(Rect::from_min_max(pos2(xa, bar.top()), pos2(lit, bar.bottom())), 0.0, c);
            }
        }
        if let Some(pk) = peaks_db.get(i) {
            if *pk > -59.5 && !muted {
                let x = bar.left() + bar.width() * frac(*pk);
                let c = if *pk >= -0.1 { p.meter_clip } else { p.meter_peak };
                painter.rect_filled(Rect::from_min_size(pos2(x - 1.0, bar.top()), vec2(1.5, bar_h)), 0.0, c);
            }
        }
    }
    resp
}

/// A switch (pill track + knob).
pub struct Toggle<'a> {
    on: &'a mut bool,
    label: Option<String>,
}

impl<'a> Toggle<'a> {
    pub fn new(on: &'a mut bool) -> Self {
        Toggle { on, label: None }
    }
    pub fn label(mut self, l: impl Into<String>) -> Self {
        self.label = Some(l.into());
        self
    }
}

impl Widget for Toggle<'_> {
    fn ui(self, ui: &mut Ui) -> Response {
        let p = palette();
        let galley = self.label.as_ref().map(|l| ui.painter().layout_no_wrap(l.clone(), regular(typography::UI_SM), p.text_secondary));
        let tw = 30.0;
        let th = 16.0;
        let total = tw + galley.as_ref().map_or(0.0, |g| space::BASE + g.size().x);
        let (rect, mut resp) = ui.allocate_exact_size(vec2(total, size::DEFAULT), Sense::click());
        if resp.clicked() {
            *self.on = !*self.on;
            resp.mark_changed();
        }
        let track = Rect::from_min_size(pos2(rect.left(), rect.center().y - th / 2.0), vec2(tw, th));
        let t = ui.ctx().animate_bool_with_time(resp.id, *self.on, 0.11);
        let fill = if *self.on { p.accent_primary } else { composite(p.surface_input, p.state_hover) };
        let fill = if resp.hovered() { composite(fill, p.state_hover) } else { fill };
        ui.painter().rect_filled(track, radius::cr(radius::PILL), fill);
        if !*self.on {
            ui.painter().rect_stroke(track, radius::cr(radius::PILL), Stroke::new(1.0f32, p.border_normal), StrokeKind::Inside);
        }
        let kx = track.left() + th / 2.0 + t * (tw - th);
        ui.painter().circle_filled(pos2(kx, track.center().y), th / 2.0 - 2.5, if *self.on { p.on_accent } else { p.text_muted });
        if let Some(g) = galley {
            ui.painter().galley(pos2(track.right() + space::BASE, rect.center().y - g.size().y / 2.0), g, p.text_secondary);
        }
        resp.on_hover_cursor(egui::CursorIcon::PointingHand)
    }
}

/// A segmented control (inset track, raised selected segment). Returns true when the selection
/// changed.
pub fn segmented<T: PartialEq + Clone>(ui: &mut Ui, value: &mut T, options: &[(T, Option<Icon>, &str)], fill_width: bool) -> bool {
    let p = palette();
    let font = regular(typography::UI_SM);
    let seg_w: Vec<f32> = options
        .iter()
        .map(|(_, icon, l)| {
            let tw = if l.is_empty() { 0.0 } else { ui.painter().layout_no_wrap(l.to_string(), font.clone(), p.text_primary).size().x };
            tw + icon.map_or(0.0, |_| 14.0 + if l.is_empty() { 0.0 } else { space::SNUG }) + 2.0 * space::LOOSE
        })
        .collect();
    let natural: f32 = seg_w.iter().sum::<f32>() + 4.0;
    let w = if fill_width { ui.available_width().max(natural) } else { natural };
    let (rect, _) = ui.allocate_exact_size(vec2(w, size::COMFORTABLE), Sense::hover());
    ui.painter().rect_filled(rect, radius::cr(radius::CONTROL), p.surface_input);
    let scale = (w - 4.0) / (natural - 4.0);
    let mut x = rect.left() + 2.0;
    let mut changed = false;
    for (i, (v, icon, label)) in options.iter().enumerate() {
        let sw = seg_w[i] * scale;
        let r = Rect::from_min_size(pos2(x, rect.top() + 2.0), vec2(sw, rect.height() - 4.0));
        let resp = ui.interact(r, ui.id().with(("seg", i, label)), Sense::click());
        let sel = *value == *v;
        if sel {
            ui.painter().rect_filled(r, radius::cr(radius::inner(radius::CONTROL, 2.0)), p.surface_panel_raised);
            ui.painter().rect_stroke(r, radius::cr(radius::inner(radius::CONTROL, 2.0)), Stroke::new(1.0f32, p.border_subtle), StrokeKind::Inside);
        } else if resp.hovered() {
            ui.painter().rect_filled(r, radius::cr(radius::inner(radius::CONTROL, 2.0)), p.state_hover);
        }
        let color = if sel { p.text_primary } else { p.text_muted };
        let tg = (!label.is_empty()).then(|| ui.painter().layout_no_wrap(label.to_string(), font.clone(), color));
        let content = tg.as_ref().map_or(0.0, |g| g.size().x) + icon.map_or(0.0, |_| 14.0 + if tg.is_some() { space::SNUG } else { 0.0 });
        let mut cx = r.center().x - content / 2.0;
        if let Some(ic) = icon {
            icons::paint(ui, *ic, pos2(cx + 7.0, r.center().y), 14.0, if sel { p.accent_primary } else { color });
            cx += 14.0 + space::SNUG;
        }
        if let Some(g) = tg {
            ui.painter().galley(pos2(cx, r.center().y - g.size().y / 2.0), g, color);
        }
        if resp.clicked() && !sel {
            *value = v.clone();
            changed = true;
        }
        resp.on_hover_cursor(egui::CursorIcon::PointingHand);
        x += sw;
    }
    changed
}

/// Workspace tabs with an accent underline (Sphere's dock tabs).
pub fn tabs<T: PartialEq + Clone>(ui: &mut Ui, value: &mut T, options: &[(T, Icon, &str)]) -> bool {
    let p = palette();
    let mut changed = false;
    let (bar, _) = ui.allocate_exact_size(vec2(ui.available_width(), size::PROMINENT), Sense::hover());
    ui.painter().hline(bar.x_range(), bar.bottom() - 0.5, Stroke::new(1.0f32, p.divider));
    let mut x = bar.left() + space::TIGHT;
    for (i, (v, icon, label)) in options.iter().enumerate() {
        let sel = *value == *v;
        let font = if sel { semibold(typography::UI_SM) } else { regular(typography::UI_SM) };
        let color = if sel { p.tab_text_active } else { p.tab_text };
        let g = ui.painter().layout_no_wrap(label.to_string(), font, color);
        let w = g.size().x + 14.0 + space::SNUG + 2.0 * space::BASE;
        let r = Rect::from_min_size(pos2(x, bar.top() + 3.0), vec2(w, bar.height() - 6.0));
        let resp = ui.interact(r, ui.id().with(("tab", i)), Sense::click());
        if resp.hovered() && !sel {
            ui.painter().rect_filled(r, radius::cr(radius::CONTROL), p.state_hover);
        }
        icons::paint(ui, *icon, pos2(r.left() + space::BASE + 7.0, r.center().y), 14.0, if sel { p.tab_indicator } else { p.text_muted });
        ui.painter().galley(pos2(r.left() + space::BASE + 14.0 + space::SNUG, r.center().y - g.size().y / 2.0), g, color);
        if sel {
            let u = Rect::from_min_max(pos2(r.left() + 4.0, bar.bottom() - 2.0), pos2(r.right() - 4.0, bar.bottom()));
            ui.painter().rect_filled(u, radius::cr(1.0), p.tab_indicator);
        }
        if resp.clicked() && !sel {
            *value = v.clone();
            changed = true;
        }
        resp.on_hover_cursor(egui::CursorIcon::PointingHand);
        x += w + space::HAIR;
    }
    changed
}

/// Visual weight of a [`chip`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ChipStyle {
    /// Solid fill (on-air states).
    Solid,
    /// Tinted wash + coloured text.
    Soft,
    /// Neutral outline.
    Outline,
}

/// A status chip (pill): optional dot or icon, text.
pub fn chip(ui: &mut Ui, text: &str, color: Color32, style: ChipStyle, icon: Option<Icon>, dot: bool) -> Response {
    let p = palette();
    let font = semibold(typography::UI_XS);
    let (fill, fg, border) = match style {
        ChipStyle::Solid => (color, crate::tokens::on_color(color, &p), None),
        ChipStyle::Soft => (with_alpha(color, 0.16), color, Some(with_alpha(color, 0.35))),
        ChipStyle::Outline => (Color32::TRANSPARENT, p.text_muted, Some(p.border_normal)),
    };
    let g = ui.painter().layout_no_wrap(text.to_string(), font, fg);
    let lead = if dot { 6.0 + space::SNUG } else if icon.is_some() { 12.0 + space::TIGHT } else { 0.0 };
    let (rect, resp) = ui.allocate_exact_size(vec2(g.size().x + lead + 2.0 * space::BASE, size::DENSE), Sense::hover());
    ui.painter().rect_filled(rect, radius::cr(radius::PILL), fill);
    if let Some(b) = border {
        ui.painter().rect_stroke(rect, radius::cr(radius::PILL), Stroke::new(1.0f32, b), StrokeKind::Inside);
    }
    let mut x = rect.left() + space::BASE;
    if dot {
        ui.painter().circle_filled(pos2(x + 3.0, rect.center().y), 3.0, fg);
        x += 6.0 + space::SNUG;
    } else if let Some(i) = icon {
        icons::paint(ui, i, pos2(x + 6.0, rect.center().y), 12.0, fg);
        x += 12.0 + space::TIGHT;
    }
    ui.painter().galley(pos2(x, rect.center().y - g.size().y / 2.0), g, fg);
    resp
}

/// A thin vertical divider for toolbars.
pub fn vdivider(ui: &mut Ui, height: f32) {
    let p = palette();
    let (rect, _) = ui.allocate_exact_size(vec2(1.0 + 2.0 * space::TIGHT, height), Sense::hover());
    ui.painter().vline(rect.center().x, rect.y_range(), Stroke::new(1.0f32, p.divider));
}
