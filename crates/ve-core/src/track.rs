use serde::{Deserialize, Serialize};
use ve_time::{Ticks, TimeRange};

use crate::clip::Clip;
use crate::id::{ClipId, TrackId};
use crate::CoreError;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TrackKind {
    Video,
    Audio,
}

impl TrackKind {
    pub fn prefix(self) -> &'static str {
        match self {
            TrackKind::Video => "V",
            TrackKind::Audio => "A",
        }
    }
}

/// A lane of non-overlapping clips.
///
/// The clip list is kept sorted by `timeline_start` and free of overlaps at all
/// times. Every mutating method restores that invariant or refuses the edit, so
/// lookups can binary-search and the compositor can walk tracks in order
/// without sorting first.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Track {
    pub id: TrackId,
    pub name: String,
    pub kind: TrackKind,
    #[serde(default)]
    pub muted: bool,
    #[serde(default)]
    pub locked: bool,
    #[serde(default)]
    pub solo: bool,
    /// UI row height in logical pixels. Project data because users expect their
    /// track layout to survive a reopen.
    #[serde(default = "default_track_height")]
    pub height: f32,
    clips: Vec<Clip>,
}

fn default_track_height() -> f32 {
    64.0
}

impl Track {
    pub fn new(id: TrackId, kind: TrackKind, name: impl Into<String>) -> Self {
        Track {
            id,
            name: name.into(),
            kind,
            muted: false,
            locked: false,
            solo: false,
            height: default_track_height(),
            clips: Vec::new(),
        }
    }

    #[inline]
    pub fn clips(&self) -> &[Clip] {
        &self.clips
    }

    #[inline]
    pub fn is_empty(&self) -> bool {
        self.clips.is_empty()
    }

    #[inline]
    pub fn len(&self) -> usize {
        self.clips.len()
    }

    /// End of the last clip, i.e. how much timeline this track occupies.
    pub fn content_end(&self) -> Ticks {
        self.clips.last().map(|c| c.timeline_end()).unwrap_or(Ticks::ZERO)
    }

    pub fn clip(&self, id: ClipId) -> Option<&Clip> {
        self.clips.iter().find(|c| c.id == id)
    }

    /// Mutable access by ID.
    ///
    /// Callers must not change `timeline_start` or `duration` through this
    /// handle; doing so can break the sort and overlap invariants. Use
    /// [`Track::move_clip`], [`Track::trim_clip_start`] or
    /// [`Track::trim_clip_end`] for anything positional.
    pub fn clip_mut(&mut self, id: ClipId) -> Option<&mut Clip> {
        self.clips.iter_mut().find(|c| c.id == id)
    }

    pub fn index_of(&self, id: ClipId) -> Option<usize> {
        self.clips.iter().position(|c| c.id == id)
    }

    /// The clip covering `t`, found by binary search.
    pub fn clip_at(&self, t: Ticks) -> Option<&Clip> {
        let idx = self.clips.partition_point(|c| c.timeline_start <= t);
        if idx == 0 {
            return None;
        }
        let candidate = &self.clips[idx - 1];
        candidate.range().contains(t).then_some(candidate)
    }

    /// Every clip intersecting `range`, in timeline order.
    ///
    /// Binary-searches to the first candidate rather than scanning, which is
    /// what keeps timeline drawing and compositing independent of how many
    /// clips sit off-screen.
    pub fn clips_in_range(&self, range: TimeRange) -> impl Iterator<Item = &Clip> {
        // The clip before the first one starting inside the range may still
        // overlap it, so back up one.
        let start = self.clips.partition_point(|c| c.timeline_end() <= range.start);
        self.clips[start..].iter().take_while(move |c| c.timeline_start < range.end())
    }

    /// Whether `range` is free, ignoring the clip identified by `ignore`.
    ///
    /// The exclusion is what makes moving a clip within its own footprint work:
    /// a clip never collides with itself.
    pub fn is_range_free(&self, range: TimeRange, ignore: Option<ClipId>) -> bool {
        !self
            .clips
            .iter()
            .any(|c| Some(c.id) != ignore && c.range().intersects(range))
    }

    /// Inserts a clip, refusing the edit if it would overlap another.
    pub fn insert_clip(&mut self, clip: Clip) -> Result<(), CoreError> {
        if self.locked {
            return Err(CoreError::TrackLocked);
        }
        if clip.duration.raw() <= 0 {
            return Err(CoreError::EmptyClip);
        }
        if !self.is_range_free(clip.range(), None) {
            return Err(CoreError::ClipOverlap);
        }
        let idx = self.clips.partition_point(|c| c.timeline_start <= clip.timeline_start);
        self.clips.insert(idx, clip);
        debug_assert!(self.invariants_hold());
        Ok(())
    }

    pub fn remove_clip(&mut self, id: ClipId) -> Option<Clip> {
        let idx = self.index_of(id)?;
        Some(self.clips.remove(idx))
    }

