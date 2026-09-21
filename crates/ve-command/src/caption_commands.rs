//! Commands for caption tracks and the cues on them.
//!
//! # Every caption edit states an absolute destination
//!
//! The same rule the keyframe commands follow, for the same reason. A cue drag
//! re-issues its command on every pointer move and the history merges them into
//! one entry, which only works if applying the same edit twice lands in the same
//! place: "this cue now runs from here to here", never "move it 40 ms left".
//!
//! Undo restores what was captured rather than replaying an inverse, so a cue
//! that was trimmed by its neighbour on the way in comes back as it was.

use std::any::Any;

use ve_core::{CaptionTrack, CaptionTrackId, Cue, CueId, Project, SequenceId};
use ve_time::{Ticks, TimeRange};

use crate::{Command, CommandError};

/// Resolves a caption track for mutation.
fn captions_mut(
    project: &mut Project,
    sequence: SequenceId,
    track: CaptionTrackId,
) -> Result<&mut CaptionTrack, CommandError> {
    project
        .sequence_mut(sequence)
        .ok_or(CommandError::SequenceNotFound(sequence))?
        .caption_track_mut(track)
        .ok_or(CommandError::CaptionTrackNotFound(track))
}

/// Adds an empty caption track.
#[derive(Debug)]
pub struct AddCaptionTrack {
    sequence: SequenceId,
    language: Option<String>,
    /// Minted on the first apply and reused on redo, so a selection or an
    /// import pointing at the track survives an undo/redo cycle.
    track_id: Option<CaptionTrackId>,
}

impl AddCaptionTrack {
    pub fn new(sequence: SequenceId) -> Self {
        AddCaptionTrack { sequence, language: None, track_id: None }
    }

    /// Tags the new track, as an import from `movie.pt-BR.srt` does.
    pub fn with_language(mut self, language: impl Into<String>) -> Self {
        self.language = Some(language.into());
        self
    }

    pub fn track_id(&self) -> Option<CaptionTrackId> {
        self.track_id
    }
}

impl Command for AddCaptionTrack {
    fn name(&self) -> &str {
        "Add Caption Track"
    }

    fn apply(&mut self, project: &mut Project) -> Result<(), CommandError> {
        let id = match self.track_id {
            Some(id) => id,
            None => {
                let id = project.new_caption_track_id();
                self.track_id = Some(id);
                id
            }
        };
        let language = self.language.clone();
        let sequence = project
            .sequence_mut(self.sequence)
            .ok_or(CommandError::SequenceNotFound(self.sequence))?;
        sequence.add_caption_track(id);
        if let Some(language) = language {
            if let Some(track) = sequence.caption_track_mut(id) {
                track.language = language;
            }
        }
        Ok(())
    }

    fn undo(&mut self, project: &mut Project) -> Result<(), CommandError> {
        let id = self
            .track_id
            .ok_or_else(|| CommandError::Rejected("track was never added".into()))?;
        project
            .sequence_mut(self.sequence)
            .ok_or(CommandError::SequenceNotFound(self.sequence))?
            .remove_caption_track(id)
            .ok_or(CommandError::CaptionTrackNotFound(id))?;
        Ok(())
    }

    fn as_any(&self) -> &dyn Any {
        self
    }
}

/// Deletes a caption track and every cue on it.
#[derive(Debug)]
pub struct RemoveCaptionTrack {
    sequence: SequenceId,
    track: CaptionTrackId,
    /// The whole track, so undo restores an hour of transcription intact and in
    /// the slot it came from.
    removed: Option<(usize, CaptionTrack)>,
}

impl RemoveCaptionTrack {
    pub fn new(sequence: SequenceId, track: CaptionTrackId) -> Self {
        RemoveCaptionTrack { sequence, track, removed: None }
    }
}

impl Command for RemoveCaptionTrack {
    fn name(&self) -> &str {
        "Delete Caption Track"
    }

    fn apply(&mut self, project: &mut Project) -> Result<(), CommandError> {
        let sequence = project
            .sequence_mut(self.sequence)
            .ok_or(CommandError::SequenceNotFound(self.sequence))?;
        let index = sequence
            .caption_track_index(self.track)
            .ok_or(CommandError::CaptionTrackNotFound(self.track))?;
        let track = sequence
            .remove_caption_track(self.track)
            .ok_or(CommandError::CaptionTrackNotFound(self.track))?;
        self.removed = Some((index, track));
        Ok(())
    }

    fn undo(&mut self, project: &mut Project) -> Result<(), CommandError> {
        let (index, track) = self
            .removed
            .take()
            .ok_or_else(|| CommandError::Rejected("nothing captured to restore".into()))?;
        let sequence = project
            .sequence_mut(self.sequence)
            .ok_or(CommandError::SequenceNotFound(self.sequence))?;
        let index = index.min(sequence.captions.len());
        sequence.captions.insert(index, track);
        Ok(())
    }

    fn as_any(&self) -> &dyn Any {
        self
    }
}

/// Retags a caption track's language.
#[derive(Debug)]
pub struct SetCaptionLanguage {
    sequence: SequenceId,
    track: CaptionTrackId,
    language: String,
    previous: Option<String>,
}

