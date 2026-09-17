//! Compositing tests that render on a real GPU device and read the pixels back.
//!
//! These run against whatever adapter is available, including a software
//! rasteriser such as Mesa's lavapipe, so continuous integration exercises the
//! actual shader and blend state rather than skipping the renderer.

use std::sync::Arc;

use ve_core::{BlendMode, ColorSpace, Rgba, Size, TransformState, Vec2};
use ve_media::{PixelFormat, VideoFrame};
use ve_metrics::Metrics;
use ve_render::{GpuContext, Layer, RenderTarget, Renderer};
use ve_time::Ticks;

/// A GPU context shared by every test in the process.
///
/// Creating a device costs tens of milliseconds and, on some drivers, is not
/// safe to do many times over in one process.
fn gpu() -> &'static GpuContext {
    use std::sync::OnceLock;
    static GPU: OnceLock<Option<GpuContext>> = OnceLock::new();
    GPU.get_or_init(|| match GpuContext::headless() {
        Ok(ctx) => {
            eprintln!("rendering tests using adapter: {ctx:?}");
            Some(ctx)
        }
        Err(e) => {
            // A machine with no usable adapter at all should fail loudly rather
            // than quietly reporting green for tests that never ran.
            panic!("no GPU adapter available for rendering tests: {e}");
        }
    })
    .as_ref()
    .unwrap()
}

/// A solid-colour frame, standing in for a decoded one.
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

struct Harness {
    renderer: Renderer,
    target: RenderTarget,
    size: Size,
}

impl Harness {
    fn new(size: Size) -> Self {
        let gpu = gpu();
        Harness {
            renderer: Renderer::new(&gpu.device),
            target: RenderTarget::new(&gpu.device, size),
            size,
        }
    }

    fn upload(&self, frame: &VideoFrame) -> ve_render::GpuTexture {
        let gpu = gpu();
        self.renderer.upload(&gpu.device, &gpu.queue, frame)
    }

