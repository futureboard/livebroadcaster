//! System UI fonts, as in Sphere's composite font: the platform UI face first, then a
//! coverage-ordered fallback chain (Thai, CJK, symbols) — no language detection, per-glyph
//! fallback only. egui's bundled fonts stay last for its emoji / icon glyphs.

use egui::{FontData, FontDefinitions, FontFamily};

/// Family for semibold text (titles, buttons that need weight).
pub const SEMIBOLD: &str = "sphere-semibold";

#[cfg(target_os = "windows")]
const REGULAR: &[&str] = &[
    "C:/Windows/Fonts/segoeui.ttf",
    "C:/Windows/Fonts/LeelawUI.ttf",
    "C:/Windows/Fonts/tahoma.ttf",
    "C:/Windows/Fonts/msyh.ttc",
    "C:/Windows/Fonts/YuGothM.ttc",
    "C:/Windows/Fonts/malgun.ttf",
    "C:/Windows/Fonts/Nirmala.ttc",
    "C:/Windows/Fonts/seguisym.ttf",
];
#[cfg(target_os = "windows")]
const BOLD: &[&str] = &[
    "C:/Windows/Fonts/seguisb.ttf",
    "C:/Windows/Fonts/LeelUIsl.ttf",
    "C:/Windows/Fonts/LeelaUIb.ttf",
    "C:/Windows/Fonts/msyhbd.ttc",
    "C:/Windows/Fonts/YuGothB.ttc",
];

/// Which CJK face draws the Han characters Chinese and Japanese share (their shapes differ).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum HanPreference {
    #[default]
    Chinese,
    Japanese,
}

/// Japanese faces in the chains (moved ahead of the Chinese ones for Japanese).
const JAPANESE: &[&str] = &["YuGoth", "Hiragino", "NotoSansJP"];
const CHINESE: &[&str] = &["msyh", "PingFang", "NotoSansCJK", "NotoSansSC"];

fn ordered(paths: &[&'static str], han: HanPreference) -> Vec<&'static str> {
    let mut v = paths.to_vec();
    if han == HanPreference::Japanese {
        let is = |p: &str, set: &[&str]| set.iter().any(|k| p.contains(k));
        if let (Some(zh), Some(ja)) = (v.iter().position(|p| is(p, CHINESE)), v.iter().position(|p| is(p, JAPANESE))) {
            if ja > zh {
                let j = v.remove(ja);
                v.insert(zh, j);
            }
        }
    }
    v
}
#[cfg(target_os = "windows")]
const MONO: &[&str] = &["C:/Windows/Fonts/CascadiaMono.ttf", "C:/Windows/Fonts/consola.ttf"];

#[cfg(target_os = "macos")]
const REGULAR: &[&str] = &[
    "/System/Library/Fonts/SFNS.ttf",
    "/System/Library/Fonts/Supplemental/Thonburi.ttc",
    "/System/Library/Fonts/PingFang.ttc",
    "/System/Library/Fonts/Apple Symbols.ttf",
];
#[cfg(target_os = "macos")]
const BOLD: &[&str] = &["/System/Library/Fonts/SFNS.ttf"];
#[cfg(target_os = "macos")]
const MONO: &[&str] = &["/System/Library/Fonts/SFNSMono.ttf", "/System/Library/Fonts/Menlo.ttc"];

#[cfg(not(any(target_os = "windows", target_os = "macos")))]
const REGULAR: &[&str] = &[
    "/usr/share/fonts/truetype/noto/NotoSans-Regular.ttf",
    "/usr/share/fonts/noto/NotoSans-Regular.ttf",
    "/usr/share/fonts/truetype/noto/NotoSansThai-Regular.ttf",
    "/usr/share/fonts/noto/NotoSansThai-Regular.ttf",
    "/usr/share/fonts/opentype/noto/NotoSansCJK-Regular.ttc",
    "/usr/share/fonts/truetype/dejavu/DejaVuSans.ttf",
];
#[cfg(not(any(target_os = "windows", target_os = "macos")))]
const BOLD: &[&str] = &["/usr/share/fonts/truetype/noto/NotoSans-SemiBold.ttf", "/usr/share/fonts/truetype/noto/NotoSansThai-SemiBold.ttf"];
#[cfg(not(any(target_os = "windows", target_os = "macos")))]
const MONO: &[&str] = &["/usr/share/fonts/truetype/noto/NotoSansMono-Regular.ttf", "/usr/share/fonts/truetype/dejavu/DejaVuSansMono.ttf"];

fn load(fonts: &mut FontDefinitions, paths: &[&str]) -> Vec<String> {
    let mut names = Vec::new();
    for p in paths {
        let name = format!("sys:{p}");
        if fonts.font_data.contains_key(&name) {
            names.push(name);
            continue;
        }
        if let Ok(bytes) = std::fs::read(p) {
            fonts.font_data.insert(name.clone(), std::sync::Arc::new(FontData::from_owned(bytes)));
            names.push(name);
        }
    }
    names
}

/// Install the system font chain. Returns false when no system font was found (egui's bundled
/// fonts are used then).
pub fn install(ctx: &egui::Context) -> bool {
    install_for(ctx, HanPreference::default())
}

/// [`install`], with the CJK face order for an interface language.
pub fn install_for(ctx: &egui::Context, han: HanPreference) -> bool {
    let mut fonts = FontDefinitions::default();
    let builtin_prop = fonts.families.get(&FontFamily::Proportional).cloned().unwrap_or_default();
    let builtin_mono = fonts.families.get(&FontFamily::Monospace).cloned().unwrap_or_default();
    let regular = load(&mut fonts, &ordered(REGULAR, han));
    let bold = load(&mut fonts, &ordered(BOLD, han));
    let mono = load(&mut fonts, MONO);
    let found = !regular.is_empty();
    // Scripts beyond the primary face (everything after the first regular font).
    let fallbacks: Vec<String> = regular.iter().skip(1).cloned().collect();

    let mut prop = regular.clone();
    prop.extend(builtin_prop.iter().cloned());
    fonts.families.insert(FontFamily::Proportional, prop);

    let mut semi = bold.clone();
    semi.extend(regular.iter().cloned());
    semi.extend(builtin_prop.iter().cloned());
    fonts.families.insert(FontFamily::Name(SEMIBOLD.into()), semi);

    let mut m = mono;
    m.extend(builtin_mono);
    m.extend(fallbacks);
    fonts.families.insert(FontFamily::Monospace, m);
    ctx.set_fonts(fonts);
    found
}
