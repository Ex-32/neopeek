# neopeek — Design Plan

A modern Rust rewrite of sndpeek (Princeton Sound Lab): a real-time audio visualizer
that renders the host's live audio as a 3D waterfall of FFT snapshots receding into a
"horizon", plus a 2D HUD with level meters and diagnostics, with theming support and
modern Linux audio backends.

> **Source status.** The clean-room analysis of sndpeek-1.41 is complete and landed in
> `docs/research-sndpeek.md`; this plan incorporates its verified constants, the control
> surface, and its "Lessons" modernization recommendations. Items still marked
> **[assumption]** are our own design choices not dictated by sndpeek or the user.
>
> **Clean-room rule:** sndpeek is GPL v2-or-later (Marsyas/CARL components included in
> the tarball are GPL-family). neopeek copies **no code, comments, structure, or
> assets** — we match *behavior and discrete parameters* (facts, not copyrightable
> expression) and implement everything fresh in Rust with independent crates
> (`rustfft` for DSP, no ported Marsyas/CARL/RtAudio code). neopeek is independently
> licensed (open question §9, default MIT/Apache-2.0 dual).

---

## 1. Goals / Non-goals

### Goals

1. Visualize *whatever the host is playing* (default sink monitor capture) as:
   - a **3D waterfall** ("horizon effect"): a ring of FFT magnitude snapshots laid out
     along z, receding into the distance, depth-dimmed — the sndpeek signature look;
   - companion views faithful to sndpeek: **2D waveform** and **Lissajous** figure;
   - optional filled-surface modern variant of the waterfall (theme/toggle).
2. Free 3D camera: orbit (rotate), pan, zoom — mouse + keyboard (the sndpeek
   "click-to-spin" is replaced by a proper orbital camera per the research lessons,
   defaulting to the sndpeek-looking vantage).
3. 2D HUD: L/R level meters (RMS + peak-hold), FPS/frame ms, FFT stats, sample rate,
   device name, backend, history depth; sndpeek-style feature readouts (centroid,
   flux, RMS, 50%/80% rolloff) with 4-frame moving average.
4. **Theming**: color schemes (waterfall colors/ramp, waveform, meters, HUD, background)
   from user-editable theme files; several built-in themes.
5. Modern Linux audio: **PipeWire and PulseAudio** first-class (via the PulseAudio
   protocol — see §3.3); ALSA nice-to-have.
6. Minimal resource footprint: no GUI framework; direct GPU rendering (wgpu). Idle CPU
   single-digit % on a modern laptop; GPU load trivial.
7. Rust; builds with a `flake.nix` (nixpkgs `buildRustPackage`). Linux primary;
   macOS/Windows a bonus (wgpu makes most of it portable for free).

### Non-goals

- No GTK/Qt/egui GUI toolkit for the main render (HUD is hand-drawn in our own pipeline).
- No duplex playback: sndpeek played the captured input back (and needed `m`/mute
  because of it). neopeek is a **listener only** — playing the host's audio back at
  itself would double it. `m` "mute" therefore pauses ingestion.
- No file playback (wav/aiff) mode in M1–M8 — the capture path is designed so a file
  source can be added later as just another PCM producer (nice-to-have, M9+).
- No PipeWire **native** bindings in the initial milestones (§3.3); PulseAudio-protocol
  capture covers PipeWire hosts via `pipewire-pulse`.
- No LPC/MFCC features (sndpeek used them only for headless stdout dumps, marked
  "broken" in its own UI) — skip per research lesson 6.
- No network/streaming capture, no per-app capture, no screenshot/export in M1–M8.
- Windows/macOS: tolerated, not tested.

---

## 2. Architecture

### 2.1 Crates / modules (cargo workspace)

```
neopeek/                     (cargo workspace)
├── Cargo.toml               workspace members
├── flake.nix, flake.lock
├── crates/
│   ├── neopeek/             binary: wiring, event loop, app state, camera, input mapping
│   ├── neopeek-audio/       capture: backends, device discovery, PCM feed
│   ├── neopeek-dsp/         deinterleave/window/FFT/features → frames
│   ├── neopeek-render/      wgpu renderer: waterfall, waveform, lissajous, HUD passes
│   ├── neopeek-theme/       theme model, RON parsing, built-ins, hot-reload
│   └── neopeek-config/      app config (TOML), CLI (clap), paths
└── docs/
    ├── research-sndpeek.md  (clean-room analysis of original)
    └── PLAN.md              (this file)
```

