//! The GPU side of the preview: upload, composite, hand the result to the UI.

use egui_wgpu::RenderState;
use ve_core::Size;
use ve_engine::{EngineUpdate, LayerContent, PlanItem};
use ve_media::CacheKey;
use ve_metrics::{counters, Metrics};
use ve_render::{
    CompositeCache, CompositeCacheStats, CompositeKey, GpuTexture, Layer, RenderTarget,
    Renderer, TextureCache,
};

/// How much GPU memory uploaded frames may occupy.
const TEXTURE_BUDGET_MB: usize = 384;

/// How much GPU memory composited pictures may occupy.
///
/// Smaller than the frame budget on purpose: a composite is only worth keeping
/// while the edit that produced it stands, whereas a decoded frame stays useful
/// across every change that does not touch its clip.
const COMPOSITE_BUDGET_MB: usize = 256;

/// Composites the timeline into a texture the interface can draw.
///
/// Shares the interface's own wgpu device rather than creating a second one, so
/// a decoded frame is uploaded once and both the compositor and the UI draw
/// from the same texture. A separate device would mean a copy across the PCIe
/// bus for every frame.
///
/// # Three costs, not one
///
/// A repaint takes the cheapest path that is correct:
///
/// * the composition is unchanged — nothing happens at all, because the picture
///   already on screen is still the right one. This is the usual case while
///   editing, where most repaints come from the pointer moving over a panel;
/// * the composition was composited before — the cached picture is copied into
///   the target the interface draws from;
/// * otherwise — upload what is missing, composite, and cache the result.
///
/// # Nesting
///
/// The engine hands over the instant as a list of nodes in render order, every
/// node's children before it. The preview walks that list once: each node is
/// composited into a target of its own, which the node above samples as an
/// ordinary layer. Each node is keyed and cached separately, so changing one
/// layer of a deep composite redraws that composition and the ones containing
/// it, and leaves its siblings alone.
pub struct Preview {
    renderer: Renderer,
    /// The one texture the interface draws from. Composites are copied into it
    /// rather than handed to egui directly, so the handle registered with the UI
    /// stays valid for as long as the resolution does.
    target: RenderTarget,
    textures: TextureCache,
    composites: CompositeCache,
    /// Which composite the target currently holds, if it is still current.
    presented: Option<CompositeKey>,
    /// The handle egui draws with, re-registered whenever the target is rebuilt.
    texture_id: Option<egui::TextureId>,
    metrics: Metrics,
}

impl Preview {
    pub fn new(render_state: &RenderState, size: Size, metrics: Metrics) -> Self {
        let renderer = Renderer::new(&render_state.device).with_metrics(metrics.clone());
        let target = RenderTarget::new(&render_state.device, size);
        let mut preview = Preview {
            renderer,
            target,
            textures: TextureCache::with_budget_mb(TEXTURE_BUDGET_MB)
                .with_metrics(metrics.clone()),
            composites: CompositeCache::with_budget_mb(COMPOSITE_BUDGET_MB)
                .with_metrics(metrics.clone()),
            presented: None,
            texture_id: None,
            metrics,
        };
        preview.register(render_state);
        preview
    }

    pub fn texture_id(&self) -> Option<egui::TextureId> {
        self.texture_id
    }

    pub fn size(&self) -> Size {
        self.target.size()
    }

    /// Composites one engine update into the preview texture.
    pub fn render(
        &mut self,
        render_state: &RenderState,
        update: &EngineUpdate,
        key_for: impl Fn(&PlanItem) -> Option<CacheKey>,
    ) {
        let device = &render_state.device;
        let queue = &render_state.queue;

        // The preview always renders at the composition's own resolution, not
        // the panel's. Geometry then does not depend on window size, so what is
        // previewed is exactly what will be exported.
        if self.target.resize(device, update.plan.size()) {
            self.register(render_state);
            // A fresh target holds nothing, so nothing is presented.
            self.presented = None;
        }

        // Upload anything not already resident. Keyed the same way the frame
        // cache is, so a frame revisited during scrubbing is not re-uploaded.
        for node in &update.nodes {
            for layer in &node.layers {
                let LayerContent::Frame(frame) = &layer.content else { continue };
                let Some(key) = key_for(&layer.item) else { continue };
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

        let root = keys.get(update.plan.root_index()).copied().flatten();
        self.present(device, queue, root);
        self.metrics.set_gauge(counters::GPU_TEXTURE_BYTES, self.textures.bytes() as f64);
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
                LayerContent::Frame(_) => key_for(&layer.item)
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

        // A motion-blurred layer is averaged into a target of its own first,
        // and the node then draws that target once. Done before the node's own
        // layer list is built, so by the time the pass below runs every blurred
        // layer is an ordinary texture like any other.
        let blurred: Vec<Option<GpuTexture>> = resolved
            .layers
            .iter()
            .zip(&bound)
            .zip(&nested)
            .map(|((layer, bound), nested)| {
                if !layer.item.is_blurred() {
                    return None;
                }
                let texture = match bound {
                    Some(Bound::Frame(key)) => self.textures.peek(key),
                    Some(Bound::Nested(_)) => nested.as_ref(),
                    None => None,
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
        for (((layer, bound), nested), blurred) in
            resolved.layers.iter().zip(&bound).zip(&nested).zip(&blurred)
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
            let texture = match bound {
                Some(Bound::Frame(key)) => textures.peek(key),
                Some(Bound::Nested(_)) => nested.as_ref(),
                None => None,
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

    /// Copies a node's picture into the texture the interface draws from.
    fn present(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        key: Option<CompositeKey>,
    ) {
        let Some(key) = key else { return };

        // Already on screen: nothing to draw, nothing even to copy.
        if self.presented == Some(key) {
            self.metrics.incr(counters::COMPOSITE_UNCHANGED, 1);
            return;
        }

        let copied = match self.composites.peek(&key) {
            Some(target) => self.target.blit_from(device, queue, target),
            None => false,
        };
        self.presented = copied.then_some(key);
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

    /// Points egui's renderer at the current target texture.
    fn register(&mut self, render_state: &RenderState) {
        let mut egui_renderer = render_state.renderer.write();
        if let Some(old) = self.texture_id.take() {
            egui_renderer.free_texture(&old);
        }
        self.texture_id = Some(egui_renderer.register_native_texture(
            &render_state.device,
            self.target.view(),
            // Linear, so a preview scaled to fit the panel is smooth rather
            // than aliased.
            wgpu::FilterMode::Linear,
        ));
    }
}

/// Which cache a layer's picture was found in, once it has been touched.
enum Bound {
    Frame(CacheKey),
    Nested(CompositeKey),
}
