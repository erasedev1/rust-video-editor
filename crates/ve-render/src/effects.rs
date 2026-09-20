//! Running an effect chain on the GPU.
//!
//! # Dispatch lives here
//!
//! The edit model names an effect with a string and carries its parameters as
//! key/value pairs; the registry says what those parameters mean; and this
//! module is where a kind finally becomes a *program*. That is the whole of the
//! dynamic dispatch: [`passes_for`] matches on the registry key and packs the
//! numbers the matching shader entry point expects. An effect this build has no
//! program for produces no passes and is skipped, which is what lets a project
//! carrying a plugin's effect open and render everything else.
//!
//! Keeping the match here rather than in `ve-core` is deliberate. The parameter
//! *meanings* — that a blur has a radius in pixels — belong to the registry,
//! which the interface reads too. The parameter *packing* — that the radius
//! goes in `p0.x` and that a blur is two passes rather than one — belongs to
//! the shader, and the only way to keep those two in step is to write them
//! next to each other.
//!
//! # Layer space, not canvas space
//!
//! A chain runs on the clip's own picture, at the clip's own resolution, before
//! the clip's transform places it on the canvas. So a blur radius is in *source*
//! pixels and a mask is positioned in fractions of *the clip*, not of the
//! sequence.
//!
//! That is what every compositor does, and it is worth saying why. Effects that
//! ran after the transform would change as the clip moved: a mask would slide
//! off what it was cut around, and a blur would soften by a different amount as
//! a zoom went on. Running before means a chain is a property of the picture
//! and nothing else — which is also what lets the render cache keep the result
//! while the clip is moved around the canvas.
//!
//! # A pass at a time
//!
//! Each pass reads one texture and writes one target. The caller owns the
//! ping-pong between them, because it is the caller — the preview — that has
//! the target pool and the cache, and because a cached pass output can then be
//! taken mid-chain without this module knowing a cache exists.

use bytemuck::{Pod, Zeroable};
use ve_core::registry::kinds;
use ve_core::{ColorSpace, EffectState, Rgba, Size, Vec2};

use crate::target::RenderTarget;
use crate::texture::{view_format_for, GpuTexture, FRAME_FORMAT};

/// The shader programs an effect can dispatch to.
///
/// One per fragment entry point in `effects.wgsl`. Several registry kinds could
/// in principle share one — two blurs differing only in how their radius is
/// dialled would — so this is a list of *programs*, not a copy of the registry.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Program {
    Blur,
    Color,
    Sharpen,
    Transform,
    Mask,
    LumaKey,
}

impl Program {
    const ALL: [Program; 6] = [
        Program::Blur,
        Program::Color,
        Program::Sharpen,
        Program::Transform,
        Program::Mask,
        Program::LumaKey,
    ];

    fn entry_point(self) -> &'static str {
        match self {
            Program::Blur => "fs_blur",
            Program::Color => "fs_color",
            Program::Sharpen => "fs_sharpen",
            Program::Transform => "fs_transform",
            Program::Mask => "fs_mask",
            Program::LumaKey => "fs_luma",
        }
    }

    fn index(self) -> usize {
        Program::ALL.iter().position(|p| *p == self).expect("every program is listed")
    }
}

/// The uniform block every effect pass shares.
///
/// `repr(C)` and `Pod` so it is written into the buffer directly. The trailing
/// padding is not decoration: WGSL rounds a struct's size up to its alignment,
/// which is 16 here because of the `vec4`s, so the shader's `Effect` is 96
/// bytes and a Rust type of 88 would disagree with it about where the next one
/// starts.
#[repr(C)]
#[derive(Debug, Clone, Copy, PartialEq, Pod, Zeroable)]
pub struct EffectUniform {
    /// Sixteen floats whose meaning is the program's own. See `effects.wgsl`.
    params: [f32; 16],
    texel: [f32; 2],
    size: [f32; 2],
    premultiply: f32,
    _pad: [f32; 3],
}

