//! CPU pictures: straight-alpha RGBA8 images, conversion from MKFF frames, and the blitter the
//! compositor draws scene items with.

use std::sync::Arc;

use mkff_codecs::{ChromaFormat, Frame, Matrix, Planes};
use rayon::prelude::*;

/// Straight (non-premultiplied) sRGB RGBA8, rows packed (`width * 4` bytes each).
#[derive(Clone, Debug, Default)]
pub struct RgbaImage {
    pub width: u32,
    pub height: u32,
    pub data: Vec<u8>,
    /// Sources that know what changed (the browser) say how this picture differs from their
    /// previous one, so the GPU copy is patched instead of re-uploaded whole.
    pub change: Option<Change>,
}

/// This picture is picture `prev` of the same source with only `rect` (x, y, w, h) redrawn.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Change {
    pub seq: u64,
    pub prev: u64,
    pub rect: [u32; 4],
}

impl RgbaImage {
    pub fn new(width: u32, height: u32) -> Self {
        RgbaImage { width, height, data: vec![0; width as usize * height as usize * 4], change: None }
    }

    pub fn from_data(width: u32, height: u32, data: Vec<u8>) -> Self {
        RgbaImage { width, height, data, change: None }
    }

    pub fn filled(width: u32, height: u32, rgba: [u8; 4]) -> Self {
        let mut data = Vec::with_capacity(width as usize * height as usize * 4);
        for _ in 0..width as usize * height as usize {
            data.extend_from_slice(&rgba);
        }
        RgbaImage::from_data(width, height, data)
    }

    pub fn size(&self) -> (u32, u32) {
        (self.width, self.height)
    }

    /// Half the size in each direction, 2×2 box filter (for downscaling without aliasing).
    pub fn half(&self) -> RgbaImage {
        let (w, h) = (self.width as usize, self.height as usize);
        let (nw, nh) = ((w / 2).max(1), (h / 2).max(1));
        let mut out = vec![0u8; nw * nh * 4];
        let src = &self.data;
        out.par_chunks_mut(nw * 4).enumerate().for_each(|(y, row)| {
            let y0 = (y * 2).min(h - 1);
            let y1 = (y * 2 + 1).min(h - 1);
            for x in 0..nw {
                let x0 = (x * 2).min(w - 1);
                let x1 = (x * 2 + 1).min(w - 1);
                let p = |xx: usize, yy: usize, c: usize| src[(yy * w + xx) * 4 + c] as u32;
                for c in 0..4 {
                    row[x * 4 + c] = ((p(x0, y0, c) + p(x1, y0, c) + p(x0, y1, c) + p(x1, y1, c) + 2) / 4) as u8;
                }
            }
        });
        RgbaImage::from_data(nw as u32, nh as u32, out)
    }
}

pub type SharedImage = Arc<RgbaImage>;

// ---------------------------------------------------------------------------------------------
// MKFF frame → RGBA

/// Convert a decoded MKFF frame to RGBA. 8 / 10 / 12-bit BT.601 / 709 Y'CbCr (any subsampling)
/// takes a parallel fast path; anything else goes through MKFF's colour-managed conversion.
pub fn frame_to_rgba(frame: &Frame) -> RgbaImage {
    let (w, h) = (frame.width(), frame.height());
    let color = frame.color();
    let fast_matrix = match color.matrix {
        Matrix::Bt601 => Some((0.299f32, 0.114f32)),
        Matrix::Bt709 => Some((0.2126, 0.0722)),
        _ => None,
    };
    let layout = frame.layout();
    let chroma = match layout {
        mkff_codecs::PixelLayout::Yuv { chroma, .. } => Some(chroma),
        _ => None,
    };
    match (frame.planes(), fast_matrix, chroma) {
        (Planes::Rgba8(d), _, _) => RgbaImage::from_data(w, h, d.to_vec()),
        (Planes::U8 { y, u, v, alpha }, Some(m), Some(c)) => {
            yuv_to_rgba(w, h, c, m, color.full_range, 8, |i| y[i] as u32, |i| u[i] as u32, |i| v[i] as u32, alpha.map(|a| move |i: usize| a[i] as u32))
        }
        (Planes::U16 { y, u, v, alpha }, Some(m), Some(c)) => {
            let bits = match layout {
                mkff_codecs::PixelLayout::Yuv { bit_depth, .. } => bit_depth as u32,
                _ => 10,
            };
            yuv_to_rgba(w, h, c, m, color.full_range, bits, |i| y[i] as u32, |i| u[i] as u32, |i| v[i] as u32, alpha.map(|a| move |i: usize| a[i] as u32))
        }
        _ => RgbaImage::from_data(w, h, frame.to_rgba8()),
    }
}

