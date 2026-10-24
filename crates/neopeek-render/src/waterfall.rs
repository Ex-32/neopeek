//! The waterfall pipeline: static grid VB/IB + history texture + uniforms
//! (PLAN.md §4.1, §4.2 pass 1). Per rendered frame the CPU work is one
//! uniform write + a ~4 KB texture row per *new* slice — no allocation.

use crate::grid::{self, DEFAULT_DEPTH};

const SHADER: &str = include_str!("shaders/waterfall.wgsl");

/// Uniform block matching `shaders/waterfall.wgsl` offset-for-offset (the
/// naga reflection test in `tests/` pins every member offset + the 192 B
/// span; see WGSL alignment rules below).
#[repr(C)]
#[derive(Debug, Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
pub struct WaterUniforms {
    /// view·proj, column-major.
    pub view_proj: [f32; 16], // 0…64
    pub head: u32,            // 64
    pub depth: u32,           // 68
    pub spacing: f32,         // 72
    pub z_offset: f32,        // 76
    pub db: f32,              // 1.0 = dB magnitude mode
    pub rainbow: f32,         // 1.0 = rainbow depth colors
    pub gain: f32,            // 88
    pub freq_scale: f32,      // 92
    pub drift: f32,           // slice-fade animation factor 0…1; 96…100
    /// Explicit WGSL alignment gap: the shader's next member
    /// `pad: vec3<f32>` has align 16, so WGSL places it at offset 112 and
    /// inserts 12 bytes here after `drift` (ends 100). Naming it a real
    /// field keeps the offset visible to testing instead of relying on
    /// invisible repr(C) padding.
    pub _align_hole: [f32; 3], // 100…112 (unused, WGSL gap)
    /// The shader's `pad: vec3<f32>` member (unused in the shader). In the
    /// uniform address space the vec3 occupies 16 B (its align), hence the
    /// quad here; allocates exactly 112…128.
    pub pad: [f32; 4],
    /// Theme colors (PLAN.md §6.1); rgb used, a ignored. Align-16 in WGSL.
    pub past_color: [f32; 4],   // 128
    pub future_color: [f32; 4], // 144
    pub now_color: [f32; 4],    // 160
    /// dB magnitude mapping window (Ember matches the M4 formula).
    pub db_floor: f32, // 176
    pub db_ceil: f32,  // 180
    /// 1.0 = filled-surface mode (`S`, M8).
    pub surface: f32, // 184
    /// Closes the block at 192 B = the WGSL struct span (align 16).
    pub pad_end: [f32; 1],
}

impl WaterUniforms {
    pub fn zeroed_view_proj() -> Self {
        Self {
            view_proj: [0.0; 16],
            head: 0,
            depth: DEFAULT_DEPTH as u32,
            spacing: 0.12,
            z_offset: 0.0,
            db: 0.0,
            rainbow: 0.0,
            gain: 1.0,
            freq_scale: 1.0,
            drift: 0.0,
            _align_hole: [0.0; 3],
            pad: [0.0; 4],
            past_color: [0.4, 1.0, 0.4, 1.0],
            future_color: [1.0, 0.7, 0.4, 1.0],
            now_color: [0.4, 1.0, 1.0, 1.0],
            db_floor: -80.0,
            db_ceil: 0.0,
            surface: 0.0,
            pad_end: [0.0],
        }
    }
}

pub struct Waterfall {
    pub pipeline: wgpu::RenderPipeline,
    vb: wgpu::Buffer,
    ib: wgpu::Buffer,
    pub uniform: wgpu::Buffer,
    /// Bind-group layout kept for `set_depth` (bind-group rebuild).
    bgl: wgpu::BindGroupLayout,
    bind_group: wgpu::BindGroup,
    /// Theme ramp view kept for `set_depth` (bind-group rebuild).
    ramp_view: wgpu::TextureView,
    index_count: u32,
    /// Grid rebuilds when this differs from the incoming `log_factor`.
    grid_log_factor: f32,
    /// Grid layer count (matches the bound history texture height).
    grid_depth: usize,
    /// Grid bin count (matches the frames the engine produces).
    grid_bins: usize,
    /// Filled-surface mode (M8): triangle topology over the same grid.
    surface_pipeline: wgpu::RenderPipeline,
    surface_ib: wgpu::Buffer,
    surface_index_count: u32,
    /// 1×256 RGBA8 ramp texture (theme-driven).
    ramp: wgpu::Texture,
}

