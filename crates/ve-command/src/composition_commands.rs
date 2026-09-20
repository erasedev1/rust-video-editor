//! Commands that create compositions and edit their layer stacks.
//!
//! Everything here goes through the same [`Command`] contract as cutting does,
//! which is what makes a composite undoable in the same breath as a trim: one
//! history, one dirty flag, one scripting surface.
//!
//! Two rules are enforced here rather than in the interface, because they are
//! about the model staying coherent and not about what is convenient to click:
//!
//! * a nested reference that would close a cycle is refused, since a
//!   composition containing itself is a render that never finishes;
//! * a composition still used anywhere cannot be deleted, so a source never
//!   dangles.

use std::any::Any;

use ve_core::{
    Clip, ClipId, Composition, CompositionId, CompositionLayer, CompositionSettings, CoreError,
    LayerId, Project, SequenceId, Source, TrackId,
};
use ve_time::Ticks;

use crate::{clip_of, track_mut, Command, CommandError};

/// Resolves a composition for mutation, turning a missing one into a typed
/// error rather than a panic.
fn composition_mut(
    project: &mut Project,
    id: CompositionId,
) -> Result<&mut Composition, CommandError> {
    project.composition_mut(id).ok_or(CommandError::CompositionNotFound(id))
}

fn layer_mut(
    project: &mut Project,
    composition: CompositionId,
    layer: LayerId,
) -> Result<&mut CompositionLayer, CommandError> {
    composition_mut(project, composition)?
        .layer_mut(layer)
        .ok_or(CommandError::LayerNotFound(layer))
}

/// The part of a layer that every trim-shaped edit has to put back on undo.
///
/// The layer equivalent of [`crate::ClipWindow`]: a head trim moves the layer
/// *and* rolls its source window, so restoring one without the other would
/// silently change which frames it shows.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct LayerWindow {
    source_in: Ticks,
    start: Ticks,
    duration: Ticks,
}

impl LayerWindow {
    fn capture(layer: &CompositionLayer) -> Self {
        LayerWindow { source_in: layer.source_in, start: layer.start, duration: layer.duration }
    }

    fn restore(self, layer: &mut CompositionLayer) {
        layer.source_in = self.source_in;
        layer.start = self.start;
        layer.duration = self.duration;
    }
}

// ---- compositions -------------------------------------------------------

/// Creates an empty composition.
#[derive(Debug)]
pub struct AddComposition {
    name: String,
    settings: CompositionSettings,
    /// Allocated on the first apply and reused by redo, so a redone composition
    /// is the same composition — anything referring to it still does.
    id: Option<CompositionId>,
}

impl AddComposition {
    pub fn new(name: impl Into<String>, settings: CompositionSettings) -> Self {
        AddComposition { name: name.into(), settings, id: None }
    }

    /// The composition's ID, once the command has been applied.
    pub fn composition_id(&self) -> Option<CompositionId> {
        self.id
    }
}

impl Command for AddComposition {
    fn name(&self) -> &str {
        "New Composition"
    }

    fn apply(&mut self, project: &mut Project) -> Result<(), CommandError> {
        match self.id {
            Some(id) => {
                project.compositions.push(Composition::new(
                    id,
                    self.name.clone(),
                    self.settings.clone(),
                ));
            }
            None => {
                let id = project.add_composition(self.name.clone(), self.settings.clone());
                self.id = Some(id);
            }
        }
        Ok(())
    }

    fn undo(&mut self, project: &mut Project) -> Result<(), CommandError> {
        let id = self.id.ok_or_else(|| CommandError::Rejected("never applied".into()))?;
        project.remove_composition(id).map(|_| ()).map_err(Into::into)
    }

    fn as_any(&self) -> &dyn Any {
        self
    }
}

/// Deletes a composition, remembering it whole so undo can put it back.
#[derive(Debug)]
pub struct RemoveComposition {
    id: CompositionId,
    /// Captured on apply, including its position in the list, so undo restores
    /// the project exactly rather than moving the composition to the end.
    removed: Option<(usize, Composition)>,
}

