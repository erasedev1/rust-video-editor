//! Tests for the render cache, on a real device.
//!
//! The behaviour that matters here is not "does the hash map work" but "does a
//! hit give back the picture that was stored, and does an edit invalidate
//! exactly the instants it should". Both need real composites, so these run
//! against whatever adapter is available — including a software rasteriser.

use std::sync::Arc;

use ve_core::{BlendMode, Rgba, Size, TransformState, Vec2};
use ve_media::{PixelFormat, VideoFrame};
use ve_metrics::{counters, Metrics};
use ve_render::{
    CompositeCache, CompositeKey, GpuContext, GpuTexture, Layer, RenderTarget, Renderer,
};
use ve_time::Ticks;

/// A GPU context shared by every test in the process.
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

const SIZE: Size = Size { width: 32, height: 32 };
const RED: [u8; 4] = [255, 0, 0, 255];
const GREEN: [u8; 4] = [0, 255, 0, 255];
const BLACK: Rgba = Rgba { r: 0.0, g: 0.0, b: 0.0, a: 1.0 };

struct Harness {
    renderer: Renderer,
    cache: CompositeCache,
    /// Stands in for the texture the interface draws from.
    present: RenderTarget,
}

impl Harness {
    fn new(capacity_bytes: usize) -> Self {
        let gpu = gpu();
        Harness {
            renderer: Renderer::new(&gpu.device),
            cache: CompositeCache::new(capacity_bytes),
            present: RenderTarget::new(&gpu.device, SIZE),
        }
    }

    fn upload(&self, colour: [u8; 4]) -> GpuTexture {
        let gpu = gpu();
        self.renderer.upload(&gpu.device, &gpu.queue, &solid_frame(SIZE, colour))
    }