impl Waterfall {
    pub fn new(
        device: &wgpu::Device,
        format: wgpu::TextureFormat,
        history_view: &wgpu::TextureView,
        depth_format: wgpu::TextureFormat,
        log_factor: f32,
        depth: usize,
        bins: usize,
    ) -> Self {
        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("neopeek-waterfall-shader"),
            source: wgpu::ShaderSource::Wgsl(SHADER.into()),
        });

        let bgl = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("neopeek-waterfall-bgl"),
            entries: &[
                wgpu::BindGroupLayoutEntry {
                    binding: 0,
                    visibility: wgpu::ShaderStages::VERTEX,
                    ty: wgpu::BindingType::Buffer {
                        ty: wgpu::BufferBindingType::Uniform,
                        has_dynamic_offset: false,
                        // Same constant the uniform buffer is sized from;
                        // mismatch fails at pipeline creation, not first draw.
                        min_binding_size: wgpu::BufferSize::new(
                            std::mem::size_of::<WaterUniforms>() as u64,
                        ),
                    },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 1,
                    visibility: wgpu::ShaderStages::VERTEX,
                    ty: wgpu::BindingType::Texture {
                        sample_type: wgpu::TextureSampleType::Float { filterable: false },
                        view_dimension: wgpu::TextureViewDimension::D2,
                        multisampled: false,
                    },
                    count: None,
                },
                // Filled-surface ramp (M8): 1D ramp stored as a 1-row
                // texture, loaded (not sampled — vertex stage) in the shader.
                wgpu::BindGroupLayoutEntry {
                    binding: 2,
                    visibility: wgpu::ShaderStages::VERTEX,
                    ty: wgpu::BindingType::Texture {
                        sample_type: wgpu::TextureSampleType::Float { filterable: false },
                        view_dimension: wgpu::TextureViewDimension::D2,
                        multisampled: false,
                    },
                    count: None,
                },
            ],
        });

        let layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("neopeek-waterfall-layout"),
            bind_group_layouts: &[Some(&bgl)],
            immediate_size: 0,
        });

        let vb_attrs = [
            wgpu::VertexAttribute { shader_location: 0, format: wgpu::VertexFormat::Float32, offset: 0 },
            wgpu::VertexAttribute { shader_location: 1, format: wgpu::VertexFormat::Uint32, offset: 4 },
            wgpu::VertexAttribute { shader_location: 2, format: wgpu::VertexFormat::Uint32, offset: 8 },
        ];
        let vb_layout = wgpu::VertexBufferLayout {
            array_stride: 12,
            step_mode: wgpu::VertexStepMode::Vertex,
            attributes: &vb_attrs,
        };

        let pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("neopeek-waterfall"),
            layout: Some(&layout),
            vertex: wgpu::VertexState {
                module: &shader,
                entry_point: Some("vs_main"),
                buffers: &[Some(vb_layout.clone())],
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
                topology: wgpu::PrimitiveTopology::LineList,
                ..Default::default()
            },
            depth_stencil: Some(wgpu::DepthStencilState {
                format: depth_format,
                depth_write_enabled: Some(true),
                depth_compare: Some(wgpu::CompareFunction::Less),
                stencil: wgpu::StencilState::default(),
                bias: wgpu::DepthBiasState::default(),
            }),
            multisample: wgpu::MultisampleState::default(),
            multiview_mask: None,
            cache: None,
        });

        // Static grid, allocated once here; replaced only on a log-factor,
        // depth, or FFT-size change (`set_log_factor` / `set_depth` /
        // `set_fft_size`).
        let (vb, ib, index_count) = create_grid_buffers(device, log_factor, depth, bins);

        // Filled-surface mode (M8): same VB, triangle index buffer.
        let surface_index_list = grid::surface_indices(depth, bins);
        let surface_index_count = surface_index_list.len() as u32;
        let surface_ib = create_buffer_from(
            device,
            "neopeek-waterfall-surface-indices",
            bytemuck::cast_slice(&surface_index_list),
            wgpu::BufferUsages::INDEX,
        );
        // 1D ramp texture: 256×1 RGBA8; contents come from the theme via
        // `set_ramp` (rebuilt on every theme application).
        let ramp = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("neopeek-waterfall-ramp"),
            size: wgpu::Extent3d { width: 256, height: 1, depth_or_array_layers: 1 },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::Rgba8Unorm,
            usage: wgpu::TextureUsages::COPY_DST | wgpu::TextureUsages::TEXTURE_BINDING,
            view_formats: &[],
        });
        let ramp_view = ramp.create_view(&wgpu::TextureViewDescriptor::default());

        let uniform = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("neopeek-waterfall-uniforms"),
            size: std::mem::size_of::<WaterUniforms>() as u64,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });

        let bind_group = grid_bind_group(device, &bgl, &uniform, history_view, &ramp_view);

        // The filled-surface variant differs only in topology.
        let surface_pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("neopeek-waterfall-surface"),
            layout: Some(&layout),
            vertex: wgpu::VertexState {
                module: &shader,
                entry_point: Some("vs_main"),
                buffers: &[Some(vb_layout.clone())],
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
                topology: wgpu::PrimitiveTopology::TriangleList,
                ..Default::default()
            },
            depth_stencil: Some(wgpu::DepthStencilState {
                format: depth_format,
                depth_write_enabled: Some(true),
                depth_compare: Some(wgpu::CompareFunction::Less),
                stencil: wgpu::StencilState::default(),
                bias: wgpu::DepthBiasState::default(),
            }),
            multisample: wgpu::MultisampleState::default(),
            multiview_mask: None,
            cache: None,
        });

        Self {
            pipeline,
            vb,
            ib,
            uniform,
            bgl,
            bind_group,
            ramp_view,
            index_count,
            grid_log_factor: log_factor,
            grid_depth: depth,
            grid_bins: bins,
            surface_pipeline,
            surface_ib,
            surface_index_count,
            ramp,
        }
    }

    /// Uploads a theme's surface ramp (RGBA8, 256 samples) — called when
    /// the theme changes. `neopeek_theme::ramp_to_rgba8` produces the data.
    pub fn set_ramp(&self, queue: &wgpu::Queue, data: &[u8]) {
        debug_assert_eq!(data.len(), 256 * 4);
        queue.write_texture(
            wgpu::TexelCopyTextureInfo {
                texture: &self.ramp,
                mip_level: 0,
                origin: wgpu::Origin3d::ZERO,
                aspect: wgpu::TextureAspect::All,
            },
            data,
            wgpu::TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: Some(256 * 4),
                rows_per_image: None,
            },
            wgpu::Extent3d { width: 256, height: 1, depth_or_array_layers: 1 },
        );
    }

    /// Re-upload the grid vertices when the log factor changed. Indices
    /// never change (same topology), so only the VB is written.
    ///
    /// Invariant: this is an **in-place rewrite** of `self.vb`, never a
    /// buffer swap — [`grid_vertex_count`] is factor-independent (equal
    /// byte length per depth), so the queued `write_buffer` always fits
    /// the buffer built at construction. `draw`/`draw_surface` capture
    /// the *current* `self.vb` every frame and vertex buffers bind on the
    /// render pass (not through the bind group — the bind group only
    /// references uniform + history/ramp textures), so there is no stale
    /// handle: the new x positions take effect on the next pass.
    /// Buffer handles are exchanged only in [`Self::set_depth`], which
    /// rebuilds the whole grid + index + bind set in one atomic update.
    pub fn set_log_factor(&mut self, queue: &wgpu::Queue, device: &wgpu::Device, factor: f32) {
        if !needs_rebuild(self.grid_log_factor, factor) {
            return;
        }
        let verts = grid::grid_vertices(factor, self.grid_depth, self.grid_bins);
        queue.write_buffer(&self.vb, 0, bytemuck::cast_slice(&verts));
        self.grid_log_factor = factor;
        let _ = device;
    }

    /// Rebuilds the grid + index buffers and rebinds the *new* history
    /// texture view for a changed history depth. Called after swapping in
    /// a fresh `History` — see `Renderer::set_depth`. The grid is built
    /// with the current log factor, so the two knobs compose.
    ///
    /// This is the ONLY method that exchanges buffer handles (`vb`, `ib`,
    /// `surface_ib`) and rebuilds the bind group, and it does all of it
    /// before any field the draw calls read goes stale: after the return,
    /// `draw`/`draw_surface` see the new buffers, new index counts, and a
    /// bind group binding the new history view together with the
    /// unchanged uniform buffer and ramp view. `grid_log_factor` is
    /// preserved across the swap so a following `set_log_factor` compares
    /// against the factor the new grid was actually built with.
    pub fn set_depth(
        &mut self,
        device: &wgpu::Device,
        depth: usize,
        history_view: &wgpu::TextureView,
    ) {
        if depth == self.grid_depth {
            return;
        }
        let (vb, ib, index_count) =
            create_grid_buffers(device, self.grid_log_factor, depth, self.grid_bins);
        let surface_index_list = grid::surface_indices(depth, self.grid_bins);
        self.surface_ib = create_buffer_from(
            device,
            "neopeek-waterfall-surface-indices",
            bytemuck::cast_slice(&surface_index_list),
            wgpu::BufferUsages::INDEX,
        );
        self.surface_index_count = surface_index_list.len() as u32;
        self.vb = vb;
        self.ib = ib;
        self.index_count = index_count;
        self.bind_group =
            grid_bind_group(device, &self.bgl, &self.uniform, history_view, &self.ramp_view);
        self.grid_depth = depth;
    }

    /// Rebuilds the grid + index buffers for a changed FFT bin count
    /// (called after the engine switched size — see
    /// `Renderer::set_fft_size`). Unlike [`Self::set_depth`] this needs
    /// **no** bind-group rebuild: the history texture is allocated at
    /// `MAX_BINS` width once and only the first `bins` columns are
    /// written/read, so the texture view stays valid. The grid is built
    /// with the current log factor, so the knobs compose.
    pub fn set_fft_size(&mut self, device: &wgpu::Device, bins: usize) {
        if bins == self.grid_bins {
            return;
        }
        let (vb, ib, index_count) =
            create_grid_buffers(device, self.grid_log_factor, self.grid_depth, bins);
        let surface_index_list = grid::surface_indices(self.grid_depth, bins);
        self.surface_ib = create_buffer_from(
            device,
            "neopeek-waterfall-surface-indices",
            bytemuck::cast_slice(&surface_index_list),
            wgpu::BufferUsages::INDEX,
        );
        self.surface_index_count = surface_index_list.len() as u32;
        self.vb = vb;
        self.ib = ib;
        self.index_count = index_count;
        self.grid_bins = bins;
    }

    pub fn upload_uniform(&self, queue: &wgpu::Queue, uniforms: &WaterUniforms) {
        queue.write_buffer(&self.uniform, 0, bytemuck::bytes_of(uniforms));
    }

    pub fn draw<'pass>(&'pass self, pass: &mut wgpu::RenderPass<'pass>) {
        pass.set_pipeline(&self.pipeline);
        pass.set_bind_group(0, &self.bind_group, &[]);
        pass.set_vertex_buffer(0, self.vb.slice(..));
        pass.set_index_buffer(self.ib.slice(..), wgpu::IndexFormat::Uint32);
        pass.draw_indexed(0..self.index_count, 0, 0..1);
    }

    /// Filled-surface variant (`S`, M8): same bind group/grid, triangles.
    pub fn draw_surface<'pass>(&'pass self, pass: &mut wgpu::RenderPass<'pass>) {
        pass.set_pipeline(&self.surface_pipeline);
        pass.set_bind_group(0, &self.bind_group, &[]);
        pass.set_vertex_buffer(0, self.vb.slice(..));
        pass.set_index_buffer(self.surface_ib.slice(..), wgpu::IndexFormat::Uint32);
        pass.draw_indexed(0..self.surface_index_count, 0, 0..1);
    }
}

