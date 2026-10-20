//! Surfaces: panels (card with a header row and an actions slot), list rows, section headers,
//! property rows and empty states.

use egui::{pos2, vec2, Align, Color32, Layout, Rect, Response, Sense, Stroke, Ui, UiBuilder};

use crate::icons::{self, Icon};
use crate::theme::{palette, regular, semibold};
use crate::tokens::{composite, radius, size, space, typography};

/// A titled surface: `[icon] Title ……… actions` over a body.
pub struct Panel {
    title: String,
    icon: Option<Icon>,
    fill_height: bool,
    body_margin: f32,
    subtitle: Option<String>,
}

impl Panel {
    pub fn new(title: impl Into<String>) -> Self {
        Panel { title: title.into(), icon: None, fill_height: false, body_margin: space::BASE, subtitle: None }
    }
    pub fn icon(mut self, i: Icon) -> Self {
        self.icon = Some(i);
        self
    }
    /// Take all the height the parent offers.
    pub fn fill_height(mut self, b: bool) -> Self {
        self.fill_height = b;
        self
    }
    pub fn body_margin(mut self, m: f32) -> Self {
        self.body_margin = m;
        self
    }
    /// Small muted text after the title (e.g. a count).
    pub fn subtitle(mut self, s: impl Into<String>) -> Self {
        self.subtitle = Some(s.into());
        self
    }

    pub fn show<R>(self, ui: &mut Ui, actions: impl FnOnce(&mut Ui), body: impl FnOnce(&mut Ui) -> R) -> R {
        let p = palette();
        let outer = ui.available_rect_before_wrap();
        let frame = egui::Frame::new()
            .fill(p.surface_panel)
            .stroke(Stroke::new(1.0f32, p.border_panel))
            .corner_radius(radius::cr(radius::SURFACE));
        let resp = frame.show(ui, |ui| {
            ui.set_width(outer.width() - 2.0);
            if self.fill_height {
                ui.set_min_height(outer.height() - 2.0);
            }
            ui.spacing_mut().item_spacing.y = 0.0;
            // Header.
            let (hrect, _) = ui.allocate_exact_size(vec2(ui.available_width(), size::PROMINENT), Sense::hover());
            let mut x = hrect.left() + space::LOOSE;
            if let Some(i) = self.icon {
                icons::paint(ui, i, pos2(x + 7.0, hrect.center().y), 14.0, p.text_muted);
                x += 14.0 + space::SNUG + 2.0;
            }
            let title = ui.painter().layout_no_wrap(self.title.clone(), semibold(typography::UI_SM), p.text_primary);
            let tw = title.size().x;
            ui.painter().galley(pos2(x, hrect.center().y - title.size().y / 2.0), title, p.text_primary);
            if let Some(s) = &self.subtitle {
                let g = ui.painter().layout_no_wrap(s.clone(), regular(typography::UI_XS), p.text_faint);
                ui.painter().galley(pos2(x + tw + space::SNUG, hrect.center().y - g.size().y / 2.0 + 0.5), g, p.text_faint);
            }
            let arect = Rect::from_min_max(pos2(hrect.center().x, hrect.top()), pos2(hrect.right() - space::SNUG, hrect.bottom()));
            let mut aui = ui.new_child(UiBuilder::new().max_rect(arect).layout(Layout::right_to_left(Align::Center)));
            aui.spacing_mut().item_spacing.x = space::HAIR;
            actions(&mut aui);
            // Hairline under the header.
            let y = hrect.bottom() - 0.5;
            ui.painter().hline(hrect.x_range(), y, Stroke::new(1.0f32, p.divider));
            ui.spacing_mut().item_spacing.y = space::SNUG;
            egui::Frame::new()
                .inner_margin(egui::Margin::same(self.body_margin as i8))
                .show(ui, |ui| {
                    ui.set_width(ui.available_width());
                    if self.fill_height {
                        ui.set_min_height(ui.available_height());
                    }
                    body(ui)
                })
                .inner
        });
        resp.inner
    }
}

/// One entry of a list (scenes, layers, devices): icon, label, trailing controls.
pub struct ListRow {
    label: String,
    icon: Option<Icon>,
    icon_color: Option<Color32>,
    selected: bool,
    dimmed: bool,
    warning: bool,
    height: f32,
    meta: Option<String>,
}

impl ListRow {
    pub fn new(label: impl Into<String>) -> Self {
        ListRow { label: label.into(), icon: None, icon_color: None, selected: false, dimmed: false, warning: false, height: size::ROW + 2.0, meta: None }
    }
    pub fn icon(mut self, i: Icon) -> Self {
        self.icon = Some(i);
        self
    }
    pub fn icon_color(mut self, c: Color32) -> Self {
        self.icon_color = Some(c);
        self
    }
    pub fn selected(mut self, b: bool) -> Self {
        self.selected = b;
        self
    }
    /// Hidden / inactive entry.
    pub fn dimmed(mut self, b: bool) -> Self {
        self.dimmed = b;
        self
    }
    pub fn warning(mut self, b: bool) -> Self {
        self.warning = b;
        self
    }
    pub fn height(mut self, h: f32) -> Self {
        self.height = h;
        self
    }
    /// Right-aligned muted text before the trailing controls.
    pub fn meta(mut self, m: impl Into<String>) -> Self {
        self.meta = Some(m.into());
        self
    }

