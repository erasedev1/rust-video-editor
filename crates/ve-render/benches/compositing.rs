//! GPU compositing benchmarks.
//!
//! Run against whatever adapter is present. On a software rasteriser these
//! numbers say nothing about real hardware; they are here so a regression in
//! the number of draws, uploads or allocations per frame shows up as a change
//! rather than being invisible.

use criterion::{criterion_group, criterion_main, BenchmarkId, Criterion};
use std::hint::black_box;
use std::sync::Arc;
use ve_core::registry::kinds;
use ve_core::{
    builtin_registry, BlendMode, ColorSpace, EffectId, EffectState, ParamValue, Rgba, Size,
    TransformState, Vec2,
};
use ve_media::{PixelFormat, VideoFrame};
use ve_render::{
    chain_passes, CompositeCache, CompositeKey, EffectPass, GpuContext, Layer, RenderTarget,
    Renderer,
};
use ve_time::Ticks;

fn solid_frame(size: Size) -> VideoFrame {
    let stride = size.width * 4;
    let data = vec![128u8; (stride * size.height) as usize];
    VideoFrame::new(
        Arc::from(data.into_boxed_slice()),
        size,
        stride,
        PixelFormat::Rgba8,
        Ticks::ZERO,
    )
}

fn upload(c: &mut Criterion) {
    let Ok(gpu) = GpuContext::headless() else {
        eprintln!("skipping GPU benchmarks: no adapter");
        return;
    };
    let renderer = Renderer::new(&gpu.device);

    let mut group = c.benchmark_group("upload_frame");
    for &(w, h, label) in &[(640u32, 360u32, "360p"), (1920, 1080, "1080p")] {
        let frame = solid_frame(Size::new(w, h));
        group.bench_with_input(BenchmarkId::from_parameter(label), &label, |b, _| {
            b.iter(|| black_box(renderer.upload(&gpu.device, &gpu.queue, black_box(&frame))));
        });
    }
    group.finish();
}

fn composite(c: &mut Criterion) {
    let Ok(gpu) = GpuContext::headless() else { return };
    let mut renderer = Renderer::new(&gpu.device);
    let size = Size::new(1920, 1080);
    let target = RenderTarget::new(&gpu.device, size);
    let frame = solid_frame(Size::new(1920, 1080));

    let textures: Vec<_> =
        (0..16).map(|_| renderer.upload(&gpu.device, &gpu.queue, &frame)).collect();

    let mut group = c.benchmark_group("composite_1080p");
    for &layers in &[1usize, 4, 16] {
        group.bench_with_input(BenchmarkId::from_parameter(layers), &layers, |b, &layers| {
            let list: Vec<Layer> = textures[..layers].iter().map(Layer::new).collect();
            b.iter(|| {
                renderer.render(
                    &gpu.device,
                    &gpu.queue,
                    &target,
                    Rgba::BLACK,
                    ColorSpace::Perceptual,
                    black_box(&list),
                );
                // Waiting for the GPU is what makes this a measurement of the
                // work rather than of how fast commands can be queued.
                let _ = gpu.device.poll(wgpu::PollType::wait_indefinitely());
            });
        });
    }
    group.finish();
}

/// What a blend mode costs.
///
/// `one_mode` is sixteen layers that all blend the same way, so the pipeline is
/// bound once for the pass. `alternating` cycles through the modes, forcing a
/// rebind before every draw. The gap between them is the price of a pipeline
/// switch, and the reason the renderer only rebinds when the mode changes rather
/// than once per layer.
fn blend_modes(c: &mut Criterion) {
    let Ok(gpu) = GpuContext::headless() else { return };
    let mut renderer = Renderer::new(&gpu.device);
    let size = Size::new(1920, 1080);
    let target = RenderTarget::new(&gpu.device, size);
    let frame = solid_frame(size);

    const LAYERS: usize = 16;
    let textures: Vec<_> =
        (0..LAYERS).map(|_| renderer.upload(&gpu.device, &gpu.queue, &frame)).collect();

    let mut group = c.benchmark_group("composite_1080p_16layers");
    for (label, modes) in [
        ("one_mode", vec![BlendMode::Screen; LAYERS]),
        (
            "alternating",
            (0..LAYERS).map(|i| BlendMode::ALL[i % BlendMode::ALL.len()]).collect(),
        ),
    ] {
        let list: Vec<Layer> = textures
            .iter()
            .zip(&modes)
            .map(|(t, mode)| Layer::new(t).with_blend(*mode))
            .collect();
        group.bench_function(label, |b| {
            b.iter(|| {
                renderer.render(
                    &gpu.device,
                    &gpu.queue,
                    &target,
                    Rgba::BLACK,
                    ColorSpace::Perceptual,
                    black_box(&list),
                );
                let _ = gpu.device.poll(wgpu::PollType::wait_indefinitely());
            });
        });
    }
    group.finish();
}

