//! What a clip or a layer shows.

use serde::{Deserialize, Serialize};

use crate::id::{AssetId, CompositionId};

/// The thing a clip or a composition layer draws from.
///
/// One enum for both, because "a window onto something, placed in time" is the
/// same idea whether the something is a file on disk or a composition rendered
/// on the spot. Keeping it single means every consumer — decode scheduling,
/// rendering, the inspector — handles both cases in one place rather than one
/// path per container.
///
/// Serialised as an externally tagged enum, `{"asset": 3}` or
/// `{"composition": 7}`, so a file says which kind it means rather than relying
/// on an ID space that a future entity could join.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Source {
    /// Media on disk, decoded by `ve-media`.
    Asset(AssetId),
    /// Another composition, rendered into a texture and then treated exactly
    /// like a decoded frame.
    Composition(CompositionId),
}

impl Source {
    /// The asset this draws from, or `None` for a nested composition.
    ///
    /// Most call sites want this rather than a match: decoding, relinking and
    /// the media browser are all about assets specifically, and a nested
    /// composition is simply not their business.
    pub fn asset(self) -> Option<AssetId> {
        match self {
            Source::Asset(id) => Some(id),
            Source::Composition(_) => None,
        }
    }

    pub fn composition(self) -> Option<CompositionId> {
        match self {
            Source::Composition(id) => Some(id),
            Source::Asset(_) => None,
        }
    }

    pub fn is_asset(self) -> bool {
        matches!(self, Source::Asset(_))
    }

    pub fn is_composition(self) -> bool {
        matches!(self, Source::Composition(_))
    }
}

impl From<AssetId> for Source {
    fn from(id: AssetId) -> Self {
        Source::Asset(id)
    }
}

impl From<CompositionId> for Source {
    fn from(id: CompositionId) -> Self {
        Source::Composition(id)
    }
}

impl std::fmt::Display for Source {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Source::Asset(id) => write!(f, "asset {id}"),
            Source::Composition(id) => write!(f, "composition {id}"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_asset_and_a_composition_with_the_same_number_are_different_sources() {
        // IDs come from one allocator, so this cannot happen in a project built
        // through the model — but it can in a hand-edited file, and the two must
        // not be confused when it does.
        let asset = Source::Asset(AssetId::from_raw(4));
        let comp = Source::Composition(CompositionId::from_raw(4));
        assert_ne!(asset, comp);
        assert_eq!(asset.asset().map(|i| i.raw()), Some(4));
        assert_eq!(asset.composition(), None);
        assert_eq!(comp.composition().map(|i| i.raw()), Some(4));
        assert_eq!(comp.asset(), None);
    }

    #[test]
    fn sources_are_tagged_by_kind_on_disk() {
        let json = serde_json::to_string(&Source::Asset(AssetId::from_raw(3))).unwrap();
        assert_eq!(json, r#"{"asset":3}"#);
        let json =
            serde_json::to_string(&Source::Composition(CompositionId::from_raw(7))).unwrap();
        assert_eq!(json, r#"{"composition":7}"#);

        let back: Source = serde_json::from_str(r#"{"composition":7}"#).unwrap();
        assert_eq!(back, Source::Composition(CompositionId::from_raw(7)));
    }
}
