//! Commands for the audio-only parts of an edit: fades, transitions and the
//! track level.
//!
//! These sit apart from [`crate::property_commands`] because a fade is not a
//! [`ve_core::Property`] — see [`ve_core::fade`] for why — and because a track
//! level belongs to a lane rather than to a clip. What they share with every
//! other command is the contract: capture before mutating, restore exactly, and
//! merge a gesture into one history entry.

use std::any::Any;

use ve_core::{ClipId, Fade, FadeCurve, FadeEdge, Project, SequenceId, TrackId};
use ve_time::{Ticks, TimeRange};

use crate::{clip_mut, Command, CommandError};

/// Sets the fade at one end of a clip.
///
/// Merges with later sets of the same edge on the same clip, so dragging a fade
/// handle across the timeline is one undo step rather than one per mouse move.
#[derive(Debug)]
pub struct SetClipFade {
    sequence: SequenceId,
    clip: ClipId,
    edge: FadeEdge,
    fade: Fade,
    previous: Option<Fade>,
}

impl SetClipFade {
    pub fn new(sequence: SequenceId, clip: ClipId, edge: FadeEdge, fade: Fade) -> Self {
        SetClipFade { sequence, clip, edge, fade, previous: None }
    }

    /// Removes the fade, which is the same command with a zero length.
    pub fn clear(sequence: SequenceId, clip: ClipId, edge: FadeEdge) -> Self {
        SetClipFade::new(sequence, clip, edge, Fade::NONE)
    }
}

impl Command for SetClipFade {
    fn name(&self) -> &str {
        self.edge.label()
    }

    fn apply(&mut self, project: &mut Project) -> Result<(), CommandError> {
        let (edge, fade) = (self.edge, self.fade);
        let clip = clip_mut(project, self.sequence, self.clip)?;
        // A fade longer than the clip it is on has no meaning: the length is
        // what the envelope is measured against, so it is clamped here rather
        // than left to be reinterpreted at every evaluation.
        let fade =
            Fade::new(Ticks::new(fade.length.raw().clamp(0, clip.duration.raw())), fade.curve);
        self.previous.get_or_insert(clip.audio.fade(edge));
        clip.audio.set_fade(edge, fade);
        Ok(())
    }

    fn undo(&mut self, project: &mut Project) -> Result<(), CommandError> {
        let previous =
            self.previous.ok_or_else(|| CommandError::Rejected("fade was never set".into()))?;
        let edge = self.edge;
        clip_mut(project, self.sequence, self.clip)?.audio.set_fade(edge, previous);
        Ok(())
    }

    fn merge(&mut self, next: &dyn Command) -> bool {
        match next.as_any().downcast_ref::<SetClipFade>() {
            Some(other) if other.clip == self.clip && other.edge == self.edge => {
                self.fade = other.fade;
                true
            }
            _ => false,
        }
    }

    fn as_any(&self) -> &dyn Any {
        self
    }
}

/// Crossfades two clips that overlap in time.
///
/// # Why an overlap is required
///
/// A crossfade is two clips sounding *at once*, one rising while the other
/// falls. A track here holds non-overlapping clips by construction, so two
/// clips meeting at a cut can never sound together: fading them into each other
/// there would mean both reaching silence at the cut, which is a dip, not a
/// transition. The honest version needs the clips on different tracks, overlapped
/// by however long the transition should be — which is exactly what this command
/// requires and refuses to fake.
///
/// Both fades are given the same curve, and [`FadeCurve::EqualPower`] is the one
/// to use: it is the only shape here whose complementary pair holds the power
/// constant through the crossing, so two unrelated recordings do not dip in the
/// middle. `ve-core`'s fade tests assert exactly that.
#[derive(Debug)]
pub struct CrossfadeClips {
    sequence: SequenceId,
    outgoing: ClipId,
    incoming: ClipId,
    curve: FadeCurve,
    /// `(outgoing fade-out, incoming fade-in)` as they were before.
    previous: Option<(Fade, Fade)>,
}

impl CrossfadeClips {
    /// The two clips may be given in either order; which one is outgoing is
    /// decided by where they sit on the timeline, not by the argument order.
    pub fn new(sequence: SequenceId, a: ClipId, b: ClipId, curve: FadeCurve) -> Self {
        CrossfadeClips { sequence, outgoing: a, incoming: b, curve, previous: None }
    }

