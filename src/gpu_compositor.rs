//! The GPU compositor: source pictures live in wgpu textures (uploaded only when the source
//! publishes a new one), scenes are drawn with the same rules as the CPU blitter in
//! [`crate::video`] (alpha-weighted bilinear, box-filtered mips for large downscales, straight
//! alpha over an opaque canvas), the program stays on the GPU for the preview, and the encoders
//! get NV12 (BT.709, limited range) converted on the GPU and read back asynchronously.
//!
//! It runs on the window's wgpu device, so the preview shows the program texture without any
//! copy through the CPU.

use std::collections::HashMap;
use std::sync::atomic::{AtomicU8, Ordering};
use std::sync::{Arc, Weak};

use eframe::egui_wgpu::wgpu;

use crate::model::Id;
use crate::video::{Placement, RgbaImage, SharedImage};

/// A wgpu device and queue (the window's, or a headless one).
#[derive(Clone)]
pub struct Gpu {
    pub device: wgpu::Device,
    pub queue: wgpu::Queue,
    /// "NVIDIA GeForce GTX 1060 3GB · Dx12".
    pub name: String,
}

impl Gpu {
    /// A device of its own (tests).
    #[cfg(test)]
    pub fn headless(backends: wgpu::Backends) -> Option<Gpu> {
        let instance = wgpu::Instance::new(&wgpu::InstanceDescriptor { backends, ..Default::default() });
        let adapter = pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions {
            power_preference: wgpu::PowerPreference::HighPerformance,
            force_fallback_adapter: false,
            compatible_surface: None,
        }))?;
        let info = adapter.get_info();
        let (device, queue) = pollster::block_on(adapter.request_device(
            &wgpu::DeviceDescriptor {
                label: Some("compositor"),
                required_features: wgpu::Features::empty(),
                required_limits: wgpu::Limits::downlevel_webgl2_defaults().using_resolution(adapter.limits()),
                memory_hints: wgpu::MemoryHints::Performance,
            },
            None,
        ))
        .ok()?;
        Some(Gpu { device, queue, name: format!("{} · {:?}", info.name, info.backend) })
    }
}

/// One picture to draw.
pub struct GpuLayer<'a> {
    pub source: Id,
    pub image: &'a SharedImage,
    pub placement: Placement,
}

/// The program picture for the preview: an sRGB view of the latest frame. `generation` changes
/// when the texture is replaced (canvas resized).
#[derive(Clone)]
pub struct ProgramTexture {
    pub view: wgpu::TextureView,
    pub generation: u64,
}

/// An encoder frame read back from the GPU.
pub struct Nv12Frame {
    pub tick: u64,
    pub width: u32,
    pub height: u32,
    /// `width * height` luma bytes, then interleaved CbCr (`width * height / 2`).
    pub data: Vec<u8>,
}

pub struct RenderOutput {
    /// Encoder frames whose read-back finished, oldest first.
    pub nv12: Vec<Nv12Frame>,
    /// A read-back was wanted but every buffer was still in flight.
    pub readback_dropped: bool,
}

const SHADER: &str = r#"
struct Draw {
    dst: vec4<f32>,   // x0, y0, x1, y1 in canvas pixels
    src: vec4<f32>,   // crop left, top, width, height in texels of `level`
    misc: vec4<f32>,  // canvas width, canvas height, opacity (0..1), mip level
};
@group(0) @binding(0) var<uniform> d: Draw;
@group(1) @binding(0) var tex: texture_2d<f32>;

@vertex
fn vs_layer(@builtin(vertex_index) i: u32) -> @builtin(position) vec4<f32> {
    var corners = array<vec2<f32>, 6>(
        vec2<f32>(0.0, 0.0), vec2<f32>(1.0, 0.0), vec2<f32>(0.0, 1.0),
        vec2<f32>(0.0, 1.0), vec2<f32>(1.0, 0.0), vec2<f32>(1.0, 1.0));
    let p = mix(d.dst.xy, d.dst.zw, corners[i]);
    return vec4<f32>(p.x / d.misc.x * 2.0 - 1.0, 1.0 - p.y / d.misc.y * 2.0, 0.0, 1.0);
}

@fragment
fn fs_layer(@builtin(position) pos: vec4<f32>) -> @location(0) vec4<f32> {
    let level = i32(d.misc.w);
    let size = d.src.zw;
    let f = size / (d.dst.zw - d.dst.xy);
    let s = clamp((pos.xy - d.dst.xy) * f - 0.5, vec2<f32>(0.0), size - 1.0);
    let s0 = floor(s);
    let w = s - s0;
    let i0 = vec2<i32>(s0 + d.src.xy);
    let i1 = vec2<i32>(min(s0 + 1.0, size - 1.0) + d.src.xy);
    let t00 = textureLoad(tex, vec2<i32>(i0.x, i0.y), level);
    let t01 = textureLoad(tex, vec2<i32>(i1.x, i0.y), level);
    let t10 = textureLoad(tex, vec2<i32>(i0.x, i1.y), level);
    let t11 = textureLoad(tex, vec2<i32>(i1.x, i1.y), level);
    let w00 = (1.0 - w.x) * (1.0 - w.y);
    let w01 = w.x * (1.0 - w.y);
    let w10 = (1.0 - w.x) * w.y;
    let w11 = w.x * w.y;
    // Colour weighted by alpha, so transparent texels don't bleed their (arbitrary) colour.
    let a = t00.a * w00 + t01.a * w01 + t10.a * w10 + t11.a * w11;
    var rgb = vec3<f32>(0.0);
    if (a > 0.0) {
        rgb = (t00.rgb * (t00.a * w00) + t01.rgb * (t01.a * w01) + t10.rgb * (t10.a * w10) + t11.rgb * (t11.a * w11)) / a;
    }
    return vec4<f32>(rgb, a * d.misc.z);
}