impl RemoveComposition {
    pub fn new(id: CompositionId) -> Self {
        RemoveComposition { id, removed: None }
    }
}

impl Command for RemoveComposition {
    fn name(&self) -> &str {
        "Delete Composition"
    }

    fn apply(&mut self, project: &mut Project) -> Result<(), CommandError> {
        let index = project
            .compositions
            .iter()
            .position(|c| c.id == self.id)
            .ok_or(CommandError::CompositionNotFound(self.id))?;
        // The model refuses while anything still draws from it, which is the
        // check that keeps a source from dangling.
        let composition = project.remove_composition(self.id)?;
        self.removed = Some((index, composition));
        Ok(())
    }

    fn undo(&mut self, project: &mut Project) -> Result<(), CommandError> {
        let (index, composition) = self
            .removed
            .take()
            .ok_or_else(|| CommandError::Rejected("nothing captured to restore".into()))?;
        let index = index.min(project.compositions.len());
        project.compositions.insert(index, composition);
        Ok(())
    }

    fn as_any(&self) -> &dyn Any {
        self
    }
}

/// Renames a composition.
#[derive(Debug)]
pub struct RenameComposition {
    id: CompositionId,
    name: String,
    previous: Option<String>,
}

impl RenameComposition {
    pub fn new(id: CompositionId, name: impl Into<String>) -> Self {
        RenameComposition { id, name: name.into(), previous: None }
    }
}

impl Command for RenameComposition {
    fn name(&self) -> &str {
        "Rename Composition"
    }

    fn apply(&mut self, project: &mut Project) -> Result<(), CommandError> {
        let composition = composition_mut(project, self.id)?;
        if self.previous.is_none() {
            self.previous = Some(composition.name.clone());
        }
        composition.name = self.name.clone();
        Ok(())
    }

    fn undo(&mut self, project: &mut Project) -> Result<(), CommandError> {
        let previous = self
            .previous
            .clone()
            .ok_or_else(|| CommandError::Rejected("never applied".into()))?;
        composition_mut(project, self.id)?.name = previous;
        Ok(())
    }

    /// Typing in the name field is one decision, not one per keystroke.
    fn merge(&mut self, next: &dyn Command) -> bool {
        match next.as_any().downcast_ref::<RenameComposition>() {
            Some(other) if other.id == self.id => {
                self.name = other.name.clone();
                true
            }
            _ => false,
        }
    }

    fn as_any(&self) -> &dyn Any {
        self
    }
}

/// Changes a composition's canvas: resolution, rate, duration and background.
///
/// Wholesale rather than one command per field, because the settings dialog
/// applies them together and undoing half a format change is not a state anyone
/// wants to be in.
#[derive(Debug)]
pub struct SetCompositionSettings {
    id: CompositionId,
    settings: CompositionSettings,
    previous: Option<CompositionSettings>,
}

impl SetCompositionSettings {
    pub fn new(id: CompositionId, settings: CompositionSettings) -> Self {
        SetCompositionSettings { id, settings, previous: None }
    }
}

impl Command for SetCompositionSettings {
    fn name(&self) -> &str {
        "Composition Settings"
    }

    fn apply(&mut self, project: &mut Project) -> Result<(), CommandError> {
        if self.settings.duration.is_negative() {
            return Err(CommandError::Rejected(
                "a composition cannot be shorter than nothing".into(),
            ));
        }
        let composition = composition_mut(project, self.id)?;
        if self.previous.is_none() {
            self.previous = Some(composition.settings.clone());
        }
        composition.settings = self.settings.clone();
        Ok(())
    }

    fn undo(&mut self, project: &mut Project) -> Result<(), CommandError> {
        let previous = self
            .previous
            .clone()
            .ok_or_else(|| CommandError::Rejected("never applied".into()))?;
        composition_mut(project, self.id)?.settings = previous;
        Ok(())
    }