    /// The whole preview path for one instant: hit the cache if possible,
    /// otherwise composite and store. Returns whether it was a hit.
    fn present(&mut self, background: Rgba, layers: &[Layer<'_>]) -> bool {
        let gpu = gpu();
        let key = CompositeKey::of(SIZE, background, layers);
        if let Some(cached) = self.cache.get(&key) {
            assert!(self.present.blit_from(&gpu.device, &gpu.queue, cached));
            return true;
        }
        let scratch = self.cache.take_target(&gpu.device, SIZE);
        self.renderer.render(&gpu.device, &gpu.queue, &scratch, background, layers);
        assert!(self.present.blit_from(&gpu.device, &gpu.queue, &scratch));
        self.cache.insert(key, scratch);
        false
    }

    /// The colour at the centre of what the interface would be drawing.
    fn centre(&self) -> [u8; 4] {
        let gpu = gpu();
        let pixels = self.present.read_pixels(&gpu.device, &gpu.queue);
        RenderTarget::pixel_at(&pixels, SIZE, SIZE.width / 2, SIZE.height / 2)
    }
}

#[track_caller]
fn assert_colour(got: [u8; 4], want: [u8; 4], what: &str) {
    let close = got.iter().zip(want.iter()).all(|(a, b)| (*a as i32 - *b as i32).abs() <= 2);
    assert!(close, "{what}: got {got:?}, expected {want:?}");
}

#[test]
fn the_same_composition_hashes_the_same_way_twice() {
    let h = Harness::new(1 << 20);
    let texture = h.upload(RED);
    let layers = [Layer::new(&texture)];

    let first = CompositeKey::of(SIZE, BLACK, &layers);
    let second = CompositeKey::of(SIZE, BLACK, &layers);
    assert_eq!(first, second, "a composition must be its own cache key");
}

#[test]
fn every_input_the_compositor_reads_is_part_of_the_key() {
    let h = Harness::new(1 << 20);
    let red = h.upload(RED);
    let green = h.upload(GREEN);

    let base = CompositeKey::of(SIZE, BLACK, &[Layer::new(&red)]);

    let moved = TransformState { position: Vec2::new(4.0, 0.0), ..Default::default() };
    let faded = TransformState { opacity: 0.5, ..Default::default() };

    let cases: [(&str, CompositeKey); 7] = [
        ("a different source", CompositeKey::of(SIZE, BLACK, &[Layer::new(&green)])),
        (
            "a moved layer",
            CompositeKey::of(SIZE, BLACK, &[Layer::new(&red).with_transform(moved)]),
        ),
        (
            "a faded layer",
            CompositeKey::of(SIZE, BLACK, &[Layer::new(&red).with_transform(faded)]),
        ),
        ("another background", CompositeKey::of(SIZE, Rgba::WHITE, &[Layer::new(&red)])),
        ("another size", CompositeKey::of(Size::new(64, 64), BLACK, &[Layer::new(&red)])),
        (
            "an extra layer",
            CompositeKey::of(SIZE, BLACK, &[Layer::new(&red), Layer::new(&green)]),
        ),
        (
            "another blend mode",
            CompositeKey::of(SIZE, BLACK, &[Layer::new(&red).with_blend(BlendMode::Screen)]),
        ),
    ];
    for (what, key) in cases {
        assert_ne!(base, key, "{what} must not hash the same as the original");
    }
}

#[test]
fn layer_order_is_part_of_the_key() {
    let h = Harness::new(1 << 20);
    let red = h.upload(RED);
    let green = h.upload(GREEN);

    // Swapping two layers changes which one is on top, so it has to change the
    // key even though the set of layers is identical.
    let bottom_red = CompositeKey::of(SIZE, BLACK, &[Layer::new(&red), Layer::new(&green)]);
    let bottom_green = CompositeKey::of(SIZE, BLACK, &[Layer::new(&green), Layer::new(&red)]);
    assert_ne!(bottom_red, bottom_green);
}

#[test]
fn an_opacity_above_one_hashes_as_fully_opaque() {
    let h = Harness::new(1 << 20);
    let texture = h.upload(RED);

    // The renderer clamps, so these two draw the same picture and must share an
    // entry rather than each paying for a composite.
    let opaque = TransformState { opacity: 1.0, ..Default::default() };
    let louder = TransformState { opacity: 4.0, ..Default::default() };
    assert_eq!(
        CompositeKey::of(SIZE, BLACK, &[Layer::new(&texture).with_transform(opaque)]),
        CompositeKey::of(SIZE, BLACK, &[Layer::new(&texture).with_transform(louder)]),
    );
}

#[test]
fn a_hit_gives_back_the_picture_that_was_stored() {
    let mut h = Harness::new(1 << 20);
    let red = h.upload(RED);
    let green = h.upload(GREEN);

    assert!(!h.present(BLACK, &[Layer::new(&red)]), "the first composite is a miss");
    assert_colour(h.centre(), RED, "the freshly composited picture");

    // Something else, then back again: the second visit must come from the
    // cache and must still be red rather than whatever was drawn in between.
    assert!(!h.present(BLACK, &[Layer::new(&green)]));
    assert_colour(h.centre(), GREEN, "the second composite");

    assert!(h.present(BLACK, &[Layer::new(&red)]), "the revisit should hit");
    assert_colour(h.centre(), RED, "the picture restored from the cache");

    let stats = h.cache.stats();
    assert_eq!(stats.entries, 2);
    assert_eq!(stats.hits, 1);
}

/// The point of the whole design: an edit costs only the instants it changes.
#[test]
fn changing_one_clip_leaves_the_instants_without_it_cached() {
    let mut h = Harness::new(1 << 20);
    // Two instants of a two-track sequence: one shows the red clip over the
    // green one, the other shows the green clip alone.
    let red = h.upload(RED);
    let green = h.upload(GREEN);

    fn untouched<'a>(texture: &'a GpuTexture) -> Vec<Layer<'a>> {
        vec![Layer::new(texture)]
    }
    fn edited<'a>(
        red: &'a GpuTexture,
        green: &'a GpuTexture,
        transform: TransformState,
    ) -> Vec<Layer<'a>> {
        vec![Layer::new(green), Layer::new(red).with_transform(transform)]
    }

    let original = TransformState::default();
    h.present(BLACK, &edited(&red, &green, original));
    h.present(BLACK, &untouched(&green));

    // Now the red clip's opacity is dragged in the inspector. Only the instant
    // it appears in has to be composited again.
    let after_edit = TransformState { opacity: 0.25, ..Default::default() };
    assert!(
        !h.present(BLACK, &edited(&red, &green, after_edit)),
        "the instant the edited clip appears in must be recomposited"
    );
    assert!(
        h.present(BLACK, &untouched(&green)),
        "an instant the edited clip is not in must still be a hit"
    );
}

