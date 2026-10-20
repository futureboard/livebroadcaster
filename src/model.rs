//! The studio's persistent state: sources, scenes, and settings (all serialised to JSON).

use serde::{Deserialize, Serialize};

pub type Id = u64;

/// Where a source gets its pictures and / or sound from.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum SourceKind {
    Color {
        color: [u8; 4],
        width: u32,
        height: u32,
    },
    Image {
        path: String,
    },
    Text(TextSettings),
    /// Any file MKFF decodes (video, audio or still).
    Media(MediaSettings),
    /// A whole monitor (DXGI desktop duplication).
    Display {
        /// Index into the monitor list (adapter order, then output order).
        monitor: usize,
        cursor: bool,
    },
    /// One window.
    Window {
        target: WindowTarget,
        cursor: bool,
        #[serde(default)]
        method: CaptureMethod,
    },
    /// A webcam / capture card (Media Foundation).
    Camera {
        /// Symbolic link of the device (stable across reboots).
        device: String,
        device_name: String,
        /// Requested capture format; `None` picks the largest up to 1080p.
        format: Option<CameraFormat>,
    },
    /// A microphone / line input. `device: None` follows the system default.
    AudioInput {
        device: Option<String>,
    },
    /// What a playback device plays (WASAPI loopback). `device: None` follows the default.
    AudioOutput {
        device: Option<String>,
    },
    /// A web page (Chromium Embedded Framework, off-screen), with its sound.
    Browser(BrowserSettings),
    /// What applications play, whatever device they use (WASAPI process loopback).
    AppAudio {
        mode: AppAudioMode,
        /// Executable name ("chrome.exe") for `Only` / `AllExcept`.
        exe: String,
        /// A window title of it, shown in the UI.
        title: String,
    },
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct BrowserSettings {
    /// A web address, or a local .html file.
    pub url: String,
    pub width: u32,
    pub height: u32,
    pub fps: u32,
    /// Style sheet added to every page (transparent background by default).
    pub css: String,
    /// Play the page's sound through the mixer.
    pub audio: bool,
    /// Pause rendering while the source isn't on air (saves CPU / GPU).
    pub pause_when_hidden: bool,
}

impl Default for BrowserSettings {
    fn default() -> Self {
        BrowserSettings {
            url: String::new(),
            width: 1280,
            height: 720,
            fps: 30,
            css: "body { background-color: rgba(0, 0, 0, 0); margin: 0px auto; overflow: hidden; }".into(),
            audio: true,
            pause_when_hidden: false,
        }
    }
}

/// Which applications' sound an [`SourceKind::AppAudio`] source captures.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum AppAudioMode {
    /// Every application except LiveBroadcaster itself.
    #[default]
    System,
    /// One application and its child processes.
    Only,
    /// Every application except one (and LiveBroadcaster's own sound).
    AllExcept,
}

