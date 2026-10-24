//! HUD drawing (wgpu + glyphon). Isolated in this module per PLAN.md §9's
//! glyphon-risk note: swapping the text engine costs one module.
//!
//! No runtime test here (headless build env): the headless math lives in
//! `hud`; this module is checked by clippy and the user's local flake build.

use glyphon::cosmic_text::{Family, Shaping};
use glyphon::cosmic_text as cosmic_text;
use glyphon::{Resolution, TextArea, TextBounds, TextRenderer, Viewport};

use crate::hud::HudData;
use neopeek_theme::Theme;

/// Embedded OFL monospace font (single weight); cosmic-text's system
/// fallback chain stays available for anything this lacks.
const FONT_JETBRAINS_MONO: &[u8] = include_bytes!("../assets/fonts/JetBrainsMono-Regular.ttf");

/// Screen-space ortho matrix (pixel coordinates, y down), column-major.
pub fn ortho_pixels(width: u32, height: u32) -> [f32; 16] {
    let w = width.max(1) as f32;
    let h = height.max(1) as f32;
    [
        2.0 / w, 0.0, 0.0, 0.0,
        0.0, -2.0 / h, 0.0, 0.0,
        0.0, 0.0, 1.0, 0.0,
        -1.0, 1.0, 0.0, 1.0,
    ]
}

/// Meter-gradient: fill fraction 0…1 → RGB lerp between the theme's
/// grad_low/grad_high (Ember reproduces the M6 green→red ramp).
pub fn gradient_color(low: [f32; 3], high: [f32; 3], fill: f32) -> [f32; 3] {
    let t = fill.clamp(0.0, 1.0);
    // Exact at the endpoints (t=1 L/R rounding shouldn't shift hues).
    if t <= 0.0 {
        return low;
    }
    if t >= 1.0 {
        return high;
    }
    let mut out = [0.0; 3];
    for (o, (l, h)) in out.iter_mut().zip(low.iter().zip(high)) {
        *o = l + (h - l) * t;
    }
    out
}

/// One axis-aligned colored rect, uploaded as one instance record
/// (rect xywh + rgba; the shader expands it to a 4-vertex strip).
fn push_rect(verts: &mut Vec<f32>, x: f32, y: f32, w: f32, h: f32, color: [f32; 4]) {
    verts.extend_from_slice(&[x, y, w, h, color[0], color[1], color[2], color[3]]);
}

/// 2 channels × 3 rects instance records × 8 floats (rect4 + rgba4).
pub const HUD_QUAD_INSTANCE_CAP: usize = 2 * 3;
pub const HUD_QUAD_INSTANCE_FLOATS: usize = HUD_QUAD_INSTANCE_CAP * 8;
/// One meter-rect instance in the vertex buffer, bytes (2×vec4).
const INSTANCE_BYTES: usize = 32;

/// The meter quads: per channel a background rect, an RMS fill rect and a
/// white peak-hold tick.
pub struct HudQuads {
    pipeline: wgpu::RenderPipeline,
    vb: wgpu::Buffer,
    uniform: wgpu::Buffer,
    bind_group: wgpu::BindGroup,
    /// Reused instance scratch.
    verts: Vec<f32>,
    /// f32 count uploaded by the last `prepare` (draw guards on it).
    len: usize,
}