impl EffectUniform {
    fn new(params: [f32; 16], size: Size) -> Self {
        let (w, h) = (size.width.max(1) as f32, size.height.max(1) as f32);
        EffectUniform {
            params,
            texel: [1.0 / w, 1.0 / h],
            size: [w, h],
            // Filled in when the pass runs, from the texture it is handed: the
            // same pass reads a decoded frame at the head of a chain and
            // another pass's output everywhere else.
            premultiply: 0.0,
            _pad: [0.0; 3],
        }
    }
}

/// One draw: a program and the numbers it takes.
///
/// An effect is not always one pass — a blur is two, one per axis — so a chain
/// is a flat list of these rather than a list of effects.
#[derive(Debug, Clone, PartialEq)]
pub struct EffectPass {
    program: Program,
    uniform: EffectUniform,
}

impl EffectPass {
    pub fn program(&self) -> Program {
        self.program
    }

    /// The bytes that decide what this pass draws, for a cache key.
    ///
    /// The whole uniform, so a parameter that reaches the shader cannot fail to
    /// reach the key — the same reason [`crate::CompositeKey`] hashes exactly
    /// what the compositor is given.
    pub(crate) fn uniform_bytes(&self) -> &[u8] {
        bytemuck::bytes_of(&self.uniform)
    }
}

/// The passes one resolved effect draws, at a source of `size`.
///
/// Empty means "nothing to draw": either this build has no program for the
/// effect, or the parameters make it a no-op — a blur of zero radius, a mask
/// that hides nothing. Skipping a no-op is not only an optimisation: each pass
/// costs a full-resolution target, and a chain of switched-to-neutral effects
/// should cost what no effects cost.
pub fn passes_for(effect: &EffectState, size: Size) -> Vec<EffectPass> {
    match effect.kind.as_str() {
        kinds::GAUSSIAN_BLUR => blur_passes(effect, size),
        kinds::COLOR_ADJUST => vec![color_pass(effect, size)],
        kinds::SHARPEN => sharpen_pass(effect, size).into_iter().collect(),
        kinds::TRANSFORM => transform_pass(effect, size).into_iter().collect(),
        kinds::SHAPE_MASK => mask_pass(effect, size).into_iter().collect(),
        kinds::LUMA_KEY => vec![luma_pass(effect, size)],
        _ => Vec::new(),
    }
}

/// Every pass a whole chain draws, in order.
pub fn chain_passes(effects: &[EffectState], size: Size) -> Vec<EffectPass> {
    effects.iter().flat_map(|e| passes_for(e, size)).collect()
}

/// How many samples a blur takes per side of its kernel.
///
/// The cost ceiling. Past the radius this covers exactly, the kernel is sampled
/// more sparsely rather than more expensively — a 200-pixel blur costs the same
/// as a 30-pixel one — which is the trade every real-time blur makes, and the
/// reason the parameter is called a radius rather than a promise.
const BLUR_TAPS: f32 = 32.0;

fn blur_passes(effect: &EffectState, size: Size) -> Vec<EffectPass> {
    let radius = effect.scalar("radius", 0.0) as f32;
    if radius <= 0.0 {
        return Vec::new();
    }
    // 0 both, 1 horizontal, 2 vertical. One axis is one pass, which is half the
    // cost — so the choice is worth having rather than being a preset.
    let directions: &[[f32; 2]] = match effect.choice("direction", 0) {
        1 => &[[1.0, 0.0]],
        2 => &[[0.0, 1.0]],
        _ => &[[1.0, 0.0], [0.0, 1.0]],
    };
    directions
        .iter()
        .map(|d| {
            let mut params = [0.0f32; 16];
            params[0] = radius;
            params[1] = d[0];
            params[2] = d[1];
            params[3] = BLUR_TAPS;
            EffectPass { program: Program::Blur, uniform: EffectUniform::new(params, size) }
        })
        .collect()
}