@group(0) @binding(0) var src_img: texture_2d<f32>;

@vertex
fn vs_full(@builtin(vertex_index) i: u32) -> @builtin(position) vec4<f32> {
    let uv = vec2<f32>(f32((i << 1u) & 2u), f32(i & 2u));
    return vec4<f32>(uv * vec2<f32>(2.0, -2.0) + vec2<f32>(-1.0, 1.0), 0.0, 1.0);
}

// 2x2 box filter of the level above.
@fragment
fn fs_mip(@builtin(position) pos: vec4<f32>) -> @location(0) vec4<f32> {
    let last = vec2<i32>(textureDimensions(src_img)) - 1;
    let p0 = min(vec2<i32>(pos.xy) * 2, last);
    let p1 = min(vec2<i32>(pos.xy) * 2 + 1, last);
    return (textureLoad(src_img, p0, 0) + textureLoad(src_img, vec2<i32>(p1.x, p0.y), 0)
        + textureLoad(src_img, vec2<i32>(p0.x, p1.y), 0) + textureLoad(src_img, p1, 0)) * 0.25;
}

// BT.709, limited range.
@fragment
fn fs_y(@builtin(position) pos: vec4<f32>) -> @location(0) vec4<f32> {
    let c = textureLoad(src_img, vec2<i32>(pos.xy), 0).rgb;
    return vec4<f32>(16.0 / 255.0 + dot(c, vec3<f32>(0.2126, 0.7152, 0.0722)) * (219.0 / 255.0), 0.0, 0.0, 1.0);
}

@fragment
fn fs_uv(@builtin(position) pos: vec4<f32>) -> @location(0) vec4<f32> {
    let p = vec2<i32>(pos.xy) * 2;
    let c = (textureLoad(src_img, p, 0) + textureLoad(src_img, p + vec2<i32>(1, 0), 0)
        + textureLoad(src_img, p + vec2<i32>(0, 1), 0) + textureLoad(src_img, p + vec2<i32>(1, 1), 0)).rgb * 0.25;
    let y = dot(c, vec3<f32>(0.2126, 0.7152, 0.0722));
    let u = (c.b - y) / 1.8556;
    let v = (c.r - y) / 1.5748;
    return vec4<f32>(128.0 / 255.0 + u * (224.0 / 255.0), 128.0 / 255.0 + v * (224.0 / 255.0), 0.0, 1.0);
}
"#;

/// Size of one draw's uniforms (`Draw` above).
const DRAW_BYTES: u64 = 48;
const READBACK_SLOTS: usize = 3;
/// Source textures not drawn for this many frames are freed.
const TEXTURE_IDLE_FRAMES: u64 = 300;

const FREE: u8 = 0;
const PENDING: u8 = 1;
const READY: u8 = 2;
const FAILED: u8 = 3;

/// A source picture on the GPU, with a mip chain filled on demand.
struct SourceTexture {
    tex: wgpu::Texture,
    /// All levels, for drawing.
    draw_bg: wgpu::BindGroup,
    /// One view per level (render target of that level, input of the next).
    level_views: Vec<wgpu::TextureView>,
    level_bgs: Vec<wgpu::BindGroup>,
    width: u32,
    height: u32,
    /// Levels holding the current picture (1 = just the base).
    ready_levels: u32,
    /// The picture uploaded (a weak handle: its allocation can't be reused for another
    /// picture while we hold it, so pointer equality is safe).
    uploaded: Weak<RgbaImage>,
    /// The source's sequence number of the uploaded picture (sources that report changes).
    uploaded_seq: Option<u64>,
    last_used: u64,
}

/// A render target the size of the canvas.
struct Canvas {
    tex: wgpu::Texture,
    view: wgpu::TextureView,
    /// The canvas as a layer source (fades) / NV12 input.
    bg: wgpu::BindGroup,
    width: u32,
    height: u32,
}

struct Nv12Targets {
    y_view: wgpu::TextureView,
    uv_view: wgpu::TextureView,
    y_tex: wgpu::Texture,
    uv_tex: wgpu::Texture,
    width: u32,
    height: u32,
    y_pitch: u32,
    uv_pitch: u32,
}

struct Readback {
    buf: wgpu::Buffer,
    state: Arc<AtomicU8>,
    tick: u64,
    width: u32,
    height: u32,
    y_pitch: u32,
    uv_pitch: u32,
}

pub struct GpuCompositor {
    gpu: Gpu,
    layer_pipeline: wgpu::RenderPipeline,
    mip_pipeline: wgpu::RenderPipeline,
    y_pipeline: wgpu::RenderPipeline,
    uv_pipeline: wgpu::RenderPipeline,
    uniform_bgl: wgpu::BindGroupLayout,
    tex_bgl: wgpu::BindGroupLayout,
    /// Per-draw uniforms at `stride` offsets.
    uniforms: wgpu::Buffer,
    uniform_bg: wgpu::BindGroup,
    uniform_capacity: u64,
    stride: u64,
    sources: HashMap<Id, SourceTexture>,
    program: Option<Canvas>,
    /// The outgoing scene during a fade.
    fade: Option<Canvas>,
    /// What the preview shows (sRGB, copied from `program` each frame).
    preview: Option<(wgpu::Texture, ProgramTexture)>,
    nv12: Option<Nv12Targets>,
    readbacks: Vec<Readback>,
    frame: u64,
    generation: u64,
    /// Picture bytes sent to the GPU (statistics).
    pub uploaded_bytes: u64,
}

