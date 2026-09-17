//! Fades: the envelope at a clip's own ends.
//!
//! # Why not keyframes
//!
//! A fade could be two keyframes on [`AudioProperties::volume`], and that would
//! reuse the animation system. It is stored separately anyway, for two reasons
//! that matter more than the reuse:
//!
//! * A fade **multiplies** the volume rather than replacing it. Keyframes are
//!   the volume, so adding a fade that way would discard whatever level the clip
//!   was set to, and every later change to that level would have to rewrite the
//!   keyframes to keep the fade's shape.
//! * A fade is anchored to an *end* of the clip, not to a point in time.
//!   Trimming a clip's tail should carry the fade-out with it; a keyframe at a
//!   fixed clip-local time would be left stranded in the middle.
//!
//! Volume automation and fades therefore compose: a clip can ride a keyframed
//! level and still fade in and out of it.
//!
//! # The curves are exact
//!
//! Each shape is evaluated from its closed form — `sin`, `cos` — rather than
//! being approximated by a Bezier ease. That is both cheaper and exact, and
//! equal-power in particular only has its defining property (two complementary
//! fades summing to constant power) if it really is a quarter sine.

use serde::{Deserialize, Serialize};
use ve_time::Ticks;

/// The shape a fade follows from silence to full level.
///
/// Every curve is normalised: `gain(0) == 0` and `gain(1) == 1`. What differs
/// is the path between them, and each one is right for a different job.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FadeCurve {
    /// Gain rises in a straight line. Correct for a fade against silence, and
    /// what a level meter's needle does.
    Linear,
    /// A quarter sine. Two complementary equal-power fades sum to constant
    /// *power* rather than constant amplitude, which is what keeps a transition
    /// between two uncorrelated recordings from dipping in the middle.
    #[default]
    EqualPower,
    /// A raised cosine: flat at both ends, steepest in the middle. The gentlest
    /// of the three at the point where the clip actually starts or stops.
    Smooth,
}

impl FadeCurve {
    pub const ALL: [FadeCurve; 3] =
        [FadeCurve::Linear, FadeCurve::EqualPower, FadeCurve::Smooth];

    pub fn label(self) -> &'static str {
        match self {
            FadeCurve::Linear => "Linear",
            FadeCurve::EqualPower => "Equal power",
            FadeCurve::Smooth => "Smooth",
        }
    }

    pub fn description(self) -> &'static str {
        match self {
            FadeCurve::Linear => "Constant change in amplitude. Best against silence.",
            FadeCurve::EqualPower => {
                "Constant power when two clips cross. The default for transitions."
            }
            FadeCurve::Smooth => "Eased at both ends. The gentlest at the clip's edge.",
        }
    }

    /// Gain at normalised progress `t`, where `0` is silent and `1` is full
    /// level. Progress outside `0..=1` is clamped, so a caller need not.
    pub fn gain(self, t: f64) -> f64 {
        let t = t.clamp(0.0, 1.0);
        match self {
            FadeCurve::Linear => t,
            FadeCurve::EqualPower => (t * std::f64::consts::FRAC_PI_2).sin(),
            FadeCurve::Smooth => 0.5 - 0.5 * (t * std::f64::consts::PI).cos(),
        }
    }
}

/// Which end of a clip a fade belongs to.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FadeEdge {
    In,
    Out,
}

impl FadeEdge {
    pub const ALL: [FadeEdge; 2] = [FadeEdge::In, FadeEdge::Out];

    pub fn label(self) -> &'static str {
        match self {
            FadeEdge::In => "Fade In",
            FadeEdge::Out => "Fade Out",
        }
    }

    /// How far into the fade a clip-local time sits, measured from the silent
    /// end. Negative before the fade starts; past `length` once it is over.
    #[inline]
    pub fn distance(self, local: Ticks, duration: Ticks) -> Ticks {
        match self {
            FadeEdge::In => local,
            FadeEdge::Out => duration - local,
        }
    }
}

/// A fade at one end of a clip: how long it lasts and what shape it follows.
///
/// A zero length means no fade, which is why [`Fade::NONE`] is also the default
/// and why a project written before fades existed loads with none rather than
/// with something arbitrary.
#[derive(Debug, Clone, Copy, PartialEq, Default, Serialize, Deserialize)]
pub struct Fade {
    #[serde(default)]
    pub length: Ticks,
    #[serde(default)]
    pub curve: FadeCurve,
}

impl Fade {
    pub const NONE: Fade = Fade { length: Ticks::ZERO, curve: FadeCurve::EqualPower };

    pub fn new(length: Ticks, curve: FadeCurve) -> Self {
        Fade { length: length.clamp_non_negative(), curve }
    }

    #[inline]
    pub fn is_active(&self) -> bool {
        self.length.raw() > 0
    }

    /// The negation, spelled as a free-standing predicate so serde can skip
    /// writing the overwhelmingly common "no fade" out to every clip.
    #[inline]
    pub fn is_inactive(&self) -> bool {
        !self.is_active()
    }

    /// Gain `distance` into the fade, measured from its silent end.
    ///
    /// An inactive fade is transparent — gain `1.0` everywhere — so this can be
    /// multiplied in unconditionally rather than being branched around.
    pub fn gain(&self, distance: Ticks) -> f64 {
        if !self.is_active() {
            return 1.0;
        }
        if distance.raw() <= 0 {
            return 0.0;
        }
        if distance >= self.length {
            return 1.0;
        }
        self.curve.gain(distance.raw() as f64 / self.length.raw() as f64)
    }

