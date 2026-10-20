//! Window chrome for a client-decorated window, after Sphere's `app_chrome` / `title_bar`:
//! the title row (drag area, double-click to maximise, caption buttons), edge resizing, the
//! status bar, and dialog frames.

use egui::{pos2, vec2, Align, Color32, Layout, Rect, Sense, Stroke, Ui, UiBuilder, ViewportCommand};

use crate::icons::{self, Icon};
use crate::theme::{palette, regular};
use crate::tokens::{composite, radius, size, space, typography, with_alpha};

/// Caption button width.
pub const CAPTION_WIDTH: f32 = 44.0;

/// The title row as a top panel. `left` gets a left-to-right Ui after the drag handle starts,
/// `right` a right-to-left Ui just before the caption buttons; `center` a centred Ui.
pub fn title_bar(
    ctx: &egui::Context,
    left: impl FnOnce(&mut Ui),
    center: impl FnOnce(&mut Ui),
    right: impl FnOnce(&mut Ui),
) {
    chrome_row(ctx, true, left, center, right);
}

/// The same row under a native title bar / menu: no caption buttons, no window dragging.
pub fn toolbar(
    ctx: &egui::Context,
    left: impl FnOnce(&mut Ui),
    center: impl FnOnce(&mut Ui),
    right: impl FnOnce(&mut Ui),
) {
    chrome_row(ctx, false, left, center, right);
}

/// [`title_bar`] (`client_decorated`: caption buttons + window dragging) or [`toolbar`].
pub fn chrome_row(
    ctx: &egui::Context,
    client_decorated: bool,
    left: impl FnOnce(&mut Ui),
    center: impl FnOnce(&mut Ui),
    right: impl FnOnce(&mut Ui),
) {
    let p = palette();
    egui::TopBottomPanel::top("sphere-titlebar")
        .exact_height(size::TITLEBAR)
        .frame(egui::Frame::new().fill(p.surface_titlebar))
        .show_separator_line(false)
        .show(ctx, |ui| {
            let rect = ui.max_rect();
            if client_decorated {
                // Empty space drags the window (added first so every control above wins).
                let drag = ui.interact(rect, ui.id().with("drag"), Sense::click_and_drag());
                if drag.drag_started_by(egui::PointerButton::Primary) {
                    ctx.send_viewport_cmd(ViewportCommand::StartDrag);
                }
                if drag.double_clicked() {
                    let max = ctx.input(|i| i.viewport().maximized.unwrap_or(false));
                    ctx.send_viewport_cmd(ViewportCommand::Maximized(!max));
                }
            }
            ui.painter().hline(rect.x_range(), rect.bottom() - 0.5, Stroke::new(1.0f32, p.divider));

            // Caption buttons.
            let max = ctx.input(|i| i.viewport().maximized.unwrap_or(false));
            let mut x = rect.right();
            let caps: [(Icon, bool); 3] = [(Icon::X, true), (if max { Icon::Copy } else { Icon::Square }, false), (Icon::Minus, false)];
            for (i, (icon, close)) in caps.iter().enumerate().filter(|_| client_decorated) {
                let r = Rect::from_min_max(pos2(x - CAPTION_WIDTH, rect.top()), pos2(x, rect.bottom() - 1.0));
                let resp = ui.interact(r, ui.id().with(("caption", i)), Sense::click());
                if resp.hovered() {
                    let c = if *close { Color32::from_rgb(196, 43, 28) } else { p.state_hover };
                    ui.painter().rect_filled(r, 0.0, c);
                }
                let fg = if resp.hovered() && *close { Color32::WHITE } else { p.text_secondary };
                icons::paint(ui, *icon, r.center(), if *icon == Icon::Square || *icon == Icon::Copy { 12.0 } else { 15.0 }, fg);
                if resp.clicked() {
                    match i {
                        0 => ctx.send_viewport_cmd(ViewportCommand::Close),
                        1 => ctx.send_viewport_cmd(ViewportCommand::Maximized(!max)),
                        _ => ctx.send_viewport_cmd(ViewportCommand::Minimized(true)),
                    }
                }
                x -= CAPTION_WIDTH;
            }
            let right_edge = x - space::SNUG;
            let inner = Rect::from_min_max(pos2(rect.left() + space::BASE, rect.top()), pos2(right_edge, rect.bottom()));
            let mut lui = ui.new_child(UiBuilder::new().max_rect(inner).layout(Layout::left_to_right(Align::Center)));
            lui.spacing_mut().item_spacing.x = space::TIGHT;
            left(&mut lui);
            let left_end = lui.min_rect().right().min(inner.right());
            let mut rui = ui.new_child(UiBuilder::new().max_rect(inner).layout(Layout::right_to_left(Align::Center)));
            rui.spacing_mut().item_spacing.x = space::SNUG;
            right(&mut rui);
            let right_start = rui.min_rect().left().max(inner.left());
            // Centre slot on the window's centre line, as wide as the free space either side allows.
            let half = (rect.center().x - left_end).min(right_start - rect.center().x) - space::LOOSE;
            if half > 60.0 {
                let crect = Rect::from_center_size(rect.center(), vec2((half * 2.0).min(440.0), rect.height()));
                let mut cui = ui.new_child(UiBuilder::new().max_rect(crect).layout(Layout::left_to_right(Align::Center)));
                center(&mut cui);
            }
        });
}