impl SourceKind {
    pub fn type_name(&self) -> &'static str {
        match self {
            SourceKind::Color { .. } => tr!("Color Source"),
            SourceKind::Image { .. } => tr!("Image"),
            SourceKind::Text(_) => tr!("Text"),
            SourceKind::Media(_) => tr!("Media Source"),
            SourceKind::Display { .. } => tr!("Display Capture"),
            SourceKind::Window { .. } => tr!("Window Capture"),
            SourceKind::Camera { .. } => tr!("Video Capture Device"),
            SourceKind::AudioInput { .. } => tr!("Audio Input Capture"),
            SourceKind::AudioOutput { .. } => tr!("Audio Output Capture"),
            SourceKind::AppAudio { .. } => tr!("Application Audio Capture"),
            SourceKind::Browser(_) => tr!("Browser"),
        }
    }

    pub fn icon(&self) -> sphere_ui::Icon {
        use sphere_ui::Icon;
        match self {
            SourceKind::Color { .. } => Icon::Palette,
            SourceKind::Image { .. } => Icon::Image,
            SourceKind::Text(_) => Icon::Type,
            SourceKind::Media(_) => Icon::Film,
            SourceKind::Display { .. } => Icon::Monitor,
            SourceKind::Window { .. } => Icon::AppWindow,
            SourceKind::Camera { .. } => Icon::Camera,
            SourceKind::AudioInput { .. } => Icon::Mic,
            SourceKind::AudioOutput { .. } => Icon::Speaker,
            SourceKind::AppAudio { .. } => Icon::AudioLines,
            SourceKind::Browser(_) => Icon::Globe,
        }
    }

    pub fn has_video(&self) -> bool {
        !matches!(self, SourceKind::AudioInput { .. } | SourceKind::AudioOutput { .. } | SourceKind::AppAudio { .. })
    }

    pub fn has_audio(&self) -> bool {
        match self {
            SourceKind::Media(m) => m.audio,
            SourceKind::Browser(b) => b.audio,
            SourceKind::AudioInput { .. } | SourceKind::AudioOutput { .. } | SourceKind::AppAudio { .. } => true,
            _ => false,
        }
    }

    /// Every kind the "Add source" menu offers, with default settings.
    pub fn templates() -> Vec<SourceKind> {
        vec![
            SourceKind::Display { monitor: 0, cursor: true },
            SourceKind::Window { target: WindowTarget::default(), cursor: true, method: CaptureMethod::Auto },
            SourceKind::Camera { device: String::new(), device_name: String::new(), format: None },
            SourceKind::Media(MediaSettings::default()),
            SourceKind::Browser(BrowserSettings::default()),
            SourceKind::Image { path: String::new() },
            SourceKind::Text(TextSettings::default()),
            SourceKind::Color { color: [40, 44, 52, 255], width: 1920, height: 1080 },
            SourceKind::AudioInput { device: None },
            SourceKind::AudioOutput { device: None },
            SourceKind::AppAudio { mode: AppAudioMode::System, exe: String::new(), title: String::new() },
        ]
    }
}

