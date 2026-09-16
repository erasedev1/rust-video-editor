use serde::{Deserialize, Serialize};
use ve_time::{Rate, SampleRate, Ticks, TimeRange, Timecode};

use crate::clip::Clip;
use crate::geometry::{Rgba, Size};
use crate::id::{ClipId, MarkerId, SequenceId, TrackId};
use crate::track::{Track, TrackKind};
use crate::CoreError;

/// A named point or span on the timeline.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Marker {
    pub id: MarkerId,
    pub name: String,
    pub time: Ticks,
    /// Zero for a point marker, non-zero for a span.
    #[serde(default)]
    pub duration: Ticks,
    #[serde(default)]
    pub color: Rgba,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub comment: String,
}

impl Marker {
    pub fn new(id: MarkerId, name: impl Into<String>, time: Ticks) -> Self {
        Marker {
            id,
            name: name.into(),
            time,
            duration: Ticks::ZERO,
            color: Rgba::new(0.95, 0.75, 0.2, 1.0),
            comment: String::new(),
        }
    }

    pub fn range(&self) -> TimeRange {
        TimeRange::new(self.time, self.duration)
    }
}

/// Resolution, frame rate and audio format a sequence renders at.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SequenceSettings {
    pub resolution: Size,
    pub rate: Rate,
    pub sample_rate: SampleRate,
    pub channels: u16,
    /// Shown wherever no clip covers the playhead.
    #[serde(default)]
    pub background: Rgba,
    /// Timecode of the sequence's first frame. Broadcast deliverables commonly
    /// start at 01:00:00:00 rather than zero.
    #[serde(default)]
    pub start_timecode: Ticks,
}

impl Default for SequenceSettings {
    fn default() -> Self {
        SequenceSettings {
            resolution: Size::new(1920, 1080),
            rate: Rate::FPS_30,
            sample_rate: SampleRate::HZ_48000,
            channels: 2,
            background: Rgba::BLACK,
            start_timecode: Ticks::ZERO,
        }
    }
}

/// An ordered stack of tracks sharing a frame rate and resolution.
///
/// Track order is bottom-up for video (index 0 is V1, the lowest layer) and
/// arbitrary for audio, matching how the timeline draws them.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Sequence {
    pub id: SequenceId,
    pub name: String,
    pub settings: SequenceSettings,
    pub tracks: Vec<Track>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub markers: Vec<Marker>,
    /// Persisted so reopening a project restores the editing position.
    #[serde(default)]
    pub playhead: Ticks,
    /// The span the user is currently working in, used to scope preview renders
    /// and exports.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub work_area: Option<TimeRange>,
}

impl Sequence {
    pub fn new(id: SequenceId, name: impl Into<String>, settings: SequenceSettings) -> Self {
        Sequence {
            id,
            name: name.into(),
            settings,
            tracks: Vec::new(),
            markers: Vec::new(),
            playhead: Ticks::ZERO,
            work_area: None,
        }
    }

    #[inline]
    pub fn rate(&self) -> Rate {
        self.settings.rate
    }

    pub fn track(&self, id: TrackId) -> Option<&Track> {
        self.tracks.iter().find(|t| t.id == id)
    }

    pub fn track_mut(&mut self, id: TrackId) -> Option<&mut Track> {
        self.tracks.iter_mut().find(|t| t.id == id)
    }

    pub fn track_index(&self, id: TrackId) -> Option<usize> {
        self.tracks.iter().position(|t| t.id == id)
    }

    pub fn video_tracks(&self) -> impl Iterator<Item = &Track> {
        self.tracks.iter().filter(|t| t.kind == TrackKind::Video)
    }

    pub fn audio_tracks(&self) -> impl Iterator<Item = &Track> {
        self.tracks.iter().filter(|t| t.kind == TrackKind::Audio)
    }

    /// Finds a clip anywhere in the sequence.
    pub fn find_clip(&self, id: ClipId) -> Option<(TrackId, &Clip)> {
        self.tracks.iter().find_map(|t| t.clip(id).map(|c| (t.id, c)))
    }

    pub fn find_clip_mut(&mut self, id: ClipId) -> Option<(TrackId, &mut Clip)> {
        self.tracks.iter_mut().find_map(|t| {
            let tid = t.id;
            t.clip_mut(id).map(|c| (tid, c))
        })
    }

