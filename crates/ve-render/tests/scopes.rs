//! The scope sampler against a real device.
//!
//! The unit tests in `scopes.rs` check what the counting does with a picture.
//! This checks the step before it: that the picture the sampler reads back is
//! the one that was composited, scaled to fit and not cropped, and that a frame
//! already sampled is not read back a second time.

use std::sync::Arc;

use ve_core::{ColorSpace, Size};
use ve_media::{PixelFormat, VideoFrame};
use ve_render::scopes::{Histogram, ScopeSampler, WaveformMode};
use ve_render::{waveform, CompositeKey, GpuContext, Renderer};
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

fn frame(size: Size, paint: impl Fn(u32, u32) -> [u8; 4]) -> VideoFrame {
    let stride = size.width * 4;
    let mut data = Vec::with_capacity((stride * size.height) as usize);
    for y in 0..size.height {
        for x in 0..size.width {
            data.extend_from_slice(&paint(x, y));
        }
    }
    VideoFrame::new(
        Arc::from(data.into_boxed_slice()),
        size,
        stride,
        PixelFormat::Rgba8,
        Ticks::ZERO,
    )
}

/// Distinct keys, standing for distinct composites.
fn key(n: u64) -> CompositeKey {
    let gpu = gpu();
    let renderer = Renderer::new(&gpu.device);
    let texture = renderer.upload(
        &gpu.device,
        &gpu.queue,
        &frame(Size::new(1, 1), |_, _| [n as u8, 0, 0, 255]),
    );
    CompositeKey::of(
        Size::new(1, 1),
        ve_core::Rgba::TRANSPARENT,
        ColorSpace::Perceptual,
        &[ve_render::Layer::new(&texture)],
    )
}

const HD: Size = Size::new(640, 360);

#[test]
fn the_sample_is_the_picture_that_was_composited() {
    let gpu = gpu();
    let mut renderer = Renderer::new(&gpu.device);
    let mut sampler = ScopeSampler::new(&gpu.device, HD);

    // Left half white, right half black.
    let source = frame(HD, |x, _| if x < HD.width / 2 { [255; 4] } else { [0, 0, 0, 255] });
    let texture = renderer.upload(&gpu.device, &gpu.queue, &source);
    assert!(sampler.read(
        &gpu.device,
        &gpu.queue,
        &mut renderer,
        ColorSpace::Perceptual,
        &texture,
        key(1),
    ));

    let sample = sampler.sample();
    assert_eq!(sample.size(), Size::new(256, 144), "sampled at the picture's own shape");

    // Half the frame at each end of the range, and nothing in between beyond
    // the one column of pixels the downscale straddles the edge with.
    let histogram = Histogram::of(sample);
    let total: u32 = histogram.luma.iter().sum();
    assert_eq!(total, 256 * 144, "every pixel counted once");
    assert!(histogram.luma[0] > total / 3, "the black half is there");
    assert!(histogram.luma[255] > total / 3, "and so is the white half");

    // And the halves are the right way round on the plot.
    let plot = waveform(sample, WaveformMode::Luma, Size::new(256, 256));
    let lit = |x: u32| (0..256).find(|y| plot.pixel_at(x, *y)[3] > 0).expect("a trace");
    assert_eq!(lit(20), 0, "white is on the left, at the top of the plot");
    assert_eq!(lit(235), 255, "black is on the right, at the bottom");
}

#[test]
fn a_picture_already_sampled_is_not_read_back_again() {
    // The common case: a grade is dialled in on a held frame, so the same
    // composite is presented on every repaint. Reading it back each time would
    // stall the pipeline sixty times a second for an answer that has not moved.
    let gpu = gpu();
    let mut renderer = Renderer::new(&gpu.device);
    let mut sampler = ScopeSampler::new(&gpu.device, HD);
    let texture =
        renderer.upload(&gpu.device, &gpu.queue, &frame(HD, |_, _| [128, 128, 128, 255]));

    let (first, second) = (key(1), key(2));
    macro_rules! read {
        ($k:expr) => {
            sampler.read(
                &gpu.device,
                &gpu.queue,
                &mut renderer,
                ColorSpace::Perceptual,
                &texture,
                $k,
            )
        };
    }
    assert!(read!(first), "the first read happens");
    assert!(!read!(first), "the second does not");
    assert!(read!(second), "a different picture does");

    sampler.invalidate();
    assert!(read!(second), "and switching the scopes on reads whatever is on screen");
}

#[test]
fn a_tall_picture_is_scaled_to_fit_rather_than_cropped() {
    // A scope reads the whole frame. One that quietly left the edges out would
    // under-report exactly the vignette and the fall-off it is there to show.
    let gpu = gpu();
    let mut renderer = Renderer::new(&gpu.device);
    let tall = Size::new(360, 640);
    let mut sampler = ScopeSampler::new(&gpu.device, tall);

    // A border of white round a black field: cropping loses it, fitting keeps it.
    let source = frame(tall, |x, y| {
        let edge = x < 8 || y < 8 || x >= tall.width - 8 || y >= tall.height - 8;
        if edge {
            [255; 4]
        } else {
            [0, 0, 0, 255]
        }
    });
    let texture = renderer.upload(&gpu.device, &gpu.queue, &source);
    sampler.read(
        &gpu.device,
        &gpu.queue,
        &mut renderer,
        ColorSpace::Perceptual,
        &texture,
        key(1),
    );

    let sample = sampler.sample();
    assert_eq!(sample.size(), Size::new(256, 455), "the sample is the picture's shape");
    let histogram = Histogram::of(sample);
    assert!(histogram.luma[255] > 0, "the border survived the scaling");
}