/// How a window is captured.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum CaptureMethod {
    /// Windows Graphics Capture when available, else GDI.
    #[default]
    Auto,
    /// Windows Graphics Capture: what DWM composes, at the window's frame rate (fast; works for
    /// browsers and games).
    Wgc,
    /// GDI `PrintWindow`: makes the window repaint itself (slow; old Windows versions).
    Gdi,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct WindowTarget {
    pub title: String,
    pub class: String,
    pub exe: String,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct CameraFormat {
    pub width: u32,
    pub height: u32,
    /// Frame rate as `num / den`.
    pub fps_num: u32,
    pub fps_den: u32,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct MediaSettings {
    pub path: String,
    pub looping: bool,
    /// Start from the beginning whenever the source becomes visible on air.
    pub restart_on_activate: bool,
    /// Keep the last picture when playback ends (otherwise the source turns transparent).
    pub hold_last_frame: bool,
    /// Play the file's sound through the mixer.
    pub audio: bool,
    /// Playback speed in percent.
    pub speed_percent: u32,
}

impl Default for MediaSettings {
    fn default() -> Self {
        MediaSettings {
            path: String::new(),
            looping: true,
            restart_on_activate: true,
            hold_last_frame: true,
            audio: true,
            speed_percent: 100,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, Default)]
pub enum TextAlign {
    #[default]
    Left,
    Center,
    Right,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct TextSettings {
    pub text: String,
    /// Read the text from this file instead (re-read when it changes).
    pub from_file: Option<String>,
    /// A .ttf / .otf / .ttc file; empty uses the system UI font.
    pub font_path: String,
    pub size: f32,
    pub color: [u8; 4],
    pub align: TextAlign,
    pub outline: bool,
    pub outline_size: f32,
    pub outline_color: [u8; 4],
    pub background: [u8; 4],
    pub padding: u32,
    /// Wrap lines longer than this (pixels); 0 = no wrapping.
    pub wrap_width: u32,
}

impl Default for TextSettings {
    fn default() -> Self {
        TextSettings {
            text: "Hello, world! สวัสดี".into(),
            from_file: None,
            font_path: String::new(),
            size: 64.0,
            color: [255, 255, 255, 255],
            align: TextAlign::Left,
            outline: true,
            outline_size: 3.0,
            outline_color: [0, 0, 0, 255],
            background: [0, 0, 0, 0],
            padding: 8,
            wrap_width: 0,
        }
    }
}

/// A source: shared by every scene that shows it (as in OBS).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct SourceConfig {
    pub id: Id,
    pub name: String,
    pub kind: SourceKind,
    /// Linear gain (1.0 = 0 dB).
    pub volume: f32,
    pub muted: bool,
    /// Listening on the monitoring device; `None` = the kind's default ([`SourceConfig::monitor_mode`]).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub monitor: Option<MonitorMode>,
}

/// Whether a source is heard on the monitoring device (speakers / headphones) and / or goes to
/// the stream and recording.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum MonitorMode {
    /// Only in the output (stream / recording).
    Off,
    /// Only on the monitoring device, not in the output.
    MonitorOnly,
    /// Both.
    MonitorAndOutput,
}

impl MonitorMode {
    pub const ALL: [MonitorMode; 3] = [MonitorMode::Off, MonitorMode::MonitorAndOutput, MonitorMode::MonitorOnly];

    pub fn name(self) -> &'static str {
        match self {
            MonitorMode::Off => tr!("Output only"),
            MonitorMode::MonitorOnly => tr!("Monitor only"),
            MonitorMode::MonitorAndOutput => tr!("Monitor + output"),
        }
    }

    pub fn heard(self) -> bool {
        self != MonitorMode::Off
    }

    pub fn in_output(self) -> bool {
        self != MonitorMode::MonitorOnly
    }
}

impl SourceConfig {
    /// The monitoring choice: web pages are heard by default (their sound never reaches the
    /// speakers otherwise — alerts would be silent for the streamer); capture devices aren't
    /// (you already hear them, and monitoring them would echo).
    pub fn monitor_mode(&self) -> MonitorMode {
        self.monitor.unwrap_or(match self.kind {
            SourceKind::Browser(_) => MonitorMode::MonitorAndOutput,
            _ => MonitorMode::Off,
        })
    }
}

/// A source placed on a scene. Position and size are in canvas pixels.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct SceneItem {
    pub id: Id,
    pub source: Id,
    pub visible: bool,
    pub locked: bool,
    /// Top-left corner on the canvas.
    pub pos: [f32; 2],
    pub scale: [f32; 2],
    /// Pixels cut from the source: left, top, right, bottom.
    pub crop: [u32; 4],
    /// 0–100.
    #[serde(default = "full_opacity")]
    pub opacity: u8,
    /// Not placed yet: fitted to the canvas once the source reports its size.
    #[serde(default)]
    pub needs_layout: bool,
}

fn full_opacity() -> u8 {
    100
}

impl SceneItem {
    /// Size on the canvas for a source of `src` pixels.
    pub fn display_size(&self, src: (u32, u32)) -> [f32; 2] {
        let w = src.0.saturating_sub(self.crop[0] + self.crop[2]) as f32;
        let h = src.1.saturating_sub(self.crop[1] + self.crop[3]) as f32;
        [w * self.scale[0], h * self.scale[1]]
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Scene {
    pub id: Id,
    pub name: String,
    /// Bottom to top.
    pub items: Vec<SceneItem>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, Default)]
pub enum TransitionKind {
    Cut,
    #[default]
    Fade,
}

/// Everything the user builds: sources and the scenes that arrange them.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct SceneCollection {
    pub sources: Vec<SourceConfig>,
    pub scenes: Vec<Scene>,
    pub current_scene: Id,
    /// Audio sources mixed regardless of the scene (Desktop Audio, Mic/Aux).
    pub global_audio: Vec<Id>,
    pub transition: TransitionKind,
    pub transition_ms: u32,
    /// Master gain (linear).
    pub master_volume: f32,
    pub next_id: Id,
}

