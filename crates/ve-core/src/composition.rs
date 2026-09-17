//! Compositions: a canvas, a duration, and a stack of layers.
//!
//! # Why this is not a sequence
//!
//! A [`crate::Sequence`] is for cutting. Its clips live on tracks, each track
//! holds them sorted and non-overlapping, and that invariant is what makes
//! ripple, roll, slip and slide mean anything: at every instant a track has at
//! most one clip, so "the clip after this one" is a well-defined thing to move.
//!
//! Compositing wants the opposite. Layers are *meant* to overlap — that is what
//! blending is — and their order is a stack the user rearranges, not a
//! consequence of where they sit in time. Bolting that onto a track would mean
//! either abandoning the non-overlap invariant, and with it every trim
//! operation, or pretending each layer needs its own track, which is how you end
//! up with forty tracks and no way to reorder two of them.
//!
//! So a [`Composition`] is its own thing: an explicit canvas (resolution, rate,
//! duration and background of its own, not inherited from whatever it is used
//! in), and a flat, ordered [`Vec`] of [`CompositionLayer`]s. Index 0 is the
//! bottom layer, drawn first, matching both the compositor's back-to-front order
//! and the timeline's V1-at-the-bottom convention.
//!
//! # Nesting
//!
//! A layer's source is a [`Source`], so it is either media or *another
//! composition*, which is rendered into a texture and then treated exactly like
//! a decoded frame. Sequences can hold a composition the same way, which is how
//! compositing work gets cut into an edit.
//!
//! Nesting is a graph, and a graph can have cycles: a composition that contains
//! itself, directly or through a chain of others, would render forever. That is
//! checked when a layer is added ([`Composition::would_cycle`] via
//! [`crate::Project::nesting_would_cycle`]), repaired when a project is loaded,
//! and bounded by a depth limit in the engine as a last line of defence.

use serde::{Deserialize, Serialize};
use ve_time::{Rate, Ticks, TimeRange, Timecode};

use crate::blend::BlendMode;
use crate::clip::Speed;
use crate::effect::{AudioProperties, Effect, Transform};
use crate::geometry::{Rgba, Size};
use crate::id::{CompositionId, LayerId};
use crate::source::Source;
use crate::CoreError;

/// The canvas a composition renders onto.
///
/// Unlike a sequence's settings there is a `duration` here: a composition has a
/// length of its own, independent of what it happens to contain, because it is
/// used as a source and a source has to have a length before anything is in it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CompositionSettings {
    pub resolution: Size,
    pub rate: Rate,
    pub duration: Ticks,
    /// Shown where no layer covers the canvas. Transparent by default, because a
    /// composition is usually meant to be laid over something else — the
    /// opposite default from a sequence, which is what the viewer sees directly.
    #[serde(default)]
    pub background: Rgba,
}

impl Default for CompositionSettings {
    fn default() -> Self {
        CompositionSettings {
            resolution: Size::new(1920, 1080),
            rate: Rate::FPS_30,
            duration: Ticks::from_seconds(10),
            background: Rgba::TRANSPARENT,
        }
    }
}

/// One layer of a composition.
///
/// Carries the same window-onto-a-source idea as a [`crate::Clip`] — where to
/// start reading, where to sit in time, how fast to play — plus the compositing
/// state that only makes sense in a stack: its place in the order is its place
/// in the [`Composition::layers`] vector.
///
/// Deliberately a separate type from `Clip` rather than a reuse of it. They
/// agree today on almost every field, but they are heading in different
/// directions: a clip grows toward the edit (link groups, transitions, speed
/// ramps) and a layer toward the composite (parenting, track mattes, 3D,
/// motion blur). Sharing the type would mean every one of those features has to
/// be meaningful in both places, and most are not.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CompositionLayer {
    pub id: LayerId,
    pub name: String,
    pub source: Source,
    /// Where the layer starts on the composition's own timeline.
    pub start: Ticks,
    pub duration: Ticks,
    /// Offset into the source of the layer's first frame.
    #[serde(default)]
    pub source_in: Ticks,
    #[serde(default)]
    pub speed: Speed,
    #[serde(default = "crate::default_true")]
    pub enabled: bool,
    /// Excludes every other layer from the render while set, for inspecting one
    /// layer of a deep stack.
    #[serde(default)]
    pub solo: bool,
    #[serde(default)]
    pub transform: Transform,
    #[serde(default)]
    pub blend: BlendMode,
    #[serde(default)]
    pub audio: AudioProperties,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub effects: Vec<Effect>,
}

impl CompositionLayer {
    pub fn new(
        id: LayerId,
        name: impl Into<String>,
        source: impl Into<Source>,
        start: Ticks,
        duration: Ticks,
    ) -> Self {
        CompositionLayer {
            id,
            name: name.into(),
            source: source.into(),
            start,
            duration,
            source_in: Ticks::ZERO,
            speed: Speed::NORMAL,
            enabled: true,
            solo: false,
            transform: Transform::default(),
            blend: BlendMode::Normal,
            audio: AudioProperties::default(),
            effects: Vec::new(),
        }
    }

