//! Interface languages: English (the source text), Thai, Simplified Chinese, Japanese and
//! Indonesian. gettext-style: code writes the English text in [`tr!`] / [`trf!`], the catalogs
//! in `i18n/*.json` map it to the other languages (missing entries fall back to English).
//! The language switches at run time; the UI redraws in it on the next frame.

use std::collections::HashMap;
use std::sync::atomic::{AtomicU8, Ordering};
use std::sync::OnceLock;

use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum Lang {
    En,
    Th,
    Zh,
    Ja,
    Id,
}

impl Lang {
    pub const ALL: [Lang; 5] = [Lang::En, Lang::Th, Lang::Zh, Lang::Ja, Lang::Id];

    /// The language's own name (shown in the picker whatever the current language).
    pub fn native_name(self) -> &'static str {
        match self {
            Lang::En => "English",
            Lang::Th => "ไทย",
            Lang::Zh => "简体中文",
            Lang::Ja => "日本語",
            Lang::Id => "Bahasa Indonesia",
        }
    }

    fn catalog_source(self) -> Option<&'static str> {
        match self {
            Lang::En => None,
            Lang::Th => Some(include_str!("../i18n/th.json")),
            Lang::Zh => Some(include_str!("../i18n/zh.json")),
            Lang::Ja => Some(include_str!("../i18n/ja.json")),
            Lang::Id => Some(include_str!("../i18n/id.json")),
        }
    }

    fn index(self) -> u8 {
        Lang::ALL.iter().position(|l| *l == self).unwrap() as u8
    }

    /// The system's UI language, when it is one of ours (else English).
    pub fn system() -> Lang {
        let tag = system_locale().to_ascii_lowercase();
        match tag.split(['-', '_']).next().unwrap_or("") {
            "th" => Lang::Th,
            "zh" => Lang::Zh,
            "ja" => Lang::Ja,
            "id" | "in" => Lang::Id,
            _ => Lang::En,
        }
    }
}

#[cfg(windows)]
fn system_locale() -> String {
    use windows::Win32::Globalization::GetUserDefaultLocaleName;
    let mut buf = [0u16; 85];
    let n = unsafe { GetUserDefaultLocaleName(&mut buf) };
    if n > 1 {
        String::from_utf16_lossy(&buf[..n as usize - 1])
    } else {
        String::new()
    }
}

#[cfg(not(windows))]
fn system_locale() -> String {
    std::env::var("LC_ALL").or_else(|_| std::env::var("LC_MESSAGES")).or_else(|_| std::env::var("LANG")).unwrap_or_default()
}

static CURRENT: AtomicU8 = AtomicU8::new(0);

type Catalog = HashMap<&'static str, &'static str>;

fn catalogs() -> &'static [Option<Catalog>; 5] {
    static C: OnceLock<[Option<Catalog>; 5]> = OnceLock::new();
    C.get_or_init(|| {
        Lang::ALL.map(|l| {
            l.catalog_source().map(|src| {
                let map: HashMap<String, String> = serde_json::from_str(src).unwrap_or_else(|e| panic!("i18n/{l:?}.json: {e}"));
                // Leaked once for the process: lookups hand out &'static str.
                map.into_iter().filter(|(_, v)| !v.is_empty()).map(|(k, v)| (&*Box::leak(k.into_boxed_str()), &*Box::leak(v.into_boxed_str()))).collect()
            })
        })
    })
}

pub fn set_lang(l: Lang) {
    CURRENT.store(l.index(), Ordering::Relaxed);
}

pub fn lang() -> Lang {
    Lang::ALL[CURRENT.load(Ordering::Relaxed) as usize]
}

