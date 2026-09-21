//! Turning one resolved instant into a picture on the GPU.
//!
//! This is the walk the preview does on every repaint and the exporter does on
//! every frame, and it is **one** piece of code on purpose. An export that
//! composited by a second route would eventually disagree with what the editor
//! showed — over a blend mode, the order of an effect chain, how a motion blur
//! is averaged — and the disagreement would be discovered in the delivered file.
//!
//! # What happens to one layer, in order
//!
//! 1. **Effects**, in chain order, each into a target of its own at the layer's
//!    own resolution.
//! 2. **Motion blur**, averaging the result across the shutter into a target at
//!    the node's resolution.
//! 3. **The draw** into the node, with the layer's transform and blend mode.
//!
//! Effects before the blur because a chain is part of the layer's own picture
//! and the shutter smears whatever picture the layer has; the transform last
//! because that is what puts the picture on the canvas.
//!
//! # Nesting
//!
//! The engine hands over the instant as a list of nodes in render order, every
//! node's children before it. The walk goes through that list once: each node
//! is composited into a target of its own, which the node above samples as an
//! ordinary layer. Each node is keyed and cached separately, so changing one
//! layer of a deep composite redraws that composition and the ones containing
//! it, and leaves its siblings alone.
//!
//! # Caching, in both callers
//!
//! Every step is content-addressed, so the preview gets its hits from
//! revisiting instants and the exporter — which visits each instant once —
//! still gets the target *pool*, which is what keeps a long render from
//! allocating a full-resolution texture per frame.

use ve_core::Size;
use ve_engine::{EngineUpdate, LayerContent, PlanItem};
use ve_media::CacheKey;
use ve_metrics::{counters, Metrics};
use ve_render::{
    chain_passes, CompositeCache, CompositeCacheStats, CompositeKey, GpuTexture, Layer,
    RenderTarget, Renderer, ScopeSampler, TextureCache, TextureKey,
};

/// Composites resolved instants, holding the caches that make repeats cheap.
///
/// Owns no window, no surface and no presentation target: what comes out is a
/// key into its own cache, which the caller either blits onto the screen or
/// reads back into an encoder.
pub struct FrameComposer {
    renderer: Renderer,
    textures: TextureCache,
    composites: CompositeCache,
    metrics: Metrics,
}

impl FrameComposer {
    /// `texture_budget_mb` bounds uploaded frames, `composite_budget_mb`
    /// composited pictures. They are separate budgets because they are
    /// differently useful: a decoded frame stays valid across every edit that
    /// does not touch its clip, while a composite is only worth keeping while
    /// the edit that produced it stands.
    pub fn new(
        device: &wgpu::Device,
        texture_budget_mb: usize,
        composite_budget_mb: usize,
        metrics: Metrics,
    ) -> Self {
        FrameComposer {
            renderer: Renderer::new(device).with_metrics(metrics.clone()),
            textures: TextureCache::with_budget_mb(texture_budget_mb)
                .with_metrics(metrics.clone()),
            composites: CompositeCache::with_budget_mb(composite_budget_mb)
                .with_metrics(metrics.clone()),
            metrics,
        }
    }

    /// Composites every node of `update`, returning the key the root landed on.
    ///
    /// `key_for` says how to identify a layer's decoded frame, which is the
    /// caller's business: the preview asks its decode service, and the exporter
    /// keys the frames it decoded itself.
    pub fn compose(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        update: &EngineUpdate,
        key_for: impl Fn(&PlanItem) -> Option<CacheKey>,
    ) -> Option<CompositeKey> {
        // Upload anything not already resident. Keyed the same way the frame
        // cache is, so a frame revisited during scrubbing is not re-uploaded.
        for node in &update.nodes {
            for layer in &node.layers {
                let LayerContent::Frame(frame) = &layer.content else { continue };
                let Some(key) = texture_key(&layer.item, &key_for) else { continue };
                if !self.textures.contains(&key) {
                    let texture = self.renderer.upload(device, queue, frame);
                    self.textures.insert(key, texture);
                }
            }
        }

        // Children before parents, so a nested target is finished before
        // anything samples it. `keys[i]` is what node `i` composited to.
        let mut keys: Vec<Option<CompositeKey>> = vec![None; update.nodes.len()];
        for index in 0..update.nodes.len() {
            keys[index] = self.render_node(device, queue, update, index, &keys, &key_for);
        }

        self.metrics.set_gauge(counters::GPU_TEXTURE_BYTES, self.textures.bytes() as f64);
        keys.get(update.plan.root_index()).copied().flatten()
    }

    /// The target a key's picture is in, for as long as it is not evicted.
    pub fn picture(&self, key: &CompositeKey) -> Option<&RenderTarget> {
        self.composites.peek(key)
    }