impl HudQuads {
    pub fn new(device: &wgpu::Device, format: wgpu::TextureFormat) -> Self {
        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("neopeek-hud-quad-shader"),
            source: wgpu::ShaderSource::Wgsl(include_str!("shaders/hud.wgsl").into()),
        });
        let bgl = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("neopeek-hud-quad-bgl"),
            entries: &[wgpu::BindGroupLayoutEntry {
                binding: 0,
                visibility: wgpu::ShaderStages::VERTEX,
                ty: wgpu::BindingType::Buffer {
                    ty: wgpu::BufferBindingType::Uniform,
                    has_dynamic_offset: false,
                    min_binding_size: wgpu::BufferSize::new(
                        std::mem::size_of::<[f32; 16]>() as u64,
                    ),
                },
                count: None,
            }],
        });
        let layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("neopeek-hud-quad-layout"),
            bind_group_layouts: &[Some(&bgl)],
            immediate_size: 0,
        });
        let vb_layout = wgpu::VertexBufferLayout {
            // One rect per instance; the shader builds the 4 corners from
            // the vertex index (see shaders/hud.wgsl).
            array_stride: INSTANCE_BYTES as wgpu::BufferAddress,
            step_mode: wgpu::VertexStepMode::Instance,
            attributes: &[
                wgpu::VertexAttribute {
                    shader_location: 0,
                    format: wgpu::VertexFormat::Float32x4,
                    offset: 0,
                },
                wgpu::VertexAttribute {
                    shader_location: 1,
                    format: wgpu::VertexFormat::Float32x4,
                    offset: 16,
                },
            ],
        };
        let pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("neopeek-hud-quad"),
            layout: Some(&layout),
            vertex: wgpu::VertexState {
                module: &shader,
                entry_point: Some("vs_main"),
                buffers: &[Some(vb_layout)],
                compilation_options: wgpu::PipelineCompilationOptions::default(),
            },
            fragment: Some(wgpu::FragmentState {
                module: &shader,
                entry_point: Some("fs_main"),
                targets: &[Some(wgpu::ColorTargetState {
                    format,
                    blend: Some(wgpu::BlendState::REPLACE),
                    write_mask: wgpu::ColorWrites::ALL,
                })],
                compilation_options: wgpu::PipelineCompilationOptions::default(),
            }),
            primitive: wgpu::PrimitiveState {
                topology: wgpu::PrimitiveTopology::TriangleStrip,
                ..Default::default()
            },
            depth_stencil: None, // screen-space overlay, no depth tests
            multisample: wgpu::MultisampleState::default(),
            multiview_mask: None,
            cache: None,
        });
        let vb = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("neopeek-hud-quad-vb"),
            size: (HUD_QUAD_INSTANCE_FLOATS * 4) as u64,
            usage: wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let uniform = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("neopeek-hud-quad-uniforms"),
            size: std::mem::size_of::<[f32; 16]>() as u64, // one mat4
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("neopeek-hud-quad-bindgroup"),
            layout: &bgl,
            entries: &[wgpu::BindGroupEntry {
                binding: 0,
                resource: uniform.as_entire_binding(),
            }],
        });
        Self {
            pipeline,
            vb,
            uniform,
            bind_group,
            verts: Vec::with_capacity(HUD_QUAD_INSTANCE_FLOATS),
            len: 0,
        }
    }

    /// Re-lays out the meter rects (DPI-scaled) and uploads everything.
    pub fn prepare(&mut self, queue: &wgpu::Queue, hud: &HudData, width: u32, height: u32, scale: f32, theme: &Theme) {
        queue.write_buffer(
            &self.uniform,
            0,
            bytemuck::bytes_of(&ortho_pixels(width, height)),
        );
        self.verts.clear();
        let grad_low = Theme::rgb(&theme.meters.grad_low);
        let grad_high = Theme::rgb(&theme.meters.grad_high);
        for (c, r) in crate::hud::meter_rects(hud.rms_fill, hud.peak_fill)
            .iter()
            .enumerate()
        {
            let (x, y, w, h) = (
                r.x * scale,
                r.y * scale,
                r.w * scale,
                r.h * scale,
            );
            // 6 rects: channel 0 = indices 0…2, channel 1 = 3…5.
            let color = if c % 3 == 1 {
                let tint = gradient_color(grad_low, grad_high, hud.rms_fill[c / 3]);
                [tint[0], tint[1], tint[2], 1.0]
            } else if c % 3 == 2 {
                [1.0, 1.0, 1.0, 0.95]
            } else {
                [0.45, 0.47, 0.55, 0.5]
            };
            push_rect(&mut self.verts, x, y, w, h, color);
        }
        self.len = self.verts.len();
        queue.write_buffer(&self.vb, 0, bytemuck::cast_slice(&self.verts));
    }

    pub fn draw(&self, pass: &mut wgpu::RenderPass<'_>) {
        if self.len < 8 {
            return; // nothing laid out
        }
        pass.set_pipeline(&self.pipeline);
        pass.set_bind_group(0, &self.bind_group, &[]);
        pass.set_vertex_buffer(0, self.vb.slice(..));
        let instances = self.len / 8;
        pass.draw(0..4, 0..instances as u32);
    }
}

/// One laid-out text panel: buffer, physical-pixel (left, top) anchor, rgb color.
type Area<'a> = (&'a cosmic_text::Buffer, (f32, f32), (u8, u8, u8));