impl SceneCollection {
    pub fn new_default() -> Self {
        let mut c = SceneCollection {
            sources: Vec::new(),
            scenes: Vec::new(),
            current_scene: 0,
            global_audio: Vec::new(),
            transition: TransitionKind::Fade,
            transition_ms: 300,
            master_volume: 1.0,
            next_id: 1,
        };
        let scene = c.add_scene(tr!("Scene"));
        c.current_scene = scene;
        // Whatever the system plays (except the studio itself), on any device.
        #[cfg(windows)]
        let desk = c.add_source(tr!("Desktop Audio"), SourceKind::AppAudio { mode: AppAudioMode::System, exe: String::new(), title: String::new() });
        #[cfg(not(windows))]
        let desk = c.add_source(tr!("Desktop Audio"), SourceKind::AudioOutput { device: None });
        let mic = c.add_source(tr!("Mic/Aux"), SourceKind::AudioInput { device: None });
        c.global_audio = vec![desk, mic];
        c
    }

    pub fn alloc_id(&mut self) -> Id {
        let id = self.next_id;
        self.next_id += 1;
        id
    }

    pub fn add_scene(&mut self, name: &str) -> Id {
        let id = self.alloc_id();
        let name = self.unique_scene_name(name);
        self.scenes.push(Scene { id, name, items: Vec::new() });
        id
    }

    pub fn add_source(&mut self, name: &str, kind: SourceKind) -> Id {
        let id = self.alloc_id();
        let name = self.unique_source_name(name);
        self.sources.push(SourceConfig { id, name, kind, volume: 1.0, muted: false, monitor: None });
        id
    }

    /// Put `source` on top of `scene`; returns the new item's id.
    pub fn add_item(&mut self, scene: Id, source: Id) -> Option<Id> {
        let id = self.alloc_id();
        let s = self.scene_mut(scene)?;
        s.items.push(SceneItem {
            id,
            source,
            visible: true,
            locked: false,
            pos: [0.0, 0.0],
            scale: [1.0, 1.0],
            crop: [0; 4],
            opacity: 100,
            needs_layout: true,
        });
        Some(id)
    }

    pub fn unique_source_name(&self, base: &str) -> String {
        unique(base, |n| self.sources.iter().any(|s| s.name == n))
    }

    pub fn unique_scene_name(&self, base: &str) -> String {
        unique(base, |n| self.scenes.iter().any(|s| s.name == n))
    }

    pub fn source(&self, id: Id) -> Option<&SourceConfig> {
        self.sources.iter().find(|s| s.id == id)
    }

    pub fn source_mut(&mut self, id: Id) -> Option<&mut SourceConfig> {
        self.sources.iter_mut().find(|s| s.id == id)
    }

    pub fn scene(&self, id: Id) -> Option<&Scene> {
        self.scenes.iter().find(|s| s.id == id)
    }

    pub fn scene_mut(&mut self, id: Id) -> Option<&mut Scene> {
        self.scenes.iter_mut().find(|s| s.id == id)
    }

    /// Drop sources no scene shows any more (OBS removes a source with its last item).
    pub fn prune_orphans(&mut self) {
        let used: std::collections::HashSet<Id> = self
            .scenes
            .iter()
            .flat_map(|s| s.items.iter().map(|i| i.source))
            .chain(self.global_audio.iter().copied())
            .collect();
        self.sources.retain(|s| used.contains(&s.id));
    }

    /// Make the state consistent after loading a file someone may have edited.
    pub fn repair(&mut self) {
        if self.scenes.is_empty() {
            let id = self.add_scene(tr!("Scene"));
            self.current_scene = id;
        }
        if self.scene(self.current_scene).is_none() {
            self.current_scene = self.scenes[0].id;
        }
        let max = self
            .sources
            .iter()
            .map(|s| s.id)
            .chain(self.scenes.iter().map(|s| s.id))
            .chain(self.scenes.iter().flat_map(|s| s.items.iter().map(|i| i.id)))
            .max()
            .unwrap_or(0);
        self.next_id = self.next_id.max(max + 1);
        let ids: std::collections::HashSet<Id> = self.sources.iter().map(|s| s.id).collect();
        for scene in &mut self.scenes {
            scene.items.retain(|i| ids.contains(&i.source));
        }
        self.global_audio.retain(|g| ids.contains(g));
    }
}