fn create_grid_buffers(
    device: &wgpu::Device,
    log_factor: f32,
    depth: usize,
    bins: usize,
) -> (wgpu::Buffer, wgpu::Buffer, u32) {
    let verts = grid::grid_vertices(log_factor, depth, bins);
    let indices = grid::grid_indices(depth, bins);
    let index_count = indices.len() as u32;
    let vert_bytes: &[u8] = bytemuck::cast_slice(&verts);
    let index_bytes: &[u8] = bytemuck::cast_slice(&indices);

    let vb = create_buffer_from(
        device,
        "neopeek-waterfall-grid",
        vert_bytes,
        wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::COPY_DST,
    );
    let ib = create_buffer_from(
        device,
        "neopeek-waterfall-indices",
        index_bytes,
        wgpu::BufferUsages::INDEX | wgpu::BufferUsages::COPY_DST,
    );

    (vb, ib, index_count)
}

/// Uniform + history texture + theme ramp; rebuilt on `set_depth` because
/// the history view (texture) is swapped.
fn grid_bind_group(
    device: &wgpu::Device,
    bgl: &wgpu::BindGroupLayout,
    uniform: &wgpu::Buffer,
    history_view: &wgpu::TextureView,
    ramp_view: &wgpu::TextureView,
) -> wgpu::BindGroup {
    device.create_bind_group(&wgpu::BindGroupDescriptor {
        label: Some("neopeek-waterfall-bindgroup"),
        layout: bgl,
        entries: &[
            wgpu::BindGroupEntry {
                binding: 0,
                resource: uniform.as_entire_binding(),
            },
            wgpu::BindGroupEntry {
                binding: 1,
                resource: wgpu::BindingResource::TextureView(history_view),
            },
            wgpu::BindGroupEntry {
                binding: 2,
                resource: wgpu::BindingResource::TextureView(ramp_view),
            },
        ],
    })
}

