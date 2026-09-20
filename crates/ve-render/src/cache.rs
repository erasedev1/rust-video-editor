//! Caching composited pictures, so an unchanged composition is not redrawn.
//!
//! # Content addressing instead of dependency tracking
//!
//! The obvious way to build a render cache is to record what each cached frame
//! depended on, then walk those dependencies whenever an edit happens and throw
//! away whatever the edit touched. That design has a failure mode that is very
//! hard to test for: a dependency nobody declared. Add a property to the model,
//! forget to list it, and the editor shows a stale picture — the worst class of
//! bug a compositor can have, because it looks like the edit did not work.
//!
//! So a cache entry is keyed on a hash of *everything the compositor reads* to
//! produce it: the target size, the background, and for each layer the identity
//! of its source texture, the transform applied to it and its blend mode. Two
//! instants that
//! composite to the same pixels have the same key by construction, and anything
//! that would change the picture changes the key.
//!
//! Incremental invalidation then falls out rather than being implemented.
//! Changing one clip changes the key of every instant that clip is visible at,
//! so those instants are recomputed, while every other cached instant keeps its
//! key and is still a hit. Nothing has to be enumerated, so nothing can be
//! forgotten. Entries whose keys will never be asked for again are not deleted
//! eagerly; they age out as the least recently used, which is the same
//! mechanism that bounds the cache anyway.
//!
//! # What a hit saves
//!
//! Today a hit saves a render pass and its draw calls, and the copy that
//! replaces it is a single blit. That is a modest saving with two layers and a
//! large one with twenty; it becomes the difference between an interactive and
//! an unusable preview once effect passes, nested compositions and colour
//! management run between the layer draws, which is what the rest of this phase
//! adds. The cheapest case is handled without the cache at all: when the
//! composition has not changed since the last repaint the preview skips
//! everything, because the picture it already presented is still correct.

use std::collections::HashMap;
use std::hash::{Hash, Hasher};

use ve_core::{ColorSpace, Rgba, Size, TransformState};
use ve_metrics::{counters, Metrics};

use crate::renderer::Layer;
use crate::target::RenderTarget;

/// What kind of pass a key describes, so that two passes reading the same
/// inputs cannot share a key.
const COMPOSITE: u8 = 0;
const AVERAGE: u8 = 1;
const EFFECT: u8 = 2;

/// Identifies a composited picture by its contents.
///
/// A 64-bit hash, so two different compositions could in principle collide and
/// show the wrong picture. At one composite per frame that is roughly one
/// collision per ten million years of continuous editing, which is a better
/// risk than the stale-frame bugs that dependency tracking invites.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct CompositeKey(u64);