fn unique(base: &str, taken: impl Fn(&str) -> bool) -> String {
    if !taken(base) {
        return base.to_string();
    }
    (2..)
        .map(|n| format!("{base} {n}"))
        .find(|n| !taken(n))
        .unwrap()
}

// ---------------------------------------------------------------------------------------------
// Settings

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, Default)]
pub enum StreamService {
    #[default]
    Custom,
    YouTube,
    Twitch,
    Facebook,
    Kick,
}

impl StreamService {
    pub const ALL: [StreamService; 5] = [
        StreamService::Custom,
        StreamService::YouTube,
        StreamService::Twitch,
        StreamService::Facebook,
        StreamService::Kick,
    ];

    pub fn name(self) -> &'static str {
        match self {
            StreamService::Custom => tr!("Custom"),
            StreamService::YouTube => "YouTube - RTMP",
            StreamService::Twitch => "Twitch",
            StreamService::Facebook => "Facebook Live",
            StreamService::Kick => "Kick",
        }
    }

    /// Ingest server of the service (`None` for a custom server).
    pub fn server(self) -> Option<&'static str> {
        match self {
            StreamService::Custom => None,
            StreamService::YouTube => Some("rtmp://a.rtmp.youtube.com/live2"),
            StreamService::Twitch => Some("rtmp://live.twitch.tv/app"),
            StreamService::Facebook => Some("rtmps://live-api-s.facebook.com:443/rtmp"),
            StreamService::Kick => Some("rtmps://fa723fc1b171.global-contribute.live-video.net/app"),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct StreamSettings {
    pub service: StreamService,
    /// rtmp:// or rtmps:// URL (custom service).
    pub server: String,
    pub key: String,
}

impl StreamSettings {
    pub fn server_url(&self) -> String {
        self.service.server().map(str::to_string).unwrap_or_else(|| self.server.clone())
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, Default)]
pub enum EncoderPreset {
    #[default]
    Speed,
    Balanced,
    Quality,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, Default)]
pub enum EncoderProfile {
    Baseline,
    #[default]
    Main,
    High,
}

/// Which H.264 encoder the outputs use.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, Default)]
pub enum EncoderChoice {
    /// A GPU encoder if there is one (NVIDIA, AMD, Intel), else software.
    #[default]
    Auto,
    Nvenc,
    Qsv,
    Amf,
    Software,
}

impl EncoderChoice {
    pub const ALL: [EncoderChoice; 5] = [EncoderChoice::Auto, EncoderChoice::Nvenc, EncoderChoice::Qsv, EncoderChoice::Amf, EncoderChoice::Software];

    pub fn name(self) -> &'static str {
        match self {
            EncoderChoice::Auto => tr!("Auto (GPU if available)"),
            EncoderChoice::Nvenc => "NVIDIA NVENC H.264",
            EncoderChoice::Qsv => "Intel Quick Sync H.264",
            EncoderChoice::Amf => "AMD AMF H.264",
            EncoderChoice::Software => tr!("Software (MKFF H.264)"),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, Default)]
pub enum RecordFormat {
    /// Fragmented MP4: still playable if the recording is cut short.
    #[default]
    FragmentedMp4,
    Mp4,
    Mov,
}