impl SetCaptionLanguage {
    pub fn new(
        sequence: SequenceId,
        track: CaptionTrackId,
        language: impl Into<String>,
    ) -> Self {
        SetCaptionLanguage { sequence, track, language: language.into(), previous: None }
    }
}

impl Command for SetCaptionLanguage {
    fn name(&self) -> &str {
        "Set Caption Language"
    }

    fn apply(&mut self, project: &mut Project) -> Result<(), CommandError> {
        let track = captions_mut(project, self.sequence, self.track)?;
        if self.previous.is_none() {
            self.previous = Some(track.language.clone());
        }
        track.language = self.language.clone();
        Ok(())
    }

    fn undo(&mut self, project: &mut Project) -> Result<(), CommandError> {
        let previous = self
            .previous
            .clone()
            .ok_or_else(|| CommandError::Rejected("nothing captured to restore".into()))?;
        captions_mut(project, self.sequence, self.track)?.language = previous;
        Ok(())
    }

    /// Typing in the language box is one edit, not one per keystroke.
    fn merge(&mut self, next: &dyn Command) -> bool {
        match next.as_any().downcast_ref::<SetCaptionLanguage>() {
            Some(next) if next.track == self.track && next.sequence == self.sequence => {
                self.language = next.language.clone();
                true
            }
            _ => false,
        }
    }

    fn as_any(&self) -> &dyn Any {
        self
    }
}

/// Puts a new cue on a caption track.
#[derive(Debug)]
pub struct AddCue {
    sequence: SequenceId,
    track: CaptionTrackId,
    span: TimeRange,
    text: String,
    cue_id: Option<CueId>,
}

impl AddCue {
    pub fn new(
        sequence: SequenceId,
        track: CaptionTrackId,
        span: TimeRange,
        text: impl Into<String>,
    ) -> Self {
        AddCue { sequence, track, span, text: text.into(), cue_id: None }
    }

    pub fn cue_id(&self) -> Option<CueId> {
        self.cue_id
    }
}

impl Command for AddCue {
    fn name(&self) -> &str {
        "Add Caption"
    }

    fn apply(&mut self, project: &mut Project) -> Result<(), CommandError> {
        let id = match self.cue_id {
            Some(id) => id,
            None => {
                let id = project.new_cue_id();
                self.cue_id = Some(id);
                id
            }
        };
        let cue = Cue::new(id, self.span.start, self.span.duration, self.text.clone());
        captions_mut(project, self.sequence, self.track)?.insert_cue(cue)?;
        Ok(())
    }

    fn undo(&mut self, project: &mut Project) -> Result<(), CommandError> {
        let id =
            self.cue_id.ok_or_else(|| CommandError::Rejected("cue was never added".into()))?;
        captions_mut(project, self.sequence, self.track)?.remove_cue(id)?;
        Ok(())
    }

    fn as_any(&self) -> &dyn Any {
        self
    }
}

/// Takes a cue off a caption track.
#[derive(Debug)]
pub struct RemoveCue {
    sequence: SequenceId,
    track: CaptionTrackId,
    cue: CueId,
    removed: Option<Cue>,
}

impl RemoveCue {
    pub fn new(sequence: SequenceId, track: CaptionTrackId, cue: CueId) -> Self {
        RemoveCue { sequence, track, cue, removed: None }
    }
}

impl Command for RemoveCue {
    fn name(&self) -> &str {
        "Delete Caption"
    }

    fn apply(&mut self, project: &mut Project) -> Result<(), CommandError> {
        self.removed =
            Some(captions_mut(project, self.sequence, self.track)?.remove_cue(self.cue)?);
        Ok(())
    }

    fn undo(&mut self, project: &mut Project) -> Result<(), CommandError> {
        let cue = self
            .removed
            .take()
            .ok_or_else(|| CommandError::Rejected("nothing captured to restore".into()))?;
        captions_mut(project, self.sequence, self.track)?.insert_cue(cue)?;
        Ok(())
    }

    fn as_any(&self) -> &dyn Any {
        self
    }
}

/// Rewrites a cue's text.
#[derive(Debug)]
pub struct SetCueText {
    sequence: SequenceId,
    track: CaptionTrackId,
    cue: CueId,
    text: String,
    previous: Option<String>,
}

impl SetCueText {
    pub fn new(
        sequence: SequenceId,
        track: CaptionTrackId,
        cue: CueId,
        text: impl Into<String>,
    ) -> Self {
        SetCueText { sequence, track, cue, text: text.into(), previous: None }
    }
}

impl Command for SetCueText {
    fn name(&self) -> &str {
        "Edit Caption"
    }

    fn apply(&mut self, project: &mut Project) -> Result<(), CommandError> {
        let track = captions_mut(project, self.sequence, self.track)?;
        let cue = track.cue_mut(self.cue).ok_or(CommandError::CueNotFound(self.cue))?;
        if self.previous.is_none() {
            self.previous = Some(cue.text.clone());
        }
        cue.set_text(self.text.clone());
        Ok(())
    }

