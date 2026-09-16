//! One animation system, shared by every property in the editor.
//!
//! There is deliberately no per-property keyframe machinery: transform,
//! opacity, volume and arbitrary effect parameters all use [`Property<T>`], so
//! adding a new animatable parameter means implementing [`Animatable`] and
//! nothing else.

use serde::{Deserialize, Serialize};
use ve_time::Ticks;

use crate::geometry::{Rgba, Vec2};

/// A value that can be interpolated between keyframes.
pub trait Animatable: Copy + PartialEq + std::fmt::Debug {
    /// Interpolates from `a` to `b`, where `t` is the eased progress in `0..=1`.
    fn lerp(a: Self, b: Self, t: f64) -> Self;
}

impl Animatable for f64 {
    #[inline]
    fn lerp(a: f64, b: f64, t: f64) -> f64 {
        a + (b - a) * t
    }
}

impl Animatable for Vec2 {
    #[inline]
    fn lerp(a: Vec2, b: Vec2, t: f64) -> Vec2 {
        Vec2::new(f64::lerp(a.x, b.x, t), f64::lerp(a.y, b.y, t))
    }
}

impl Animatable for Rgba {
    #[inline]
    fn lerp(a: Rgba, b: Rgba, t: f64) -> Rgba {
        Rgba::new(
            f64::lerp(a.r, b.r, t),
            f64::lerp(a.g, b.g, t),
            f64::lerp(a.b, b.b, t),
            f64::lerp(a.a, b.a, t),
        )
    }
}

/// How a keyframe's value approaches the next one.
///
/// Every easing mode is a cubic Bezier on the unit square, evaluated by the
/// same solver. The named variants are the standard control-point presets, so a
/// graph editor can later promote any of them to a free-form `Bezier` without a
/// change of representation.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(tag = "mode", rename_all = "snake_case")]
#[derive(Default)]
pub enum Interpolation {
    /// Holds this keyframe's value until the next one. Used for stepped,
    /// non-interpolated parameters (a blend mode, an enum choice).
    Hold,
    #[default]
    Linear,
    EaseIn,
    EaseOut,
    EaseInOut,
    /// CSS-style `cubic-bezier(x1, y1, x2, y2)`.
    Bezier {
        x1: f64,
        y1: f64,
        x2: f64,
        y2: f64,
    },
}

impl Interpolation {
    /// The control points this mode resolves to, or `None` for [`Hold`].
    ///
    /// [`Hold`]: Interpolation::Hold
    pub fn control_points(self) -> Option<[f64; 4]> {
        match self {
            Interpolation::Hold => None,
            Interpolation::Linear => Some([0.0, 0.0, 1.0, 1.0]),
            Interpolation::EaseIn => Some([0.42, 0.0, 1.0, 1.0]),
            Interpolation::EaseOut => Some([0.0, 0.0, 0.58, 1.0]),
            Interpolation::EaseInOut => Some([0.42, 0.0, 0.58, 1.0]),
            Interpolation::Bezier { x1, y1, x2, y2 } => Some([x1, y1, x2, y2]),
        }
    }

    /// Maps linear progress `t` in `0..=1` to eased progress.
    pub fn ease(self, t: f64) -> f64 {
        let t = t.clamp(0.0, 1.0);
        match self.control_points() {
            None => 0.0, // Hold: never leaves the starting value.
            Some([x1, y1, x2, y2]) => {
                if x1 == y1 && x2 == y2 {
                    return t; // Linear, and the common case: skip the solver.
                }
                cubic_bezier_ease(x1, y1, x2, y2, t)
            }
        }
    }
}

/// Solves `y` for a given `x` on the unit cubic Bezier with control points
/// `(x1, y1)` and `(x2, y2)`, endpoints fixed at `(0,0)` and `(1,1)`.
///
/// Newton-Raphson converges in a few iterations for well-behaved curves; the
/// bisection fallback keeps pathological control points (near-vertical
/// segments, where the derivative vanishes) from diverging.
fn cubic_bezier_ease(x1: f64, y1: f64, x2: f64, y2: f64, x: f64) -> f64 {
    const EPSILON: f64 = 1e-7;
    const NEWTON_ITERATIONS: usize = 8;

    // Bezier basis, expanded into polynomial coefficients.
    let bezier = |c1: f64, c2: f64, t: f64| {
        let a = 1.0 - 3.0 * c2 + 3.0 * c1;
        let b = 3.0 * c2 - 6.0 * c1;
        let c = 3.0 * c1;
        ((a * t + b) * t + c) * t
    };
    let bezier_slope = |c1: f64, c2: f64, t: f64| {
        let a = 1.0 - 3.0 * c2 + 3.0 * c1;
        let b = 3.0 * c2 - 6.0 * c1;
        let c = 3.0 * c1;
        (3.0 * a * t + 2.0 * b) * t + c
    };

    let mut t = x;
    for _ in 0..NEWTON_ITERATIONS {
        let err = bezier(x1, x2, t) - x;
        if err.abs() < EPSILON {
            return bezier(y1, y2, t);
        }
        let slope = bezier_slope(x1, x2, t);
        if slope.abs() < EPSILON {
            break;
        }
        t -= err / slope;
    }

    // Bisection: guaranteed to converge because bezier(x) is monotonic in t for
    // control points in range.
    let (mut lo, mut hi) = (0.0f64, 1.0f64);
    t = x.clamp(0.0, 1.0);
    for _ in 0..32 {
        let cur = bezier(x1, x2, t);
        if (cur - x).abs() < EPSILON {
            break;
        }
        if cur < x {
            lo = t;
        } else {
            hi = t;
        }
        t = (lo + hi) / 2.0;
    }
    bezier(y1, y2, t)
}

