//! The studio engine: source runtimes, the video compositor thread (renders the program scene
//! at the canvas frame rate), the audio mixer thread (10 ms blocks at 48 kHz), and the outputs.

use std::collections::{HashMap, HashSet};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use parking_lot::{Mutex, RwLock};

use crate::audio::{self, Level, BLOCK, CHANNELS, SAMPLE_RATE};
use crate::gpu_compositor::{Gpu, GpuCompositor, GpuLayer, ProgramTexture};
use crate::model::{Id, MonitorMode, SceneCollection, Settings, TransitionKind};
use crate::monitor::Monitor;
use crate::output::{OutputManager, OutputTaps, VideoFrame};
use crate::sources::SourceRuntime;
use crate::video::{self, Placement, RgbaImage, SharedImage};

/// A scene switch in progress.
#[derive(Clone, Copy, Debug)]
pub struct Transition {
    pub from: Id,
    pub start: Instant,
    pub duration: Duration,
}

#[derive(Default)]
pub struct Stats {
    pub render_ms: Level,
    /// Measured compositor rate.
    pub fps: Level,
    pub rendered: AtomicU64,
    /// Ticks the compositor missed (machine too busy).
    pub lagged: AtomicU64,
    pub mixer_underruns: AtomicU64,
}

pub struct Shared {
    pub epoch: Instant,
    pub collection: Mutex<SceneCollection>,
    pub settings: Mutex<Settings>,
    pub sources: RwLock<HashMap<Id, Arc<SourceRuntime>>>,
    /// The latest program picture (CPU compositor) and a counter bumped with each frame.
    pub program: Mutex<Option<SharedImage>>,
    /// The program on the GPU (GPU compositor): the preview draws it directly.
    pub program_gpu: Mutex<Option<ProgramTexture>>,
    pub program_seq: AtomicU64,
    /// "GPU · <adapter>" or "CPU".
    pub compositor_name: Mutex<String>,
    pub transition: Mutex<Option<Transition>>,
    pub master_level: [Level; 2],
    /// Monitored sources, played on the monitoring device.
    pub monitor: Arc<Monitor>,
    pub stats: Stats,
    pub taps: Arc<OutputTaps>,
    pub repaint: Mutex<Option<eframe::egui::Context>>,
    /// The UI redraws on every vsync by itself (vsync on): no wake-ups per frame needed, and
    /// they would bunch UI frames against vsync, halving the preview rate.
    pub ui_paced: AtomicBool,
    pub quit: AtomicBool,
}

pub struct Engine {
    pub shared: Arc<Shared>,
    pub outputs: OutputManager,
    threads: Vec<JoinHandle<()>>,
}

impl Engine {
    /// Start the engine. With a `gpu` (normally the window's device) scenes are composited on the
    /// GPU; without one, or if that fails, on the CPU.
    pub fn start(collection: SceneCollection, settings: Settings, gpu: Option<Gpu>) -> Engine {
        let taps = Arc::new(OutputTaps::default());
        let shared = Arc::new(Shared {
            epoch: Instant::now(),
            collection: Mutex::new(collection.clone()),
            settings: Mutex::new(settings),
            sources: RwLock::new(HashMap::new()),
            program: Mutex::new(None),
            program_gpu: Mutex::new(None),
            program_seq: AtomicU64::new(0),
            compositor_name: Mutex::new(String::new()),
            transition: Mutex::new(None),
            master_level: [Level::default(), Level::default()],
            monitor: Monitor::new(),
            stats: Stats::default(),
            taps: Arc::clone(&taps),
            repaint: Mutex::new(None),
            ui_paced: AtomicBool::new(false),
            quit: AtomicBool::new(false),
        });
        shared.monitor.set_device(shared.settings.lock().audio.monitor_device.clone());
        let mut threads = Vec::new();
        let s = Arc::clone(&shared);
        threads.push(std::thread::Builder::new().name("compositor".into()).spawn(move || compositor(s, gpu)).unwrap());
        let s = Arc::clone(&shared);
        threads.push(std::thread::Builder::new().name("mixer".into()).spawn(move || mixer(s)).unwrap());
        let s = Arc::clone(&shared);
        threads.push(std::thread::Builder::new().name("monitor".into()).spawn(move || Arc::clone(&s.monitor).run(&s.quit)).unwrap());
        let mut e = Engine { shared, outputs: OutputManager::new(taps), threads };
        e.sync_sources(&collection);
        e
    }

