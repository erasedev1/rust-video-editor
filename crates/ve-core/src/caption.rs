//! Captions: text with a span, on tracks of their own.
//!
//! # Why a cue is not a clip
//!
//! A [`Clip`](crate::Clip) is a *window onto a source*, placed in time. A cue
//! is text that appears and goes away. Written as a clip it would arrive with a
//! `source_in`, a `speed`, a transform, a blend mode, an effect chain and a
//! pair of audio properties, and every one of them would need a rule saying it
//! means nothing here. So captions get their own small type, and the parts of
//! the editor that walk clips are not made to understand a clip that is not
//! one.
//!
//! What is kept from the clip model is the **invariant**, because it is the
//! invariant rather than the payload that makes a track fast and predictable:
//! cues are sorted by start and never overlap, so "what is on screen at this
//! instant" is a binary search and cannot have two answers.
//!
//! # Two captions at one moment
//!
//! Two people talking at once is one cue of two lines — `- Who's there?` over
//! `- It's me.` — which is what every captioning standard asks for and what a
//! reader can actually follow. It is not two cues stacked on top of each other,
//! and the non-overlap invariant says so rather than leaving it to convention.
//!
//! What a second caption *track* is for is a second **language**: a track
//! carries a language tag, and each one is written out as its own file.

use serde::{Deserialize, Serialize};
use ve_time::{Ticks, TimeRange};

use crate::id::{CaptionTrackId, CueId};
use crate::CoreError;

/// A line or two of text, shown over a span of the timeline.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Cue {
    pub id: CueId,
    pub start: Ticks,
    pub duration: Ticks,
    /// The caption itself. Newlines separate the lines as they are shown, which
    /// is the one piece of formatting both SubRip and WebVTT agree on.
    pub text: String,
}

impl Cue {
    pub fn new(id: CueId, start: Ticks, duration: Ticks, text: impl Into<String>) -> Self {
        Cue { id, start, duration, text: normalise_text(text.into()) }
    }

    #[inline]
    pub fn range(&self) -> TimeRange {
        TimeRange::new(self.start, self.duration)
    }

    #[inline]
    pub fn end(&self) -> Ticks {
        self.start + self.duration
    }

    /// The lines as they are drawn, top to bottom.
    pub fn lines(&self) -> impl Iterator<Item = &str> {
        self.text.lines()
    }

    /// Whether this cue would show nothing.
    ///
    /// A cue is allowed to be blank in the model — one is blank for as long as
    /// it takes to type into it — but a writer skips it, because neither
    /// caption format has a representation for "a cue with no text" that a
    /// player will read back.
    pub fn is_blank(&self) -> bool {
        self.text.trim().is_empty()
    }

    /// Replaces the text, normalising line endings.
    pub fn set_text(&mut self, text: impl Into<String>) {
        self.text = normalise_text(text.into());
    }
}

/// Line endings as the model holds them: `\n`, always.
///
/// Caption files in the wild arrive with CRLF as often as not — SubRip's own
/// specification is a Windows format — and a `\r` left on the end of a line
/// would be drawn as a box by the text shaper and written back out doubled.
fn normalise_text(text: String) -> String {
    if text.contains('\r') {
        text.replace("\r\n", "\n").replace('\r', "\n")
    } else {
        text
    }
}

/// One language's worth of captions: a lane of cues that do not overlap.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CaptionTrack {
    pub id: CaptionTrackId,
    pub name: String,
    /// BCP 47 language tag — `en`, `pt-BR`. Written into the name of the file
    /// this track exports to, which is how a player finds the right one, so it
    /// is data rather than a label.
    pub language: String,
    cues: Vec<Cue>,
}

/// What a track is called and tagged when the user has not said.
pub const DEFAULT_LANGUAGE: &str = "en";

impl CaptionTrack {
    pub fn new(id: CaptionTrackId, name: impl Into<String>) -> Self {
        CaptionTrack {
            id,
            name: name.into(),
            language: DEFAULT_LANGUAGE.to_string(),
            cues: Vec::new(),
        }
    }

    pub fn with_language(mut self, language: impl Into<String>) -> Self {
        self.language = language.into();
        self
    }

    #[inline]
    pub fn cues(&self) -> &[Cue] {
        &self.cues
    }

    #[inline]
    pub fn is_empty(&self) -> bool {
        self.cues.is_empty()
    }

    #[inline]
    pub fn len(&self) -> usize {
        self.cues.len()
    }

    /// End of the last cue.
    pub fn content_end(&self) -> Ticks {
        self.cues.last().map(|c| c.end()).unwrap_or(Ticks::ZERO)
    }