    fn merge(&mut self, next: &dyn Command) -> bool {
        match next.as_any().downcast_ref::<SetCompositionSettings>() {
            Some(other) if other.id == self.id => {
                self.settings = other.settings.clone();
                true
            }
            _ => false,
        }
    }

    fn as_any(&self) -> &dyn Any {
        self
    }
}

// ---- layers -------------------------------------------------------------

/// Adds a layer to a composition, on top of the stack by default.
#[derive(Debug)]
pub struct AddLayer {
    composition: CompositionId,
    /// Taken on apply and put back on undo, so a redo does not need a clone.
    layer: Option<CompositionLayer>,
    layer_id: LayerId,
    /// Where in the stack it goes. `None` means the top.
    index: Option<usize>,
}

impl AddLayer {
    pub fn new(composition: CompositionId, layer: CompositionLayer) -> Self {
        let layer_id = layer.id;
        AddLayer { composition, layer: Some(layer), layer_id, index: None }
    }

    /// Inserts at a specific depth rather than on top.
    pub fn at(mut self, index: usize) -> Self {
        self.index = Some(index);
        self
    }

    pub fn layer_id(&self) -> LayerId {
        self.layer_id
    }
}

impl Command for AddLayer {
    fn name(&self) -> &str {
        "Add Layer"
    }

    fn apply(&mut self, project: &mut Project) -> Result<(), CommandError> {
        let layer = self.layer.take().ok_or_else(|| {
            CommandError::Rejected("AddLayer applied twice without an undo".into())
        })?;

        // The one check that cannot be skipped: a layer whose source reaches
        // back to this composition would make the render recurse for ever.
        if let Some(inner) = layer.source.composition() {
            if project.nesting_would_cycle(self.composition, inner) {
                self.layer = Some(layer);
                return Err(CoreError::CompositionCycle.into());
            }
        }

        let composition = match composition_mut(project, self.composition) {
            Ok(composition) => composition,
            Err(e) => {
                self.layer = Some(layer);
                return Err(e);
            }
        };
        match self.index {
            Some(index) => composition.insert_layer(index, layer),
            None => composition.push_layer(layer),
        };
        Ok(())
    }

    fn undo(&mut self, project: &mut Project) -> Result<(), CommandError> {
        let composition = composition_mut(project, self.composition)?;
        self.layer = Some(
            composition
                .remove_layer(self.layer_id)
                .ok_or(CommandError::LayerNotFound(self.layer_id))?,
        );
        Ok(())
    }

    fn as_any(&self) -> &dyn Any {
        self
    }
}

/// Deletes a layer, remembering it and its depth.
#[derive(Debug)]
pub struct RemoveLayer {
    composition: CompositionId,
    layer_id: LayerId,
    removed: Option<(usize, CompositionLayer)>,
}

impl RemoveLayer {
    pub fn new(composition: CompositionId, layer_id: LayerId) -> Self {
        RemoveLayer { composition, layer_id, removed: None }
    }
}

impl Command for RemoveLayer {
    fn name(&self) -> &str {
        "Delete Layer"
    }

    fn apply(&mut self, project: &mut Project) -> Result<(), CommandError> {
        let composition = composition_mut(project, self.composition)?;
        let index = composition
            .layer_index(self.layer_id)
            .ok_or(CommandError::LayerNotFound(self.layer_id))?;
        let layer = composition
            .remove_layer(self.layer_id)
            .ok_or(CommandError::LayerNotFound(self.layer_id))?;
        self.removed = Some((index, layer));
        Ok(())
    }

    fn undo(&mut self, project: &mut Project) -> Result<(), CommandError> {
        let (index, layer) = self
            .removed
            .take()
            .ok_or_else(|| CommandError::Rejected("nothing captured to restore".into()))?;
        composition_mut(project, self.composition)?.insert_layer(index, layer);
        Ok(())
    }

    fn as_any(&self) -> &dyn Any {
        self
    }
}

