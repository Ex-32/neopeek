//! The waterfall pipeline: static grid VB/IB + history texture + uniforms
//! (PLAN.md §4.1, §4.2 pass 1). Per rendered frame the CPU work is one
//! uniform write + a ~4 KB texture row per *new* slice — no allocation.

use crate::grid::{self, DEPTH};

const SHADER: &str = include_str!("shaders/waterfall.wgsl");

/// Uniform block matching `shaders/waterfall.wgsl` byte-for-byte (the
/// pad/pad_end fields mirror WGSL's vec3/vec4 16-byte alignment).
#[repr(C)]
#[derive(Debug, Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
pub struct WaterUniforms {
    /// view·proj, column-major.
    pub view_proj: [f32; 16],
    pub head: u32,
    pub depth: u32,
    pub spacing: f32,
    pub z_offset: f32,
    pub db: f32,      // 1.0 = dB magnitude mode
    pub rainbow: f32, // 1.0 = rainbow depth colors
    pub gain: f32,
    pub freq_scale: f32,
    pub drift: f32, // slice-fade animation factor 0…1
    /// Aligns the color vec3s to offset 112 (as WGSL does).
    pub pad: [f32; 3],
    /// Theme colors (PLAN.md §6.1); rgb used, a ignored.
    pub past_color: [f32; 4],
    pub future_color: [f32; 4],
    pub now_color: [f32; 4],
    /// dB magnitude mapping window (Ember matches the M4 formula).
    pub db_floor: f32,
    pub db_ceil: f32,
    /// 1.0 = filled-surface mode (`S`, M8).
    pub surface: f32,
    /// Brings the block to 176 bytes = WGSL struct size (align 16).
    pub pad_end: [f32; 1],
}

impl WaterUniforms {
    pub fn zeroed_view_proj() -> Self {
        Self {
            view_proj: [0.0; 16],
            head: 0,
            depth: DEPTH as u32,
            spacing: 0.12,
            z_offset: 0.0,
            db: 0.0,
            rainbow: 0.0,
            gain: 1.0,
            freq_scale: 1.0,
            drift: 0.0,
            pad: [0.0; 3],
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
    bind_group: wgpu::BindGroup,
    index_count: u32,
    /// Grid rebuilds when this differs from the incoming `log_factor`.
    grid_log_factor: f32,
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
                        min_binding_size: None,
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

        // Static grid, allocated once here; written again only on a
        // log-factor change (`set_log_factor`).
        let (vb, ib, index_count) = create_grid_buffers(device, log_factor);

        // Filled-surface mode (M8): same VB, triangle index buffer.
        let surface_index_list = grid::surface_indices();
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

        let bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("neopeek-waterfall-bindgroup"),
            layout: &bgl,
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
                    resource: wgpu::BindingResource::TextureView(&ramp_view),
                },
            ],
        });

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
            bind_group,
            index_count,
            grid_log_factor: log_factor,
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
    pub fn set_log_factor(&mut self, queue: &wgpu::Queue, device: &wgpu::Device, factor: f32) {
        if factor == self.grid_log_factor {
            return;
        }
        let verts = grid::grid_vertices(factor);
        queue.write_buffer(&self.vb, 0, bytemuck::cast_slice(&verts));
        self.grid_log_factor = factor;
        let _ = device;
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
) -> (wgpu::Buffer, wgpu::Buffer, u32) {
    let verts = grid::grid_vertices(log_factor);
    let indices = grid::grid_indices();
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

#[cfg(test)]
mod tests {
    use super::*;

    /// The Rust block must have exactly the WGSL-side size (176 = 11×16),
    /// else the uniform binding fails min-binding validation.
    #[test]
    fn uniforms_match_wgsl_layout() {
        assert_eq!(std::mem::size_of::<WaterUniforms>(), 176);
        assert_eq!(
            std::mem::offset_of!(WaterUniforms, past_color),
            16 * 7,
            "colors start after 11 vec4 slots (mat4 + 4 scalars + pad)"
        );
        assert_eq!(std::mem::offset_of!(WaterUniforms, db_floor), 160);
        assert_eq!(std::mem::offset_of!(WaterUniforms, surface), 168);
    }
}
