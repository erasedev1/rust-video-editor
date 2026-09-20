//! Orchestration: clock, plan evaluation, decode scheduling.

use std::sync::Arc;

use ve_core::{CompositionId, Project, SequenceId, Size};
use ve_media::{DecodeService, FrameRequest, MediaError, VideoFrame};
use ve_metrics::{spans, Metrics};
use ve_time::{Rate, Ticks};

use crate::clock::{PlaybackClock, TransportState};
use crate::plan::{
    evaluate_composition, evaluate_project, Draw, PlanItem, PlanNode, RenderPlan,
};

/// How far ahead to decode while playing, in frames.
///
/// Enough to ride out a slow decode without committing so much of the cache to
/// the future that scrubbing backwards loses everything it just had.
const DEFAULT_PREFETCH_FRAMES: i64 = 12;

/// What the transport and the preview are pointed at.
///
/// A composition is editable in its own right, so the playhead, the preview and
/// the transport all have to be able to mean "this composition" rather than
/// always meaning the sequence. Everything downstream reads the rate, duration
/// and canvas from here, so opening a composition does not need a second engine.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Viewing {
    Sequence(SequenceId),
    Composition(CompositionId),
}

impl Viewing {
    /// The timebase and canvas of whatever is being viewed.
    pub fn timebase(self, project: &Project) -> Option<Timebase> {
        match self {
            Viewing::Sequence(id) => project.sequence(id).map(|s| Timebase {
                rate: s.rate(),
                duration: s.duration(),
                size: s.settings.resolution,
            }),
            Viewing::Composition(id) => project.composition(id).map(|c| Timebase {
                rate: c.rate(),
                duration: c.duration(),
                size: c.size(),
            }),
        }
    }

    pub fn composition(self) -> Option<CompositionId> {
        match self {
            Viewing::Composition(id) => Some(id),
            Viewing::Sequence(_) => None,
        }
    }

    pub fn sequence(self) -> Option<SequenceId> {
        match self {
            Viewing::Sequence(id) => Some(id),
            Viewing::Composition(_) => None,
        }
    }
}

/// The frame grid, length and canvas of whatever is being viewed.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Timebase {
    pub rate: Rate,
    pub duration: Ticks,
    pub size: Size,
}

impl Timebase {
    /// Snaps to the frame grid. Every user-facing time lands on a frame
    /// boundary; sub-frame positions only exist inside the audio engine.
    pub fn snap(&self, t: Ticks) -> Ticks {
        self.rate.snap_round(t)
    }
}

/// One thing the compositor can draw, with its picture in hand.
#[derive(Debug, Clone)]
pub struct ResolvedLayer {
    pub item: PlanItem,
    pub content: LayerContent,
}

/// Where a resolved layer's pixels come from.
#[derive(Debug, Clone)]
pub enum LayerContent {
    /// A decoded frame of media.
    Frame(VideoFrame),
    /// Another node of this update, already resolved because nodes come in
    /// render order. Indexes [`EngineUpdate::nodes`].
    Nested(usize),
}

/// One target's worth of resolved drawing.
///
/// Parallel to [`RenderPlan::nodes`] — same length, same order, same indices —
/// so a renderer reads the canvas from the plan and what to draw from here.
#[derive(Debug, Clone, Default)]
pub struct ResolvedNode {
    /// Bottom first, in draw order. Only what is actually drawable: an item
    /// whose frame has not arrived is counted in `pending` instead.
    pub layers: Vec<ResolvedLayer>,
    /// Items in *this* node still waiting on a decode.
    pub pending: usize,
}

/// What the engine resolved for one instant.
pub struct EngineUpdate {
    pub position: Ticks,
    pub plan: RenderPlan,
    /// Resolved nodes, parallel to `plan.nodes`: children first, root last.
    pub nodes: Vec<ResolvedNode>,
    /// Items anywhere in the tree whose frames are still decoding. Non-zero
    /// means the preview is showing an incomplete picture this instant.
    pub pending: usize,
    pub playing: bool,
    /// Set when playback ran to the end during this update.
    pub reached_end: bool,
}

impl EngineUpdate {
    /// Whether every visible layer, at every level, had a frame ready.
    pub fn is_complete(&self) -> bool {
        self.pending == 0
    }

    /// The node the preview presents.
    pub fn root(&self) -> &ResolvedNode {
        self.nodes.last().expect("a plan always has a root")
    }

    /// How many layers are drawn in total, nested ones included.
    pub fn layer_count(&self) -> usize {
        self.nodes.iter().map(|n| n.layers.len()).sum()
    }
}

/// Drives playback: reads the clock, resolves the instant into a plan, and asks
/// the decode service for what it needs.
///
/// Holds no GPU state and does no drawing. The UI calls [`PlaybackEngine::update`]
/// once per repaint and composites whatever came back, which is what keeps the
/// playback logic testable without a window.
pub struct PlaybackEngine {
    clock: PlaybackClock,
    decode: Arc<DecodeService>,
    metrics: Metrics,
    prefetch_frames: i64,
}