    /// Returns the row's response (click / double-click / context menu / drag).
    pub fn show(self, ui: &mut Ui, trailing: impl FnOnce(&mut Ui)) -> Response {
        let p = palette();
        let (rect, resp) = ui.allocate_exact_size(vec2(ui.available_width(), self.height), Sense::click_and_drag());
        let base = p.surface_panel;
        let cr = radius::cr(radius::CONTROL);
        if self.selected {
            ui.painter().rect_filled(rect, cr, composite(base, p.accent_soft));
            let bar = Rect::from_min_size(pos2(rect.left(), rect.top() + 5.0), vec2(2.5, rect.height() - 10.0));
            ui.painter().rect_filled(bar, radius::cr(2.0), p.accent_primary);
        } else if resp.hovered() {
            ui.painter().rect_filled(rect, cr, composite(base, p.state_hover));
        }
        // Trailing controls first: their width bounds the label.
        let trail_rect = Rect::from_min_max(pos2(rect.left() + rect.width() * 0.35, rect.top()), pos2(rect.right() - space::TIGHT, rect.bottom()));
        let mut tui = ui.new_child(UiBuilder::new().max_rect(trail_rect).layout(Layout::right_to_left(Align::Center)));
        tui.spacing_mut().item_spacing.x = 0.0;
        trailing(&mut tui);
        let mut right = tui.min_rect().left().min(rect.right() - space::TIGHT);
        if tui.min_rect().width() <= 0.5 {
            right = rect.right() - space::BASE;
        }
        let text_color = if self.warning {
            p.status_warning
        } else if self.dimmed {
            p.text_faint
        } else if self.selected {
            p.text_primary
        } else {
            p.text_secondary
        };
        if let Some(m) = &self.meta {
            let g = ui.painter().layout_no_wrap(m.clone(), regular(typography::UI_XS), p.text_faint);
            right -= g.size().x + space::SNUG;
            ui.painter().galley(pos2(right, rect.center().y - g.size().y / 2.0), g, p.text_faint);
            right -= space::SNUG;
        }
        let mut x = rect.left() + space::BASE + 2.0;
        if let Some(i) = self.icon {
            let c = self.icon_color.unwrap_or(if self.selected { p.accent_primary } else { p.text_muted });
            let c = if self.dimmed { p.text_faint } else { c };
            icons::paint(ui, i, pos2(x + 7.0, rect.center().y), 14.0, c);
            x += 14.0 + space::BASE;
        }
        let font = if self.selected { semibold(typography::UI_SM) } else { regular(typography::UI_SM) };
        let job = egui::text::LayoutJob {
            wrap: egui::text::TextWrapping::truncate_at_width((right - x).max(10.0)),
            ..egui::text::LayoutJob::simple_singleline(self.label.clone(), font, text_color)
        };
        let g = ui.painter().layout_job(job);
        ui.painter().galley(pos2(x, rect.center().y - g.size().y / 2.0), g, text_color);
        resp
    }
}

/// Caps-style section header with a rule: `TRANSFORM ─────────`.
pub fn section_header(ui: &mut Ui, label: &str) {
    let p = palette();
    ui.add_space(space::TIGHT);
    let (rect, _) = ui.allocate_exact_size(vec2(ui.available_width(), 18.0), Sense::hover());
    let g = ui.painter().layout_no_wrap(label.to_uppercase(), semibold(typography::DENSE_CAPTION), p.text_faint);
    let w = g.size().x;
    ui.painter().galley(pos2(rect.left(), rect.center().y - g.size().y / 2.0), g, p.text_faint);
    ui.painter().hline((rect.left() + w + space::BASE)..=rect.right(), rect.center().y, Stroke::new(1.0f32, p.border_subtle));
}

/// A label + control row for property sheets (fixed label column).
pub fn property_row<R>(ui: &mut Ui, label: &str, label_width: f32, add: impl FnOnce(&mut Ui) -> R) -> R {
    let p = palette();
    ui.horizontal(|ui| {
        let (rect, _) = ui.allocate_exact_size(vec2(label_width, size::DEFAULT), Sense::hover());
        let job = egui::text::LayoutJob {
            wrap: egui::text::TextWrapping::truncate_at_width(label_width - 4.0),
            ..egui::text::LayoutJob::simple_singleline(label.to_string(), regular(typography::UI_SM), p.text_muted)
        };
        let g = ui.painter().layout_job(job);
        ui.painter().galley(pos2(rect.left(), rect.center().y - g.size().y / 2.0), g, p.text_muted);
        ui.vertical(|ui| add(ui)).inner
    })
    .inner
}

/// Centred icon + title + hint for empty lists and canvases.
pub fn empty_state(ui: &mut Ui, icon: Icon, title: &str, hint: &str) {
    let p = palette();
    ui.vertical_centered(|ui| {
        ui.add_space(space::LOOSE);
        icons::icon(ui, icon, 22.0, p.text_faint);
        ui.add_space(space::SNUG);
        ui.label(egui::RichText::new(title).font(semibold(typography::UI_SM)).color(p.text_secondary));
        if !hint.is_empty() {
            ui.label(egui::RichText::new(hint).font(regular(typography::UI_XS)).color(p.text_faint));
        }
        ui.add_space(space::LOOSE);
    });
}

/// Muted helper text.
pub fn hint(ui: &mut Ui, text: impl Into<String>) -> Response {
    let p = palette();
    ui.label(egui::RichText::new(text.into()).font(regular(typography::UI_XS)).color(p.text_faint))
}
