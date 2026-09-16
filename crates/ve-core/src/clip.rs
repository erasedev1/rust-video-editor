use serde::{Deserialize, Serialize};
use ve_time::{Ticks, TimeRange};

use crate::effect::{AudioProperties, Effect, Transform};
use crate::id::{AssetId, ClipId, EffectId};
use crate::CoreError;

/// Playback rate as an exact rational multiplier of the source.
///
/// Rational rather than float so that a 2x clip consumes exactly twice its
/// timeline duration in source ticks, with no rounding to accumulate.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct Speed {
    num: u32,
    den: u32,
}

impl Speed {
    pub const NORMAL: Speed = Speed { num: 1, den: 1 };

    pub fn new(num: u32, den: u32) -> Result<Speed, CoreError> {
        if num == 0 || den == 0 {
            return Err(CoreError::InvalidSpeed);
        }
        let g = gcd(num, den);
        Ok(Speed { num: num / g, den: den / g })
    }

    /// Builds the nearest exact rational to a float, for UI input like "1.5x".
    /// Denominators are limited to 1000, which covers every speed a user can
    /// meaningfully dial in.
    pub fn from_f64(v: f64) -> Result<Speed, CoreError> {
        if !v.is_finite() || v <= 0.0 {
            return Err(CoreError::InvalidSpeed);
        }
        let den = 1000u32;
        let num = (v * den as f64).round() as u32;
        Speed::new(num, den)
    }

    #[inline]
    pub const fn numerator(self) -> u32 {
        self.num
    }
    #[inline]
    pub const fn denominator(self) -> u32 {
        self.den
    }
    #[inline]
    pub fn as_f64(self) -> f64 {
        self.num as f64 / self.den as f64
    }
    #[inline]
    pub fn is_normal(self) -> bool {
        self.num == self.den
    }

    /// Source ticks consumed by `timeline` ticks of playback.
    #[inline]
    pub fn timeline_to_source(self, timeline: Ticks) -> Ticks {
        timeline.scale(self.num as i64, self.den as i64)
    }

    /// Timeline ticks produced by `source` ticks of media.
    #[inline]
    pub fn source_to_timeline(self, source: Ticks) -> Ticks {
        source.scale(self.den as i64, self.num as i64)
    }
}

impl Default for Speed {
    fn default() -> Self {
        Speed::NORMAL
    }
}

const fn gcd(mut a: u32, mut b: u32) -> u32 {
    while b != 0 {
        let t = b;
        b = a % b;
        a = t;
    }
    a
}

/// An edit decision: a window onto a [`crate::MediaAsset`], placed on a track.
///
/// A clip stores *where to look* in the source and *where to put it* on the
/// timeline. It never stores media. Trimming a clip narrows this window; the
/// source file is untouched, and widening the window again recovers the frames.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Clip {
    pub id: ClipId,
    pub name: String,
    pub asset: AssetId,

    /// Offset into the source media of the clip's first frame.
    pub source_in: Ticks,
    /// Where the clip starts on the sequence timeline.
    pub timeline_start: Ticks,
    /// How long the clip occupies the timeline. Source length consumed is this
    /// scaled by [`Self::speed`].
    pub duration: Ticks,

    #[serde(default)]
    pub speed: Speed,
    #[serde(default = "crate::default_true")]
    pub enabled: bool,

    #[serde(default)]
    pub transform: Transform,
    #[serde(default)]
    pub audio: AudioProperties,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub effects: Vec<Effect>,
}

impl Clip {
    pub fn new(
        id: ClipId,
        asset: AssetId,
        name: impl Into<String>,
        source_in: Ticks,
        timeline_start: Ticks,
        duration: Ticks,
    ) -> Self {
        Clip {
            id,
            name: name.into(),
            asset,
            source_in,
            timeline_start,
            duration,
            speed: Speed::NORMAL,
            enabled: true,
            transform: Transform::default(),
            audio: AudioProperties::default(),
            effects: Vec::new(),
        }
    }

    /// The clip's footprint on the timeline.
    #[inline]
    pub fn range(&self) -> TimeRange {
        TimeRange::new(self.timeline_start, self.duration)
    }

    #[inline]
    pub fn timeline_end(&self) -> Ticks {
        self.timeline_start + self.duration
    }

    /// Source ticks consumed, accounting for speed.
    #[inline]
    pub fn source_duration(&self) -> Ticks {
        self.speed.timeline_to_source(self.duration)
    }

    /// Exclusive end of the source window.
    #[inline]
    pub fn source_out(&self) -> Ticks {
        self.source_in + self.source_duration()
    }

    /// The source position that a timeline position maps to.
    ///
    /// Returns `None` when the timeline position lies outside the clip, which
    /// is what the compositor uses to decide whether a clip contributes to a
    /// given frame.
    pub fn source_time_at(&self, timeline: Ticks) -> Option<Ticks> {
        if !self.range().contains(timeline) {
            return None;
        }
        Some(self.source_in + self.speed.timeline_to_source(timeline - self.timeline_start))
    }

