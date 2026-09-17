//! Commands that change a sequence's structure: tracks, markers, clip state.

use std::any::Any;

use ve_core::{
    BlendMode, ClipId, Marker, MarkerId, Project, SequenceId, Track, TrackId, TrackKind,
};

use crate::{clip_mut, Command, CommandError};

/// Appends a video or audio track.
#[derive(Debug)]
pub struct AddTrack {
    sequence: SequenceId,
    kind: TrackKind,
    /// Minted on the first apply and reused on redo, so anything referring to
    /// the track survives an undo/redo cycle.
    track_id: Option<TrackId>,
}

impl AddTrack {
    pub fn new(sequence: SequenceId, kind: TrackKind) -> Self {
        AddTrack { sequence, kind, track_id: None }
    }

    pub fn track_id(&self) -> Option<TrackId> {
        self.track_id
    }
}

impl Command for AddTrack {
    fn name(&self) -> &str {
        "Add Track"
    }

    fn apply(&mut self, project: &mut Project) -> Result<(), CommandError> {
        let id = match self.track_id {
            Some(id) => id,
            None => {
                let id = project.new_track_id();
                self.track_id = Some(id);
                id
            }
        };
        let kind = self.kind;
        project
            .sequence_mut(self.sequence)
            .ok_or(CommandError::SequenceNotFound(self.sequence))?
            .add_track(id, kind);
        Ok(())
    }

    fn undo(&mut self, project: &mut Project) -> Result<(), CommandError> {
        let id = self
            .track_id
            .ok_or_else(|| CommandError::Rejected("track was never added".into()))?;
        project
            .sequence_mut(self.sequence)
            .ok_or(CommandError::SequenceNotFound(self.sequence))?
            .remove_track(id)
            .ok_or(CommandError::TrackNotFound(id))?;
        Ok(())
    }

    fn as_any(&self) -> &dyn Any {
        self
    }
}

/// Deletes a track and everything on it.
///
/// The whole track, clips included, is captured so undo restores it intact.
#[derive(Debug)]
pub struct RemoveTrack {
    sequence: SequenceId,
    track_id: TrackId,
    removed: Option<(usize, Track)>,
}

impl RemoveTrack {
    pub fn new(sequence: SequenceId, track_id: TrackId) -> Self {
        RemoveTrack { sequence, track_id, removed: None }
    }
}

impl Command for RemoveTrack {
    fn name(&self) -> &str {
        "Delete Track"
    }

    fn apply(&mut self, project: &mut Project) -> Result<(), CommandError> {
        let seq = project
            .sequence_mut(self.sequence)
            .ok_or(CommandError::SequenceNotFound(self.sequence))?;
        let index =
            seq.track_index(self.track_id).ok_or(CommandError::TrackNotFound(self.track_id))?;
        let track = seq
            .remove_track(self.track_id)
            .ok_or(CommandError::TrackNotFound(self.track_id))?;
        self.removed = Some((index, track));
        Ok(())
    }

    fn undo(&mut self, project: &mut Project) -> Result<(), CommandError> {
        let (index, track) = self
            .removed
            .take()
            .ok_or_else(|| CommandError::Rejected("nothing captured to restore".into()))?;
        let seq = project
            .sequence_mut(self.sequence)
            .ok_or(CommandError::SequenceNotFound(self.sequence))?;
        // Back into its original slot: track order is layer order for video, so
        // restoring it at the end would change what composites over what.
        let index = index.min(seq.tracks.len());
        seq.tracks.insert(index, track);
        Ok(())
    }

    fn as_any(&self) -> &dyn Any {
        self
    }
}

/// Moves a track to a different position in the stack.
///
/// Order is layer order for video — index 0 is the bottom layer — so this is a
/// compositing change, not a cosmetic one.
#[derive(Debug)]
pub struct MoveTrack {
    sequence: SequenceId,
    track_id: TrackId,
    to: usize,
    from: Option<usize>,
}

impl MoveTrack {
    pub fn new(sequence: SequenceId, track_id: TrackId, to: usize) -> Self {
        MoveTrack { sequence, track_id, to, from: None }
    }
}

impl Command for MoveTrack {
    fn name(&self) -> &str {
        "Move Track"
    }