/// The render cache's two paths, against the composite they replace.
///
/// `hit` is what a repaint of an unchanged-but-evicted picture costs: a key, a
/// lookup and a blit. `miss` is the full path — composite, blit, store — so the
/// difference between the two is what the cache actually saves. A repaint whose
/// composition has not changed at all costs neither: the preview presents what
/// is already on screen and does no GPU work, which is why that case is not
/// benchmarked here.
fn render_cache(c: &mut Criterion) {
    let Ok(gpu) = GpuContext::headless() else { return };
    let mut renderer = Renderer::new(&gpu.device);
    let size = Size::new(1920, 1080);
    let present = RenderTarget::new(&gpu.device, size);
    let frame = solid_frame(size);

    const LAYERS: usize = 4;
    let textures: Vec<_> =
        (0..LAYERS).map(|_| renderer.upload(&gpu.device, &gpu.queue, &frame)).collect();
    let layers: Vec<Layer> = textures.iter().map(Layer::new).collect();

    let mut group = c.benchmark_group("render_cache_1080p_4layers");

    group.bench_function("hit", |b| {
        let mut cache = CompositeCache::with_budget_mb(256);
        let key = CompositeKey::of(size, Rgba::BLACK, ColorSpace::Perceptual, &layers);
        let target = cache.take_target(&gpu.device, size);
        renderer.render(
            &gpu.device,
            &gpu.queue,
            &target,
            Rgba::BLACK,
            ColorSpace::Perceptual,
            &layers,
        );
        cache.insert(key, target);

        b.iter(|| {
            let cached = cache.get(black_box(&key)).expect("stored above");
            present.blit_from(&gpu.device, &gpu.queue, cached);
            let _ = gpu.device.poll(wgpu::PollType::wait_indefinitely());
        });
    });

    group.bench_function("miss", |b| {
        let mut cache = CompositeCache::with_budget_mb(256);
        // A moving layer, so every iteration is a picture the cache has not
        // seen — which is also what playing a sequence with an animated
        // transform looks like.
        let mut nudge = 0.0f64;
        b.iter(|| {
            nudge += 1.0;
            let moved: Vec<Layer> = textures
                .iter()
                .map(|t| {
                    Layer::new(t).with_transform(TransformState {
                        position: Vec2::new(nudge, 0.0),
                        ..Default::default()
                    })
                })
                .collect();
            let key = CompositeKey::of(size, Rgba::BLACK, ColorSpace::Perceptual, &moved);
            let target = cache.take_target(&gpu.device, size);
            renderer.render(
                &gpu.device,
                &gpu.queue,
                &target,
                Rgba::BLACK,
                ColorSpace::Perceptual,
                &moved,
            );
            present.blit_from(&gpu.device, &gpu.queue, &target);
            cache.insert(key, target);
            let _ = gpu.device.poll(wgpu::PollType::wait_indefinitely());
        });
    });

    group.bench_function("key", |b| {
        b.iter(|| {
            black_box(CompositeKey::of(
                size,
                Rgba::BLACK,
                ColorSpace::Perceptual,
                black_box(&layers),
            ))
        });
    });

    group.finish();
}

