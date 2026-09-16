use std::fmt;

use serde::{Deserialize, Serialize};

use crate::ticks::{div_floor, div_round, Ticks, TICKS_PER_SECOND};
use crate::TimeError;

/// An exact rational frame rate, in frames per second.
///
/// Stored reduced, so `Rate::new(60000, 1000)` and `Rate::new(60, 1)` compare
/// equal and hash identically.
#[derive(Copy, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct Rate {
    num: u32,
    den: u32,
}

impl Rate {
    pub const FPS_23_976: Rate = Rate { num: 24000, den: 1001 };
    pub const FPS_24: Rate = Rate { num: 24, den: 1 };
    pub const FPS_25: Rate = Rate { num: 25, den: 1 };
    pub const FPS_29_97: Rate = Rate { num: 30000, den: 1001 };
    pub const FPS_30: Rate = Rate { num: 30, den: 1 };
    pub const FPS_50: Rate = Rate { num: 50, den: 1 };
    pub const FPS_59_94: Rate = Rate { num: 60000, den: 1001 };
    pub const FPS_60: Rate = Rate { num: 60, den: 1 };
    pub const FPS_120: Rate = Rate { num: 120, den: 1 };

    /// Frame rates offered in the sequence-settings UI.
    pub const PRESETS: [Rate; 9] = [
        Rate::FPS_23_976,
        Rate::FPS_24,
        Rate::FPS_25,
        Rate::FPS_29_97,
        Rate::FPS_30,
        Rate::FPS_50,
        Rate::FPS_59_94,
        Rate::FPS_60,
        Rate::FPS_120,
    ];

    pub fn new(num: u32, den: u32) -> Result<Rate, TimeError> {
        if num == 0 || den == 0 {
            return Err(TimeError::ZeroRate);
        }
        let g = gcd(num, den);
        Ok(Rate { num: num / g, den: den / g })
    }

    /// Builds a rate from whole frames per second.
    pub const fn from_integer(fps: u32) -> Rate {
        Rate { num: fps, den: 1 }
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

    /// The nominal (rounded up) frame count per second, used for timecode.
    ///
    /// 29.97 counts 30 frames per timecode second, 59.94 counts 60.
    #[inline]
    pub fn nominal_fps(self) -> u32 {
        // Integer ceiling division: NTSC rates round up to their integer parent.
        self.num.div_ceil(self.den)
    }

    /// Whether SMPTE drop-frame timecode applies.
    ///
    /// True for the `1000/1001`-pulled-down NTSC rates whose nominal rate is a
    /// multiple of 30 (29.97, 59.94, 119.88). 23.976 is *not* drop-frame: it has
    /// no standard drop-frame notation.
    pub fn is_drop_frame(self) -> bool {
        self.den == 1001 && self.num.is_multiple_of(30000)
    }

    /// Exact tick duration of one frame, if the rate divides the timebase.
    ///
    /// Returns `None` only for exotic rates the timebase cannot express
    /// exactly; callers then fall back on [`Rate::frame_to_ticks`], which
    /// computes each frame boundary from the frame index and so never
    /// accumulates drift.
    pub fn frame_duration_exact(self) -> Option<Ticks> {
        let n = TICKS_PER_SECOND as i128 * self.den as i128;
        let d = self.num as i128;
        if n % d == 0 {
            Some(Ticks((n / d) as i64))
        } else {
            None
        }
    }

    /// Approximate tick duration of one frame, rounded.
    pub fn frame_duration(self) -> Ticks {
        Ticks(div_round(TICKS_PER_SECOND as i128 * self.den as i128, self.num as i128) as i64)
    }

    /// Start of frame `frame` on the timeline.
    ///
    /// Always computed from the absolute frame index rather than by summing
    /// frame durations, so long timelines stay exact.
    pub fn frame_to_ticks(self, frame: i64) -> Ticks {
        let n = frame as i128 * TICKS_PER_SECOND as i128 * self.den as i128;
        Ticks(div_round(n, self.num as i128) as i64)
    }

    /// The frame containing `t` (rounds toward negative infinity).
    pub fn ticks_to_frame(self, t: Ticks) -> i64 {
        let n = t.raw() as i128 * self.num as i128;
        let d = TICKS_PER_SECOND as i128 * self.den as i128;
        div_floor(n, d) as i64
    }

    /// The nearest frame boundary to `t`.
    pub fn ticks_to_frame_round(self, t: Ticks) -> i64 {
        let n = t.raw() as i128 * self.num as i128;
        let d = TICKS_PER_SECOND as i128 * self.den as i128;
        div_round(n, d) as i64
    }

    /// Snaps `t` down to the start of the frame that contains it.
    pub fn snap_floor(self, t: Ticks) -> Ticks {
        self.frame_to_ticks(self.ticks_to_frame(t))
    }

    /// Snaps `t` to the nearest frame boundary.
    pub fn snap_round(self, t: Ticks) -> Ticks {
        self.frame_to_ticks(self.ticks_to_frame_round(t))
    }

    /// How many whole frames fit in `duration`.
    pub fn duration_to_frames(self, duration: Ticks) -> i64 {
        self.ticks_to_frame(duration)
    }
}

impl Default for Rate {
    fn default() -> Self {
        Rate::FPS_30
    }
}

impl fmt::Display for Rate {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if self.den == 1 {
            write!(f, "{} fps", self.num)
        } else {
            // Two decimals is the conventional shorthand (29.97, 23.976 needs three).
            let v = self.as_f64();
            if (v * 100.0).round() / 100.0 == (v * 1000.0).round() / 1000.0 {
                write!(f, "{:.2} fps", v)
            } else {
                write!(f, "{:.3} fps", v)
            }
        }
    }
}

impl fmt::Debug for Rate {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "Rate({}/{})", self.num, self.den)
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