    fn apply(&mut self, project: &mut Project) -> Result<(), CommandError> {
        let seq = project
            .sequence_mut(self.sequence)
            .ok_or(CommandError::SequenceNotFound(self.sequence))?;
        let from =
            seq.track_index(self.track_id).ok_or(CommandError::TrackNotFound(self.track_id))?;
        if self.to >= seq.tracks.len() {
            return Err(CommandError::Rejected("no such position in the track stack".into()));
        }
        let track = seq.tracks.remove(from);
        seq.tracks.insert(self.to, track);
        self.from.get_or_insert(from);
        Ok(())
    }

    fn undo(&mut self, project: &mut Project) -> Result<(), CommandError> {
        let from =
            self.from.ok_or_else(|| CommandError::Rejected("track was never moved".into()))?;
        let seq = project
            .sequence_mut(self.sequence)
            .ok_or(CommandError::SequenceNotFound(self.sequence))?;
        let now =
            seq.track_index(self.track_id).ok_or(CommandError::TrackNotFound(self.track_id))?;
        let track = seq.tracks.remove(now);
        seq.tracks.insert(from.min(seq.tracks.len()), track);
        Ok(())
    }

    fn as_any(&self) -> &dyn Any {
        self
    }
}

/// Which of a track's three switches a [`SetTrackFlag`] sets.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TrackFlag {
    Muted,
    Solo,
    Locked,
}

/// Mutes, solos or locks a track.
///
/// These are project data — they are saved with the file and they change what
/// is rendered and what can be edited — so they go through the history like
/// every other edit rather than being poked into the model from the interface.
#[derive(Debug)]
pub struct SetTrackFlag {
    sequence: SequenceId,
    track_id: TrackId,
    flag: TrackFlag,
    value: bool,
    previous: Option<bool>,
}

impl SetTrackFlag {
    pub fn new(sequence: SequenceId, track_id: TrackId, flag: TrackFlag, value: bool) -> Self {
        SetTrackFlag { sequence, track_id, flag, value, previous: None }
    }
}

impl Command for SetTrackFlag {
    fn name(&self) -> &str {
        match (self.flag, self.value) {
            (TrackFlag::Muted, true) => "Mute Track",
            (TrackFlag::Muted, false) => "Unmute Track",
            (TrackFlag::Solo, true) => "Solo Track",
            (TrackFlag::Solo, false) => "Unsolo Track",
            (TrackFlag::Locked, true) => "Lock Track",
            (TrackFlag::Locked, false) => "Unlock Track",
        }
    }

    fn apply(&mut self, project: &mut Project) -> Result<(), CommandError> {
        let track = crate::track_mut(project, self.sequence, self.track_id)?;
        let slot = match self.flag {
            TrackFlag::Muted => &mut track.muted,
            TrackFlag::Solo => &mut track.solo,
            TrackFlag::Locked => &mut track.locked,
        };
        self.previous.get_or_insert(*slot);
        *slot = self.value;
        Ok(())
    }

    fn undo(&mut self, project: &mut Project) -> Result<(), CommandError> {
        let previous =
            self.previous.ok_or_else(|| CommandError::Rejected("never applied".into()))?;
        let track = crate::track_mut(project, self.sequence, self.track_id)?;
        match self.flag {
            TrackFlag::Muted => track.muted = previous,
            TrackFlag::Solo => track.solo = previous,
            TrackFlag::Locked => track.locked = previous,
        }
        Ok(())
    }

    fn as_any(&self) -> &dyn Any {
        self
    }
}

/// Adds a marker to a sequence.
#[derive(Debug)]
pub struct AddMarker {
    sequence: SequenceId,
    marker: Option<Marker>,
    marker_id: MarkerId,
}

impl AddMarker {
    pub fn new(sequence: SequenceId, marker: Marker) -> Self {
        let marker_id = marker.id;
        AddMarker { sequence, marker: Some(marker), marker_id }
    }
}

impl Command for AddMarker {
    fn name(&self) -> &str {
        "Add Marker"
    }

    fn apply(&mut self, project: &mut Project) -> Result<(), CommandError> {
        let marker = self
            .marker
            .take()
            .ok_or_else(|| CommandError::Rejected("marker already added".into()))?;
        project
            .sequence_mut(self.sequence)
            .ok_or(CommandError::SequenceNotFound(self.sequence))?
            .add_marker(marker);
        Ok(())
    }

    fn undo(&mut self, project: &mut Project) -> Result<(), CommandError> {
        self.marker = project
            .sequence_mut(self.sequence)
            .ok_or(CommandError::SequenceNotFound(self.sequence))?
            .remove_marker(self.marker_id);
        Ok(())
    }

    fn as_any(&self) -> &dyn Any {
        self
    }
}

