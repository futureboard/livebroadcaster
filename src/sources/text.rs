//! Text source: rasterised with ab_glyph, with per-character font fallback (Thai and other
//! scripts fall back to the system fonts that have them), outline, background and wrapping.

use std::sync::Arc;
use std::time::{Duration, SystemTime};

use ab_glyph::{point, Font, FontVec, PxScale, ScaleFont};

use super::SourceShared;
use crate::model::{SourceKind, TextAlign, TextSettings};
use crate::video::RgbaImage;

/// System fonts tried after the chosen one, in order.
#[cfg(windows)]
const FALLBACK_FONTS: &[&str] = &[
    "C:/Windows/Fonts/segoeui.ttf",
    "C:/Windows/Fonts/LeelawUI.ttf",
    "C:/Windows/Fonts/tahoma.ttf",
    "C:/Windows/Fonts/msyh.ttc",
    "C:/Windows/Fonts/YuGothM.ttc",
    "C:/Windows/Fonts/malgun.ttf",
    "C:/Windows/Fonts/seguiemj.ttf",
    "C:/Windows/Fonts/seguisym.ttf",
];
#[cfg(not(windows))]
const FALLBACK_FONTS: &[&str] = &[
    "/usr/share/fonts/truetype/dejavu/DejaVuSans.ttf",
    "/usr/share/fonts/truetype/noto/NotoSans-Regular.ttf",
    "/usr/share/fonts/truetype/noto/NotoSansThai-Regular.ttf",
    "/System/Library/Fonts/Helvetica.ttc",
    "/System/Library/Fonts/Supplemental/Thonburi.ttc",
];

pub fn run(kind: SourceKind, s: Arc<SourceShared>) {
    let SourceKind::Text(t) = kind else { return };
    let fonts = load_fonts(&t.font_path);
    if fonts.is_empty() {
        s.set_error("no usable font found");
        return;
    }
    match &t.from_file {
        None => {
            s.video.set(render(&t, &t.text, &fonts));
        }
        Some(path) => {
            let mut seen: Option<SystemTime> = None;
            loop {
                let mtime = std::fs::metadata(path).and_then(|m| m.modified()).ok();
                if mtime != seen {
                    match std::fs::read(path) {
                        Ok(bytes) => {
                            let text = String::from_utf8_lossy(&bytes);
                            s.video.set(render(&t, text.trim_end_matches(['\r', '\n']), &fonts));
                            s.clear_error();
                        }
                        Err(e) => s.set_error(format!("{path}: {e}")),
                    }
                    seen = mtime;
                }
                if s.sleep(Duration::from_millis(500)) {
                    return;
                }
            }
        }
    }
}

fn load_fonts(chosen: &str) -> Vec<FontVec> {
    let mut out = Vec::new();
    let paths = std::iter::once(chosen).filter(|p| !p.is_empty()).chain(FALLBACK_FONTS.iter().copied());
    for p in paths {
        if let Ok(data) = std::fs::read(p) {
            if let Ok(f) = FontVec::try_from_vec_and_index(data, 0) {
                out.push(f);
            }
        }
    }
    out
}

struct Glyph {
    font: usize,
    id: ab_glyph::GlyphId,
    x: f32,
}

struct Line {
    glyphs: Vec<Glyph>,
    width: f32,
}

fn font_for(fonts: &[FontVec], c: char) -> usize {
    fonts.iter().position(|f| f.glyph_id(c).0 != 0).unwrap_or(0)
}

fn layout(t: &TextSettings, text: &str, fonts: &[FontVec], scale: PxScale) -> Vec<Line> {
    let mut lines = Vec::new();
    for para in text.split('\n') {
        let para = para.trim_end_matches('\r');
        let chars: Vec<char> = para.chars().collect();
        let mut start = 0;
        loop {
            // Greedy wrap: break at the last space that fits, else at the last character that fits.
            let mut glyphs = Vec::new();
            let mut x = 0.0f32;
            let mut prev: Option<(usize, ab_glyph::GlyphId)> = None;
            let mut last_space: Option<(usize, usize, f32)> = None; // (char index, glyph count, width)
            let mut i = start;
            while i < chars.len() {
                let c = chars[i];
                let fi = font_for(fonts, c);
                let sf = fonts[fi].as_scaled(scale);
                let id = fonts[fi].glyph_id(c);
                if let Some((pf, pid)) = prev {
                    if pf == fi {
                        x += sf.kern(pid, id);
                    }
                }
                let adv = sf.h_advance(id);
                if t.wrap_width > 0 && x + adv > t.wrap_width as f32 && i > start && !c.is_whitespace() {
                    break;
                }
                if c.is_whitespace() {
                    last_space = Some((i, glyphs.len(), x));
                }
                glyphs.push(Glyph { font: fi, id, x });
                x += adv;
                prev = Some((fi, id));
                i += 1;
            }
            if i < chars.len() {
                if let Some((si, gi, w)) = last_space.filter(|(si, _, _)| *si > start) {
                    glyphs.truncate(gi);
                    lines.push(Line { glyphs, width: w });
                    start = si + 1;
                    continue;
                }
                lines.push(Line { glyphs, width: x });
                start = i;
            } else {
                lines.push(Line { glyphs, width: x });
                break;
            }
        }
    }
    lines
}