fn color_pass(effect: &EffectState, size: Size) -> EffectPass {
    let exposure = effect.scalar("exposure", 0.0) as f32;
    let contrast = (effect.scalar("contrast", 0.0) as f32).clamp(-0.99, 0.99);
    let saturation = effect.scalar("saturation", 1.0) as f32;
    let gamma = effect.scalar("gamma", 1.0).max(0.01) as f32;
    let tint = effect.color("tint", Rgba::WHITE);

    let mut params = [0.0f32; 16];
    // Stops are doublings, so the gain is a power of two.
    params[0] = exposure.exp2();
    // Contrast as a symmetric multiplier about mid grey: +0.5 and -0.5 are
    // reciprocal, so a pair of them cancels out rather than leaving the picture
    // flatter than it started.
    params[1] = (1.0 + contrast) / (1.0 - contrast);
    params[2] = saturation;
    params[3] = 1.0 / gamma;
    params[4] = tint.r as f32;
    params[5] = tint.g as f32;
    params[6] = tint.b as f32;
    params[7] = tint.a as f32;
    EffectPass { program: Program::Color, uniform: EffectUniform::new(params, size) }
}

fn sharpen_pass(effect: &EffectState, size: Size) -> Option<EffectPass> {
    let amount = effect.scalar("amount", 0.0) as f32;
    if amount <= 0.0 {
        return None;
    }
    let mut params = [0.0f32; 16];
    params[0] = amount;
    params[1] = effect.scalar("radius", 1.0).max(0.0) as f32;
    Some(EffectPass { program: Program::Sharpen, uniform: EffectUniform::new(params, size) })
}

/// The inverse map a transform effect resamples through.
///
/// A fragment shader can only ask "where did this output pixel come from", so
/// what goes to the GPU is the *inverse* of the transform the user dialled in.
/// Composing and inverting it here rather than in the shader keeps the maths in
/// one testable place — and it is the kind of maths that is wrong in a way that
/// looks almost right.
///
/// Forward, in source pixels: `dest = R·S·(src − anchor) + anchor + position`.
/// Inverting gives `src = (R·S)⁻¹·(dest − anchor − position) + anchor`, and the
/// shader works in UV, so both ends are divided by the source size.
fn transform_pass(effect: &EffectState, size: Size) -> Option<EffectPass> {
    let position = effect.point("position", Vec2::ZERO);
    let scale = effect.point("scale", Vec2::ONE);
    let rotation = effect.scalar("rotation", 0.0);
    let anchor = effect.point("anchor", Vec2::splat(0.5));
    let opacity = effect.scalar("opacity", 1.0).clamp(0.0, 1.0) as f32;

    let identity = position == Vec2::ZERO
        && scale == Vec2::ONE
        && rotation == 0.0
        && (opacity - 1.0).abs() < f32::EPSILON;
    if identity {
        return None;
    }

    let (w, h) = (size.width.max(1) as f64, size.height.max(1) as f64);
    // A scale of zero collapses the picture to nothing and makes the inverse
    // undefined; the smallest value that still has an inverse draws the same
    // empty frame without dividing by zero.
    let sx = if scale.x.abs() < 1e-4 { 1e-4_f64.copysign(scale.x) } else { scale.x };
    let sy = if scale.y.abs() < 1e-4 { 1e-4_f64.copysign(scale.y) } else { scale.y };
    let theta = rotation.to_radians();
    let (sin, cos) = theta.sin_cos();

    // (R·S)⁻¹ = S⁻¹·R⁻¹, in pixel space.
    let inv = [[cos / sx, sin / sx], [-sin / sy, cos / sy]];
    let anchor_px = [anchor.x * w, anchor.y * h];
    let centre = [anchor_px[0] + position.x, anchor_px[1] + position.y];

    // Pixels to UV on both sides: A = diag(1/size)·inv·diag(size).
    let a = [[inv[0][0], inv[0][1] * h / w], [inv[1][0] * w / h, inv[1][1]]];
    let offset = [
        (anchor_px[0] - (inv[0][0] * centre[0] + inv[0][1] * centre[1])) / w,
        (anchor_px[1] - (inv[1][0] * centre[0] + inv[1][1] * centre[1])) / h,
    ];

    let mut params = [0.0f32; 16];
    params[0] = a[0][0] as f32;
    params[1] = a[0][1] as f32;
    params[2] = a[1][0] as f32;
    params[3] = a[1][1] as f32;
    params[4] = offset[0] as f32;
    params[5] = offset[1] as f32;
    params[6] = opacity;
    Some(EffectPass { program: Program::Transform, uniform: EffectUniform::new(params, size) })
}

