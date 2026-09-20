//! Every edit goes through a [`Command`].
//!
//! Nothing in the editor mutates a [`Project`] directly. The UI builds a
//! command, hands it to a [`History`], and the history applies it and remembers
//! how to take it back. That buys three things at once:
//!
//! * **Undo/redo** for free on every operation, rather than as a retrofit.
//! * **A single audit point** for marking the project dirty and triggering
//!   autosave.
//! * **A scripting surface.** Commands are plain data with a uniform interface,
//!   so a future plugin or macro system drives the editor through exactly the
//!   same path a user does.
//!
//! [`Project`]: ve_core::Project

use std::any::Any;

use ve_core::{Clip, ClipId, CompositionId, CoreError, LayerId, Project, SequenceId, TrackId};
use ve_time::Ticks;

mod audio_commands;
mod clip_commands;
mod composition_commands;
mod edit_commands;
mod history;
mod keyframe_commands;
mod property_commands;
mod structure_commands;

pub use audio_commands::{CrossfadeClips, SetClipFade, SetTrackLevel, TrackLevel};
pub use clip_commands::{AddClip, MoveClip, RemoveClip, SplitClip, TrimClip, TrimEdge};
pub use composition_commands::{
    composition_clip, AddComposition, AddLayer, LayerEdge, LayerFlag, MoveLayer,
    MoveLayerInTime, NestClips, RemoveComposition, RemoveLayer, RenameComposition,
    SetCompositionSettings, SetLayerBlendMode, SetLayerFlag, TrimLayer,
};
pub use edit_commands::{Compound, RollEdit, SetClipSpeed, ShiftClips, SlideClip, SlipClip};
pub use history::{History, HistoryEntry};
pub use keyframe_commands::{
    animatable_properties, property_ref, Animated, EditKeyframes, KeyframeEdit, KeyframePoint,
    PropertyRef, PropertyState,
};
pub use property_commands::{
    ClipProperty, PropertyValue, RemoveClipKeyframe, SetClipKeyframe, SetClipProperty,
    SetLayerProperty,
};
pub use structure_commands::{
    AddMarker, AddTrack, MoveTrack, RemoveMarker, RemoveTrack, SetClipBlendMode,
    SetClipEnabled, SetSequenceColorSpace, SetSequenceFormat, SetTrackFlag, TrackFlag,
};

/// An undoable edit.
///
/// # Contract
///
/// * [`Command::apply`] must capture whatever [`Command::undo`] needs before it
///   mutates anything. A command that cannot restore the previous state has no
///   business being in the history.
/// * Both directions must be **all-or-nothing**. If `apply` returns an error the
///   project must be exactly as it was, because the history does not push
///   failed commands and so will never call `undo` to clean up.
/// * `apply` after `undo` must reproduce the same result, since that is what
///   redo does.
pub trait Command: std::fmt::Debug + Send {
    /// Short label for the undo menu, e.g. "Move Clip".
    fn name(&self) -> &str;

    fn apply(&mut self, project: &mut Project) -> Result<(), CommandError>;

    fn undo(&mut self, project: &mut Project) -> Result<(), CommandError>;

    /// Attempts to absorb a later command into this one.
    ///
    /// This is what keeps a clip drag from filling the undo stack with one
    /// entry per mouse move: the history offers each new command to the one on
    /// top, and a `MoveClip` that targets the same clip simply updates its
    /// destination while keeping its original captured position.
    ///
    /// Returns `false` by default, meaning "push a separate entry".
    fn merge(&mut self, _next: &dyn Command) -> bool {
        false
    }

    /// Enables the downcast that [`Command::merge`] implementations need.
    fn as_any(&self) -> &dyn Any;
}

#[derive(Debug, thiserror::Error)]
pub enum CommandError {
    #[error(transparent)]
    Core(CoreError),
    #[error("sequence {0} not found")]
    SequenceNotFound(SequenceId),
    #[error("track {0} not found")]
    TrackNotFound(TrackId),
    #[error("clip {0} not found")]
    ClipNotFound(ClipId),
    #[error("composition {0} not found")]
    CompositionNotFound(CompositionId),
    #[error("layer {0} not found")]
    LayerNotFound(LayerId),
    #[error("nothing to undo")]
    NothingToUndo,
    #[error("nothing to redo")]
    NothingToRedo,
    #[error("{0}")]
    Rejected(String),
}

