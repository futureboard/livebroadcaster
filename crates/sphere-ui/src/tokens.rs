//! Design tokens, ported from SphereUIComponents' `theme.rs`: the colour palette (read from
//! Futureboard theme files, same schema), and the radius / spacing / size / typography / state
//! ladders. Components pick a token by *what the thing is*, never by how big it looks.

use egui::Color32;

/// Corner radii (logical px).
pub mod radius {
    /// Grid cells, meter fills, table rows, the chrome row, the status bar.
    pub const NONE: f32 = 0.0;
    /// Chips, fader / slider thumbs, caret and selection highlights.
    pub const MICRO: f32 = 3.0;
    /// Controls in the 16–20 px band.
    pub const CONTROL_SM: f32 = 4.0;
    /// The workhorse: controls 24–32 px tall (buttons, inputs, selects, tabs).
    pub const CONTROL: f32 = 6.0;
    /// Surfaces above another plane: menus, popovers, tooltips, cards, panels.
    pub const SURFACE: f32 = 10.0;
    /// Window-level surfaces: modals and dialogs.
    pub const DIALOG: f32 = 14.0;
    /// Fully round: status chips, badges, switch tracks.
    pub const PILL: f32 = 9999.0;

    /// Radius for an element nested `pad` px inside a container of radius `outer`.
    pub const fn inner(outer: f32, pad: f32) -> f32 {
        let r = outer - pad;
        if r > 0.0 {
            r
        } else {
            0.0
        }
    }

    pub fn cr(r: f32) -> egui::CornerRadius {
        egui::CornerRadius::same(r.min(255.0) as u8)
    }
}

/// Spacing scale (logical px).
pub mod space {
    pub const HAIR: f32 = 2.0;
    pub const TIGHT: f32 = 4.0;
    pub const SNUG: f32 = 6.0;
    pub const BASE: f32 = 8.0;
    pub const LOOSE: f32 = 12.0;
    pub const SECTION: f32 = 16.0;
    pub const BLOCK: f32 = 24.0;
    pub const PAGE: f32 = 32.0;
}

/// Control height ladder (logical px).
pub mod size {
    pub const MICRO: f32 = 16.0;
    pub const DENSE: f32 = 20.0;
    /// Toolbar buttons, tabs, list rows.
    pub const DEFAULT: f32 = 24.0;
    /// Text inputs, selects, segmented controls, secondary dialog buttons.
    pub const COMFORTABLE: f32 = 28.0;
    /// Primary actions, the app chrome row.
    pub const PROMINENT: f32 = 32.0;
    pub const ROW: f32 = 26.0;
    pub const TITLEBAR: f32 = 38.0;
    pub const STATUSBAR: f32 = 24.0;
}

/// Typography (logical px). Sphere scales text by 1.08 for readability; the values here
/// already include it.
pub mod typography {
    pub const TEXT_SCALE: f32 = 1.08;
    pub const UI_XS: f32 = 11.0 * TEXT_SCALE;
    pub const UI_SM: f32 = 12.0 * TEXT_SCALE;
    pub const UI_MD: f32 = 13.0 * TEXT_SCALE;
    pub const UI_LG: f32 = 15.0 * TEXT_SCALE;
    pub const DENSE_CAPTION: f32 = 9.5 * TEXT_SCALE;
    pub const DISPLAY: f32 = 20.0 * TEXT_SCALE;
}

/// State-layer alphas (Material 3 values reduced ~25% for dense controls).
pub mod state {
    pub const HOVER: f32 = 0.06;
    pub const PRESSED: f32 = 0.10;
    pub const SELECTED: f32 = 0.10;
    pub const ARMED_WASH: f32 = 0.18;
    pub const ARMED_BORDER: f32 = 0.55;
    pub const DISABLED_CONTENT: f32 = 0.38;
    pub const FOCUS_RING_PX: f32 = 2.0;
}

/// Parse `#RGB`, `#RRGGBB` or `#RRGGBBAA` (straight alpha).
pub fn parse_hex(s: &str) -> Option<Color32> {
    let h = s.trim().strip_prefix('#')?;
    let v = |i: usize| u8::from_str_radix(h.get(i..i + 2)?, 16).ok();
    match h.len() {
        3 => {
            let d = |i: usize| u8::from_str_radix(&h[i..i + 1], 16).ok().map(|x| x * 17);
            Some(Color32::from_rgb(d(0)?, d(1)?, d(2)?))
        }
        6 => Some(Color32::from_rgb(v(0)?, v(2)?, v(4)?)),
        8 => Some(Color32::from_rgba_unmultiplied(v(0)?, v(2)?, v(4)?, v(6)?)),
        _ => None,
    }
}