#[test]
fn re_uploading_a_frame_is_treated_as_a_different_picture() {
    let mut h = Harness::new(1 << 20);
    let first = h.upload(RED);
    h.present(BLACK, &[Layer::new(&first)]);

    // A texture evicted from the texture cache and uploaded again is a new
    // object, and the cache cannot know its pixels are the same ones. The cost
    // of that is one extra composite; the alternative is hashing every pixel of
    // every frame, which is the thing the cache exists to avoid.
    let again = h.upload(RED);
    assert!(!h.present(BLACK, &[Layer::new(&again)]));
}

#[test]
fn eviction_keeps_the_cache_inside_its_budget() {
    // Room for two 32x32 RGBA pictures.
    let picture_bytes = (SIZE.pixel_count() * 4) as usize;
    let mut h = Harness::new(picture_bytes * 2);

    let textures: Vec<GpuTexture> = [RED, GREEN, [0, 0, 255, 255], [255, 255, 0, 255]]
        .iter()
        .map(|c| h.upload(*c))
        .collect();
    for texture in &textures {
        h.present(BLACK, &[Layer::new(texture)]);
    }

    let stats = h.cache.stats();
    assert_eq!(stats.entries, 2, "the budget allows two pictures");
    assert!(stats.bytes <= picture_bytes * 2);
    assert!(stats.evictions >= 2, "the older pictures should have gone");

    // The first picture is gone, so revisiting it is a miss; the most recent
    // one is still there.
    assert!(!h.present(BLACK, &[Layer::new(&textures[0])]));
    assert!(h.present(BLACK, &[Layer::new(&textures[3])]));
}

#[test]
fn an_evicted_target_is_kept_for_the_next_composite_rather_than_freed() {
    let picture_bytes = (SIZE.pixel_count() * 4) as usize;
    let mut h = Harness::new(picture_bytes);

    let red = h.upload(RED);
    let green = h.upload(GREEN);
    h.present(BLACK, &[Layer::new(&red)]);
    // This one evicts the first, which must leave a target ready to reuse:
    // allocating a full-resolution texture per frame during playback would cost
    // more than the composite the cache is saving.
    h.present(BLACK, &[Layer::new(&green)]);

    let gpu = gpu();
    let before = h.cache.stats().spare;
    let reused = h.cache.take_target(&gpu.device, SIZE);
    assert!(before > 0, "eviction should have left a spare target");
    assert_eq!(h.cache.stats().spare, before - 1, "the spare should have been handed out");
    assert_eq!(reused.size(), SIZE);
}

#[test]
fn a_discarded_target_is_kept_without_caching_what_was_drawn() {
    let mut h = Harness::new(1 << 20);
    let gpu = gpu();
    let target = h.cache.take_target(&gpu.device, SIZE);

    h.cache.discard_target(target);
    assert_eq!(h.cache.len(), 0, "discarding must not create an entry");
    assert_eq!(h.cache.stats().spare, 1, "but the target is still worth keeping");
}

