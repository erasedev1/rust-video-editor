use std::fmt;
use std::hash::{Hash, Hasher};
use std::marker::PhantomData;

use serde::{Deserialize, Serialize};

/// A stable, typed handle to an object inside a [`crate::Project`].
///
/// IDs are drawn from a single monotonic counter stored on the project, which
/// gives three properties the editor depends on:
///
/// * **Stability across save/load** — an ID written to the project file still
///   refers to the same object when it is read back.
/// * **Determinism** — the same sequence of edits produces the same IDs, so
///   tests and undo/redo are reproducible.
/// * **No reuse** — deleting a clip and adding another never recycles the ID, so
///   a stale reference resolves to "missing" rather than to the wrong object.
///
/// The phantom type parameter keeps a `ClipId` from being passed where a
/// `TrackId` is expected, at zero runtime cost.
#[derive(Serialize, Deserialize)]
#[serde(transparent)]
pub struct Id<T> {
    raw: u64,
    #[serde(skip)]
    _marker: PhantomData<fn() -> T>,
}

impl<T> Id<T> {
    #[inline]
    pub const fn from_raw(raw: u64) -> Self {
        Id { raw, _marker: PhantomData }
    }

    #[inline]
    pub const fn raw(self) -> u64 {
        self.raw
    }
}

// Derived impls would demand `T: Clone` and friends, which the tag types do not
// need to satisfy, so these are written out.
impl<T> Clone for Id<T> {
    fn clone(&self) -> Self {
        *self
    }
}
impl<T> Copy for Id<T> {}
impl<T> PartialEq for Id<T> {
    fn eq(&self, other: &Self) -> bool {
        self.raw == other.raw
    }
}
impl<T> Eq for Id<T> {}
impl<T> PartialOrd for Id<T> {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}
impl<T> Ord for Id<T> {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        self.raw.cmp(&other.raw)
    }
}
impl<T> Hash for Id<T> {
    fn hash<H: Hasher>(&self, state: &mut H) {
        self.raw.hash(state);
    }
}
impl<T> fmt::Debug for Id<T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "#{}", self.raw)
    }
}
impl<T> fmt::Display for Id<T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "#{}", self.raw)
    }
}

/// Hands out fresh [`Id`]s. Persisted with the project so IDs stay unique
/// across sessions.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct IdAllocator {
    next: u64,
}

impl IdAllocator {
    pub const fn new() -> Self {
        // Zero is reserved so that a defaulted ID is recognisably invalid.
        IdAllocator { next: 1 }
    }

    pub fn alloc<T>(&mut self) -> Id<T> {
        let id = Id::from_raw(self.next);
        self.next += 1;
        id
    }

    /// Ensures future allocations sit above `raw`. Used when loading a project
    /// so that hand-edited or migrated files cannot collide with new objects.
    pub fn bump_past(&mut self, raw: u64) {
        self.next = self.next.max(raw + 1);
    }

    pub fn peek(&self) -> u64 {
        self.next
    }
}

impl Default for IdAllocator {
    fn default() -> Self {
        IdAllocator::new()
    }
}

macro_rules! id_types {
    ($($tag:ident => $alias:ident),* $(,)?) => {
        $(
            /// Type tag; only ever used as a phantom parameter.
            #[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
            pub enum $tag {}
            pub type $alias = Id<$tag>;
        )*
    };
}

id_types! {
    AssetTag => AssetId,
    SequenceTag => SequenceId,
    CompositionTag => CompositionId,
    TrackTag => TrackId,
    ClipTag => ClipId,
    LayerTag => LayerId,
    MarkerTag => MarkerId,
    EffectTag => EffectId,
    MulticamTag => MulticamId,
    AngleTag => AngleId,
    CaptionTrackTag => CaptionTrackId,
    CueTag => CueId,
}
