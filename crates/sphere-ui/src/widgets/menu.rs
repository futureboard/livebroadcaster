//! Menus: icon rows for menus / popovers (Sphere's menu tokens: 24 px rows, icon slot, muted
//! shortcut) and a popover anchored under a widget.

use egui::{pos2, vec2, Color32, Response, Sense, Ui};

use crate::icons::{self, Icon};
use crate::theme::{palette, regular};
use crate::tokens::{composite, radius, size, space, typography};

/// Menu row. Returns its response; call `ui.close_menu()` on click when inside an egui menu.
pub fn menu_item(ui: &mut Ui, icon: Option<Icon>, label: &str, shortcut: Option<&str>) -> Response {
    menu_item_colored(ui, icon, label, shortcut, None, true)
}

/// Menu row with an optional accent colour (e.g. destructive in red) and enabled flag.
pub fn menu_item_colored(ui: &mut Ui, icon: Option<Icon>, label: &str, shortcut: Option<&str>, color: Option<Color32>, enabled: bool) -> Response {
    let p = palette();
    let width = ui.available_width().max(200.0);
    let sense = if enabled { Sense::click() } else { Sense::hover() };
    let (rect, resp) = ui.allocate_exact_size(vec2(width, size::ROW), sense);
    if enabled && resp.hovered() {
        ui.painter().rect_filled(rect, radius::cr(radius::CONTROL), composite(p.surface_panel_raised, p.state_hover));
    }
    let fg = if !enabled { p.text_disabled } else { color.unwrap_or(p.text_primary) };
    let mut x = rect.left() + space::BASE;
    if let Some(i) = icon {
        icons::paint(ui, i, pos2(x + 7.0, rect.center().y), 14.0, if enabled { color.unwrap_or(p.text_muted) } else { p.text_disabled });
    }
    x += 14.0 + space::BASE;
    let g = ui.painter().layout_no_wrap(label.to_string(), regular(typography::UI_SM), fg);
    ui.painter().galley(pos2(x, rect.center().y - g.size().y / 2.0), g, fg);
    if let Some(s) = shortcut {
        let g = ui.painter().layout_no_wrap(s.to_string(), regular(typography::UI_XS), p.text_faint);
        ui.painter().galley(pos2(rect.right() - space::BASE - g.size().x, rect.center().y - g.size().y / 2.0), g, p.text_faint);
    }
    if enabled {
        resp.on_hover_cursor(egui::CursorIcon::PointingHand)
    } else {
        resp
    }
}

/// A small caps header inside a menu.
pub fn menu_header(ui: &mut Ui, text: &str) {
    let p = palette();
    ui.add_space(space::HAIR);
    ui.label(egui::RichText::new(text.to_uppercase()).font(crate::theme::semibold(typography::DENSE_CAPTION)).color(p.text_faint));
}

/// Toggle a popover under `anchor` when it is clicked, and show it while open. The popover
/// closes on any click outside or on an item.
pub fn popover<R>(ui: &mut Ui, anchor: &Response, id: egui::Id, add: impl FnOnce(&mut Ui) -> R) -> Option<R> {
    if anchor.clicked() {
        ui.memory_mut(|m| m.toggle_popup(id));
    }
    #[allow(deprecated)]
    egui::popup::popup_below_widget(ui, id, anchor, egui::PopupCloseBehavior::CloseOnClick, |ui| {
        ui.set_min_width(220.0);
        add(ui)
    })
}