    pub fn cue(&self, id: CueId) -> Option<&Cue> {
        self.cues.iter().find(|c| c.id == id)
    }

    /// Mutable access by ID.
    ///
    /// Callers must not change `start` or `duration` through this handle; that
    /// is [`CaptionTrack::move_cue`] and [`CaptionTrack::retime_cue`], which
    /// keep the sort and overlap invariants. Editing the **text** through here
    /// is exactly what it is for.
    pub fn cue_mut(&mut self, id: CueId) -> Option<&mut Cue> {
        self.cues.iter_mut().find(|c| c.id == id)
    }

    pub fn index_of(&self, id: CueId) -> Option<usize> {
        self.cues.iter().position(|c| c.id == id)
    }

    /// The cue covering `t`, found by binary search.
    ///
    /// This runs once per drawn frame while the preview is playing, which is
    /// why it is a search rather than a scan: an hour of dialogue is a few
    /// thousand cues.
    pub fn cue_at(&self, t: Ticks) -> Option<&Cue> {
        let idx = self.cues.partition_point(|c| c.start <= t);
        let candidate = self.cues.get(idx.checked_sub(1)?)?;
        candidate.range().contains(t).then_some(candidate)
    }

    /// Every cue intersecting `range`, in order.
    pub fn cues_in_range(&self, range: TimeRange) -> impl Iterator<Item = &Cue> {
        let start = self.cues.partition_point(|c| c.end() <= range.start);
        self.cues[start..].iter().take_while(move |c| c.start < range.end())
    }

    /// Whether `range` is free, ignoring the cue identified by `ignore`.
    pub fn is_range_free(&self, range: TimeRange, ignore: Option<CueId>) -> bool {
        !self.cues_in_range(range).any(|c| Some(c.id) != ignore)
    }

    /// Inserts a cue, refusing the edit if it would overlap another.
    pub fn insert_cue(&mut self, cue: Cue) -> Result<(), CoreError> {
        if cue.duration.raw() <= 0 {
            return Err(CoreError::EmptyCue);
        }
        if !self.is_range_free(cue.range(), None) {
            return Err(CoreError::CueOverlap);
        }
        let idx = self.cues.partition_point(|c| c.start <= cue.start);
        self.cues.insert(idx, cue);
        debug_assert!(self.invariants_hold());
        Ok(())
    }

    pub fn remove_cue(&mut self, id: CueId) -> Result<Cue, CoreError> {
        let idx = self.index_of(id).ok_or(CoreError::CueNotFound(id))?;
        Ok(self.cues.remove(idx))
    }

    /// Moves a cue without changing how long it lasts. Returns where it was.
    pub fn move_cue(&mut self, id: CueId, start: Ticks) -> Result<Ticks, CoreError> {
        let idx = self.index_of(id).ok_or(CoreError::CueNotFound(id))?;
        let start = start.clamp_non_negative();
        let was = self.cues[idx].start;
        if !self.is_range_free(TimeRange::new(start, self.cues[idx].duration), Some(id)) {
            return Err(CoreError::CueOverlap);
        }
        self.cues[idx].start = start;
        self.resort_from(idx);
        debug_assert!(self.invariants_hold());
        Ok(was)
    }

    /// Sets a cue's span outright. Returns the span it had.
    ///
    /// One call for both edges rather than a trim per end: a cue has no source
    /// to run out of, so moving an edge is only ever a question of what the
    /// neighbours allow, and stating the whole span makes an undo a restore
    /// rather than an inverse.
    pub fn retime_cue(&mut self, id: CueId, span: TimeRange) -> Result<TimeRange, CoreError> {
        let idx = self.index_of(id).ok_or(CoreError::CueNotFound(id))?;
        if span.duration.raw() <= 0 {
            return Err(CoreError::EmptyCue);
        }
        let span = TimeRange::new(span.start.clamp_non_negative(), span.duration);
        let was = self.cues[idx].range();
        if !self.is_range_free(span, Some(id)) {
            return Err(CoreError::CueOverlap);
        }
        self.cues[idx].start = span.start;
        self.cues[idx].duration = span.duration;
        self.resort_from(idx);
        debug_assert!(self.invariants_hold());
        Ok(was)
    }

    /// Shifts every cue starting at or after `from`, the way a ripple shifts
    /// clips. Returns how many moved.
    pub fn shift_cues_from(&mut self, from: Ticks, delta: Ticks) -> Result<usize, CoreError> {
        let first = self.cues.partition_point(|c| c.start < from);
        if delta.is_zero() || first == self.cues.len() {
            return Ok(0);
        }
        if delta.is_negative() {
            let limit = if first == 0 { Ticks::ZERO } else { self.cues[first - 1].end() };
            if self.cues[first].start + delta < limit {
                return Err(CoreError::CueOverlap);
            }
        }
        for cue in &mut self.cues[first..] {
            cue.start += delta;
        }
        debug_assert!(self.invariants_hold());
        Ok(self.cues.len() - first)
    }