impl PlaybackEngine {
    pub fn new(clock: PlaybackClock, decode: Arc<DecodeService>, metrics: Metrics) -> Self {
        PlaybackEngine { clock, decode, metrics, prefetch_frames: DEFAULT_PREFETCH_FRAMES }
    }

    pub fn clock(&self) -> &PlaybackClock {
        &self.clock
    }

    pub fn clock_mut(&mut self) -> &mut PlaybackClock {
        &mut self.clock
    }

    pub fn decode_service(&self) -> &Arc<DecodeService> {
        &self.decode
    }

    pub fn set_prefetch_frames(&mut self, frames: i64) {
        self.prefetch_frames = frames.max(0);
    }

    /// Opens a decoder for every asset the project references.
    ///
    /// Each one is opened on whichever file the project says its picture comes
    /// from — the original, or a proxy where there is one and proxies are
    /// switched on. `picture_source` decides, so there is one copy of that rule
    /// rather than one per call site.
    ///
    /// Paths are already absolute by the time a project is in memory: loading
    /// resolves them, which is why no project directory is needed here.
    ///
    /// Returns the assets that could not be opened, paired with why. A failure
    /// is not fatal: the rest of the timeline still plays, and the offending
    /// clip renders as offline.
    pub fn open_project_assets(
        &self,
        project: &Project,
    ) -> Vec<(ve_core::AssetId, MediaError)> {
        let mut failures = Vec::new();
        let proxies = project.settings.use_proxies;
        for asset in &project.assets {
            if asset.offline || !asset.info.has_video() || self.decode.is_open(asset.id) {
                continue;
            }
            let source = asset.picture_source(None, proxies);
            if let Err(e) = self.decode.open(asset.id, &source.path, None) {
                failures.push((asset.id, e));
            }
        }
        failures
    }

    /// Closes every asset's decoder and opens it again.
    ///
    /// What switching proxies on or off *is*: which file an asset decodes from
    /// is fixed when its worker is opened, so changing the answer means
    /// replacing the worker. Closing also drops that asset's cached frames,
    /// which would otherwise sit there at the wrong resolution — harmlessly,
    /// since the cache key carries the width, but taking up the budget the new
    /// ones need.
    ///
    /// Assets with no proxy are reopened too. That is one wasted file open per
    /// asset on a toggle the user does by hand, against a rule that has no
    /// exceptions to get wrong.
    pub fn reopen_project_assets(
        &self,
        project: &Project,
    ) -> Vec<(ve_core::AssetId, MediaError)> {
        for asset in &project.assets {
            self.decode.close(asset.id);
        }
        self.open_project_assets(project)
    }

    /// Stops playback and moves the playhead.
    ///
    /// Scrubbing goes through here so that the requested frame is posted as
    /// interactive work, which supersedes any decode still in flight for a
    /// position the user has already left.
    pub fn scrub_to(&mut self, project: &Project, viewing: Viewing, to: Ticks) {
        let Some(timebase) = viewing.timebase(project) else { return };
        let snapped = timebase.snap(to).clamp_non_negative();
        self.clock.seek(snapped);
        let plan = self.plan_for(project, viewing, snapped);
        for item in plan.nodes.iter().flat_map(|n| &n.items) {
            if let Draw::Media { asset, source_time } = item.draw {
                self.decode.request(FrameRequest::interactive(asset, source_time));
            }
        }
    }

    pub fn play(&mut self, project: &Project, viewing: Viewing) {
        let Some(timebase) = viewing.timebase(project) else { return };
        // Starting from the very end would stop immediately; rewind instead,
        // which is what pressing play at the end should do.
        let from =
            if self.clock.position() >= timebase.duration && timebase.duration > Ticks::ZERO {
                Ticks::ZERO
            } else {
                self.clock.position()
            };
        self.clock.set_limit(Some(timebase.duration));
        self.clock.play(from);
        self.metrics.reset_playback_counters();
    }

    pub fn stop(&mut self) -> Ticks {
        self.clock.stop()
    }

    pub fn is_playing(&self) -> bool {
        self.clock.is_playing()
    }

