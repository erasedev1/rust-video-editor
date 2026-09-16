use std::fmt;
use std::ops::{Add, AddAssign, Div, Mul, Neg, Rem, Sub, SubAssign};

use serde::{Deserialize, Serialize};

/// The canonical timeline resolution, in ticks per second.
///
/// `282_240_000 = 2^10 * 3^2 * 5^4 * 7^2`
///
/// The factorisation is what matters. A duration is exactly representable when
/// its denominator divides this constant, so this value was picked as a common
/// multiple of every rate the editor needs to be exact about:
///
/// * Integer video rates: 24, 25, 30, 48, 50, 60, 120, 240
/// * NTSC video rates: their `1001` denominators cancel because
///   `24000`, `30000`, `60000` and `120000` all divide `TICKS_PER_SECOND`
/// * Audio sample rates: 8000, 11025, 16000, 22050, 32000, 44100, 48000,
///   88200, 96000, 176400, 192000
///
/// At this resolution an `i64` still spans roughly 1035 years, so overflow is
/// not a practical concern for timeline positions.
pub const TICKS_PER_SECOND: i64 = 282_240_000;

/// A point in time, or a duration, measured in [`TICKS_PER_SECOND`] ticks.
///
/// This is the canonical time type across the whole editor. Frames, seconds and
/// timecode are *views* onto it, produced on demand.
#[derive(Copy, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Default, Serialize, Deserialize)]
#[serde(transparent)]
pub struct Ticks(pub i64);

impl Ticks {
    pub const ZERO: Ticks = Ticks(0);
    pub const MIN: Ticks = Ticks(i64::MIN);
    pub const MAX: Ticks = Ticks(i64::MAX);
    /// Exactly one second.
    pub const SECOND: Ticks = Ticks(TICKS_PER_SECOND);

    #[inline]
    pub const fn new(raw: i64) -> Self {
        Ticks(raw)
    }

    #[inline]
    pub const fn raw(self) -> i64 {
        self.0
    }

    /// Exact whole seconds.
    #[inline]
    pub const fn from_seconds(secs: i64) -> Self {
        Ticks(secs.saturating_mul(TICKS_PER_SECOND))
    }

    /// Exact milliseconds (1000 divides `TICKS_PER_SECOND`).
    #[inline]
    pub const fn from_millis(ms: i64) -> Self {
        Ticks(ms.saturating_mul(TICKS_PER_SECOND / 1000))
    }

    /// Microseconds, rounded.
    ///
    /// Unlike milliseconds, microseconds are *not* exactly representable:
    /// `TICKS_PER_SECOND / 1_000_000 = 7056 / 25`, so only multiples of 25 us
    /// land on a whole tick. Microsecond input is an interop convenience, never
    /// a storage format.
    pub fn from_micros(us: i64) -> Self {
        Ticks(div_round(us as i128 * 7056, 25) as i64)
    }

    /// Converts from a rational `num / den` seconds, rounding half away from zero.
    ///
    /// This is the entry point for values coming out of FFmpeg, whose packet and
    /// frame timestamps are rationals on a container-defined timebase.
    pub fn from_rational(num: i64, den: i64) -> Self {
        if den == 0 {
            return Ticks::ZERO;
        }
        let n = num as i128 * TICKS_PER_SECOND as i128;
        Ticks(div_round(n, den as i128) as i64)
    }

    /// Lossy conversion from floating point seconds.
    ///
    /// Only for interop at the edges of the system (UI drag deltas, external
    /// metadata). Never use `f64` seconds as storage.
    pub fn from_secs_f64(secs: f64) -> Self {
        if !secs.is_finite() {
            return Ticks::ZERO;
        }
        Ticks((secs * TICKS_PER_SECOND as f64).round() as i64)
    }

    /// Lossy conversion to floating point seconds, for display and for audio
    /// and GPU interfaces that are inherently float-based.
    #[inline]
    pub fn as_secs_f64(self) -> f64 {
        self.0 as f64 / TICKS_PER_SECOND as f64
    }

    #[inline]
    pub fn as_secs_f32(self) -> f32 {
        self.as_secs_f64() as f32
    }

    #[inline]
    pub const fn is_zero(self) -> bool {
        self.0 == 0
    }

    #[inline]
    pub const fn is_negative(self) -> bool {
        self.0 < 0
    }

