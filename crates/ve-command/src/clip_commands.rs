//! Commands that create, move, trim, split and delete clips.

use std::any::Any;

use ve_core::{Clip, ClipId, Project, SequenceId, TrackId};
use ve_time::Ticks;

use crate::{track_mut, ClipWindow, Command, CommandError};

/// Places a clip on a track.
#[derive(Debug)]
pub struct AddClip {
    sequence: SequenceId,
    track: TrackId,
    /// The clip to insert. Taken on apply and put back on undo, so the command
    /// can be redone without cloning.
    clip: Option<Clip>,
    clip_id: ClipId,
}

impl AddClip {
    pub fn new(sequence: SequenceId, track: TrackId, clip: Clip) -> Self {
        let clip_id = clip.id;
        AddClip { sequence, track, clip: Some(clip), clip_id }
    }

    pub fn clip_id(&self) -> ClipId {
        self.clip_id
    }
}

impl Command for AddClip {
    fn name(&self) -> &str {
        "Add Clip"
    }

    fn apply(&mut self, project: &mut Project) -> Result<(), CommandError> {
        let clip = self.clip.take().ok_or_else(|| {
            CommandError::Rejected("AddClip applied twice without an undo".into())
        })?;
        let track = track_mut(project, self.sequence, self.track)?;
        // Hand the clip back if the insert is refused, so the command stays
        // re-appliable and the project is untouched.
        if let Err(e) = track.insert_clip(clip.clone()) {
            self.clip = Some(clip);
            return Err(e.into());
        }
        Ok(())
    }

    fn undo(&mut self, project: &mut Project) -> Result<(), CommandError> {
        let track = track_mut(project, self.sequence, self.track)?;
        self.clip = Some(
            track.remove_clip(self.clip_id).ok_or(CommandError::ClipNotFound(self.clip_id))?,
        );
        Ok(())
    }

    fn as_any(&self) -> &dyn Any {
        self
    }
}

/// Deletes a clip, remembering it so undo can put it back exactly as it was.
#[derive(Debug)]
pub struct RemoveClip {
    sequence: SequenceId,
    track: TrackId,
    clip_id: ClipId,
    removed: Option<Clip>,
}

impl RemoveClip {
    pub fn new(sequence: SequenceId, track: TrackId, clip_id: ClipId) -> Self {
        RemoveClip { sequence, track, clip_id, removed: None }
    }
}

impl Command for RemoveClip {
    fn name(&self) -> &str {
        "Delete Clip"
    }

    fn apply(&mut self, project: &mut Project) -> Result<(), CommandError> {
        let track = track_mut(project, self.sequence, self.track)?;
        self.removed = Some(
            track.remove_clip(self.clip_id).ok_or(CommandError::ClipNotFound(self.clip_id))?,
        );
        Ok(())
    }

    fn undo(&mut self, project: &mut Project) -> Result<(), CommandError> {
        let clip = self
            .removed
            .take()
            .ok_or_else(|| CommandError::Rejected("nothing captured to restore".into()))?;
        let track = track_mut(project, self.sequence, self.track)?;
        if let Err(e) = track.insert_clip(clip.clone()) {
            self.removed = Some(clip);
            return Err(e.into());
        }
        Ok(())
    }

    fn as_any(&self) -> &dyn Any {
        self
    }
}

/// Slides a clip along its track.
///
/// Merges with later moves of the same clip, so a drag is a single undo step.
#[derive(Debug)]
pub struct MoveClip {
    sequence: SequenceId,
    track: TrackId,
    clip_id: ClipId,
    to: Ticks,
    /// Captured on the first apply and never overwritten by a merge, so undo
    /// always returns to where the gesture started rather than to the previous
    /// mouse position.
    from: Option<Ticks>,
}

impl MoveClip {
    pub fn new(sequence: SequenceId, track: TrackId, clip_id: ClipId, to: Ticks) -> Self {
        MoveClip { sequence, track, clip_id, to, from: None }
    }
}

impl Command for MoveClip {
    fn name(&self) -> &str {
        "Move Clip"
    }

    fn apply(&mut self, project: &mut Project) -> Result<(), CommandError> {
        let track = track_mut(project, self.sequence, self.track)?;
        let previous = track.move_clip(self.clip_id, self.to)?;
        self.from.get_or_insert(previous);
        Ok(())
    }

    fn undo(&mut self, project: &mut Project) -> Result<(), CommandError> {
        let from =
            self.from.ok_or_else(|| CommandError::Rejected("move was never applied".into()))?;
        let track = track_mut(project, self.sequence, self.track)?;
        track.move_clip(self.clip_id, from)?;
        Ok(())
    }