/// Perceptual against linear at the same layer count.
///
/// Linear compositing adds no shader work and no passes — the conversion
/// belongs to the sampler and the attachment. That makes it free on hardware,
/// where both are fixed-function, and decidedly not free on a software
/// rasteriser, which has to execute the transfer function per texel and per
/// pixel. Measured at roughly +48% on llvmpipe; see docs/BENCHMARKS.md.
fn color_space(c: &mut Criterion) {
    let Ok(gpu) = GpuContext::headless() else { return };
    let mut renderer = Renderer::new(&gpu.device);
    let size = Size::new(1920, 1080);
    let target = RenderTarget::new(&gpu.device, size);
    let frame = solid_frame(size);

    let textures: Vec<_> =
        (0..4).map(|_| renderer.upload(&gpu.device, &gpu.queue, &frame)).collect();
    let list: Vec<Layer> = textures.iter().map(Layer::new).collect();

    let mut group = c.benchmark_group("composite_1080p_4_layers");
    for space in ColorSpace::ALL {
        group.bench_with_input(
            BenchmarkId::from_parameter(space.label()),
            &space,
            |b, &space| {
                b.iter(|| {
                    renderer.render(
                        &gpu.device,
                        &gpu.queue,
                        &target,
                        Rgba::BLACK,
                        black_box(space),
                        black_box(&list),
                    );
                    let _ = gpu.device.poll(wgpu::PollType::wait_indefinitely());
                });
            },
        );
    }
    group.finish();
}

fn transform_math(c: &mut Criterion) {
    let source = Size::new(1920, 1080);
    let composition = Size::new(1920, 1080);
    let state = TransformState { rotation: 33.0, ..Default::default() };
    c.bench_function("layer_matrix", |b| {
        b.iter(|| {
            black_box(ve_render::layer_matrix(
                black_box(source),
                black_box(composition),
                black_box(&state),
            ))
        });
    });
}

/// What a motion-blurred layer costs, against the single draw it replaces.
///
/// A blurred layer is a pass of its own — N draws into a target, then one draw
/// of that target into the node — so the number to watch is how the cost grows
/// with the sample count. It should be close to linear in the samples and
/// nothing else: the samples share one texture, one uniform buffer and one
/// pass.
fn motion_blur(c: &mut Criterion) {
    let Ok(gpu) = GpuContext::headless() else { return };
    let mut renderer = Renderer::new(&gpu.device);
    let size = Size::new(1920, 1080);
    let target = RenderTarget::new(&gpu.device, size);
    let frame = solid_frame(size);
    let texture = renderer.upload(&gpu.device, &gpu.queue, &frame);

    let mut group = c.benchmark_group("motion_blur_1080p");
    for &samples in &[1usize, 4, 12, 32] {
        group.bench_with_input(
            BenchmarkId::from_parameter(samples),
            &samples,
            |b, &samples| {
                let list: Vec<Layer> = (0..samples)
                    .map(|i| {
                        Layer::new(&texture).with_transform(TransformState {
                            position: Vec2::new(i as f64 * 0.5, 0.0),
                            ..Default::default()
                        })
                    })
                    .collect();
                b.iter(|| {
                    renderer.accumulate(
                        &gpu.device,
                        &gpu.queue,
                        &target,
                        ColorSpace::Perceptual,
                        black_box(&list),
                    );
                    let _ = gpu.device.poll(wgpu::PollType::wait_indefinitely());
                });
            },
        );
    }
    group.finish();
}

/// An effect of `kind` at its defaults, with `overrides` applied, resolved.
fn effect(kind: &str, overrides: &[(&str, ParamValue)]) -> EffectState {
    let registry = builtin_registry();
    let mut effect = registry.instantiate(kind, EffectId::from_raw(1)).expect("a built-in");
    for (key, value) in overrides {
        *effect.param_mut(key).expect("a declared parameter") = value.clone();
    }
    effect.evaluate(Ticks::ZERO, registry.get(kind))
}

