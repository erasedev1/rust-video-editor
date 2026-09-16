use serde::{Deserialize, Serialize};

use crate::ticks::{div_floor, div_round, Ticks, TICKS_PER_SECOND};
use crate::TimeError;

/// An audio sample rate in Hz.
///
/// Every rate in the 44.1 kHz and 48 kHz families divides [`TICKS_PER_SECOND`],
/// so sample indices convert to and from timeline positions without rounding.
#[derive(Copy, Clone, PartialEq, Eq, Hash, Debug, Serialize, Deserialize)]
#[serde(transparent)]
pub struct SampleRate(u32);

impl SampleRate {
    pub const HZ_44100: SampleRate = SampleRate(44_100);
    pub const HZ_48000: SampleRate = SampleRate(48_000);
    pub const HZ_96000: SampleRate = SampleRate(96_000);

    pub fn new(hz: u32) -> Result<SampleRate, TimeError> {
        if hz == 0 {
            return Err(TimeError::ZeroSampleRate);
        }
        Ok(SampleRate(hz))
    }

    #[inline]
    pub const fn hz(self) -> u32 {
        self.0
    }

    /// Whether this rate maps onto the timebase without any rounding.
    pub fn is_exact(self) -> bool {
        TICKS_PER_SECOND % self.0 as i64 == 0
    }

    /// Timeline position of sample `index`.
    pub fn sample_to_ticks(self, index: i64) -> Ticks {
        let n = index as i128 * TICKS_PER_SECOND as i128;
        Ticks(div_round(n, self.0 as i128) as i64)
    }

    /// The sample containing `t` (rounds toward negative infinity).
    pub fn ticks_to_sample(self, t: Ticks) -> i64 {
        let n = t.raw() as i128 * self.0 as i128;
        div_floor(n, TICKS_PER_SECOND as i128) as i64
    }

    /// Number of whole samples in `duration`.
    pub fn duration_to_samples(self, duration: Ticks) -> i64 {
        self.ticks_to_sample(duration)
    }
}

impl Default for SampleRate {
    fn default() -> Self {
        SampleRate::HZ_48000
    }
}