    /// Resolves the current instant and schedules the decoding it needs.
    ///
    /// Non-blocking: layers whose frames are not decoded yet are reported as
    /// pending rather than waited for, so a slow decode drops a frame instead
    /// of stalling the interface.
    pub fn update(&mut self, project: &Project, viewing: Viewing) -> EngineUpdate {
        let _span = self.metrics.span(spans::ENGINE);

        let mut reached_end = false;
        if self.clock.has_reached_limit() {
            self.clock.stop();
            reached_end = true;
        }

        let playing = self.clock.is_playing();
        let timebase = viewing.timebase(project);
        // Snapping means every request in a frame's worth of wall-clock time
        // resolves to the same cache key, so playback does not decode a frame
        // twice just because two repaints landed inside it.
        let position = match timebase {
            Some(t) => t.snap(self.clock.position()),
            None => self.clock.position(),
        };
        let plan = self.plan_for(project, viewing, position);

        let mut nodes: Vec<ResolvedNode> = Vec::with_capacity(plan.nodes.len());
        let mut pending = 0usize;

        // In render order, children before parents, which is the order the
        // renderer walks them in.
        for node in &plan.nodes {
            let resolved = self.resolve_node(node);
            pending += resolved.pending;
            nodes.push(resolved);
        }

        if pending > 0 && playing {
            // A frame the clock wanted that was not ready is a dropped frame,
            // and saying so is the only way the overlay can be honest about
            // playback quality.
            self.metrics.frame_dropped();
        } else if playing {
            self.metrics.frame_presented();
        }

        if playing {
            if let Some(timebase) = timebase {
                self.prefetch(project, viewing, timebase, position);
            }
        }

        EngineUpdate { position, plan, nodes, pending, playing, reached_end }
    }

    /// Resolves one node against what is already decoded.
    fn resolve_node(&self, node: &PlanNode) -> ResolvedNode {
        let mut layers = Vec::with_capacity(node.items.len());
        let mut pending = 0usize;

        for item in &node.items {
            match item.draw {
                Draw::Media { asset, source_time } => {
                    match self.decode.cached_frame(asset, source_time) {
                        Some(frame) => layers.push(ResolvedLayer {
                            item: item.clone(),
                            content: LayerContent::Frame(frame),
                        }),
                        None => {
                            pending += 1;
                            // Even while playing this is interactive work: it is
                            // the frame being shown right now, and it must beat
                            // any read-ahead already queued.
                            self.decode.request(FrameRequest::interactive(asset, source_time));
                        }
                    }
                }
                // A nested node is drawn even while something inside it is
                // still decoding: a partly-filled composite is a better preview
                // than a hole where it should be.
                Draw::Nested { node: index, .. } => {
                    layers.push(ResolvedLayer {
                        item: item.clone(),
                        content: LayerContent::Nested(index),
                    });
                }
            }
        }

        ResolvedNode { layers, pending }
    }

    fn plan_for(&self, project: &Project, viewing: Viewing, at: Ticks) -> RenderPlan {
        match viewing {
            Viewing::Sequence(id) => match project.sequence(id) {
                Some(sequence) => evaluate_project(project, sequence, at),
                None => RenderPlan::empty(
                    at,
                    project.settings.default_sequence.resolution,
                    project.settings.default_sequence.background,
                ),
            },
            Viewing::Composition(id) => evaluate_composition(project, id, at),
        }
    }

    /// Queues decoding for the frames just after `position`.
    fn prefetch(
        &self,
        project: &Project,
        viewing: Viewing,
        timebase: Timebase,
        position: Ticks,
    ) {
        if self.prefetch_frames == 0 {
            return;
        }
        let current_frame = timebase.rate.ticks_to_frame(position);

        for ahead in 1..=self.prefetch_frames {
            let at = timebase.rate.frame_to_ticks(current_frame + ahead);
            if at >= timebase.duration {
                break;
            }
            let plan = self.plan_for(project, viewing, at);
            for item in plan.nodes.iter().flat_map(|n| &n.items) {
                let Draw::Media { asset, source_time } = item.draw else { continue };
                // Skip what is already decoded, so read-ahead does not fill the
                // queue with work the cache has already done.
                if self.decode.cached_frame(asset, source_time).is_none() {
                    self.decode.request(FrameRequest::prefetch(asset, source_time));
                }
            }
        }
    }

    /// The transport state, for the UI.
    pub fn transport(&self) -> TransportState {
        self.clock.state()
    }

    /// Convenience for the common "play/pause" key.
    pub fn toggle_playback(&mut self, project: &Project, viewing: Viewing) {
        if self.is_playing() {
            self.stop();
        } else {
            self.play(project, viewing);
        }
    }

    /// Steps the playhead by whole frames, stopping playback first.
    pub fn step_frames(&mut self, project: &Project, viewing: Viewing, delta: i64) {
        self.clock.stop();
        let Some(timebase) = viewing.timebase(project) else { return };
        let frame = timebase.rate.ticks_to_frame_round(self.clock.position()) + delta;
        let to = timebase.rate.frame_to_ticks(frame.max(0));
        self.scrub_to(project, viewing, to);
    }
}

/// Resolves a sequence at an instant without any decoding, for tests and for
/// tooling that only needs to know *what* is there.
pub fn peek(project: &Project, sequence: SequenceId, at: Ticks) -> Option<RenderPlan> {
    project.sequence(sequence).map(|s| evaluate_project(project, s, at))
}