impl GpuCompositor {
    pub fn new(gpu: Gpu) -> Result<GpuCompositor, String> {
        let device = &gpu.device;
        device.push_error_scope(wgpu::ErrorFilter::Validation);
        let module = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("compositor"),
            source: wgpu::ShaderSource::Wgsl(SHADER.into()),
        });
        let uniform_bgl = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("draw uniforms"),
            entries: &[wgpu::BindGroupLayoutEntry {
                binding: 0,
                visibility: wgpu::ShaderStages::VERTEX_FRAGMENT,
                ty: wgpu::BindingType::Buffer {
                    ty: wgpu::BufferBindingType::Uniform,
                    has_dynamic_offset: true,
                    min_binding_size: wgpu::BufferSize::new(DRAW_BYTES),
                },
                count: None,
            }],
        });
        let tex_bgl = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("picture"),
            entries: &[wgpu::BindGroupLayoutEntry {
                binding: 0,
                visibility: wgpu::ShaderStages::FRAGMENT,
                ty: wgpu::BindingType::Texture {
                    sample_type: wgpu::TextureSampleType::Float { filterable: false },
                    view_dimension: wgpu::TextureViewDimension::D2,
                    multisampled: false,
                },
                count: None,
            }],
        });
        let layer_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("layer"),
            bind_group_layouts: &[&uniform_bgl, &tex_bgl],
            push_constant_ranges: &[],
        });
        let full_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("fullscreen"),
            bind_group_layouts: &[&tex_bgl],
            push_constant_ranges: &[],
        });
        let pipeline = |label: &str, layout: &wgpu::PipelineLayout, vs: &str, fs: &str, format: wgpu::TextureFormat, blend: Option<wgpu::BlendState>| {
            device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
                label: Some(label),
                layout: Some(layout),
                vertex: wgpu::VertexState { module: &module, entry_point: Some(vs), compilation_options: Default::default(), buffers: &[] },
                primitive: wgpu::PrimitiveState::default(),
                depth_stencil: None,
                multisample: wgpu::MultisampleState::default(),
                fragment: Some(wgpu::FragmentState {
                    module: &module,
                    entry_point: Some(fs),
                    compilation_options: Default::default(),
                    targets: &[Some(wgpu::ColorTargetState { format, blend, write_mask: wgpu::ColorWrites::ALL })],
                }),
                multiview: None,
                cache: None,
            })
        };
        // Straight alpha over an opaque canvas; the canvas alpha stays 1.
        let over = wgpu::BlendState {
            color: wgpu::BlendComponent {
                src_factor: wgpu::BlendFactor::SrcAlpha,
                dst_factor: wgpu::BlendFactor::OneMinusSrcAlpha,
                operation: wgpu::BlendOperation::Add,
            },
            alpha: wgpu::BlendComponent { src_factor: wgpu::BlendFactor::Zero, dst_factor: wgpu::BlendFactor::One, operation: wgpu::BlendOperation::Add },
        };
        let rgba = wgpu::TextureFormat::Rgba8Unorm;
        let layer_pipeline = pipeline("layer", &layer_layout, "vs_layer", "fs_layer", rgba, Some(over));
        let mip_pipeline = pipeline("mip", &full_layout, "vs_full", "fs_mip", rgba, None);
        let y_pipeline = pipeline("luma", &full_layout, "vs_full", "fs_y", wgpu::TextureFormat::R8Unorm, None);
        let uv_pipeline = pipeline("chroma", &full_layout, "vs_full", "fs_uv", wgpu::TextureFormat::Rg8Unorm, None);
        let stride = (device.limits().min_uniform_buffer_offset_alignment as u64).max(DRAW_BYTES);
        let (uniforms, uniform_bg) = uniform_buffer(device, &uniform_bgl, stride, 16);
        if let Some(e) = pollster::block_on(device.pop_error_scope()) {
            return Err(format!("GPU compositor: {e}"));
        }
        Ok(GpuCompositor {
            gpu,
            layer_pipeline,
            mip_pipeline,
            y_pipeline,
            uv_pipeline,
            uniform_bgl,
            tex_bgl,
            uniforms,
            uniform_bg,
            uniform_capacity: 16,
            stride,
            sources: HashMap::new(),
            program: None,
            fade: None,
            preview: None,
            nv12: None,
            readbacks: Vec::new(),
            frame: 0,
            generation: 0,
            uploaded_bytes: 0,
        })
    }

    pub fn gpu(&self) -> &Gpu {
        &self.gpu
    }

    /// The preview's view of the program (after the first frame).
    pub fn program_texture(&self) -> Option<ProgramTexture> {
        self.preview.as_ref().map(|(_, p)| p.clone())
    }

    /// Draw `now` (fading in from `from` at `mix` 0..1) on a `w × h` canvas; when `readback` is
    /// set, also start reading the frame back as NV12 for that tick.
    pub fn render(&mut self, now: &[GpuLayer], from: Option<(&[GpuLayer], f32)>, w: u32, h: u32, readback: Option<u64>) -> RenderOutput {
        self.frame += 1;
        let mut out = RenderOutput { nv12: Vec::new(), readback_dropped: false };
        self.collect_readbacks(&mut out.nv12);
        self.ensure_canvases(w, h, from.is_some());

        let device = self.gpu.device.clone();
        let mut enc = device.create_command_encoder(&wgpu::CommandEncoderDescriptor { label: Some("composite") });
        // Upload changed pictures and fill the mip levels the draws need.
        let mut draws: Vec<(Id, [f32; 12])> = Vec::new();
        let canvas = (w as f32, h as f32);
        let prepare = |this: &mut Self, layers: &[GpuLayer], enc: &mut wgpu::CommandEncoder| -> Vec<(Id, [f32; 12])> {
            let mut v = Vec::new();
            for l in layers {
                let Some((level, uniforms)) = draw_params(l.image, l.placement, canvas) else { continue };
                this.upload(l.source, l.image, level, enc);
                v.push((l.source, uniforms));
            }
            v
        };
        let from_draws = from.map(|(layers, mix)| (prepare(self, layers, &mut enc), mix));
        let now_draws = prepare(self, now, &mut enc);
        draws.extend(now_draws.iter().copied());
        if let Some((fd, _)) = &from_draws {
            draws.extend(fd.iter().copied());
        }
        // The fade draws the outgoing scene over the incoming one.
        let fade_draw = from_draws.as_ref().map(|(_, mix)| {
            let opacity = 1.0 - mix.clamp(0.0, 1.0);
            [0.0, 0.0, canvas.0, canvas.1, 0.0, 0.0, canvas.0, canvas.1, canvas.0, canvas.1, opacity, 0.0]
        });
        let total = draws.len() as u64 + fade_draw.is_some() as u64;
        self.ensure_uniforms(total.max(1));
        let mut bytes = vec![0u8; (self.stride * total.max(1)) as usize];
        let all = draws.iter().map(|d| d.1).chain(fade_draw);
        for (i, u) in all.enumerate() {
            let o = i * self.stride as usize;
            for (k, f) in u.iter().enumerate() {
                bytes[o + k * 4..o + k * 4 + 4].copy_from_slice(&f.to_le_bytes());
            }
        }
        self.gpu.queue.write_buffer(&self.uniforms, 0, &bytes);

        let program = self.program.as_ref().unwrap();
        let mut slot = 0u64;
        // Incoming (current) scene into the program canvas.
        self.draw_pass(&mut enc, &program.view, &now_draws, &mut slot);
        if let Some((fd, _)) = &from_draws {
            let fade = self.fade.as_ref().unwrap();
            self.draw_pass(&mut enc, &fade.view, fd, &mut slot);
            // Outgoing scene over it, faded.
            let mut pass = begin(&mut enc, &program.view, wgpu::LoadOp::Load);
            pass.set_pipeline(&self.layer_pipeline);
            pass.set_bind_group(0, &self.uniform_bg, &[(slot * self.stride) as u32]);
            pass.set_bind_group(1, &fade.bg, &[]);
            pass.draw(0..6, 0..1);
        }
        // Preview copy (sRGB view of the same bytes).
        let (ptex, _) = self.preview.as_ref().unwrap();
        enc.copy_texture_to_texture(
            program.tex.as_image_copy(),
            ptex.as_image_copy(),
            wgpu::Extent3d { width: w, height: h, depth_or_array_layers: 1 },
        );
        // NV12 for the encoders.
        let mut started: Option<usize> = None;
        if let Some(tick) = readback {
            match self.readbacks.iter().position(|r| r.state.load(Ordering::Acquire) == FREE) {
                Some(i) => {
                    self.convert_nv12(&mut enc, i, tick);
                    started = Some(i);
                }
                None => out.readback_dropped = true,
            }
        }
        self.gpu.queue.submit([enc.finish()]);
        if let Some(i) = started {
            let r = &self.readbacks[i];
            r.state.store(PENDING, Ordering::Release);
            let state = Arc::clone(&r.state);
            r.buf.slice(..).map_async(wgpu::MapMode::Read, move |res| {
                state.store(if res.is_ok() { READY } else { FAILED }, Ordering::Release);
            });
        }
        // Free textures of sources that left the scenes.
        let frame = self.frame;
        self.sources.retain(|_, t| frame - t.last_used < TEXTURE_IDLE_FRAMES);
        out
    }

    fn draw_pass(&self, enc: &mut wgpu::CommandEncoder, target: &wgpu::TextureView, draws: &[(Id, [f32; 12])], slot: &mut u64) {
        let mut pass = begin(enc, target, wgpu::LoadOp::Clear(wgpu::Color::BLACK));
        pass.set_pipeline(&self.layer_pipeline);
        for (id, _) in draws {
            let Some(t) = self.sources.get(id) else {
                *slot += 1;
                continue;
            };
            pass.set_bind_group(0, &self.uniform_bg, &[(*slot * self.stride) as u32]);
            pass.set_bind_group(1, &t.draw_bg, &[]);
            pass.draw(0..6, 0..1);
            *slot += 1;
        }
    }

    /// Make the GPU copy of a source current, with `level` mips filled.
    fn upload(&mut self, id: Id, img: &SharedImage, level: u32, enc: &mut wgpu::CommandEncoder) {
        let device = &self.gpu.device;
        let (w, h) = (img.width, img.height);
        let fresh = !matches!(self.sources.get(&id), Some(t) if t.width == w && t.height == h);
        if fresh {
            let levels = 32 - w.max(h).leading_zeros();
            let tex = device.create_texture(&wgpu::TextureDescriptor {
                label: Some("source"),
                size: wgpu::Extent3d { width: w, height: h, depth_or_array_layers: 1 },
                mip_level_count: levels,
                sample_count: 1,
                dimension: wgpu::TextureDimension::D2,
                format: wgpu::TextureFormat::Rgba8Unorm,
                usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST | wgpu::TextureUsages::RENDER_ATTACHMENT,
                view_formats: &[],
            });
            let full = tex.create_view(&wgpu::TextureViewDescriptor::default());
            let level_views: Vec<wgpu::TextureView> = (0..levels)
                .map(|l| tex.create_view(&wgpu::TextureViewDescriptor { base_mip_level: l, mip_level_count: Some(1), ..Default::default() }))
                .collect();
            let level_bgs = level_views.iter().map(|v| texture_bg(device, &self.tex_bgl, v)).collect();
            let draw_bg = texture_bg(device, &self.tex_bgl, &full);
            self.sources.insert(
                id,
                SourceTexture { tex, draw_bg, level_views, level_bgs, width: w, height: h, ready_levels: 0, uploaded: Weak::new(), uploaded_seq: None, last_used: 0 },
            );
        }
        let t = self.sources.get_mut(&id).unwrap();
        t.last_used = self.frame;
        if !std::ptr::eq(t.uploaded.as_ptr(), Arc::as_ptr(img)) {
            // Only the redrawn part when the texture holds the source's previous picture.
            let [x, y, rw, rh] = match img.change {
                Some(c) if t.uploaded_seq == Some(c.prev) && t.ready_levels > 0 => c.rect,
                _ => [0, 0, w, h],
            };
            let (rw, rh) = (rw.min(w.saturating_sub(x)), rh.min(h.saturating_sub(y)));
            if rw > 0 && rh > 0 {
                self.gpu.queue.write_texture(
                    wgpu::TexelCopyTextureInfo { texture: &t.tex, mip_level: 0, origin: wgpu::Origin3d { x, y, z: 0 }, aspect: wgpu::TextureAspect::All },
                    &img.data,
                    wgpu::TexelCopyBufferLayout { offset: (y as u64 * w as u64 + x as u64) * 4, bytes_per_row: Some(4 * w), rows_per_image: Some(rh) },
                    wgpu::Extent3d { width: rw, height: rh, depth_or_array_layers: 1 },
                );
            }
            t.uploaded_seq = img.change.map(|c| c.seq);
            self.uploaded_bytes += rw as u64 * rh as u64 * 4;
            t.uploaded = Arc::downgrade(img);
            t.ready_levels = 1;
        }
        let want = (level + 1).min(t.level_views.len() as u32);
        while t.ready_levels < want {
            let l = t.ready_levels as usize;
            let mut pass = begin(enc, &t.level_views[l], wgpu::LoadOp::Clear(wgpu::Color::TRANSPARENT));
            pass.set_pipeline(&self.mip_pipeline);
            pass.set_bind_group(0, &t.level_bgs[l - 1], &[]);
            pass.draw(0..3, 0..1);
            drop(pass);
            t.ready_levels += 1;
        }
    }

    fn ensure_canvases(&mut self, w: u32, h: u32, fading: bool) {
        let device = &self.gpu.device;
        if !matches!(&self.program, Some(c) if c.width == w && c.height == h) {
            self.program = Some(canvas(device, &self.tex_bgl, w, h, "program"));
            let tex = device.create_texture(&wgpu::TextureDescriptor {
                label: Some("preview"),
                size: wgpu::Extent3d { width: w, height: h, depth_or_array_layers: 1 },
                mip_level_count: 1,
                sample_count: 1,
                dimension: wgpu::TextureDimension::D2,
                format: wgpu::TextureFormat::Rgba8UnormSrgb,
                usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
                view_formats: &[],
            });
            let view = tex.create_view(&wgpu::TextureViewDescriptor::default());
            self.generation += 1;
            self.preview = Some((tex, ProgramTexture { view, generation: self.generation }));
            self.fade = None;
            self.nv12 = None;
        }
        if fading && self.fade.is_none() {
            self.fade = Some(canvas(device, &self.tex_bgl, w, h, "fade"));
        }
        let (ew, eh) = (w & !1, h & !1);
        if !matches!(&self.nv12, Some(n) if n.width == ew && n.height == eh) && ew > 0 && eh > 0 {
            let plane = |label, format, w, h| {
                let tex = device.create_texture(&wgpu::TextureDescriptor {
                    label: Some(label),
                    size: wgpu::Extent3d { width: w, height: h, depth_or_array_layers: 1 },
                    mip_level_count: 1,
                    sample_count: 1,
                    dimension: wgpu::TextureDimension::D2,
                    format,
                    usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_SRC,
                    view_formats: &[],
                });
                let view = tex.create_view(&wgpu::TextureViewDescriptor::default());
                (tex, view)
            };
            let (y_tex, y_view) = plane("luma", wgpu::TextureFormat::R8Unorm, ew, eh);
            let (uv_tex, uv_view) = plane("chroma", wgpu::TextureFormat::Rg8Unorm, ew / 2, eh / 2);
            let align = wgpu::COPY_BYTES_PER_ROW_ALIGNMENT;
            let (y_pitch, uv_pitch) = (ew.div_ceil(align) * align, ew.div_ceil(align) * align);
            self.nv12 = Some(Nv12Targets { y_view, uv_view, y_tex, uv_tex, width: ew, height: eh, y_pitch, uv_pitch });
            // In-flight read-backs of the old size finish (and are dropped as stale).
            self.readbacks.retain(|r| r.state.load(Ordering::Acquire) != FREE);
            let size = y_pitch as u64 * eh as u64 + uv_pitch as u64 * (eh / 2) as u64;
            while self.readbacks.len() < READBACK_SLOTS {
                self.readbacks.push(Readback {
                    buf: device.create_buffer(&wgpu::BufferDescriptor {
                        label: Some("nv12 read-back"),
                        size,
                        usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
                        mapped_at_creation: false,
                    }),
                    state: Arc::new(AtomicU8::new(FREE)),
                    tick: 0,
                    width: ew,
                    height: eh,
                    y_pitch,
                    uv_pitch,
                });
            }
        }
    }

    fn ensure_uniforms(&mut self, n: u64) {
        if n > self.uniform_capacity {
            let cap = n.next_power_of_two();
            let (b, bg) = uniform_buffer(&self.gpu.device, &self.uniform_bgl, self.stride, cap);
            self.uniforms = b;
            self.uniform_bg = bg;
            self.uniform_capacity = cap;
        }
    }

    fn convert_nv12(&mut self, enc: &mut wgpu::CommandEncoder, slot: usize, tick: u64) {
        let (Some(n), Some(program)) = (&self.nv12, &self.program) else { return };
        for (view, pipeline) in [(&n.y_view, &self.y_pipeline), (&n.uv_view, &self.uv_pipeline)] {
            let mut pass = begin(enc, view, wgpu::LoadOp::Clear(wgpu::Color::BLACK));
            pass.set_pipeline(pipeline);
            pass.set_bind_group(0, &program.bg, &[]);
            pass.draw(0..3, 0..1);
        }
        let r = &mut self.readbacks[slot];
        if r.width != n.width || r.height != n.height {
            return;
        }
        r.tick = tick;
        enc.copy_texture_to_buffer(
            n.y_tex.as_image_copy(),
            wgpu::TexelCopyBufferInfo {
                buffer: &r.buf,
                layout: wgpu::TexelCopyBufferLayout { offset: 0, bytes_per_row: Some(n.y_pitch), rows_per_image: Some(n.height) },
            },
            wgpu::Extent3d { width: n.width, height: n.height, depth_or_array_layers: 1 },
        );
        enc.copy_texture_to_buffer(
            n.uv_tex.as_image_copy(),
            wgpu::TexelCopyBufferInfo {
                buffer: &r.buf,
                layout: wgpu::TexelCopyBufferLayout {
                    offset: n.y_pitch as u64 * n.height as u64,
                    bytes_per_row: Some(n.uv_pitch),
                    rows_per_image: Some(n.height / 2),
                },
            },
            wgpu::Extent3d { width: n.width / 2, height: n.height / 2, depth_or_array_layers: 1 },
        );
    }

    fn collect_readbacks(&mut self, out: &mut Vec<Nv12Frame>) {
        if self.readbacks.iter().all(|r| r.state.load(Ordering::Acquire) != PENDING) && !self.readbacks.iter().any(|r| r.state.load(Ordering::Acquire) == READY) {
            return;
        }
        let _ = self.gpu.device.poll(wgpu::Maintain::Poll);
        let mut ready: Vec<usize> = (0..self.readbacks.len()).filter(|&i| matches!(self.readbacks[i].state.load(Ordering::Acquire), READY | FAILED)).collect();
        ready.sort_by_key(|&i| self.readbacks[i].tick);
        for i in ready {
            let r = &self.readbacks[i];
            if r.state.load(Ordering::Acquire) == READY {
                let current = self.nv12.as_ref().is_some_and(|n| n.width == r.width && n.height == r.height);
                if current {
                    let (w, h) = (r.width as usize, r.height as usize);
                    let mut data = Vec::with_capacity(w * h * 3 / 2);
                    {
                        let mapped = r.buf.slice(..).get_mapped_range();
                        for y in 0..h {
                            let o = y * r.y_pitch as usize;
                            data.extend_from_slice(&mapped[o..o + w]);
                        }
                        let base = r.y_pitch as usize * h;
                        for y in 0..h / 2 {
                            let o = base + y * r.uv_pitch as usize;
                            data.extend_from_slice(&mapped[o..o + w]);
                        }
                    }
                    out.push(Nv12Frame { tick: r.tick, width: r.width, height: r.height, data });
                }
                r.buf.unmap();
            }
            r.state.store(FREE, Ordering::Release);
        }
        let n = &self.nv12;
        // Buffers of an old canvas size go once they're idle.
        self.readbacks.retain(|r| n.as_ref().is_some_and(|n| n.width == r.width && n.height == r.height) || r.state.load(Ordering::Acquire) != FREE);
    }
}

