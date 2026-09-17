//! How a layer combines with what is beneath it.

use serde::{Deserialize, Serialize};

/// A clip's blend mode.
///
/// # Why this list and not a longer one
///
/// These four are exactly the modes a graphics pipeline's fixed-function blender
/// can evaluate, given a premultiplied source: each is a weighted sum of the
/// source and the destination, and the weights are ones the hardware already
/// offers. They therefore cost a pipeline variant and nothing else — no extra
/// pass, no copy of the frame being composited into, no bandwidth.
///
/// The rest of the familiar set — overlay, soft light, hard light, colour dodge,
/// difference, hue, saturation — cannot be written that way, because the result
/// depends on the destination through something other than a multiply. Those
/// need the destination as a *texture*, which means compositing into one target
/// and reading from a copy of it, so they belong with the work that introduces
/// intermediate targets (nested compositions) rather than being bolted on here.
/// Two modes that look like they would fit, darken and lighten, are the same
/// story: `min` and `max` ignore the blend factors, so they are only correct for
/// a fully opaque layer, and a mode that quietly misbehaves at 50% opacity is
/// worse than a mode that is not there yet.
///
/// # Alpha
///
/// Every mode composites alpha the same way — the union of the source's and the
/// destination's coverage — because a blend mode describes how *colour*
/// combines, not how much of the frame the layer covers. Only the colour
/// weighting changes between them.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BlendMode {
    /// Source over destination: the layer covers what is beneath it, in
    /// proportion to its alpha.
    #[default]
    Normal,
    /// Sum of the two, which brightens. Light, fire and glows are additive in
    /// reality, which is why this is the mode composites of them use.
    Add,
    /// Product of the two, which darkens. Multiplying by white leaves the
    /// backdrop alone, so it is how shadows and stains are laid over a plate.
    Multiply,
    /// The inverse of multiplying the inverses, which brightens without
    /// clipping the way adding does.
    Screen,
}

impl BlendMode {
    /// Every mode, in the order the interface lists them.
    pub const ALL: [BlendMode; 4] =
        [BlendMode::Normal, BlendMode::Add, BlendMode::Multiply, BlendMode::Screen];

    pub fn label(self) -> &'static str {
        match self {
            BlendMode::Normal => "Normal",
            BlendMode::Add => "Add",
            BlendMode::Multiply => "Multiply",
            BlendMode::Screen => "Screen",
        }
    }

    /// Whether this is the default, which the timeline leaves unannotated and
    /// the inspector shows as the resting state.
    pub fn is_normal(self) -> bool {
        self == BlendMode::Normal
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_default_is_normal() {
        assert_eq!(BlendMode::default(), BlendMode::Normal);
        assert!(BlendMode::default().is_normal());
    }

    #[test]
    fn every_mode_is_listed_exactly_once_with_a_label() {
        let mut labels: Vec<&str> = BlendMode::ALL.iter().map(|m| m.label()).collect();
        labels.sort_unstable();
        let count = labels.len();
        labels.dedup();
        assert_eq!(labels.len(), count, "two modes share a label");
    }

    #[test]
    fn modes_round_trip_through_serde_by_name() {
        // Names rather than indices, so inserting a mode later cannot change
        // what an existing project file means.
        let json = serde_json::to_string(&BlendMode::Screen).unwrap();
        assert_eq!(json, "\"screen\"");
        assert_eq!(serde_json::from_str::<BlendMode>(&json).unwrap(), BlendMode::Screen);
    }
}