fn mask_pass(effect: &EffectState, size: Size) -> Option<EffectPass> {
    let opacity = effect.scalar("opacity", 1.0).clamp(0.0, 1.0) as f32;
    if opacity <= 0.0 {
        return None;
    }
    let (w, h) = (size.width.max(1) as f32, size.height.max(1) as f32);
    let centre = effect.point("center", Vec2::splat(0.5));
    let extent = effect.point("size", Vec2::splat(0.5));
    let theta = (effect.scalar("rotation", 0.0) as f32).to_radians();
    let (sin, cos) = theta.sin_cos();

    let mut params = [0.0f32; 16];
    params[0] = centre.x as f32 * w;
    params[1] = centre.y as f32 * h;
    // Half extents, because a signed distance is measured from the centre.
    params[2] = extent.x as f32 * w * 0.5;
    params[3] = extent.y as f32 * h * 0.5;
    params[4] = cos;
    params[5] = sin;
    params[6] = effect.scalar("feather", 0.0).max(0.0) as f32;
    params[7] = if effect.choice("shape", 1) == 1 { 1.0 } else { 0.0 };
    params[8] = opacity;
    params[9] = if effect.flag("invert", false) { 1.0 } else { 0.0 };
    Some(EffectPass { program: Program::Mask, uniform: EffectUniform::new(params, size) })
}

fn luma_pass(effect: &EffectState, size: Size) -> EffectPass {
    let threshold = effect.scalar("threshold", 0.0) as f32;
    // The two edges rather than an edge and a width: a softness of zero would
    // make the shader's smoothstep undefined exactly where the key is hardest,
    // and a hair's width of ramp is also what antialiases it.
    let softness = (effect.scalar("softness", 0.0) as f32).max(1e-4);
    let mut params = [0.0f32; 16];
    params[0] = threshold;
    params[1] = threshold + softness;
    params[2] = if effect.flag("invert", false) { 1.0 } else { 0.0 };
    EffectPass { program: Program::LumaKey, uniform: EffectUniform::new(params, size) }
}

/// The pipelines and the uniform buffer effect passes are drawn with.
///
/// Held by the [`Renderer`], which owns the texture bind group layout and the
/// sampler an effect pass has to share: a pass reads exactly the textures the
/// compositor reads, including the output of another pass, and a second layout
/// would mean a second bind group per texture.
///
/// [`Renderer`]: crate::Renderer
pub(crate) struct EffectRenderer {
    /// One per (program, colour space), in [`Program::ALL`] then
    /// [`ColorSpace::ALL`] order. The colour space is an axis for the same
    /// reason it is in the compositor: it decides the attachment's format, and
    /// a pipeline's target format has to match the view the pass attaches.
    pipelines: Vec<wgpu::RenderPipeline>,
    buffer: wgpu::Buffer,
    bind_group: wgpu::BindGroup,
}