/// The mip level and uniforms (`Draw`) for one layer, or `None` when nothing would show.
fn draw_params(img: &RgbaImage, p: Placement, canvas: (f32, f32)) -> Option<(u32, [f32; 12])> {
    if p.w < 0.5 || p.h < 0.5 || p.opacity == 0 || img.width == 0 || img.height == 0 {
        return None;
    }
    let crop_w = img.width.saturating_sub(p.crop[0] + p.crop[2]);
    let crop_h = img.height.saturating_sub(p.crop[1] + p.crop[3]);
    if crop_w == 0 || crop_h == 0 {
        return None;
    }
    // Off the canvas entirely.
    if p.x >= canvas.0 || p.y >= canvas.1 || p.x + p.w <= 0.0 || p.y + p.h <= 0.0 {
        return None;
    }
    // Same rule as the CPU blitter: halve while the picture is shrunk to less than half.
    let mut level = 0u32;
    while (crop_w >> (level + 1)) as f32 >= p.w && (crop_h >> (level + 1)) as f32 >= p.h && level < 6 {
        level += 1;
    }
    let f = 1u32 << level;
    let (lw, lh) = ((img.width >> level).max(1), (img.height >> level).max(1));
    let (cl, ct) = (p.crop[0] / f, p.crop[1] / f);
    let cw = lw.saturating_sub(cl + p.crop[2] / f).max(1);
    let ch = lh.saturating_sub(ct + p.crop[3] / f).max(1);
    let opacity = p.opacity.min(255) as f32 / 255.0;
    Some((level, [p.x, p.y, p.x + p.w, p.y + p.h, cl as f32, ct as f32, cw as f32, ch as f32, canvas.0, canvas.1, opacity, level as f32]))
}

