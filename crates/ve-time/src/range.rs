use std::fmt;

use serde::{Deserialize, Serialize};

use crate::{Rate, Ticks};

/// A half-open interval `[start, start + duration)` on the timeline.
///
/// Half-open is what makes adjacent clips behave: a clip ending at tick `N` and
/// one starting at tick `N` touch but do not overlap, so butt-joined edits never
/// report a spurious collision.
#[derive(Copy, Clone, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
pub struct TimeRange {
    pub start: Ticks,
    pub duration: Ticks,
}

impl TimeRange {
    pub const EMPTY: TimeRange = TimeRange { start: Ticks::ZERO, duration: Ticks::ZERO };

    #[inline]
    pub const fn new(start: Ticks, duration: Ticks) -> Self {
        TimeRange { start, duration }
    }

    /// Builds a range from inclusive start and exclusive end, clamping a
    /// reversed pair to an empty range at `start`.
    pub fn from_bounds(start: Ticks, end: Ticks) -> Self {
        TimeRange { start, duration: (end - start).clamp_non_negative() }
    }

    /// The exclusive end of the range.
    #[inline]
    pub fn end(self) -> Ticks {
        self.start + self.duration
    }

    #[inline]
    pub fn is_empty(self) -> bool {
        self.duration.raw() <= 0
    }

    #[inline]
    pub fn contains(self, t: Ticks) -> bool {
        t >= self.start && t < self.end()
    }

    /// Whether the two ranges share any tick. Touching ranges do not overlap.
    pub fn intersects(self, other: TimeRange) -> bool {
        self.start < other.end() && other.start < self.end() && !self.is_empty() && !other.is_empty()
    }

    pub fn intersection(self, other: TimeRange) -> Option<TimeRange> {
        let start = self.start.max(other.start);
        let end = self.end().min(other.end());
        if end > start {
            Some(TimeRange::from_bounds(start, end))
        } else {
            None
        }
    }

    /// The smallest range covering both inputs.
    pub fn union(self, other: TimeRange) -> TimeRange {
        if self.is_empty() {
            return other;
        }
        if other.is_empty() {
            return self;
        }
        TimeRange::from_bounds(self.start.min(other.start), self.end().max(other.end()))
    }

    /// Moves the range along the timeline without changing its length.
    #[inline]
    pub fn shifted(self, delta: Ticks) -> TimeRange {
        TimeRange { start: self.start + delta, duration: self.duration }
    }

    /// Restricts the range to lie within `bounds`.
    pub fn clamped_to(self, bounds: TimeRange) -> TimeRange {
        self.intersection(bounds).unwrap_or(TimeRange { start: self.start, duration: Ticks::ZERO })
    }

    /// Range in frames `[first, last_exclusive)` at the given rate.
    pub fn frame_bounds(self, rate: Rate) -> (i64, i64) {
        (rate.ticks_to_frame(self.start), rate.ticks_to_frame(self.end()))
    }
}

impl fmt::Debug for TimeRange {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "TimeRange[{} .. {})", self.start, self.end())
    }
}
