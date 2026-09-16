//! Compositing tests that render on a real GPU device and read the pixels back.
//!
//! These run against whatever adapter is available, including a software
//! rasteriser such as Mesa's lavapipe, so continuous integration exercises the
//! actual shader and blend state rather than skipping the renderer.

use std::sync::Arc;

use ve_core::{Rgba, Size, TransformState, Vec2};
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
        let gpu = gpu();
        self.renderer.render(&gpu.device, &gpu.queue, &self.target, background, layers);
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
    renderer.render(&gpu.device, &gpu.queue, &target, Rgba::new(1.0, 0.0, 0.0, 1.0), &[]);

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
    renderer.render(&gpu.device, &gpu.queue, &target, OPAQUE_BLACK, &[Layer::new(&texture)]);

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