    fn undo(&mut self, project: &mut Project) -> Result<(), CommandError> {
        let previous = self
            .previous
            .clone()
            .ok_or_else(|| CommandError::Rejected("nothing captured to restore".into()))?;
        let track = captions_mut(project, self.sequence, self.track)?;
        track.cue_mut(self.cue).ok_or(CommandError::CueNotFound(self.cue))?.set_text(previous);
        Ok(())
    }

    /// Typing is one edit. Without this, undo after writing a line of dialogue
    /// would take it back a character at a time.
    fn merge(&mut self, next: &dyn Command) -> bool {
        match next.as_any().downcast_ref::<SetCueText>() {
            Some(next) if next.cue == self.cue && next.sequence == self.sequence => {
                self.text = next.text.clone();
                true
            }
            _ => false,
        }
    }

    fn as_any(&self) -> &dyn Any {
        self
    }
}

/// Which gesture a [`RetimeCue`] came from. Only the undo label differs: both
/// state a span, because both are the same edit from the model's point of view.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CueGesture {
    Move,
    Trim,
}

/// Puts a cue's span where it now is.
///
/// One command for dragging a cue and for dragging either edge of one. A cue
/// has no source media to run out of, so moving an edge is only ever a question
/// of what the neighbours allow — there is nothing a trim has to know that a
/// move does not, and two commands would be the same code twice.
#[derive(Debug)]
pub struct RetimeCue {
    sequence: SequenceId,
    track: CaptionTrackId,
    cue: CueId,
    span: TimeRange,
    gesture: CueGesture,
    was: Option<TimeRange>,
}

impl RetimeCue {
    pub fn moving(
        sequence: SequenceId,
        track: CaptionTrackId,
        cue: CueId,
        start: Ticks,
        duration: Ticks,
    ) -> Self {
        RetimeCue {
            sequence,
            track,
            cue,
            span: TimeRange::new(start, duration),
            gesture: CueGesture::Move,
            was: None,
        }
    }

    pub fn trimming(
        sequence: SequenceId,
        track: CaptionTrackId,
        cue: CueId,
        span: TimeRange,
    ) -> Self {
        RetimeCue { sequence, track, cue, span, gesture: CueGesture::Trim, was: None }
    }
}

impl Command for RetimeCue {
    fn name(&self) -> &str {
        match self.gesture {
            CueGesture::Move => "Move Caption",
            CueGesture::Trim => "Trim Caption",
        }
    }

    fn apply(&mut self, project: &mut Project) -> Result<(), CommandError> {
        let track = captions_mut(project, self.sequence, self.track)?;
        let was = track.retime_cue(self.cue, self.span)?;
        // Only on the first apply: a merged drag has to undo to where the
        // gesture started, not to where the previous pointer move left it.
        self.was.get_or_insert(was);
        Ok(())
    }

    fn undo(&mut self, project: &mut Project) -> Result<(), CommandError> {
        let was = self
            .was
            .ok_or_else(|| CommandError::Rejected("nothing captured to restore".into()))?;
        captions_mut(project, self.sequence, self.track)?.retime_cue(self.cue, was)?;
        Ok(())
    }

    fn merge(&mut self, next: &dyn Command) -> bool {
        match next.as_any().downcast_ref::<RetimeCue>() {
            Some(next)
                if next.cue == self.cue
                    && next.sequence == self.sequence
                    && next.gesture == self.gesture =>
            {
                self.span = next.span;
                true
            }
            _ => false,
        }
    }

    fn as_any(&self) -> &dyn Any {
        self
    }
}

/// Replaces every cue on a track, as importing a caption file does.
///
/// One command rather than one per cue: a transcription is a few thousand cues,
/// and an undo stack with a few thousand entries in it after one import would
/// be unusable — and undoing the import would take a few thousand keystrokes.
#[derive(Debug)]
pub struct ReplaceCues {
    sequence: SequenceId,
    track: CaptionTrackId,
    cues: Vec<Cue>,
    label: &'static str,
    previous: Option<Vec<Cue>>,
}

impl ReplaceCues {
    pub fn new(sequence: SequenceId, track: CaptionTrackId, cues: Vec<Cue>) -> Self {
        ReplaceCues { sequence, track, cues, label: "Import Captions", previous: None }
    }

    pub fn with_label(mut self, label: &'static str) -> Self {
        self.label = label;
        self
    }
}

impl Command for ReplaceCues {
    fn name(&self) -> &str {
        self.label
    }

    fn apply(&mut self, project: &mut Project) -> Result<(), CommandError> {
        let track = captions_mut(project, self.sequence, self.track)?;
        let previous = track.replace_cues(self.cues.clone());
        // Captured once. On redo the same cues go back, and what was there
        // before the *first* apply is what undo has to restore.
        self.previous.get_or_insert(previous);
        Ok(())
    }

    fn undo(&mut self, project: &mut Project) -> Result<(), CommandError> {
        let previous = self
            .previous
            .clone()
            .ok_or_else(|| CommandError::Rejected("nothing captured to restore".into()))?;
        captions_mut(project, self.sequence, self.track)?.replace_cues(previous);
        Ok(())
    }

    fn as_any(&self) -> &dyn Any {
        self
    }
}