    /// Replaces every cue on the track, as an import does.
    ///
    /// Returns what was there, so the command layer can put it back. Cues
    /// arrive sorted and overlap-free or not at all: a file is outside data,
    /// and [`CaptionTrack::normalise`] is what it is held to.
    pub fn replace_cues(&mut self, cues: Vec<Cue>) -> Vec<Cue> {
        let previous = std::mem::replace(&mut self.cues, cues);
        self.normalise();
        previous
    }

    /// Restores sort order after one element moved.
    fn resort_from(&mut self, idx: usize) {
        let mut i = idx;
        while i > 0 && self.cues[i - 1].start > self.cues[i].start {
            self.cues.swap(i - 1, i);
            i -= 1;
        }
        while i + 1 < self.cues.len() && self.cues[i + 1].start < self.cues[i].start {
            self.cues.swap(i, i + 1);
            i += 1;
        }
    }

    /// Sorts, and shortens any cue that runs into the next one. Returns the
    /// cues that were trimmed.
    ///
    /// Overlap is **repaired** here rather than reported, which is the opposite
    /// of what loading a track of clips does, and deliberately: an overlapping
    /// clip is a broken edit, while an overlapping cue is an ordinary caption
    /// file. Subtitles written by hand routinely run one cue a few milliseconds
    /// into the next, and every player resolves it by cutting the earlier one
    /// short. Refusing the file, or dropping the cue, would lose a line of
    /// dialogue over a rounding error someone made in another program.
    pub fn normalise(&mut self) -> Vec<CueId> {
        self.cues.sort_by_key(|c| c.start);
        self.cues.retain(|c| c.duration.raw() > 0);
        let mut trimmed = Vec::new();
        for i in 1..self.cues.len() {
            let next_start = self.cues[i].start;
            let previous = &mut self.cues[i - 1];
            if previous.end() > next_start {
                previous.duration = (next_start - previous.start).clamp_non_negative();
                trimmed.push(previous.id);
            }
        }
        // A cue cut down to nothing by the one after it starting at the same
        // instant has no span left to show text in.
        self.cues.retain(|c| c.duration.raw() > 0);
        debug_assert!(self.invariants_hold());
        trimmed
    }