    /// Start, restart or drop source runtimes to match `col`.
    pub fn sync_sources(&mut self, col: &SceneCollection) {
        let mut map = self.shared.sources.write();
        let wanted: HashSet<Id> = col.sources.iter().map(|s| s.id).collect();
        map.retain(|id, _| wanted.contains(id));
        for s in &col.sources {
            let restart = map.get(&s.id).is_none_or(|r| r.kind != s.kind);
            if restart {
                map.insert(s.id, Arc::new(SourceRuntime::start(&s.kind)));
            }
        }
    }

    /// Restart one source (e.g. a "Reload" button), keeping its settings.
    pub fn restart_source(&mut self, id: Id) {
        let mut map = self.shared.sources.write();
        if let Some(r) = map.get(&id) {
            let kind = r.kind.clone();
            map.insert(id, Arc::new(SourceRuntime::start(&kind)));
        }
    }

    pub fn runtime(&self, id: Id) -> Option<Arc<SourceRuntime>> {
        self.shared.sources.read().get(&id).cloned()
    }

    /// Publish the UI's copy of the collection (and start / stop sources it changed).
    pub fn set_collection(&mut self, col: &SceneCollection) {
        let changed_sources = {
            let mut cur = self.shared.collection.lock();
            let changed = cur.sources != col.sources;
            if cur.current_scene != col.current_scene && col.transition == TransitionKind::Fade && col.transition_ms > 0 {
                *self.shared.transition.lock() = Some(Transition {
                    from: cur.current_scene,
                    start: Instant::now(),
                    duration: Duration::from_millis(col.transition_ms as u64),
                });
            } else if cur.current_scene != col.current_scene {
                *self.shared.transition.lock() = None;
            }
            *cur = col.clone();
            changed
        };
        if changed_sources {
            self.sync_sources(col);
        }
    }

    pub fn set_settings(&self, s: &Settings) {
        self.shared.monitor.set_device(s.audio.monitor_device.clone());
        *self.shared.settings.lock() = s.clone();
    }

    pub fn shutdown(&mut self) {
        self.outputs.stop_all();
        self.shared.quit.store(true, Ordering::Relaxed);
        for t in self.threads.drain(..) {
            let _ = t.join();
        }
        self.shared.sources.write().clear();
    }
}

// ---------------------------------------------------------------------------------------------
// Video

struct Layer {
    source: Id,
    image: SharedImage,
    placement: Placement,
}

/// The visible items of `scene`, bottom to top, with their current pictures.
fn scene_layers(col: &SceneCollection, scene: Id, sources: &HashMap<Id, Arc<SourceRuntime>>, active: &mut HashSet<Id>) -> Vec<Layer> {
    let Some(scene) = col.scene(scene) else { return Vec::new() };
    let mut out = Vec::new();
    for item in &scene.items {
        if !item.visible {
            continue;
        }
        active.insert(item.source);
        let Some(rt) = sources.get(&item.source) else { continue };
        let Some(image) = rt.shared.video.get() else { continue };
        let [w, h] = item.display_size(image.size());
        out.push(Layer {
            source: item.source,
            placement: Placement {
                x: item.pos[0],
                y: item.pos[1],
                w,
                h,
                crop: item.crop,
                opacity: (item.opacity.min(100) as u32 * 255 + 50) / 100,
            },
            image,
        });
    }
    out
}

fn render(layers: &[Layer], w: u32, h: u32) -> RgbaImage {
    let mut img = RgbaImage::filled(w, h, [0, 0, 0, 255]);
    for l in layers {
        video::draw(&mut img.data, w as usize, h as usize, &l.image, l.placement);
    }
    img
}

fn gpu_layers(layers: &[Layer]) -> Vec<GpuLayer<'_>> {
    layers.iter().map(|l| GpuLayer { source: l.source, image: &l.image, placement: l.placement }).collect()
}