/// Persistent glyphon state for the HUD text.
pub struct HudUI {
    atlas: glyphon::TextAtlas,
    renderer: TextRenderer,
    swash: cosmic_text::SwashCache,
    viewport: Viewport,
    font_system: cosmic_text::FontSystem,
    /// [0] features, [1] perf, [2] device, [3] overlay, [4] keybind hints.
    buffers: Vec<cosmic_text::Buffer>,
    /// Font size the buffers were built with (theme font_size, M7).
    font_px: f32,
}

impl HudUI {
    pub fn new(device: &wgpu::Device, queue: &wgpu::Queue, format: wgpu::TextureFormat) -> Self {
        // Font size/colors come from the theme at each `prepare`; the
        // buffers start at the Ember default metrics.
        let font_px = 14.0;
        let cache = glyphon::Cache::new(device);
        let mut atlas = glyphon::TextAtlas::new(device, queue, &cache, format);
        let renderer = TextRenderer::new(
            &mut atlas,
            device,
            wgpu::MultisampleState::default(),
            None,
        );
        let mut font_system = cosmic_text::FontSystem::new();
        font_system.db_mut().load_font_data(FONT_JETBRAINS_MONO.to_vec());
        Self {
            atlas,
            renderer,
            swash: cosmic_text::SwashCache::new(),
            viewport: Viewport::new(device, &cache),
            font_system,
            buffers: HudUI::make_buffers(font_px),
            font_px,
        }
    }

    /// Five text panels, rebuilt when the theme's font size changes.
    fn make_buffers(font_px: f32) -> Vec<cosmic_text::Buffer> {
        // 14 px font / 20 px line height at the default (ratio 1.43).
        let metrics = cosmic_text::Metrics::new(font_px, (font_px * 1.43).max(1.0));
        std::iter::repeat_with(|| cosmic_text::Buffer::new_empty(metrics))
            .take(5)
            .collect()
    }

    /// Builds the HUD text (feature panel, diagnostics, device line,
    /// overlay, hints), reshapes it, and prepares glyphon for this
    /// frame's resolution. Colors + font size come from the theme (M7).
    ///
    /// The wide signature is deliberate: text + quads + theme + DPI.
    #[allow(clippy::too_many_arguments)]
    pub fn prepare(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        hud: &HudData,
        width: u32,
        height: u32,
        scale: f32,
        theme: &Theme,
    ) {
        if (theme.hud.font_size - self.font_px).abs() > f32::EPSILON {
            // Theme swap: rebuild the buffers at the new metrics.
            self.font_px = theme.hud.font_size;
            self.buffers = HudUI::make_buffers(self.font_px);
        }
        let (text, accent) = (
            Theme::rgb8(&theme.hud.text),
            Theme::rgb8(&theme.hud.accent),
        );
        feature_panel(self, hud);
        perf_panel(self, hud);
        device_panel(self, hud);
        hints_panel(self);
        overlay_panel(self, hud);

        for buf in self.buffers.iter_mut() {
            buf.shape_until_scroll(&mut self.font_system, false);
        }
        self.viewport.update(queue, Resolution { width, height });

        // Layout in physical pixels via the pure `hud_text_anchors`
        // (see hud.rs): feature panel below the meter quads, perf
        // top-right, device stats bottom, keybind hints dim under it
        // (omitted when hidden), overlay centered. `TextArea.left/top`
        // are physical; glyphon scales the buffer's logical glyph
        // coordinates itself.
        let anchors =
            crate::hud::hud_text_anchors(width, height, scale, self.font_px, crate::hud::HINT_LINES);
        let hint_color = Theme::rgb8(&theme.hud.dim);
        let ours: [Area; 4] = [
            (&self.buffers[0], anchors[0], text),
            (&self.buffers[1], anchors[1], text),
            (&self.buffers[2], anchors[2], text),
            (&self.buffers[3], anchors[3], accent),
        ];
        let mut areas: Vec<TextArea> = ours
            .iter()
            .map(|(buf, (left, top), color)| TextArea {
                buffer: buf,
                left: *left,
                top: *top,
                scale,
                bounds: TextBounds::default(),
                default_color: cosmic_text::Color::rgb(color.0, color.1, color.2),
                custom_glyphs: &[],
            })
            .collect();
        if hud.show_hints {
            areas.push(TextArea {
                buffer: &self.buffers[4],
                left: anchors[4].0,
                top: anchors[4].1,
                scale,
                bounds: TextBounds::default(),
                default_color: cosmic_text::Color::rgb(hint_color.0, hint_color.1, hint_color.2),
                custom_glyphs: &[],
            });
        }
        // Depth per glyph (metadata): 0.5 keeps text above the scene in the
        // screen-space pass (no depth state on this pipeline — unused).
        let _ = self.renderer.prepare(
            device,
            queue,
            &mut self.font_system,
            &mut self.atlas,
            &self.viewport,
            areas,
            &mut self.swash,
        );
    }