/// Reorders a layer within the stack, which is what changes what covers what.
#[derive(Debug)]
pub struct MoveLayer {
    composition: CompositionId,
    layer_id: LayerId,
    to: usize,
    /// Captured on the first apply and never overwritten by a merge, so undo
    /// returns to where the drag began.
    from: Option<usize>,
}

impl MoveLayer {
    pub fn new(composition: CompositionId, layer_id: LayerId, to: usize) -> Self {
        MoveLayer { composition, layer_id, to, from: None }
    }
}

impl Command for MoveLayer {
    fn name(&self) -> &str {
        "Reorder Layer"
    }

    fn apply(&mut self, project: &mut Project) -> Result<(), CommandError> {
        let composition = composition_mut(project, self.composition)?;
        let from = composition
            .move_layer(self.layer_id, self.to)
            .ok_or(CommandError::LayerNotFound(self.layer_id))?;
        self.from.get_or_insert(from);
        Ok(())
    }

    fn undo(&mut self, project: &mut Project) -> Result<(), CommandError> {
        let from = self.from.ok_or_else(|| CommandError::Rejected("never applied".into()))?;
        composition_mut(project, self.composition)?
            .move_layer(self.layer_id, from)
            .ok_or(CommandError::LayerNotFound(self.layer_id))?;
        Ok(())
    }

    fn merge(&mut self, next: &dyn Command) -> bool {
        match next.as_any().downcast_ref::<MoveLayer>() {
            Some(other)
                if other.layer_id == self.layer_id && other.composition == self.composition =>
            {
                self.to = other.to;
                true
            }
            _ => false,
        }
    }

    fn as_any(&self) -> &dyn Any {
        self
    }
}

/// Slides a layer along the composition's timeline.
#[derive(Debug)]
pub struct MoveLayerInTime {
    composition: CompositionId,
    layer_id: LayerId,
    to: Ticks,
    from: Option<Ticks>,
}

impl MoveLayerInTime {
    pub fn new(composition: CompositionId, layer_id: LayerId, to: Ticks) -> Self {
        MoveLayerInTime { composition, layer_id, to, from: None }
    }
}

impl Command for MoveLayerInTime {
    fn name(&self) -> &str {
        "Move Layer"
    }

    fn apply(&mut self, project: &mut Project) -> Result<(), CommandError> {
        let to = self.to;
        let layer = layer_mut(project, self.composition, self.layer_id)?;
        let from = layer.start;
        layer.move_to(to);
        self.from.get_or_insert(from);
        Ok(())
    }

    fn undo(&mut self, project: &mut Project) -> Result<(), CommandError> {
        let from = self.from.ok_or_else(|| CommandError::Rejected("never applied".into()))?;
        layer_mut(project, self.composition, self.layer_id)?.move_to(from);
        Ok(())
    }

    fn merge(&mut self, next: &dyn Command) -> bool {
        match next.as_any().downcast_ref::<MoveLayerInTime>() {
            Some(other)
                if other.layer_id == self.layer_id && other.composition == self.composition =>
            {
                self.to = other.to;
                true
            }
            _ => false,
        }
    }

    fn as_any(&self) -> &dyn Any {
        self
    }
}

/// Which end of a layer a trim acts on. Mirrors [`crate::TrimEdge`] for layers.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LayerEdge {
    /// Rolls the source in-point so the visible frames stay put.
    Start,
    /// Extends or shortens the tail.
    End,
}

/// Drags one end of a layer.
#[derive(Debug)]
pub struct TrimLayer {
    composition: CompositionId,
    layer_id: LayerId,
    edge: LayerEdge,
    to: Ticks,
    /// The whole window, restored wholesale on undo: a head trim changes the
    /// start and the source in-point together, and putting back only one would
    /// change which frames the layer shows.
    before: Option<LayerWindow>,
}

impl TrimLayer {
    pub fn new(
        composition: CompositionId,
        layer_id: LayerId,
        edge: LayerEdge,
        to: Ticks,
    ) -> Self {
        TrimLayer { composition, layer_id, edge, to, before: None }
    }
}

