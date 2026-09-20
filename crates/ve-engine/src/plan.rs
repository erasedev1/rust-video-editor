//! Turning a sequence and a time into a flat list of what to draw and hear.
//!
//! This is the boundary between the edit model and the engine, and it is
//! deliberately **pure**: no decoding, no GPU, no clock. Given a [`Sequence`]
//! and an instant it answers "what is on screen, where, and how loud", which
//! makes every question about track ordering, muting, soloing, nesting and
//! animation answerable in a unit test rather than by looking at a preview.
//!
//! # Why a plan and not a "composition"
//!
//! A [`RenderPlan`] is the flattened, ready-to-draw description of one instant.
//! It is deliberately not called a composition, because a composition is a thing
//! the *user* authors and saves — see [`ve_core::Composition`] — whereas this is
//! derived, thrown away every frame, and never persisted.
//!
//! # Nesting is flattened, not recursive
//!
//! A clip or a layer can draw another composition, so an instant is really a
//! tree: the sequence's picture needs a composition's picture, which may need
//! another's. The plan holds that tree as a **list of nodes in render order** —
//! every node's children come before it, and the root is last — rather than as
//! nested structs.
//!
//! That shape is what the renderer wants. Walking the list front to back means
//! each nested target is already drawn by the time something samples it, with no
//! recursion in the hot path and no borrow gymnastics to hold several targets at
//! once. It is also what makes the plan easy to assert on: a test can name node
//! indices instead of matching down a chain of boxes.

use ve_core::registry::EffectRegistry;
use ve_core::{
    builtin_registry, AssetId, BlendMode, ClipId, ColorSpace, Composition, CompositionId,
    Effect, EffectState, LayerId, MotionBlur, Project, Rgba, Sequence, Size, Source, TrackId,
    TrackKind, Transform, TransformState,
};
use ve_time::{Rate, Ticks};

/// How deep nesting may go before the engine stops descending.
///
/// Cycles are impossible by construction — the model refuses them and the loader
/// repairs them — so this is not what stops infinite recursion. It is a bound on
/// *cost*: every level multiplies the render targets a frame needs, and a
/// thousand-deep chain is a mistake rather than an intention. Sixteen is far
/// past what any real composite uses.
pub const MAX_NESTING_DEPTH: usize = 16;

/// What produced an entry in a plan.
///
/// Kept so the interface can map a drawn thing back to the clip or layer the
/// user selected, and so a test can say which one it means.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Origin {
    /// A clip on a sequence track.
    Clip(ClipId),
    /// A layer in a composition.
    Layer(LayerId),
}

/// Where a visible item's picture comes from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Draw {
    /// A frame of media, to be decoded.
    Media { asset: AssetId, source_time: Ticks },
    /// A nested composition, to be rendered first. `node` indexes
    /// [`RenderPlan::nodes`], and is always less than the index of the node that
    /// holds this item.
    Nested { composition: CompositionId, node: usize },
}

/// One thing to draw, in back-to-front order within its node.
#[derive(Debug, Clone, PartialEq)]
pub struct PlanItem {
    pub origin: Origin,
    pub draw: Draw,
    /// Every animated property resolved at this instant.
    pub transform: TransformState,
    /// The transform resolved again at each instant the shutter is open, when
    /// this item is motion blurred and actually moves during it. Empty
    /// otherwise, which is the overwhelmingly common case and the one that
    /// costs a single draw.
    ///
    /// The samples are the *whole* description of the blur: the renderer
    /// averages what they draw and needs to know nothing about shutters or
    /// frame rates. See [`ve_core::MotionBlur`].
    pub samples: Vec<TransformState>,
    /// The item's effect chain, resolved at this instant, in the order it runs.
    ///
    /// Only what will actually be drawn: an effect that is switched off, or
    /// that this build has no program for, is not here. So an empty chain and
    /// no chain are the same thing to everything downstream, which is what
    /// keeps the common case — no effects — free.
    pub effects: Vec<EffectState>,
    pub blend: BlendMode,
}

impl PlanItem {
    /// Whether this item is drawn once or averaged over a shutter.
    pub fn is_blurred(&self) -> bool {
        self.samples.len() > 1
    }