/// The part of a clip that every trim-shaped edit has to put back on undo.
///
/// A head trim moves the timeline position *and* rolls the source window;
/// restoring only one of the two would silently change which frames the clip
/// shows. Capturing all three together, and writing them back wholesale rather
/// than replaying an inverse trim, also sidesteps the minimum-duration and
/// source-bound checks on the way back — the state being restored was legal
/// when it was captured.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct ClipWindow {
    source_in: Ticks,
    timeline_start: Ticks,
    duration: Ticks,
}

impl ClipWindow {
    pub(crate) fn capture(clip: &Clip) -> Self {
        ClipWindow {
            source_in: clip.source_in,
            timeline_start: clip.timeline_start,
            duration: clip.duration,
        }
    }

    pub(crate) fn restore(self, clip: &mut Clip) {
        clip.source_in = self.source_in;
        clip.timeline_start = self.timeline_start;
        clip.duration = self.duration;
    }
}

/// Model refusals arrive as [`CoreError`] and are lifted here rather than by a
/// derived `From`, so that "no such clip" has *one* spelling in this crate
/// whether the check happened in a command or inside `ve-core`. Without this,
/// the same failure would reach callers as `ClipNotFound` down one path and
/// `Core(ClipNotFound)` down another, and matching on either would be wrong
/// half the time.
impl From<CoreError> for CommandError {
    fn from(e: CoreError) -> Self {
        match e {
            CoreError::ClipNotFound(id) => CommandError::ClipNotFound(id),
            CoreError::TrackNotFound(id) => CommandError::TrackNotFound(id),
            CoreError::SequenceNotFound(id) => CommandError::SequenceNotFound(id),
            CoreError::CompositionNotFound(id) => CommandError::CompositionNotFound(id),
            CoreError::LayerNotFound(id) => CommandError::LayerNotFound(id),
            other => CommandError::Core(other),
        }
    }
}

/// Resolves a clip for reading, turning any missing link in the chain into a
/// typed error. The read-only half of [`track_mut`]: commands that need to
/// check bounds before mutating go through here first, because the checks need
/// values (asset duration, the minimum clip length) that live on the project
/// and cannot be read while a track is borrowed mutably.
pub(crate) fn clip_of(
    project: &Project,
    sequence: SequenceId,
    track: TrackId,
    clip: ClipId,
) -> Result<&Clip, CommandError> {
    project
        .sequence(sequence)
        .ok_or(CommandError::SequenceNotFound(sequence))?
        .track(track)
        .ok_or(CommandError::TrackNotFound(track))?
        .clip(clip)
        .ok_or(CommandError::ClipNotFound(clip))
}

/// Resolves a clip for mutation without naming its track.
///
/// For the commands that change something about a clip in place — a flag, a
/// blend mode — where which track it sits on is irrelevant. Commands that move
/// clips between tracks go through [`track_mut`] instead, because they need the
/// track itself.
pub(crate) fn clip_mut(
    project: &mut Project,
    sequence: SequenceId,
    clip: ClipId,
) -> Result<&mut Clip, CommandError> {
    project
        .sequence_mut(sequence)
        .ok_or(CommandError::SequenceNotFound(sequence))?
        .find_clip_mut(clip)
        .map(|(_, c)| c)
        .ok_or(CommandError::ClipNotFound(clip))
}

/// Resolves a `(sequence, track)` pair, turning a missing one into a typed
/// error rather than a panic. Used by every command, so it lives here.
pub(crate) fn track_mut(
    project: &mut Project,
    sequence: SequenceId,
    track: TrackId,
) -> Result<&mut ve_core::Track, CommandError> {
    project
        .sequence_mut(sequence)
        .ok_or(CommandError::SequenceNotFound(sequence))?
        .track_mut(track)
        .ok_or(CommandError::TrackNotFound(track))
}