impl Command for TrimLayer {
    fn name(&self) -> &str {
        "Trim Layer"
    }

    fn apply(&mut self, project: &mut Project) -> Result<(), CommandError> {
        // Both bounds come from the project, so they are read before the layer
        // is borrowed mutably.
        let min = project
            .composition(self.composition)
            .map(|c| c.rate().frame_to_ticks(project.settings.min_clip_frames.max(1) as i64))
            .ok_or(CommandError::CompositionNotFound(self.composition))?;
        let source = project
            .composition(self.composition)
            .and_then(|c| c.layer(self.layer_id))
            .map(|l| l.source)
            .ok_or(CommandError::LayerNotFound(self.layer_id))?;
        let available = project.source_duration(source);

        let (edge, to) = (self.edge, self.to);
        let layer = layer_mut(project, self.composition, self.layer_id)?;
        let window = LayerWindow::capture(layer);
        match edge {
            LayerEdge::Start => layer.trim_start(to, min)?,
            LayerEdge::End => layer.trim_end(to, min, available)?,
        }
        self.before.get_or_insert(window);
        Ok(())
    }

    fn undo(&mut self, project: &mut Project) -> Result<(), CommandError> {
        let before =
            self.before.ok_or_else(|| CommandError::Rejected("never applied".into()))?;
        before.restore(layer_mut(project, self.composition, self.layer_id)?);
        Ok(())
    }

    fn merge(&mut self, next: &dyn Command) -> bool {
        match next.as_any().downcast_ref::<TrimLayer>() {
            Some(other)
                if other.layer_id == self.layer_id
                    && other.composition == self.composition
                    && other.edge == self.edge =>
            {
                self.to = other.to;
                true
            }
            _ => false,
        }
    }

    fn as_any(&self) -> &dyn Any {
        self
    }
}

/// A layer's on/off switches.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LayerFlag {
    Enabled,
    Solo,
}

impl LayerFlag {
    fn label(self) -> &'static str {
        match self {
            LayerFlag::Enabled => "Enable Layer",
            LayerFlag::Solo => "Solo Layer",
        }
    }
}

/// Sets one of a layer's switches.
#[derive(Debug)]
pub struct SetLayerFlag {
    composition: CompositionId,
    layer_id: LayerId,
    flag: LayerFlag,
    value: bool,
    previous: Option<bool>,
}

impl SetLayerFlag {
    pub fn new(
        composition: CompositionId,
        layer_id: LayerId,
        flag: LayerFlag,
        value: bool,
    ) -> Self {
        SetLayerFlag { composition, layer_id, flag, value, previous: None }
    }
}

impl Command for SetLayerFlag {
    fn name(&self) -> &str {
        self.flag.label()
    }

    fn apply(&mut self, project: &mut Project) -> Result<(), CommandError> {
        let (flag, value) = (self.flag, self.value);
        let layer = layer_mut(project, self.composition, self.layer_id)?;
        let field = match flag {
            LayerFlag::Enabled => &mut layer.enabled,
            LayerFlag::Solo => &mut layer.solo,
        };
        self.previous.get_or_insert(*field);
        *field = value;
        Ok(())
    }

    fn undo(&mut self, project: &mut Project) -> Result<(), CommandError> {
        let previous =
            self.previous.ok_or_else(|| CommandError::Rejected("never applied".into()))?;
        let flag = self.flag;
        let layer = layer_mut(project, self.composition, self.layer_id)?;
        match flag {
            LayerFlag::Enabled => layer.enabled = previous,
            LayerFlag::Solo => layer.solo = previous,
        }
        Ok(())
    }

    fn as_any(&self) -> &dyn Any {
        self
    }
}

/// Sets a layer's blend mode.
#[derive(Debug)]
pub struct SetLayerBlendMode {
    composition: CompositionId,
    layer_id: LayerId,
    blend: ve_core::BlendMode,
    previous: Option<ve_core::BlendMode>,
}

