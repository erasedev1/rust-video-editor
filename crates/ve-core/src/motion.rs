//! Motion blur: what a moving layer looks like over the time a shutter is open.
//!
//! # A camera setting, not an effect
//!
//! A frame is not an instant. A camera's shutter is open for part of the frame
//! interval, and whatever moves while it is open is smeared across the picture.
//! The transform is animated, so where a layer is at any instant inside that
//! interval is already known exactly — motion blur is therefore not something
//! to invent, but something to *stop ignoring*.
//!
//! # Two switches, as every compositor has
//!
//! The shutter belongs to the canvas: it is a property of the camera the
//! sequence is pretending to be, and every layer in one frame is exposed for
//! the same length of time. Whether a *particular* layer is blurred is per clip,
//! because blur costs a pass per sample and most layers do not move.
//!
//! Both have to be on, which is what lets one switch turn the cost off across a
//! whole sequence while a scrub or an export is set up. A clip defaults to
//! **off**, so opening a project written before any of this existed draws
//! exactly what it drew before.
//!
//! # What is sampled, and what is not
//!
//! The **transform** is sampled across the shutter; the picture is not. Every
//! sample shows the same decoded frame at a different position, scale, rotation
//! and opacity, which is exactly the "motion blur from the keyframed transform"
//! this phase is about.
//!
//! Sampling the *source* time as well — showing frames from between two frames
//! — is a different and much larger feature: it needs sub-frame decoding or
//! frame interpolation, and it blurs footage that is moving inside itself
//! rather than a layer that is moving across the frame. That belongs with the
//! professional work in a later phase, not smuggled in here under the same
//! name.

use serde::{Deserialize, Serialize};
use ve_time::Ticks;

/// How long the shutter is open, and how finely the interval is sampled.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct MotionBlur {
    /// Off turns the cost off for the whole canvas at once, whatever the
    /// individual clips ask for.
    pub enabled: bool,
    /// Degrees of a full rotation of a rotary shutter. 180° — half the frame
    /// interval — is the film convention and what most footage is shot at; 360°
    /// exposes for the whole interval, and small angles give the stepped,
    /// strobing look of a fast shutter.
    pub shutter_angle: f64,
    /// How many instants the interval is sampled at. Every sample is a draw, so
    /// this is the cost knob: enough of them and the smear is smooth, too few
    /// and it reads as a row of ghosts.
    pub samples: u32,
}

impl MotionBlur {
    /// The widest shutter there is: the whole frame interval.
    pub const MAX_ANGLE: f64 = 360.0;
    /// Two samples is the least that can smear at all; the ceiling is a cost
    /// bound rather than a quality one.
    pub const MIN_SAMPLES: u32 = 2;
    pub const MAX_SAMPLES: u32 = 32;

    pub fn new(shutter_angle: f64, samples: u32) -> Self {
        MotionBlur {
            enabled: true,
            shutter_angle: shutter_angle.clamp(0.0, Self::MAX_ANGLE),
            samples: samples.clamp(Self::MIN_SAMPLES, Self::MAX_SAMPLES),
        }
    }

    /// Whether this canvas would blur anything at all.
    pub fn is_active(&self) -> bool {
        self.enabled && self.shutter_angle > 0.0 && self.samples >= Self::MIN_SAMPLES
    }

    /// How long the shutter stays open, given the frame interval.
    pub fn open_for(&self, frame: Ticks) -> Ticks {
        frame.scale(
            (self.shutter_angle.clamp(0.0, Self::MAX_ANGLE) * 1000.0).round() as i64,
            (Self::MAX_ANGLE * 1000.0) as i64,
        )
    }

    /// The instants to sample, as offsets from the frame's own time.
    ///
    /// Centred on the frame, so a blurred layer is smeared symmetrically about
    /// where it would have been drawn rather than trailing behind it — the
    /// same choice a shutter phase of zero makes in the tools this borrows its
    /// vocabulary from.
    ///
    /// Each sample sits in the middle of the slice of time it stands for, which
    /// is why the offsets are at half-steps: sampling the two ends instead
    /// would weight the extremes twice and leave the middle thin.
    pub fn offsets(&self, frame: Ticks) -> Vec<Ticks> {
        if !self.is_active() {
            return Vec::new();
        }
        let open = self.open_for(frame);
        let n = self.samples.clamp(Self::MIN_SAMPLES, Self::MAX_SAMPLES) as i64;
        (0..n)
            .map(|i| {
                // (i + 0.5) / n of the way through the interval, in exact
                // rational arithmetic so the offsets stay symmetric.
                open.scale(2 * i + 1, 2 * n) - open.scale(1, 2)
            })
            .collect()
    }
}

impl Default for MotionBlur {
    fn default() -> Self {
        // The film convention, sampled finely enough that a fast move reads as
        // a smear rather than as a row of copies. Enabled here, but nothing is
        // blurred until a clip asks to be.
        MotionBlur { enabled: true, shutter_angle: 180.0, samples: 12 }
    }
}