    /// Whether anything has to be run over this item's picture before it is
    /// drawn.
    pub fn has_effects(&self) -> bool {
        !self.effects.is_empty()
    }

    /// The media this item needs decoded, if it is media at all.
    pub fn asset(&self) -> Option<AssetId> {
        match self.draw {
            Draw::Media { asset, .. } => Some(asset),
            Draw::Nested { .. } => None,
        }
    }

    pub fn nested_node(&self) -> Option<usize> {
        match self.draw {
            Draw::Nested { node, .. } => Some(node),
            Draw::Media { .. } => None,
        }
    }
}

/// One target's worth of drawing: a canvas and what goes on it.
#[derive(Debug, Clone, PartialEq)]
pub struct PlanNode {
    /// The composition this node renders, or `None` for the sequence itself.
    pub composition: Option<CompositionId>,
    pub size: Size,
    pub background: Rgba,
    /// How this node's own layers are combined. Per node rather than per plan:
    /// a composition renders to its own target, so it can be authored in linear
    /// light and still be laid into a perceptual sequence.
    pub color_space: ColorSpace,
    /// The instant *this node's* own timeline is at, which for a nested node is
    /// its layer's source time rather than the playhead.
    pub time: Ticks,
    /// Bottom layer first, which is the order the compositor draws in.
    pub items: Vec<PlanItem>,
}

/// One item contributing sound at a given instant.
///
/// Flat across the whole tree: a nested composition's audio is lifted into the
/// root with its gain multiplied by every layer it passes through, because the
/// mixer mixes once rather than once per nesting level.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct AudibleItem {
    pub origin: Origin,
    pub asset: AssetId,
    pub source_time: Ticks,
    /// Linear gain, already resolved and composed down the nesting chain, with
    /// the track's own level folded in.
    pub gain: f64,
    /// -1 hard left to +1 hard right.
    pub pan: f64,
    /// The sequence track this sound reaches the mix through, which is what a
    /// per-track meter is grouped by.
    ///
    /// `None` when a composition is being previewed on its own: there is no
    /// sequence, so there is no track for its layers to belong to.
    pub track: Option<TrackId>,
}

/// Everything a single instant resolves to.
#[derive(Debug, Clone, PartialEq)]
pub struct RenderPlan {
    /// The playhead this plan describes, in the sequence's own time.
    pub time: Ticks,
    /// Render order: a node's children always come before it, and the root is
    /// last. Empty only when there is no sequence at all.
    pub nodes: Vec<PlanNode>,
    pub audio: Vec<AudibleItem>,
}

impl RenderPlan {
    /// An empty plan for a canvas of `size`.
    pub fn empty(time: Ticks, size: Size, background: Rgba) -> Self {
        RenderPlan {
            time,
            nodes: vec![PlanNode {
                composition: None,
                size,
                background,
                color_space: ColorSpace::default(),
                time,
                items: Vec::new(),
            }],
            audio: Vec::new(),
        }
    }

    /// The node everything else feeds into.
    pub fn root(&self) -> &PlanNode {
        self.nodes.last().expect("a plan always has a root")
    }

    /// The index of the root node, which is what a renderer presents.
    pub fn root_index(&self) -> usize {
        self.nodes.len() - 1
    }

    pub fn size(&self) -> Size {
        self.root().size
    }

    pub fn background(&self) -> Rgba {
        self.root().background
    }

    /// Whether nothing at all is drawn or heard.
    pub fn is_empty(&self) -> bool {
        self.root().items.is_empty() && self.audio.is_empty()
    }

    /// How many things are drawn across every node, nested ones included.
    pub fn item_count(&self) -> usize {
        self.nodes.iter().map(|n| n.items.len()).sum()
    }

    /// How many nested compositions this instant needs rendered first.
    pub fn nested_count(&self) -> usize {
        self.nodes.len() - 1
    }

    /// Assets this instant needs decoded, without duplicates.
    ///
    /// Only media: a nested composition needs rendering, not decoding.
    pub fn required_assets(&self) -> Vec<AssetId> {
        let mut assets: Vec<AssetId> = self
            .nodes
            .iter()
            .flat_map(|n| n.items.iter().filter_map(|i| i.asset()))
            .chain(self.audio.iter().map(|a| a.asset))
            .collect();
        assets.sort_unstable();
        assets.dedup();
        assets
    }
}