    #[inline]
    pub const fn abs(self) -> Ticks {
        Ticks(self.0.abs())
    }

    #[inline]
    pub const fn saturating_add(self, rhs: Ticks) -> Ticks {
        Ticks(self.0.saturating_add(rhs.0))
    }

    #[inline]
    pub const fn saturating_sub(self, rhs: Ticks) -> Ticks {
        Ticks(self.0.saturating_sub(rhs.0))
    }

    #[inline]
    pub fn clamp_range(self, min: Ticks, max: Ticks) -> Ticks {
        debug_assert!(min <= max);
        self.max(min).min(max)
    }

    /// Clamps to be non-negative. Timeline positions and durations are never
    /// allowed below zero.
    #[inline]
    pub fn clamp_non_negative(self) -> Ticks {
        if self.0 < 0 {
            Ticks::ZERO
        } else {
            self
        }
    }

    /// Scales by a rational factor, rounding half away from zero.
    ///
    /// Used for clip playback speed, where a source duration maps onto a
    /// different timeline duration.
    pub fn scale(self, num: i64, den: i64) -> Ticks {
        if den == 0 {
            return Ticks::ZERO;
        }
        Ticks(div_round(self.0 as i128 * num as i128, den as i128) as i64)
    }
}

/// Integer division rounding half away from zero, for signed i128.
pub(crate) fn div_round(num: i128, den: i128) -> i128 {
    debug_assert!(den != 0);
    let (num, den) = if den < 0 { (-num, -den) } else { (num, den) };
    if num >= 0 {
        (num + den / 2) / den
    } else {
        -((-num + den / 2) / den)
    }
}

/// Integer division rounding toward negative infinity, for signed i128.
pub(crate) fn div_floor(num: i128, den: i128) -> i128 {
    debug_assert!(den != 0);
    let (num, den) = if den < 0 { (-num, -den) } else { (num, den) };
    let q = num / den;
    if num % den != 0 && num < 0 {
        q - 1
    } else {
        q
    }
}

impl Add for Ticks {
    type Output = Ticks;
    #[inline]
    fn add(self, rhs: Ticks) -> Ticks {
        Ticks(self.0 + rhs.0)
    }
}

impl Sub for Ticks {
    type Output = Ticks;
    #[inline]
    fn sub(self, rhs: Ticks) -> Ticks {
        Ticks(self.0 - rhs.0)
    }
}

impl Neg for Ticks {
    type Output = Ticks;
    #[inline]
    fn neg(self) -> Ticks {
        Ticks(-self.0)
    }
}

impl Mul<i64> for Ticks {
    type Output = Ticks;
    #[inline]
    fn mul(self, rhs: i64) -> Ticks {
        Ticks(self.0 * rhs)
    }
}

impl Div<i64> for Ticks {
    type Output = Ticks;
    #[inline]
    fn div(self, rhs: i64) -> Ticks {
        Ticks(self.0 / rhs)
    }
}

impl Rem for Ticks {
    type Output = Ticks;
    #[inline]
    fn rem(self, rhs: Ticks) -> Ticks {
        Ticks(self.0 % rhs.0)
    }
}

impl AddAssign for Ticks {
    #[inline]
    fn add_assign(&mut self, rhs: Ticks) {
        self.0 += rhs.0;
    }
}

impl SubAssign for Ticks {
    #[inline]
    fn sub_assign(&mut self, rhs: Ticks) {
        self.0 -= rhs.0;
    }
}

impl std::iter::Sum for Ticks {
    fn sum<I: Iterator<Item = Ticks>>(iter: I) -> Ticks {
        iter.fold(Ticks::ZERO, |a, b| a + b)
    }
}

impl fmt::Debug for Ticks {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "Ticks({} = {:.6}s)", self.0, self.as_secs_f64())
    }
}

impl fmt::Display for Ticks {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let neg = self.0 < 0;
        let t = self.0.unsigned_abs();
        let tps = TICKS_PER_SECOND as u64;
        let total_secs = t / tps;
        let frac = t % tps;
        let millis = frac * 1000 / tps;
        write!(
            f,
            "{}{:02}:{:02}:{:02}.{:03}",
            if neg { "-" } else { "" },
            total_secs / 3600,
            (total_secs / 60) % 60,
            total_secs % 60,
            millis
        )
    }
}