    pub fn render(&self, pass: &mut wgpu::RenderPass<'_>) {
        let _ = self.renderer.render(&self.atlas, &self.viewport, pass);
    }
}

// The five text panels are small writers kept separate for readability.

fn feature_panel(ui: &mut HudUI, hud: &HudData) {
    let attrs = cosmic_text::Attrs::new().family(Family::Name("JetBrains Mono"));
    let text = hud.feature_lines.join("\n");
    ui.buffers[0].set_text(&text, &attrs, Shaping::Advanced, None);
}

fn perf_panel(ui: &mut HudUI, hud: &HudData) {
    let attrs = cosmic_text::Attrs::new().family(Family::Name("JetBrains Mono"));
    let text = format!("{:.0} fps\n{:.1} ms\n{}", hud.fps, hud.frame_ms, hud.adapter);
    ui.buffers[1].set_text(&text, &attrs, Shaping::Advanced, None);
}

fn device_panel(ui: &mut HudUI, hud: &HudData) {
    let attrs = cosmic_text::Attrs::new().family(Family::Name("JetBrains Mono"));
    let text = format!(
        "{} · {} · {} Hz · FFT {} (window {}, hop {:.1} ms) · depth {}",
        hud.source_name,
        hud.backend,
        hud.rate,
        hud.fft_size,
        hud.window_frames,
        hud.hop_ms,
        hud.depth,
    );
    ui.buffers[2].set_text(&text, &attrs, Shaping::Advanced, None);
}

/// The keybind dump lives in its own buffer so the theme's `dim` color
/// can shade it independently of the stats line. Two lines: the single
/// line overflowed the surface width and its tail was cut off.
fn hints_panel(ui: &mut HudUI) {
    let attrs = cosmic_text::Attrs::new().family(Family::Name("JetBrains Mono"));
    let text = concat!(
        "drag orbit · shift-drag pan · wheel zoom · [ ] tilt · 1 wave · 2 liss · 3 waterfall · S surface · d dB · r rainbow · u i spacing\n",
        "j k z-offset · l L liss · < > width · y liss-delay · f freeze · m mute · h hints · H hud · T theme · F X Y Z snap · p print · q quit",
    );
    ui.buffers[4].set_text(text, &attrs, Shaping::Advanced, None);
}

fn overlay_panel(ui: &mut HudUI, hud: &HudData) {
    let attrs = cosmic_text::Attrs::new().family(Family::Name("JetBrains Mono"));
    let text = hud.overlay.join("\n");
    ui.buffers[3].set_text(&text, &attrs, Shaping::Advanced, None);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn gradient_lerps_theme_endpoints() {
        let ember = neopeek_theme::builtin("Ember").unwrap();
        let low = Theme::rgb(&ember.meters.grad_low);
        let high = Theme::rgb(&ember.meters.grad_high);
        assert_eq!(gradient_color(low, high, 0.0), low);
        assert_eq!(gradient_color(low, high, 1.0), high);
        let mid = gradient_color(low, high, 0.5);
        // Monotonic red up, green down (Ember: green→red).
        assert!(mid[0] > low[0] && mid[0] < high[0]);
        assert!(mid[1] < low[1] && mid[1] > high[1]);
        // Out-of-range inputs clamp.
        assert_eq!(gradient_color(low, high, -1.0), low);
        assert_eq!(gradient_color(low, high, 2.0), high);
    }

    #[test]
    fn quads_layout_yields_six_instances() {
        // Sanity: the layout fns feed exactly 6 instance records into the
        // cap-sized scratch.
        assert_eq!(crate::hud::meter_rects([0.5, 0.5], [0.5, 0.5]).len(), HUD_QUAD_INSTANCE_CAP);
        assert_eq!(HUD_QUAD_INSTANCE_FLOATS, HUD_QUAD_INSTANCE_CAP * 8);
    }
}