/// Deletes a marker.
#[derive(Debug)]
pub struct RemoveMarker {
    sequence: SequenceId,
    marker_id: MarkerId,
    removed: Option<Marker>,
}

impl RemoveMarker {
    pub fn new(sequence: SequenceId, marker_id: MarkerId) -> Self {
        RemoveMarker { sequence, marker_id, removed: None }
    }
}

impl Command for RemoveMarker {
    fn name(&self) -> &str {
        "Delete Marker"
    }

    fn apply(&mut self, project: &mut Project) -> Result<(), CommandError> {
        self.removed = project
            .sequence_mut(self.sequence)
            .ok_or(CommandError::SequenceNotFound(self.sequence))?
            .remove_marker(self.marker_id);
        if self.removed.is_none() {
            return Err(CommandError::Rejected("no such marker".into()));
        }
        Ok(())
    }

    fn undo(&mut self, project: &mut Project) -> Result<(), CommandError> {
        let marker = self
            .removed
            .take()
            .ok_or_else(|| CommandError::Rejected("nothing captured to restore".into()))?;
        project
            .sequence_mut(self.sequence)
            .ok_or(CommandError::SequenceNotFound(self.sequence))?
            .add_marker(marker);
        Ok(())
    }

    fn as_any(&self) -> &dyn Any {
        self
    }
}

/// Toggles a clip's enabled flag.
#[derive(Debug)]
pub struct SetClipEnabled {
    sequence: SequenceId,
    clip: ClipId,
    enabled: bool,
    previous: Option<bool>,
}

impl SetClipEnabled {
    pub fn new(sequence: SequenceId, clip: ClipId, enabled: bool) -> Self {
        SetClipEnabled { sequence, clip, enabled, previous: None }
    }
}

impl Command for SetClipEnabled {
    fn name(&self) -> &str {
        if self.enabled {
            "Enable Clip"
        } else {
            "Disable Clip"
        }
    }

    fn apply(&mut self, project: &mut Project) -> Result<(), CommandError> {
        let clip = clip_mut(project, self.sequence, self.clip)?;
        self.previous.get_or_insert(clip.enabled);
        clip.enabled = self.enabled;
        Ok(())
    }

    fn undo(&mut self, project: &mut Project) -> Result<(), CommandError> {
        let previous =
            self.previous.ok_or_else(|| CommandError::Rejected("never applied".into()))?;
        clip_mut(project, self.sequence, self.clip)?.enabled = previous;
        Ok(())
    }

    fn as_any(&self) -> &dyn Any {
        self
    }
}

/// Sets a clip's blend mode.
///
/// Discrete rather than animatable, so it is its own command rather than a
/// [`crate::SetClipProperty`]: a blend mode has no in-between values to
/// interpolate, and a keyframe on one would mean nothing.
#[derive(Debug)]
pub struct SetClipBlendMode {
    sequence: SequenceId,
    clip: ClipId,
    blend: BlendMode,
    previous: Option<BlendMode>,
}

impl SetClipBlendMode {
    pub fn new(sequence: SequenceId, clip: ClipId, blend: BlendMode) -> Self {
        SetClipBlendMode { sequence, clip, blend, previous: None }
    }
}

impl Command for SetClipBlendMode {
    fn name(&self) -> &str {
        "Set Blend Mode"
    }

    fn apply(&mut self, project: &mut Project) -> Result<(), CommandError> {
        let clip = clip_mut(project, self.sequence, self.clip)?;
        self.previous.get_or_insert(clip.blend);
        clip.blend = self.blend;
        Ok(())
    }

    fn undo(&mut self, project: &mut Project) -> Result<(), CommandError> {
        let previous =
            self.previous.ok_or_else(|| CommandError::Rejected("never applied".into()))?;
        clip_mut(project, self.sequence, self.clip)?.blend = previous;
        Ok(())
    }

    /// Cycling through modes in the inspector is one decision, not one per mode
    /// tried, so a later change to the same clip absorbs the earlier one and
    /// keeps the mode it started from.
    fn merge(&mut self, next: &dyn Command) -> bool {
        match next.as_any().downcast_ref::<SetClipBlendMode>() {
            Some(next) if next.clip == self.clip && next.sequence == self.sequence => {
                self.blend = next.blend;
                true
            }
            _ => false,
        }
    }

    fn as_any(&self) -> &dyn Any {
        self
    }
}

