//! The edits that move more than one thing.
//!
//! Cutting is not only "change this clip". Ripple, roll, slip and slide each
//! change a clip *and* what sits beside it, and each is expected to be a single
//! undo step even though several clips end up different. Two mechanisms cover
//! all of them:
//!
//! * [`Compound`], for edits that are genuinely a sequence of existing
//!   commands — a ripple delete is a delete plus a shift — so they compose
//!   rather than duplicating validated logic.
//! * Dedicated commands, for edits whose halves are only legal *together*.
//!   A roll trims one clip's tail and another's head; performing either alone
//!   would leave an overlap or a gap and be refused, so the pair has to be
//!   validated up front and written as one mutation.

use std::any::Any;

use ve_core::{ClipId, Project, SequenceId, Speed, TrackId};
use ve_time::Ticks;

use crate::{clip_of, track_mut, ClipWindow, Command, CommandError};

/// Several commands as one undo step.
///
/// Applies in order and undoes in reverse, which is what makes nesting safe: an
/// inner command only ever sees the project in the state its own `apply` left
/// behind.
///
/// If any command fails, the ones already applied are rolled back before the
/// error is returned, so the all-or-nothing contract in [`Command`] holds for
/// the group exactly as it does for a single command.
#[derive(Debug)]
pub struct Compound {
    name: String,
    commands: Vec<Box<dyn Command>>,
}

impl Compound {
    pub fn new(name: impl Into<String>) -> Self {
        Compound { name: name.into(), commands: Vec::new() }
    }

    pub fn push(&mut self, command: Box<dyn Command>) {
        self.commands.push(command);
    }

    /// Builder form, for assembling a compound in one expression.
    pub fn with(mut self, command: Box<dyn Command>) -> Self {
        self.push(command);
        self
    }

    pub fn is_empty(&self) -> bool {
        self.commands.is_empty()
    }

    pub fn len(&self) -> usize {
        self.commands.len()
    }
}

impl Command for Compound {
    fn name(&self) -> &str {
        &self.name
    }

    fn apply(&mut self, project: &mut Project) -> Result<(), CommandError> {
        for i in 0..self.commands.len() {
            if let Err(e) = self.commands[i].apply(project) {
                // Unwind what has already landed, newest first. A failure here
                // would mean a command cannot undo what it just did, which is a
                // contract violation rather than a rejected edit, so it is
                // logged and the original cause is still what the caller sees.
                for earlier in self.commands[..i].iter_mut().rev() {
                    if let Err(unwind) = earlier.undo(project) {
                        log::error!(
                            "could not roll back '{}' inside '{}': {unwind}",
                            earlier.name(),
                            self.name
                        );
                    }
                }
                return Err(e);
            }
        }
        Ok(())
    }

    fn undo(&mut self, project: &mut Project) -> Result<(), CommandError> {
        for command in self.commands.iter_mut().rev() {
            command.undo(project)?;
        }
        Ok(())
    }

    fn as_any(&self) -> &dyn Any {
        self
    }
}

/// Slides everything from a point onwards along one track.
///
/// The ripple primitive. Deleting a clip and closing the hole is this after a
/// [`RemoveClip`](crate::RemoveClip); closing a gap is this on its own.
#[derive(Debug)]
pub struct ShiftClips {
    sequence: SequenceId,
    track: TrackId,
    from: Ticks,
    delta: Ticks,
    /// Where the shifted run begins *after* the shift, captured because it is
    /// not always `from + delta`: the run starts at the first clip at or after
    /// `from`, which may sit well past it.
    ///
    /// `None` means nothing moved, so there is nothing to undo.
    undo_from: Option<Ticks>,
}

impl ShiftClips {
    pub fn new(sequence: SequenceId, track: TrackId, from: Ticks, delta: Ticks) -> Self {
        ShiftClips { sequence, track, from, delta, undo_from: None }
    }
}

impl Command for ShiftClips {
    fn name(&self) -> &str {
        if self.delta.is_negative() {
            "Close Gap"
        } else {
            "Ripple Insert"
        }
    }