/// `color` with alpha `a` (0–1), keeping its straight RGB.
pub fn with_alpha(color: Color32, a: f32) -> Color32 {
    let [r, g, b, _] = color.to_srgba_unmultiplied();
    Color32::from_rgba_unmultiplied(r, g, b, (a.clamp(0.0, 1.0) * 255.0).round() as u8)
}

/// Porter-Duff "over" of `layer` onto `base`, in sRGB like Sphere's `Colors::composite`: state
/// layers lift a control's own rest fill instead of replacing it.
pub fn composite(base: Color32, layer: Color32) -> Color32 {
    let [br, bg, bb, ba] = base.to_srgba_unmultiplied().map(|v| v as f32 / 255.0);
    let [lr, lg, lb, la] = layer.to_srgba_unmultiplied().map(|v| v as f32 / 255.0);
    let a = la + ba * (1.0 - la);
    if a <= f32::EPSILON {
        return Color32::TRANSPARENT;
    }
    let k = ba * (1.0 - la);
    let c = |l: f32, b: f32| (((l * la + b * k) / a).clamp(0.0, 1.0) * 255.0).round() as u8;
    Color32::from_rgba_unmultiplied(c(lr, br), c(lg, bg), c(lb, bb), (a * 255.0).round() as u8)
}

/// Text colour that stays readable on an arbitrary fill (Rec. 709 luminance, threshold 0.55).
pub fn on_color(fill: Color32, p: &Palette) -> Color32 {
    let [r, g, b, _] = fill.to_srgba_unmultiplied().map(|v| v as f32 / 255.0);
    if 0.2126 * r + 0.7152 * g + 0.0722 * b > 0.55 {
        p.text_inverse
    } else {
        p.text_primary
    }
}

macro_rules! palette {
    ($( $field:ident : $key:literal = $fallback:literal ),* $(,)?) => {
        /// Every colour the components use, resolved from a theme.
        #[derive(Clone, Debug, PartialEq)]
        pub struct Palette {
            pub dark: bool,
            $( pub $field: Color32, )*
        }

        impl Palette {
            /// Sphere's built-in fallbacks (used for tokens a theme omits).
            pub fn fallback() -> Palette {
                Palette { dark: true, $( $field: parse_hex($fallback).unwrap(), )* }
            }

            fn set(&mut self, key: &str, c: Color32) {
                match key {
                    $( $key => self.$field = c, )*
                    _ => {}
                }
            }
        }
    };
}

palette! {
    // Surfaces
    surface_base: "surface.base" = "#1A1D24",
    surface_panel: "surface.panel" = "#22252E",
    surface_panel_alt: "surface.panelAlt" = "#14161C",
    surface_panel_raised: "surface.panelRaised" = "#2A2E38",
    surface_canvas: "surface.canvas" = "#0E1015",
    surface_input: "surface.input" = "#0E1015",
    surface_window: "surface.window" = "#16181F",
    surface_titlebar: "surface.titlebar" = "#14161C",
    surface_sidebar: "surface.sidebar" = "#14161C",
    surface_card: "surface.card" = "#22252E",
    surface_card_hover: "surface.cardHover" = "#383D48",
    surface_hover: "surface.hover" = "#383D48",
    surface_active: "surface.active" = "#4A4F5C",
    surface_selected: "surface.selected" = "#4A4F5C",
    surface_overlay: "surface.overlay" = "#05070BA6",
    // Borders
    border_subtle: "border.subtle" = "#FFFFFF14",
    border_normal: "border.normal" = "#FFFFFF1F",
    border_strong: "border.strong" = "#FFFFFF33",
    border_focus: "border.focus" = "#2FC9D6E0",
    border_accent: "border.accent" = "#2FC9D68C",
    border_panel: "border.panel" = "#FFFFFF0D",
    divider: "border.divider" = "#FFFFFF0F",
    // Text
    text_primary: "text.primary" = "#EBEDF2",
    text_secondary: "text.secondary" = "#B9BDC6",
    text_muted: "text.muted" = "#8F949F",
    text_faint: "text.faint" = "#6F7480",
    text_disabled: "text.disabled" = "#5B606B",
    text_inverse: "text.inverse" = "#0E1015",
    // Accent
    accent_primary: "accent.primary" = "#2FC9D6",
    accent_hover: "accent.primaryHover" = "#54D8E2",
    accent_pressed: "accent.pressed" = "#1AA8B5",
    accent_soft: "accent.soft" = "#2FC9D629",
    accent_muted: "accent.muted" = "#2FC9D61A",
    on_accent: "accent.onAccent" = "#06181B",
    accent_purple: "accent.purple" = "#A78BFA",
    // Status
    status_error: "status.error" = "#F2645F",
    status_warning: "status.warning" = "#E8B75C",
    status_success: "status.success" = "#43D18A",
    // State layers
    state_hover: "state.hover" = "#FFFFFF0F",
    state_pressed: "state.pressed" = "#FFFFFF1A",
    state_selected: "state.selected" = "#FFFFFF1A",
    state_recessed: "state.recessed" = "#0000001F",
    state_scrim: "state.scrim" = "#05070BA6",
    state_focus_ring: "state.focusRing" = "#2FC9D6E0",
    state_mute: "state.mute" = "#6F9BFF",
    state_solo: "state.solo" = "#E8B75C",
    state_arm: "state.arm" = "#F2645F",
    state_monitor: "state.monitor" = "#43D18A",
    // Buttons
    button_bg: "button.bg" = "#22252E",
    button_bg_active: "button.bgActive" = "#1E4650",
    button_border: "button.border" = "#FFFFFF12",
    button_border_hover: "button.borderHover" = "#FFFFFF29",
    button_text: "button.text" = "#EBEDF2",
    button_text_muted: "button.textMuted" = "#8F949F",
    // Tabs
    tab_text: "tab.text" = "#B9BDC6",
    tab_text_active: "tab.text_active" = "#EBEDF2",
    tab_indicator: "tab.indicator_active" = "#2FC9D6",
    tab_bg_active: "tab.backgroundActive" = "#2FC9D61A",
    // Meters / faders
    meter_rail: "meter.rail" = "#00000024",
    meter_low: "meter.low" = "#43D18A",
    meter_mid: "meter.mid" = "#E8B75C",
    meter_high: "meter.high" = "#F2645F",
    meter_peak: "meter.peak" = "#EBEDF2",
    meter_clip: "meter.clip" = "#FF2D20",
    fader_groove: "fader.groove" = "#0E1015",
    fader_thumb: "fader.thumb" = "#C8CCD4",
    fader_thumb_border: "fader.thumbBorder" = "#FFFFFF33",
    // Status bar
    statusbar_bg: "statusbar.background" = "#14161C",
    statusbar_text: "statusbar.text" = "#8F949F",
    statusbar_text_muted: "statusbar.textMuted" = "#6F7480",
}

