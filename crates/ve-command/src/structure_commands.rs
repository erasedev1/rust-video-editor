//! Commands that change a sequence's structure: tracks, markers, clip state.

use std::any::Any;

use ve_core::{ClipId, Marker, MarkerId, Project, SequenceId, Track, TrackId, TrackKind};

use crate::{Command, CommandError};

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
        let index = seq.track_index(self.track_id).ok_or(CommandError::TrackNotFound(self.track_id))?;
        let track = seq.remove_track(self.track_id).ok_or(CommandError::TrackNotFound(self.track_id))?;
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
        let clip = project
            .sequence_mut(self.sequence)
            .ok_or(CommandError::SequenceNotFound(self.sequence))?
            .find_clip_mut(self.clip)
            .map(|(_, c)| c)
            .ok_or(CommandError::ClipNotFound(self.clip))?;
        self.previous.get_or_insert(clip.enabled);
        clip.enabled = self.enabled;
        Ok(())
    }

    fn undo(&mut self, project: &mut Project) -> Result<(), CommandError> {
        let previous = self
            .previous
            .ok_or_else(|| CommandError::Rejected("never applied".into()))?;
        let clip = project
            .sequence_mut(self.sequence)
            .ok_or(CommandError::SequenceNotFound(self.sequence))?
            .find_clip_mut(self.clip)
            .map(|(_, c)| c)
            .ok_or(CommandError::ClipNotFound(self.clip))?;
        clip.enabled = previous;
        Ok(())
    }

    fn as_any(&self) -> &dyn Any {
        self
    }
}
