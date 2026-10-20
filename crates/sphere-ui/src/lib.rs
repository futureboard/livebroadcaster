//! # Sphere UI for egui
//!
//! The design system of Futureboard's SphereUIComponents (GPUI), ported to egui: the same
//! tokens (palette from Futureboard theme files, radius / spacing / size / typography /
//! state-layer ladders), the system font chain, Lucide icons, and the core components —
//! buttons, icon buttons, panels, list rows, faders, meters, switches, segmented controls,
//! tabs, chips, title-bar chrome, status bar and dialog frames.
//!
//! ```no_run
//! # fn f(ctx: &egui::Context, ui: &mut egui::Ui) {
//! sphere_ui::install(ctx, sphere_ui::ThemeKind::Dark);
//! if ui.add(sphere_ui::Button::primary("Go Live").icon(sphere_ui::Icon::Radio)).clicked() {}
//! # }
//! ```

pub mod fonts;
mod icon_set;
pub mod icons;
pub mod theme;
pub mod tokens;
mod widgets {
    pub mod button;
    pub mod chrome;
    pub mod controls;
    pub mod menu;
    pub mod panel;
}

pub use icons::Icon;

static TRANSLATE: std::sync::OnceLock<fn(&'static str) -> &'static str> = std::sync::OnceLock::new();

/// Give the components' own texts (tooltips like "Close") to the app's translation.
pub fn set_translator(f: fn(&'static str) -> &'static str) {
    let _ = TRANSLATE.set(f);
}

pub(crate) fn tr(s: &'static str) -> &'static str {
    TRANSLATE.get().map_or(s, |f| f(s))
}
pub use theme::{install, mono, palette, regular, semibold, set_palette, ThemeKind};
pub use tokens::{composite, radius, size, space, state, typography, with_alpha, Palette};
pub use widgets::button::{Button, ButtonKind, IconButton};
pub use widgets::chrome::{chrome_row, dialog_frame, dialog_header, scrim, status_bar, status_item, title_bar, toolbar, window_outline, window_resize, CAPTION_WIDTH};
pub use widgets::controls::{chip, meter, segmented, tabs, vdivider, ChipStyle, Fader, Toggle};
pub use widgets::menu::{menu_header, menu_item, menu_item_colored, popover};
pub use widgets::panel::{empty_state, hint, property_row, section_header, ListRow, Panel};