pub fn render(t: &TextSettings, text: &str, fonts: &[FontVec]) -> RgbaImage {
    let scale = PxScale::from(t.size.clamp(4.0, 512.0));
    let primary = fonts[0].as_scaled(scale);
    let line_h = (primary.ascent() - primary.descent() + primary.line_gap()).ceil();
    let ascent = primary.ascent();
    let lines = layout(t, text, fonts, scale);
    let outline = if t.outline { t.outline_size.clamp(0.0, 32.0) } else { 0.0 };
    let margin = t.padding as f32 + outline.ceil();
    let text_w = lines.iter().map(|l| l.width).fold(0.0f32, f32::max).ceil();
    let w = ((text_w + 2.0 * margin).ceil() as usize).max(1);
    let h = ((lines.len().max(1) as f32 * line_h + 2.0 * margin).ceil() as usize).max(1);

    let mut mask = vec![0f32; w * h];
    for (li, line) in lines.iter().enumerate() {
        let off = match t.align {
            TextAlign::Left => 0.0,
            TextAlign::Center => (text_w - line.width) / 2.0,
            TextAlign::Right => text_w - line.width,
        };
        let baseline = margin + li as f32 * line_h + ascent;
        for g in &line.glyphs {
            let glyph = g.id.with_scale_and_position(scale, point(margin + off + g.x, baseline));
            if let Some(og) = fonts[g.font].outline_glyph(glyph) {
                let b = og.px_bounds();
                og.draw(|x, y, c| {
                    let px = b.min.x as i32 + x as i32;
                    let py = b.min.y as i32 + y as i32;
                    if px >= 0 && py >= 0 && (px as usize) < w && (py as usize) < h {
                        let m = &mut mask[py as usize * w + px as usize];
                        *m = (*m + c).min(1.0);
                    }
                });
            }
        }
    }

    let outline_mask = (outline > 0.0).then(|| dilate(&mask, w, h, outline));
    let mut img = RgbaImage::filled(w as u32, h as u32, t.background);
    for i in 0..w * h {
        let mut px = straight_to_premul(t.background);
        if let Some(om) = &outline_mask {
            px = over(premul(t.outline_color, om[i]), px);
        }
        px = over(premul(t.color, mask[i]), px);
        let out = premul_to_straight(px);
        img.data[i * 4..i * 4 + 4].copy_from_slice(&out);
    }
    img
}

/// Max filter over a disk of `radius` pixels (anti-aliased edge).
fn dilate(mask: &[f32], w: usize, h: usize, radius: f32) -> Vec<f32> {
    let r = radius.ceil() as i32;
    let mut offsets = Vec::new();
    for dy in -r..=r {
        for dx in -r..=r {
            let d = ((dx * dx + dy * dy) as f32).sqrt();
            let wgt = (radius + 0.5 - d).clamp(0.0, 1.0);
            if wgt > 0.0 {
                offsets.push((dx, dy, wgt));
            }
        }
    }
    let mut out = vec![0f32; w * h];
    use rayon::prelude::*;
    out.par_chunks_mut(w).enumerate().for_each(|(y, row)| {
        for (x, o) in row.iter_mut().enumerate() {
            let mut m = 0f32;
            for &(dx, dy, wgt) in &offsets {
                let (sx, sy) = (x as i32 + dx, y as i32 + dy);
                if sx >= 0 && sy >= 0 && (sx as usize) < w && (sy as usize) < h {
                    m = m.max(mask[sy as usize * w + sx as usize] * wgt);
                    if m >= 1.0 {
                        break;
                    }
                }
            }
            *o = m;
        }
    });
    out
}

type Premul = [f32; 4];

fn premul(c: [u8; 4], coverage: f32) -> Premul {
    let a = c[3] as f32 / 255.0 * coverage;
    [c[0] as f32 / 255.0 * a, c[1] as f32 / 255.0 * a, c[2] as f32 / 255.0 * a, a]
}

fn straight_to_premul(c: [u8; 4]) -> Premul {
    premul(c, 1.0)
}

fn over(top: Premul, bottom: Premul) -> Premul {
    let k = 1.0 - top[3];
    [top[0] + bottom[0] * k, top[1] + bottom[1] * k, top[2] + bottom[2] * k, top[3] + bottom[3] * k]
}

fn premul_to_straight(p: Premul) -> [u8; 4] {
    if p[3] <= 0.0 {
        return [0, 0, 0, 0];
    }
    let q = |v: f32| ((v / p[3]).clamp(0.0, 1.0) * 255.0 + 0.5) as u8;
    [q(p[0]), q(p[1]), q(p[2]), (p[3].clamp(0.0, 1.0) * 255.0 + 0.5) as u8]
}
