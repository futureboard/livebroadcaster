//! How fast MKFF's software H.264 encoder runs on this machine:
//! `cargo run --release --example encode_bench -- 1280 720 2500`
use mkff_codecs::encode::{H264Rate, H264Settings, VideoEncodeCodec, VideoEncoder, VideoEncoderConfig};
use std::time::Instant;

fn main() {
    let a: Vec<u32> = std::env::args().skip(1).filter_map(|s| s.parse().ok()).collect();
    let (w, h, kbps) = (*a.first().unwrap_or(&1280), *a.get(1).unwrap_or(&720), *a.get(2).unwrap_or(&2500));
    let mut cfg = VideoEncoderConfig::h264(w, h, 30, 1);
    cfg.codec = VideoEncodeCodec::H264(H264Settings { rate: H264Rate::Cbr { kbps }, keyint: Some(60), ..Default::default() });
    let mut enc = VideoEncoder::new(cfg).unwrap();
    let mut rgba = vec![0u8; (w * h * 4) as usize];
    let frames = 90u64;
    let t = Instant::now();
    let mut bytes = 0usize;
    for i in 0..frames {
        // moving gradient + noise-ish pattern
        for (p, px) in rgba.chunks_exact_mut(4).enumerate() {
            let (x, y) = ((p as u32 % w) as u64, (p as u32 / w) as u64);
            px[0] = ((x + i * 4) % 256) as u8;
            px[1] = ((y + i * 2) % 256) as u8;
            px[2] = ((x * y + i) % 251) as u8;
            px[3] = 255;
        }
        let t1 = Instant::now();
        for p in enc.encode_rgba(&rgba, i).unwrap() { bytes += p.data.len(); }
        if i % 30 == 0 { println!("frame {i}: {:.1} ms", t1.elapsed().as_secs_f64() * 1000.0); }
    }
    let el = t.elapsed().as_secs_f64();
    println!("{w}x{h}: {frames} frames in {el:.2}s (incl. pattern gen) → {:.1} fps, {:.0} kb/s", frames as f64 / el, bytes as f64 * 8.0 / (frames as f64 / 30.0) / 1000.0);
}