    /// Reads a composited picture back, small, for the video scopes.
    ///
    /// Narrow on purpose: the scopes need a scaled copy of one composite and
    /// nothing else, so what is exposed is that and not the renderer. Returns
    /// whether anything was read — `false` for a picture that has been evicted,
    /// and for the common case of one already sampled.
    ///
    /// The scopes are the only interactive reader-back in the editor. It is
    /// affordable because it is small and because it happens once per distinct
    /// composite rather than once per repaint; see `ve_render::scopes`.
    pub fn sample_scopes(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        sampler: &mut ScopeSampler,
        key: &CompositeKey,
        color_space: ve_core::ColorSpace,
    ) -> bool {
        let Some(target) = self.composites.peek(key) else { return false };
        let texture = self.renderer.bind_target(device, target, *key);
        sampler.read(device, queue, &mut self.renderer, color_space, &texture, *key)
    }

    /// Composites one node, reusing a cached picture where the contents match.
    ///
    /// Returns the key the node composited to, which is what the node above
    /// hashes into its own key — that is how a change deep in a nest reaches the
    /// top, and how an unchanged nest stops one from going further.
    fn render_node(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        update: &EngineUpdate,
        index: usize,
        keys: &[Option<CompositeKey>],
        key_for: &impl Fn(&PlanItem) -> Option<CacheKey>,
    ) -> Option<CompositeKey> {
        let plan = update.plan.nodes.get(index)?;
        let resolved = update.nodes.get(index)?;

        // Touch every input first: the render pass needs shared references to
        // several textures and targets at once, which a lookup per layer taking
        // `&mut self` could not hand out.
        let mut bound: Vec<Option<Bound>> = Vec::with_capacity(resolved.layers.len());
        for layer in &resolved.layers {
            bound.push(match &layer.content {
                LayerContent::Frame(_) => texture_key(&layer.item, key_for)
                    .filter(|key| self.textures.touch(key))
                    .map(Bound::Frame),
                LayerContent::Nested(child) => keys
                    .get(*child)
                    .copied()
                    .flatten()
                    .filter(|key| self.composites.touch(key))
                    .map(Bound::Nested),
            });
        }

        // A nested target is wrapped as an ordinary layer texture, identified by
        // what is *in* it rather than by the target object — targets are reused,
        // so their own identity would tell the cache nothing.
        let nested: Vec<Option<GpuTexture>> = bound
            .iter()
            .map(|b| match b {
                Some(Bound::Nested(key)) => {
                    let target = self.composites.peek(key)?;
                    Some(self.renderer.bind_target(device, target, *key))
                }
                _ => None,
            })
            .collect();

        // The effect chain, run before anything else touches the picture. Each
        // pass reads the last one's output, and each output is cached on its
        // own key, so re-running a chain after one parameter changed redraws
        // from that parameter's pass onwards and no further back.
        let effected: Vec<Option<GpuTexture>> = resolved
            .layers
            .iter()
            .zip(&bound)
            .zip(&nested)
            .map(|((layer, bound), nested)| {
                if !layer.item.has_effects() {
                    return None;
                }
                let source = match bound {
                    Some(Bound::Frame(key)) => self.textures.peek(key),
                    Some(Bound::Nested(_)) => nested.as_ref(),
                    None => None,
                }?;
                run_chain(
                    &mut self.renderer,
                    &mut self.composites,
                    device,
                    queue,
                    plan.color_space,
                    source,
                    &layer.item.effects,
                )
            })
            .collect();

        // A motion-blurred layer is averaged into a target of its own first,
        // and the node then draws that target once. Done before the node's own
        // layer list is built, so by the time the pass below runs every blurred
        // layer is an ordinary texture like any other.
        let blurred: Vec<Option<GpuTexture>> = resolved
            .layers
            .iter()
            .zip(&bound)
            .zip(&nested)
            .zip(&effected)
            .map(|(((layer, bound), nested), effected)| {
                if !layer.item.is_blurred() {
                    return None;
                }
                // What the shutter smears is the picture the chain produced,
                // not the one the decoder handed over.
                let texture = match (effected, bound) {
                    (Some(texture), _) => Some(texture),
                    (None, Some(Bound::Frame(key))) => self.textures.peek(key),
                    (None, Some(Bound::Nested(_))) => nested.as_ref(),
                    (None, None) => None,
                }?;
                let samples: Vec<Layer<'_>> = layer
                    .item
                    .samples
                    .iter()
                    .map(|transform| Layer {
                        texture,
                        transform: *transform,
                        // The samples are averaged, not blended with each
                        // other; the layer's own blend mode applies once, when
                        // the average is drawn into the node.
                        blend: ve_core::BlendMode::Normal,
                    })
                    .collect();

                let key = CompositeKey::of_average(plan.size, plan.color_space, &samples);
                if !self.composites.touch(&key) {
                    let scratch = self.composites.take_target(device, plan.size);
                    self.renderer.accumulate(
                        device,
                        queue,
                        &scratch,
                        plan.color_space,
                        &samples,
                    );
                    self.composites.insert(key, scratch);
                }
                let target = self.composites.peek(&key)?;
                Some(self.renderer.bind_target(device, target, key))
            })
            .collect();

        let textures = &self.textures;
        let mut layers = Vec::with_capacity(resolved.layers.len());
        for ((((layer, bound), nested), blurred), effected) in
            resolved.layers.iter().zip(&bound).zip(&nested).zip(&blurred).zip(&effected)
        {
            // A blurred layer's picture is the average, drawn at the node's own
            // size with the transform already inside it — so it is laid down
            // untransformed and at full opacity, both of which every sample
            // already carried.
            if let Some(texture) = blurred {
                layers.push(Layer {
                    texture,
                    transform: ve_core::TransformState::default(),
                    blend: layer.item.blend,
                });
                continue;
            }
            let texture = match (effected, bound) {
                (Some(texture), _) => Some(texture),
                (None, Some(Bound::Frame(key))) => textures.peek(key),
                (None, Some(Bound::Nested(_))) => nested.as_ref(),
                (None, None) => None,
            };
            let Some(texture) = texture else { continue };
            layers.push(Layer {
                texture,
                transform: layer.item.transform,
                blend: layer.item.blend,
            });
        }

        let key = CompositeKey::of(plan.size, plan.background, plan.color_space, &layers);

        // Already composited: nothing to draw, and whoever needs it can sample
        // the cached target.
        if self.composites.touch(&key) {
            return Some(key);
        }

        let scratch = self.composites.take_target(device, plan.size);
        self.renderer.render(
            device,
            queue,
            &scratch,
            plan.background,
            plan.color_space,
            &layers,
        );
        // Cached even when something in it was still decoding. The key describes
        // exactly the layers that were drawn, so a hit on it is the same picture
        // rather than a stale one; the frames still arriving change the key, and
        // the transient entry ages out under the same LRU as everything else.
        self.composites.insert(key, scratch);
        Some(key)
    }