fn begin<'e>(enc: &'e mut wgpu::CommandEncoder, view: &wgpu::TextureView, load: wgpu::LoadOp<wgpu::Color>) -> wgpu::RenderPass<'e> {
    enc.begin_render_pass(&wgpu::RenderPassDescriptor {
        label: None,
        color_attachments: &[Some(wgpu::RenderPassColorAttachment {
            view,
            resolve_target: None,
            ops: wgpu::Operations { load, store: wgpu::StoreOp::Store },
        })],
        depth_stencil_attachment: None,
        timestamp_writes: None,
        occlusion_query_set: None,
    })
}

fn texture_bg(device: &wgpu::Device, layout: &wgpu::BindGroupLayout, view: &wgpu::TextureView) -> wgpu::BindGroup {
    device.create_bind_group(&wgpu::BindGroupDescriptor {
        label: None,
        layout,
        entries: &[wgpu::BindGroupEntry { binding: 0, resource: wgpu::BindingResource::TextureView(view) }],
    })
}

fn uniform_buffer(device: &wgpu::Device, layout: &wgpu::BindGroupLayout, stride: u64, n: u64) -> (wgpu::Buffer, wgpu::BindGroup) {
    let buf = device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("draws"),
        size: stride * n,
        usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
        mapped_at_creation: false,
    });
    let bg = device.create_bind_group(&wgpu::BindGroupDescriptor {
        label: None,
        layout,
        entries: &[wgpu::BindGroupEntry {
            binding: 0,
            resource: wgpu::BindingResource::Buffer(wgpu::BufferBinding { buffer: &buf, offset: 0, size: wgpu::BufferSize::new(DRAW_BYTES) }),
        }],
    });
    (buf, bg)
}