    fn merge(&mut self, next: &dyn Command) -> bool {
        match next.as_any().downcast_ref::<MoveClip>() {
            Some(other)
                if other.clip_id == self.clip_id
                    && other.track == self.track
                    && other.sequence == self.sequence =>
            {
                // Absorb the new destination, keep the original origin.
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

/// Which end of a clip a trim acts on.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TrimEdge {
    /// Rolls the source in-point so the visible frames stay pinned to the
    /// timeline.
    Start,
    /// Extends or shortens the tail; the source in-point is unchanged.
    End,
}

/// Trims one edge of a clip.
///
/// Undo restores the whole source window, not just the timeline position: see
/// [`ClipWindow`].
#[derive(Debug)]
pub struct TrimClip {
    sequence: SequenceId,
    track: TrackId,
    clip_id: ClipId,
    edge: TrimEdge,
    to: Ticks,
    before: Option<ClipWindow>,
}

impl TrimClip {
    pub fn new(
        sequence: SequenceId,
        track: TrackId,
        clip_id: ClipId,
        edge: TrimEdge,
        to: Ticks,
    ) -> Self {
        TrimClip { sequence, track, clip_id, edge, to, before: None }
    }
}

impl Command for TrimClip {
    fn name(&self) -> &str {
        "Trim Clip"
    }

    fn apply(&mut self, project: &mut Project) -> Result<(), CommandError> {
        // These bounds are read from the project before the mutable borrow.
        let min_duration = project.min_clip_duration(self.sequence);
        let asset_duration = {
            let clip = project
                .sequence(self.sequence)
                .ok_or(CommandError::SequenceNotFound(self.sequence))?
                .track(self.track)
                .ok_or(CommandError::TrackNotFound(self.track))?
                .clip(self.clip_id)
                .ok_or(CommandError::ClipNotFound(self.clip_id))?;
            project.asset_duration(clip.asset)
        };

        let track = track_mut(project, self.sequence, self.track)?;
        let captured = ClipWindow::capture(
            track.clip(self.clip_id).ok_or(CommandError::ClipNotFound(self.clip_id))?,
        );

        match self.edge {
            TrimEdge::Start => track.trim_clip_start(self.clip_id, self.to, min_duration)?,
            TrimEdge::End => {
                track.trim_clip_end(self.clip_id, self.to, min_duration, asset_duration)?
            }
        }

        self.before.get_or_insert(captured);
        Ok(())
    }

    fn undo(&mut self, project: &mut Project) -> Result<(), CommandError> {
        let before = self
            .before
            .ok_or_else(|| CommandError::Rejected("trim was never applied".into()))?;
        let track = track_mut(project, self.sequence, self.track)?;
        let clip =
            track.clip_mut(self.clip_id).ok_or(CommandError::ClipNotFound(self.clip_id))?;
        before.restore(clip);
        debug_assert!(track.invariants_hold());
        Ok(())
    }

    fn merge(&mut self, next: &dyn Command) -> bool {
        match next.as_any().downcast_ref::<TrimClip>() {
            Some(other)
                if other.clip_id == self.clip_id
                    && other.edge == self.edge
                    && other.track == self.track =>
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

/// Cuts a clip in two at a timeline position.
#[derive(Debug)]
pub struct SplitClip {
    sequence: SequenceId,
    track: TrackId,
    clip_id: ClipId,
    at: Ticks,
    /// Allocated on the first apply and reused on redo, so a redone split
    /// produces the same IDs as the original. Anything holding a selection or a
    /// cache key stays valid across undo/redo.
    right_id: Option<ClipId>,
    /// Effect IDs minted for the right-hand clip, reused on redo for the same
    /// reason.
    right_effect_ids: Vec<ve_core::EffectId>,
    left_duration_before: Option<Ticks>,
}

impl SplitClip {
    pub fn new(sequence: SequenceId, track: TrackId, clip_id: ClipId, at: Ticks) -> Self {
        SplitClip {
            sequence,
            track,
            clip_id,
            at,
            right_id: None,
            right_effect_ids: Vec::new(),
            left_duration_before: None,
        }
    }

    /// The right-hand clip's ID, available once the command has been applied.
    pub fn right_id(&self) -> Option<ClipId> {
        self.right_id
    }
}

impl Command for SplitClip {
    fn name(&self) -> &str {
        "Split Clip"
    }

    fn apply(&mut self, project: &mut Project) -> Result<(), CommandError> {
        let effect_count = project
            .sequence(self.sequence)
            .ok_or(CommandError::SequenceNotFound(self.sequence))?
            .track(self.track)
            .ok_or(CommandError::TrackNotFound(self.track))?
            .clip(self.clip_id)
            .ok_or(CommandError::ClipNotFound(self.clip_id))?
            .effects
            .len();

        // Mint IDs on the first apply only; redo reuses them.
        let right_id = match self.right_id {
            Some(id) => id,
            None => {
                let id = project.new_clip_id();
                self.right_id = Some(id);
                id
            }
        };
        if self.right_effect_ids.len() < effect_count {
            for _ in self.right_effect_ids.len()..effect_count {
                let id = project.new_effect_id();
                self.right_effect_ids.push(id);
            }
        }

        let mut minted = self.right_effect_ids.clone().into_iter();
        let track = track_mut(project, self.sequence, self.track)?;
        let before =
            track.clip(self.clip_id).ok_or(CommandError::ClipNotFound(self.clip_id))?.duration;

        track.split_clip(self.clip_id, self.at, right_id, || {
            minted.next().expect("one effect ID was minted per effect")
        })?;

        self.left_duration_before.get_or_insert(before);
        Ok(())
    }

    fn undo(&mut self, project: &mut Project) -> Result<(), CommandError> {
        let right_id = self
            .right_id
            .ok_or_else(|| CommandError::Rejected("split was never applied".into()))?;
        let restore = self
            .left_duration_before
            .ok_or_else(|| CommandError::Rejected("split was never applied".into()))?;

        let track = track_mut(project, self.sequence, self.track)?;
        track.remove_clip(right_id).ok_or(CommandError::ClipNotFound(right_id))?;
        let left =
            track.clip_mut(self.clip_id).ok_or(CommandError::ClipNotFound(self.clip_id))?;
        left.duration = restore;
        debug_assert!(track.invariants_hold());
        Ok(())
    }

    fn as_any(&self) -> &dyn Any {
        self
    }
}