/// Resize from the window edges (client-decorated windows have no native border). Call every
/// frame before the panels.
pub fn window_resize(ctx: &egui::Context) {
    use egui::viewport::ResizeDirection as D;
    if ctx.input(|i| i.viewport().maximized.unwrap_or(false) || i.viewport().fullscreen.unwrap_or(false)) {
        return;
    }
    let screen = ctx.screen_rect();
    let Some(pos) = ctx.input(|i| i.pointer.hover_pos()) else { return };
    let m = 5.0;
    let (l, r, t, b) = (pos.x - screen.left() < m, screen.right() - pos.x < m, pos.y - screen.top() < m, screen.bottom() - pos.y < m);
    let dir = match (l, r, t, b) {
        (true, _, true, _) => Some(D::NorthWest),
        (_, true, true, _) => Some(D::NorthEast),
        (true, _, _, true) => Some(D::SouthWest),
        (_, true, _, true) => Some(D::SouthEast),
        (true, ..) => Some(D::West),
        (_, true, ..) => Some(D::East),
        (_, _, true, _) => Some(D::North),
        (_, _, _, true) => Some(D::South),
        _ => None,
    };
    let Some(dir) = dir else { return };
    ctx.set_cursor_icon(match dir {
        D::North | D::South => egui::CursorIcon::ResizeVertical,
        D::East | D::West => egui::CursorIcon::ResizeHorizontal,
        D::NorthWest | D::SouthEast => egui::CursorIcon::ResizeNwSe,
        _ => egui::CursorIcon::ResizeNeSw,
    });
    if ctx.input(|i| i.pointer.primary_pressed()) {
        ctx.send_viewport_cmd(ViewportCommand::BeginResize(dir));
    }
}

/// A 1 px window outline (client-decorated windows draw their own).
pub fn window_outline(ctx: &egui::Context) {
    let p = palette();
    if ctx.input(|i| i.viewport().maximized.unwrap_or(false)) {
        return;
    }
    let painter = ctx.layer_painter(egui::LayerId::new(egui::Order::Foreground, egui::Id::new("sphere-outline")));
    painter.rect_stroke(ctx.screen_rect().shrink(0.5), 0.0, Stroke::new(1.0f32, p.border_normal), egui::StrokeKind::Inside);
}

/// The status bar as a bottom panel.
pub fn status_bar(ctx: &egui::Context, left: impl FnOnce(&mut Ui), right: impl FnOnce(&mut Ui)) {
    let p = palette();
    egui::TopBottomPanel::bottom("sphere-statusbar")
        .exact_height(size::STATUSBAR)
        .frame(egui::Frame::new().fill(p.statusbar_bg).inner_margin(egui::Margin::symmetric(space::BASE as i8, 0)))
        .show_separator_line(false)
        .show(ctx, |ui| {
            let rect = ui.max_rect();
            ui.painter().hline(rect.expand2(vec2(space::BASE, 0.0)).x_range(), rect.top() + 0.5, Stroke::new(1.0f32, p.divider));
            let mut lui = ui.new_child(UiBuilder::new().max_rect(rect).layout(Layout::left_to_right(Align::Center)));
            lui.spacing_mut().item_spacing.x = space::LOOSE;
            left(&mut lui);
            let mut rui = ui.new_child(UiBuilder::new().max_rect(rect).layout(Layout::right_to_left(Align::Center)));
            rui.spacing_mut().item_spacing.x = space::LOOSE;
            right(&mut rui);
        });
}

/// One status-bar item: icon + text.
pub fn status_item(ui: &mut Ui, icon: Option<Icon>, text: &str, color: Option<Color32>) -> egui::Response {
    let p = palette();
    let c = color.unwrap_or(p.statusbar_text);
    let g = ui.painter().layout_no_wrap(text.to_string(), regular(typography::UI_XS), c);
    let iw = if icon.is_some() { 12.0 + space::TIGHT } else { 0.0 };
    let (rect, resp) = ui.allocate_exact_size(vec2(g.size().x + iw, size::STATUSBAR), Sense::hover());
    if let Some(i) = icon {
        icons::paint(ui, i, pos2(rect.left() + 6.0, rect.center().y), 12.0, c);
    }
    ui.painter().galley(pos2(rect.left() + iw, rect.center().y - g.size().y / 2.0), g, c);
    resp
}

/// Frame for dialogs / floating windows (DIALOG radius, overlay shadow).
pub fn dialog_frame() -> egui::Frame {
    let p = palette();
    egui::Frame::new()
        .fill(p.surface_panel_raised)
        .stroke(Stroke::new(1.0f32, p.border_normal))
        .corner_radius(radius::cr(radius::DIALOG))
        .inner_margin(egui::Margin::same(space::SECTION as i8))
        .shadow(egui::epaint::Shadow { offset: [0, 10], blur: 30, spread: 0, color: with_alpha(Color32::BLACK, 0.5) })
}

/// A dialog title row: icon + heading + close button. Returns true when close was clicked.
pub fn dialog_header(ui: &mut Ui, icon: Icon, title: &str) -> bool {
    let p = palette();
    let mut close = false;
    ui.horizontal(|ui| {
        icons::icon(ui, icon, 16.0, p.accent_primary);
        ui.label(egui::RichText::new(title).font(crate::theme::semibold(typography::UI_MD)).color(p.text_primary));
        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
            close = ui.add(crate::IconButton::new(Icon::X).tooltip(crate::tr("Close"))).clicked();
        });
    });
    ui.add_space(space::TIGHT);
    close
}

/// Dim everything below a modal.
pub fn scrim(ctx: &egui::Context) {
    let p = palette();
    let painter = ctx.layer_painter(egui::LayerId::new(egui::Order::Middle, egui::Id::new("sphere-scrim")));
    painter.rect_filled(ctx.screen_rect(), 0.0, composite(Color32::TRANSPARENT, with_alpha(p.state_scrim, 0.55)));
}