fn create_buffer_from(
    device: &wgpu::Device,
    label: &str,
    bytes: &[u8],
    usage: wgpu::BufferUsages,
) -> wgpu::Buffer {
    let buf = device.create_buffer(&wgpu::BufferDescriptor {
        label: Some(label),
        size: bytes.len() as u64,
        usage,
        mapped_at_creation: true,
    });
    buf.slice(..).get_mapped_range_mut().unwrap().copy_from_slice(bytes);
    buf.unmap();
    buf
}

/// Pure grid-rebuild decision for [`Waterfall::set_log_factor`], kept
/// side-effect-free so the state machine is unit-testable headlessly (the
/// build env has no GPU control plane; the device path is re-verified by
/// clippy + the host flake build). The grid is re-uploaded exactly when
/// the incoming factor differs from the factor the currently bound
/// vertex buffer was built with — clamped 0.0 (linear axis)…8.0 are
/// values the `c`/`v` keys genuinely reach (see `views::adjust_log_factor`).
fn needs_rebuild(current: f32, incoming: f32) -> bool {
    incoming != current
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Zeroing must produce a valid (parseable) uniform image; actual
    /// offset/size correctness is pinned against naga's reflection of the
    /// WGSL struct in `tests/wgsl_layout.rs`.
    #[test]
    fn zeroed_uniform_is_pod_message_free() {
        let u = WaterUniforms::zeroed_view_proj();
        assert_eq!(u.head, 0, "zeroed means head/row 0");
        assert_eq!(u.db, 0.0);
        assert!(u._align_hole.iter().all(|v| *v == 0.0) && u.pad.iter().all(|v| *v == 0.0));
    }

    #[test]
    fn needs_rebuild_smokes() {
        assert!(!needs_rebuild(1.5, 1.5), "1.5 → 1.5 is the no-op window");
        assert!(!needs_rebuild(8.0, 8.0), "ceiling key press");
        assert!(!needs_rebuild(0.0, 0.0), "pinned floor (`v` on exact zero)");
        assert!(needs_rebuild(0.0, 8.0), "clamped-domain extremes rebuild");
        assert!(needs_rebuild(1.5, 1.725), "a single c press");
        assert!(needs_rebuild(0.0, 0.05), "stepping off the exact linear axis");
        assert!(needs_rebuild(8.0, 0.0), "collapsing back to fully linear");
    }

    /// Drive the real `c`/`v` settings machine over its whole reachable
    /// domain: every value it produces must demand a rebuild relative to
    /// the previous value, ceiling/floor extraneous presses are the only
    /// no-ops, and the grid content ends up different from the start —
    /// i.e. the keybind's INFO value genuinely reaches the vertex buffer.
    #[test]
    fn log_factor_machine_rebuilds_and_moves_the_grid() {
        let mut s = crate::views::ViewSettings::default();
        let start = s.log_factor;
        let mut prev = start;
        let mut rebuilt = 0usize;
        for _ in 0..64 {
            s.adjust_log_factor(true); // `c`: ×1.15, clamped at 8.0
            rebuilt += usize::from(needs_rebuild(prev, s.log_factor));
            prev = s.log_factor;
        }
        assert!((s.log_factor - 8.0).abs() < 1e-4, "clamped at {:?}", s.log_factor);
        // Snappy steps: ~12 presses traverse the whole 1.5…8.0 range.
        assert!(rebuilt > 10, "c presses rebuild until the ceiling: {rebuilt}");
        assert!(!needs_rebuild(prev, s.log_factor), "ceiling presses stay silent");
        while s.log_factor > 0.0 {
            s.adjust_log_factor(false); // `v`: ×0.87, snaps to 0.0 below 0.05
        }
        assert!(needs_rebuild(prev, s.log_factor), "the exact linear axis is a change");
        prev = s.log_factor;
        s.adjust_log_factor(false);
        assert!(!needs_rebuild(prev, s.log_factor), "floor is pinned");
        s.adjust_log_factor(true);
        assert!(needs_rebuild(prev, s.log_factor), "0.0 → 0.05 steps off the floor");
        // End-state proof: the grid the waterfall would now upload differs
        // in vertex data from the start grid (same length — see grid tests).
        let start_grid = grid::grid_vertices(start, crate::grid::DEFAULT_DEPTH, neopeek_dsp::DEFAULT_BINS);
        let end_grid = grid::grid_vertices(s.log_factor, crate::grid::DEFAULT_DEPTH, neopeek_dsp::DEFAULT_BINS);
        assert!(start_grid.len() == end_grid.len() && start_grid != end_grid,
            "axis moved end-to-end at {:?}", s.log_factor);
    }
}
