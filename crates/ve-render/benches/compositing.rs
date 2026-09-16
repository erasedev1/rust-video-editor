//! GPU compositing benchmarks.
//!
//! Run against whatever adapter is present. On a software rasteriser these
//! numbers say nothing about real hardware; they are here so a regression in
//! the number of draws, uploads or allocations per frame shows up as a change
//! rather than being invisible.

use criterion::{criterion_group, criterion_main, BenchmarkId, Criterion};
use std::hint::black_box;
use std::sync::Arc;
use ve_core::{Rgba, Size, TransformState};
use ve_media::{PixelFormat, VideoFrame};
use ve_render::{GpuContext, Layer, RenderTarget, Renderer};
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
            let list: Vec<Layer> = textures[..layers]
                .iter()
                .map(|t| Layer { texture: t, transform: TransformState::default() })
                .collect();
            b.iter(|| {
                renderer.render(
                    &gpu.device,
                    &gpu.queue,
                    &target,
                    Rgba::BLACK,
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

criterion_group!(benches, upload, composite, transform_math);
criterion_main!(benches);
