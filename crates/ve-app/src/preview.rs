//! The GPU side of the preview: upload, composite, hand the result to the UI.

use egui_wgpu::RenderState;
use ve_core::Size;
use ve_engine::EngineUpdate;
use ve_media::CacheKey;
use ve_metrics::{counters, Metrics};
use ve_render::{Layer, RenderTarget, Renderer, TextureCache};

/// How much GPU memory uploaded frames may occupy.
const TEXTURE_BUDGET_MB: usize = 384;

/// Composites the timeline into a texture the interface can draw.
///
/// Shares the interface's own wgpu device rather than creating a second one, so
/// a decoded frame is uploaded once and both the compositor and the UI draw
/// from the same texture. A separate device would mean a copy across the PCIe
/// bus for every frame.
pub struct Preview {
    renderer: Renderer,
    target: RenderTarget,
    textures: TextureCache,
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
        key_for: impl Fn(&ve_engine::VisibleClip) -> Option<CacheKey>,
    ) {
        let device = &render_state.device;
        let queue = &render_state.queue;

        // The preview always renders at the sequence's own resolution, not the
        // panel's. Composition geometry then does not depend on window size, so
        // what is previewed is exactly what will be exported.
        if self.target.resize(device, update.composition.size) {
            self.register(render_state);
        }

        // Upload anything not already resident. Keyed the same way the frame
        // cache is, so a frame revisited during scrubbing is not re-uploaded.
        for layer in &update.layers {
            if let Some(key) = key_for(&layer.clip) {
                if !self.textures.contains(&key) {
                    let texture = self.renderer.upload(device, queue, &layer.frame);
                    self.textures.insert(key, texture);
                }
            }
        }

        // Two passes: update LRU order first, then take the shared references
        // the render pass needs. A single `&mut` lookup per layer could not
        // hand out several textures at once.
        let keys: Vec<Option<CacheKey>> = update
            .layers
            .iter()
            .map(|layer| {
                let key = key_for(&layer.clip)?;
                self.textures.touch(&key).then_some(key)
            })
            .collect();

        let textures = &self.textures;
        let mut layers = Vec::with_capacity(update.layers.len());
        for (layer, key) in update.layers.iter().zip(&keys) {
            let Some(key) = key else { continue };
            let Some(texture) = textures.peek(key) else { continue };
            layers.push(Layer { texture, transform: layer.clip.transform });
        }

        self.renderer.render(
            device,
            queue,
            &self.target,
            update.composition.background,
            &layers,
        );
        self.metrics.set_gauge(counters::GPU_TEXTURE_BYTES, self.textures.bytes() as f64);
    }

    pub fn texture_bytes(&self) -> usize {
        self.textures.bytes()
    }

    pub fn texture_count(&self) -> usize {
        self.textures.len()
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