/// Resolves a sequence at `at` into a plan, without needing the project.
///
/// Compositions cannot be reached this way — they live on the project, not the
/// sequence — so a clip holding one contributes nothing. Use
/// [`evaluate_project`] for a sequence that may nest.
pub fn evaluate(sequence: &Sequence, at: Ticks) -> RenderPlan {
    Evaluator::new(None).plan(sequence, at)
}

/// Resolves a sequence at `at`, descending into every composition it draws.
pub fn evaluate_project(project: &Project, sequence: &Sequence, at: Ticks) -> RenderPlan {
    Evaluator::new(Some(project)).plan(sequence, at)
}

/// Resolves the instants a range of frames lands on.
///
/// Used by export and by prefetch, both of which want to know what is coming
/// rather than only what is current.
pub fn evaluate_frames(
    project: &Project,
    sequence: &Sequence,
    first_frame: i64,
    count: usize,
) -> Vec<RenderPlan> {
    let rate = sequence.rate();
    (0..count)
        .map(|i| {
            evaluate_project(project, sequence, rate.frame_to_ticks(first_frame + i as i64))
        })
        .collect()
}

/// Walks a sequence and the compositions it reaches, building the node list.
struct Evaluator<'a> {
    /// `None` when evaluating a sequence in isolation, which is what the pure
    /// track-ordering tests want.
    project: Option<&'a Project>,
    /// What an effect's kind and parameters mean. The built-in table unless a
    /// host has loaded plugin effects, in which case it is that host's, so a
    /// plugin's effect resolves rather than being silently dropped.
    registry: &'static EffectRegistry,
}

impl<'a> Evaluator<'a> {
    fn new(project: Option<&'a Project>) -> Self {
        Evaluator { project, registry: builtin_registry() }
    }

    /// Every effect that will actually be drawn, resolved at clip-local time.
    fn effects(&self, effects: &[Effect], local: Ticks) -> Vec<EffectState> {
        if effects.is_empty() {
            return Vec::new();
        }
        self.registry.evaluate_chain(effects, local)
    }
}

