use std::path::Path;

use serde::{Deserialize, Serialize};
use ve_time::Ticks;

use crate::asset::{MediaAsset, MediaInfo};
use crate::id::{AssetId, ClipId, EffectId, IdAllocator, MarkerId, SequenceId, TrackId};
use crate::sequence::{Sequence, SequenceSettings};
use crate::track::TrackKind;
use crate::CoreError;

/// Editor-wide preferences stored with the project.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ProjectSettings {
    /// Defaults applied to newly created sequences.
    pub default_sequence: SequenceSettings,
    /// Shortest a clip may be trimmed to, in frames of the sequence rate.
    /// One frame, because a zero-length clip is not a clip.
    #[serde(default = "default_min_clip_frames")]
    pub min_clip_frames: u32,
    #[serde(default = "default_autosave_seconds")]
    pub autosave_interval_seconds: u32,
}

fn default_min_clip_frames() -> u32 {
    1
}
fn default_autosave_seconds() -> u32 {
    120
}

impl Default for ProjectSettings {
    fn default() -> Self {
        ProjectSettings {
            default_sequence: SequenceSettings::default(),
            min_clip_frames: default_min_clip_frames(),
            autosave_interval_seconds: default_autosave_seconds(),
        }
    }
}

/// The root of the edit model.
///
/// Holds media references and sequences; owns the ID allocator that keeps every
/// handle in the project unique. This type is pure data with no I/O, no
/// threading and no GPU state, which is what lets the whole edit model be
/// tested, diffed and serialised in isolation from the engine.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Project {
    pub name: String,
    #[serde(default)]
    pub settings: ProjectSettings,
    pub assets: Vec<MediaAsset>,
    pub sequences: Vec<Sequence>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub active_sequence: Option<SequenceId>,
    pub ids: IdAllocator,
}

impl Project {
    pub fn new(name: impl Into<String>) -> Self {
        Project {
            name: name.into(),
            settings: ProjectSettings::default(),
            assets: Vec::new(),
            sequences: Vec::new(),
            active_sequence: None,
            ids: IdAllocator::new(),
        }
    }

    /// A project with one empty sequence holding two video and two audio
    /// tracks, which is what "File > New Project" produces.
    pub fn with_default_sequence(name: impl Into<String>) -> Self {
        let mut p = Project::new(name);
        let settings = p.settings.default_sequence.clone();
        let seq = p.add_sequence("Sequence 1", settings);
        for kind in [TrackKind::Video, TrackKind::Video, TrackKind::Audio, TrackKind::Audio] {
            let tid = p.ids.alloc::<crate::id::TrackTag>();
            p.sequence_mut(seq).expect("just created").add_track(tid, kind);
        }
        p.active_sequence = Some(seq);
        p
    }

    // ---- assets -------------------------------------------------------

    pub fn add_asset(&mut self, path: impl AsRef<Path>, info: MediaInfo) -> AssetId {
        let id = self.ids.alloc::<crate::id::AssetTag>();
        self.assets.push(MediaAsset::new(id, path.as_ref().to_path_buf(), info));
        id
    }

    pub fn asset(&self, id: AssetId) -> Option<&MediaAsset> {
        self.assets.iter().find(|a| a.id == id)
    }

    pub fn asset_mut(&mut self, id: AssetId) -> Option<&mut MediaAsset> {
        self.assets.iter_mut().find(|a| a.id == id)
    }

    /// Finds an already-imported asset by path, so importing the same file
    /// twice reuses one entry rather than duplicating media.
    pub fn asset_by_path(&self, path: &Path) -> Option<&MediaAsset> {
        self.assets.iter().find(|a| a.path == path)
    }

    /// Removes an asset. Refuses while clips still reference it, naming how
    /// many, rather than leaving dangling references behind.
    pub fn remove_asset(&mut self, id: AssetId) -> Result<MediaAsset, CoreError> {
        let uses = self.clips_using_asset(id).count();
        if uses > 0 {
            return Err(CoreError::AssetInUse { id, clips: uses });
        }
        let idx = self.assets.iter().position(|a| a.id == id).ok_or(CoreError::AssetNotFound(id))?;
        Ok(self.assets.remove(idx))
    }