    pub fn invariants_hold(&self) -> bool {
        self.cues.windows(2).all(|w| w[0].end() <= w[1].start)
            && self.cues.iter().all(|c| c.duration.raw() > 0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn track() -> CaptionTrack {
        CaptionTrack::new(CaptionTrackId::from_raw(1), "Captions")
    }

    fn cue(raw: u64, start_ms: i64, length_ms: i64, text: &str) -> Cue {
        Cue::new(
            CueId::from_raw(raw),
            Ticks::from_millis(start_ms),
            Ticks::from_millis(length_ms),
            text,
        )
    }

    #[test]
    fn cues_are_kept_in_order_however_they_arrive() {
        let mut track = track();
        track.insert_cue(cue(3, 4000, 1000, "third")).unwrap();
        track.insert_cue(cue(1, 0, 1000, "first")).unwrap();
        track.insert_cue(cue(2, 2000, 1000, "second")).unwrap();
        let texts: Vec<&str> = track.cues().iter().map(|c| c.text.as_str()).collect();
        assert_eq!(texts, ["first", "second", "third"]);
        assert!(track.invariants_hold());
    }

    #[test]
    fn a_cue_may_not_be_laid_over_another() {
        let mut track = track();
        track.insert_cue(cue(1, 1000, 1000, "one")).unwrap();
        assert_eq!(track.insert_cue(cue(2, 1500, 1000, "two")), Err(CoreError::CueOverlap));
        // Butting up against it is not overlapping it.
        track.insert_cue(cue(3, 2000, 1000, "two")).unwrap();
        assert_eq!(track.len(), 2);
    }

    #[test]
    fn what_is_on_screen_is_one_cue_or_none() {
        let mut track = track();
        track.insert_cue(cue(1, 1000, 1000, "one")).unwrap();
        track.insert_cue(cue(2, 3000, 1000, "two")).unwrap();

        assert_eq!(track.cue_at(Ticks::from_millis(500)), None);
        assert_eq!(track.cue_at(Ticks::from_millis(1000)).unwrap().text, "one");
        assert_eq!(track.cue_at(Ticks::from_millis(1999)).unwrap().text, "one");
        // Half-open: the instant a cue ends belongs to what comes next.
        assert_eq!(track.cue_at(Ticks::from_millis(2000)), None);
        assert_eq!(track.cue_at(Ticks::from_millis(3500)).unwrap().text, "two");
    }

    #[test]
    fn a_cue_moved_over_its_neighbour_is_refused_and_nothing_changes() {
        let mut track = track();
        track.insert_cue(cue(1, 0, 1000, "one")).unwrap();
        track.insert_cue(cue(2, 2000, 1000, "two")).unwrap();
        let before = track.clone();
        assert_eq!(
            track.move_cue(CueId::from_raw(2), Ticks::from_millis(500)),
            Err(CoreError::CueOverlap)
        );
        assert_eq!(track, before);
    }

    #[test]
    fn a_cue_moved_past_its_neighbour_lands_in_the_right_place_in_the_list() {
        let mut track = track();
        track.insert_cue(cue(1, 0, 1000, "one")).unwrap();
        track.insert_cue(cue(2, 2000, 1000, "two")).unwrap();
        let was = track.move_cue(CueId::from_raw(1), Ticks::from_millis(4000)).unwrap();
        assert_eq!(was, Ticks::ZERO);
        let texts: Vec<&str> = track.cues().iter().map(|c| c.text.as_str()).collect();
        assert_eq!(texts, ["two", "one"]);
        assert!(track.invariants_hold());
    }

    #[test]
    fn an_overlap_from_a_file_shortens_the_earlier_cue_rather_than_losing_it() {
        let mut track = track();
        // What a hand-written subtitle file routinely looks like: the first cue
        // runs 200 ms into the second.
        track.replace_cues(vec![
            cue(1, 0, 1200, "one"),
            cue(2, 1000, 1000, "two"),
            cue(3, 500, 100, "written out of order"),
        ]);
        assert_eq!(track.len(), 3);
        let spans: Vec<(i64, i64)> = track
            .cues()
            .iter()
            .map(|c| (c.start.raw() / (Ticks::SECOND.raw() / 1000), c.duration.raw()))
            .collect();
        assert_eq!(spans[0].0, 0);
        assert_eq!(spans[1].0, 500);
        assert_eq!(spans[2].0, 1000);
        assert!(track.invariants_hold());
        // Nothing was dropped, and the text is all still there.
        let texts: Vec<&str> = track.cues().iter().map(|c| c.text.as_str()).collect();
        assert_eq!(texts, ["one", "written out of order", "two"]);
    }

    #[test]
    fn a_cue_swallowed_whole_by_the_next_one_is_the_one_case_that_is_dropped() {
        let mut track = track();
        // Two cues starting at the same instant: the earlier has no span left
        // once it is cut back to where the next begins.
        track.replace_cues(vec![cue(1, 1000, 500, "same instant"), cue(2, 1000, 1000, "wins")]);
        assert_eq!(track.len(), 1);
        assert!(track.invariants_hold());
    }

    #[test]
    fn line_endings_from_a_windows_file_do_not_survive_into_the_model() {
        let cue = cue(1, 0, 1000, "- Who's there?\r\n- It's me.\r");
        assert_eq!(cue.text, "- Who's there?\n- It's me.\n");
        assert_eq!(cue.lines().collect::<Vec<_>>(), ["- Who's there?", "- It's me."]);
    }

    #[test]
    fn a_ripple_carries_the_cues_after_it() {
        let mut track = track();
        track.insert_cue(cue(1, 0, 1000, "one")).unwrap();
        track.insert_cue(cue(2, 2000, 1000, "two")).unwrap();
        track.insert_cue(cue(3, 4000, 1000, "three")).unwrap();

        let moved =
            track.shift_cues_from(Ticks::from_millis(2000), Ticks::from_millis(-1000)).unwrap();
        assert_eq!(moved, 2);
        assert_eq!(track.cues()[1].start, Ticks::from_millis(1000));
        assert!(track.invariants_hold());

        // Far enough back to collide with the cue left behind: refused whole.
        let before = track.clone();
        assert_eq!(
            track.shift_cues_from(Ticks::from_millis(1000), Ticks::from_millis(-5000)),
            Err(CoreError::CueOverlap)
        );
        assert_eq!(track, before);
    }

    #[test]
    fn a_blank_cue_is_legal_in_the_model_and_says_so() {
        let mut track = track();
        track.insert_cue(cue(1, 0, 1000, "   \n ")).unwrap();
        assert!(track.cues()[0].is_blank());
        track.cue_mut(CueId::from_raw(1)).unwrap().set_text("typed");
        assert!(!track.cues()[0].is_blank());
    }
}