fn compositor(s: Arc<Shared>, gpu: Option<Gpu>) {
    let mut gpu = match gpu {
        Some(_) if std::env::var_os("LB_CPU_COMPOSITOR").is_some() => None,
        Some(g) => match GpuCompositor::new(g) {
            Ok(c) => Some(c),
            Err(e) => {
                log::warn!("{e}; compositing on the CPU");
                None
            }
        },
        None => None,
    };
    let name = gpu.as_ref().map(|c| format!("GPU · {}", c.gpu().name)).unwrap_or_else(|| "CPU".into());
    log::info!("compositor: {name}");
    *s.compositor_name.lock() = name;
    let mut fps = s.settings.lock().video.fps.max(1);
    let mut tick = (s.epoch.elapsed().as_secs_f64() * fps as f64).ceil() as u64;
    let mut rate_window = (Instant::now(), 0u32);
    let perf = std::env::var_os("LB_PERF").is_some();
    let (mut worst_render, mut worst_late) = (0f32, 0f32);
    while !s.quit.load(Ordering::Relaxed) {
        let (w, h, new_fps) = {
            let st = s.settings.lock();
            (st.video.width.max(16), st.video.height.max(16), st.video.fps.max(1))
        };
        if new_fps != fps {
            fps = new_fps;
            tick = (s.epoch.elapsed().as_secs_f64() * fps as f64).ceil() as u64;
        }
        let deadline = s.epoch + Duration::from_secs_f64(tick as f64 / fps as f64);
        let now = Instant::now();
        if deadline > now {
            std::thread::sleep(deadline - now);
        } else if now - deadline > Duration::from_secs_f64(2.0 / fps as f64) {
            let current = (s.epoch.elapsed().as_secs_f64() * fps as f64).floor() as u64;
            s.stats.lagged.fetch_add(current.saturating_sub(tick), Ordering::Relaxed);
            tick = current;
        }

        let t0 = Instant::now();
        worst_late = worst_late.max(t0.saturating_duration_since(deadline).as_secs_f32() * 1000.0);
        let (layers_now, layers_from, mix) = {
            let col = s.collection.lock();
            let sources = s.sources.read();
            let mut active = HashSet::new();
            let now_layers = scene_layers(&col, col.current_scene, &sources, &mut active);
            let tr = *s.transition.lock();
            let (from_layers, mix) = match tr {
                Some(t) if t.start.elapsed() < t.duration && t.from != col.current_scene => {
                    let f = scene_layers(&col, t.from, &sources, &mut active);
                    (Some(f), t.start.elapsed().as_secs_f32() / t.duration.as_secs_f32())
                }
                _ => (None, 1.0),
            };
            for id in &col.global_audio {
                active.insert(*id);
            }
            for (id, rt) in sources.iter() {
                rt.shared.active.store(active.contains(id), Ordering::Relaxed);
            }
            (now_layers, from_layers, mix)
        };
        let tap = s.taps.video.lock().clone();
        let skip = || {
            // Encoder still busy with the previous frames.
            s.taps.skipped_frames.fetch_add(1, Ordering::Relaxed);
        };
        if let Some(gc) = gpu.as_mut() {
            let now_g = gpu_layers(&layers_now);
            let from_g = layers_from.as_deref().map(gpu_layers);
            let out = gc.render(&now_g, from_g.as_deref().map(|f| (f, smoothstep(mix))), w, h, tap.is_some().then_some(tick));
            if layers_from.is_none() && s.transition.lock().is_some_and(|t| t.start.elapsed() >= t.duration) {
                *s.transition.lock() = None;
            }
            let rms = t0.elapsed().as_secs_f32() * 1000.0;
            worst_render = worst_render.max(rms);
            s.stats.render_ms.set(rms);
            s.stats.rendered.fetch_add(1, Ordering::Relaxed);
            {
                let mut p = s.program_gpu.lock();
                if p.as_ref().map(|p| p.generation) != gc.program_texture().map(|t| t.generation) {
                    *p = gc.program_texture();
                }
            }
            s.program_seq.fetch_add(1, Ordering::Relaxed);
            if out.readback_dropped && tap.is_some() {
                skip();
            }
            for f in out.nv12 {
                if let Some(tx) = &tap {
                    if tx.try_send((f.tick, VideoFrame::Nv12(Arc::new(f)))).is_err() {
                        skip();
                    }
                }
            }
        } else {
            let mut frame = render(&layers_now, w, h);
            if let Some(from) = layers_from {
                let mut a = render(&from, w, h);
                video::crossfade(&mut a.data, &frame.data, smoothstep(mix));
                frame = a;
            } else if s.transition.lock().is_some_and(|t| t.start.elapsed() >= t.duration) {
                *s.transition.lock() = None;
            }
            let frame = Arc::new(frame);
            let rms = t0.elapsed().as_secs_f32() * 1000.0;
            worst_render = worst_render.max(rms);
            s.stats.render_ms.set(rms);
            s.stats.rendered.fetch_add(1, Ordering::Relaxed);
            *s.program.lock() = Some(Arc::clone(&frame));
            s.program_seq.fetch_add(1, Ordering::Relaxed);
            if let Some(tx) = &tap {
                if tx.try_send((tick, VideoFrame::Rgba(frame))).is_err() {
                    skip();
                }
            }
        }
        if !s.ui_paced.load(Ordering::Relaxed) {
            if let Some(ctx) = s.repaint.lock().as_ref() {
                ctx.request_repaint();
            }
        }
        rate_window.1 += 1;
        let el = rate_window.0.elapsed();
        if el >= Duration::from_secs(1) {
            s.stats.fps.set(rate_window.1 as f32 / el.as_secs_f32());
            if perf {
                let up = gpu.as_mut().map(|g| std::mem::take(&mut g.uploaded_bytes)).unwrap_or(0);
                log::info!(
                    "compositor: {:.1} fps, worst render {worst_render:.2} ms, worst start lateness {worst_late:.2} ms, {w}x{h}, uploads {:.1} MB/s",
                    rate_window.1 as f32 / el.as_secs_f32(),
                    up as f64 / 1e6 / el.as_secs_f64()
                );
            }
            worst_render = 0.0;
            worst_late = 0.0;
            rate_window = (Instant::now(), 0);
        }
        tick += 1;
    }
}

