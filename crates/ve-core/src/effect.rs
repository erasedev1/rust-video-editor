use serde::{Deserialize, Serialize};
use ve_time::Ticks;

use crate::animation::Property;
use crate::fade::{Fade, FadeEdge};
use crate::geometry::{Rgba, Vec2};
use crate::id::EffectId;

/// A single tunable input to an effect.
///
/// Every animatable variant wraps a [`Property`], so effect parameters get
/// keyframing from the shared animation system rather than from anything
/// effect-specific.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ParamValue {
    Scalar(Property<f64>),
    Point(Property<Vec2>),
    Color(Property<Rgba>),
    /// Not animatable; a stepped parameter uses `Choice` with `Interpolation::Hold`.
    Bool(bool),
    /// Index into an effect-defined list of options.
    Choice(u32),
}

impl ParamValue {
    pub fn scalar(v: f64) -> Self {
        ParamValue::Scalar(Property::constant(v))
    }
    pub fn point(v: Vec2) -> Self {
        ParamValue::Point(Property::constant(v))
    }
    pub fn color(v: Rgba) -> Self {
        ParamValue::Color(Property::constant(v))
    }

    pub fn as_scalar_at(&self, t: Ticks) -> Option<f64> {
        match self {
            ParamValue::Scalar(p) => Some(p.evaluate(t)),
            _ => None,
        }
    }

    pub fn is_animated(&self) -> bool {
        match self {
            ParamValue::Scalar(p) => p.is_animated(),
            ParamValue::Point(p) => p.is_animated(),
            ParamValue::Color(p) => p.is_animated(),
            ParamValue::Bool(_) | ParamValue::Choice(_) => false,
        }
    }
}

/// One node in a clip's effect chain.
///
/// The effect is identified by a string `kind` rather than a Rust enum so that
/// the render graph can dispatch on it dynamically. That is what will let
/// third-party plugins register effects without the core crate knowing about
/// them; parameters are equally open-ended.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Effect {
    pub id: EffectId,
    /// Registry key, e.g. `"verge.blur.gaussian"`.
    pub kind: String,
    /// Display name, defaulting to the registry entry's but user-editable.
    pub name: String,
    #[serde(default = "default_true")]
    pub enabled: bool,
    /// Ordered so the inspector shows parameters in a stable, authored order.
    pub params: Vec<(String, ParamValue)>,
}

fn default_true() -> bool {
    true
}

impl Effect {
    pub fn new(id: EffectId, kind: impl Into<String>, name: impl Into<String>) -> Self {
        Effect { id, kind: kind.into(), name: name.into(), enabled: true, params: Vec::new() }
    }

    pub fn with_param(mut self, key: impl Into<String>, value: ParamValue) -> Self {
        self.params.push((key.into(), value));
        self
    }

    pub fn param(&self, key: &str) -> Option<&ParamValue> {
        self.params.iter().find(|(k, _)| k == key).map(|(_, v)| v)
    }

    pub fn param_mut(&mut self, key: &str) -> Option<&mut ParamValue> {
        self.params.iter_mut().find(|(k, _)| k == key).map(|(_, v)| v)
    }

    /// Whether any parameter changes over time, which tells the render cache
    /// that this effect's output cannot be reused across frames.
    pub fn is_animated(&self) -> bool {
        self.params.iter().any(|(_, v)| v.is_animated())
    }
}

/// The geometric and opacity state applied to every clip, before its effect
/// chain runs.
///
/// Transform is built in rather than being an effect because it maps directly
/// onto the renderer's per-quad vertex transform, so keeping it separate lets
/// the common "no effects" path skip the effect graph entirely.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Transform {
    /// Offset from the composition centre, in composition pixels.
    pub position: Property<Vec2>,
    /// Multiplier, `1.0` being the source's natural size.
    pub scale: Property<Vec2>,
    /// Clockwise degrees.
    pub rotation: Property<f64>,
    /// The point the clip rotates and scales about, in normalised clip space
    /// where `(0.5, 0.5)` is the centre.
    pub anchor: Property<Vec2>,
    /// `0.0..=1.0`.
    pub opacity: Property<f64>,
}

impl Default for Transform {
    fn default() -> Self {
        Transform {
            position: Property::constant(Vec2::ZERO),
            scale: Property::constant(Vec2::ONE),
            rotation: Property::constant(0.0),
            anchor: Property::constant(Vec2::splat(0.5)),
            opacity: Property::constant(1.0),
        }
    }
}