    fn render(&mut self, background: Rgba, layers: &[Layer<'_>]) -> Vec<u8> {
        self.render_in(ColorSpace::Perceptual, background, layers)
    }

    fn render_in(
        &mut self,
        color_space: ColorSpace,
        background: Rgba,
        layers: &[Layer<'_>],
    ) -> Vec<u8> {
        let gpu = gpu();
        self.renderer.render(
            &gpu.device,
            &gpu.queue,
            &self.target,
            background,
            color_space,
            layers,
        );
        self.target.read_pixels(&gpu.device, &gpu.queue)
    }

    fn pixel(&self, pixels: &[u8], x: u32, y: u32) -> [u8; 4] {
        RenderTarget::pixel_at(pixels, self.size, x, y)
    }
}

/// Asserts two colours match within the tolerance a rasteriser's rounding needs.
#[track_caller]
fn assert_colour(got: [u8; 4], want: [u8; 4], what: &str) {
    let close = got.iter().zip(want.iter()).all(|(a, b)| (*a as i32 - *b as i32).abs() <= 2);
    assert!(close, "{what}: got {got:?}, expected {want:?}");
}

const RED: [u8; 4] = [255, 0, 0, 255];
const GREEN: [u8; 4] = [0, 255, 0, 255];
const BLUE: [u8; 4] = [0, 0, 255, 255];
const OPAQUE_BLACK: Rgba = Rgba { r: 0.0, g: 0.0, b: 0.0, a: 1.0 };

#[test]
fn an_empty_composition_is_the_background_colour() {
    let size = Size::new(64, 48);
    let mut h = Harness::new(size);
    let pixels = h.render(Rgba::new(0.0, 0.0, 1.0, 1.0), &[]);

    assert_colour(h.pixel(&pixels, 0, 0), BLUE, "top-left");
    assert_colour(h.pixel(&pixels, 63, 47), BLUE, "bottom-right");
    assert_colour(h.pixel(&pixels, 32, 24), BLUE, "centre");
}

#[test]
fn a_matching_layer_fills_the_frame_exactly() {
    let size = Size::new(64, 48);
    let mut h = Harness::new(size);
    let texture = h.upload(&solid_frame(size, RED));
    let pixels = h.render(OPAQUE_BLACK, &[Layer::new(&texture)]);

    for (x, y) in [(0, 0), (63, 0), (0, 47), (63, 47), (32, 24)] {
        assert_colour(h.pixel(&pixels, x, y), RED, &format!("({x}, {y})"));
    }
}

#[test]
fn opacity_blends_the_layer_towards_the_background() {
    let size = Size::new(32, 32);
    let mut h = Harness::new(size);
    let texture = h.upload(&solid_frame(size, RED));

    let transform = TransformState { opacity: 0.5, ..Default::default() };
    let pixels = h.render(OPAQUE_BLACK, &[Layer::new(&texture).with_transform(transform)]);

    // Half-strength red over black, and still fully opaque because the
    // background was.
    assert_colour(h.pixel(&pixels, 16, 16), [128, 0, 0, 255], "50% red over black");
}

#[test]
fn zero_opacity_makes_a_layer_disappear_entirely() {
    let size = Size::new(32, 32);
    let mut h = Harness::new(size);
    let texture = h.upload(&solid_frame(size, RED));

    let transform = TransformState { opacity: 0.0, ..Default::default() };
    let pixels = h.render(
        Rgba::new(0.0, 1.0, 0.0, 1.0),
        &[Layer::new(&texture).with_transform(transform)],
    );
    assert_colour(h.pixel(&pixels, 16, 16), GREEN, "background must show through");
}

#[test]
fn layers_composite_back_to_front() {
    let size = Size::new(32, 32);
    let mut h = Harness::new(size);
    let bottom = h.upload(&solid_frame(size, RED));
    let top = h.upload(&solid_frame(size, GREEN));

    let pixels = h.render(OPAQUE_BLACK, &[Layer::new(&bottom), Layer::new(&top)]);
    assert_colour(h.pixel(&pixels, 16, 16), GREEN, "the later layer must be on top");

    // Reversing the order reverses which one wins.
    let pixels = h.render(OPAQUE_BLACK, &[Layer::new(&top), Layer::new(&bottom)]);
    assert_colour(h.pixel(&pixels, 16, 16), RED, "order must be respected");
}

#[test]
fn a_half_opaque_top_layer_mixes_with_the_one_below() {
    let size = Size::new(32, 32);
    let mut h = Harness::new(size);
    let bottom = h.upload(&solid_frame(size, RED));
    let top = h.upload(&solid_frame(size, GREEN));

    let fade = TransformState { opacity: 0.5, ..Default::default() };
    let pixels =
        h.render(OPAQUE_BLACK, &[Layer::new(&bottom), Layer::new(&top).with_transform(fade)]);
    assert_colour(h.pixel(&pixels, 16, 16), [128, 128, 0, 255], "50% green over red");
}

#[test]
fn a_scaled_down_layer_leaves_the_background_visible_around_it() {
    let size = Size::new(64, 64);
    let mut h = Harness::new(size);
    let texture = h.upload(&solid_frame(size, RED));

    let transform = TransformState { scale: Vec2::splat(0.5), ..Default::default() };
    let pixels = h.render(
        Rgba::new(0.0, 0.0, 1.0, 1.0),
        &[Layer::new(&texture).with_transform(transform)],
    );

    // Centred at half size: the middle is the layer, the corners are not.
    assert_colour(h.pixel(&pixels, 32, 32), RED, "centre");
    assert_colour(h.pixel(&pixels, 2, 2), BLUE, "top-left corner");
    assert_colour(h.pixel(&pixels, 61, 61), BLUE, "bottom-right corner");
    // The layer spans the middle half, so just inside is layer and just
    // outside is background.
    assert_colour(h.pixel(&pixels, 20, 32), RED, "inside the left edge");
    assert_colour(h.pixel(&pixels, 12, 32), BLUE, "outside the left edge");
}

#[test]
fn a_smaller_source_sits_centred_at_its_natural_size() {
    let composition = Size::new(64, 64);
    let mut h = Harness::new(composition);
    // A 32x32 source in a 64x64 composition occupies the middle quarter.
    let texture = h.upload(&solid_frame(Size::new(32, 32), RED));

    let pixels = h.render(Rgba::new(0.0, 0.0, 1.0, 1.0), &[Layer::new(&texture)]);
    assert_colour(h.pixel(&pixels, 32, 32), RED, "centre");
    assert_colour(h.pixel(&pixels, 4, 4), BLUE, "corner");
    assert_colour(h.pixel(&pixels, 20, 32), RED, "inside the source");
    assert_colour(h.pixel(&pixels, 10, 32), BLUE, "outside the source");
}

#[test]
fn position_translates_the_layer() {
    let size = Size::new(64, 64);
    let mut h = Harness::new(size);
    let texture = h.upload(&solid_frame(Size::new(32, 32), RED));

    // Move right and down by a quarter of the composition.
    let transform = TransformState { position: Vec2::new(16.0, 16.0), ..Default::default() };
    let pixels = h.render(
        Rgba::new(0.0, 0.0, 1.0, 1.0),
        &[Layer::new(&texture).with_transform(transform)],
    );

    // Centred, the layer covered x and y in 16..48; moved, it covers 32..64.
    assert_colour(h.pixel(&pixels, 48, 48), RED, "moved centre");
    // Sampled well inside the vacated region rather than on the new edge at
    // exactly 32, where a boundary pixel could legitimately go either way.
    assert_colour(h.pixel(&pixels, 24, 24), BLUE, "where it used to be");
    assert_colour(h.pixel(&pixels, 60, 60), RED, "the far corner it moved into");
}

#[test]
fn a_rotated_layer_covers_the_area_a_rotation_should() {
    let size = Size::new(64, 64);
    let mut h = Harness::new(size);
    // A wide source so the rotation is visible in the coverage.
    let texture = h.upload(&solid_frame(Size::new(64, 16), RED));

    let upright = h.render(Rgba::new(0.0, 0.0, 1.0, 1.0), &[Layer::new(&texture)]);
    // Upright: a horizontal band across the middle.
    assert_colour(h.pixel(&upright, 32, 32), RED, "band centre");
    assert_colour(h.pixel(&upright, 32, 8), BLUE, "above the band");
    assert_colour(h.pixel(&upright, 8, 32), RED, "left of the band centre");

    let transform = TransformState { rotation: 90.0, ..Default::default() };
    let rotated = h.render(
        Rgba::new(0.0, 0.0, 1.0, 1.0),
        &[Layer::new(&texture).with_transform(transform)],
    );
    // Rotated a quarter turn: the band is now vertical.
    assert_colour(h.pixel(&rotated, 32, 32), RED, "band centre");
    assert_colour(h.pixel(&rotated, 32, 8), RED, "above the centre is now band");
    assert_colour(h.pixel(&rotated, 8, 32), BLUE, "left of the centre is now empty");
}

#[test]
fn a_negative_scale_mirrors_rather_than_culling_the_layer() {
    let size = Size::new(32, 32);
    let mut h = Harness::new(size);
    let texture = h.upload(&solid_frame(size, RED));

    let transform = TransformState { scale: Vec2::new(-1.0, 1.0), ..Default::default() };
    let pixels = h.render(OPAQUE_BLACK, &[Layer::new(&texture).with_transform(transform)]);
    // Back-face culling would make a mirrored layer vanish, which is never what
    // a user flipping a clip wants.
    assert_colour(h.pixel(&pixels, 16, 16), RED, "mirrored layer must still draw");
}

#[test]
fn a_transparent_source_lets_the_background_through() {
    let size = Size::new(32, 32);
    let mut h = Harness::new(size);
    // Half-transparent red, as a graphic with an alpha channel would be.
    let texture = h.upload(&solid_frame(size, [255, 0, 0, 128]));

    let pixels = h.render(Rgba::new(0.0, 0.0, 1.0, 1.0), &[Layer::new(&texture)]);
    assert_colour(h.pixel(&pixels, 16, 16), [128, 0, 127, 255], "50% red over blue");
}

#[test]
fn source_alpha_and_layer_opacity_multiply() {
    let size = Size::new(32, 32);
    let mut h = Harness::new(size);
    let texture = h.upload(&solid_frame(size, [255, 0, 0, 128]));

    let transform = TransformState { opacity: 0.5, ..Default::default() };
    let pixels = h.render(OPAQUE_BLACK, &[Layer::new(&texture).with_transform(transform)]);
    // 0.5 source alpha times 0.5 layer opacity is a quarter-strength red.
    assert_colour(h.pixel(&pixels, 16, 16), [64, 0, 0, 255], "25% red over black");
}

#[test]
fn many_layers_render_in_a_single_pass() {
    let size = Size::new(32, 32);
    let mut h = Harness::new(size);
    // More than the uniform buffer's initial capacity, to exercise its growth.
    let textures: Vec<_> = (0..40).map(|_| h.upload(&solid_frame(size, RED))).collect();
    let mut layers: Vec<Layer> = textures.iter().map(Layer::new).collect();

    // Make the last one green so we can tell it was drawn last.
    let green = h.upload(&solid_frame(size, GREEN));
    layers.push(Layer::new(&green));

    let pixels = h.render(OPAQUE_BLACK, &layers);
    assert_colour(h.pixel(&pixels, 16, 16), GREEN, "the 41st layer must be on top");
}

#[test]
fn a_frame_whose_stride_is_not_aligned_uploads_correctly() {
    // 33 pixels is 132 bytes per row, which is not a multiple of the 256-byte
    // copy alignment wgpu requires, so this exercises the repacking path.
    let source = Size::new(33, 17);
    let mut h = Harness::new(source);
    let texture = h.upload(&solid_frame(source, GREEN));
    let pixels = h.render(OPAQUE_BLACK, &[Layer::new(&texture)]);

    assert_colour(h.pixel(&pixels, 0, 0), GREEN, "first pixel");
    assert_colour(h.pixel(&pixels, 32, 16), GREEN, "last pixel");
    assert_colour(h.pixel(&pixels, 16, 8), GREEN, "middle");
}

#[test]
fn a_render_target_resizes_only_when_the_size_changes() {
    let gpu = gpu();
    let mut target = RenderTarget::new(&gpu.device, Size::new(64, 64));
    assert!(!target.resize(&gpu.device, Size::new(64, 64)), "same size is a no-op");
    assert!(target.resize(&gpu.device, Size::new(128, 72)));
    assert_eq!(target.size(), Size::new(128, 72));

    // A zero dimension must not produce an invalid texture.
    assert!(target.resize(&gpu.device, Size::new(0, 0)));
    assert_eq!(target.size(), Size::new(1, 1));
}

#[test]
fn readback_returns_tightly_packed_rows() {
    let gpu = gpu();
    // 33 pixels wide, so the GPU's padded rows differ from tight ones.
    let size = Size::new(33, 5);
    let mut renderer = Renderer::new(&gpu.device);
    let target = RenderTarget::new(&gpu.device, size);
    renderer.render(
        &gpu.device,
        &gpu.queue,
        &target,
        Rgba::new(1.0, 0.0, 0.0, 1.0),
        ColorSpace::Perceptual,
        &[],
    );

    let pixels = target.read_pixels(&gpu.device, &gpu.queue);
    assert_eq!(pixels.len(), (size.width * size.height * 4) as usize);
    // Every pixel is the background, including the ones that would land in the
    // padding if the rows had not been repacked.
    for y in 0..size.height {
        for x in 0..size.width {
            assert_colour(
                RenderTarget::pixel_at(&pixels, size, x, y),
                RED,
                &format!("({x},{y})"),
            );
        }
    }
}

#[test]
fn rendering_records_metrics() {
    let gpu = gpu();
    let metrics = Metrics::new();
    let mut renderer = Renderer::new(&gpu.device).with_metrics(metrics.clone());
    let target = RenderTarget::new(&gpu.device, Size::new(32, 32));
    let frame = solid_frame(Size::new(32, 32), RED);

    let texture = renderer.upload(&gpu.device, &gpu.queue, &frame);
    renderer.render(
        &gpu.device,
        &gpu.queue,
        &target,
        OPAQUE_BLACK,
        ColorSpace::Perceptual,
        &[Layer::new(&texture)],
    );

    assert!(metrics.span_stats(ve_metrics::spans::UPLOAD).is_some());
    assert!(metrics.span_stats(ve_metrics::spans::COMPOSITE).is_some());
}

// ---- the GPU texture cache --------------------------------------------

#[test]
fn the_texture_cache_evicts_under_its_budget() {
    use ve_core::AssetId;
    use ve_media::CacheKey;
    use ve_render::TextureCache;

    let gpu = gpu();
    let renderer = Renderer::new(&gpu.device);
    let size = Size::new(64, 64);
    let bytes_each = 64 * 64 * 4;
    let mut cache = TextureCache::new(bytes_each * 2);

    let key = |i| CacheKey::new(AssetId::from_raw(1), i, 64);
    for i in 0..3 {
        let texture = renderer.upload(&gpu.device, &gpu.queue, &solid_frame(size, RED));
        cache.insert(key(i), texture);
    }

    assert!(cache.len() <= 2, "cache held {} entries over its budget", cache.len());
    assert!(cache.bytes() <= bytes_each * 2);
    assert!(cache.get(&key(2)).is_some(), "the newest entry must survive");
    assert!(cache.get(&key(0)).is_none(), "the oldest must have been evicted");
}

#[test]
fn the_texture_cache_drops_an_assets_textures_on_request() {
    use ve_core::AssetId;
    use ve_media::CacheKey;
    use ve_render::TextureCache;

    let gpu = gpu();
    let renderer = Renderer::new(&gpu.device);
    let mut cache = TextureCache::with_budget_mb(16);
    let size = Size::new(16, 16);

    for asset in 1..=2u64 {
        for frame in 0..3 {
            let texture = renderer.upload(&gpu.device, &gpu.queue, &solid_frame(size, RED));
            cache.insert(CacheKey::new(AssetId::from_raw(asset), frame, 16), texture);
        }
    }
    assert_eq!(cache.len(), 6);

    cache.invalidate_asset(AssetId::from_raw(1));
    assert_eq!(cache.len(), 3);
    assert!(cache.get(&CacheKey::new(AssetId::from_raw(2), 0, 16)).is_some());
    assert_eq!(cache.bytes(), 3 * 16 * 16 * 4);
}

// --- Blend modes -----------------------------------------------------------
//
// Each mode is checked against the arithmetic it claims to implement, with an
// opaque layer over an opaque one so the expected value is a plain function of
// the two colours. `HALF` is 128/255 = 0.502, near enough to a half that the
// products below stay well clear of the ±2 tolerance.

const HALF: u8 = 128;
const GREY: [u8; 4] = [HALF, HALF, HALF, 255];

/// Composites `top` over `bottom` in `mode`, over an opaque black background.
fn blended(mode: BlendMode, bottom: [u8; 4], top: [u8; 4], top_opacity: f64) -> [u8; 4] {
    let size = Size::new(16, 16);
    let mut h = Harness::new(size);
    let under = h.upload(&solid_frame(size, bottom));
    let over = h.upload(&solid_frame(size, top));

    let transform = TransformState { opacity: top_opacity, ..Default::default() };
    let pixels = h.render(
        OPAQUE_BLACK,
        &[Layer::new(&under), Layer::new(&over).with_transform(transform).with_blend(mode)],
    );
    h.pixel(&pixels, 8, 8)
}

#[test]
fn normal_shows_the_top_layer() {
    assert_colour(blended(BlendMode::Normal, GREY, RED, 1.0), RED, "red over grey");
}

#[test]
fn add_sums_the_two_layers() {
    // Channels chosen so nothing clips: the sum is exactly both contributions.
    let blue_ish = [0, 0, HALF, 255];
    let red_ish = [HALF, 0, 0, 255];
    assert_colour(
        blended(BlendMode::Add, blue_ish, red_ish, 1.0),
        [HALF, 0, HALF, 255],
        "half red added to half blue",
    );
}

#[test]
fn add_clips_rather_than_wrapping() {
    // Two thirds plus two thirds is more than white. Saturating is the right
    // answer; wrapping would turn a bright highlight black.
    let bright = [170, 170, 170, 255];
    assert_colour(
        blended(BlendMode::Add, bright, bright, 1.0),
        [255, 255, 255, 255],
        "an overflowing sum",
    );
}

#[test]
fn multiply_darkens_by_the_product() {
    // 0.502 × 0.502 = 0.252, which is 64.
    assert_colour(
        blended(BlendMode::Multiply, GREY, GREY, 1.0),
        [64, 64, 64, 255],
        "half times half",
    );
}

#[test]
fn multiplying_by_white_leaves_the_backdrop_alone() {
    // The identity of the mode, and the reason a white matte is how you mask
    // nothing out.
    assert_colour(
        blended(BlendMode::Multiply, GREY, [255, 255, 255, 255], 1.0),
        GREY,
        "grey multiplied by white",
    );
}

#[test]
fn screen_brightens_by_the_inverse_product() {
    // 0.502 + 0.502 × (1 − 0.502) = 0.752, which is 192.
    assert_colour(
        blended(BlendMode::Screen, GREY, GREY, 1.0),
        [192, 192, 192, 255],
        "half screened onto half",
    );
}

#[test]
fn screening_with_black_leaves_the_backdrop_alone() {
    assert_colour(
        blended(BlendMode::Screen, GREY, [0, 0, 0, 255], 1.0),
        GREY,
        "grey screened with black",
    );
}

#[test]
fn a_blended_layer_still_obeys_its_opacity() {
    // Half-opacity halves the layer's contribution before the mode sees it,
    // because the shader premultiplies. Without that, fading a glow in would
    // jump straight to full brightness.
    let blue_ish = [0, 0, HALF, 255];
    let red_ish = [HALF, 0, 0, 255];
    assert_colour(
        blended(BlendMode::Add, blue_ish, red_ish, 0.5),
        [64, 0, HALF, 255],
        "half-opacity red added to half blue",
    );
}

#[test]
fn a_blend_mode_leaves_the_frame_opaque_where_the_backdrop_was() {
    // Alpha is the union of coverages in every mode: a blend mode says how
    // colour combines, not how much of the frame is covered.
    let got = blended(BlendMode::Multiply, GREY, GREY, 1.0);
    assert_eq!(got[3], 255, "compositing over an opaque backdrop must stay opaque");
}

#[test]
fn multiply_over_a_transparent_background_has_nothing_to_multiply() {
    // Documented behaviour, not an accident: the fixed-function blender cannot
    // scale by the backdrop's alpha, so a multiply layer over emptiness comes
    // out black rather than showing itself as the Porter-Duff form would. The
    // default sequence background is opaque, so this is only reachable when
    // rendering for an alpha export, and the general form needs the destination
    // as a texture — which arrives with nested compositions.
    let size = Size::new(16, 16);
    let mut h = Harness::new(size);
    let over = h.upload(&solid_frame(size, GREY));
    let pixels =
        h.render(Rgba::TRANSPARENT, &[Layer::new(&over).with_blend(BlendMode::Multiply)]);

    let got = h.pixel(&pixels, 8, 8);
    assert_colour(got, [0, 0, 0, 255], "multiply with no backdrop");
}

#[test]
fn modes_can_be_mixed_within_one_composition() {
    // The pipeline is only rebound when the mode changes, so a composition that
    // alternates modes has to still draw each layer with its own.
    let size = Size::new(16, 16);
    let mut h = Harness::new(size);
    let grey = h.upload(&solid_frame(size, GREY));
    let white = h.upload(&solid_frame(size, [255, 255, 255, 255]));

    let pixels = h.render(
        OPAQUE_BLACK,
        &[
            // Grey, then multiplied by white (no change), then screened with
            // itself: 0.502 → 0.502 → 0.752.
            Layer::new(&grey),
            Layer::new(&white).with_blend(BlendMode::Multiply),
            Layer::new(&grey).with_blend(BlendMode::Screen),
        ],
    );
    assert_colour(h.pixel(&pixels, 8, 8), [192, 192, 192, 255], "three modes in one pass");
}

// ---- linear-light compositing -----------------------------------------

/// The sRGB transfer function, as the hardware applies it. Duplicated here on
/// purpose: a test that reuses the renderer's own conversion would agree with
/// it even if both were wrong.
fn srgb_to_linear(encoded: f64) -> f64 {
    if encoded <= 0.040_45 {
        encoded / 12.92
    } else {
        ((encoded + 0.055) / 1.055).powf(2.4)
    }
}

fn linear_to_srgb(linear: f64) -> f64 {
    if linear <= 0.003_130_8 {
        linear * 12.92
    } else {
        1.055 * linear.powf(1.0 / 2.4) - 0.055
    }
}

/// Half of `value` in linear light, returned as an 8-bit encoded level.
fn half_in_linear(value: u8) -> u8 {
    let linear = srgb_to_linear(value as f64 / 255.0);
    (linear_to_srgb(linear * 0.5) * 255.0).round() as u8
}

#[test]
fn a_half_opaque_layer_over_black_differs_between_the_two_spaces() {
    let size = Size::new(32, 32);
    let mut h = Harness::new(size);
    let texture = h.upload(&solid_frame(size, [255, 255, 255, 255]));
    let fade = TransformState { opacity: 0.5, ..Default::default() };
    let layer = [Layer::new(&texture).with_transform(fade)];

    // Perceptual: the encoded values are halved, so white becomes 128.
    let perceptual = h.render_in(ColorSpace::Perceptual, OPAQUE_BLACK, &layer);
    assert_colour(h.pixel(&perceptual, 16, 16), [128, 128, 128, 255], "perceptual half");

    // Linear: half the *light*, which re-encodes to about 188 — visibly
    // brighter, and the whole reason this is a setting rather than a constant.
    let expected = half_in_linear(255);
    let linear = h.render_in(ColorSpace::Linear, OPAQUE_BLACK, &layer);
    assert_colour(h.pixel(&linear, 16, 16), [expected, expected, expected, 255], "linear half");
    assert!(expected > 180, "half of white in linear light should be near 188, got {expected}");
}

#[test]
fn a_dissolve_between_two_pictures_lands_where_each_space_says_it_should() {
    let size = Size::new(32, 32);
    let mut h = Harness::new(size);
    let bottom = h.upload(&solid_frame(size, [0, 0, 0, 255]));
    let top = h.upload(&solid_frame(size, [255, 255, 255, 255]));
    let fade = TransformState { opacity: 0.5, ..Default::default() };
    let layers = [Layer::new(&bottom), Layer::new(&top).with_transform(fade)];

    let perceptual = h.render_in(ColorSpace::Perceptual, OPAQUE_BLACK, &layers);
    assert_colour(h.pixel(&perceptual, 16, 16), [128, 128, 128, 255], "perceptual midpoint");

    let expected = half_in_linear(255);
    let linear = h.render_in(ColorSpace::Linear, OPAQUE_BLACK, &layers);
    assert_colour(
        h.pixel(&linear, 16, 16),
        [expected, expected, expected, 255],
        "linear midpoint",
    );
}

#[test]
fn an_opaque_layer_is_identical_in_both_spaces() {
    // Nothing is blended, so nothing depends on the space. If this ever
    // differs, the conversion is being applied where it should not be — which
    // would mean every untouched clip in a linear sequence shifted colour.
    let size = Size::new(32, 32);
    let mut h = Harness::new(size);

    for colour in [[255u8, 0, 0, 255], [64, 128, 192, 255], [0, 0, 0, 255]] {
        let texture = h.upload(&solid_frame(size, colour));
        let layer = [Layer::new(&texture)];
        let perceptual = h.render_in(ColorSpace::Perceptual, OPAQUE_BLACK, &layer);
        let linear = h.render_in(ColorSpace::Linear, OPAQUE_BLACK, &layer);
        assert_colour(h.pixel(&perceptual, 16, 16), colour, "perceptual passthrough");
        assert_colour(h.pixel(&linear, 16, 16), colour, "linear passthrough");
    }
}

#[test]
fn additive_highlights_clip_later_in_linear_light() {
    let size = Size::new(32, 32);
    let mut h = Harness::new(size);
    // Two mid-grey layers added together.
    let texture = h.upload(&solid_frame(size, [128, 128, 128, 255]));
    let layers =
        [Layer::new(&texture), Layer::new(&texture).with_blend(ve_core::BlendMode::Add)];

    // Perceptual adds the codes: 128 + 128 saturates at 255.
    let perceptual = h.render_in(ColorSpace::Perceptual, OPAQUE_BLACK, &layers);
    assert_colour(h.pixel(&perceptual, 16, 16), [255, 255, 255, 255], "perceptual add");

    // Linear adds the light: 0.216 + 0.216 is 0.432, which encodes to about
    // 188 — still plenty of headroom.
    let doubled = srgb_to_linear(128.0 / 255.0) * 2.0;
    let expected = (linear_to_srgb(doubled) * 255.0).round() as u8;
    let linear = h.render_in(ColorSpace::Linear, OPAQUE_BLACK, &layers);
    assert_colour(h.pixel(&linear, 16, 16), [expected, expected, expected, 255], "linear add");
    assert!(expected < 255, "linear add should not have clipped, got {expected}");
}

#[test]
fn a_background_keeps_its_colour_in_linear_light() {
    // The clear value bypasses the attachment's encode, so it has to be encoded
    // by hand. Getting that wrong turns a mid-grey background near-black.
    let size = Size::new(16, 16);
    let mut h = Harness::new(size);
    let grey = Rgba::new(0.5, 0.5, 0.5, 1.0);

    let perceptual = h.render_in(ColorSpace::Perceptual, grey, &[]);
    let linear = h.render_in(ColorSpace::Linear, grey, &[]);
    assert_colour(h.pixel(&linear, 8, 8), h.pixel(&perceptual, 8, 8), "background colour");
}

#[test]
fn the_composite_key_separates_the_two_spaces() {
    // The cache is content-addressed, so the space has to be part of the
    // content. Without this, switching the setting would leave the previous
    // picture on screen until some other input happened to change.
    use ve_render::CompositeKey;

    let gpu = gpu();
    let renderer = Renderer::new(&gpu.device);
    let size = Size::new(16, 16);
    let texture = renderer.upload(&gpu.device, &gpu.queue, &solid_frame(size, RED));
    let layers = [Layer::new(&texture)];

    let perceptual = CompositeKey::of(size, OPAQUE_BLACK, ColorSpace::Perceptual, &layers);
    let linear = CompositeKey::of(size, OPAQUE_BLACK, ColorSpace::Linear, &layers);
    assert_ne!(perceptual, linear, "the same layers in two spaces are two pictures");

    // And the key is still stable for the same inputs.
    assert_eq!(
        perceptual,
        CompositeKey::of(size, OPAQUE_BLACK, ColorSpace::Perceptual, &layers)
    );
}
