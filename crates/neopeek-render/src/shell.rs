//! The winit/wgpu window shell, moved here from the binary at M4
//! (PLAN.md §4). Owns the surface/depth buffers, the waterfall + waveform +
//! Lissajous pipelines, the history texture ring, and the frame-ring
//! consumer. The binary keeps app state, the event loop, and input mapping.
//!
//! There is no display/GPU in the build environment — this module is
//! verified by `cargo check`/clippy and the user's local `nix build`, per
//! the no-GUI-testing rule in PLAN.md §9.

use std::sync::Arc;
use std::time::Instant;

use winit::window::Window;

use neopeek_dsp::{Frame, WINDOW_FRAMES};
use neopeek_theme::Theme;

use crate::camera::Camera;
use crate::frame_ring::{self, FrameCons, FrameProd, FRAME_RING_FRAMES};
use crate::history::History;
use crate::hud::HudData;
use crate::hud_render::{HudQuads, HudUI};
use crate::lines::LinePipeline;
use crate::views::{LineVertex, ViewSettings};
use crate::waterfall::{WaterUniforms, Waterfall};

/// Slice period at sndpeek's default 48 kHz, for the drift animation.
const DEFAULT_RATE: u32 = 48_000;

/// Slices drained per wake; the ring holds 256.
const MAX_FRAMES_PER_WAKE: usize = FRAME_RING_FRAMES;

#[derive(Debug)]
pub struct ShellError(String);

impl std::fmt::Display for ShellError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "renderer init failed: {}", self.0)
    }
}

impl std::error::Error for ShellError {}

pub struct Renderer {
    window: Arc<Window>,
    surface: wgpu::Surface<'static>,
    device: wgpu::Device,
    queue: wgpu::Queue,
    config: wgpu::SurfaceConfiguration,
    depth: wgpu::TextureView,
    camera: Camera,
    waterfall: Waterfall,
    waveform: LinePipeline,
    lissajous: LinePipeline,
    history: History,
    // HUD (M6): glyphon text + meter quads.
    hud_quads: HudQuads,
    hud_ui: HudUI,
    adapter_name: String,
    // Theming (M7): last uploaded surface-ramp data (skip identical).
    ramp_data: Option<Vec<u8>>,
    // frame plumbing (DSP thread side is outside this crate)
    frame_cons: FrameCons,
    /// Newest frame; the waveform/Lissajous strips are built from it.
    latest: Option<Frame>,
    /// Scratch buffers reused every frame (no hot-path allocation).
    scratch_frames: Vec<Frame>,
    scratch_wave: Vec<LineVertex>,
    scratch_liss: Vec<LineVertex>,
    last_frame_arrival: Option<Instant>,
    rate: u32,
}

impl Renderer {
    /// Creates the ring pair up front; the returned producer goes to the
    /// DSP thread, the consumer is wired when capture starts.
    pub fn frame_pair() -> (FrameProd, FrameCons) {
        frame_ring::frame_pair()
    }

