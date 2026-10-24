//! neopeek-render: wgpu pipelines for the sndpeek-style views (PLAN.md §4.1,
//! §4.2 — milestone M4).
//!
//! Modules:
//! - [`camera`]: fixed M4 camera + minimal mat4 math (orbit controls land in M5).
//! - [`grid`]: CPU precompute of the waterfall vertex grid (log-frequency
//!   positions, x-span, depth layout) and its index buffer.
//! - [`history`]: the 1024×48 `R32Float` history texture ring (head wrap in
//!   the shader, no CPU row shifting).
//! - [`frame_ring`]: the SPSC `Frame` ring between the DSP thread and render.
//! - [`views`]: view settings + waveform/Lissajous line-strip builders.
//! - [`autoscale`]: the content-adaptive waterfall-gain state machine (`a`).
//! - [`hud`]: headless HUD math (meters, features, overlays, `HudData`).
//! - [`hud_render`]: the glyphon text pass + meter quad pipeline (M6).
//! - [`idle`]: the pure idle-pause gate (stop redrawing while audio is
//!   silent/absent; `IdleGate`).
//! - [`shell`]: the winit/wgpu window shell + pipelines + per-frame draw.
//!
//! Shaders live next to the sources in `shaders/` and are `include_str!`-ed
//! (one convention: WGSL files in the render crate only).
//!
//! No GPU/display exists in the build environment: all headless logic
//! (camera math, grid, history-row wrap, ring semantics, view builders) has
//! unit tests; the window path is validated by the user locally via the
//! flake, per the no-GUI-testing rule in PLAN.md §9.

pub mod autoscale;
pub mod camera;
pub mod dump;
pub mod frame_ring;
pub mod grid;
pub mod history;
pub mod hud;
pub mod hud_render;
pub mod idle;
pub mod lines;
pub mod shell;
pub mod views;
pub mod waterfall;
