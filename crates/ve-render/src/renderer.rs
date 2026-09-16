//! The compositor.

use bytemuck::{Pod, Zeroable};
use ve_core::{Rgba, TransformState};
use ve_media::VideoFrame;
use ve_metrics::{spans, Metrics};

use crate::target::RenderTarget;
use crate::texture::{GpuTexture, FRAME_FORMAT};
use crate::transform::{layer_matrix, Matrix4};

/// Per-layer data handed to the shader.
///
/// `repr(C)` and `Pod` so it can be written into a uniform buffer directly.
/// The explicit padding matches WGSL's `vec3<f32>` alignment rule.
#[repr(C)]
#[derive(Debug, Clone, Copy, Pod, Zeroable)]
struct LayerUniform {
    transform: Matrix4,
    opacity: f32,
    _padding: [f32; 3],
}

/// One thing to draw, in back-to-front order.
pub struct Layer<'a> {
    pub texture: &'a GpuTexture,
    pub transform: TransformState,
}

impl<'a> Layer<'a> {
    /// A layer drawn at its natural size, centred, fully opaque.
    pub fn new(texture: &'a GpuTexture) -> Self {
        Layer { texture, transform: TransformState::default() }
    }

    pub fn with_transform(mut self, transform: TransformState) -> Self {
        self.transform = transform;
        self
    }
}

/// Composites layers into a [`RenderTarget`].
///
/// Holds the pipeline, the sampler and a growable uniform buffer. Layers are
/// drawn back to front with alpha blending, which is the model every later
/// feature extends: blend modes become pipeline variants, effects become passes
/// between the layer draws, and a nested composition becomes a layer whose
/// texture is another target's output.
pub struct Renderer {
    pipeline: wgpu::RenderPipeline,
    uniform_layout: wgpu::BindGroupLayout,
    texture_layout: wgpu::BindGroupLayout,
    sampler: wgpu::Sampler,
    uniform_buffer: wgpu::Buffer,
    uniform_bind_group: wgpu::BindGroup,
    /// Layers the current buffer can hold before it has to grow.
    uniform_capacity: usize,
    /// Distance between consecutive uniforms, which the device dictates.
    uniform_stride: u32,
    format: wgpu::TextureFormat,
    metrics: Option<Metrics>,
}

/// How many layers the uniform buffer starts out able to hold. Grows on demand;
/// most compositions never exceed it.
const INITIAL_LAYER_CAPACITY: usize = 32;

impl Renderer {
    pub fn new(device: &wgpu::Device) -> Self {
        Self::with_format(device, FRAME_FORMAT)
    }