#[allow(clippy::too_many_arguments)]
fn yuv_to_rgba<Y, U, V, A>(
    w: u32,
    h: u32,
    chroma: ChromaFormat,
    (kr, kb): (f32, f32),
    full_range: bool,
    bits: u32,
    y: Y,
    u: U,
    v: V,
    alpha: Option<A>,
) -> RgbaImage
where
    Y: Fn(usize) -> u32 + Sync,
    U: Fn(usize) -> u32 + Sync,
    V: Fn(usize) -> u32 + Sync,
    A: Fn(usize) -> u32 + Sync,
{
    let (w, h) = (w as usize, h as usize);
    let (sx, sy) = chroma.shifts();
    let cw = w.div_ceil(1 << sx);
    let max = ((1u32 << bits) - 1) as f32;
    let scale = (1u32 << (bits - 8)) as f32;
    let (y_off, y_rng, c_rng) = if full_range { (0.0, max, max) } else { (16.0 * scale, 219.0 * scale, 224.0 * scale) };
    let c_mid = 128.0 * scale;
    let kg = 1.0 - kr - kb;
    // R = Y + cr_r * Cr ; G = Y - cb_g * Cb - cr_g * Cr ; B = Y + cb_b * Cb
    let cr_r = 2.0 * (1.0 - kr);
    let cb_b = 2.0 * (1.0 - kb);
    let cb_g = cb_b * kb / kg;
    let cr_g = cr_r * kr / kg;
    let amax = max;
    let mut data = vec![0u8; w * h * 4];
    data.par_chunks_mut(w * 4).enumerate().for_each(|(row, out)| {
        let crow = (row >> sy) * cw;
        for x in 0..w {
            let yy = (y(row * w + x) as f32 - y_off) / y_rng;
            let ci = crow + (x >> sx);
            let cb = (u(ci) as f32 - c_mid) / c_rng;
            let cr = (v(ci) as f32 - c_mid) / c_rng;
            let r = yy + cr_r * cr;
            let g = yy - cb_g * cb - cr_g * cr;
            let b = yy + cb_b * cb;
            let o = &mut out[x * 4..x * 4 + 4];
            o[0] = (r * 255.0 + 0.5).clamp(0.0, 255.0) as u8;
            o[1] = (g * 255.0 + 0.5).clamp(0.0, 255.0) as u8;
            o[2] = (b * 255.0 + 0.5).clamp(0.0, 255.0) as u8;
            o[3] = match &alpha {
                Some(a) => (a(row * w + x) as f32 / amax * 255.0 + 0.5) as u8,
                None => 255,
            };
        }
    });
    RgbaImage::from_data(w as u32, h as u32, data)
}

// ---------------------------------------------------------------------------------------------
// Blitter

/// Where and how to draw a source picture on the canvas.
#[derive(Clone, Copy, Debug)]
pub struct Placement {
    /// Destination rectangle in canvas pixels (may extend past the canvas).
    pub x: f32,
    pub y: f32,
    pub w: f32,
    pub h: f32,
    /// Source pixels cut off: left, top, right, bottom.
    pub crop: [u32; 4],
    /// 0–255.
    pub opacity: u32,
}