/// `english` in the current language.
pub fn t(english: &'static str) -> &'static str {
    match &catalogs()[CURRENT.load(Ordering::Relaxed) as usize] {
        Some(c) => c.get(english).copied().unwrap_or(english),
        None => english,
    }
}

/// Fill `{name}` placeholders of a (translated) template.
pub fn fill(template: &str, args: &[(&str, &dyn std::fmt::Display)]) -> String {
    let mut out = String::with_capacity(template.len() + 16);
    let mut rest = template;
    while let Some(open) = rest.find('{') {
        out.push_str(&rest[..open]);
        let after = &rest[open + 1..];
        match after.find('}') {
            Some(close) => {
                let name = &after[..close];
                match args.iter().find(|(k, _)| *k == name) {
                    Some((_, v)) => {
                        use std::fmt::Write;
                        let _ = write!(out, "{v}");
                    }
                    None => {
                        out.push('{');
                        out.push_str(name);
                        out.push('}');
                    }
                }
                rest = &after[close + 1..];
            }
            None => {
                out.push_str(&rest[open..]);
                rest = "";
            }
        }
    }
    out.push_str(rest);
    out
}

/// The interface text `"English"` in the current language (`&'static str`).
#[macro_export]
macro_rules! tr {
    ($s:literal) => {
        $crate::i18n::t($s)
    };
}

/// A translated message with `{name}` placeholders: `trf!("Saved to {path}", path = p.display())`
/// (`name` alone means `name = name`). Formatting options belong in the argument
/// (`cpu = format!("{cpu:.0}")`), so translators only move the placeholder.
#[macro_export]
macro_rules! trf {
    ($s:literal $(, $k:ident $(= $v:expr)?)* $(,)?) => {
        $crate::i18n::fill($crate::i18n::t($s), &[$((stringify!($k), &$crate::trf!(@v $k $(= $v)?) as &dyn ::std::fmt::Display)),*])
    };
    (@v $k:ident = $v:expr) => { $v };
    (@v $k:ident) => { $k };
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fill_placeholders() {
        let path = "C:/x";
        assert_eq!(fill("Saved to {path} ({n})", &[("path", &path), ("n", &3)]), "Saved to C:/x (3)");
        assert_eq!(fill("{missing} {", &[]), "{missing} {");
        let e = "boom";
        assert_eq!(crate::trf!("Export failed: {e}", e), "Export failed: boom");
    }

    /// Every text the code asks for has an entry in every catalog, and each translation keeps
    /// the English placeholders (and menu accelerators).
    #[test]
    fn catalogs_cover_the_code() {
        let mut keys = std::collections::BTreeSet::new();
        let re_start = ["tr!(\"", "trf!(\""];
        fn walk(dir: &std::path::Path, out: &mut Vec<std::path::PathBuf>) {
            for e in std::fs::read_dir(dir).unwrap().flatten() {
                let p = e.path();
                if p.is_dir() {
                    walk(&p, out);
                } else if p.extension().is_some_and(|x| x == "rs") {
                    out.push(p);
                }
            }
        }
        let mut files = Vec::new();
        walk(std::path::Path::new(concat!(env!("CARGO_MANIFEST_DIR"), "/src")), &mut files);
        for f in files {
            // This file's own examples and include_str!s aren't interface texts.
            if f.ends_with("i18n.rs") {
                continue;
            }
            let src = std::fs::read_to_string(&f).unwrap();
            for start in re_start {
                let mut rest = src.as_str();
                while let Some(i) = rest.find(start) {
                    let s = &rest[i + start.len()..];
                    // The literal up to the closing quote (with \" escapes).
                    let mut end = 0;
                    let b = s.as_bytes();
                    while end < b.len() && !(b[end] == b'"' && (end == 0 || b[end - 1] != b'\\')) {
                        end += 1;
                    }
                    let lit = s[..end].replace("\\\"", "\"").replace("\\n", "\n").replace("\\t", "\t").replace("\\\\", "\\");
                    keys.insert(lit);
                    rest = &s[end..];
                }
            }
        }
        assert!(keys.len() > 50, "found only {} texts", keys.len());
        let placeholders = |s: &str| {
            let mut v: Vec<String> = s.match_indices('{').filter_map(|(i, _)| s[i + 1..].find('}').map(|j| s[i + 1..i + 1 + j].to_string())).collect();
            v.sort();
            v
        };
        let mut problems = Vec::new();
        for (i, l) in Lang::ALL.iter().enumerate().skip(1) {
            let c = catalogs()[i].as_ref().unwrap();
            for k in &keys {
                match c.get(k.as_str()) {
                    None => problems.push(format!("{l:?}: missing {k:?}")),
                    Some(v) => {
                        if placeholders(k) != placeholders(v) {
                            problems.push(format!("{l:?}: placeholders differ in {k:?} → {v:?}"));
                        }
                        if k.contains('\t') != v.contains('\t') {
                            problems.push(format!("{l:?}: shortcut tab lost in {k:?}"));
                        }
                    }
                }
            }
            for k in c.keys() {
                if !keys.contains(*k) {
                    problems.push(format!("{l:?}: unused {k:?}"));
                }
            }
        }
        assert!(problems.is_empty(), "{} catalog problems:\n{}", problems.len(), problems.join("\n"));
    }
}
