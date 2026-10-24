//! Waveform / Lissajous line-strip pipeline (PLAN.md §4.2 pass 2): one
//! small dynamic VB per pipeline, uploaded per frame (~≤48 KB, trivial),
//! uniform color per instantiation. No allocation on the hot path.

const SHADER: &str = include_str!("shaders/line.wgsl");

#[repr(C)]
#[derive(Debug, Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
pub struct LineUniforms {
    pub view_proj: [f32; 16],
    pub color: [f32; 4],
}

pub struct LinePipeline {
    pipeline: wgpu::RenderPipeline,
    vb: wgpu::Buffer,
    uniform: wgpu::Buffer,
    bind_group: wgpu::BindGroup,
}

impl LinePipeline {
    /// Creates an instance with a fixed color; `max_vertices` sizes the
    /// dynamic VB once (1024 waveform + 1024 lissajous → 2048 total, but
    /// each instance draws its own strip).
    pub fn new(
        device: &wgpu::Device,
        format: wgpu::TextureFormat,
        depth_format: wgpu::TextureFormat,
        personality: &str,
        max_vertices: u64,
    ) -> Self {
        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some(&format!("neopeek-{personality}")),
            source: wgpu::ShaderSource::Wgsl(SHADER.into()),
        });

        let bgl = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some(&format!("neopeek-{personality}-bgl")),
            entries: &[wgpu::BindGroupLayoutEntry {
                binding: 0,
                visibility: wgpu::ShaderStages::VERTEX,
                ty: wgpu::BindingType::Buffer {
                    ty: wgpu::BufferBindingType::Uniform,
                    has_dynamic_offset: false,
                    min_binding_size: None,
                },
                count: None,
            }],
        });

        let layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some(&format!("neopeek-{personality}-layout")),
            bind_group_layouts: &[Some(&bgl)],
            immediate_size: 0,
        });

        let pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some(&format!("neopeek-{personality}")),
            layout: Some(&layout),
            vertex: wgpu::VertexState {
                module: &shader,
                entry_point: Some("vs_main"),
                buffers: &[Some(wgpu::VertexBufferLayout {
                    array_stride: 12,
                    step_mode: wgpu::VertexStepMode::Vertex,
                    attributes: &[wgpu::VertexAttribute {
                        shader_location: 0,
                        format: wgpu::VertexFormat::Float32x3,
                        offset: 0,
                    }],
                })],
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
                // M8: width-expanded quad strips (2 verts per point).
                topology: wgpu::PrimitiveTopology::TriangleStrip,
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

        let vb = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some(&format!("neopeek-{personality}-vb")),
            size: max_vertices * 12,
            usage: wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });

        let uniform = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some(&format!("neopeek-{personality}-uniforms")),
            size: std::mem::size_of::<LineUniforms>() as u64,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });

        let bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some(&format!("neopeek-{personality}-bindgroup")),
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
        }
    }

    pub fn set(&self, queue: &wgpu::Queue, view_proj: &[f32; 16], color: [f32; 3]) {
        let u = LineUniforms {
            view_proj: *view_proj,
            color: [color[0], color[1], color[2], 1.0],
        };
        queue.write_buffer(&self.uniform, 0, bytemuck::bytes_of(&u));
    }

    /// Uploads `verts` and draws them as one triangle-strip quad run
    /// (2 verts per polyline point — see `views::append_waveform`).
    pub fn draw_strip<'pass>(
        &self,
        pass: &mut wgpu::RenderPass<'pass>,
        queue: &wgpu::Queue,
        verts: &[crate::views::LineVertex],
    ) {
        if verts.len() < 2 {
            return;
        }
        queue.write_buffer(&self.vb, 0, bytemuck::cast_slice(verts));
        pass.set_pipeline(&self.pipeline);
        pass.set_bind_group(0, &self.bind_group, &[]);
        pass.set_vertex_buffer(0, self.vb.slice(..));
        pass.draw(0..verts.len() as u32, 0..1);
    }
}