fn smoothstep(t: f32) -> f32 {
    let t = t.clamp(0.0, 1.0);
    t * t * (3.0 - 2.0 * t)
}

// ---------------------------------------------------------------------------------------------
// Audio

fn mixer(s: Arc<Shared>) {
    let mut produced = (s.epoch.elapsed().as_secs_f64() * SAMPLE_RATE as f64) as u64 / BLOCK as u64 * BLOCK as u64;
    let mut mix = vec![0f32; BLOCK * CHANNELS];
    let mut tmp = vec![0f32; BLOCK * CHANNELS];
    let mut mon = vec![0f32; BLOCK * CHANNELS];
    let mut peaks: HashMap<usize, f32> = HashMap::new();
    while !s.quit.load(Ordering::Relaxed) {
        let now = (s.epoch.elapsed().as_secs_f64() * SAMPLE_RATE as f64) as u64;
        if now > produced + SAMPLE_RATE as u64 {
            // Stalled for over a second (debugger, sleep): skip ahead rather than burst.
            produced = now / BLOCK as u64 * BLOCK as u64;
            s.stats.mixer_underruns.fetch_add(1, Ordering::Relaxed);
        }
        if produced + BLOCK as u64 > now {
            std::thread::sleep(Duration::from_millis(2));
            continue;
        }
        // Who is mixed, at what gain.
        let (inputs, idle, master) = {
            let col = s.collection.lock();
            let sources = s.sources.read();
            let mut inputs = Vec::new();
            let mut idle = Vec::new();
            for cfg in &col.sources {
                let Some(rt) = sources.get(&cfg.id) else { continue };
                let Some(tap) = rt.shared.audio.clone() else { continue };
                if rt.shared.is_active() {
                    inputs.push((tap, if cfg.muted { 0.0 } else { cfg.volume }, cfg.monitor_mode()));
                } else {
                    idle.push(tap);
                }
            }
            (inputs, idle, col.master_volume)
        };
        for tap in &idle {
            tap.clear();
            tap.level.set(0.0);
        }
        let monitoring = inputs.iter().any(|(_, _, m)| m.heard());
        while produced + BLOCK as u64 <= now {
            mix.fill(0.0);
            mon.fill(0.0);
            for (i, (tap, gain, mode)) in inputs.iter().enumerate() {
                tap.pull(&mut tmp);
                if *gain != 1.0 {
                    for v in tmp.iter_mut() {
                        *v *= gain;
                    }
                }
                let p = audio::peak(&tmp);
                let e = peaks.entry(i).or_insert(0.0);
                *e = e.max(p);
                if mode.in_output() {
                    for (m, v) in mix.iter_mut().zip(&tmp) {
                        *m += v;
                    }
                }
                if *mode != MonitorMode::Off {
                    for (m, v) in mon.iter_mut().zip(&tmp) {
                        *m += v;
                    }
                }
            }
            if monitoring {
                for v in mon.iter_mut() {
                    *v = v.clamp(-1.0, 1.0);
                }
            }
            s.monitor.push(monitoring.then_some(&mon[..]));
            let (mut pl, mut pr) = (0f32, 0f32);
            for f in mix.chunks_exact_mut(2) {
                f[0] = (f[0] * master).clamp(-1.0, 1.0);
                f[1] = (f[1] * master).clamp(-1.0, 1.0);
                pl = pl.max(f[0].abs());
                pr = pr.max(f[1].abs());
            }
            s.master_level[0].set(pl);
            s.master_level[1].set(pr);
            if let Some(tx) = s.taps.audio.lock().as_ref() {
                let _ = tx.send((produced, mix.clone()));
            }
            produced += BLOCK as u64;
        }
        for (i, (tap, _, _)) in inputs.iter().enumerate() {
            tap.level.set(peaks.remove(&i).unwrap_or(0.0));
        }
    }
}