impl RecordFormat {
    pub fn name(self) -> &'static str {
        match self {
            RecordFormat::FragmentedMp4 => tr!("Fragmented MP4 (.mp4, crash-safe)"),
            RecordFormat::Mp4 => tr!("MP4 (.mp4)"),
            RecordFormat::Mov => tr!("QuickTime (.mov)"),
        }
    }

    pub fn extension(self) -> &'static str {
        match self {
            RecordFormat::Mov => "mov",
            _ => "mp4",
        }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct OutputSettings {
    #[serde(default)]
    pub encoder: EncoderChoice,
    pub video_kbps: u32,
    pub audio_kbps: u32,
    pub preset: EncoderPreset,
    pub profile: EncoderProfile,
    /// Seconds between keyframes (2 is what most services ask for).
    pub keyint_secs: u32,
    pub record_path: String,
    pub record_format: RecordFormat,
    /// Recording quality: constant rate factor (lower is better).
    pub record_crf: f32,
    /// Record with the stream's bitrate (shares one encoder) instead of CRF.
    pub record_same_as_stream: bool,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct VideoSettings {
    pub width: u32,
    pub height: u32,
    pub fps: u32,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, Default)]
pub enum Appearance {
    #[default]
    Dark,
    Light,
}

/// Graphics API for the preview / UI renderer (wgpu backend).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, Default)]
pub enum GpuBackend {
    #[default]
    Auto,
    Dx12,
    Vulkan,
    Gl,
}

impl GpuBackend {
    pub fn name(self) -> &'static str {
        match self {
            GpuBackend::Auto => tr!("Auto"),
            GpuBackend::Dx12 => "DirectX 12",
            GpuBackend::Vulkan => "Vulkan",
            GpuBackend::Gl => "OpenGL",
        }
    }
}

/// Which GPU draws the window and the preview (applied at start-up).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct PreviewSettings {
    /// wgpu adapter name; `None` picks the fastest GPU that can present to the window.
    pub adapter: Option<String>,
    pub backend: GpuBackend,
    /// Present on vertical blank (smooth, no tearing); off presents immediately.
    pub vsync: bool,
}

impl Default for PreviewSettings {
    fn default() -> Self {
        PreviewSettings { adapter: None, backend: GpuBackend::Auto, vsync: true }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Settings {
    #[serde(default)]
    pub preview: PreviewSettings,
    pub stream: StreamSettings,
    pub output: OutputSettings,
    pub video: VideoSettings,
    #[serde(default)]
    pub appearance: Appearance,
    #[serde(default)]
    pub audio: AudioSettings,
    /// Interface language; `None` follows the system's.
    #[serde(default)]
    pub language: Option<crate::i18n::Lang>,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct AudioSettings {
    /// Playback device that monitored sources are heard on; `None` follows the system default.
    pub monitor_device: Option<String>,
}

impl Default for Settings {
    fn default() -> Self {
        let record_path = directories::UserDirs::new()
            .and_then(|d| d.video_dir().map(|p| p.to_path_buf()))
            .or_else(|| directories::UserDirs::new().map(|d| d.home_dir().to_path_buf()))
            .map(|p| p.to_string_lossy().into_owned())
            .unwrap_or_else(|| ".".into());
        Settings {
            preview: PreviewSettings::default(),
            stream: StreamSettings { service: StreamService::YouTube, server: String::new(), key: String::new() },
            output: OutputSettings {
                encoder: EncoderChoice::Auto,
                video_kbps: 2500,
                audio_kbps: 160,
                preset: EncoderPreset::Speed,
                profile: EncoderProfile::Main,
                keyint_secs: 2,
                record_path,
                record_format: RecordFormat::FragmentedMp4,
                record_crf: 23.0,
                record_same_as_stream: true,
            },
            video: VideoSettings { width: 1280, height: 720, fps: 30 },
            appearance: Appearance::Dark,
            audio: AudioSettings::default(),
            language: None,
        }
    }
}

/// Canvas resolutions offered in the settings.
pub const RESOLUTIONS: [(u32, u32); 6] = [(3840, 2160), (2560, 1440), (1920, 1080), (1600, 900), (1280, 720), (854, 480)];
pub const FRAME_RATES: [u32; 5] = [24, 25, 30, 50, 60];