#[test]
fn pictures_of_different_sizes_coexist() {
    let mut h = Harness::new(1 << 20);
    let red = h.upload(RED);
    h.present(BLACK, &[Layer::new(&red)]);
    assert_eq!(h.cache.len(), 1);

    // Nesting means several sizes are in flight at once: a 1080p sequence can
    // hold a 512-pixel title composition, and both are cached during the same
    // frame. The size is part of every key, so they cannot be confused.
    let gpu = gpu();
    let bigger = h.cache.take_target(&gpu.device, Size::new(64, 64));
    assert_eq!(bigger.size(), Size::new(64, 64));
    assert_eq!(h.cache.len(), 1, "the smaller picture is still cached");

    let key = CompositeKey::of(Size::new(64, 64), BLACK, &[]);
    h.cache.insert(key, bigger);
    assert_eq!(h.cache.len(), 2);
    assert!(h.cache.get(&key).is_some());
}

#[test]
fn a_spare_target_is_only_reused_at_its_own_size() {
    let mut h = Harness::new(1 << 20);
    let gpu = gpu();

    // Hand back a small target, then ask for a large one: drawing a 64-pixel
    // composite into a 32-pixel target would silently crop it.
    let small = h.cache.take_target(&gpu.device, SIZE);
    h.cache.discard_target(small);
    let large = h.cache.take_target(&gpu.device, Size::new(64, 64));
    assert_eq!(large.size(), Size::new(64, 64));
    assert_eq!(h.cache.stats().spare, 1, "the small one is still spare");

    // And asking for its size again hands the spare straight back.
    let small = h.cache.take_target(&gpu.device, SIZE);
    assert_eq!(small.size(), SIZE);
    assert_eq!(h.cache.stats().spare, 0);
}

#[test]
fn a_picture_larger_than_the_whole_budget_is_not_cached() {
    let mut h = Harness::new(16);
    let red = h.upload(RED);

    // It still has to be drawn and presented; it just must not evict
    // everything else and then fail to fit.
    assert!(!h.present(BLACK, &[Layer::new(&red)]));
    assert_colour(h.centre(), RED, "an uncacheable picture still reaches the screen");
    assert_eq!(h.cache.len(), 0);
}

#[test]
fn clearing_empties_the_cache_but_keeps_the_budget() {
    let mut h = Harness::new(1 << 20);
    let red = h.upload(RED);
    h.present(BLACK, &[Layer::new(&red)]);

    h.cache.clear();
    assert!(h.cache.is_empty());
    assert_eq!(h.cache.bytes(), 0);
    assert_eq!(h.cache.stats().capacity_bytes, 1 << 20);
}

#[test]
fn hits_and_misses_are_reported_to_the_metrics() {
    let gpu = gpu();
    let metrics = Metrics::new();
    let mut cache = CompositeCache::new(1 << 20).with_metrics(metrics.clone());
    let renderer = Renderer::new(&gpu.device);
    let texture = renderer.upload(&gpu.device, &gpu.queue, &solid_frame(SIZE, RED));

    let layers = [Layer::new(&texture)];
    let key = CompositeKey::of(SIZE, BLACK, &layers);

    assert!(cache.get(&key).is_none());
    let target = cache.take_target(&gpu.device, SIZE);
    cache.insert(key, target);
    assert!(cache.get(&key).is_some());

    assert_eq!(metrics.counter(counters::COMPOSITE_CACHE_MISS), 1);
    assert_eq!(metrics.counter(counters::COMPOSITE_CACHE_HIT), 1);
    assert_eq!(metrics.gauge(counters::COMPOSITE_CACHE_ENTRIES), Some(1.0));
    assert!(metrics.gauge(counters::COMPOSITE_CACHE_BYTES).unwrap_or(0.0) > 0.0);
}