Why separate lib crates: `neopeek-dsp` and `neopeek-theme` are unit-testable without a
GPU/window; `neopeek-audio` is exercisable headless (dump PCM/FFT to stdout). Cost is
minimal (one workspace lockfile).

### 2.2 Data flow

```mermaid
flowchart LR
  subgraph host[Host audio stack]
    PW[(PipeWire / PulseAudio\n(default sink monitor))]
  end
  subgraph at[Capture thread — libpulse mainloop]
    CAP[capture: f32 stereo @device rate\n1024-frame chunks]
  end
  subgraph dt[DSP thread]
    DSP[stereo→mono avg + keep L/R\nHann window → zero-pad → real FFT\n1024 bins + features\n4-frame smoothing]
  end
  subgraph rt[Render thread = main]
    R1[waterfall pipeline\n48 slices along z] --> R3[surface present]
    R2[waveform + lissajous\n+ HUD passes] --> R3
    CAM[orbit camera state]
    IN[winit events]
  end
  PW -->|PCM| CAP
  CAP -->|SPSC ring: PCM f32 ~1s| DSP
  DSP -->|SPSC ring: Frame — 1024 bins + meters/features| R1
  DSP -->|same ring, newest frame| R2
  IN --> CAM --> R1
  THEME[theme file] -.hot-reload.-> R1 & R2
  CFG[config/CLI] --> CAP & DSP & R1
```

### 2.3 Threads & buffer model

Three threads, two lock-free SPSC queues (`ringbuf` 0.5), no `Mutex` on the hot path:

1. **Capture thread** (`neopeek-audio`): libpulse async mainloop in its own thread.
   Requests **1024-frame** chunks (sndpeek's `SND_BUFFER_SIZE`), float32 stereo, at
   the device's rate (48 kHz default; clamp to the device's actual fragment size the
   way RtAudio 4.1.1 did, logging a warning). Copies samples into a pre-allocated PCM
   ring. Overrun policy: drop-oldest (a visualizer must never block the audio server).
2. **DSP thread**: consumes 1024 samples per slice: stereo→mono average (for FFT/
   waveform) while retaining L and R separately (meters + Lissajous); Hanning window;
   zero-pad to 2048; real FFT (`rustfft` 6 `RealFftPlanner`) → 1024 magnitude bins;
   features (centroid, flux, RMS, rolloff 50%/80%) each smoothed by a 4-frame moving
   average (sndpeek `LP = 4`). Pushes one `Frame` into the frame ring — one slice per
   1024 samples ≈ 21.3 ms @ 48 kHz ≈ 47 slices/s, matching a 60 Hz display well.
   Overrun policy: drop-oldest (render always paints the freshest history).