    /// End of the last clip on any track.
    pub fn duration(&self) -> Ticks {
        self.tracks.iter().map(|t| t.content_end()).max().unwrap_or(Ticks::ZERO)
    }

    /// The span the preview and export should cover: the work area if one is
    /// set, otherwise the whole sequence.
    pub fn active_range(&self) -> TimeRange {
        self.work_area.unwrap_or_else(|| TimeRange::new(Ticks::ZERO, self.duration()))
    }

    /// Adds a track, naming it `V1`, `A2` and so on by position among its kind.
    pub fn add_track(&mut self, id: TrackId, kind: TrackKind) -> TrackId {
        let n = self.tracks.iter().filter(|t| t.kind == kind).count() + 1;
        let name = format!("{}{}", kind.prefix(), n);
        self.tracks.push(Track::new(id, kind, name));
        id
    }

    pub fn remove_track(&mut self, id: TrackId) -> Option<Track> {
        let idx = self.track_index(id)?;
        Some(self.tracks.remove(idx))
    }

    /// Snaps `t` to the sequence frame grid. Every user-facing time lands on a
    /// frame boundary; sub-frame positions only exist inside the audio engine.
    #[inline]
    pub fn snap_to_frame(&self, t: Ticks) -> Ticks {
        self.settings.rate.snap_round(t)
    }

    pub fn set_playhead(&mut self, t: Ticks) {
        self.playhead = self.snap_to_frame(t).clamp_non_negative();
    }

    /// Renders a timeline position as timecode, offset by the sequence's start
    /// timecode.
    pub fn timecode_at(&self, t: Ticks) -> Timecode {
        Timecode::from_ticks(t + self.settings.start_timecode, self.settings.rate)
    }

    pub fn add_marker(&mut self, marker: Marker) {
        let idx = self.markers.partition_point(|m| m.time <= marker.time);
        self.markers.insert(idx, marker);
    }

    pub fn remove_marker(&mut self, id: MarkerId) -> Option<Marker> {
        let idx = self.markers.iter().position(|m| m.id == id)?;
        Some(self.markers.remove(idx))
    }

    /// Candidate snap targets near `t`: clip edges, markers and the origin.
    ///
    /// Only edges within `tolerance` are returned, and the closest one wins, so
    /// the caller does not have to rank them.
    pub fn snap_candidate(
        &self,
        t: Ticks,
        tolerance: Ticks,
        exclude_clip: Option<ClipId>,
    ) -> Option<Ticks> {
        let window = TimeRange::from_bounds(t - tolerance, t + tolerance);
        let mut best: Option<(Ticks, Ticks)> = None;
        let mut consider = |candidate: Ticks| {
            if !window.contains(candidate) {
                return;
            }
            let dist = (candidate - t).abs();
            if best.is_none_or(|(_, d)| dist < d) {
                best = Some((candidate, dist));
            }
        };

        consider(Ticks::ZERO);
        for track in &self.tracks {
            for clip in track.clips_in_range(window) {
                if Some(clip.id) == exclude_clip {
                    continue;
                }
                consider(clip.timeline_start);
                consider(clip.timeline_end());
            }
        }
        for marker in &self.markers {
            consider(marker.time);
        }
        best.map(|(c, _)| c)
    }

    /// Re-establishes per-track invariants after loading. Returns the IDs of
    /// clips that were found overlapping, which the loader reports as warnings
    /// rather than silently discarding user data.
    pub(crate) fn normalise(&mut self) -> Vec<ClipId> {
        let mut overlaps = Vec::new();
        for track in &mut self.tracks {
            overlaps.extend(track.normalise());
        }
        self.markers.sort_by_key(|m| m.time);
        self.playhead = self.playhead.clamp_non_negative();
        overlaps
    }

    /// Returns the clip that a split at the playhead would affect on a track.
    pub fn clip_at(&self, track: TrackId, t: Ticks) -> Result<&Clip, CoreError> {
        self.track(track)
            .ok_or(CoreError::TrackNotFound(track))?
            .clip_at(t)
            .ok_or(CoreError::NoClipAtTime)
    }
}