#[test]
fn a_blit_between_mismatched_targets_is_refused() {
    let gpu = gpu();
    let small = RenderTarget::new(&gpu.device, SIZE);
    let large = RenderTarget::new(&gpu.device, Size::new(64, 64));
    let also_large = RenderTarget::new(&gpu.device, Size::new(64, 64));

    // Copying part of one picture over another would put a half-updated frame
    // on screen, which is worse than not updating at all.
    assert!(!large.blit_from(&gpu.device, &gpu.queue, &small));
    assert!(large.blit_from(&gpu.device, &gpu.queue, &also_large));
    // A target already holds its own picture, and asking the driver to copy a
    // texture over itself is a validation error rather than a no-op.
    assert!(large.blit_from(&gpu.device, &gpu.queue, &large));
}

#[test]
fn uploaded_textures_have_distinct_identities() {
    let h = Harness::new(1 << 20);
    let a = h.upload(RED);
    let b = h.upload(RED);
    assert_ne!(a.id(), b.id(), "two uploads are two pictures as far as the cache knows");
    assert_eq!(a.id(), a.id());
}

#[test]
fn changing_only_the_blend_mode_recomposites_that_instant() {
    let mut h = Harness::new(1 << 20);
    let grey = h.upload([128, 128, 128, 255]);
    let under = h.upload(GREEN);

    fn composite<'a>(
        mode: BlendMode,
        under: &'a GpuTexture,
        over: &'a GpuTexture,
    ) -> Vec<Layer<'a>> {
        vec![Layer::new(under), Layer::new(over).with_blend(mode)]
    }

    assert!(!h.present(BLACK, &composite(BlendMode::Normal, &under, &grey)));
    let normal = h.centre();

    // The layers, their transforms and the background are all identical; only
    // the mode changed. A cache that keyed on the layer set alone would show the
    // stale picture here.
    assert!(!h.present(BLACK, &composite(BlendMode::Screen, &under, &grey)));
    let screened = h.centre();
    assert_ne!(normal, screened, "the mode has to reach the picture");

    assert!(h.present(BLACK, &composite(BlendMode::Normal, &under, &grey)), "back to a hit");
    assert_colour(h.centre(), normal, "the first mode's picture, restored");
}

// ---- nesting ------------------------------------------------------------
//
// A nested composition is drawn into a target of its own, which the level above
// samples as an ordinary layer. These exercise that path the way the preview
// walks it: child first, then parent, with both cached separately.

