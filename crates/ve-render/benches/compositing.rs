//! GPU compositing benchmarks.
//!
//! Run against whatever adapter is present. On a software rasteriser these
//! numbers say nothing about real hardware; they are here so a regression in
//! the number of draws, uploads or allocations per frame shows up as a change
//! rather than being invisible.

use criterion::{criterion_group, criterion_main, BenchmarkId, Criterion};
use std::hint::black_box;
use std::sync::Arc;
use ve_core::{BlendMode, ColorSpace, Rgba, Size, TransformState, Vec2};
use ve_media::{PixelFormat, VideoFrame};
use ve_render::{CompositeCache, CompositeKey, GpuContext, Layer, RenderTarget, Renderer};
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

criterion_group!(
    benches,
    upload,
    composite,
    blend_modes,
    render_cache,
    transform_math,
    color_space
);
criterion_main!(benches);