    /// Whether two clips can be crossfaded, and over how long.
    ///
    /// Exposed so the interface can enable or disable the command rather than
    /// offering it and then reporting a refusal.
    pub fn overlap(
        project: &Project,
        sequence: SequenceId,
        a: ClipId,
        b: ClipId,
    ) -> Option<(ClipId, ClipId, Ticks)> {
        if a == b {
            return None;
        }
        let sequence = project.sequence(sequence)?;
        let (_, first) = sequence.find_clip(a)?;
        let (_, second) = sequence.find_clip(b)?;
        // Whichever starts earlier is the one fading out.
        let (outgoing, incoming) = if first.timeline_start <= second.timeline_start {
            (first, second)
        } else {
            (second, first)
        };
        let overlap = TimeRange::from_bounds(
            incoming.timeline_start,
            outgoing.timeline_end().min(incoming.timeline_end()),
        );
        (!overlap.is_empty()).then_some((outgoing.id, incoming.id, overlap.duration))
    }
}

impl Command for CrossfadeClips {
    fn name(&self) -> &str {
        "Crossfade"
    }

    fn apply(&mut self, project: &mut Project) -> Result<(), CommandError> {
        let (outgoing, incoming, length) =
            CrossfadeClips::overlap(project, self.sequence, self.outgoing, self.incoming)
                .ok_or_else(|| {
                    CommandError::Rejected(
                        "a crossfade needs two clips that overlap in time, which means \
                         putting them on different tracks"
                            .into(),
                    )
                })?;
        self.outgoing = outgoing;
        self.incoming = incoming;

        let fade = Fade::new(length, self.curve);
        let out_clip = clip_mut(project, self.sequence, outgoing)?;
        let was_out = out_clip.audio.fade(FadeEdge::Out);
        out_clip.audio.set_fade(FadeEdge::Out, fade);

        let in_clip = clip_mut(project, self.sequence, incoming)?;
        let was_in = in_clip.audio.fade(FadeEdge::In);
        in_clip.audio.set_fade(FadeEdge::In, fade);

        self.previous.get_or_insert((was_out, was_in));
        Ok(())
    }

    fn undo(&mut self, project: &mut Project) -> Result<(), CommandError> {
        let (was_out, was_in) = self
            .previous
            .ok_or_else(|| CommandError::Rejected("crossfade was never applied".into()))?;
        clip_mut(project, self.sequence, self.outgoing)?.audio.set_fade(FadeEdge::Out, was_out);
        clip_mut(project, self.sequence, self.incoming)?.audio.set_fade(FadeEdge::In, was_in);
        Ok(())
    }

    fn as_any(&self) -> &dyn Any {
        self
    }
}

/// Which of a track's two audio controls a [`SetTrackLevel`] sets.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TrackLevel {
    Volume,
    Pan,
}

impl TrackLevel {
    pub fn label(self) -> &'static str {
        match self {
            TrackLevel::Volume => "Track Volume",
            TrackLevel::Pan => "Track Pan",
        }
    }
}

/// Sets a track's volume or pan.
///
/// Merges like every other slider, so a drag is one undo step.
#[derive(Debug)]
pub struct SetTrackLevel {
    sequence: SequenceId,
    track: TrackId,
    which: TrackLevel,
    value: f64,
    previous: Option<f64>,
}

impl SetTrackLevel {
    pub fn new(sequence: SequenceId, track: TrackId, which: TrackLevel, value: f64) -> Self {
        SetTrackLevel { sequence, track, which, value, previous: None }
    }
}

impl Command for SetTrackLevel {
    fn name(&self) -> &str {
        self.which.label()
    }

    fn apply(&mut self, project: &mut Project) -> Result<(), CommandError> {
        let value = match self.which {
            // Bounded here so no path into the model can store a gain the mixer
            // would have to defend itself against. Four times unity is +12 dB,
            // which is as far as a track strip has any business going.
            TrackLevel::Volume => self.value.clamp(0.0, 4.0),
            TrackLevel::Pan => self.value.clamp(-1.0, 1.0),
        };
        if !value.is_finite() {
            return Err(CommandError::Rejected("that is not a level".into()));
        }
        let track = crate::track_mut(project, self.sequence, self.track)?;
        let slot = match self.which {
            TrackLevel::Volume => &mut track.volume,
            TrackLevel::Pan => &mut track.pan,
        };
        self.previous.get_or_insert(*slot);
        *slot = value;
        Ok(())
    }

    fn undo(&mut self, project: &mut Project) -> Result<(), CommandError> {
        let previous =
            self.previous.ok_or_else(|| CommandError::Rejected("never applied".into()))?;
        let track = crate::track_mut(project, self.sequence, self.track)?;
        match self.which {
            TrackLevel::Volume => track.volume = previous,
            TrackLevel::Pan => track.pan = previous,
        }
        Ok(())
    }

    fn merge(&mut self, next: &dyn Command) -> bool {
        match next.as_any().downcast_ref::<SetTrackLevel>() {
            Some(other) if other.track == self.track && other.which == self.which => {
                self.value = other.value;
                true
            }
            _ => false,
        }
    }

    fn as_any(&self) -> &dyn Any {
        self
    }
}