    /// Clip-relative time, which is the domain every [`crate::Property`] on the
    /// clip is keyed on.
    #[inline]
    pub fn local_time_at(&self, timeline: Ticks) -> Ticks {
        timeline - self.timeline_start
    }

    /// Moves the clip without changing which frames it shows.
    pub fn move_to(&mut self, timeline_start: Ticks) {
        self.timeline_start = timeline_start.clamp_non_negative();
    }

    /// Trims the head to a new timeline start, rolling the source window to
    /// match so the visible frames stay pinned to the timeline.
    ///
    /// `available_source` is the asset's total duration; the trim is clamped so
    /// the window cannot run off the front of the media.
    pub fn trim_start(
        &mut self,
        new_start: Ticks,
        min_duration: Ticks,
    ) -> Result<(), CoreError> {
        let delta = new_start - self.timeline_start;
        let new_duration = self.duration - delta;
        if new_duration < min_duration {
            return Err(CoreError::TrimTooShort);
        }
        let source_delta = self.speed.timeline_to_source(delta);
        let new_source_in = self.source_in + source_delta;
        if new_source_in.is_negative() {
            return Err(CoreError::TrimBeyondSource);
        }
        self.timeline_start = new_start;
        self.source_in = new_source_in;
        self.duration = new_duration;
        Ok(())
    }

    /// Trims the tail to a new timeline end. The source in-point is unaffected.
    pub fn trim_end(
        &mut self,
        new_end: Ticks,
        min_duration: Ticks,
        available_source: Ticks,
    ) -> Result<(), CoreError> {
        let new_duration = new_end - self.timeline_start;
        if new_duration < min_duration {
            return Err(CoreError::TrimTooShort);
        }
        if self.source_in + self.speed.timeline_to_source(new_duration) > available_source {
            return Err(CoreError::TrimBeyondSource);
        }
        self.duration = new_duration;
        Ok(())
    }

    /// Splits at a timeline position, shortening `self` to the left-hand part
    /// and returning the right-hand part.
    ///
    /// Both halves keep the same source window semantics, so a split followed
    /// by deleting one half is exactly a trim — no frames are lost or
    /// duplicated. The new clip needs a fresh [`ClipId`] and fresh
    /// [`EffectId`]s, which the caller supplies.
    pub fn split_at(
        &mut self,
        at: Ticks,
        new_clip_id: ClipId,
        mut new_effect_id: impl FnMut() -> EffectId,
    ) -> Result<Clip, CoreError> {
        if at <= self.timeline_start || at >= self.timeline_end() {
            return Err(CoreError::SplitOutsideClip);
        }

        let left_duration = at - self.timeline_start;
        let mut right = self.clone();

        right.id = new_clip_id;
        right.timeline_start = at;
        right.duration = self.duration - left_duration;
        right.source_in = self.source_in + self.speed.timeline_to_source(left_duration);
        for effect in &mut right.effects {
            effect.id = new_effect_id();
        }
        // Keyframe times are clip-relative, so the right-hand clip's animation
        // has to shift back by the length of the left-hand part to keep
        // pointing at the same moments.
        right.rebase_keyframes(-left_duration);

        self.duration = left_duration;
        Ok(right)
    }

    /// Shifts every keyframe on the clip by `delta` in clip-local time.
    fn rebase_keyframes(&mut self, delta: Ticks) {
        use crate::effect::ParamValue;

        macro_rules! shift {
            ($prop:expr) => {{
                if $prop.is_animated() {
                    let kfs = $prop
                        .keyframes()
                        .iter()
                        .map(|k| {
                            let mut k = *k;
                            k.time += delta;
                            k
                        })
                        .collect();
                    $prop.set_keyframes(kfs);
                }
            }};
        }

        shift!(self.transform.position);
        shift!(self.transform.scale);
        shift!(self.transform.rotation);
        shift!(self.transform.anchor);
        shift!(self.transform.opacity);
        shift!(self.audio.volume);
        shift!(self.audio.pan);

        for effect in &mut self.effects {
            for (_, param) in &mut effect.params {
                match param {
                    ParamValue::Scalar(p) => shift!(*p),
                    ParamValue::Point(p) => shift!(*p),
                    ParamValue::Color(p) => shift!(*p),
                    ParamValue::Bool(_) | ParamValue::Choice(_) => {}
                }
            }
        }
    }

    /// Whether anything about this clip changes over its own duration. Static
    /// clips can have a single composited result cached for their whole span.
    pub fn is_animated(&self) -> bool {
        self.transform.is_animated()
            || self.audio.volume.is_animated()
            || self.audio.pan.is_animated()
            || self.effects.iter().any(|e| e.enabled && e.is_animated())
    }
}
