//! Project interchange: how an edit leaves Verge for another tool, and arrives.
//!
//! An edit decision list is the oldest and the most widely read of the
//! interchange formats, so it comes first: nearly every grading, conform and
//! finishing tool still reads a CMX3600 list, and its limits are an honest
//! statement of what "just the cuts" means.
//!
//! Like `ve-caption`, this crate is the only place that knows what the files
//! look like. [`ve_core`] holds the edit, `ve-command` changes it, and neither
//! contains a timecode column parser. And like `ve-caption`, reading never
//! allocates IDs or touches a project: a parse produces events, and
//! [`edl::Edl::place`] turns them into clips *described* by plain values that
//! the caller numbers and inserts through its own undoable commands.
//!
//! # What is not carried
//!
//! Cuts, and constant speed changes. A dissolve or wipe is read, reported and
//! placed as a cut, since the model has no transitions to put it in. Nothing
//! that is not a clip on a track — effects, keyframes, titles — has an EDL
//! spelling, and nothing is invented for it.

pub mod edl;
pub mod otio;

pub use edl::{Edl, EdlError, EdlEvent, PlacedClip, Placement, Resolved};
pub use otio::{OtioClip, OtioError};