    /// Moves a clip to a new start position.
    pub fn move_clip(&mut self, id: ClipId, new_start: Ticks) -> Result<Ticks, CoreError> {
        if self.locked {
            return Err(CoreError::TrackLocked);
        }
        let idx = self.index_of(id).ok_or(CoreError::ClipNotFound(id))?;
        let new_start = new_start.clamp_non_negative();
        let old_start = self.clips[idx].timeline_start;
        let candidate = TimeRange::new(new_start, self.clips[idx].duration);
        if !self.is_range_free(candidate, Some(id)) {
            return Err(CoreError::ClipOverlap);
        }
        self.clips[idx].timeline_start = new_start;
        self.resort_from(idx);
        debug_assert!(self.invariants_hold());
        Ok(old_start)
    }

    /// Trims a clip's head, keeping the visible frames pinned to the timeline.
    pub fn trim_clip_start(
        &mut self,
        id: ClipId,
        new_start: Ticks,
        min_duration: Ticks,
    ) -> Result<(), CoreError> {
        if self.locked {
            return Err(CoreError::TrackLocked);
        }
        let idx = self.index_of(id).ok_or(CoreError::ClipNotFound(id))?;
        let new_start = new_start.clamp_non_negative();
        let end = self.clips[idx].timeline_end();
        if !self.is_range_free(TimeRange::from_bounds(new_start, end), Some(id)) {
            return Err(CoreError::ClipOverlap);
        }
        self.clips[idx].trim_start(new_start, min_duration)?;
        self.resort_from(idx);
        debug_assert!(self.invariants_hold());
        Ok(())
    }

    /// Trims a clip's tail.
    pub fn trim_clip_end(
        &mut self,
        id: ClipId,
        new_end: Ticks,
        min_duration: Ticks,
        available_source: Ticks,
    ) -> Result<(), CoreError> {
        if self.locked {
            return Err(CoreError::TrackLocked);
        }
        let idx = self.index_of(id).ok_or(CoreError::ClipNotFound(id))?;
        let start = self.clips[idx].timeline_start;
        if !self.is_range_free(TimeRange::from_bounds(start, new_end), Some(id)) {
            return Err(CoreError::ClipOverlap);
        }
        self.clips[idx].trim_clip_end_inner(new_end, min_duration, available_source)?;
        debug_assert!(self.invariants_hold());
        Ok(())
    }

    /// Splits the clip covering `at` into two, returning the new right-hand ID.
    pub fn split_clip(
        &mut self,
        id: ClipId,
        at: Ticks,
        new_clip_id: ClipId,
        new_effect_id: impl FnMut() -> crate::id::EffectId,
    ) -> Result<ClipId, CoreError> {
        if self.locked {
            return Err(CoreError::TrackLocked);
        }
        let idx = self.index_of(id).ok_or(CoreError::ClipNotFound(id))?;
        let right = self.clips[idx].split_at(at, new_clip_id, new_effect_id)?;
        let right_id = right.id;
        self.clips.insert(idx + 1, right);
        debug_assert!(self.invariants_hold());
        Ok(right_id)
    }

    /// Restores sort order after a single element moved, by shifting it to its
    /// new home. Cheaper than a full re-sort and preserves the ordering of
    /// everything else.
    fn resort_from(&mut self, idx: usize) {
        let mut i = idx;
        while i > 0 && self.clips[i - 1].timeline_start > self.clips[i].timeline_start {
            self.clips.swap(i - 1, i);
            i -= 1;
        }
        while i + 1 < self.clips.len()
            && self.clips[i + 1].timeline_start < self.clips[i].timeline_start
        {
            self.clips.swap(i, i + 1);
            i += 1;
        }
    }

    /// Re-establishes sort order and reports overlaps. Used after loading a
    /// project file, whose contents cannot be trusted to hold the invariant.
    pub(crate) fn normalise(&mut self) -> Vec<ClipId> {
        self.clips.sort_by_key(|c| c.timeline_start);
        let mut overlapping = Vec::new();
        for i in 1..self.clips.len() {
            if self.clips[i - 1].timeline_end() > self.clips[i].timeline_start {
                overlapping.push(self.clips[i].id);
            }
        }
        overlapping
    }

    /// Checks the sort and non-overlap invariants. Called from `debug_assert!`
    /// after every mutation, so a broken edit path fails loudly in tests.
    pub fn invariants_hold(&self) -> bool {
        self.clips.windows(2).all(|w| w[0].timeline_end() <= w[1].timeline_start)
            && self.clips.iter().all(|c| c.duration.raw() > 0)
    }

    /// The first position at or after `after` where `duration` ticks are free.
    pub fn first_free_slot(&self, after: Ticks, duration: Ticks) -> Ticks {
        let mut cursor = after.clamp_non_negative();
        for clip in &self.clips {
            if clip.timeline_end() <= cursor {
                continue;
            }
            if clip.timeline_start >= cursor + duration {
                break;
            }
            cursor = clip.timeline_end();
        }
        cursor
    }
}

impl Clip {
    /// Inherent helper so `Track::trim_clip_end` can borrow one clip mutably
    /// while the overlap check has already released its borrow of the track.
    fn trim_clip_end_inner(
        &mut self,
        new_end: Ticks,
        min_duration: Ticks,
        available_source: Ticks,
    ) -> Result<(), CoreError> {
        self.trim_end(new_end, min_duration, available_source)
    }
}