/// Draw `src` over `canvas` (opaque RGBA, `cw × ch`) with bilinear filtering. Large downscales
/// first halve the source with a box filter so detail doesn't alias.
pub fn draw(canvas: &mut [u8], cw: usize, ch: usize, src: &RgbaImage, p: Placement) {
    if p.w < 0.5 || p.h < 0.5 || p.opacity == 0 || src.width == 0 || src.height == 0 {
        return;
    }
    let crop_w = src.width.saturating_sub(p.crop[0] + p.crop[2]);
    let crop_h = src.height.saturating_sub(p.crop[1] + p.crop[3]);
    if crop_w == 0 || crop_h == 0 {
        return;
    }
    // Mip down while the picture is shrunk to less than half.
    let mut level = 0u32;
    while (crop_w >> (level + 1)) as f32 >= p.w && (crop_h >> (level + 1)) as f32 >= p.h && level < 6 {
        level += 1;
    }
    if level > 0 {
        let mut img = src.half();
        for _ in 1..level {
            img = img.half();
        }
        let f = 1u32 << level;
        let p2 = Placement { crop: [p.crop[0] / f, p.crop[1] / f, p.crop[2] / f, p.crop[3] / f], ..p };
        return draw_level(canvas, cw, ch, &img, p2);
    }
    draw_level(canvas, cw, ch, src, p)
}

fn draw_level(canvas: &mut [u8], cw: usize, ch: usize, src: &RgbaImage, p: Placement) {
    let sw = src.width as usize;
    let sh = src.height as usize;
    let cl = p.crop[0] as usize;
    let ct = p.crop[1] as usize;
    let crop_w = sw.saturating_sub(cl + p.crop[2] as usize).max(1);
    let crop_h = sh.saturating_sub(ct + p.crop[3] as usize).max(1);
    let x_begin = p.x.floor().max(0.0) as usize;
    let x_end = ((p.x + p.w).ceil().max(0.0) as usize).min(cw);
    let y_begin = p.y.floor().max(0.0) as usize;
    let y_end = ((p.y + p.h).ceil().max(0.0) as usize).min(ch);
    if x_begin >= x_end || y_begin >= y_end {
        return;
    }
    let fx = crop_w as f32 / p.w;
    let fy = crop_h as f32 / p.h;
    let exact = (fx - 1.0).abs() < 1e-4 && (fy - 1.0).abs() < 1e-4 && p.x.fract() == 0.0 && p.y.fract() == 0.0;

    // Per destination column: source columns and the 8-bit blend weight, or None outside.
    let cols: Vec<Option<(usize, usize, u32)>> = (x_begin..x_end)
        .map(|dx| {
            let c = dx as f32 + 0.5;
            if c < p.x || c >= p.x + p.w {
                return None;
            }
            let s = (c - p.x) * fx - 0.5;
            let s = s.clamp(0.0, (crop_w - 1) as f32);
            let s0 = s.floor() as usize;
            let s1 = (s0 + 1).min(crop_w - 1);
            Some((cl + s0, cl + s1, ((s - s0 as f32) * 256.0) as u32))
        })
        .collect();
    let opacity = p.opacity.min(255);
    let data = &src.data;
    canvas
        .par_chunks_mut(cw * 4)
        .enumerate()
        .skip(y_begin)
        .take(y_end - y_begin)
        .for_each(|(dy, row)| {
            let c = dy as f32 + 0.5;
            if c < p.y || c >= p.y + p.h {
                return;
            }
            let s = ((c - p.y) * fy - 0.5).clamp(0.0, (crop_h - 1) as f32);
            let r0 = s.floor() as usize;
            let r1 = (r0 + 1).min(crop_h - 1);
            let wy = ((s - r0 as f32) * 256.0) as u32;
            let row0 = &data[(ct + r0) * sw * 4..(ct + r0 + 1) * sw * 4];
            let row1 = &data[(ct + r1) * sw * 4..(ct + r1 + 1) * sw * 4];
            for (i, col) in cols.iter().enumerate() {
                let Some((s0, s1, wx)) = *col else { continue };
                let px = if exact {
                    [row0[s0 * 4], row0[s0 * 4 + 1], row0[s0 * 4 + 2], row0[s0 * 4 + 3]]
                } else {
                    bilinear(row0, row1, s0, s1, wx, wy)
                };
                let a = px[3] as u32 * opacity / 255;
                if a == 0 {
                    continue;
                }
                let o = &mut row[(x_begin + i) * 4..(x_begin + i) * 4 + 4];
                if a >= 255 {
                    o[0] = px[0];
                    o[1] = px[1];
                    o[2] = px[2];
                } else {
                    let ia = 255 - a;
                    o[0] = ((px[0] as u32 * a + o[0] as u32 * ia + 127) / 255) as u8;
                    o[1] = ((px[1] as u32 * a + o[1] as u32 * ia + 127) / 255) as u8;
                    o[2] = ((px[2] as u32 * a + o[2] as u32 * ia + 127) / 255) as u8;
                }
                o[3] = 255;
            }
        });
}