/// What one pass of each program costs, against the composite it sits beside.
///
/// Every program is one full-screen draw over the same target, so what this
/// separates is the *shader*: a blur takes tens of taps per pixel and a colour
/// adjustment takes one, and the gap between them is the only thing here that
/// is about the effect rather than about the rasteriser.
fn effect_passes(c: &mut Criterion) {
    let Ok(gpu) = GpuContext::headless() else { return };
    let mut renderer = Renderer::new(&gpu.device);
    let size = Size::new(1920, 1080);
    let target = RenderTarget::new(&gpu.device, size);
    let texture = renderer.upload(&gpu.device, &gpu.queue, &solid_frame(size));

    let cases: Vec<(&str, EffectState)> = vec![
        (
            "blur_one_axis",
            effect(kinds::GAUSSIAN_BLUR, &[("direction", ParamValue::Choice(1))]),
        ),
        ("color", effect(kinds::COLOR_ADJUST, &[])),
        ("sharpen", effect(kinds::SHARPEN, &[])),
        (
            "transform",
            effect(kinds::TRANSFORM, &[("scale", ParamValue::point(Vec2::splat(1.5)))]),
        ),
        ("mask", effect(kinds::SHAPE_MASK, &[])),
        ("luma_key", effect(kinds::LUMA_KEY, &[])),
        // The three-way corrector resolves to three vectors on the CPU, so the
        // shader is a multiply, an add and a `pow` — which is what this is
        // measuring against the colour adjustment beside it.
        ("three_way", effect(kinds::THREE_WAY, &[("shadow_level", ParamValue::scalar(0.3))])),
        // The secondary is the expensive one: two colour space conversions per
        // pixel, which is what a qualifier costs anywhere.
        (
            "hsl_secondary",
            effect(kinds::HSL_SECONDARY, &[("saturation_scale", ParamValue::scalar(1.5))]),
        ),
    ];

    let mut group = c.benchmark_group("effect_pass_1080p");
    for (label, state) in &cases {
        let passes = chain_passes(std::slice::from_ref(state), size);
        let pass: &EffectPass = &passes[0];
        group.bench_with_input(BenchmarkId::from_parameter(label), pass, |b, pass| {
            b.iter(|| {
                renderer.apply_effect(
                    &gpu.device,
                    &gpu.queue,
                    &target,
                    ColorSpace::Perceptual,
                    &texture,
                    black_box(pass),
                );
                let _ = gpu.device.poll(wgpu::PollType::wait_indefinitely());
            });
        });
    }
    group.finish();
}

/// How a blur's cost grows with its radius.
///
/// It should stop growing. The kernel is sampled a bounded number of times —
/// see `BLUR_TAPS` — so past that radius the samples spread out rather than
/// multiply, which is the trade that keeps a 200-pixel blur interactive.
fn blur_radius(c: &mut Criterion) {
    let Ok(gpu) = GpuContext::headless() else { return };
    let mut renderer = Renderer::new(&gpu.device);
    let size = Size::new(1920, 1080);
    let target = RenderTarget::new(&gpu.device, size);
    let texture = renderer.upload(&gpu.device, &gpu.queue, &solid_frame(size));

    let mut group = c.benchmark_group("blur_1080p_one_axis");
    for &radius in &[1.0f64, 8.0, 64.0, 200.0] {
        let state = effect(
            kinds::GAUSSIAN_BLUR,
            &[("radius", ParamValue::scalar(radius)), ("direction", ParamValue::Choice(1))],
        );
        let passes = chain_passes(std::slice::from_ref(&state), size);
        group.bench_with_input(
            BenchmarkId::from_parameter(radius as u32),
            &passes[0],
            |b, pass| {
                b.iter(|| {
                    renderer.apply_effect(
                        &gpu.device,
                        &gpu.queue,
                        &target,
                        ColorSpace::Perceptual,
                        &texture,
                        black_box(pass),
                    );
                    let _ = gpu.device.poll(wgpu::PollType::wait_indefinitely());
                });
            },
        );
    }
    group.finish();
}

/// Resolving a chain into passes, which happens on the interface thread once
/// per frame per layer.
///
/// It is CPU work between the plan and the GPU, so what matters is that it is
/// nowhere near a frame: a chain that cost microseconds to *describe* would be
/// a problem however fast the shaders were.
fn chain_planning(c: &mut Criterion) {
    let size = Size::new(1920, 1080);
    let chain = [
        effect(kinds::GAUSSIAN_BLUR, &[]),
        effect(kinds::COLOR_ADJUST, &[]),
        effect(kinds::SHAPE_MASK, &[]),
    ];
    c.bench_function("chain_passes_3_effects", |b| {
        b.iter(|| black_box(chain_passes(black_box(&chain), black_box(size))));
    });
}

criterion_group!(
    benches,
    upload,
    composite,
    blend_modes,
    render_cache,
    transform_math,
    color_space,
    motion_blur,
    effect_passes,
    blur_radius,
    chain_planning
);
criterion_main!(benches);