impl SetLayerBlendMode {
    pub fn new(
        composition: CompositionId,
        layer_id: LayerId,
        blend: ve_core::BlendMode,
    ) -> Self {
        SetLayerBlendMode { composition, layer_id, blend, previous: None }
    }
}

impl Command for SetLayerBlendMode {
    fn name(&self) -> &str {
        "Set Blend Mode"
    }

    fn apply(&mut self, project: &mut Project) -> Result<(), CommandError> {
        let blend = self.blend;
        let layer = layer_mut(project, self.composition, self.layer_id)?;
        self.previous.get_or_insert(layer.blend);
        layer.blend = blend;
        Ok(())
    }

    fn undo(&mut self, project: &mut Project) -> Result<(), CommandError> {
        let previous =
            self.previous.ok_or_else(|| CommandError::Rejected("never applied".into()))?;
        layer_mut(project, self.composition, self.layer_id)?.blend = previous;
        Ok(())
    }

    fn merge(&mut self, next: &dyn Command) -> bool {
        match next.as_any().downcast_ref::<SetLayerBlendMode>() {
            Some(other)
                if other.layer_id == self.layer_id && other.composition == self.composition =>
            {
                self.blend = other.blend;
                true
            }
            _ => false,
        }
    }

    fn as_any(&self) -> &dyn Any {
        self
    }
}

// ---- nesting an edit ----------------------------------------------------

/// Replaces clips on a track with a single clip holding a new composition of
/// them.
///
/// This is the bridge between the two halves of the editor: cut something
/// together, then treat it as one thing to composite. It is a single command
/// rather than a [`crate::Compound`] because the composition it creates has to
/// exist before the replacement clip can reference it, and undo has to take the
/// composition away again — a sequence of independent commands would leave an
/// orphan behind if the middle step were refused.
///
/// The clips keep their IDs inside the composition's layers only in spirit: each
/// becomes a layer with a fresh layer ID, positioned relative to the earliest
/// clip so the composition starts at zero.
#[derive(Debug)]
pub struct NestClips {
    sequence: SequenceId,
    track: TrackId,
    clips: Vec<ClipId>,
    name: String,
    /// Everything the apply created or took away, so undo is exact.
    applied: Option<Applied>,
}

#[derive(Debug)]
struct Applied {
    composition: CompositionId,
    /// The clips as they were, with their original positions on the track.
    original: Vec<Clip>,
    /// The clip that replaced them.
    replacement: ClipId,
}

impl NestClips {
    pub fn new(
        sequence: SequenceId,
        track: TrackId,
        clips: Vec<ClipId>,
        name: impl Into<String>,
    ) -> Self {
        NestClips { sequence, track, clips, name: name.into(), applied: None }
    }

    pub fn composition_id(&self) -> Option<CompositionId> {
        self.applied.as_ref().map(|a| a.composition)
    }

    pub fn replacement_clip(&self) -> Option<ClipId> {
        self.applied.as_ref().map(|a| a.replacement)
    }
}

impl Command for NestClips {
    fn name(&self) -> &str {
        "Nest Clips"
    }

