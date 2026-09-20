//! The Verge edit model: projects, sequences, tracks, clips and animation.
//!
//! This crate is deliberately pure: no I/O, no threads, no GPU. Everything here
//! is plain data plus the invariants that keep an edit coherent. Persistence
//! lives in `ve-project`, mutation-with-undo in `ve-command`, and decoding and
//! rendering in `ve-media`, `ve-render` and `ve-engine`.
//!
//! # Non-destructive editing
//!
//! A [`MediaAsset`] references a file on disk and is never written to. A [`Clip`]
//! records *which window* of that asset to show and *where* on the timeline to
//! show it. Trimming narrows the window; it cannot destroy frames, and widening
//! the window again brings them back. Effects and transforms are stored as
//! parameters on the clip, never baked into media.

pub mod animation;
pub mod asset;
pub mod blend;
pub mod clip;
pub mod composition;
pub mod effect;
pub mod fade;
pub mod geometry;
pub mod id;
pub mod motion;
pub mod project;
pub mod sequence;
pub mod source;
pub mod track;

pub use animation::{Animatable, Interpolation, Keyframe, Property};
pub use asset::{AudioStreamInfo, MediaAsset, MediaInfo, VideoStreamInfo};
pub use blend::BlendMode;
pub use clip::{Clip, Speed};
pub use composition::{Composition, CompositionLayer, CompositionSettings};
pub use effect::{AudioProperties, Effect, ParamValue, Transform, TransformState};
pub use fade::{Fade, FadeCurve, FadeEdge};
pub use geometry::{ColorSpace, Rgba, Size, Vec2};
pub use id::{
    AssetId, ClipId, CompositionId, EffectId, Id, IdAllocator, LayerId, MarkerId, SequenceId,
    TrackId,
};
pub use motion::MotionBlur;
pub use project::{Project, ProjectSettings};
pub use sequence::{Marker, Sequence, SequenceSettings};
pub use source::Source;
pub use track::{Track, TrackKind};

pub(crate) fn default_true() -> bool {
    true
}

/// Errors from edit-model operations.
///
/// These are all *rejected edits*, not failures: the model is left untouched,
/// so a command layer can surface the message and move on.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum CoreError {
    #[error("clip {0} not found")]
    ClipNotFound(ClipId),
    #[error("track {0} not found")]
    TrackNotFound(TrackId),
    #[error("sequence {0} not found")]
    SequenceNotFound(SequenceId),
    #[error("asset {0} not found")]
    AssetNotFound(AssetId),
    #[error("composition {0} not found")]
    CompositionNotFound(CompositionId),
    #[error("layer {0} not found")]
    LayerNotFound(LayerId),
    #[error("asset {id} is still used by {clips} clip(s)")]
    AssetInUse { id: AssetId, clips: usize },
    #[error("composition {id} is still used in {places} place(s)")]
    CompositionInUse { id: CompositionId, places: usize },
    #[error("a composition cannot contain itself, even indirectly")]
    CompositionCycle,
    #[error("clips may not overlap on a track")]
    ClipOverlap,
    #[error("track is locked")]
    TrackLocked,
    #[error("a clip must be at least one frame long")]
    TrimTooShort,
    #[error("trim would run past the end of the source media")]
    TrimBeyondSource,
    #[error("split point lies outside the clip")]
    SplitOutsideClip,
    #[error("no clip at that position")]
    NoClipAtTime,
    #[error("a clip must have a positive duration")]
    EmptyClip,
    #[error("playback speed must be a positive ratio")]
    InvalidSpeed,
}