impl Evaluator<'_> {
    /// Track order is layer order: index 0 is the bottom video layer, drawn
    /// first. Clips that are disabled, on a muted track, or excluded by a solo
    /// are left out entirely rather than being included at zero opacity, so the
    /// compositor never spends a draw call on something invisible.
    fn plan(&self, sequence: &Sequence, at: Ticks) -> RenderPlan {
        // Solo is exclusive: the moment anything is soloed, everything else on
        // that side goes quiet. Video and audio solo independently, because
        // soloing a video track to inspect it should not silence the mix.
        let video_solo =
            sequence.tracks.iter().any(|t| t.kind == TrackKind::Video && t.solo && !t.muted);
        let audio_solo =
            sequence.tracks.iter().any(|t| t.kind == TrackKind::Audio && t.solo && !t.muted);

        let mut nodes = Vec::new();
        let mut items = Vec::new();
        let mut audio = Vec::new();

        for track in &sequence.tracks {
            if track.muted {
                continue;
            }
            let soloing = match track.kind {
                TrackKind::Video => video_solo,
                TrackKind::Audio => audio_solo,
            };
            if soloing && !track.solo {
                continue;
            }

            let Some(clip) = track.clip_at(at) else { continue };
            if !clip.enabled {
                continue;
            }
            let Some(source_time) = clip.source_time_at(at) else { continue };
            // Properties are keyed on clip-local time, so moving a clip along
            // the timeline carries its animation with it.
            let local = clip.local_time_at(at);
            // Everything this track adds to the mix lands after here, so the
            // track's own level is applied to that slice once the clip and any
            // composition under it have had their say. Folding it in afterwards
            // rather than threading a gain through every call keeps nesting and
            // track level independent of each other.
            let from = audio.len();

            match track.kind {
                TrackKind::Video => {
                    let samples = motion_samples(
                        &clip.transform,
                        local,
                        clip.motion_blur,
                        sequence.settings.motion_blur,
                        sequence.rate(),
                    );
                    if let Some(item) = self.video_item(
                        Origin::Clip(clip.id),
                        clip.source,
                        source_time,
                        clip.transform.evaluate(local),
                        samples,
                        self.effects(&clip.effects, local),
                        clip.blend,
                        &mut nodes,
                        &mut audio,
                        1,
                    ) {
                        items.push(item);
                    }
                }
                TrackKind::Audio => {
                    let (gain, pan) = clip.audio.evaluate(local, clip.duration);
                    self.audio_items(
                        Origin::Clip(clip.id),
                        clip.source,
                        source_time,
                        gain,
                        pan,
                        &mut audio,
                        1,
                    );
                }
            }

            apply_track_level(&mut audio[from..], track);
        }

        nodes.push(PlanNode {
            composition: None,
            size: sequence.settings.resolution,
            background: sequence.settings.background,
            color_space: sequence.settings.color_space,
            time: at,
            items,
        });
        RenderPlan { time: at, nodes, audio }
    }

    /// Resolves one visible thing, descending into a composition if that is what
    /// it draws. Returns `None` when the source cannot contribute a picture.
    #[allow(clippy::too_many_arguments)]
    fn video_item(
        &self,
        origin: Origin,
        source: Source,
        source_time: Ticks,
        transform: TransformState,
        samples: Vec<TransformState>,
        effects: Vec<EffectState>,
        blend: BlendMode,
        nodes: &mut Vec<PlanNode>,
        audio: &mut Vec<AudibleItem>,
        depth: usize,
    ) -> Option<PlanItem> {
        let draw = match source {
            Source::Asset(asset) => Draw::Media { asset, source_time },
            Source::Composition(id) => {
                let project = self.project?;
                if depth > MAX_NESTING_DEPTH {
                    log::warn!(
                        "composition {id} nested more than {MAX_NESTING_DEPTH} deep; not drawn"
                    );
                    return None;
                }
                let composition = project.composition(id)?;
                // The nested composition is at its own local time, which is
                // where the layer is reading from — so a nested composite
                // scrubs, plays and slips with the layer that holds it.
                let node =
                    self.nested_node(composition, source_time, nodes, audio, depth, transform)?;
                Draw::Nested { composition: id, node }
            }
        };
        Some(PlanItem { origin, draw, transform, samples, effects, blend })
    }

    /// Builds the node for a nested composition and returns its index.
    ///
    /// Children are pushed before the node itself, which is what keeps
    /// [`RenderPlan::nodes`] in render order. `holder` is the transform of the
    /// layer holding this composition, whose opacity scales the nested audio the
    /// same way it scales the picture.
    fn nested_node(
        &self,
        composition: &Composition,
        at: Ticks,
        nodes: &mut Vec<PlanNode>,
        audio: &mut Vec<AudibleItem>,
        depth: usize,
        holder: TransformState,
    ) -> Option<usize> {
        let mut items = Vec::new();
        for layer in composition.layers_at(at) {
            let Some(source_time) = layer.source_time_at(at) else { continue };
            let local = layer.local_time_at(at);
            let transform = layer.transform.evaluate(local);
            let samples = motion_samples(
                &layer.transform,
                local,
                layer.motion_blur,
                composition.settings.motion_blur,
                composition.settings.rate,
            );

            if let Some(item) = self.video_item(
                Origin::Layer(layer.id),
                layer.source,
                source_time,
                transform,
                samples,
                self.effects(&layer.effects, local),
                layer.blend,
                nodes,
                audio,
                depth + 1,
            ) {
                items.push(item);
            }

            // A layer's own sound, lifted into the root mix. Scaled by the
            // holding layer's opacity so fading a nested composition out takes
            // its audio with it, which is what the picture doing so implies.
            let (gain, pan) = layer.audio.evaluate(local, layer.duration);
            self.audio_items(
                Origin::Layer(layer.id),
                layer.source,
                source_time,
                gain * holder.opacity.clamp(0.0, 1.0),
                pan,
                audio,
                depth + 1,
            );
        }

        nodes.push(PlanNode {
            composition: Some(composition.id),
            size: composition.settings.resolution,
            background: composition.settings.background,
            color_space: composition.settings.color_space,
            time: at,
            items,
        });
        Some(nodes.len() - 1)
    }

    /// Collects the sound a source contributes, descending into compositions.
    ///
    /// Gain multiplies down the chain, which is what "the nested composition
    /// plays through this layer's volume" means. Pan is summed and clamped: a
    /// parent that pushes its mix right should carry its children with it, and
    /// summing offsets is the approximation every editor makes short of
    /// rotating a stereo field per level.
    #[allow(clippy::too_many_arguments)]
    fn audio_items(
        &self,
        origin: Origin,
        source: Source,
        source_time: Ticks,
        gain: f64,
        pan: f64,
        audio: &mut Vec<AudibleItem>,
        depth: usize,
    ) {
        match source {
            Source::Asset(asset) => {
                audio.push(AudibleItem {
                    origin,
                    asset,
                    source_time,
                    gain,
                    pan: pan.clamp(-1.0, 1.0),
                    track: None,
                });
            }
            Source::Composition(id) => {
                let Some(project) = self.project else { return };
                if depth > MAX_NESTING_DEPTH {
                    return;
                }
                let Some(composition) = project.composition(id) else { return };
                for layer in composition.layers_at(source_time) {
                    let Some(inner_time) = layer.source_time_at(source_time) else { continue };
                    let local = layer.local_time_at(source_time);
                    let (inner_gain, inner_pan) = layer.audio.evaluate(local, layer.duration);
                    self.audio_items(
                        Origin::Layer(layer.id),
                        layer.source,
                        inner_time,
                        gain * inner_gain,
                        pan + inner_pan,
                        audio,
                        depth + 1,
                    );
                }
            }
        }
    }
}

