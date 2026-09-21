//! What a scope costs, split into the two halves that behave differently.
//!
//! Reading the picture back is a GPU cost paid **once per distinct composite**:
//! a scaled draw, a copy across the bus and a stall waiting for the map. While
//! a grade is being dialled in on a held frame it is paid once and then not
//! again, however many times the interface repaints.
//!
//! Counting the picture is a CPU cost paid once per *plot*, which is to say
//! once per distinct composite as well — the panel keeps the uploaded trace and
//! re-uploads only when the picture or the scope changes.
//!
//! Both are measured at the size actually used, 256 across, because the whole
//! argument for sampling small is that these two numbers stop depending on the
//! resolution of the sequence being cut. The 1080p case is here to show what
//! reading back the preview target directly would have cost instead.

use std::hint::black_box;
use std::sync::Arc;

use criterion::{criterion_group, criterion_main, Criterion};
use ve_core::{ColorSpace, Rgba, Size};
use ve_media::{PixelFormat, VideoFrame};
use ve_render::scopes::{Histogram, ScopeSample, ScopeSampler, WaveformMode};
use ve_render::{
    vectorscope, waveform, CompositeKey, GpuContext, Layer, RenderTarget, Renderer,
};
use ve_time::Ticks;

const PLOT: Size = Size::new(256, 256);

/// A picture with something in every part of the range, so no scope gets an
/// unrealistically easy frame: a flat field lands every pixel of a column in
/// one cell, which is the cheapest thing a counter can be given.
fn spread(size: Size) -> Vec<u8> {
    let mut pixels = Vec::with_capacity((size.width * size.height * 4) as usize);
    for y in 0..size.height {
        for x in 0..size.width {
            let r = (x * 255 / size.width.max(1)) as u8;
            let g = (y * 255 / size.height.max(1)) as u8;
            let b = ((x + y) * 255 / (size.width + size.height).max(1)) as u8;
            pixels.extend_from_slice(&[r, g, b, 255]);
        }
    }
    pixels
}

fn frame(size: Size) -> VideoFrame {
    VideoFrame::new(
        Arc::from(spread(size).into_boxed_slice()),
        size,
        size.width * 4,
        PixelFormat::Rgba8,
        Ticks::ZERO,
    )
}

/// Counting one sample into each of the three plots.
fn counting(c: &mut Criterion) {
    let size = Size::new(256, 144);
    let sample = ScopeSample::new(spread(size), size);

    let mut group = c.benchmark_group("scope_count_256x144");
    group.bench_function("waveform_luma", |b| {
        b.iter(|| black_box(waveform(black_box(&sample), WaveformMode::Luma, PLOT)));
    });
    // Three traces instead of one, so this is the honest worst case.
    group.bench_function("waveform_parade", |b| {
        b.iter(|| black_box(waveform(black_box(&sample), WaveformMode::Parade, PLOT)));
    });
    group.bench_function("vectorscope", |b| {
        b.iter(|| black_box(vectorscope(black_box(&sample), PLOT)));
    });
    group.bench_function("histogram", |b| {
        b.iter(|| black_box(Histogram::of(black_box(&sample))));
    });
    group.finish();
}

/// The readback: a scaled draw, a copy and the wait for the map.
///
/// Both sizes are measured against the same 1080p source. The gap between them
/// is the whole argument for sampling small, and it is the cost that would
/// otherwise be paid on every frame of playback.
fn readback(c: &mut Criterion) {
    let Ok(gpu) = GpuContext::headless() else { return };
    let mut renderer = Renderer::new(&gpu.device);
    let source = Size::new(1920, 1080);
    let texture = renderer.upload(&gpu.device, &gpu.queue, &frame(source));

    let mut group = c.benchmark_group("scope_readback");
    let mut sampler = ScopeSampler::new(&gpu.device, source);
    // One key, and the sampler invalidated each time: a fresh key per iteration
    // would have to be built inside the loop, and building it is not what is
    // being measured.
    let held = key(&mut renderer, &gpu, 1);
    group.bench_function("sample_256_wide", |b| {
        b.iter(|| {
            sampler.invalidate();
            sampler.read(
                &gpu.device,
                &gpu.queue,
                &mut renderer,
                ColorSpace::Perceptual,
                &texture,
                black_box(held),
            );
        });
    });

    // What reading the preview target itself would cost, for comparison.
    let full = RenderTarget::new(&gpu.device, source);
    renderer.render(
        &gpu.device,
        &gpu.queue,
        &full,
        Rgba::TRANSPARENT,
        ColorSpace::Perceptual,
        &[Layer::new(&texture)],
    );
    group.bench_function("read_1080p_target", |b| {
        b.iter(|| black_box(full.read_pixels(&gpu.device, &gpu.queue)));
    });
    group.finish();
}

/// A key that differs per iteration, so the sampler's "already read" path is
/// not what is being timed.
fn key(renderer: &mut Renderer, gpu: &GpuContext, n: u64) -> CompositeKey {
    let one = Size::new(1, 1);
    let texture = renderer.upload(
        &gpu.device,
        &gpu.queue,
        &VideoFrame::new(
            Arc::from(vec![n as u8, 0, 0, 255].into_boxed_slice()),
            one,
            4,
            PixelFormat::Rgba8,
            Ticks::ZERO,
        ),
    );
    CompositeKey::of(one, Rgba::TRANSPARENT, ColorSpace::Perceptual, &[Layer::new(&texture)])
}

/// The cost of noticing the picture has not changed.
///
/// Paid on every repaint, so it has to be nothing beside the count it avoids.
fn already_sampled(c: &mut Criterion) {
    let Ok(gpu) = GpuContext::headless() else { return };
    let mut renderer = Renderer::new(&gpu.device);
    let source = Size::new(1920, 1080);
    let texture = renderer.upload(&gpu.device, &gpu.queue, &frame(source));
    let mut sampler = ScopeSampler::new(&gpu.device, source);
    let held = key(&mut renderer, &gpu, 1);
    sampler.read(
        &gpu.device,
        &gpu.queue,
        &mut renderer,
        ColorSpace::Perceptual,
        &texture,
        held,
    );

    c.bench_function("scope_held_frame", |b| {
        b.iter(|| {
            black_box(sampler.read(
                &gpu.device,
                &gpu.queue,
                &mut renderer,
                ColorSpace::Perceptual,
                &texture,
                black_box(held),
            ))
        });
    });
}

criterion_group!(benches, counting, readback, already_sampled);
criterion_main!(benches);