    pub fn clips_using_asset(&self, id: AssetId) -> impl Iterator<Item = ClipId> + '_ {
        self.sequences
            .iter()
            .flat_map(|s| s.tracks.iter())
            .flat_map(|t| t.clips().iter())
            .filter(move |c| c.asset == id)
            .map(|c| c.id)
    }

    // ---- sequences ----------------------------------------------------

    pub fn add_sequence(&mut self, name: impl Into<String>, settings: SequenceSettings) -> SequenceId {
        let id = self.ids.alloc::<crate::id::SequenceTag>();
        self.sequences.push(Sequence::new(id, name, settings));
        if self.active_sequence.is_none() {
            self.active_sequence = Some(id);
        }
        id
    }

    pub fn sequence(&self, id: SequenceId) -> Option<&Sequence> {
        self.sequences.iter().find(|s| s.id == id)
    }

    pub fn sequence_mut(&mut self, id: SequenceId) -> Option<&mut Sequence> {
        self.sequences.iter_mut().find(|s| s.id == id)
    }

    pub fn active(&self) -> Option<&Sequence> {
        self.active_sequence.and_then(|id| self.sequence(id))
    }

    pub fn active_mut(&mut self) -> Option<&mut Sequence> {
        let id = self.active_sequence?;
        self.sequence_mut(id)
    }

    // ---- id allocation -------------------------------------------------
    //
    // Thin wrappers so call sites read as intent rather than as turbofished
    // phantom tags.

    pub fn new_clip_id(&mut self) -> ClipId {
        self.ids.alloc()
    }
    pub fn new_track_id(&mut self) -> TrackId {
        self.ids.alloc()
    }
    pub fn new_effect_id(&mut self) -> EffectId {
        self.ids.alloc()
    }
    pub fn new_marker_id(&mut self) -> MarkerId {
        self.ids.alloc()
    }

    /// Shortest permitted clip duration, in the given sequence's timebase.
    pub fn min_clip_duration(&self, sequence: SequenceId) -> Ticks {
        let rate = self
            .sequence(sequence)
            .map(|s| s.rate())
            .unwrap_or(self.settings.default_sequence.rate);
        rate.frame_to_ticks(self.settings.min_clip_frames.max(1) as i64)
    }

    /// Source duration available to a clip, used to bound trims.
    pub fn asset_duration(&self, id: AssetId) -> Ticks {
        self.asset(id).map(|a| a.duration()).unwrap_or(Ticks::ZERO)
    }

    /// Repairs invariants after loading a project file.
    ///
    /// Returns a list of human-readable warnings. The project is always left in
    /// a usable state; nothing is discarded, so a file written by a newer or
    /// buggier build still opens and the user can fix it by hand.
    pub fn normalise(&mut self) -> Vec<String> {
        let mut warnings = Vec::new();

        // Future allocations must never collide with IDs already in the file.
        let mut max_id = 0u64;
        for a in &self.assets {
            max_id = max_id.max(a.id.raw());
        }
        for s in &self.sequences {
            max_id = max_id.max(s.id.raw());
            for m in &s.markers {
                max_id = max_id.max(m.id.raw());
            }
            for t in &s.tracks {
                max_id = max_id.max(t.id.raw());
                for c in t.clips() {
                    max_id = max_id.max(c.id.raw());
                    // Referenced IDs count too, not just owned ones. A clip
                    // pointing at an asset that is missing from the file must
                    // not have that ID handed to a future import, or the
                    // orphan would silently bind to unrelated media.
                    max_id = max_id.max(c.asset.raw());
                    for e in &c.effects {
                        max_id = max_id.max(e.id.raw());
                    }
                }
            }
        }
        self.ids.bump_past(max_id);

        let known_assets: Vec<AssetId> = self.assets.iter().map(|a| a.id).collect();
        for seq in &mut self.sequences {
            let seq_name = seq.name.clone();
            for id in seq.normalise() {
                warnings.push(format!("sequence '{seq_name}': clip {id} overlaps its neighbour"));
            }
            for track in &seq.tracks {
                for clip in track.clips() {
                    if !known_assets.contains(&clip.asset) {
                        warnings.push(format!(
                            "sequence '{seq_name}': clip '{}' references missing asset {}",
                            clip.name, clip.asset
                        ));
                    }
                }
            }
        }

        if let Some(active) = self.active_sequence {
            if self.sequence(active).is_none() {
                warnings.push("active sequence does not exist; falling back to the first".into());
                self.active_sequence = self.sequences.first().map(|s| s.id);
            }
        } else {
            self.active_sequence = self.sequences.first().map(|s| s.id);
        }

        warnings
    }

    /// Total clip count, used by the benchmark suite and the status bar.
    pub fn clip_count(&self) -> usize {
        self.sequences
            .iter()
            .flat_map(|s| s.tracks.iter())
            .map(|t| t.len())
            .sum()
    }
}