    /// The fade, shortened so it fits inside a clip of `duration` alongside a
    /// fade of `other` ticks at the far end.
    ///
    /// Trimming a clip shorter than its fades has to do *something*; overlapping
    /// them would make a clip that never reaches full level, which is not what
    /// either fade was asked for. They are scaled back proportionally instead,
    /// so their ratio survives and the clip still reaches unity somewhere.
    pub fn fitted(self, duration: Ticks, other: Ticks) -> Fade {
        let total = self.length + other;
        if duration.raw() <= 0 || total.raw() <= duration.raw() {
            return self;
        }
        let scaled = self.length.raw() as i128 * duration.raw() as i128 / total.raw() as i128;
        Fade { length: Ticks::new(scaled as i64), curve: self.curve }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn secs(v: f64) -> Ticks {
        Ticks::from_secs_f64(v)
    }

    #[test]
    fn every_curve_runs_from_silence_to_unity() {
        for curve in FadeCurve::ALL {
            assert_eq!(curve.gain(0.0), 0.0, "{curve:?} must start silent");
            assert!(
                (curve.gain(1.0) - 1.0).abs() < 1e-12,
                "{curve:?} must reach unity, got {}",
                curve.gain(1.0)
            );
        }
    }

    #[test]
    fn every_curve_is_monotonic_and_in_range() {
        for curve in FadeCurve::ALL {
            let mut previous = -1.0;
            for i in 0..=1000 {
                let g = curve.gain(i as f64 / 1000.0);
                assert!((0.0..=1.0).contains(&g), "{curve:?} left the unit range at {i}");
                assert!(g >= previous, "{curve:?} went backwards at {i}");
                previous = g;
            }
        }
    }

    #[test]
    fn progress_outside_the_unit_range_is_clamped() {
        for curve in FadeCurve::ALL {
            assert_eq!(curve.gain(-3.0), 0.0);
            assert!((curve.gain(4.0) - 1.0).abs() < 1e-12);
        }
    }

    /// The property the curve exists for: a rising and a falling equal-power
    /// fade, crossing, keep the *power* constant rather than the amplitude.
    #[test]
    fn two_equal_power_fades_crossing_hold_their_power() {
        for i in 0..=1000 {
            let t = i as f64 / 1000.0;
            let rising = FadeCurve::EqualPower.gain(t);
            let falling = FadeCurve::EqualPower.gain(1.0 - t);
            let power = rising * rising + falling * falling;
            assert!((power - 1.0).abs() < 1e-12, "power was {power} at t={t}");
        }
    }

    /// And the reason linear is not the default for a transition: crossing
    /// linear fades lose 3 dB in the middle.
    #[test]
    fn crossing_linear_fades_dip_which_is_why_they_are_not_the_default() {
        let power = 0.5f64 * 0.5 + 0.5 * 0.5;
        let dip_db = 10.0 * power.log10();
        assert!((dip_db + 3.0103).abs() < 0.001, "expected about -3 dB, got {dip_db}");
    }

    #[test]
    fn an_inactive_fade_is_transparent() {
        let fade = Fade::NONE;
        assert!(!fade.is_active());
        assert_eq!(fade.gain(Ticks::ZERO), 1.0);
        assert_eq!(fade.gain(secs(-1.0)), 1.0);
        assert_eq!(fade.gain(secs(10.0)), 1.0);
    }

    #[test]
    fn a_fade_is_silent_at_its_edge_and_unity_past_its_length() {
        let fade = Fade::new(secs(2.0), FadeCurve::Linear);
        assert_eq!(fade.gain(Ticks::ZERO), 0.0);
        assert_eq!(fade.gain(secs(-1.0)), 0.0);
        assert!((fade.gain(secs(1.0)) - 0.5).abs() < 1e-9);
        assert_eq!(fade.gain(secs(2.0)), 1.0);
        assert_eq!(fade.gain(secs(5.0)), 1.0);
    }

    #[test]
    fn a_negative_length_is_no_fade_rather_than_an_inverted_one() {
        let fade = Fade::new(secs(-3.0), FadeCurve::Linear);
        assert!(!fade.is_active());
        assert_eq!(fade.gain(secs(1.0)), 1.0);
    }

    #[test]
    fn the_out_edge_measures_backwards_from_the_clip_end() {
        let duration = secs(10.0);
        assert_eq!(FadeEdge::In.distance(secs(1.0), duration), secs(1.0));
        assert_eq!(FadeEdge::Out.distance(secs(9.0), duration), secs(1.0));
        assert_eq!(FadeEdge::Out.distance(duration, duration), Ticks::ZERO);
    }

    #[test]
    fn fades_that_would_overlap_are_scaled_back_to_fit() {
        // Six seconds of fade asked for in a four second clip.
        let fade_in = Fade::new(secs(2.0), FadeCurve::Linear);
        let fade_out = Fade::new(secs(4.0), FadeCurve::Linear);
        let duration = secs(4.0);

        let fitted_in = fade_in.fitted(duration, fade_out.length);
        let fitted_out = fade_out.fitted(duration, fade_in.length);

        assert_eq!(fitted_in.length, secs(4.0 / 3.0));
        assert_eq!(fitted_out.length, secs(8.0 / 3.0));
        assert!(
            fitted_in.length + fitted_out.length <= duration,
            "the two fades must fit inside the clip"
        );
    }

    #[test]
    fn fades_that_already_fit_are_left_exactly_alone() {
        let fade = Fade::new(secs(1.0), FadeCurve::Smooth);
        assert_eq!(fade.fitted(secs(10.0), secs(1.0)), fade);
        // Meeting exactly in the middle still fits.
        assert_eq!(fade.fitted(secs(2.0), secs(1.0)), fade);
    }
}