impl Palette {
    /// Read a Futureboard theme file (`{"appearance": "dark", "tokens": {"surface": {"base":
    /// "#232323", …}}}`), on top of `base` for tokens it omits.
    pub fn from_theme_json(json: &str, base: &Palette) -> Result<Palette, String> {
        let v: serde_json::Value = serde_json::from_str(json).map_err(|e| e.to_string())?;
        let mut p = base.clone();
        if let Some(a) = v.get("appearance").and_then(|a| a.as_str()) {
            p.dark = a != "light";
        }
        fn walk(v: &serde_json::Value, prefix: &str, p: &mut Palette) {
            match v {
                serde_json::Value::Object(m) => {
                    for (k, v) in m {
                        let key = if prefix.is_empty() { k.clone() } else { format!("{prefix}.{k}") };
                        walk(v, &key, p);
                    }
                }
                serde_json::Value::String(s) => {
                    if let Some(c) = parse_hex(s) {
                        p.set(prefix, c);
                    }
                }
                _ => {}
            }
        }
        if let Some(t) = v.get("tokens") {
            walk(t, "", &mut p);
        }
        Ok(p)
    }

    /// Futureboard Default (dark).
    pub fn dark() -> Palette {
        Palette::from_theme_json(include_str!("../themes/Default.json"), &Palette::fallback()).unwrap_or_else(|_| Palette::fallback())
    }

    /// Futureboard Light.
    pub fn light() -> Palette {
        Palette::from_theme_json(include_str!("../themes/Light.json"), &Palette::dark()).unwrap_or_else(|_| Palette::fallback())
    }

    /// `base` lifted by a neutral state layer of `alpha`.
    pub fn lift(&self, base: Color32, alpha: f32) -> Color32 {
        let white = if self.dark { Color32::WHITE } else { Color32::BLACK };
        composite(base, with_alpha(white, alpha))
    }

    /// Fill and border of a latched toggle in `semantic` colour.
    pub fn latched(&self, base: Color32, semantic: Color32) -> (Color32, Color32) {
        (composite(base, with_alpha(semantic, state::ARMED_WASH)), with_alpha(semantic, state::ARMED_BORDER))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn themes_load() {
        let d = Palette::dark();
        assert_eq!(d.surface_base, parse_hex("#232323").unwrap());
        assert_eq!(d.accent_primary, parse_hex("#4D9CF8").unwrap());
        let l = Palette::light();
        assert!(!l.dark);
        assert_ne!(l.surface_base, d.surface_base);
    }

    #[test]
    fn composite_over_opaque() {
        let c = composite(Color32::from_rgb(100, 100, 100), with_alpha(Color32::BLACK, 0.5));
        assert_eq!(c.to_srgba_unmultiplied(), [50, 50, 50, 255]);
    }
}