    fn apply(&mut self, project: &mut Project) -> Result<(), CommandError> {
        let track = track_mut(project, self.sequence, self.track)?;
        let from = self.from;
        let run_start =
            track.clips().iter().find(|c| c.timeline_start >= from).map(|c| c.timeline_start);
        track.shift_clips_from(self.from, self.delta)?;
        self.undo_from = run_start.map(|s| s + self.delta);
        Ok(())
    }

    fn undo(&mut self, project: &mut Project) -> Result<(), CommandError> {
        let Some(anchor) = self.undo_from else { return Ok(()) };
        let track = track_mut(project, self.sequence, self.track)?;
        track.shift_clips_from(anchor, -self.delta)?;
        Ok(())
    }

    fn as_any(&self) -> &dyn Any {
        self
    }
}

/// Moves the shared edge between two adjacent clips.
///
/// A roll changes where the cut falls without changing the sequence's overall
/// length: one clip gives up exactly what the other takes. Both halves are
/// validated before either is written, because trimming one on its own would
/// leave an overlap or a gap and be refused.
#[derive(Debug)]
pub struct RollEdit {
    sequence: SequenceId,
    track: TrackId,
    left: ClipId,
    right: ClipId,
    to: Ticks,
    before: Option<(ClipWindow, ClipWindow)>,
}

impl RollEdit {
    pub fn new(
        sequence: SequenceId,
        track: TrackId,
        left: ClipId,
        right: ClipId,
        to: Ticks,
    ) -> Self {
        RollEdit { sequence, track, left, right, to, before: None }
    }
}

impl Command for RollEdit {
    fn name(&self) -> &str {
        "Roll Edit"
    }

    fn apply(&mut self, project: &mut Project) -> Result<(), CommandError> {
        let min = project.min_clip_duration(self.sequence);
        let left = clip_of(project, self.sequence, self.track, self.left)?;
        let right = clip_of(project, self.sequence, self.track, self.right)?;

        if left.timeline_end() != right.timeline_start {
            return Err(CommandError::Rejected(
                "a roll needs two clips that meet at a cut".into(),
            ));
        }

        let left_duration = self.to - left.timeline_start;
        let right_duration = right.timeline_end() - self.to;
        if left_duration < min || right_duration < min {
            return Err(ve_core::CoreError::TrimTooShort.into());
        }

        // Rolling right needs unused tail on the left clip; rolling left needs
        // unused head on the right one. Either can run out before the minimum
        // duration does.
        let left_source_available = project.asset_duration(left.asset);
        if left.source_in + left.speed.timeline_to_source(left_duration) > left_source_available
        {
            return Err(ve_core::CoreError::TrimBeyondSource.into());
        }
        let right_source_in =
            right.source_in + right.speed.timeline_to_source(self.to - right.timeline_start);
        if right_source_in.is_negative() {
            return Err(ve_core::CoreError::TrimBeyondSource.into());
        }

        let captured = (ClipWindow::capture(left), ClipWindow::capture(right));
        let (to, left_id, right_id) = (self.to, self.left, self.right);

        let track = track_mut(project, self.sequence, self.track)?;
        if track.locked {
            return Err(ve_core::CoreError::TrackLocked.into());
        }
        // The edge stays strictly between the two outer edges, so neither clip
        // can pass the other and the sort order is untouched.
        let left_clip = track.clip_mut(left_id).ok_or(CommandError::ClipNotFound(left_id))?;
        left_clip.duration = left_duration;
        let right_clip =
            track.clip_mut(right_id).ok_or(CommandError::ClipNotFound(right_id))?;
        right_clip.source_in = right_source_in;
        right_clip.timeline_start = to;
        right_clip.duration = right_duration;
        debug_assert!(track.invariants_hold());

        self.before.get_or_insert(captured);
        Ok(())
    }