    pub fn texture_bytes(&self) -> usize {
        self.textures.bytes()
    }

    pub fn texture_count(&self) -> usize {
        self.textures.len()
    }

    pub fn composite_stats(&self) -> CompositeCacheStats {
        self.composites.stats()
    }

    /// Drops every uploaded frame, keeping the target pool.
    ///
    /// The exporter calls this between runs: the frames it decoded are keyed on
    /// asset and frame index, and the next export is a different project's
    /// worth of them.
    pub fn forget_frames(&mut self) {
        self.textures.clear();
    }

    /// The canvas the root node was composited at, if anything was.
    pub fn picture_size(&self, key: &CompositeKey) -> Option<Size> {
        self.composites.peek(key).map(|t| t.size())
    }
}

/// Runs an effect chain over one picture, returning what came out.
///
/// One target per pass, taken from the same pool everything else composites
/// into. The chain runs at the **source's own resolution** — a 4K clip's blur
/// is computed at 4K even on a 1080p canvas — because that is the picture the
/// effects are a property of, and because the alternative would make a chain's
/// result depend on where the clip happens to be scaled to.
///
/// Returns `None` only if a pass's output went missing from the cache between
/// being inserted and being read, which means the budget evicted it; the layer
/// is then drawn without its effects rather than not drawn at all.
///
/// Takes the renderer and the cache rather than `&mut FrameComposer` so that
/// the source texture, which is borrowed from the *other* cache, can still be
/// held across the call.
fn run_chain(
    renderer: &mut Renderer,
    composites: &mut CompositeCache,
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    color_space: ve_core::ColorSpace,
    source: &GpuTexture,
    effects: &[ve_core::EffectState],
) -> Option<GpuTexture> {
    let size = source.size();
    let passes = chain_passes(effects, size);
    let mut current: Option<GpuTexture> = None;
    for pass in &passes {
        let input = current.as_ref().unwrap_or(source);
        let key = CompositeKey::of_effect(input.id(), size, color_space, pass);
        if !composites.touch(&key) {
            let scratch = composites.take_target(device, size);
            renderer.apply_effect(device, queue, &scratch, color_space, input, pass);
            composites.insert(key, scratch);
        }
        let target = composites.peek(&key)?;
        current = Some(renderer.bind_target(device, target, key));
    }
    current
}

/// Which cache a layer's picture was found in, once it has been touched.
enum Bound {
    Frame(TextureKey),
    Nested(CompositeKey),
}

/// How an uploaded picture is identified.
///
/// A decoded frame is the caller's business — the preview asks its decode
/// service, an export keys what it decoded itself — while a drawn graphic is
/// not: its identity is the hash of what drew it, which the plan already
/// carries. So graphics are keyed here rather than being pushed out to every
/// caller that has nothing to say about them.
fn texture_key(
    item: &PlanItem,
    key_for: &impl Fn(&PlanItem) -> Option<CacheKey>,
) -> Option<TextureKey> {
    match item.graphic() {
        Some(state) => Some(TextureKey::Graphic(state.content_hash())),
        None => key_for(item).map(TextureKey::Frame),
    }
}