    fn apply(&mut self, project: &mut Project) -> Result<(), CommandError> {
        if self.clips.is_empty() {
            return Err(CommandError::Rejected("nothing selected to nest".into()));
        }

        // Read everything needed before mutating, so a refusal leaves the
        // project untouched.
        let mut originals = Vec::with_capacity(self.clips.len());
        for id in &self.clips {
            originals.push(clip_of(project, self.sequence, self.track, *id)?.clone());
        }
        originals.sort_by_key(|c| c.timeline_start);

        let origin = originals.first().expect("non-empty").timeline_start;
        let span_end = originals.iter().map(|c| c.timeline_end()).max().expect("non-empty");
        let span = span_end - origin;

        let settings = project
            .sequence(self.sequence)
            .map(|s| CompositionSettings {
                resolution: s.settings.resolution,
                rate: s.settings.rate,
                duration: span,
                // Opaque, unlike a composition made from scratch: this one stands
                // in for a stretch of the edit, and the edit was opaque.
                background: s.settings.background,
                // Inherited for the same reason. Pre-composing is meant to be a
                // reorganisation, not an edit: taking clips out of a linear
                // sequence into a perceptual composition would change how they
                // blend with each other and quietly alter the picture.
                color_space: s.settings.color_space,
                // And the same again for the shutter: the clips being nested
                // keep whichever one they were already exposed through.
                motion_blur: s.settings.motion_blur,
            })
            .ok_or(CommandError::SequenceNotFound(self.sequence))?;

        // A nested clip taken into a composition keeps nesting the thing it
        // already held, which is legal — but only if that does not reach back to
        // the composition being created. It cannot: the composition is new, so
        // nothing can reference it yet.
        let composition = project.add_composition(self.name.clone(), settings);

        // Layers, bottom first in timeline order, positioned relative to the
        // start of the selection so the composition begins at zero.
        for clip in &originals {
            let layer_id = project.new_layer_id();
            let mut layer = CompositionLayer::new(
                layer_id,
                clip.name.clone(),
                clip.source,
                clip.timeline_start - origin,
                clip.duration,
            );
            layer.source_in = clip.source_in;
            layer.speed = clip.speed;
            layer.enabled = clip.enabled;
            layer.transform = clip.transform.clone();
            layer.blend = clip.blend;
            layer.audio = clip.audio.clone();
            layer.effects = clip.effects.clone();
            project.composition_mut(composition).expect("just created").push_layer(layer);
        }

        let replacement_id = project.new_clip_id();
        let replacement = Clip::new(
            replacement_id,
            composition,
            self.name.clone(),
            Ticks::ZERO,
            origin,
            span,
        );

        // Now the mutation: the clips out, the replacement in. If the insert is
        // refused the clips go back and the composition is removed, so a failed
        // nest leaves nothing behind.
        let track = track_mut(project, self.sequence, self.track)?;
        for clip in &originals {
            track.remove_clip(clip.id)?;
        }
        if let Err(e) = track.insert_clip(replacement) {
            for clip in &originals {
                // Restoring what was just removed cannot overlap; if it somehow
                // does, the error is logged rather than swallowed and the caller
                // still sees the original cause.
                if let Err(restore) = track.insert_clip(clip.clone()) {
                    log::error!(
                        "could not restore '{}' after a failed nest: {restore}",
                        clip.name
                    );
                }
            }
            let _ = project.remove_composition(composition);
            return Err(e.into());
        }

        self.applied =
            Some(Applied { composition, original: originals, replacement: replacement_id });
        Ok(())
    }

    fn undo(&mut self, project: &mut Project) -> Result<(), CommandError> {
        let applied = self
            .applied
            .take()
            .ok_or_else(|| CommandError::Rejected("never applied".into()))?;

        let track = track_mut(project, self.sequence, self.track)?;
        track.remove_clip(applied.replacement)?;
        for clip in &applied.original {
            if let Err(e) = track.insert_clip(clip.clone()) {
                self.applied = Some(applied);
                return Err(e.into());
            }
        }
        // The composition goes too: undoing a nest should not leave the project
        // carrying a composition the user never asked for.
        project.remove_composition(applied.composition)?;
        Ok(())
    }

    fn as_any(&self) -> &dyn Any {
        self
    }
}

/// Places an existing composition on a track as a clip.
pub fn composition_clip(
    project: &mut Project,
    composition: CompositionId,
    at: Ticks,
) -> Result<Clip, CommandError> {
    let comp = project
        .composition(composition)
        .ok_or(CommandError::CompositionNotFound(composition))?;
    let (name, duration) = (comp.name.clone(), comp.duration());
    let id = project.new_clip_id();
    Ok(Clip::new(id, Source::Composition(composition), name, Ticks::ZERO, at, duration))
}
