//! The waterfall pipeline: static grid VB/IB + history texture + uniforms
//! (PLAN.md §4.1, §4.2 pass 1). Per rendered frame the CPU work is one
//! uniform write + a ~4 KB texture row per *new* slice — no allocation.

use crate::grid::{self, DEPTH};

const SHADER: &str = include_str!("shaders/waterfall.wgsl");

/// Uniform block matching `shaders/waterfall.wgsl`.
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
            ],
        });

        Self {
            pipeline,
            vb,
            ib,
            uniform,
            bind_group,
            index_count,
            grid_log_factor: log_factor,
        }
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

    let vb = device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("neopeek-waterfall-grid"),
        size: vert_bytes.len() as u64,
        usage: wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::COPY_DST,
        mapped_at_creation: true,
    });
    vb.slice(..).get_mapped_range_mut().unwrap().copy_from_slice(vert_bytes);
    vb.unmap();

    let ib = device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("neopeek-waterfall-indices"),
        size: index_bytes.len() as u64,
        usage: wgpu::BufferUsages::INDEX | wgpu::BufferUsages::COPY_DST,
        mapped_at_creation: true,
    });
    ib.slice(..).get_mapped_range_mut().unwrap().copy_from_slice(index_bytes);
    ib.unmap();

    (vb, ib, index_count)
}
