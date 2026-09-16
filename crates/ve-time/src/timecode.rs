use std::fmt;
use std::str::FromStr;

use crate::{Rate, Ticks, TimeError};

/// SMPTE timecode, with NTSC drop-frame support.
///
/// A `Timecode` is a *rendering* of a frame index at a particular [`Rate`]; the
/// rate is carried along so that the value can be converted back to [`Ticks`]
/// unambiguously.
#[derive(Copy, Clone, PartialEq, Eq, Debug)]
pub struct Timecode {
    pub hours: u32,
    pub minutes: u32,
    pub seconds: u32,
    pub frames: u32,
    pub rate: Rate,
    /// True when rendered with drop-frame (`;`) separators.
    pub drop_frame: bool,
    /// True when the underlying position was before zero.
    pub negative: bool,
}

impl Timecode {
    /// Renders an absolute frame index as timecode.
    pub fn from_frame(frame: i64, rate: Rate) -> Timecode {
        let negative = frame < 0;
        let mut f = frame.unsigned_abs() as i64;
        let nominal = rate.nominal_fps() as i64;
        let drop_frame = rate.is_drop_frame();

        if drop_frame {
            // Frames dropped per minute, except every tenth minute:
            // 2 at 29.97, 4 at 59.94, 8 at 119.88.
            let drop = nominal / 15;
            // Nine minutes in every ten drop `drop` frame labels; the tenth does not.
            let frames_per_min = nominal * 60 - drop;
            let frames_per_10min = nominal * 600 - drop * 9;
            let tens = f / frames_per_10min;
            let rem = f % frames_per_10min;
            // Re-inflate the dropped labels so that the wall-clock fields read correctly.
            f += drop * 9 * tens;
            if rem >= drop {
                f += drop * ((rem - drop) / frames_per_min);
            }
        }

        Timecode {
            hours: ((f / (nominal * 3600)) % 24) as u32,
            minutes: ((f / (nominal * 60)) % 60) as u32,
            seconds: ((f / nominal) % 60) as u32,
            frames: (f % nominal) as u32,
            rate,
            drop_frame,
            negative,
        }
    }

    /// Renders a timeline position as timecode.
    pub fn from_ticks(t: Ticks, rate: Rate) -> Timecode {
        Timecode::from_frame(rate.ticks_to_frame(t), rate)
    }

    /// The absolute frame index this timecode denotes.
    pub fn to_frame(self) -> i64 {
        let nominal = self.rate.nominal_fps() as i64;
        let mut f = self.hours as i64 * 3600 * nominal
            + self.minutes as i64 * 60 * nominal
            + self.seconds as i64 * nominal
            + self.frames as i64;

        if self.drop_frame {
            let drop = nominal / 15;
            let total_minutes = self.hours as i64 * 60 + self.minutes as i64;
            f -= drop * (total_minutes - total_minutes / 10);
        }

        if self.negative {
            -f
        } else {
            f
        }
    }

    /// The timeline position this timecode denotes.
    pub fn to_ticks(self) -> Ticks {
        self.rate.frame_to_ticks(self.to_frame())
    }

    /// Parses `hh:mm:ss:ff` (or `hh:mm:ss;ff` for drop-frame) at a known rate.
    pub fn parse(s: &str, rate: Rate) -> Result<Timecode, TimeError> {
        let (negative, body) = match s.strip_prefix('-') {
            Some(rest) => (true, rest),
            None => (false, s),
        };

        // The last separator distinguishes drop-frame (`;` or `.`) from non-drop (`:`).
        let drop_marker = body.rfind([';', '.']);
        let normalised = body.replace([';', '.'], ":");
        let parts: Vec<&str> = normalised.split(':').collect();
        if parts.len() != 4 {
            return Err(TimeError::MalformedTimecode(s.to_string()));
        }

        let mut nums = [0u32; 4];
        for (i, p) in parts.iter().enumerate() {
            nums[i] =
                p.parse::<u32>().map_err(|_| TimeError::MalformedTimecode(s.to_string()))?;
        }
        let [hours, minutes, seconds, frames] = nums;

        if minutes > 59 || seconds > 59 || frames >= rate.nominal_fps() {
            return Err(TimeError::TimecodeOutOfRange(s.to_string()));
        }

        // A `;` in the string only means drop-frame if the rate actually supports it.
        let drop_frame = drop_marker.is_some() && rate.is_drop_frame();

        Ok(Timecode { hours, minutes, seconds, frames, rate, drop_frame, negative })
    }

    /// Parses at 29.97 drop-frame, the most common interchange default.
    pub fn from_str_ntsc(s: &str) -> Result<Timecode, TimeError> {
        Timecode::parse(s, Rate::FPS_29_97)
    }
}

impl fmt::Display for Timecode {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let sep = if self.drop_frame { ';' } else { ':' };
        write!(
            f,
            "{}{:02}:{:02}:{:02}{}{:02}",
            if self.negative { "-" } else { "" },
            self.hours,
            self.minutes,
            self.seconds,
            sep,
            self.frames
        )
    }
}

impl FromStr for Timecode {
    type Err = TimeError;
    /// Parses assuming 30 fps non-drop. Prefer [`Timecode::parse`] when the
    /// sequence rate is known.
    fn from_str(s: &str) -> Result<Timecode, TimeError> {
        Timecode::parse(s, Rate::FPS_30)
    }
}