impl CompositeKey {
    /// Hashes the inputs of one composite.
    ///
    /// Takes exactly what [`crate::Renderer::render`] takes, so the key cannot
    /// drift away from what is actually drawn: any new input the compositor
    /// starts reading has to be threaded through here to reach the shader.
    pub fn of(
        size: Size,
        background: Rgba,
        color_space: ColorSpace,
        layers: &[Layer<'_>],
    ) -> Self {
        Self::hash_inputs(COMPOSITE, size, background, color_space, layers)
    }

    /// The key of a motion-blurred layer: the mean of `samples` over
    /// transparency, which is what [`crate::Renderer::accumulate`] draws.
    ///
    /// Tagged differently from a composite of the same layers, because the two
    /// read the same inputs and produce different pictures — summing weighted
    /// samples is not stacking them — and a key that could not tell them apart
    /// would hand one out for the other.
    pub fn of_average(size: Size, color_space: ColorSpace, samples: &[Layer<'_>]) -> Self {
        Self::hash_inputs(AVERAGE, size, Rgba::TRANSPARENT, color_space, samples)
    }

    /// The key of one effect pass's output: a source texture, drawn through a
    /// pass, at a size and in a colour space.
    ///
    /// The whole uniform is hashed rather than the parameters the caller
    /// thought were interesting, so a number that reaches the shader cannot
    /// fail to reach the key — which is the same rule the composite key
    /// follows, and the reason neither can go stale.
    ///
    /// A chain is keyed by construction: the second pass's source is the first
    /// pass's output, whose identity is *its* key, so changing the first
    /// parameter of a five-effect chain changes every key after it and nothing
    /// before it.
    pub fn of_effect(
        source: crate::texture::TextureId,
        size: Size,
        color_space: ColorSpace,
        pass: &crate::effects::EffectPass,
    ) -> Self {
        let mut hasher = std::collections::hash_map::DefaultHasher::new();
        EFFECT.hash(&mut hasher);
        source.hash(&mut hasher);
        size.hash(&mut hasher);
        color_space.hash(&mut hasher);
        pass.program().hash(&mut hasher);
        pass.uniform_bytes().hash(&mut hasher);
        CompositeKey(hasher.finish())
    }

    fn hash_inputs(
        tag: u8,
        size: Size,
        background: Rgba,
        color_space: ColorSpace,
        layers: &[Layer<'_>],
    ) -> Self {
        let mut hasher = std::collections::hash_map::DefaultHasher::new();
        tag.hash(&mut hasher);
        size.hash(&mut hasher);
        hash_colour(background, &mut hasher);
        // The same layers composited in the other space are a different
        // picture. Leaving this out would make switching the setting a no-op
        // on screen until something else happened to change the key.
        color_space.hash(&mut hasher);
        // The count is hashed as well as each layer, so a trailing transparent
        // layer cannot hash the same as no layer at all.
        layers.len().hash(&mut hasher);
        for layer in layers {
            // The texture's identity stands in for its pixels: a texture is
            // written once at upload and never again.
            layer.texture.id().hash(&mut hasher);
            layer.texture.size().hash(&mut hasher);
            layer.blend.hash(&mut hasher);
            hash_transform(&layer.transform, &mut hasher);
        }
        CompositeKey(hasher.finish())
    }

    pub fn raw(self) -> u64 {
        self.0
    }
}

/// Floats are hashed by their bits, which is exact but slightly conservative:
/// `0.0` and `-0.0` render identically yet hash differently. The cost of that
/// is one recomposite, and the alternative — normalising before hashing — is
/// how a key stops describing what the renderer actually received.
fn hash_f64(value: f64, hasher: &mut impl Hasher) {
    value.to_bits().hash(hasher);
}

fn hash_colour(colour: Rgba, hasher: &mut impl Hasher) {
    for channel in [colour.r, colour.g, colour.b, colour.a] {
        hash_f64(channel, hasher);
    }
}

/// The transform is hashed field by field rather than as its matrix, because
/// the matrix is derived from these values plus the layer and composition
/// sizes, all of which are already in the key.
fn hash_transform(transform: &TransformState, hasher: &mut impl Hasher) {
    hash_f64(transform.position.x, hasher);
    hash_f64(transform.position.y, hasher);
    hash_f64(transform.scale.x, hasher);
    hash_f64(transform.scale.y, hasher);
    hash_f64(transform.rotation, hasher);
    hash_f64(transform.anchor.x, hasher);
    hash_f64(transform.anchor.y, hasher);
    // Clamped, matching what the renderer writes into the uniform: two
    // opacities above one produce the same picture and so must not produce
    // different keys.
    hash_f64(transform.opacity.clamp(0.0, 1.0), hasher);
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct CompositeCacheStats {
    pub hits: u64,
    pub misses: u64,
    pub evictions: u64,
    pub entries: usize,
    pub bytes: usize,
    pub capacity_bytes: usize,
    /// Targets held for reuse, which are not entries and cost no budget.
    pub spare: usize,
}

impl CompositeCacheStats {
    pub fn hit_rate(&self) -> f64 {
        let total = self.hits + self.misses;
        if total == 0 {
            0.0
        } else {
            self.hits as f64 / total as f64
        }
    }
}

struct Entry {
    target: RenderTarget,
    /// Monotonic stamp of last use, for LRU ordering.
    last_used: u64,
}

/// How many freed targets to keep for reuse.
///
/// Playback inserts a new composite every frame and evicts one to pay for it, so
/// a couple of spares is enough to make the steady state allocation-free. A
/// nested composite pushes that a little further — a frame can need one target
/// per nesting level — so the pool holds a handful and matches by size, since a
/// 512-pixel title card cannot be drawn into a 4K target.
const MAX_SPARE: usize = 6;

/// An LRU cache of composited pictures, bounded by GPU memory.
///
/// Sits alongside [`crate::TextureCache`] rather than inside it: that one holds
/// the *inputs* to compositing, one texture per decoded frame, and this one
/// holds the *outputs*, one texture per distinct picture. They are separate
/// budgets because they are evicted for different reasons — dropping an input
/// costs a re-upload, dropping an output costs a render pass.
///
/// Not internally synchronised. The preview owns one on the thread that renders.
pub struct CompositeCache {
    entries: HashMap<CompositeKey, Entry>,
    /// Targets freed by eviction, ready to be drawn into again. Reusing them is
    /// what keeps playback from allocating and freeing a full-resolution
    /// texture on every single frame. Mixed sizes, matched on the way out: a
    /// frame holding nested compositions needs targets of several shapes at
    /// once.
    spare: Vec<RenderTarget>,
    capacity_bytes: usize,
    bytes: usize,
    clock: u64,
    hits: u64,
    misses: u64,
    evictions: u64,
    metrics: Option<Metrics>,
}

impl CompositeCache {
    pub fn new(capacity_bytes: usize) -> Self {
        CompositeCache {
            entries: HashMap::new(),
            spare: Vec::new(),
            capacity_bytes,
            bytes: 0,
            clock: 0,
            hits: 0,
            misses: 0,
            evictions: 0,
            metrics: None,
        }
    }

    /// A cache sized in megabytes, with a floor that can hold a few 4K frames.
    ///
    /// A budget too small to hold anything is worse than no cache at all: every
    /// insert would evict the entry the next repaint is about to ask for.
    pub fn with_budget_mb(mb: usize) -> Self {
        const MIN_BYTES: usize = 128 * 1024 * 1024;
        CompositeCache::new((mb * 1024 * 1024).max(MIN_BYTES))
    }

    pub fn with_metrics(mut self, metrics: Metrics) -> Self {
        self.metrics = Some(metrics);
        self
    }

    /// Looks up a composited picture, marking it as recently used.
    pub fn get(&mut self, key: &CompositeKey) -> Option<&RenderTarget> {
        self.touch(key).then(|| &self.entries.get(key).expect("just touched").target)
    }

    /// Marks an entry as recently used, reporting whether it was there.
    ///
    /// Separate from [`CompositeCache::get`] because one frame may need several
    /// cached targets at once — a composite holding three nested compositions —
    /// which a single `&mut self` lookup cannot hand out. Callers touch every
    /// key first, then read them all through [`CompositeCache::peek`].
    pub fn touch(&mut self, key: &CompositeKey) -> bool {
        self.clock += 1;
        let clock = self.clock;
        let found = match self.entries.get_mut(key) {
            Some(entry) => {
                entry.last_used = clock;
                true
            }
            None => false,
        };

        let counter =
            if found { counters::COMPOSITE_CACHE_HIT } else { counters::COMPOSITE_CACHE_MISS };
        if let Some(m) = &self.metrics {
            m.incr(counter, 1);
        }
        if found {
            self.hits += 1;
        } else {
            self.misses += 1;
        }
        found
    }

    /// Reads an entry without disturbing LRU order or the counters.
    pub fn peek(&self, key: &CompositeKey) -> Option<&RenderTarget> {
        self.entries.get(key).map(|e| &e.target)
    }

    /// Whether a key is present, without disturbing LRU order or the counters.
    pub fn contains(&self, key: &CompositeKey) -> bool {
        self.entries.contains_key(key)
    }

    /// A target of `size` to composite into, reused if one of that size is going
    /// spare.
    ///
    /// Sizes are mixed deliberately. A sequence at 1080p can hold a 512-pixel
    /// title composition, and both are in flight during the same frame; the size
    /// is part of every key, so entries of different shapes coexist without ever
    /// being confused for one another.
    pub fn take_target(&mut self, device: &wgpu::Device, size: Size) -> RenderTarget {
        let size = Size::new(size.width.max(1), size.height.max(1));
        match self.spare.iter().position(|t| t.size() == size) {
            Some(index) => self.spare.swap_remove(index),
            None => RenderTarget::new(device, size),
        }
    }

    /// Stores a composited picture, evicting least-recently-used ones to stay
    /// within budget.
    ///
    /// Takes the target by value: the cache owns what it hands out through
    /// [`CompositeCache::take_target`], and an entry nobody can still be drawing
    /// into is what makes reuse safe.
    pub fn insert(&mut self, key: CompositeKey, target: RenderTarget) {
        let size = target.byte_size();
        if size > self.capacity_bytes {
            log::debug!(
                "a {size} byte composite exceeds the {} byte render cache budget; not caching",
                self.capacity_bytes
            );
            self.recycle(target);
            return;
        }

        if let Some(old) = self.entries.remove(&key) {
            self.bytes -= old.target.byte_size();
            self.recycle(old.target);
        }
        self.evict_until_fits(size);

        self.clock += 1;
        self.bytes += size;
        self.entries.insert(key, Entry { target, last_used: self.clock });
        self.publish_gauges();
    }

    /// Hands a target back without keeping what was drawn into it.
    ///
    /// For a picture not worth caching — an incomplete frame whose layers are
    /// still decoding — where the *target* is still worth keeping for the next
    /// composite.
    pub fn discard_target(&mut self, target: RenderTarget) {
        self.recycle(target);
    }

    fn evict_until_fits(&mut self, incoming: usize) {
        while self.bytes + incoming > self.capacity_bytes && !self.entries.is_empty() {
            // A linear scan for the oldest entry. The cache holds tens of
            // full-resolution pictures, not thousands, and the scan happens
            // once per composite against a render pass costing milliseconds.
            let victim = self.entries.iter().min_by_key(|(_, e)| e.last_used).map(|(k, _)| *k);
            match victim {
                Some(key) => {
                    if let Some(entry) = self.entries.remove(&key) {
                        self.bytes -= entry.target.byte_size();
                        self.evictions += 1;
                        self.recycle(entry.target);
                    }
                }
                None => break,
            }
        }
    }

    /// Keeps a freed target for reuse, or drops it if enough are already held.
    fn recycle(&mut self, target: RenderTarget) {
        if self.spare.len() < MAX_SPARE {
            self.spare.push(target);
        }
    }

    /// Drops every cached picture, for a new project or a changed sequence
    /// format. Keeps the spare targets, which are still the right size.
    pub fn clear(&mut self) {
        self.entries.clear();
        self.bytes = 0;
        self.publish_gauges();
    }

    pub fn stats(&self) -> CompositeCacheStats {
        CompositeCacheStats {
            hits: self.hits,
            misses: self.misses,
            evictions: self.evictions,
            entries: self.entries.len(),
            bytes: self.bytes,
            capacity_bytes: self.capacity_bytes,
            spare: self.spare.len(),
        }
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    pub fn bytes(&self) -> usize {
        self.bytes
    }

    /// Resizes the budget, evicting immediately if it shrank.
    pub fn set_capacity_bytes(&mut self, capacity_bytes: usize) {
        self.capacity_bytes = capacity_bytes;
        self.evict_until_fits(0);
        self.publish_gauges();
    }

    fn publish_gauges(&self) {
        if let Some(m) = &self.metrics {
            m.set_gauge(counters::COMPOSITE_CACHE_BYTES, self.bytes as f64);
            m.set_gauge(counters::COMPOSITE_CACHE_ENTRIES, self.entries.len() as f64);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_empty_cache_reports_a_zero_hit_rate_rather_than_dividing_by_zero() {
        let cache = CompositeCache::new(1024);
        assert_eq!(cache.stats().hit_rate(), 0.0);
        assert!(cache.is_empty());
    }

    #[test]
    fn the_budget_floor_can_hold_a_few_4k_pictures() {
        // 4K RGBA is about 33 MB; a budget below that would thrash.
        let cache = CompositeCache::with_budget_mb(1);
        assert!(cache.stats().capacity_bytes >= 128 * 1024 * 1024);
    }

    #[test]
    fn a_miss_on_an_empty_cache_is_counted() {
        let mut cache = CompositeCache::new(1024 * 1024);
        assert!(cache.get(&CompositeKey(7)).is_none());
        assert_eq!(cache.stats().misses, 1);
        assert_eq!(cache.stats().hits, 0);
    }
}
