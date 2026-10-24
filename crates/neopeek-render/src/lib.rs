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
//! - [`shell`]: the winit/wgpu window shell + pipelines + per-frame draw.
//!
//! Shaders live next to the sources in `shaders/` and are `include_str!`-ed
//! (one convention: WGSL files in the render crate only).
//!
//! No GPU/display exists in the build environment: all headless logic
//! (camera math, grid, history-row wrap, ring semantics, view builders) has
//! unit tests; the window path is validated by the user locally via the
//! flake, per the no-GUI-testing rule in PLAN.md §9.

pub mod camera;
pub mod dump;
pub mod frame_ring;
pub mod grid;
pub mod history;
pub mod lines;
pub mod shell;
pub mod views;
pub mod waterfall;
