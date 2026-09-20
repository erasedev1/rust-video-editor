//! The motion blur pass, rendered on a real device and read back.
//!
//! What is asserted here is the one thing the averaging pass exists for: that N
//! samples of a moving layer come out as their **mean**, not as a stack of
//! semi-transparent copies. The difference is visible in a single number — a
//! fully opaque layer that does not move must stay fully opaque however many
//! times it is sampled — and getting it wrong is exactly the bug that makes
//! every blurred layer quietly translucent.

use std::sync::Arc;

use ve_core::{ColorSpace, Rgba, Size, TransformState, Vec2};
use ve_media::{PixelFormat, VideoFrame};
use ve_render::{CompositeKey, GpuContext, Layer, RenderTarget, Renderer};
use ve_time::Ticks;

fn gpu() -> &'static GpuContext {
    use std::sync::OnceLock;
    static GPU: OnceLock<Option<GpuContext>> = OnceLock::new();
    GPU.get_or_init(|| match GpuContext::headless() {
        Ok(ctx) => Some(ctx),
        Err(e) => panic!("no GPU adapter available for rendering tests: {e}"),
    })
    .as_ref()
    .unwrap()
}

fn solid_frame(size: Size, colour: [u8; 4]) -> VideoFrame {
    let stride = size.width * 4;
    let mut data = Vec::with_capacity((stride * size.height) as usize);
    for _ in 0..size.width * size.height {
        data.extend_from_slice(&colour);
    }
    VideoFrame::new(
        Arc::from(data.into_boxed_slice()),
        size,
        stride,
        PixelFormat::Rgba8,
        Ticks::ZERO,
    )
}

#[track_caller]
fn assert_close(got: [u8; 4], want: [u8; 4], what: &str) {
    let close = got.iter().zip(want.iter()).all(|(a, b)| (*a as i32 - *b as i32).abs() <= 3);
    assert!(close, "{what}: got {got:?}, expected {want:?}");
}

const RED: [u8; 4] = [255, 0, 0, 255];
const TRANSPARENT: [u8; 4] = [0, 0, 0, 0];

/// A layer at `x`, given a source the size of half the canvas.
fn at(x: f64) -> TransformState {
    TransformState { position: Vec2::new(x, 0.0), ..Default::default() }
}

#[test]
fn a_layer_that_does_not_move_survives_being_sampled_many_times() {
    let size = Size::new(32, 32);
    let gpu = gpu();
    let mut renderer = Renderer::new(&gpu.device);
    let target = RenderTarget::new(&gpu.device, size);
    let texture = renderer.upload(&gpu.device, &gpu.queue, &solid_frame(size, RED));

    // Sixteen samples of a still layer. Composited with `over` at 1/16 opacity
    // each this would come out about 64% opaque; averaged, it is exactly what
    // one sample was.
    let samples: Vec<Layer<'_>> = (0..16).map(|_| Layer::new(&texture)).collect();
    renderer.accumulate(&gpu.device, &gpu.queue, &target, ColorSpace::Perceptual, &samples);
    let pixels = target.read_pixels(&gpu.device, &gpu.queue);

    assert_close(RenderTarget::pixel_at(&pixels, size, 16, 16), RED, "centre");
    assert_close(RenderTarget::pixel_at(&pixels, size, 0, 0), RED, "corner");
}

#[test]
fn two_positions_average_into_half_coverage_at_each() {
    // A source a quarter of the canvas wide, sampled at two positions far
    // enough apart that they do not overlap: each should come out at half
    // coverage, and the gap between them empty.
    let canvas = Size::new(64, 32);
    let source = Size::new(16, 32);
    let gpu = gpu();
    let mut renderer = Renderer::new(&gpu.device);
    let target = RenderTarget::new(&gpu.device, canvas);
    let texture = renderer.upload(&gpu.device, &gpu.queue, &solid_frame(source, RED));

    let samples = vec![
        Layer::new(&texture).with_transform(at(-16.0)),
        Layer::new(&texture).with_transform(at(16.0)),
    ];
    renderer.accumulate(&gpu.device, &gpu.queue, &target, ColorSpace::Perceptual, &samples);
    let pixels = target.read_pixels(&gpu.device, &gpu.queue);

    // Premultiplied: half the alpha and half the colour.
    assert_close(
        RenderTarget::pixel_at(&pixels, canvas, 16, 16),
        [128, 0, 0, 128],
        "where the layer was at the start of the shutter",
    );
    assert_close(
        RenderTarget::pixel_at(&pixels, canvas, 48, 16),
        [128, 0, 0, 128],
        "where it was at the end",
    );
    assert_close(
        RenderTarget::pixel_at(&pixels, canvas, 4, 16),
        TRANSPARENT,
        "outside the smear the target stays transparent",
    );
}

#[test]
fn a_smear_composites_over_a_backdrop_as_one_layer() {
    // The whole point of averaging into a target of its own: the result is a
    // layer, and the backdrop shows through it exactly as much as the smear
    // leaves uncovered.
    let canvas = Size::new(64, 32);
    let source = Size::new(16, 32);
    let gpu = gpu();
    let mut renderer = Renderer::new(&gpu.device);
    let blurred = RenderTarget::new(&gpu.device, canvas);
    let final_target = RenderTarget::new(&gpu.device, canvas);
    let texture = renderer.upload(&gpu.device, &gpu.queue, &solid_frame(source, RED));

    let samples = vec![
        Layer::new(&texture).with_transform(at(-16.0)),
        Layer::new(&texture).with_transform(at(16.0)),
    ];
    let key = CompositeKey::of_average(canvas, ColorSpace::Perceptual, &samples);
    renderer.accumulate(&gpu.device, &gpu.queue, &blurred, ColorSpace::Perceptual, &samples);

    let smear = renderer.bind_target(&gpu.device, &blurred, key);
    renderer.render(
        &gpu.device,
        &gpu.queue,
        &final_target,
        // Opaque white, so anything the smear does not cover reads as white
        // rather than as a hole.
        Rgba::new(1.0, 1.0, 1.0, 1.0),
        ColorSpace::Perceptual,
        &[Layer::new(&smear)],
    );
    let pixels = final_target.read_pixels(&gpu.device, &gpu.queue);

    assert_close(
        RenderTarget::pixel_at(&pixels, canvas, 16, 16),
        [255, 128, 128, 255],
        "half red over white",
    );
    assert_close(
        RenderTarget::pixel_at(&pixels, canvas, 4, 16),
        [255, 255, 255, 255],
        "the backdrop is untouched where nothing was smeared",
    );
}

#[test]
fn an_averaged_layer_is_keyed_apart_from_a_composite_of_the_same_layers() {
    let size = Size::new(32, 32);
    let gpu = gpu();
    let renderer = Renderer::new(&gpu.device);
    let texture = renderer.upload(&gpu.device, &gpu.queue, &solid_frame(size, RED));
    let layers = vec![Layer::new(&texture).with_transform(at(-4.0)), Layer::new(&texture)];

    // Same inputs, different pictures: summing weighted samples is not stacking
    // them, so the two must not share a cache entry.
    let averaged = CompositeKey::of_average(size, ColorSpace::Perceptual, &layers);
    let composited = CompositeKey::of(size, Rgba::TRANSPARENT, ColorSpace::Perceptual, &layers);
    assert_ne!(averaged, composited);

    // And the key still describes its own inputs: moving one sample changes it.
    let moved = vec![Layer::new(&texture).with_transform(at(-5.0)), Layer::new(&texture)];
    assert_ne!(averaged, CompositeKey::of_average(size, ColorSpace::Perceptual, &moved));
}