3. **Render thread** (main thread, owns winit event loop + wgpu): each wake drains
   *up to* N pending frames into the history texture, then renders. No new frame →
   reuse last history (cheap), still render (camera/theme changes stay live, like
   sndpeek's freeze which kept rotating).

```
PCM ring   HeapRb<f32>, ~1 s @ rate × channels        capture → dsp   (drop-oldest)
Frame ring HeapRb<Frame>, 256 frames                 dsp → render    (drop-oldest)

struct Frame { seq: u64, bins: [f32; 1024] /* raw |X| */,
               rms: [f32; 2], peak: [f32; 2] /* L/R */,
               mono: [f32; 1024] /* last window, for waveform/lissajous */,
               centroid: f32, flux: f32, rolloff50: f32, rolloff80: f32 }
```

`Frame` is fixed-size POD (~12 KB); ring ≈ 3 MB — fine. No per-frame heap allocation
on any thread (`bytemuck` casts for texture/buffer uploads).

**Verified sndpeek constants honored** (from `docs/research-sndpeek.md`):

| constant | value | our knob |
|---|---|---|
| capture buffer | 1024 frames | `--buffer` (pow2, default 1024) |
| FFT | 2048-pt, 1024 samples zero-padded → **1024 bins** | `--fft` |
| window | Hanning | `--window hann|hamming|blackman` (free) |
| history depth | **48 slices** ≈ 1.02 s @48k | `--depth` 16–256 |
| slice z-spacing | **0.12** | keys `u`/`i`, `--spacing` |
| magnitude map | linear `sqrt(25·|X|)` (default) or dB `20·log10(|X|/8)+80)/80` | key `d` toggle |
| freq axis | log positions precomputed per bin; sndpeek default factor 1.0 (linear) | default **log factor 1.5** per research lesson 5; `c`/`v` adjust, `--logfactor` |
| sample rate | 48000 default (Linux) | auto from device, `--srate` override |
| lissajous delay | 512 samples (buffer/2) | `y`/`Y` |

**Latency budget** (monitor → pixels): PA stream latency 20–40 ms + window/slice
21.3 ms + hop fill ≤ 21.3 ms + present ≤ 1 frame ≈ **~60–90 ms** — comparable to or
better than sndpeek (its mutex+usleep(1000) hand-off was looser). Documented; not
tunable much below without losing the 1-s waterfall.

**Resource budget**: RSS < ~40 MB; ~5 MB of rings/textures; one 1024×48 R32Float
history texture (~200 KB); three small pipelines; no per-frame allocation.

### 2.4 Frame pacing & shutdown

- Render loop: `winit` 0.30 `ApplicationHandler`. The window maps immediately:
  `resumed` presents once, synchronously (the first Wayland buffer commit is
  what maps the surface — waiting for `RedrawRequested` would deadlock on a
  frame callback that only a commit can produce). Thereafter `about_to_wait`
  requests a redraw when DSP frames are pending or when a display-cadence
  tick (~one frame period, from the monitor's refresh rate; 60 Hz fallback)
  has elapsed since the last render, and otherwise parks the loop on
  `ControlFlow::WaitUntil`. On Wayland, `Renderer::render` calls
  `window.pre_present_notify()` before presenting so redraw dispatch aligns
  with the compositor frame callback (real vsync pacing, no busy spin).
- Clean shutdown: drop PA stream → close context → join threads; rings dropped;
  no cancellation flags beyond an `AtomicBool`.

---

## 3. Technology choices & justification

Versions verified against crates.io **2026-10-23**.

| concern       | choice                          | version   | notes |
|---------------|---------------------------------|-----------|-------|
| windowing     | `winit`                         | 0.30.13   | required pairing with wgpu; 0.30's `ApplicationHandler` is verbose but boring/stable |
| GPU API       | `wgpu`                          | 30.0.1    | see §3.1 |
| async runner  | `pollster`                      | 1.0.1     | block on `request_adapter` at startup |
| math          | `glam`                          | 0.30+     | view-projection matrices, nothing more |
| FFT           | `rustfft` 6 (`RealFftPlanner`)  | 6.4.1     | clean-room mandate + better numerics than porting CARL's radix-2 (research lesson 3) |
| window fn     | hand-rolled Hann/Hamming/Blackman | inline | ~30 lines; no dep |
| SPSC ring     | `ringbuf`                       | 0.5.3     | standard lock-free SPSC |
| text/HUD text | `glyphon`                       | 0.12.0    | wgpu-native text on `cosmic-text`; see §4.5 |
| audio capture | `libpulse-binding`              | 2.30.1    | see §3.3 |
| ALSA fallback | `cpal` (`alsa` host only)       | 0.18.2    | see §3.3 |
| theming       | `ron` + `serde`                 | 0.12 / 1.x | see §6 |
| config        | `toml` + `serde`, `clap` 4      | —         | file + CLI overrides (CLI wins) |
| paths         | `directories`                   | 6.0       | XDG config dir |
| hot-reload    | `notify` 8 (optional, off by default) | —   | watch theme dir |
| logging       | `tracing` + `tracing-subscriber`| —         | env-filter via `RUST_LOG` |
| fonts         | `cosmic-text` (via glyphon) + embedded OFL monospace font in `assets/fonts/` | — | see §4.5 |

### 3.1 GPU: **wgpu** over glow — chosen

- **Driver support on Linux (primary target):** wgpu prefers Vulkan but has a GL
  fallback backend, so a Vulkan-less box (old Mesa, vmware, RDP) still runs.
  `glow` covers only GL — and Vulkan on Mesa is excellent in 2026.
- **Ergonomics/safety:** Rust-native API, WGSL shaders, surface caps/sRGB/present
  handled. Raw GL (`glow`) means hand-rolling present pacing and sRGB plumbing.
- **Cost:** slower compile (~1–2 min), bigger binary (~10–20 MB); negligible for a
  desktop tool. Per-frame overhead is irrelevant at our workload (~100k vertices).
- Tradeoff stated honestly: for 2010-era GL 2.1 hardware or a tiny binary, `glow`
  would win. We don't need either.

WGSL shaders, vertex-buffer geometry; no compute shaders (nothing needs them).
**Feature-detect at init** (§9): if float textures are unavailable on the GL backend,
fall back to `R16Float`/dB-quantized `R8Unorm` history.

### 3.2 Windowing: winit 0.30

No realistic alternative in the wgpu ecosystem. Plan around the 0.30
`ApplicationHandler` API from the start.

### 3.3 Audio: **PulseAudio protocol first** (recommended), ALSA fallback, PipeWire-native later

The key architectural decision:

- **Every PipeWire desktop already speaks PulseAudio.** PipeWire ships
  `pipewire-pulse`, a full PulseAudio server replacement enabled by default on
  Fedora/Ubuntu/arch, etc. A libpulse client on a PipeWire host transparently
  captures the default sink's **monitor** — i.e. "what the host hears". One backend
  therefore covers both target stacks on virtually all modern distros.
- **Monitor capture is a PulseAudio-level concept.** "What the host hears" = recording
  from the default sink's `.monitor` source. libpulse gives us: device enumeration
  (`get_source_info_list`, distinguishing `.monitor` sources), `@DEFAULT_MONITOR@`
  semantics, `subscribe` events to follow default-device changes, and rate/format
  introspection for the HUD. Rebuilding this introspection on PipeWire's native API
  means SPA/pod plumbing that buys nothing today.
- **pipewire-rs maturity [2026 update]:** the `pipewire` crate (0.10.1, Aug 2026) has
  matured well beyond the 0.5–0.7 era — cpal 0.19 even gained a PipeWire backend —
  but it is ABI-coupled to `libpipewire` versions, its stream API is more verbose,
  and capturing a *sink monitor* node is still easiest through the PA compat layer.
  Verdict: keep `pipewire-rs` native capture as an **explicit future milestone (M9+ /
  non-goal now)**; revisit if pure-PipeWire features (per-node targeting without
  pipewire-pulse) become a requirement.
- **Fallback chain (automatic, logged, overridable via `--backend`):**
  1. `pulse` — libpulse, default monitor (covers PipeWire & PulseAudio hosts)
  2. `alsa` — via `cpal`'s ALSA host; on pure-ALSA systems there is *no* host-level
     monitor concept, so capture a chosen input (or `snd-aloop` if present) and say
     so in the HUD
  3. clear error message + exit code.
- Why not cpal as primary: cpal 0.18/0.19 does now ship `pulseaudio` and `pipewire`
  backends and is a fine fallback, but its device model makes "default sink monitor +
  follow default-device changes + rate/volume introspection" clunkier than the native
  libpulse API, and for PipeWire hosts it still rides the PA compat layer anyway.
  Keep cpal only for the ALSA path.

Capture parameters: float32, stereo, 1024-frame chunks, device rate (48 k typical);
`PA_STREAM_ADJUST_LATENCY` with ~40 ms target. No resampling on the capture path;
analysis runs at the device rate directly (frequency axis derives from the real
rate; HUD shows it). If a user pins `--srate 48000` on a 44.1 k device, linear
interpolation in `neopeek-dsp` (visuals don't need audiophile resampling).

### 3.4 HUD text

`glyphon` (wgpu-native, `cosmic-text`): renders into the same surface in a
screen-space pass. Chosen over egui (a whole immediate-mode framework — violates
"no big GUI") and over rolling our own atlas rasterizer (weeks of font shaping for
no benefit).

### 3.5 Theming & config formats

- Theme: **RON** (`ron` 0.12 + serde) — comments, trailing commas, struct syntax;
  nicer than TOML for hand-edited nested color lists. Low-stakes either way.
- Config: TOML — boring and familiar; merged with `clap` CLI overrides.

---

## 4. Rendering design

### 4.1 Waterfall construction

- One slice per DSP frame: 1024 Hann-windowed samples zero-padded to a 2048-pt
  real FFT → **1024 magnitude bins** (verified sndpeek path).
- Keep **48 history slices** (default; `--depth` 16–256 ≈ 48·21.3 ms ≈ 1.02 s @48k)
  in a ring. Slices laid out along **−z**, spacing **0.12** (adjustable `u`/`i`),
  plus z-offset (`j`/`k`, 0.1 steps — sndpeek `g_dz`), toward the horizon; the
  "now" slice sits nearest the camera.
- **Storage: a 1024×48 `R32Float` history texture** (ring via a head index sampled
  with wrap-around in the shader — no CPU row shifting). One `write_texture` of the
  newest row per frame (~4 KB). This beats rewriting vertex data per frame.
- **Geometry: a static vertex grid** of 48×1024 points (x = precomputed
  log-spaced positions per bin — the sndpeek `g_log_positions` idea preserved as a
  CPU-side precompute; y = 0 base; z = slice depth). **Vertex shader** fetches the
  slice's magnitude from the history texture and displaces y. One draw call.
- **Drawing mode:** sndpeek draws each slice as a **`GL_LINE_STRIP`** with
  depth-cueing colors — default mode honors that: line-list indices per slice
  (~98k indices total), colors computed in the fragment stage from the slice's
  depth factor `fval = (depth − i + drift)/depth`:
  - default: past slices green-ish `(0.4·fval, 1.0·fval, 0.4·fval)`, future/rusty
    `(1.0·fval, 0.7·fval, 0.4·fval)`, "now" line cyan `(0.4, 1.0, 1.0)`;
  - **rainbow** mode (`r` / theme flag): hue blended by depth factor;
  - **filled-surface modern extra**: same grid drawn as triangles with a theme
    color-ramp texture (1D) mapping magnitude → color; toggled by key/config.
    This is our addition, not sndpeek's look — behind a flag so the default stays
    faithful.
- **Magnitude → y:** linear `y = gain · freq_scale · 1.8 · sqrt(25·|X|)` (sndpeek
  default) or dB variant `(20·log10(|X|/8)+80)/80`; gain/scale keys per §4.3;
  `db_floor/db_ceil` in theme for the dB mode.
- **Waveform view:** last 1024 mono samples (already in `Frame`), drawn as a
  line strip at `y = 0.7`, x ∈ −1.8…1.8, default color `(0.4, 0.4, 1.0)`,
  time-window restrict via `C`/`V` (32…1024 samples shown, sndpeek `g_time_view`),
  line width `<`/`>`. Dynamic VB of ≤ 2048 verts/frame (~48 KB upload — trivial).
- **Lissajous:** L on X vs R on Y (`lissajous_scale` 0.45), drawn at `x = 1.2`,
  default color `(1.0, 1.0, 0.5)`; mono source (default monitor is effectively
  stereo but often identical channels) → pseudo-Lissajous against a 512-sample
  delayed copy (`y`/`Y` adjust delay 0…1024). Same line-strip pipeline.

### 4.2 Pipelines (3 total, all trivial)

| pass | pipeline | input |
|------|----------|-------|
| 1. waterfall | static grid VB/IB + history texture (+ optional 1D ramp for filled mode) | frame ring → texture rows |
| 2. waveform / lissajous | dynamic small VB, line strips | newest `Frame` |
| 3. HUD | glyphon (text) + tiny quad pipeline for meters (theme-gradient bars) | frame stats + camera + perf |

One surface, one command encoder per frame, surface pass + screen-space HUD pass.
No MSAA initially (line rendering doesn't benefit much; revisit in M8). Present
mode `Adaptive` on Wayland/X11, `Fifo` fallback.

### 4.3 Camera & controls

**Camera (modernized per research lesson 4):** state `target: Vec3` (default
waterfall mid-point), `azimuth θ`, `elevation φ` (clamped ±85°), `distance d`
(clamped), `fov` 45°, near/far 0.1/300 (sndpeek's `gluPerspective(45, aspect, 1, 300)`
near adjusted). View = look-at from spherical offsets:

```
eye = target + d * (cosφ·sinθ, sinφ, cosφ·cosθ)
view = look_at(eye, target, up=(0,1,0));  proj = perspective(fov, aspect, 0.1, 300)
```

Default vantage ≈ sndpeek's look (eye above origin, slight tilt); **no
always-spinning auto-rotation** — replaced by user orbit (research lesson 4); a
slow auto-spin can be an optional `--autospin` for the charming original behavior.

**Control surface** — sndpeek keys preserved where they don't collide, plus the new
mouse controls:

| input | action |
|---|---|
| left-drag | orbit (θ, φ) *(new; sndpeek had click-to-spin)* |
| shift-drag / middle-drag | pan target *(new)* |
| wheel | dolly zoom *(new; sndpeek never had one)* |
| `[` / `]` | camera tilt (elevation, 0.025 steps — sndpeek) |
| `-` / `=` | spectrum freq scale ×0.99/×1.01 |
| `_` / `+` | waveform time scale |
| `l` / `L` | lissajous scale |
| `c` / `v` | log factor (freq axis curvature) |
| `u` / `i` | slice spacing ×1.02/×0.98 |
| `j` / `k` | waterfall z-offset ±0.1 |
| `C` / `V` | restrict time-domain window (1..32 → 1024..32 samples) |
| `,` / `.` | waterfall line width 1–4 |
| `<` / `>` | waveform line width |
| `y` / `Y` | lissajous delay ±10 (0..1024) |
| `1` / `2` / `3` (`w`) / `4` | toggle waveform / lissajous / waterfall / features panel |
| `d` | dB vs linear magnitude |
| `r` | rainbow waterfall colors |
| `b` | waterfall direction forward/backward |
| `f` / `z` | freeze (keep rendering & rotating, stop ingesting new slices) |
| `m` | mute (pause ingestion; neopeek has no playback) |
| `s` | fullscreen |
| `T` | cycle theme *(new)* |
| `H` | HUD toggle *(new)* |
| `p` | print all settings to console |
| `F` | reset/fit camera *(new)*; `X`/`Y`/`Z` snap top/side/front views *(new)* |
| `q` / `Esc` | quit |
| `h` | help overlay (full key table) |

### 4.4 HUD contents (M6)

- Left: **L/R meters** (RMS bar + peak-hold tick with ~800 ms hold, dB scale,
  theme gradient); **feature panel** (sndpeek-faithful, 4-frame smoothed):
  `centroid = … Hz`, `flux`, `RMS`, `50% rolloff`, `80% rolloff`.
- Right: FPS + frame ms, render backend (Vulkan/GL + adapter name).
- Bottom: device name, backend, sample rate, FFT size/window/hop, history depth,
  keybind hint line (toggleable).
- Overlays: `frozen… (press f to resume)`, `muted… (press m to unmute)` —
  sndpeek-style; device-reconnecting notice.
- Layout in pixel space, DPI-scaled via `winit` scale factor.

### 4.5 HUD implementation choice

Boring path: **glyphon for text + a 30-line quad pipeline for meters**, one
screen-space render pass into the same surface. No atlas management of our own, no
extra framework. Font: one OFL-licensed monospace font embedded in `assets/fonts/`
(license file alongside), with `cosmic-text` system-font fallback.

---

## 5. Audio capture design

### 5.1 Device discovery & selection

- Startup: enumerate PA sources (`get_source_info_list`), mark `.monitor` sources,
  `--list-devices` prints friendly names (source name, description, monitor?).
- Default: record from the default sink's monitor (`@DEFAULT_MONITOR@` semantics).
  On `subscribe` events (default sink/source changed) migrate the stream
  (close + reopen; brief gap acceptable, HUD shows a transition notice).
- `--device <name|index>` forces a specific source (mic, specific monitor, …).

### 5.2 Buffer sizes & rates

- Format: float32, stereo, **1024-frame** chunks requested (clamped to device
  fragment size, warning logged — RtAudio-parity behavior).
- PA latency request: ~40 ms total (`PA_STREAM_ADJUST_LATENCY`).
- PCM ring: 1 s per channel (drop-oldest); DSP consumes fixed 1024-sample hops; on
  overflow, skip forward to newest data.
- Slice cadence: one FFT per 1024 samples (~21.3 ms @48k ≈ 47 slices/s) — matches
  the display; no extra pacing needed on the DSP side.

### 5.3 Failure modes

- PA daemon unreachable → try ALSA/cpal → HUD error text + exit code.
- Device disappears (USB unplug): stream error callback → reconnect with backoff
  (250 ms → 5 s), "reconnecting…" in HUD.

---

## 6. Theming

### 6.1 Theme file (RON)

```ron
(
  name: "Ember",
  background: "#0b0b10",
  waterfall: (
    past: "#66ff66",          // depth-dimmed green-ish (sndpeek default family)
    future: "#ffb366",        // rusty
    now_line: "#66ffff",      // cyan
    rainbow: true,            // optional hue-by-depth mode
    db_floor: -80.0, db_ceil: -10.0,   // dB magnitude mode
    surface_ramp: [           // used only in filled-surface mode
      (pos: 0.0, color: "#05070f"),
      (pos: 0.4, color: "#2b1a5e"),
      (pos: 0.7, color: "#e2542a"),
      (pos: 1.0, color: "#ffe3a0"),
    ],
  ),
  waveform: ( color: "#6666ff", width: 1.0 ),
  lissajous: ( color: "#ffff80", scale: 0.45 ),
  meters: ( grad_low: "#2ecc71", grad_high: "#e74c3c", peak_hold_ms: 800 ),
  hud: ( text: "#cfd3dc", dim: "#6b7280", font_size: 14.0, accent: "#e2542a" ),
)
```

### 6.2 Built-ins & loading

Built-ins compiled in as embedded RON parsed through the same code path (`Ember`,
`Ice` (blue/cyan), `Terminal` (green/mono) — final names in implementation).
Search order: `--theme <path|name>` → `$XDG_CONFIG_HOME/neopeek/themes/<name>.ron`
→ built-in default. Malformed/missing file → log warning, fall back (never crash
at startup over a theme).

Hot reload: `notify` watcher, off by default, `--watch-theme` enables (dev aid).

---

## 7. File / dependency layout

### 7.1 Workspace layout

See §2.1. Shared versions via `[workspace.dependencies]`; release profile:
`lto = "thin"`, `codegen-units = 1`, `panic = "abort"`.

### 7.2 flake.nix outline — **`buildRustPackage`** (nixpkgs `rustPlatform`)

Simplest correct path: nixpkgs' `rustPlatform.buildRustPackage` reads the workspace
`Cargo.lock` directly (no naersk/cargo2nix).

```nix
{
  description = "neopeek: modern sndpeek-style audio visualizer";
  inputs.nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";
  inputs.flake-utils.url = "github:numtide/flake-utils";

  outputs = { self, nixpkgs, flake-utils, ... }:
    flake-utils.lib.eachDefaultSystem (system:
      let pkgs = nixpkgs.legacyPackages.${system};
          neopeek = pkgs.rustPlatform.buildRustPackage {
            pname = "neopeek"; version = "0.1.0";
            src = ./.;
            cargoLock.lockFile = ./Cargo.lock;
            nativeBuildInputs = [ pkgs.pkg-config pkgs.makeWrapper ];
            buildInputs = with pkgs; [ libpulseaudio alsa-lib vulkan-loader ];
            postInstall = ''
              wrapProgram $out/bin/neopeek \
                --prefix LD_LIBRARY_PATH : ${pkgs.lib.makeLibraryPath [
                  pkgs.libpulseaudio pkgs.alsa-lib pkgs.vulkan-loader ]}
            '';
          };
      in {
        packages.default = neopeek;
        apps.default = { type = "app"; program = "${neopeek}/bin/neopeek"; };
        devShells.default = pkgs.mkShell {
          packages = with pkgs; [ cargo rustc rustfmt clippy pkg-config
                                  libpulseaudio alsa-lib vulkan-loader
                                  vulkan-tools ];
        };
      });
}
```

Runtime deps: a Vulkan driver (or nothing extra for the GL fallback — wgpu picks
automatically); a `pipewire-pulse`/PulseAudio server for capture. The
`wrapProgram` handles loader paths under `nix run`.

---

## 8. Milestone plan

Each milestone is independently shippable and **ends in a git commit** (user
requires commit-per-step; do not push). Commit style: `M<n>: <summary>`.

| # | Milestone | Scope | Done = |
|---|-----------|-------|--------|
| M1 | Scaffold + flake | workspace, 6 crates, `main` opens blank winit+wgpu window (clear background) + `RUST_LOG` tracing; flake builds | `nix build` and `cargo run` both work; commit |
| M2 | Audio capture | `neopeek-audio`: libpulse backend, `--list-devices`, default-monitor capture, PCM ring filled; `--dump-pcm` writes 2 s of f32 PCM; reconnect/backoff | valid f32 PCM at device rate dumped; commit |
| M3 | FFT pipeline | `neopeek-dsp`: stereo→mono, Hann, zero-pad 2048 real FFT → 1024 bins, features (centroid/flux/rms/rolloff50/80, 4-frame smoothing), frame ring; unit tests (440 Hz sine → correct peak bin; rolloff/centroid sanity) | correct bin peak for a test tone; `--dump-fft` debug output; commit |
| M4 | Waterfall + views | history texture, log-position grid, line-strip waterfall with depth-dim/rainbow colors; waveform strip; Lissajous; fixed camera | recognizable sndpeek-style waterfall of host audio; commit |
| M5 | Camera | orbit/pan/zoom per §4.3 (mouse + keys), clamps, reset/snap views, spacing/z-offset keys, freeze/mute semantics | smooth interaction ≥60 fps; commit |
| M6 | HUD | glyphon text + meter quads: L/R RMS+peak, feature panel (centroid/flux/RMS/rolloff), FPS, FFT stats, rate, device, backend, key hints, frozen/muted overlays | all §4.4 items visible & live; commit |
| M7 | Theming | RON loader, 3 built-ins, `--theme`, malformed-file fallback, `T` cycling, hot-reload behind flag | theme switch at runtime; commit |
| M8 | Polish/perf/docs | no-alloc hot-path audit, dB mode + filled-surface mode + line-width keys (if not landed earlier), LTO build, README, `--help`/`--print` settings dump, flake polish, icon | profiler-clean hot path; docs; commit |

Optional M9+ (unscheduled): native `pipewire-rs` backend, per-node capture, file
playback (`--nodisplay`-style headless feature dump parity), camera inertia, MSAA,
`--autospin`.

---

## 9. Risks & open questions

| risk | assessment | mitigation |
|------|------------|------------|
| PipeWire native binding maturity | resolved direction for now: PA-compat-first (§3.3); pipewire-rs 0.10 viable but ABI-pinned | native backend deferred to M9+; PA protocol covers both stacks today |
| "What the host hears" absent on pure-ALSA systems | no monitor concept outside PA/PW | document: needs `pipewire-pulse`/PA or `snd-aloop`; HUD warning when capturing a mic/input instead |
| wgpu GL fallback limits (old drivers) | vertex texture fetch + float textures are the risky bits | feature-detect; `R16Float`/dB-quantized `R8Unorm` history fallback; log adapter choice |
| glyphon 0.x API movement | 0.12.0 pinned | isolate behind `neopeek-render::hud` so a swap costs one module |
| FFT latency vs liveliness | 21.3 ms slice cadence is fine; window lag inherent | `--fft`/`--depth` knobs; measure in M8 |
| Log-factor default deviates from sndpeek | linear default squashes bins (research lesson 5) | default 1.5, `c`/`v` and `--logfactor` expose the whole range incl. 1.0 |
| Nix + Vulkan ICD paths under `nix run` | loader may not find drivers | wrapProgram (§7.2); `devShells` documented for local work |

### Open questions for the user (defaults chosen; change anytime)

1. **License**: sndpeek confirmed GPL v2+; since neopeek is fully clean-room
   (rustfft, no ported code), we recommend **MIT OR Apache-2.0** dual. GPL-3.0 is
   the alternative if you want the copyleft stance.
2. **Duplex playback**: sndpeek played captured audio back (hence its mute key).
   neopeek defaults to **no playback** (listener-only). Confirm?
3. **Log-factor default 1.5** (mildly log) vs sndpeek's faithful 1.0 — confirm 1.5?
4. Theme format RON (chosen) vs TOML — any strong preference?
5. `--print` feature dump to stdout (centroid flux rms rolloffs; no MFCC) — wanted
   as a CLI flag like sndpeek's, or drop entirely?