    pub fn new(
        window: Arc<Window>,
        frame_cons: FrameCons,
        depth: usize,
    ) -> Result<Self, ShellError> {
        let instance = wgpu::Instance::new(wgpu::InstanceDescriptor::new_without_display_handle().with_env());

        let surface = instance
            .create_surface(window.clone())
            .map_err(|err| ShellError(format!("create surface: {err}")))?;

        let adapter = pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions {
            power_preference: wgpu::PowerPreference::HighPerformance,
            force_fallback_adapter: false,
            compatible_surface: Some(&surface),
            // We are not exposing wgpu to untrusted content; no fingerprinting
            // constraints, so don't snap to a limit bucket.
            apply_limit_buckets: false,
        }))
        .map_err(|err| {
            ShellError(format!(
                "no compatible GPU adapter ({err}); no Vulkan, no usable GL fallback"
            ))
        })?;

        let (device, queue) = pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor {
            label: Some("neopeek-device"),
            required_features: wgpu::Features::empty(),
            required_limits: wgpu::Limits::default(),
            experimental_features: wgpu::wgt::ExperimentalFeatures::default(),
            memory_hints: wgpu::wgt::MemoryHints::default(),
            trace: wgpu::wgt::Trace::Off,
        }))
        .map_err(|err| ShellError(format!("request device: {err}")))?;

        let caps = surface.get_capabilities(&adapter);
        // Prefer sRGB; fall back to whatever the surface offers so we never
        // crash on picky drivers.
        let format = caps
            .formats
            .iter()
            .copied()
            .find(wgpu::TextureFormat::is_srgb)
            .unwrap_or(caps.formats[0]);
        // Adaptive vsync (Wayland/X11), Fifo fallback (PLAN.md §4.2).
        let present_mode = if caps.present_modes.contains(&wgpu::PresentMode::AutoVsync) {
            wgpu::PresentMode::AutoVsync
        } else {
            wgpu::PresentMode::Fifo
        };

        let size = window.inner_size();
        let config = wgpu::SurfaceConfiguration {
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT,
            format,
            color_space: wgpu::SurfaceColorSpace::Auto,
            width: size.width.max(1),
            height: size.height.max(1),
            present_mode,
            alpha_mode: caps.alpha_modes[0],
            view_formats: vec![],
            desired_maximum_frame_latency: 2,
        };
        surface.configure(&device, &config);

        let depth_stencil = create_depth_view(&device, config.width, config.height);
        let camera = Camera::default_vantage(config.width, config.height);
        let history = History::new(&device, depth);
        let waterfall = Waterfall::new(
            &device,
            format,
            &history.view,
            wgpu::TextureFormat::Depth24Plus,
            1.5,
            depth,
        );
        // Default view strip colors (sndpeek): waveform blue, lissajous yellow.
        let strip_verts = (2 * WINDOW_FRAMES) as u64; // quad strips: 2 verts/point
        let waveform = LinePipeline::new(&device, format, wgpu::TextureFormat::Depth24Plus, "waveform", strip_verts);
        let lissajous = LinePipeline::new(&device, format, wgpu::TextureFormat::Depth24Plus, "lissajous", strip_verts);
        let adapter_info = adapter.get_info();
        let adapter_name = format!("{} ({:?})", adapter_info.name, adapter_info.backend);
        let hud_quads = HudQuads::new(&device, format);
        let hud_ui = HudUI::new(&device, &queue, format);

        tracing::info!(
            width = config.width,
            height = config.height,
            format = ?config.format,
            present = ?config.present_mode,
            renderer = ?adapter.get_info(),
            "wgpu surface initialized"
        );

        Ok(Self {
            window,
            surface,
            device,
            queue,
            config,
            depth: depth_stencil,
            camera,
            waterfall,
            waveform,
            lissajous,
            history,
            hud_quads,
            hud_ui,
            adapter_name,
            ramp_data: None,
            frame_cons,
            latest: None,
            scratch_frames: Vec::with_capacity(FRAME_RING_FRAMES),
            scratch_wave: Vec::with_capacity(2 * WINDOW_FRAMES),
            scratch_liss: Vec::with_capacity(2 * WINDOW_FRAMES),
            last_frame_arrival: None,
            rate: DEFAULT_RATE,
        })
    }

    /// Updates the sample rate (from capture) used for the drift animation.
    pub fn set_rate(&mut self, rate: u32) {
        if rate > 0 {
            self.rate = rate;
        }
    }

    /// Camera mutation for the input layer (drag orbit/pan/zoom).
    pub fn camera_mut(&mut self) -> &mut Camera {
        &mut self.camera
    }

    /// Immutable read for diagnostics dumps.
    pub fn camera(&self) -> &Camera {
        &self.camera
    }

    /// Negotiated rate, for the HUD assembly.
    pub fn rate(&self) -> u32 {
        self.rate
    }

    /// Newest drained frame, for the HUD assembly in the binary.
    pub fn latest_frame(&self) -> Option<&Frame> {
        self.latest.as_ref()
    }

    /// Whether undrained DSP frames are waiting in the ring — the
    /// low-latency half of the redraw pacing decision (see
    /// `app::redraw_due`).
    pub fn frames_pending(&self) -> bool {
        frame_ring::has_pending(&self.frame_cons)
    }

    /// Adapter name + backend, HUD diagnostics.
    pub fn adapter_name(&self) -> &str {
        &self.adapter_name
    }

    /// History texture depth (waterfall slices).
    pub fn history_depth(&self) -> usize {
        self.history.texture.height() as usize
    }

    /// Applies a new history depth: rebuilds the history texture and the
    /// waterfall grid/index/bind state. No-op when the depth already
    /// matches (called on every depth key; only a change rebuilds).
    pub fn set_depth(&mut self, depth: usize) {
        if depth == self.history.texture.height() as usize {
            return;
        }
        let new_history = History::new(&self.device, depth);
        self.waterfall
            .set_depth(&self.device, depth, &new_history.view);
        self.history = new_history;
    }

    /// Uploads the theme's surface ramp — only when it differs from the
    /// last upload. Call on startup and on every theme application (M7/M8).
    pub fn set_theme_ramp(&mut self, theme: &Theme) {
        let data = theme.ramp_data();
        if self.ramp_data.as_deref() == Some(data.as_slice()) {
            return;
        }
        self.waterfall.set_ramp(&self.queue, &data);
        self.ramp_data = Some(data);
    }

    pub fn resize(&mut self, width: u32, height: u32) {
        self.config.width = width.max(1);
        self.config.height = height.max(1);
        self.surface.configure(&self.device, &self.config);
        self.camera.aspect = self.config.width as f32 / self.config.height as f32;
        self.depth = create_depth_view(&self.device, self.config.width, self.config.height);
    }

    /// Draws one frame: the 3D scene pass, then the screen-space HUD pass
    /// (`hud` may be `None` while the HUD toggle is off). `theme` drives
    /// all colors (M7).
    pub fn render(&mut self, settings: &ViewSettings, hud: Option<&HudData>, scale: f32, theme: &Theme) {
        // Drain pending frames (FIFO, oldest first) unless frozen or
        // muted; each is appended to the history row ring. Even with no
        // new frames we continue rendering so scale/camera changes stay
        // live.
        let now = Instant::now();
        if !settings.frozen && !settings.muted {
            self.scratch_frames.clear();
            let drained = frame_ring::drain_into(&mut self.frame_cons, &mut self.scratch_frames, MAX_FRAMES_PER_WAKE);
            for frame in &self.scratch_frames {
                self.history.write_slice(&self.queue, frame);
            }
            if drained > 0 {
                if let Some(latest) = self.scratch_frames.last().copied() {
                    self.latest = Some(latest);
                }
                self.last_frame_arrival = Some(now);
            }
        }

        // Composition drift: between slice arrivals the fade fades toward
        // the next position (sndpeek animates the planes continuously).
        let period = WINDOW_FRAMES as f32 / self.rate as f32;
        let drift = match self.last_frame_arrival {
            Some(t) => ((now - t).as_secs_f32() / period).clamp(0.0, 0.95),
            None => 0.0,
        };

        // Grid rebuild only when needed.
        self.waterfall.set_log_factor(&self.queue, &self.device, settings.log_factor);

        let vp = self.camera.view_proj();
        let uniforms = WaterUniforms {
            view_proj: vp,
            head: self.history.head() as u32,
            depth: self.history.texture.height(),
            spacing: settings.spacing,
            z_offset: settings.z_offset,
            db: if settings.db { 1.0 } else { 0.0 },
            rainbow: if settings.rainbow { 1.0 } else { 0.0 },
            gain: crate::views::DEFAULT_WATERFALL_GAIN,
            freq_scale: settings.freq_scale,
            drift,
            _align_hole: [0.0; 3],
            pad: [0.0; 4],
            past_color: theme.waterfall.past,
            future_color: theme.waterfall.future,
            now_color: theme.waterfall.now_line,
            db_floor: theme.waterfall.db_floor,
            db_ceil: theme.waterfall.db_ceil,
            surface: if settings.surface { 1.0 } else { 0.0 },
            pad_end: [0.0],
        };
        self.waterfall.upload_uniform(&self.queue, &uniforms);

        // Waveform + Lissajous strips from the newest frame.
        self.scratch_wave.clear();
        self.scratch_liss.clear();
        if let Some(latest) = &self.latest {
            crate::views::append_waveform(latest, settings, &mut self.scratch_wave);
            crate::views::append_lissajous(latest, settings, &mut self.scratch_liss);
        }

        let frame = match self.surface.get_current_texture() {
            wgpu::CurrentSurfaceTexture::Success(frame)
            | wgpu::CurrentSurfaceTexture::Suboptimal(frame) => frame,
            other => {
                // Timeout/Occluded/Outdated/Lost/Validation: retry next tick
                // (common on Wayland during resize).
                tracing::debug!("surface texture: {other:?}");
                self.window.request_redraw();
                return;
            }
        };
        let view = frame.texture.create_view(&wgpu::TextureViewDescriptor::default());
        let mut encoder = self.device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
            label: Some("neopeek-frame"),
        });
        {
            let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("neopeek-views"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: &view,
                    depth_slice: None,
                    resolve_target: None,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Clear(wgpu::Color {
                            r: theme.background[0] as f64,
                            g: theme.background[1] as f64,
                            b: theme.background[2] as f64,
                            a: theme.background[3] as f64,
                        }),
                        store: wgpu::StoreOp::Store,
                    },
                })],
                depth_stencil_attachment: Some(wgpu::RenderPassDepthStencilAttachment {
                    view: &self.depth,
                    depth_ops: Some(wgpu::Operations {
                        load: wgpu::LoadOp::Clear(1.0),
                        store: wgpu::StoreOp::Store,
                    }),
                    stencil_ops: None,
                }),
                timestamp_writes: None,
                occlusion_query_set: None,
                multiview_mask: None,
            });

            if settings.waterfall_on {
                if settings.surface {
                    self.waterfall.draw_surface(&mut pass);
                } else {
                    self.waterfall.draw(&mut pass);
                }
            }
            if settings.waveform_on && self.latest.is_some() {
                self.waveform.set(&self.queue, &vp, Theme::rgb(&theme.waveform.color));
                self.waveform.draw_strip(&mut pass, &self.queue, &self.scratch_wave);
            }
            if settings.lissajous_on && self.latest.is_some() {
                self.lissajous.set(&self.queue, &vp, Theme::rgb(&theme.lissajous.color));
                self.lissajous.draw_strip(&mut pass, &self.queue, &self.scratch_liss);
            }
        }

        // Screen-space HUD pass (M6): meter quads first, text on top; no
        // depth attachment, loading the scene's colors back. The binary
        // passes `None` while the HUD toggle is off.
        if let Some(hud) = hud {
            let (width, height) = (self.config.width, self.config.height);
            self.hud_quads.prepare(&self.queue, hud, width, height, scale, theme);
            self.hud_ui.prepare(&self.device, &self.queue, hud, width, height, scale, theme);
            let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("neopeek-hud"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: &view,
                    depth_slice: None,
                    resolve_target: None,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Load,
                        store: wgpu::StoreOp::Store,
                    },
                })],
                depth_stencil_attachment: None,
                timestamp_writes: None,
                occlusion_query_set: None,
                multiview_mask: None,
            });
            self.hud_quads.draw(&mut pass);
            self.hud_ui.render(&mut pass);
        }
        self.queue.submit([encoder.finish()]);
        // Wayland: schedule the compositor frame callback *before* the
        // buffer commit (`present`). This is what makes winit align
        // `RedrawRequested` with the display's drawing loop — without it
        // requested redraws are dispatched unpaced and the redraw loop
        // hot-spins. No-op on X11/Windows/macOS.
        self.window.pre_present_notify();
        self.queue.present(frame);
    }
}

fn create_depth_view(device: &wgpu::Device, width: u32, height: u32) -> wgpu::TextureView {
    device
        .create_texture(&wgpu::TextureDescriptor {
            label: Some("neopeek-depth"),
            size: wgpu::Extent3d {
                width: width.max(1),
                height: height.max(1),
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::Depth24Plus,
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT,
            view_formats: &[],
        })
        .create_view(&wgpu::TextureViewDescriptor::default())
}