#[inline]
fn bilinear(r0: &[u8], r1: &[u8], s0: usize, s1: usize, wx: u32, wy: u32) -> [u8; 4] {
    let mut out = [0u8; 4];
    let (iwx, iwy) = (256 - wx, 256 - wy);
    // Weight colour by alpha so transparent texels don't bleed their (arbitrary) colour.
    let a00 = r0[s0 * 4 + 3] as u32;
    let a01 = r0[s1 * 4 + 3] as u32;
    let a10 = r1[s0 * 4 + 3] as u32;
    let a11 = r1[s1 * 4 + 3] as u32;
    let w00 = iwx * iwy * a00;
    let w01 = wx * iwy * a01;
    let w10 = iwx * wy * a10;
    let w11 = wx * wy * a11;
    let wsum = w00 + w01 + w10 + w11;
    let alpha = (iwx * iwy * a00 + wx * iwy * a01 + iwx * wy * a10 + wx * wy * a11 + 32768) >> 16;
    if wsum == 0 {
        return out;
    }
    for c in 0..3 {
        let v = r0[s0 * 4 + c] as u64 * w00 as u64
            + r0[s1 * 4 + c] as u64 * w01 as u64
            + r1[s0 * 4 + c] as u64 * w10 as u64
            + r1[s1 * 4 + c] as u64 * w11 as u64;
        out[c] = ((v + wsum as u64 / 2) / wsum as u64) as u8;
    }
    out[3] = alpha.min(255) as u8;
    out
}

/// `a = a * (1 - t) + b * t` over whole opaque frames (fade transitions).
pub fn crossfade(a: &mut [u8], b: &[u8], t: f32) {
    let wb = (t.clamp(0.0, 1.0) * 256.0) as u32;
    let wa = 256 - wb;
    a.par_chunks_mut(4096).zip(b.par_chunks(4096)).for_each(|(x, y)| {
        for (p, q) in x.iter_mut().zip(y) {
            *p = ((*p as u32 * wa + *q as u32 * wb) >> 8) as u8;
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn exact_copy_and_blend() {
        let mut canvas = vec![0u8; 4 * 4 * 4];
        let src = RgbaImage::filled(2, 2, [255, 0, 0, 255]);
        draw(&mut canvas, 4, 4, &src, Placement { x: 1.0, y: 1.0, w: 2.0, h: 2.0, crop: [0; 4], opacity: 255 });
        assert_eq!(&canvas[(4 + 1) * 4..(4 + 1) * 4 + 4], &[255, 0, 0, 255]);
        assert_eq!(&canvas[0..4], &[0, 0, 0, 0]);
        let half = RgbaImage::filled(4, 4, [0, 255, 0, 128]);
        let mut c2 = vec![0u8; 4 * 4 * 4];
        draw(&mut c2, 4, 4, &half, Placement { x: 0.0, y: 0.0, w: 4.0, h: 4.0, crop: [0; 4], opacity: 255 });
        assert!((c2[1] as i32 - 128).abs() <= 1);
    }

    #[test]
    fn downscale_uses_mips() {
        let src = RgbaImage::filled(64, 64, [10, 20, 30, 255]);
        let mut canvas = vec![0u8; 8 * 8 * 4];
        draw(&mut canvas, 8, 8, &src, Placement { x: 0.0, y: 0.0, w: 8.0, h: 8.0, crop: [0; 4], opacity: 255 });
        assert_eq!(&canvas[0..4], &[10, 20, 30, 255]);
    }
}
