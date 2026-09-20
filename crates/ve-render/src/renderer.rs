//! The compositor.

use bytemuck::{Pod, Zeroable};
use ve_core::{BlendMode, ColorSpace, Rgba, TransformState};
use ve_media::VideoFrame;
use ve_metrics::{spans, Metrics};

use crate::effects::{EffectPass, EffectRenderer};
use crate::target::RenderTarget;
use crate::texture::{view_format_for, GpuTexture, FRAME_FORMAT};
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
    pub blend: BlendMode,
}

impl<'a> Layer<'a> {
    /// A layer drawn at its natural size, centred, fully opaque, over whatever
    /// is beneath it.
    pub fn new(texture: &'a GpuTexture) -> Self {
        Layer { texture, transform: TransformState::default(), blend: BlendMode::Normal }
    }

    pub fn with_transform(mut self, transform: TransformState) -> Self {
        self.transform = transform;
        self
    }

    pub fn with_blend(mut self, blend: BlendMode) -> Self {
        self.blend = blend;
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
    /// One pipeline per (colour space, blend mode) pair, in [`ColorSpace::ALL`]
    /// then [`BlendMode::ALL`] order.
    ///
    /// The colour space is an axis here rather than a uniform because it is a
    /// property of the *attachment*: blending in linear light means rendering
    /// through an sRGB view, and a pipeline's target format has to match the
    /// view the pass attaches. Built up front rather than on demand — they
    /// share a shader module and differ only in blend state and target format,
    /// so the whole set costs a few milliseconds once, and a mode or space
    /// chosen mid-drag never stalls a frame compiling.
    /// Indexed by [`Renderer::pipeline_index`]: colour space, then whether the
    /// source is already premultiplied, then blend mode.
    pipelines: [wgpu::RenderPipeline; ColorSpace::ALL.len() * 2 * BlendMode::ALL.len()],
    /// One per (colour space, premultiplied) pair, for [`Renderer::accumulate`].
    /// Additive in every channel including alpha, which is what turns a pass of
    /// weighted draws into a mean rather than into a stack.
    averaging: [wgpu::RenderPipeline; ColorSpace::ALL.len() * 2],
    uniform_layout: wgpu::BindGroupLayout,
    texture_layout: wgpu::BindGroupLayout,
    /// The pipelines effect passes are drawn with. Held here because a pass
    /// samples the same textures the compositor does, through the same layout
    /// and sampler.
    effects: EffectRenderer,
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

/// The blend state for a mode, against a **premultiplied** source.
///
/// Each mode is a weighted sum of source and destination that the fixed-function
/// blender can evaluate, so a mode costs a pipeline and nothing else. Writing
/// `Cs` for the premultiplied source colour, `As` for its alpha and `Cd` for what
/// is already in the target:
///
/// * `Normal`: `Cs + Cd(1 - As)` — source over destination.
/// * `Add`: `Cs + Cd`.
/// * `Multiply`: `Cs·Cd + Cd(1 - As)`. Exact over an opaque backdrop, which is
///   what a sequence with an opaque background gives; the general Porter-Duff
///   form also scales by the backdrop's alpha, which needs the destination as a
///   texture rather than as a blend factor.
/// * `Screen`: `Cs + Cd(1 - Cs)`, which is `Cs + Cd - Cs·Cd` — the inverse of
///   multiplying the inverses.
///
/// Alpha is the union of coverages in every mode: a blend mode says how colour
/// combines, not how much of the frame the layer covers.
fn blend_state(mode: BlendMode) -> wgpu::BlendState {
    let colour = match mode {
        BlendMode::Normal => wgpu::BlendComponent {
            src_factor: wgpu::BlendFactor::One,
            dst_factor: wgpu::BlendFactor::OneMinusSrcAlpha,
            operation: wgpu::BlendOperation::Add,
        },
        BlendMode::Add => wgpu::BlendComponent {
            src_factor: wgpu::BlendFactor::One,
            dst_factor: wgpu::BlendFactor::One,
            operation: wgpu::BlendOperation::Add,
        },
        BlendMode::Multiply => wgpu::BlendComponent {
            src_factor: wgpu::BlendFactor::Dst,
            dst_factor: wgpu::BlendFactor::OneMinusSrcAlpha,
            operation: wgpu::BlendOperation::Add,
        },
        BlendMode::Screen => wgpu::BlendComponent {
            src_factor: wgpu::BlendFactor::One,
            dst_factor: wgpu::BlendFactor::OneMinusSrc,
            operation: wgpu::BlendOperation::Add,
        },
    };
    wgpu::BlendState {
        color: colour,
        alpha: wgpu::BlendComponent {
            src_factor: wgpu::BlendFactor::One,
            dst_factor: wgpu::BlendFactor::OneMinusSrcAlpha,
            operation: wgpu::BlendOperation::Add,
        },
    }
}

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

        // Everything but the blend state and the target format is identical,
        // and the shader module is shared, so the driver compiles the same
        // program once and varies the fixed-function state around it.
        //
        // A target created with some other format — the readback tests use one
        // — cannot be reinterpreted as sRGB, so its pipelines keep that format
        // for both spaces and the linear pair simply never gets used.
        let reinterpretable = format == FRAME_FORMAT;
        let mut built = Vec::with_capacity(ColorSpace::ALL.len() * 2 * BlendMode::ALL.len());
        for space in ColorSpace::ALL {
            let target_format = if reinterpretable { view_format_for(space) } else { format };
            // Two fragment shaders, because a decoded frame and the output of
            // another pass carry their alpha differently. See
            // [`GpuTexture::is_premultiplied`].
            for entry in ["fs_main", "fs_premultiplied"] {
                for mode in BlendMode::ALL {
                    built.push(device.create_render_pipeline(
                        &wgpu::RenderPipelineDescriptor {
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
                                entry_point: Some(entry),
                                targets: &[Some(wgpu::ColorTargetState {
                                    format: target_format,
                                    // Premultiplied throughout: the shader already
                                    // multiplied colour by alpha, so every mode's source
                                    // factor works on a premultiplied value. See
                                    // [`blend_state`].
                                    blend: Some(blend_state(mode)),
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
                        },
                    ));
                }
            }
        }
        let pipelines: [wgpu::RenderPipeline;
            ColorSpace::ALL.len() * 2 * BlendMode::ALL.len()] = built
            .try_into()
            .expect("one pipeline per colour space, source kind and blend mode");

        // The averaging pipelines differ from the rest only in blend state:
        // `Cs + Cd` in colour *and* alpha, against a target cleared to
        // transparent. Summing weighted samples that way is a mean; the `over`
        // blending every other mode uses is not, because each sample would
        // occlude the ones before it and a fully opaque layer would come out
        // partly transparent.
        let mut averaging_built = Vec::with_capacity(ColorSpace::ALL.len() * 2);
        for space in ColorSpace::ALL {
            for entry in ["fs_main", "fs_premultiplied"] {
                let target_format =
                    if reinterpretable { view_format_for(space) } else { format };
                averaging_built.push(device.create_render_pipeline(
                    &wgpu::RenderPipelineDescriptor {
                        label: Some("verge-average-pipeline"),
                        layout: Some(&pipeline_layout),
                        vertex: wgpu::VertexState {
                            module: &shader,
                            entry_point: Some("vs_main"),
                            buffers: &[],
                            compilation_options: Default::default(),
                        },
                        fragment: Some(wgpu::FragmentState {
                            module: &shader,
                            entry_point: Some(entry),
                            targets: &[Some(wgpu::ColorTargetState {
                                format: target_format,
                                blend: Some(wgpu::BlendState {
                                    color: wgpu::BlendComponent {
                                        src_factor: wgpu::BlendFactor::One,
                                        dst_factor: wgpu::BlendFactor::One,
                                        operation: wgpu::BlendOperation::Add,
                                    },
                                    alpha: wgpu::BlendComponent {
                                        src_factor: wgpu::BlendFactor::One,
                                        dst_factor: wgpu::BlendFactor::One,
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
                            cull_mode: None,
                            unclipped_depth: false,
                            polygon_mode: wgpu::PolygonMode::Fill,
                            conservative: false,
                        },
                        depth_stencil: None,
                        multisample: wgpu::MultisampleState::default(),
                        multiview_mask: None,
                        cache: None,
                    },
                ));
            }
        }
        let averaging: [wgpu::RenderPipeline; ColorSpace::ALL.len() * 2] = averaging_built
            .try_into()
            .expect("one averaging pipeline per colour space and source kind");

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

        let effects = EffectRenderer::new(device, &texture_layout, format);

        Renderer {
            pipelines,
            averaging,
            uniform_layout,
            texture_layout,
            effects,
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

    /// Wraps a render target so it can be drawn as a layer.
    ///
    /// This is what makes a nested composition a layer like any other: the
    /// target it was drawn into is bound with the same layout and sampler a
    /// decoded frame gets, and the compositor cannot tell the difference.
    ///
    /// `content` identifies what is *in* the target — a target is reused from
    /// frame to frame, so its own identity would tell the render cache nothing.
    pub fn bind_target(
        &self,
        device: &wgpu::Device,
        target: &RenderTarget,
        content: crate::cache::CompositeKey,
    ) -> GpuTexture {
        GpuTexture::wrap_target(device, &self.texture_layout, &self.sampler, target, content)
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
        color_space: ColorSpace,
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
                    // Attaching the sRGB view is what puts the blender in
                    // linear light: the hardware decodes the destination and
                    // encodes the result, so the arithmetic in between is on
                    // light rather than on codes.
                    view: target.view_for(color_space),
                    depth_slice: None,
                    resolve_target: None,
                    ops: wgpu::Operations {
                        // Clearing to a premultiplied background keeps the
                        // target consistent with what the layers write.
                        // A clear value is written without the sRGB encode the
                        // attachment applies to shader output, so it has to be
                        // supplied in the space the target stores. Encoding it
                        // here keeps a linear composition's background the
                        // colour the user picked.
                        load: wgpu::LoadOp::Clear(clear_colour(background, color_space)),
                        store: wgpu::StoreOp::Store,
                    },
                })],
                depth_stencil_attachment: None,
                timestamp_writes: None,
                occlusion_query_set: None,
                multiview_mask: None,
            });

            // The pipeline is set only when the mode changes, so a composition
            // that is all one mode — nearly all of them — still binds once for
            // the whole pass. Layers keep their back-to-front order rather than
            // being grouped by mode: blending is not commutative, so reordering
            // to save a bind would change the picture.
            let mut bound: Option<usize> = None;
            for (i, layer) in layers.iter().enumerate() {
                let index = Self::pipeline_index(
                    color_space,
                    layer.texture.is_premultiplied(),
                    layer.blend,
                );
                if bound != Some(index) {
                    pass.set_pipeline(&self.pipelines[index]);
                    bound = Some(index);
                }
                let offset = (i as u32) * self.uniform_stride;
                pass.set_bind_group(0, &self.uniform_bind_group, &[offset]);
                pass.set_bind_group(1, layer.texture.bind_group(color_space), &[]);
                pass.draw(0..4, 0..1);
            }
        }
        queue.submit(Some(encoder.finish()));
    }

    /// Averages `samples` into `target`: one picture, exposed across a shutter.
    ///
    /// Each sample is the same texture under a different transform — where the
    /// layer was at one instant while the shutter was open — and what comes out
    /// is their **mean**, premultiplied, over a transparent target. The node
    /// above then draws that mean once, with its own blend mode, exactly as it
    /// would draw an unblurred layer.
    ///
    /// # Why an average, and why into a target of its own
    ///
    /// Motion blur is the mean of what the sensor saw while the shutter was
    /// open. Drawing the samples straight onto the backdrop at `1/n` opacity
    /// each is not that: `over` blending makes every sample occlude the ones
    /// before it, so a fully opaque layer comes out about 63% opaque, and the
    /// backdrop gets mixed into the smear instead of being composited under it.
    ///
    /// Summing weighted samples additively onto transparency is the mean, and
    /// the result is a layer like any other. That costs one target per blurred
    /// layer, which is what the render cache is for: the samples are part of
    /// the key, so an unchanged blur is not recomputed.
    ///
    /// The weight is applied here rather than by the caller, because "average
    /// these" is the whole contract: a caller that had to remember to divide by
    /// `n` would be one bug away from a layer that glows.
    pub fn accumulate(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        target: &RenderTarget,
        color_space: ColorSpace,
        samples: &[Layer<'_>],
    ) {
        let _span = self.metrics.as_ref().map(|m| m.span(spans::COMPOSITE));
        if samples.is_empty() {
            return;
        }
        if samples.len() > self.uniform_capacity {
            self.grow_uniform_buffer(device, samples.len());
        }

        let weight = 1.0 / samples.len() as f32;
        let composition = target.size();
        let stride = self.uniform_stride as usize;
        let mut staging = vec![0u8; stride * samples.len()];
        for (i, sample) in samples.iter().enumerate() {
            let uniform = LayerUniform {
                transform: layer_matrix(sample.texture.size(), composition, &sample.transform),
                opacity: sample.transform.opacity.clamp(0.0, 1.0) as f32 * weight,
                _padding: [0.0; 3],
            };
            let bytes = bytemuck::bytes_of(&uniform);
            staging[i * stride..i * stride + bytes.len()].copy_from_slice(bytes);
        }
        queue.write_buffer(&self.uniform_buffer, 0, &staging);

        let mut encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
            label: Some("verge-motion-blur-encoder"),
        });
        {
            let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("verge-motion-blur-pass"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: target.view_for(color_space),
                    depth_slice: None,
                    resolve_target: None,
                    ops: wgpu::Operations {
                        // Transparent, not the canvas background: this target
                        // holds one layer's own picture, which whatever is
                        // beneath it shows through.
                        load: wgpu::LoadOp::Clear(wgpu::Color::TRANSPARENT),
                        store: wgpu::StoreOp::Store,
                    },
                })],
                depth_stencil_attachment: None,
                timestamp_writes: None,
                occlusion_query_set: None,
                multiview_mask: None,
            });

            // Every sample is the same texture, so the source kind is settled
            // once for the whole pass.
            let premultiplied = samples[0].texture.is_premultiplied();
            pass.set_pipeline(
                &self.averaging[color_space as usize * 2 + premultiplied as usize],
            );
            for (i, sample) in samples.iter().enumerate() {
                let offset = (i as u32) * self.uniform_stride;
                pass.set_bind_group(0, &self.uniform_bind_group, &[offset]);
                pass.set_bind_group(1, sample.texture.bind_group(color_space), &[]);
                pass.draw(0..4, 0..1);
            }
        }
        queue.submit(Some(encoder.finish()));
    }

    /// Draws `source` through one effect pass into `target`.
    ///
    /// One pass, one target: a chain is run by calling this once per pass, with
    /// each output becoming the next input. The ping-pong belongs to the caller
    /// because it is the caller that owns the pool the targets come from and
    /// the cache a pass's output may already be sitting in.
    ///
    /// `target` is cleared and completely overwritten, so it can be a used one
    /// from the pool.
    pub fn apply_effect(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        target: &RenderTarget,
        color_space: ColorSpace,
        source: &GpuTexture,
        pass: &EffectPass,
    ) {
        let _span = self.metrics.as_ref().map(|m| m.span(spans::EFFECT));
        self.effects.run(device, queue, target, color_space, source, pass);
    }

    /// Where the pipeline for one (colour space, source kind, blend mode) sits.
    fn pipeline_index(space: ColorSpace, premultiplied: bool, mode: BlendMode) -> usize {
        let mode_index = BlendMode::ALL.iter().position(|m| *m == mode).unwrap_or(0);
        ((space as usize * 2) + premultiplied as usize) * BlendMode::ALL.len() + mode_index
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

/// The clear colour to hand a pass, in the space the attachment expects.
///
/// A clear value for an sRGB attachment is specified in **linear** light and
/// encoded by the hardware, exactly as shader output is. The background stored
/// on a sequence is a colour someone picked in a colour well, so it is an
/// encoded value like `#808080`; handing that straight to a linear pass would
/// have the hardware encode it a second time and wash it out.
///
/// So the conversion here decodes rather than encodes, which is what makes the
/// same background swatch look the same in both spaces. Alpha is applied after
/// the decode, because premultiplication has to happen in whichever space the
/// blending will.
fn clear_colour(background: Rgba, color_space: ColorSpace) -> wgpu::Color {
    let channel = |value: f64| match color_space {
        ColorSpace::Perceptual => value * background.a,
        ColorSpace::Linear => srgb_to_linear(value) * background.a,
    };
    wgpu::Color {
        r: channel(background.r),
        g: channel(background.g),
        b: channel(background.b),
        a: background.a,
    }
}

/// The inverse sRGB transfer function, matching what the hardware applies when
/// it samples an sRGB view.
fn srgb_to_linear(channel: f64) -> f64 {
    let c = channel.clamp(0.0, 1.0);
    if c <= 0.040_45 {
        c / 12.92
    } else {
        ((c + 0.055) / 1.055).powf(2.4)
    }
}