fn canvas(device: &wgpu::Device, layout: &wgpu::BindGroupLayout, w: u32, h: u32, label: &str) -> Canvas {
    let tex = device.create_texture(&wgpu::TextureDescriptor {
        label: Some(label),
        size: wgpu::Extent3d { width: w, height: h, depth_or_array_layers: 1 },
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format: wgpu::TextureFormat::Rgba8Unorm,
        usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_SRC,
        view_formats: &[],
    });
    let view = tex.create_view(&wgpu::TextureViewDescriptor::default());
    let bg = texture_bg(device, layout, &view);
    Canvas { tex, view, bg, width: w, height: h }
}

/// Read the program canvas back as RGBA (tests).
#[cfg(test)]
pub fn read_program(c: &GpuCompositor) -> Option<RgbaImage> {
    let p = c.program.as_ref()?;
    let device = &c.gpu.device;
    let pitch = (p.width * 4).div_ceil(wgpu::COPY_BYTES_PER_ROW_ALIGNMENT) * wgpu::COPY_BYTES_PER_ROW_ALIGNMENT;
    let buf = device.create_buffer(&wgpu::BufferDescriptor {
        label: None,
        size: pitch as u64 * p.height as u64,
        usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
        mapped_at_creation: false,
    });
    let mut enc = device.create_command_encoder(&Default::default());
    enc.copy_texture_to_buffer(
        p.tex.as_image_copy(),
        wgpu::TexelCopyBufferInfo { buffer: &buf, layout: wgpu::TexelCopyBufferLayout { offset: 0, bytes_per_row: Some(pitch), rows_per_image: Some(p.height) } },
        wgpu::Extent3d { width: p.width, height: p.height, depth_or_array_layers: 1 },
    );
    c.gpu.queue.submit([enc.finish()]);
    buf.slice(..).map_async(wgpu::MapMode::Read, |_| {});
    let _ = device.poll(wgpu::Maintain::Wait);
    let mapped = buf.slice(..).get_mapped_range();
    let mut img = RgbaImage::new(p.width, p.height);
    for y in 0..p.height as usize {
        let w4 = p.width as usize * 4;
        img.data[y * w4..(y + 1) * w4].copy_from_slice(&mapped[y * pitch as usize..y * pitch as usize + w4]);
    }
    Some(img)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::video;

    fn gradient(w: u32, h: u32, alpha: bool) -> RgbaImage {
        let mut img = RgbaImage::new(w, h);
        for y in 0..h {
            for x in 0..w {
                let i = ((y * w + x) * 4) as usize;
                img.data[i] = (x * 255 / w.max(1)) as u8;
                img.data[i + 1] = (y * 255 / h.max(1)) as u8;
                img.data[i + 2] = ((x + y) % 256) as u8;
                img.data[i + 3] = if alpha { ((x * 7 + y * 3) % 256) as u8 } else { 255 };
            }
        }
        img
    }

    /// Same scene through both compositors: every pixel within a couple of levels.
    fn compare(backends: wgpu::Backends) {
        let Some(gpu) = Gpu::headless(backends) else {
            eprintln!("no {backends:?} adapter; skipped");
            return;
        };
        eprintln!("GPU: {}", gpu.name);
        let mut c = GpuCompositor::new(gpu).expect("compositor");
        let (w, h) = (320u32, 180u32);
        let a: SharedImage = Arc::new(gradient(320, 180, false));
        let b: SharedImage = Arc::new(gradient(800, 600, true));
        let t: SharedImage = Arc::new(gradient(64, 64, true));
        let layers = [
            (1, &a, Placement { x: 0.0, y: 0.0, w: 320.0, h: 180.0, crop: [0; 4], opacity: 255 }),
            // Big downscale (mips), cropped, translucent.
            (2, &b, Placement { x: 10.5, y: 20.25, w: 150.0, h: 100.0, crop: [30, 10, 20, 40], opacity: 200 }),
            // Upscale.
            (3, &t, Placement { x: 180.0, y: 60.0, w: 128.0, h: 96.0, crop: [0; 4], opacity: 255 }),
        ];
        let gl: Vec<GpuLayer> = layers.iter().map(|(id, img, p)| GpuLayer { source: *id, image: img, placement: *p }).collect();
        let mut cpu = RgbaImage::filled(w, h, [0, 0, 0, 255]);
        for (_, img, p) in &layers {
            video::draw(&mut cpu.data, w as usize, h as usize, img, *p);
        }
        let mut frames = Vec::new();
        for tick in 0..6 {
            let o = c.render(&gl, None, w, h, Some(tick));
            frames.extend(o.nv12);
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
        let gpu_img = read_program(&c).expect("program");
        let mut worst = 0i32;
        let mut over = 0usize;
        for (i, (g, c)) in gpu_img.data.iter().zip(&cpu.data).enumerate() {
            let d = (*g as i32 - *c as i32).abs();
            if d > worst {
                worst = d;
                if d > 6 {
                    let px = i / 4;
                    eprintln!("pixel {},{} channel {}: gpu {g} cpu {c}", px % w as usize, px / w as usize, i % 4);
                }
            }
            if d > 6 {
                over += 1;
            }
        }
        eprintln!("worst difference {worst}, {over} values over 6");
        assert!(over < (w * h) as usize / 500, "{over} values differ by more than 6 (worst {worst})");
        // Read-back arrives, in order, matching the CPU NV12 conversion.
        let _ = c.gpu.device.poll(wgpu::Maintain::Wait);
        let o = c.render(&gl, None, w, h, None);
        frames.extend(o.nv12);
        assert!(frames.len() >= 3, "only {} NV12 frames came back", frames.len());
        assert!(frames.windows(2).all(|p| p[0].tick < p[1].tick));
        let mut nv12 = Vec::new();
        mkff_hwenc_nv12(&cpu.data, w as usize, h as usize, &mut nv12);
        let f = frames.last().unwrap();
        let worst = f.data.iter().zip(&nv12).map(|(a, b)| (*a as i32 - *b as i32).abs()).max().unwrap();
        assert!(worst <= 8, "NV12 differs by {worst}");
    }

    /// The reference NV12 conversion (same formula as mkff-hwenc's).
    fn mkff_hwenc_nv12(rgba: &[u8], w: usize, h: usize, out: &mut Vec<u8>) {
        out.resize(w * h * 3 / 2, 0);
        let px = |x: usize, y: usize| {
            let i = (y * w + x) * 4;
            (rgba[i] as f32 / 255.0, rgba[i + 1] as f32 / 255.0, rgba[i + 2] as f32 / 255.0)
        };
        for y in 0..h {
            for x in 0..w {
                let (r, g, b) = px(x, y);
                out[y * w + x] = (16.0 + (0.2126 * r + 0.7152 * g + 0.0722 * b) * 219.0).round() as u8;
            }
        }
        for cy in 0..h / 2 {
            for cx in 0..w / 2 {
                let mut s = (0.0, 0.0, 0.0);
                for (dx, dy) in [(0, 0), (1, 0), (0, 1), (1, 1)] {
                    let (r, g, b) = px(cx * 2 + dx, cy * 2 + dy);
                    s = (s.0 + r / 4.0, s.1 + g / 4.0, s.2 + b / 4.0);
                }
                let yy = 0.2126 * s.0 + 0.7152 * s.1 + 0.0722 * s.2;
                let o = w * h + cy * w + cx * 2;
                out[o] = (128.0 + (s.2 - yy) / 1.8556 * 224.0).round() as u8;
                out[o + 1] = (128.0 + (s.0 - yy) / 1.5748 * 224.0).round() as u8;
            }
        }
    }

    #[test]
    fn gpu_matches_cpu_primary() {
        compare(wgpu::Backends::PRIMARY);
    }

    #[test]
    fn gpu_matches_cpu_dx12() {
        compare(wgpu::Backends::DX12);
    }

    #[test]
    fn gpu_matches_cpu_gl() {
        compare(wgpu::Backends::GL);
    }
}
