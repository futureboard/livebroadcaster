# LiveBroadcaster

A live streaming and recording studio in Rust: egui / wgpu UI in the Futureboard **Sphere UI**
design system, with media decode, H.264 / AAC encode and MP4 / MOV mux by **MediaKit Foundation
(MKFF)** — no FFmpeg.

```sh
cargo run --release
```

The first build downloads the Chromium Embedded Framework runtime for the browser source
(~165 MB, into `.cef/`) and compiles its C++ wrapper, so it needs **CMake, Ninja and the MSVC
build tools**. `cargo build --no-default-features` leaves the browser source out.

## What it does

| Area | |
|------|---|
| Sources | Display capture (DXGI duplication + cursor), window capture (Windows Graphics Capture; GDI `PrintWindow` fallback), browser (CEF off-screen: URL or local page, size, fps, custom CSS, sound to the mixer), cameras (Media Foundation, format picker), media files (any container / codec `mkff-codecs` decodes, with sound, loop, speed, transport), images, text (system fonts, Thai, outline, from file), colour |
| Audio capture | System sound — every app except the studio, on any device (WASAPI process loopback); one application, or all but one; playback device loopback; mic / line in |
| Scenes | Sources shared across scenes, layers with move / resize / crop / opacity, snapping, Cut / Fade transitions |
| Audio | 48 kHz mixer, per-source fader / mute, meters with peak hold, master bus, monitoring (per source: output only / monitor + output / monitor only, on a chosen playback device — browser sources are heard by default), adaptive jitter buffers |
| Encode | GPU H.264 — NVIDIA NVENC, Intel Quick Sync, AMD AMF (MKFF `mkff-hwenc`) — or MKFF software H.264; Auto picks a GPU and falls back to software |
| Output | One shared H.264 + AAC encoder → RTMP / RTMPS (YouTube, Twitch, Facebook, Kick, custom; reconnect, congestion drop) and recording (fragmented MP4, MP4, MOV) |
| Compositor | On the GPU (the window's wgpu device): sources uploaded only when they change, alpha-weighted bilinear + mips (same rules as the CPU blitter), the preview draws the program texture directly, encoders get NV12 converted on the GPU and read back asynchronously. CPU compositor as the fallback (`LB_CPU_COMPOSITOR=1`) |
| Preview | wgpu renderer on a chosen GPU / API (Settings → Preview & GPU; Auto = discrete GPU, DX12 on Windows — clearly faster than OpenGL), VSync, pacing stats in Health (`LB_PERF=1` logs them) |
| Languages | English, ไทย, 简体中文, 日本語, Bahasa Indonesia — follows the system language, or Settings → Appearance; switches live (menus included). Catalogs in `i18n/*.json` (English text → translation); `cargo test` checks every text in the code is in every catalog |
| UI | Native window with a Win32 menu bar, Scenes / Layers sidebar, stage, Audio / Properties / Health tabs, settings modal, dark & light themes |

Settings and scenes live in `%APPDATA%\LiveBroadcaster\` (`settings.json`, `scenes.json`, the
browser cache in `browser\`); set `LB_CONFIG_DIR` to use another folder.

### Licences

Everything linked is MIT / Apache-2.0 / BSD / ISC / zlib — no GPL. The browser source ships
CEF (BSD-3-Clause) with Chromium inside `libcef.dll`, which contains LGPL-2.1 components (FFmpeg
built without `--enable-gpl`, Blink); they stay in that separately replaceable DLL. Ship CEF's
BSD licence and `CREDITS.html` (in `.cef/`) with a release.

MKFF (`mkff-codecs`, `mkff-hwenc`) comes from
[futureboard/mediakit-foundation](https://github.com/futureboard/mediakit-foundation), pinned to a
commit (`rev` in `Cargo.toml`): push MKFF first, then bump the `rev` here. To work on both at
once, override it locally with a `[patch."https://github.com/futureboard/mediakit-foundation"]`
section pointing at a checkout.

## Layout

```
src/
  engine.rs        compositor thread + audio mixer thread
  gpu_compositor.rs  wgpu compositor, NV12 conversion + async read-back
  video.rs         RGBA images, MKFF frame → RGBA, CPU bilinear / mip blitter
  bin/lb-browser-helper.rs  CEF's helper processes
  audio.rs         taps, resampling, fader law
  sources/         one runtime per source kind
  output/          encoder thread (MKFF), recorder, RTMP client + AMF0
  ui/              the studio window (Sphere UI)
crates/sphere-ui/  Sphere UI for egui (see below)
```

## Translating

Interface texts are written in English inside `tr!("…")` / `trf!("… {name} …", name = value)`.
To add or change a text, edit the code, then add the same English key to each `i18n/<lang>.json`
(`cargo test catalogs_cover_the_code` lists what's missing or unused). Keep `{placeholders}` and
the menu `	` shortcuts; Win32 menu accelerators use `(&F)` style in Thai / Chinese / Japanese.

## `crates/sphere-ui`

An egui port of SphereUIComponents (Futureboard, GPUI): the token system (palette loaded from
Futureboard theme files — `themes/Default.json`, `themes/Light.json`, or any theme with the same
schema via `Palette::from_theme_json`), radius / spacing / size / typography / state-layer ladders,
the system font chain (Segoe UI → Leelawadee UI → CJK … on Windows), Lucide icons (vendored SVGs,
ISC, rasterised with resvg) and components: `Button`, `IconButton`, `Panel`, `ListRow`, `Fader`,
`meter`, `Toggle`, `segmented`, `tabs`, `chip`, `title_bar`, `status_bar`, `dialog_frame`,
menus and popovers. To use another Lucide icon, copy its SVG into `crates/sphere-ui/icons/` and
add it to `src/icon_set.rs`.

## Tests

```sh
cargo test --workspace                 # unit tests + end-to-end recording through MKFF (clips: MKFF_TESTDATA=<mkff>/testdata/codecs)
cargo test -- --ignored rtmp           # streams to `ffmpeg -listen 1` (needs ffmpeg on PATH)
cargo test --release -- --ignored perf_1080p60 --nocapture   # encode time per encoder at 1080p60
cargo test -- --ignored browser_renders_page --nocapture     # CEF picture + sound (after cargo build)
cargo test -- --ignored process_loopback --nocapture         # app audio capture (plays a soft beep)
LB_TEST_PAGE=page.html cargo test -- --ignored browser_audio_timeline --nocapture   # when a page's sound comes out
LB_TEST_PAGE=page.html cargo test --release -- --ignored browser_sound_recording --nocapture   # …and in a 1080p60 recording
cargo test --release -- --ignored window_capture_methods --nocapture   # WGC vs GDI cost
cargo run --release --example encode_bench -- 1280 720 2500
```