impl Transform {
    /// Resolves every property at clip-relative time `t`.
    pub fn evaluate(&self, t: Ticks) -> TransformState {
        TransformState {
            position: self.position.evaluate(t),
            scale: self.scale.evaluate(t),
            rotation: self.rotation.evaluate(t),
            anchor: self.anchor.evaluate(t),
            opacity: self.opacity.evaluate(t).clamp(0.0, 1.0),
        }
    }

    pub fn is_animated(&self) -> bool {
        self.position.is_animated()
            || self.scale.is_animated()
            || self.rotation.is_animated()
            || self.anchor.is_animated()
            || self.opacity.is_animated()
    }

    /// True when the transform is the identity, letting the renderer take a
    /// fast path that skips the matrix entirely.
    pub fn is_identity(&self) -> bool {
        !self.is_animated() && self.evaluate(Ticks::ZERO).is_identity()
    }
}

/// A [`Transform`] with every property resolved to a concrete value.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct TransformState {
    pub position: Vec2,
    pub scale: Vec2,
    pub rotation: f64,
    pub anchor: Vec2,
    pub opacity: f64,
}

impl TransformState {
    pub fn is_identity(&self) -> bool {
        self.position == Vec2::ZERO
            && self.scale == Vec2::ONE
            && self.rotation == 0.0
            && self.opacity >= 1.0
    }
}

impl Default for TransformState {
    fn default() -> Self {
        TransformState {
            position: Vec2::ZERO,
            scale: Vec2::ONE,
            rotation: 0.0,
            anchor: Vec2::splat(0.5),
            opacity: 1.0,
        }
    }
}

/// Per-clip audio state: a level, a position in the stereo field, and a fade at
/// each end.
///
/// Level and fades are separate and **multiply**. A clip can therefore ride a
/// keyframed level and still fade in and out of it, and changing the level does
/// not disturb the fades — see [`crate::fade`] for why that is worth a field of
/// its own rather than two keyframes.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AudioProperties {
    /// Linear gain, `1.0` being unity. Animatable.
    pub volume: Property<f64>,
    /// `-1.0` hard left to `1.0` hard right.
    pub pan: Property<f64>,
    /// Rises from silence at the clip's start.
    #[serde(default, skip_serializing_if = "Fade::is_inactive")]
    pub fade_in: Fade,
    /// Falls to silence at the clip's end.
    #[serde(default, skip_serializing_if = "Fade::is_inactive")]
    pub fade_out: Fade,
}

impl Default for AudioProperties {
    fn default() -> Self {
        AudioProperties {
            volume: Property::constant(1.0),
            pan: Property::constant(0.0),
            fade_in: Fade::NONE,
            fade_out: Fade::NONE,
        }
    }
}

impl AudioProperties {
    pub fn fade(&self, edge: FadeEdge) -> Fade {
        match edge {
            FadeEdge::In => self.fade_in,
            FadeEdge::Out => self.fade_out,
        }
    }

    pub fn set_fade(&mut self, edge: FadeEdge, fade: Fade) {
        match edge {
            FadeEdge::In => self.fade_in = fade,
            FadeEdge::Out => self.fade_out = fade,
        }
    }

    /// Both fades, shortened proportionally if they would otherwise overlap in
    /// a clip of `duration`.
    ///
    /// This is what every evaluation goes through, so a clip trimmed shorter
    /// than its fades keeps a sensible envelope instead of never reaching full
    /// level — and the stored lengths are left alone, so widening the clip
    /// again restores the fades the user asked for.
    pub fn fitted_fades(&self, duration: Ticks) -> (Fade, Fade) {
        (
            self.fade_in.fitted(duration, self.fade_out.length),
            self.fade_out.fitted(duration, self.fade_in.length),
        )
    }

    /// The combined fade envelope at clip-local time `local`.
    pub fn fade_gain(&self, local: Ticks, duration: Ticks) -> f64 {
        let (fade_in, fade_out) = self.fitted_fades(duration);
        fade_in.gain(FadeEdge::In.distance(local, duration))
            * fade_out.gain(FadeEdge::Out.distance(local, duration))
    }

    /// Level and pan at clip-local time `local`, fades included.
    ///
    /// `duration` is the clip's own length, which the fade at the tail is
    /// measured back from.
    pub fn evaluate(&self, local: Ticks, duration: Ticks) -> (f64, f64) {
        let gain = self.volume.evaluate(local).max(0.0) * self.fade_gain(local, duration);
        (gain, self.pan.evaluate(local).clamp(-1.0, 1.0))
    }

    /// Whether the sound changes over the clip's own length, which is what
    /// tells a cache it cannot reuse one answer for the whole span.
    pub fn is_animated(&self) -> bool {
        self.volume.is_animated()
            || self.pan.is_animated()
            || self.fade_in.is_active()
            || self.fade_out.is_active()
    }
}
