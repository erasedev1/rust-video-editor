//! Exact time representation for the Verge video editor.
//!
//! Timeline positions are **never** stored as floating point seconds. Everything
//! is an integer count of [`Ticks`] on a fixed, very high resolution timebase.
//! That timebase was chosen so that every broadcast frame rate and every common
//! audio sample rate lands on an exact integer tick, which means repeated
//! conversions between frames, samples and timeline positions never accumulate
//! error.
//!
//! See [`TICKS_PER_SECOND`] for the derivation.

mod rate;
mod samples;
mod ticks;
mod timecode;

pub use rate::Rate;
pub use samples::SampleRate;
pub use ticks::{Ticks, TICKS_PER_SECOND};
pub use timecode::Timecode;

/// Errors produced when parsing or constructing time values.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum TimeError {
    #[error("frame rate numerator and denominator must both be non-zero")]
    ZeroRate,
    #[error("sample rate must be non-zero")]
    ZeroSampleRate,
    #[error("malformed timecode string: {0}")]
    MalformedTimecode(String),
    #[error("timecode field out of range: {0}")]
    TimecodeOutOfRange(String),
}

mod range;
pub use range::TimeRange;