/// A value pinned to a point in time.
///
/// `time` is measured **relative to the start of the owning clip**, not to the
/// sequence. That is what lets a clip be moved or rippled along the timeline
/// without touching any of its animation.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct Keyframe<T> {
    pub time: Ticks,
    pub value: T,
    #[serde(default)]
    pub interpolation: Interpolation,
}

impl<T> Keyframe<T> {
    pub fn new(time: Ticks, value: T) -> Self {
        Keyframe { time, value, interpolation: Interpolation::Linear }
    }

    pub fn with_interpolation(mut self, interpolation: Interpolation) -> Self {
        self.interpolation = interpolation;
        self
    }
}

/// An optionally animated value.
///
/// With no keyframes the property is a plain constant, which is both the common
/// case and the cheap one: [`Property::evaluate`] returns immediately without
/// touching the keyframe vector.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Property<T> {
    /// The value used when the property is not animated. Keeping it alive while
    /// keyframes exist means deleting the last keyframe restores a sensible
    /// static value instead of leaving the property undefined.
    pub value: T,
    /// Sorted by `time`, never containing two keyframes at the same time.
    #[serde(default = "Vec::new", skip_serializing_if = "Vec::is_empty")]
    keyframes: Vec<Keyframe<T>>,
}

impl<T: Animatable> Property<T> {
    pub fn constant(value: T) -> Self {
        Property { value, keyframes: Vec::new() }
    }

    #[inline]
    pub fn is_animated(&self) -> bool {
        !self.keyframes.is_empty()
    }

    #[inline]
    pub fn keyframes(&self) -> &[Keyframe<T>] {
        &self.keyframes
    }

    /// Evaluates the property at clip-relative time `t`.
    ///
    /// Outside the keyframed range the value is clamped to the first or last
    /// keyframe; the editor does not extrapolate.
    pub fn evaluate(&self, t: Ticks) -> T {
        let kfs = &self.keyframes;
        match kfs.len() {
            0 => self.value,
            1 => kfs[0].value,
            _ => {
                if t <= kfs[0].time {
                    return kfs[0].value;
                }
                let last = &kfs[kfs.len() - 1];
                if t >= last.time {
                    return last.value;
                }
                // `partition_point` is a binary search: the index of the first
                // keyframe strictly after `t`.
                let idx = kfs.partition_point(|k| k.time <= t);
                let (a, b) = (&kfs[idx - 1], &kfs[idx]);
                let span = (b.time - a.time).raw();
                if span <= 0 {
                    return a.value;
                }
                let progress = (t - a.time).raw() as f64 / span as f64;
                // The *outgoing* keyframe owns the easing into the next one.
                T::lerp(a.value, b.value, a.interpolation.ease(progress))
            }
        }
    }

    /// Adds a keyframe, replacing any existing one at the same time.
    ///
    /// Returns the keyframe that was displaced, so an undo command can restore it.
    pub fn set_keyframe(
        &mut self,
        time: Ticks,
        value: T,
        interpolation: Interpolation,
    ) -> Option<Keyframe<T>> {
        let kf = Keyframe { time, value, interpolation };
        match self.keyframes.binary_search_by_key(&time, |k| k.time) {
            Ok(i) => Some(std::mem::replace(&mut self.keyframes[i], kf)),
            Err(i) => {
                self.keyframes.insert(i, kf);
                None
            }
        }
    }

    pub fn remove_keyframe(&mut self, time: Ticks) -> Option<Keyframe<T>> {
        match self.keyframes.binary_search_by_key(&time, |k| k.time) {
            Ok(i) => Some(self.keyframes.remove(i)),
            Err(_) => None,
        }
    }

    /// Drops all keyframes, freezing the property at its value at `t`.
    pub fn freeze_at(&mut self, t: Ticks) {
        if self.is_animated() {
            self.value = self.evaluate(t);
            self.keyframes.clear();
        }
    }

    /// Restores a full keyframe list. Used by undo, and by the project loader,
    /// which must re-establish the sort invariant on untrusted input.
    pub fn set_keyframes(&mut self, mut kfs: Vec<Keyframe<T>>) {
        kfs.sort_by_key(|k| k.time);
        kfs.dedup_by_key(|k| k.time);
        self.keyframes = kfs;
    }

    /// The span covered by keyframes, if any.
    pub fn keyframe_span(&self) -> Option<(Ticks, Ticks)> {
        match (self.keyframes.first(), self.keyframes.last()) {
            (Some(a), Some(b)) => Some((a.time, b.time)),
            _ => None,
        }
    }
}

impl<T: Animatable + Default> Default for Property<T> {
    fn default() -> Self {
        Property::constant(T::default())
    }
}