    pub fn with_format(device: &wgpu::Device, format: wgpu::TextureFormat) -> Self {
        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("verge-composite-shader"),
            source: wgpu::ShaderSource::Wgsl(include_str!("shader.wgsl").into()),
        });

        let uniform_layout =
            device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
                label: Some("verge-layer-uniform-layout"),
                entries: &[wgpu::BindGroupLayoutEntry {
                    binding: 0,
                    visibility: wgpu::ShaderStages::VERTEX_FRAGMENT,
                    ty: wgpu::BindingType::Buffer {
                        ty: wgpu::BufferBindingType::Uniform,
                        // One buffer, one bind group, addressed per layer.
                        has_dynamic_offset: true,
                        min_binding_size: wgpu::BufferSize::new(
                            std::mem::size_of::<LayerUniform>() as u64,
                        ),
                    },
                    count: None,
                }],
            });

        let texture_layout =
            device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
                label: Some("verge-layer-texture-layout"),
                entries: &[
                    wgpu::BindGroupLayoutEntry {
                        binding: 0,
                        visibility: wgpu::ShaderStages::FRAGMENT,
                        ty: wgpu::BindingType::Texture {
                            sample_type: wgpu::TextureSampleType::Float { filterable: true },
                            view_dimension: wgpu::TextureViewDimension::D2,
                            multisampled: false,
                        },
                        count: None,
                    },
                    wgpu::BindGroupLayoutEntry {
                        binding: 1,
                        visibility: wgpu::ShaderStages::FRAGMENT,
                        ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
                        count: None,
                    },
                ],
            });

        let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("verge-composite-pipeline-layout"),
            bind_group_layouts: &[Some(&uniform_layout), Some(&texture_layout)],
            immediate_size: 0,
        });

        let pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("verge-composite-pipeline"),
            layout: Some(&pipeline_layout),
            vertex: wgpu::VertexState {
                module: &shader,
                entry_point: Some("vs_main"),
                // No vertex buffers: the quad comes from the vertex index.
                buffers: &[],
                compilation_options: Default::default(),
            },
            fragment: Some(wgpu::FragmentState {
                module: &shader,
                entry_point: Some("fs_main"),
                targets: &[Some(wgpu::ColorTargetState {
                    format,
                    // Premultiplied "over": the shader already multiplied colour
                    // by alpha, so the source factor is One rather than SrcAlpha.
                    blend: Some(wgpu::BlendState {
                        color: wgpu::BlendComponent {
                            src_factor: wgpu::BlendFactor::One,
                            dst_factor: wgpu::BlendFactor::OneMinusSrcAlpha,
                            operation: wgpu::BlendOperation::Add,
                        },
                        alpha: wgpu::BlendComponent {
                            src_factor: wgpu::BlendFactor::One,
                            dst_factor: wgpu::BlendFactor::OneMinusSrcAlpha,
                            operation: wgpu::BlendOperation::Add,
                        },
                    }),
                    write_mask: wgpu::ColorWrites::ALL,
                })],
                compilation_options: Default::default(),
            }),
            primitive: wgpu::PrimitiveState {
                topology: wgpu::PrimitiveTopology::TriangleStrip,
                strip_index_format: None,
                front_face: wgpu::FrontFace::Ccw,
                // Layers are routinely mirrored by a negative scale, and a
                // mirrored layer must not disappear.
                cull_mode: None,
                unclipped_depth: false,
                polygon_mode: wgpu::PolygonMode::Fill,
                conservative: false,
            },
            depth_stencil: None,
            multisample: wgpu::MultisampleState::default(),
            multiview_mask: None,
            cache: None,
        });

        let sampler = device.create_sampler(&wgpu::SamplerDescriptor {
            label: Some("verge-frame-sampler"),
            // Clamp rather than repeat: a scaled-down layer must show the
            // background past its edges, not a tiled copy of itself.
            address_mode_u: wgpu::AddressMode::ClampToEdge,
            address_mode_v: wgpu::AddressMode::ClampToEdge,
            address_mode_w: wgpu::AddressMode::ClampToEdge,
            mag_filter: wgpu::FilterMode::Linear,
            min_filter: wgpu::FilterMode::Linear,
            mipmap_filter: wgpu::MipmapFilterMode::Nearest,
            ..Default::default()
        });

        let uniform_stride = Self::stride_for(device);
        let (uniform_buffer, uniform_bind_group) = Self::make_uniform_buffer(
            device,
            &uniform_layout,
            uniform_stride,
            INITIAL_LAYER_CAPACITY,
        );

        Renderer {
            pipeline,
            uniform_layout,
            texture_layout,
            sampler,
            uniform_buffer,
            uniform_bind_group,
            uniform_capacity: INITIAL_LAYER_CAPACITY,
            uniform_stride,
            format,
            metrics: None,
        }
    }

    pub fn with_metrics(mut self, metrics: Metrics) -> Self {
        self.metrics = Some(metrics);
        self
    }

    pub fn format(&self) -> wgpu::TextureFormat {
        self.format
    }

    /// Uploads a decoded frame to the GPU.
    pub fn upload(
        &self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        frame: &VideoFrame,
    ) -> GpuTexture {
        let _span = self.metrics.as_ref().map(|m| m.span(spans::UPLOAD));
        GpuTexture::upload(device, queue, &self.texture_layout, &self.sampler, frame)
    }

    /// Composites `layers` into `target`, back to front, over `background`.
    ///
    /// The whole composition is one render pass and one submit. Every layer
    /// shares a pipeline and a uniform buffer, so a ten-layer composite costs
    /// ten draw calls and no allocations.
    pub fn render(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        target: &RenderTarget,
        background: Rgba,
        layers: &[Layer<'_>],
    ) {
        let _span = self.metrics.as_ref().map(|m| m.span(spans::COMPOSITE));

        if layers.len() > self.uniform_capacity {
            self.grow_uniform_buffer(device, layers.len());
        }

        let composition = target.size();
        let stride = self.uniform_stride as usize;
        let mut staging = vec![0u8; stride * layers.len().max(1)];
        for (i, layer) in layers.iter().enumerate() {
            let uniform = LayerUniform {
                transform: layer_matrix(layer.texture.size(), composition, &layer.transform),
                opacity: layer.transform.opacity.clamp(0.0, 1.0) as f32,
                _padding: [0.0; 3],
            };
            let bytes = bytemuck::bytes_of(&uniform);
            staging[i * stride..i * stride + bytes.len()].copy_from_slice(bytes);
        }
        if !layers.is_empty() {
            queue.write_buffer(&self.uniform_buffer, 0, &staging);
        }

        let mut encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
            label: Some("verge-composite-encoder"),
        });
        {
            let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("verge-composite-pass"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: target.view(),
                    depth_slice: None,
                    resolve_target: None,
                    ops: wgpu::Operations {
                        // Clearing to a premultiplied background keeps the
                        // target consistent with what the layers write.
                        load: wgpu::LoadOp::Clear(wgpu::Color {
                            r: background.r * background.a,
                            g: background.g * background.a,
                            b: background.b * background.a,
                            a: background.a,
                        }),
                        store: wgpu::StoreOp::Store,
                    },
                })],
                depth_stencil_attachment: None,
                timestamp_writes: None,
                occlusion_query_set: None,
                multiview_mask: None,
            });

            pass.set_pipeline(&self.pipeline);
            for (i, layer) in layers.iter().enumerate() {
                let offset = (i as u32) * self.uniform_stride;
                pass.set_bind_group(0, &self.uniform_bind_group, &[offset]);
                pass.set_bind_group(1, &layer.texture.bind_group, &[]);
                pass.draw(0..4, 0..1);
            }
        }
        queue.submit(Some(encoder.finish()));
    }

    /// The uniform buffer's per-layer stride, which must satisfy the device's
    /// minimum dynamic-offset alignment (256 bytes on most hardware).
    fn stride_for(device: &wgpu::Device) -> u32 {
        let align = device.limits().min_uniform_buffer_offset_alignment.max(1);
        let size = std::mem::size_of::<LayerUniform>() as u32;
        size.div_ceil(align) * align
    }

    fn make_uniform_buffer(
        device: &wgpu::Device,
        layout: &wgpu::BindGroupLayout,
        stride: u32,
        capacity: usize,
    ) -> (wgpu::Buffer, wgpu::BindGroup) {
        let buffer = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("verge-layer-uniforms"),
            size: (stride as u64) * (capacity.max(1) as u64),
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("verge-layer-uniform-bind-group"),
            layout,
            entries: &[wgpu::BindGroupEntry {
                binding: 0,
                resource: wgpu::BindingResource::Buffer(wgpu::BufferBinding {
                    buffer: &buffer,
                    offset: 0,
                    size: wgpu::BufferSize::new(std::mem::size_of::<LayerUniform>() as u64),
                }),
            }],
        });
        (buffer, bind_group)
    }

    fn grow_uniform_buffer(&mut self, device: &wgpu::Device, needed: usize) {
        // Doubling keeps reallocation amortised over a session where layer
        // counts creep up as the edit grows.
        let capacity = needed.next_power_of_two().max(INITIAL_LAYER_CAPACITY);
        let (buffer, bind_group) = Self::make_uniform_buffer(
            device,
            &self.uniform_layout,
            self.uniform_stride,
            capacity,
        );
        self.uniform_buffer = buffer;
        self.uniform_bind_group = bind_group;
        self.uniform_capacity = capacity;
        log::debug!("layer uniform buffer grown to {capacity} layers");
    }
}