    /// The layer's footprint on the composition's timeline.
    #[inline]
    pub fn range(&self) -> TimeRange {
        TimeRange::new(self.start, self.duration)
    }

    #[inline]
    pub fn end(&self) -> Ticks {
        self.start + self.duration
    }

    /// Source ticks consumed, accounting for speed.
    #[inline]
    pub fn source_duration(&self) -> Ticks {
        self.speed.timeline_to_source(self.duration)
    }

    /// The position in the source that a composition time maps to, or `None`
    /// when the layer does not cover that instant.
    pub fn source_time_at(&self, at: Ticks) -> Option<Ticks> {
        if !self.range().contains(at) {
            return None;
        }
        Some(self.source_in + self.speed.timeline_to_source(at - self.start))
    }

    /// Layer-relative time, which is what every animated property is keyed on,
    /// so moving a layer carries its animation with it.
    #[inline]
    pub fn local_time_at(&self, at: Ticks) -> Ticks {
        at - self.start
    }

    /// Moves the layer along the composition's timeline without changing which
    /// frames it shows.
    pub fn move_to(&mut self, start: Ticks) {
        self.start = start.clamp_non_negative();
    }

    /// Trims the head to a new start, rolling the source window to match so the
    /// visible frames stay pinned where they were.
    ///
    /// The same rule as a clip's head trim. What a layer does *not* have is a
    /// neighbour to answer to: nothing on a layer stack has to stay adjacent to
    /// anything, so the only bounds are the minimum length and the start of the
    /// source.
    pub fn trim_start(
        &mut self,
        new_start: Ticks,
        min_duration: Ticks,
    ) -> Result<(), CoreError> {
        let delta = new_start - self.start;
        let new_duration = self.duration - delta;
        if new_duration < min_duration {
            return Err(CoreError::TrimTooShort);
        }
        let new_source_in = self.source_in + self.speed.timeline_to_source(delta);
        if new_source_in.is_negative() {
            return Err(CoreError::TrimBeyondSource);
        }
        self.start = new_start;
        self.source_in = new_source_in;
        self.duration = new_duration;
        Ok(())
    }

    /// Trims the tail to a new end. The source in-point is unaffected.
    pub fn trim_end(
        &mut self,
        new_end: Ticks,
        min_duration: Ticks,
        available_source: Ticks,
    ) -> Result<(), CoreError> {
        let new_duration = new_end - self.start;
        if new_duration < min_duration {
            return Err(CoreError::TrimTooShort);
        }
        if self.source_in + self.speed.timeline_to_source(new_duration) > available_source {
            return Err(CoreError::TrimBeyondSource);
        }
        self.duration = new_duration;
        Ok(())
    }

    pub fn is_animated(&self) -> bool {
        self.transform.is_animated()
            || self.audio.volume.is_animated()
            || self.audio.pan.is_animated()
            || self.effects.iter().any(|e| e.enabled && e.is_animated())
    }
}

/// A stack of layers over a canvas of its own.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Composition {
    pub id: CompositionId,
    pub name: String,
    pub settings: CompositionSettings,
    /// Bottom-first: index 0 is drawn first and everything else goes over it.
    #[serde(default)]
    pub layers: Vec<CompositionLayer>,
    /// Persisted, so reopening a project restores where the user was working.
    #[serde(default)]
    pub playhead: Ticks,
}

impl Composition {
    pub fn new(
        id: CompositionId,
        name: impl Into<String>,
        settings: CompositionSettings,
    ) -> Self {
        Composition {
            id,
            name: name.into(),
            settings,
            layers: Vec::new(),
            playhead: Ticks::ZERO,
        }
    }

    #[inline]
    pub fn rate(&self) -> Rate {
        self.settings.rate
    }

    #[inline]
    pub fn duration(&self) -> Ticks {
        self.settings.duration
    }

    #[inline]
    pub fn size(&self) -> Size {
        self.settings.resolution
    }

    pub fn layer(&self, id: LayerId) -> Option<&CompositionLayer> {
        self.layers.iter().find(|l| l.id == id)
    }

    pub fn layer_mut(&mut self, id: LayerId) -> Option<&mut CompositionLayer> {
        self.layers.iter_mut().find(|l| l.id == id)
    }

    pub fn layer_index(&self, id: LayerId) -> Option<usize> {
        self.layers.iter().position(|l| l.id == id)
    }

    /// Adds a layer on top of the stack.
    pub fn push_layer(&mut self, layer: CompositionLayer) -> LayerId {
        let id = layer.id;
        self.layers.push(layer);
        id
    }

    /// Inserts a layer at a specific depth, clamped to the stack.
    pub fn insert_layer(&mut self, index: usize, layer: CompositionLayer) -> LayerId {
        let id = layer.id;
        self.layers.insert(index.min(self.layers.len()), layer);
        id
    }