    fn undo(&mut self, project: &mut Project) -> Result<(), CommandError> {
        let (left, right) = self
            .before
            .ok_or_else(|| CommandError::Rejected("roll was never applied".into()))?;
        let (left_id, right_id) = (self.left, self.right);
        let track = track_mut(project, self.sequence, self.track)?;
        left.restore(track.clip_mut(left_id).ok_or(CommandError::ClipNotFound(left_id))?);
        right.restore(track.clip_mut(right_id).ok_or(CommandError::ClipNotFound(right_id))?);
        debug_assert!(track.invariants_hold());
        Ok(())
    }

    fn merge(&mut self, next: &dyn Command) -> bool {
        match next.as_any().downcast_ref::<RollEdit>() {
            Some(other)
                if other.left == self.left
                    && other.right == self.right
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

/// Changes which frames a clip shows without moving it.
///
/// The clip keeps its position and its length; only the source window slides,
/// so nothing around it has to move. `to_source_in` is absolute rather than a
/// delta, which is what lets a drag merge: each pointer move states where the
/// window should end up, not how far it travelled since the last frame.
#[derive(Debug)]
pub struct SlipClip {
    sequence: SequenceId,
    track: TrackId,
    clip: ClipId,
    to_source_in: Ticks,
    before: Option<Ticks>,
}

impl SlipClip {
    pub fn new(
        sequence: SequenceId,
        track: TrackId,
        clip: ClipId,
        to_source_in: Ticks,
    ) -> Self {
        SlipClip { sequence, track, clip, to_source_in, before: None }
    }
}

impl Command for SlipClip {
    fn name(&self) -> &str {
        "Slip Clip"
    }

    fn apply(&mut self, project: &mut Project) -> Result<(), CommandError> {
        let clip = clip_of(project, self.sequence, self.track, self.clip)?;
        let available = project.asset_duration(clip.asset);
        let source_duration = clip.source_duration();
        if self.to_source_in.is_negative() || self.to_source_in + source_duration > available {
            return Err(ve_core::CoreError::TrimBeyondSource.into());
        }
        let previous = clip.source_in;

        let (clip_id, to) = (self.clip, self.to_source_in);
        let track = track_mut(project, self.sequence, self.track)?;
        if track.locked {
            return Err(ve_core::CoreError::TrackLocked.into());
        }
        track.clip_mut(clip_id).ok_or(CommandError::ClipNotFound(clip_id))?.source_in = to;

        self.before.get_or_insert(previous);
        Ok(())
    }

    fn undo(&mut self, project: &mut Project) -> Result<(), CommandError> {
        let previous = self
            .before
            .ok_or_else(|| CommandError::Rejected("slip was never applied".into()))?;
        let clip_id = self.clip;
        let track = track_mut(project, self.sequence, self.track)?;
        track.clip_mut(clip_id).ok_or(CommandError::ClipNotFound(clip_id))?.source_in =
            previous;
        Ok(())
    }

    fn merge(&mut self, next: &dyn Command) -> bool {
        match next.as_any().downcast_ref::<SlipClip>() {
            Some(other) if other.clip == self.clip && other.track == self.track => {
                self.to_source_in = other.to_source_in;
                true
            }
            _ => false,
        }
    }

    fn as_any(&self) -> &dyn Any {
        self
    }
}

/// Moves a clip along its track, paying for the move out of its neighbours.
///
/// The clip itself is unchanged — same frames, same length — while the clip
/// before it gives up or gains tail and the clip after it gains or gives up
/// head. Any gaps around the clip are carried along rather than closed, so a
/// slide never silently re-times the rest of the track.
#[derive(Debug)]
pub struct SlideClip {
    sequence: SequenceId,
    track: TrackId,
    clip: ClipId,
    to: Ticks,
    before: Option<SlideState>,
}

#[derive(Debug, Clone, Copy)]
struct SlideState {
    left: Option<(ClipId, ClipWindow)>,
    clip: ClipWindow,
    right: Option<(ClipId, ClipWindow)>,
}

impl SlideClip {
    pub fn new(sequence: SequenceId, track: TrackId, clip: ClipId, to: Ticks) -> Self {
        SlideClip { sequence, track, clip, to, before: None }
    }
}

impl Command for SlideClip {
    fn name(&self) -> &str {
        "Slide Clip"
    }

    fn apply(&mut self, project: &mut Project) -> Result<(), CommandError> {
        let min = project.min_clip_duration(self.sequence);
        let seq = project
            .sequence(self.sequence)
            .ok_or(CommandError::SequenceNotFound(self.sequence))?;
        let track_ref = seq.track(self.track).ok_or(CommandError::TrackNotFound(self.track))?;
        if track_ref.locked {
            return Err(ve_core::CoreError::TrackLocked.into());
        }
        let clip = track_ref.clip(self.clip).ok_or(CommandError::ClipNotFound(self.clip))?;
        let delta = self.to - clip.timeline_start;
        let (left, right) = track_ref.neighbours_of(self.clip);

        // Every bound is checked before anything is written, because a slide
        // that half-happened would leave an overlap the track model forbids.
        let left_plan = match left {
            Some(l) => {
                let duration = l.duration + delta;
                if duration < min {
                    return Err(ve_core::CoreError::TrimTooShort.into());
                }
                if l.source_in + l.speed.timeline_to_source(duration)
                    > project.asset_duration(l.asset)
                {
                    return Err(ve_core::CoreError::TrimBeyondSource.into());
                }
                Some((l.id, ClipWindow::capture(l), duration))
            }
            None => {
                // Nothing to the left to absorb the move, so the only limit is
                // the start of the timeline.
                if self.to.is_negative() {
                    return Err(ve_core::CoreError::ClipOverlap.into());
                }
                None
            }
        };

        let right_plan = match right {
            Some(r) => {
                let duration = r.duration - delta;
                if duration < min {
                    return Err(ve_core::CoreError::TrimTooShort.into());
                }
                let source_in = r.source_in + r.speed.timeline_to_source(delta);
                if source_in.is_negative() {
                    return Err(ve_core::CoreError::TrimBeyondSource.into());
                }
                Some((
                    r.id,
                    ClipWindow::capture(r),
                    r.timeline_start + delta,
                    source_in,
                    duration,
                ))
            }
            None => None,
        };

        let captured = SlideState {
            left: left_plan.map(|(id, window, _)| (id, window)),
            clip: ClipWindow::capture(clip),
            right: right_plan.map(|(id, window, _, _, _)| (id, window)),
        };

        let (clip_id, to) = (self.clip, self.to);
        let track = track_mut(project, self.sequence, self.track)?;
        if let Some((id, _, duration)) = left_plan {
            track.clip_mut(id).ok_or(CommandError::ClipNotFound(id))?.duration = duration;
        }
        track.clip_mut(clip_id).ok_or(CommandError::ClipNotFound(clip_id))?.timeline_start = to;
        if let Some((id, _, start, source_in, duration)) = right_plan {
            let r = track.clip_mut(id).ok_or(CommandError::ClipNotFound(id))?;
            r.timeline_start = start;
            r.source_in = source_in;
            r.duration = duration;
        }
        debug_assert!(track.invariants_hold());

        self.before.get_or_insert(captured);
        Ok(())
    }

    fn undo(&mut self, project: &mut Project) -> Result<(), CommandError> {
        let state = self
            .before
            .ok_or_else(|| CommandError::Rejected("slide was never applied".into()))?;
        let clip_id = self.clip;
        let track = track_mut(project, self.sequence, self.track)?;
        for (id, window) in
            [state.left, Some((clip_id, state.clip)), state.right].into_iter().flatten()
        {
            window.restore(track.clip_mut(id).ok_or(CommandError::ClipNotFound(id))?);
        }
        debug_assert!(track.invariants_hold());
        Ok(())
    }

    fn merge(&mut self, next: &dyn Command) -> bool {
        match next.as_any().downcast_ref::<SlideClip>() {
            Some(other) if other.clip == self.clip && other.track == self.track => {
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

/// Changes a clip's playback speed.
///
/// The source window is what stays fixed: the clip keeps showing the same
/// frames, and its *timeline* length changes to hold them at the new rate — a
/// clip at half speed occupies twice the timeline. The alternative, keeping the
/// timeline length and changing which frames are shown, is a trim wearing a
/// disguise, and is not what anyone means by "make this half speed".
///
/// The new length is snapped to the sequence's frame grid, because a clip
/// boundary between two frames is not something the rest of the editor should
/// have to reason about. That rounding can push the source window a hair past
/// the end of the media, so the result is also clamped to what the asset
/// actually has.
#[derive(Debug)]
pub struct SetClipSpeed {
    sequence: SequenceId,
    track: TrackId,
    clip: ClipId,
    speed: Speed,
    /// The speed and window before the first apply. Every apply recomputes from
    /// this rather than from the clip's current state, so a redo lands on
    /// exactly the same length as the original edit instead of compounding the
    /// frame snapping a second time.
    before: Option<(Speed, ClipWindow)>,
}

impl SetClipSpeed {
    pub fn new(sequence: SequenceId, track: TrackId, clip: ClipId, speed: Speed) -> Self {
        SetClipSpeed { sequence, track, clip, speed, before: None }
    }
}

impl Command for SetClipSpeed {
    fn name(&self) -> &str {
        "Set Clip Speed"
    }

    fn apply(&mut self, project: &mut Project) -> Result<(), CommandError> {
        let min = project.min_clip_duration(self.sequence);
        let rate = project
            .sequence(self.sequence)
            .ok_or(CommandError::SequenceNotFound(self.sequence))?
            .rate();
        let clip = clip_of(project, self.sequence, self.track, self.clip)?;
        let available = project.asset_duration(clip.asset);

        let (was_speed, window) =
            self.before.unwrap_or((clip.speed, ClipWindow::capture(clip)));
        let source_duration = was_speed.timeline_to_source(window.duration);

        let mut duration = rate.snap_round(self.speed.source_to_timeline(source_duration));
        // The longest the clip may be before its source window runs off the end
        // of the media, on the frame grid.
        let headroom =
            rate.snap_floor(self.speed.source_to_timeline(available - window.source_in));
        duration = duration.min(headroom);
        if duration < min {
            return Err(ve_core::CoreError::TrimTooShort.into());
        }

        let (clip_id, speed) = (self.clip, self.speed);
        let track = track_mut(project, self.sequence, self.track)?;
        if track.locked {
            return Err(ve_core::CoreError::TrackLocked.into());
        }
        if !track.is_range_free(
            ve_time::TimeRange::new(window.timeline_start, duration),
            Some(clip_id),
        ) {
            return Err(ve_core::CoreError::ClipOverlap.into());
        }
        let clip = track.clip_mut(clip_id).ok_or(CommandError::ClipNotFound(clip_id))?;
        clip.speed = speed;
        clip.duration = duration;
        debug_assert!(track.invariants_hold());

        self.before.get_or_insert((was_speed, window));
        Ok(())
    }

    fn undo(&mut self, project: &mut Project) -> Result<(), CommandError> {
        let (speed, window) =
            self.before.ok_or_else(|| CommandError::Rejected("speed was never set".into()))?;
        let clip_id = self.clip;
        let track = track_mut(project, self.sequence, self.track)?;
        let clip = track.clip_mut(clip_id).ok_or(CommandError::ClipNotFound(clip_id))?;
        clip.speed = speed;
        window.restore(clip);
        debug_assert!(track.invariants_hold());
        Ok(())
    }

    fn merge(&mut self, next: &dyn Command) -> bool {
        match next.as_any().downcast_ref::<SetClipSpeed>() {
            Some(other) if other.clip == self.clip && other.track == self.track => {
                self.speed = other.speed;
                true
            }
            _ => false,
        }
    }

    fn as_any(&self) -> &dyn Any {
        self
    }
}
