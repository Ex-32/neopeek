//! Minimal wgpu renderer: surface + clear-to-background.
//!
//! M1 scope only; the real pipelines move into `neopeek-render` at M4.
//! There is no display/GPU in the build environment — this module is
//! verified by `cargo check`/clippy and by the user's local `nix build`
//! run, per the no-GUI-testing rule in PLAN.md §9.

use std::sync::Arc;

use winit::window::Window;

/// Theme background, hardcoded until `neopeek-theme` lands (M7); `#0b0b10`
/// from the default `Ember` theme in PLAN.md §6.1.
const CLEAR_COLOR: wgpu::Color = wgpu::Color {
    r: 0.043,
    g: 0.043,
    b: 0.063,
    a: 1.0,
};

#[derive(Debug)]
pub struct InitError(String);

impl std::fmt::Display for InitError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "renderer init failed: {}", self.0)
    }
}

impl std::error::Error for InitError {}

pub struct Renderer {
    window: Arc<Window>,
    surface: wgpu::Surface<'static>,
    device: wgpu::Device,
    queue: wgpu::Queue,
    config: wgpu::SurfaceConfiguration,
}

impl Renderer {
    pub fn new(window: Arc<Window>) -> Result<Self, InitError> {
        let instance =
            wgpu::Instance::new(wgpu::InstanceDescriptor::new_without_display_handle().with_env());

        let surface = instance
            .create_surface(window.clone())
            .map_err(|err| InitError(format!("create surface: {err}")))?;

        let adapter = pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions {
            power_preference: wgpu::PowerPreference::HighPerformance,
            force_fallback_adapter: false,
            compatible_surface: Some(&surface),
            // We are not exposing wgpu to untrusted content; no fingerprinting
            // constraints, so don't snap to a limit bucket.
            apply_limit_buckets: false,
        }))
        .map_err(|err| {
            InitError(format!(
                "no compatible GPU adapter ({err}); no Vulkan, no usable GL backend"
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
        .map_err(|err| InitError(format!("request device: {err}")))?;

        let caps = surface.get_capabilities(&adapter);
        // Prefer sRGB; fall back to whatever the surface offers so we never
        // crash on picky drivers.
        let format = caps
            .formats
            .iter()
            .copied()
            .find(wgpu::TextureFormat::is_srgb)
            .unwrap_or(caps.formats[0]);

        let size = window.inner_size();
        let config = wgpu::SurfaceConfiguration {
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT,
            format,
            color_space: wgpu::SurfaceColorSpace::Auto,
            width: size.width.max(1),
            height: size.height.max(1),
            present_mode: wgpu::PresentMode::Fifo,
            alpha_mode: caps.alpha_modes[0],
            view_formats: vec![],
            desired_maximum_frame_latency: 2,
        };
        surface.configure(&device, &config);

        tracing::info!(
            width = config.width,
            height = config.height,
            format = ?config.format,
            renderer = ?adapter.get_info(),
            "wgpu surface initialized"
        );

        Ok(Self {
            window,
            surface,
            device,
            queue,
            config,
        })
    }

    pub fn resize(&mut self, width: u32, height: u32) {
        self.config.width = width.max(1);
        self.config.height = height.max(1);
        self.surface.configure(&self.device, &self.config);
    }

    pub fn render(&mut self) {
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
        let view = frame
            .texture
            .create_view(&wgpu::TextureViewDescriptor::default());
        let mut encoder = self
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                label: Some("neopeek-frame"),
            });
        {
            let _pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("neopeek-clear"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: &view,
                    depth_slice: None,
                    resolve_target: None,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Clear(CLEAR_COLOR),
                        store: wgpu::StoreOp::Store,
                    },
                })],
                depth_stencil_attachment: None,
                timestamp_writes: None,
                occlusion_query_set: None,
                multiview_mask: None,
            });
        }
        self.queue.submit([encoder.finish()]);
        self.queue.present(frame);
    }
}