/// The transform resolved at each instant the shutter is open.
///
/// Empty unless the item asks to be blurred, the canvas's shutter is open at
/// all, and the transform **actually differs** across the interval. That last
/// check is what keeps the cost honest: a clip with keyframes an hour apart is
/// animated, but during any one frame it is standing still, and a standing
/// layer drawn sixteen times is sixteen times the cost of the same picture.
fn motion_samples(
    transform: &Transform,
    local: Ticks,
    wanted: bool,
    blur: MotionBlur,
    rate: Rate,
) -> Vec<TransformState> {
    if !wanted || !blur.is_active() || !transform.is_animated() {
        return Vec::new();
    }
    let samples: Vec<TransformState> = blur
        .offsets(rate.frame_duration())
        .into_iter()
        .map(|offset| transform.evaluate(local + offset))
        .collect();
    if samples.windows(2).all(|pair| pair[0] == pair[1]) {
        return Vec::new();
    }
    samples
}

/// Folds a track's level into every sound that reached the mix through it, and
/// records which track that was.
///
/// Gain multiplies and pan adds, exactly as they do down a nesting chain: a
/// track pulled down 6 dB pulls everything on it down 6 dB, and a track pushed
/// right carries its clips' own positions with it rather than overriding them.
fn apply_track_level(items: &mut [AudibleItem], track: &ve_core::Track) {
    let (gain, pan) = track.audio_level();
    for item in items {
        item.track = Some(track.id);
        item.gain *= gain;
        item.pan = (item.pan + pan).clamp(-1.0, 1.0);
    }
}

/// Resolves a composition on its own, for previewing one while it is open.
///
/// The composition becomes the root node rather than being nested inside a
/// sequence, which is exactly what "open this composition and look at it" means.
pub fn evaluate_composition(
    project: &Project,
    composition: CompositionId,
    at: Ticks,
) -> RenderPlan {
    let Some(comp) = project.composition(composition) else {
        return RenderPlan::empty(at, Size::new(1920, 1080), Rgba::BLACK);
    };
    let evaluator = Evaluator::new(Some(project));
    let mut nodes = Vec::new();
    let mut audio = Vec::new();
    // Depth 0 so the composition's own layers sit at depth 1, matching what they
    // cost when reached through a sequence.
    let root =
        evaluator.nested_node(comp, at, &mut nodes, &mut audio, 0, TransformState::default());
    debug_assert_eq!(root, Some(nodes.len() - 1), "the root has to be the last node");
    RenderPlan { time: at, nodes, audio }
}