impl Harness {
    /// Composites into a cached target of its own and returns its key, the way
    /// the preview does for a nested composition.
    fn render_nested(&mut self, background: Rgba, layers: &[Layer<'_>]) -> CompositeKey {
        let gpu = gpu();
        let key = CompositeKey::of(SIZE, background, layers);
        if self.cache.touch(&key) {
            return key;
        }
        let target = self.cache.take_target(&gpu.device, SIZE);
        self.renderer.render(&gpu.device, &gpu.queue, &target, background, layers);
        self.cache.insert(key, target);
        key
    }

    /// Binds a cached composite so it can be drawn as a layer.
    fn bind(&self, key: CompositeKey) -> GpuTexture {
        let gpu = gpu();
        let target = self.cache.peek(&key).expect("the nested composite was just rendered");
        self.renderer.bind_target(&gpu.device, target, key)
    }
}

#[test]
fn a_nested_composite_is_sampled_as_an_ordinary_layer() {
    let mut h = Harness::new(1 << 20);
    let red = h.upload(RED);

    // A composition holding red, drawn into its own target…
    let nested = h.render_nested(Rgba::TRANSPARENT, &[Layer::new(&red)]);
    // …then used as the only layer of the composition above it.
    let texture = h.bind(nested);
    assert!(!h.present(BLACK, &[Layer::new(&texture)]));

    assert_colour(h.centre(), RED, "the nested picture reached the parent");
}

#[test]
fn a_nested_composite_blends_into_its_parent() {
    let mut h = Harness::new(1 << 20);
    let grey = h.upload([128, 128, 128, 255]);
    let under = h.upload(GREEN);

    let nested = h.render_nested(Rgba::TRANSPARENT, &[Layer::new(&grey)]);
    let texture = h.bind(nested);

    // Screened over green: the nested composite is a layer like any other, so
    // the blend modes apply to it unchanged.
    h.present(BLACK, &[Layer::new(&under), Layer::new(&texture).with_blend(BlendMode::Screen)]);
    let got = h.centre();
    assert!(got[0] > 100, "red channel brightened by the screen: {got:?}");
    assert_eq!(got[1], 255, "green stays full");
}

#[test]
fn an_unchanged_nest_keeps_the_parent_cached() {
    let mut h = Harness::new(1 << 20);
    let red = h.upload(RED);
    let green = h.upload(GREEN);

    // Frame one: composite the nest, then the parent.
    let nested = h.render_nested(Rgba::TRANSPARENT, &[Layer::new(&red)]);
    let texture = h.bind(nested);
    assert!(!h.present(BLACK, &[Layer::new(&texture)]));

    // Frame two, nothing changed: the nest is a hit, and because its key is what
    // identifies the texture the parent samples, the parent is a hit too.
    let again = h.render_nested(Rgba::TRANSPARENT, &[Layer::new(&red)]);
    assert_eq!(again, nested, "the same nested contents hash the same");
    let texture = h.bind(again);
    assert!(h.present(BLACK, &[Layer::new(&texture)]), "the parent should not be redrawn");

    // Frame three: change one layer *inside* the nest. The parent has to
    // recomposite even though nothing about the parent itself changed.
    let changed = h.render_nested(Rgba::TRANSPARENT, &[Layer::new(&green)]);
    assert_ne!(changed, nested);
    let texture = h.bind(changed);
    assert!(
        !h.present(BLACK, &[Layer::new(&texture)]),
        "a changed nest invalidates the parent"
    );
    assert_colour(h.centre(), GREEN, "and the new picture is what reaches the screen");
}

#[test]
fn a_reused_target_is_not_mistaken_for_an_unchanged_picture() {
    // The trap this design avoids: nested targets come from a pool, so the same
    // texture object is drawn into again and again. If the parent's key were
    // built from the texture's own identity, the parent would hit its cache and
    // show the previous frame for ever.
    let picture_bytes = (SIZE.pixel_count() * 4) as usize;
    let mut h = Harness::new(picture_bytes * 2);
    let red = h.upload(RED);
    let green = h.upload(GREEN);

    let first = h.render_nested(Rgba::TRANSPARENT, &[Layer::new(&red)]);
    let first_texture = h.bind(first).id();
    let second = h.render_nested(Rgba::TRANSPARENT, &[Layer::new(&green)]);
    let second_texture = h.bind(second).id();

    assert_ne!(first_texture, second_texture, "two pictures, two identities");
}

#[test]
fn a_nested_composite_can_be_a_different_size_from_its_parent() {
    let gpu = gpu();
    let mut cache = CompositeCache::new(1 << 22);
    let renderer = Renderer::new(&gpu.device);
    let small = Size::new(16, 16);

    // A title card at a quarter of the frame, composited into its own target and
    // then drawn into the larger one. Both live in the cache at once.
    let frame = solid_frame(small, RED);
    let texture = renderer.upload(&gpu.device, &gpu.queue, &frame);
    let nested_key = CompositeKey::of(small, Rgba::TRANSPARENT, &[Layer::new(&texture)]);
    let nested_target = cache.take_target(&gpu.device, small);
    assert_eq!(nested_target.size(), small);
    cache.insert(nested_key, nested_target);

    let root_target = cache.take_target(&gpu.device, SIZE);
    assert_eq!(root_target.size(), SIZE);
    let root_key = CompositeKey::of(SIZE, BLACK, &[]);
    cache.insert(root_key, root_target);

    assert_eq!(cache.len(), 2);
    assert!(cache.get(&nested_key).is_some());
    assert!(cache.get(&root_key).is_some());
}
