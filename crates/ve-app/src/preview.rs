//! The GPU side of the preview: composite, and hand the result to the UI.
//!
//! The compositing itself is [`ve_export::FrameComposer`], which is also what
//! an export renders with — one walk over the plan, one set of caches, one
//! answer. What is left here is everything that is about being *on screen*: the
//! texture the interface draws from, the blit into it, and the handle egui
//! holds.
//!
//! # Three costs, not one
//!
//! A repaint takes the cheapest path that is correct:
//!
//! * the composition is unchanged — nothing happens at all, because the picture
//!   already on screen is still the right one. This is the usual case while
//!   editing, where most repaints come from the pointer moving over a panel;
//! * the composition was composited before — the cached picture is copied into
//!   the target the interface draws from;
//! * otherwise — upload what is missing, composite, and cache the result.

use egui_wgpu::RenderState;
use ve_core::Size;
use ve_engine::{EngineUpdate, PlanItem};
use ve_export::FrameComposer;
use ve_media::CacheKey;
use ve_metrics::{counters, Metrics};
use ve_render::{CompositeCacheStats, CompositeKey, RenderTarget, ScopeSample, ScopeSampler};

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
pub struct Preview {
    composer: FrameComposer,
    /// The one texture the interface draws from. Composites are copied into it
    /// rather than handed to egui directly, so the handle registered with the UI
    /// stays valid for as long as the resolution does.
    target: RenderTarget,
    /// Which composite the target currently holds, if it is still current.
    presented: Option<CompositeKey>,
    /// The handle egui draws with, re-registered whenever the target is rebuilt.
    texture_id: Option<egui::TextureId>,
    /// The scope readback, built the first time a scope is opened and dropped
    /// when the last one is closed.
    ///
    /// An `Option` rather than a flag on an always-present sampler because the
    /// point is that the scopes cost *nothing* when they are not on screen —
    /// not the readback, not the stall waiting for it, and not the small target
    /// it would otherwise be held in.
    scopes: Option<ScopeSampler>,
    metrics: Metrics,
}

impl Preview {
    pub fn new(render_state: &RenderState, size: Size, metrics: Metrics) -> Self {
        let composer = FrameComposer::new(
            &render_state.device,
            TEXTURE_BUDGET_MB,
            COMPOSITE_BUDGET_MB,
            metrics.clone(),
        );
        let target = RenderTarget::new(&render_state.device, size);
        let mut preview = Preview {
            composer,
            target,
            presented: None,
            texture_id: None,
            scopes: None,
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
        scopes: bool,
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

        let root = self.composer.compose(device, queue, update, key_for);
        self.present(device, queue, root);
        self.sample_scopes(device, queue, update, root, scopes);
    }

    /// The last picture read back for the scopes, if they are open.
    pub fn scope_sample(&self) -> Option<&ScopeSample> {
        self.scopes.as_ref().map(|s| s.sample())
    }

    /// Reads the composited picture back, small, while a scope is on screen.
    ///
    /// Nothing at all happens when they are closed, and nothing happens on a
    /// repaint that presented the picture already sampled — which is most of
    /// them, because a grade is dialled in on a held frame.
    fn sample_scopes(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        update: &EngineUpdate,
        root: Option<CompositeKey>,
        wanted: bool,
    ) {
        if !wanted {
            self.scopes = None;
            return;
        }
        let Some(key) = root else { return };
        let Some(node) = update.plan.nodes.get(update.plan.root_index()) else { return };
        let space = node.color_space;

        // Taken and put back, because the sampler and the composer's renderer
        // are both borrowed for the duration of the read.
        let mut sampler =
            self.scopes.take().unwrap_or_else(|| ScopeSampler::new(device, update.plan.size()));
        self.composer.sample_scopes(device, queue, &mut sampler, &key, space);
        self.scopes = Some(sampler);
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

        let copied = match self.composer.picture(&key) {
            Some(target) => self.target.blit_from(device, queue, target),
            None => false,
        };
        self.presented = copied.then_some(key);
    }

    pub fn texture_bytes(&self) -> usize {
        self.composer.texture_bytes()
    }

    pub fn texture_count(&self) -> usize {
        self.composer.texture_count()
    }

    pub fn composite_stats(&self) -> CompositeCacheStats {
        self.composer.composite_stats()
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