    pub fn remove_layer(&mut self, id: LayerId) -> Option<CompositionLayer> {
        let index = self.layer_index(id)?;
        Some(self.layers.remove(index))
    }

    /// Moves a layer to a new depth, shifting the rest along.
    ///
    /// Returns the index it came from, so an undo can put it back exactly. A
    /// move to its own position is not an error; it simply does nothing.
    pub fn move_layer(&mut self, id: LayerId, to: usize) -> Option<usize> {
        let from = self.layer_index(id)?;
        let to = to.min(self.layers.len().saturating_sub(1));
        if from != to {
            let layer = self.layers.remove(from);
            self.layers.insert(to, layer);
        }
        Some(from)
    }

    /// The layers that contribute at `at`, bottom first.
    ///
    /// Disabled layers are left out, and if anything is soloed then only soloed
    /// layers are included — the same rule tracks use, for the same reason:
    /// inspecting one layer should not mean toggling the other nineteen.
    pub fn layers_at(&self, at: Ticks) -> impl Iterator<Item = &CompositionLayer> {
        let soloing = self.layers.iter().any(|l| l.solo && l.enabled);
        self.layers
            .iter()
            .filter(move |l| l.enabled && (!soloing || l.solo) && l.range().contains(at))
    }

    /// End of the last layer, which is what "fit the canvas to its contents"
    /// uses. Independent of [`Composition::duration`], which the user sets.
    pub fn content_end(&self) -> Ticks {
        self.layers.iter().map(|l| l.end()).max().unwrap_or(Ticks::ZERO)
    }

    /// Every composition this one references directly, without duplicates.
    pub fn nested(&self) -> Vec<CompositionId> {
        let mut ids: Vec<CompositionId> =
            self.layers.iter().filter_map(|l| l.source.composition()).collect();
        ids.sort_unstable();
        ids.dedup();
        ids
    }

    /// Whether this composition references `id` directly.
    pub fn references(&self, id: CompositionId) -> bool {
        self.layers.iter().any(|l| l.source == Source::Composition(id))
    }

    #[inline]
    pub fn snap_to_frame(&self, t: Ticks) -> Ticks {
        self.settings.rate.snap_round(t)
    }

    pub fn set_playhead(&mut self, t: Ticks) {
        self.playhead = self.snap_to_frame(t).clamp_non_negative();
    }

    pub fn timecode_at(&self, t: Ticks) -> Timecode {
        Timecode::from_ticks(t, self.settings.rate)
    }

    /// Whether putting `candidate` inside this composition would close a cycle,
    /// given a way to look compositions up.
    ///
    /// Takes a resolver rather than the project so that the check lives with the
    /// data it is about; [`crate::Project::nesting_would_cycle`] is the call
    /// everything actually uses.
    pub fn would_cycle<'a>(
        &self,
        candidate: CompositionId,
        lookup: impl Fn(CompositionId) -> Option<&'a Composition>,
    ) -> bool {
        if candidate == self.id {
            return true;
        }
        // Depth-first from the candidate: a cycle exists exactly when this
        // composition is reachable from the thing being put inside it.
        let mut stack = vec![candidate];
        let mut seen: Vec<CompositionId> = Vec::new();
        while let Some(next) = stack.pop() {
            if next == self.id {
                return true;
            }
            if seen.contains(&next) {
                continue;
            }
            seen.push(next);
            if let Some(comp) = lookup(next) {
                stack.extend(comp.nested());
            }
        }
        false
    }

    /// Re-establishes invariants after loading. Returns warnings about anything
    /// repaired, which the loader surfaces rather than silently fixing.
    pub(crate) fn normalise(&mut self) -> Vec<String> {
        let mut warnings = Vec::new();

        // A self-reference is unrenderable and cannot be repaired by reordering,
        // so the offending layers go; everything else about the composition is
        // kept. Cycles through other compositions are broken by the project,
        // which is the only place that can see them.
        let id = self.id;
        let name = self.name.clone();
        self.layers.retain(|layer| {
            if layer.source == Source::Composition(id) {
                warnings.push(format!(
                    "composition '{name}': layer '{}' contained the composition itself; removed",
                    layer.name
                ));
                return false;
            }
            true
        });

        for layer in &mut self.layers {
            if layer.duration.is_negative() {
                warnings.push(format!(
                    "composition '{name}': layer '{}' had a negative duration; clamped",
                    layer.name
                ));
                layer.duration = Ticks::ZERO;
            }
            layer.start = layer.start.clamp_non_negative();
        }

        if self.settings.duration.is_negative() {
            warnings.push(format!(
                "composition '{name}': negative duration; clamped to its content"
            ));
            self.settings.duration = self.content_end();
        }
        self.playhead = self.playhead.clamp_non_negative();
        warnings
    }
}
