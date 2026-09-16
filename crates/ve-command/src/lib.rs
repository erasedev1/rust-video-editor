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

use ve_core::{ClipId, CoreError, Project, SequenceId, TrackId};

mod clip_commands;
mod history;
mod property_commands;
mod structure_commands;

pub use clip_commands::{AddClip, MoveClip, RemoveClip, SplitClip, TrimClip, TrimEdge};
pub use history::{History, HistoryEntry};
pub use property_commands::{
    ClipProperty, PropertyValue, RemoveClipKeyframe, SetClipKeyframe, SetClipProperty,
};
pub use structure_commands::{
    AddMarker, AddTrack, RemoveMarker, RemoveTrack, SetClipEnabled, SetSequenceFormat,
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
    Core(#[from] CoreError),
    #[error("sequence {0} not found")]
    SequenceNotFound(SequenceId),
    #[error("track {0} not found")]
    TrackNotFound(TrackId),
    #[error("clip {0} not found")]
    ClipNotFound(ClipId),
    #[error("nothing to undo")]
    NothingToUndo,
    #[error("nothing to redo")]
    NothingToRedo,
    #[error("{0}")]
    Rejected(String),
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
