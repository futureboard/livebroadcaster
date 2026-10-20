//! The video encoder behind the outputs: a GPU encoder from `mkff-hwenc` (NVENC / QSV / AMF)
//! or MKFF's software H.264.

use filmcraft_isobmff::CodecConfig;
use mkff_codecs::encode::{Packet, VideoEncoder, VideoEncoderConfig};
use mkff_hwenc::{HwEncoderConfig, HwVideoEncoder, Vendor};

use crate::model::EncoderChoice;

pub enum VideoEnc {
    Software(Box<VideoEncoder>),
    Hardware(Box<HwVideoEncoder>),
}

impl VideoEnc {
    /// Open the chosen encoder. `Auto` tries the GPUs (NVIDIA, AMD, Intel) and falls back to
    /// software; an explicit hardware choice that fails is an error. Returns a description.
    pub fn open(choice: EncoderChoice, sw: &VideoEncoderConfig, hw: &HwEncoderConfig) -> Result<(VideoEnc, String), String> {
        let hw_vendor = match choice {
            EncoderChoice::Nvenc => Some(Some(Vendor::Nvidia)),
            EncoderChoice::Qsv => Some(Some(Vendor::Intel)),
            EncoderChoice::Amf => Some(Some(Vendor::Amd)),
            EncoderChoice::Auto => Some(None),
            EncoderChoice::Software => None,
        };
        let mut fallback_note = String::new();
        if let Some(vendor) = hw_vendor {
            match HwVideoEncoder::new(vendor, hw.clone()) {
                Ok(e) => {
                    let desc = format!("{} ({})", e.info().vendor.api_name(), e.info().name);
                    return Ok((VideoEnc::Hardware(Box::new(e)), desc));
                }
                Err(e) if choice != EncoderChoice::Auto => return Err(format!("{}: {e}", choice.name())),
                Err(e) => {
                    log::info!("no usable hardware encoder ({e}); using software");
                    fallback_note = " — no GPU encoder available".into();
                }
            }
        }
        let e = VideoEncoder::new(sw.clone()).map_err(|e| format!("software encoder: {e}"))?;
        Ok((VideoEnc::Software(Box::new(e)), format!("MKFF software H.264{fallback_note}")))
    }

    pub fn encode_rgba(&mut self, rgba: &[u8], index: u64) -> mkff_codecs::Result<Vec<Packet>> {
        match self {
            VideoEnc::Software(e) => e.encode_rgba(rgba, index),
            VideoEnc::Hardware(e) => e.encode_rgba(rgba, index),
        }
    }

    /// Encode NV12 (`w * h` luma, then interleaved CbCr). The software encoder takes planar
    /// 4:2:0, so the chroma is split for it.
    pub fn encode_nv12(&mut self, nv12: &[u8], w: u32, h: u32, index: u64) -> mkff_codecs::Result<Vec<Packet>> {
        match self {
            VideoEnc::Hardware(e) => e.encode_nv12(nv12, index),
            VideoEnc::Software(e) => {
                let (w, h) = (w as usize, h as usize);
                let (y, uv) = nv12.split_at(w * h);
                let n = (w / 2) * (h / 2);
                let (mut u, mut v) = (Vec::with_capacity(n), Vec::with_capacity(n));
                for c in uv[..n * 2].chunks_exact(2) {
                    u.push(c[0]);
                    v.push(c[1]);
                }
                e.encode_yuv420(y, &u, &v, w, w / 2, index)
            }
        }
    }

    pub fn timescale(&self) -> u32 {
        match self {
            VideoEnc::Software(e) => e.timescale(),
            VideoEnc::Hardware(e) => e.timescale(),
        }
    }

    pub fn frame_duration(&self) -> u32 {
        match self {
            VideoEnc::Software(e) => e.frame_duration(),
            VideoEnc::Hardware(e) => e.frame_duration(),
        }
    }

    /// The `avcC` once known (software: at once; hardware: by the first keyframe).
    pub fn avcc(&self) -> Option<Vec<u8>> {
        match self {
            VideoEnc::Software(e) => match e.sample_entry().codec {
                CodecConfig::Avc(c) => Some(c.to_bytes()),
                _ => None,
            },
            VideoEnc::Hardware(e) => e.avcc().map(<[u8]>::to_vec),
        }
    }
}