/// Changes a sequence's resolution, frame rate and audio format.
///
/// Used when the first clip lands on an empty sequence: adopting the media's
/// own format means a 4K clip is not letterboxed into a 1080p frame, and a
/// small clip is not stranded in the middle of a large one. It is a command
/// rather than a silent mutation so that undoing the first edit restores the
/// project exactly, including the format it had.
#[derive(Debug)]
pub struct SetSequenceFormat {
    sequence: SequenceId,
    resolution: ve_core::Size,
    rate: ve_time::Rate,
    sample_rate: Option<ve_time::SampleRate>,
    previous: Option<(ve_core::Size, ve_time::Rate, ve_time::SampleRate)>,
}

impl SetSequenceFormat {
    pub fn new(
        sequence: SequenceId,
        resolution: ve_core::Size,
        rate: ve_time::Rate,
        sample_rate: Option<ve_time::SampleRate>,
    ) -> Self {
        SetSequenceFormat { sequence, resolution, rate, sample_rate, previous: None }
    }

    /// Whether this would actually change anything, so a no-op never reaches
    /// the undo stack.
    pub fn would_change(&self, project: &Project) -> bool {
        match project.sequence(self.sequence) {
            Some(seq) => {
                seq.settings.resolution != self.resolution
                    || seq.settings.rate != self.rate
                    || self.sample_rate.is_some_and(|sr| seq.settings.sample_rate != sr)
            }
            None => false,
        }
    }
}

impl Command for SetSequenceFormat {
    fn name(&self) -> &str {
        "Set Sequence Format"
    }

    fn apply(&mut self, project: &mut Project) -> Result<(), CommandError> {
        let seq = project
            .sequence_mut(self.sequence)
            .ok_or(CommandError::SequenceNotFound(self.sequence))?;
        self.previous.get_or_insert((
            seq.settings.resolution,
            seq.settings.rate,
            seq.settings.sample_rate,
        ));
        seq.settings.resolution = self.resolution;
        seq.settings.rate = self.rate;
        if let Some(sample_rate) = self.sample_rate {
            seq.settings.sample_rate = sample_rate;
        }
        // The playhead was snapped to the old frame grid and may now sit
        // between frames.
        seq.playhead = seq.settings.rate.snap_round(seq.playhead);
        Ok(())
    }

    fn undo(&mut self, project: &mut Project) -> Result<(), CommandError> {
        let (resolution, rate, sample_rate) = self
            .previous
            .ok_or_else(|| CommandError::Rejected("format was never set".into()))?;
        let seq = project
            .sequence_mut(self.sequence)
            .ok_or(CommandError::SequenceNotFound(self.sequence))?;
        seq.settings.resolution = resolution;
        seq.settings.rate = rate;
        seq.settings.sample_rate = sample_rate;
        seq.playhead = rate.snap_round(seq.playhead);
        Ok(())
    }

    fn as_any(&self) -> &dyn Any {
        self
    }
}

/// Changes how a sequence combines its layers.
///
/// Its own command rather than a field on [`SetSequenceFormat`], because that
/// one exists to adopt a clip's format on first drop and runs without anyone
/// asking. Colour space is always a deliberate choice, and it changes every
/// dissolve and every semi-transparent layer in the sequence at once, so it
/// deserves its own entry in the undo menu.
#[derive(Debug)]
pub struct SetSequenceColorSpace {
    sequence: SequenceId,
    color_space: ve_core::ColorSpace,
    previous: Option<ve_core::ColorSpace>,
}

impl SetSequenceColorSpace {
    pub fn new(sequence: SequenceId, color_space: ve_core::ColorSpace) -> Self {
        SetSequenceColorSpace { sequence, color_space, previous: None }
    }
}

impl Command for SetSequenceColorSpace {
    fn name(&self) -> &str {
        "Set Colour Space"
    }

    fn apply(&mut self, project: &mut Project) -> Result<(), CommandError> {
        let seq = project
            .sequence_mut(self.sequence)
            .ok_or(CommandError::SequenceNotFound(self.sequence))?;
        self.previous.get_or_insert(seq.settings.color_space);
        seq.settings.color_space = self.color_space;
        Ok(())
    }

    fn undo(&mut self, project: &mut Project) -> Result<(), CommandError> {
        let previous =
            self.previous.ok_or_else(|| CommandError::Rejected("never applied".into()))?;
        project
            .sequence_mut(self.sequence)
            .ok_or(CommandError::SequenceNotFound(self.sequence))?
            .settings
            .color_space = previous;
        Ok(())
    }

    fn as_any(&self) -> &dyn Any {
        self
    }
}