impl EffectRenderer {
    pub(crate) fn new(
        device: &wgpu::Device,
        texture_layout: &wgpu::BindGroupLayout,
        format: wgpu::TextureFormat,
    ) -> Self {
        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("verge-effect-shader"),
            source: wgpu::ShaderSource::Wgsl(include_str!("effects.wgsl").into()),
        });

        let uniform_layout =
            device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
                label: Some("verge-effect-uniform-layout"),
                entries: &[wgpu::BindGroupLayoutEntry {
                    binding: 0,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Buffer {
                        ty: wgpu::BufferBindingType::Uniform,
                        // One pass, one uniform: unlike a composite, which draws
                        // every layer in a single pass, effects are drawn one at a
                        // time because each reads what the last one wrote.
                        has_dynamic_offset: false,
                        min_binding_size: wgpu::BufferSize::new(
                            std::mem::size_of::<EffectUniform>() as u64,
                        ),
                    },
                    count: None,
                }],
            });

        let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("verge-effect-pipeline-layout"),
            bind_group_layouts: &[Some(&uniform_layout), Some(texture_layout)],
            immediate_size: 0,
        });

        let reinterpretable = format == FRAME_FORMAT;
        let mut pipelines = Vec::with_capacity(Program::ALL.len() * ColorSpace::ALL.len());
        for program in Program::ALL {
            for space in ColorSpace::ALL {
                let target_format =
                    if reinterpretable { view_format_for(space) } else { format };
                pipelines.push(device.create_render_pipeline(
                    &wgpu::RenderPipelineDescriptor {
                        label: Some("verge-effect-pipeline"),
                        layout: Some(&pipeline_layout),
                        vertex: wgpu::VertexState {
                            module: &shader,
                            entry_point: Some("vs_main"),
                            buffers: &[],
                            compilation_options: Default::default(),
                        },
                        fragment: Some(wgpu::FragmentState {
                            module: &shader,
                            entry_point: Some(program.entry_point()),
                            targets: &[Some(wgpu::ColorTargetState {
                                format: target_format,
                                // No blending. A pass covers its whole target
                                // and replaces it; what it read is already in
                                // hand as a texture, so there is nothing to
                                // blend against.
                                blend: None,
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

        let buffer = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("verge-effect-uniform"),
            size: std::mem::size_of::<EffectUniform>() as u64,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("verge-effect-uniform-bind-group"),
            layout: &uniform_layout,
            entries: &[wgpu::BindGroupEntry {
                binding: 0,
                resource: buffer.as_entire_binding(),
            }],
        });

        EffectRenderer { pipelines, buffer, bind_group }
    }

    /// Draws `source` through one pass into `target`.
    pub(crate) fn run(
        &self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        target: &RenderTarget,
        color_space: ColorSpace,
        source: &GpuTexture,
        pass: &EffectPass,
    ) {
        // Whether the source's alpha still has to be multiplied in is a
        // property of the texture, not of the effect, so it is filled in here
        // rather than when the pass was built.
        let mut uniform = pass.uniform;
        uniform.premultiply = if source.is_premultiplied() { 0.0 } else { 1.0 };
        queue.write_buffer(&self.buffer, 0, bytemuck::bytes_of(&uniform));

        let index = pass.program.index() * ColorSpace::ALL.len() + color_space as usize;
        let mut encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
            label: Some("verge-effect-encoder"),
        });
        {
            let mut render_pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("verge-effect-pass"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: target.view_for(color_space),
                    depth_slice: None,
                    resolve_target: None,
                    ops: wgpu::Operations {
                        // Cleared rather than loaded even though the draw covers
                        // the target: a target taken from the pool still holds
                        // the last picture drawn into it, and a program that
                        // discards a pixel — the transform's outside — would
                        // otherwise show it.
                        load: wgpu::LoadOp::Clear(wgpu::Color::TRANSPARENT),
                        store: wgpu::StoreOp::Store,
                    },
                })],
                depth_stencil_attachment: None,
                timestamp_writes: None,
                occlusion_query_set: None,
                multiview_mask: None,
            });
            render_pass.set_pipeline(&self.pipelines[index]);
            render_pass.set_bind_group(0, &self.bind_group, &[]);
            render_pass.set_bind_group(1, source.bind_group(color_space), &[]);
            render_pass.draw(0..4, 0..1);
        }
        queue.submit(Some(encoder.finish()));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ve_core::{builtin_registry, Effect, EffectId, ParamValue};
    use ve_time::Ticks;

    fn state(kind: &str) -> EffectState {
        let effect = builtin_registry().instantiate(kind, EffectId::from_raw(1)).unwrap();
        effect.evaluate(Ticks::ZERO, builtin_registry().get(kind))
    }

    fn with(kind: &str, key: &str, value: ParamValue) -> EffectState {
        let mut effect: Effect =
            builtin_registry().instantiate(kind, EffectId::from_raw(1)).unwrap();
        *effect.param_mut(key).expect("declared parameter") = value;
        effect.evaluate(Ticks::ZERO, builtin_registry().get(kind))
    }

    const HD: Size = Size::new(1920, 1080);

    #[test]
    fn a_blur_is_two_passes_one_per_axis() {
        let passes = passes_for(&state(kinds::GAUSSIAN_BLUR), HD);
        assert_eq!(passes.len(), 2);
        assert!(passes.iter().all(|p| p.program() == Program::Blur));
        // Horizontal first, then vertical: different axes, same radius.
        assert_eq!(passes[0].uniform.params[1..3], [1.0, 0.0]);
        assert_eq!(passes[1].uniform.params[1..3], [0.0, 1.0]);
        assert_eq!(passes[0].uniform.params[0], passes[1].uniform.params[0]);
    }

    #[test]
    fn blurring_one_axis_costs_one_pass() {
        let horizontal = with(kinds::GAUSSIAN_BLUR, "direction", ParamValue::Choice(1));
        assert_eq!(passes_for(&horizontal, HD).len(), 1);
        let vertical = with(kinds::GAUSSIAN_BLUR, "direction", ParamValue::Choice(2));
        let passes = passes_for(&vertical, HD);
        assert_eq!(passes.len(), 1);
        assert_eq!(passes[0].uniform.params[1..3], [0.0, 1.0]);
    }

    #[test]
    fn an_effect_dialled_to_neutral_costs_nothing() {
        // Every one of these would draw the picture it was given, at the price
        // of a full-resolution target and a pass.
        let no_blur = with(kinds::GAUSSIAN_BLUR, "radius", ParamValue::scalar(0.0));
        assert!(passes_for(&no_blur, HD).is_empty());
        let no_sharpen = with(kinds::SHARPEN, "amount", ParamValue::scalar(0.0));
        assert!(passes_for(&no_sharpen, HD).is_empty());
        let no_mask = with(kinds::SHAPE_MASK, "opacity", ParamValue::scalar(0.0));
        assert!(passes_for(&no_mask, HD).is_empty());
        // A transform at its defaults is the identity, whatever its anchor is.
        assert!(passes_for(&state(kinds::TRANSFORM), HD).is_empty());
    }

    #[test]
    fn an_effect_this_build_has_no_program_for_draws_nothing() {
        let foreign = Effect::new(EffectId::from_raw(1), "someone.elses.glow", "Glow")
            .evaluate(Ticks::ZERO, None);
        assert!(passes_for(&foreign, HD).is_empty());
    }

    #[test]
    fn a_chain_flattens_into_its_passes_in_order() {
        let chain = [state(kinds::GAUSSIAN_BLUR), state(kinds::COLOR_ADJUST)];
        let passes = chain_passes(&chain, HD);
        assert_eq!(
            passes.iter().map(EffectPass::program).collect::<Vec<_>>(),
            vec![Program::Blur, Program::Blur, Program::Color]
        );
    }

    #[test]
    fn contrast_is_symmetric_about_mid_grey() {
        // Pushing contrast up by some amount and then down by the same amount
        // has to be the identity, or a grade cannot be dialled back.
        let up =
            color_pass(&with(kinds::COLOR_ADJUST, "contrast", ParamValue::scalar(0.5)), HD);
        let down =
            color_pass(&with(kinds::COLOR_ADJUST, "contrast", ParamValue::scalar(-0.5)), HD);
        assert!((up.uniform.params[1] * down.uniform.params[1] - 1.0).abs() < 1e-6);
    }

    #[test]
    fn exposure_is_in_stops() {
        let one =
            color_pass(&with(kinds::COLOR_ADJUST, "exposure", ParamValue::scalar(1.0)), HD);
        assert!((one.uniform.params[0] - 2.0).abs() < 1e-6);
        let minus =
            color_pass(&with(kinds::COLOR_ADJUST, "exposure", ParamValue::scalar(-1.0)), HD);
        assert!((minus.uniform.params[0] - 0.5).abs() < 1e-6);
    }

    /// Where the shader will read from, for an output pixel at `uv`.
    fn inverse_map(pass: &EffectPass, uv: [f32; 2]) -> [f32; 2] {
        let p = pass.uniform.params;
        [p[0] * uv[0] + p[1] * uv[1] + p[4], p[2] * uv[0] + p[3] * uv[1] + p[5]]
    }

    #[test]
    fn a_transform_maps_the_centre_to_where_the_position_puts_it() {
        // Moved a quarter of the frame to the right: the output's centre now
        // shows what used to be a quarter-frame to its left.
        let moved =
            with(kinds::TRANSFORM, "position", ParamValue::point(Vec2::new(480.0, 0.0)));
        let pass = transform_pass(&moved, HD).expect("not the identity");
        let src = inverse_map(&pass, [0.5, 0.5]);
        assert!((src[0] - 0.25).abs() < 1e-5, "{src:?}");
        assert!((src[1] - 0.5).abs() < 1e-5, "{src:?}");
    }

    #[test]
    fn a_transform_scales_about_its_anchor() {
        let doubled = with(kinds::TRANSFORM, "scale", ParamValue::point(Vec2::splat(2.0)));
        let pass = transform_pass(&doubled, HD).expect("not the identity");
        // The anchor is the one point that does not move.
        let centre = inverse_map(&pass, [0.5, 0.5]);
        assert!((centre[0] - 0.5).abs() < 1e-5 && (centre[1] - 0.5).abs() < 1e-5);
        // And the output's right-hand edge shows what was three quarters along.
        let right = inverse_map(&pass, [1.0, 0.5]);
        assert!((right[0] - 0.75).abs() < 1e-5, "{right:?}");
    }

    #[test]
    fn a_quarter_turn_maps_the_axes_onto_each_other() {
        let turned = with(kinds::TRANSFORM, "rotation", ParamValue::scalar(90.0));
        let pass = transform_pass(&turned, HD).expect("not the identity");
        // Clockwise by 90°: what was at the top of the source is now on the
        // right of the output, so the output's right edge reads from the top.
        let right = inverse_map(&pass, [1.0, 0.5]);
        assert!((right[0] - 0.5).abs() < 1e-5, "{right:?}");
        assert!(right[1] < 0.5, "the right edge should read from above centre: {right:?}");
    }

    #[test]
    fn a_transform_with_a_zero_scale_still_produces_a_finite_map() {
        let collapsed = with(kinds::TRANSFORM, "scale", ParamValue::point(Vec2::new(0.0, 1.0)));
        let pass = transform_pass(&collapsed, HD).expect("not the identity");
        assert!(pass.uniform.params.iter().all(|v| v.is_finite()));
    }

    #[test]
    fn the_uniform_is_the_size_the_shader_expects() {
        // WGSL rounds the struct up to its 16-byte alignment. A Rust type that
        // disagreed would be accepted by the validator and read as garbage.
        assert_eq!(std::mem::size_of::<EffectUniform>(), 96);
    }
}
