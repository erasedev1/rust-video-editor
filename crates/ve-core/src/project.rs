use std::path::Path;

use serde::{Deserialize, Serialize};
use ve_time::Ticks;

use crate::asset::{MediaAsset, MediaInfo};
use crate::composition::{Composition, CompositionSettings};
use crate::id::{
    AngleId, AssetId, ClipId, CompositionId, EffectId, IdAllocator, LayerId, MarkerId,
    MulticamId, SequenceId, TrackId,
};
use crate::multicam::{MulticamAngle, MulticamGroup};
use crate::sequence::{Sequence, SequenceSettings};
use crate::source::Source;
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
    /// Whether the editor decodes from proxies where an asset has one.
    ///
    /// A switch rather than a consequence of a proxy existing, because the
    /// whole point of building one is to be able to turn it off and look at the
    /// real picture — checking focus, checking a key — without throwing the
    /// proxy away. It is stored with the project so that reopening a cut
    /// resumes at the resolution it was being cut at.
    ///
    /// Only the **editor** reads this. An export always renders the originals;
    /// see `ve_export::SourceFrames`.
    #[serde(default)]
    pub use_proxies: bool,
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
            use_proxies: false,
        }
    }
}

/// The root of the edit model.
///
/// Holds media references, sequences and compositions; owns the ID allocator
/// that keeps every handle in the project unique. This type is pure data with no
/// I/O, no threading and no GPU state, which is what lets the whole edit model be
/// tested, diffed and serialised in isolation from the engine.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Project {
    pub name: String,
    #[serde(default)]
    pub settings: ProjectSettings,
    pub assets: Vec<MediaAsset>,
    pub sequences: Vec<Sequence>,
    /// Compositing work, referenced by sequences and by each other. Defaulted on
    /// read, so a project written before compositions existed still opens.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub compositions: Vec<Composition>,
    /// Camera groups a clip can cut between. Defaulted on read, so a project
    /// written before multicam existed opens with none.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub multicams: Vec<MulticamGroup>,
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
            compositions: Vec::new(),
            multicams: Vec::new(),
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
        // A camera in a group is a use even with nothing on a timeline: losing
        // the file would leave an angle pointing at nothing, and the group is
        // where that would be discovered — in the middle of a cut.
        if self.asset_is_an_angle(id) {
            return Err(CoreError::AssetIsAnAngle(id));
        }
        let idx =
            self.assets.iter().position(|a| a.id == id).ok_or(CoreError::AssetNotFound(id))?;
        Ok(self.assets.remove(idx))
    }

    pub fn clips_using_asset(&self, id: AssetId) -> impl Iterator<Item = ClipId> + '_ {
        self.clips_using(Source::Asset(id))
    }

    /// Every clip drawing from a source, anywhere in the project.
    pub fn clips_using(&self, source: Source) -> impl Iterator<Item = ClipId> + '_ {
        self.sequences
            .iter()
            .flat_map(|s| s.tracks.iter())
            .flat_map(|t| t.clips().iter())
            .filter(move |c| c.source == source)
            .map(|c| c.id)
    }

    /// Every composition layer drawing from a source.
    pub fn layers_using(
        &self,
        source: Source,
    ) -> impl Iterator<Item = (CompositionId, LayerId)> + '_ {
        self.compositions
            .iter()
            .flat_map(move |c| c.layers.iter().map(move |l| (c.id, l)))
            .filter(move |(_, l)| l.source == source)
            .map(|(c, l)| (c, l.id))
    }

    /// How many places use a source at all, which is what a delete has to refuse
    /// over: a source still on a timeline or in a stack cannot just vanish.
    pub fn uses_of(&self, source: Source) -> usize {
        self.clips_using(source).count() + self.layers_using(source).count()
    }

    // ---- sequences ----------------------------------------------------

    pub fn add_sequence(
        &mut self,
        name: impl Into<String>,
        settings: SequenceSettings,
    ) -> SequenceId {
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

    // ---- multicam -----------------------------------------------------

    pub fn add_multicam_group(&mut self, name: impl Into<String>) -> MulticamId {
        let id = self.ids.alloc::<crate::id::MulticamTag>();
        self.multicams.push(MulticamGroup::new(id, name));
        id
    }

    pub fn multicam(&self, id: MulticamId) -> Option<&MulticamGroup> {
        self.multicams.iter().find(|g| g.id == id)
    }

    pub fn multicam_mut(&mut self, id: MulticamId) -> Option<&mut MulticamGroup> {
        self.multicams.iter_mut().find(|g| g.id == id)
    }

    /// Mints an angle for a group without adding it, so a command can capture
    /// the ID it is about to use before mutating anything.
    pub fn new_angle_id(&mut self) -> AngleId {
        self.ids.alloc::<crate::id::AngleTag>()
    }

    /// One angle, reached through its group.
    pub fn multicam_angle(&self, group: MulticamId, angle: AngleId) -> Option<&MulticamAngle> {
        self.multicam(group)?.angle(angle)
    }

    /// How many clips and layers draw any angle of a group.
    ///
    /// Counted across angles rather than per angle, because what a delete has
    /// to refuse over is the group disappearing, and a clip on angle 2 is just
    /// as stranded by that as one on angle 1.
    pub fn uses_of_multicam(&self, id: MulticamId) -> usize {
        let on_group = |s: &Source| s.multicam().is_some_and(|(g, _)| g == id);
        self.sequences
            .iter()
            .flat_map(|s| s.tracks.iter())
            .flat_map(|t| t.clips().iter())
            .filter(|c| on_group(&c.source))
            .count()
            + self
                .compositions
                .iter()
                .flat_map(|c| c.layers.iter())
                .filter(|l| on_group(&l.source))
                .count()
    }

    /// Removes a group, refusing while anything still cuts with it.
    pub fn remove_multicam_group(
        &mut self,
        id: MulticamId,
    ) -> Result<MulticamGroup, CoreError> {
        let places = self.uses_of_multicam(id);
        if places > 0 {
            return Err(CoreError::MulticamInUse { id, places });
        }
        let index = self
            .multicams
            .iter()
            .position(|g| g.id == id)
            .ok_or(CoreError::MulticamNotFound(id))?;
        Ok(self.multicams.remove(index))
    }

    /// Whether an asset is a camera in any group.
    ///
    /// Separate from [`Project::clips_using_asset`] because an angle is not a
    /// clip: nothing is on a timeline, but deleting the file out from under the
    /// group would leave an angle pointing at nothing all the same.
    pub fn asset_is_an_angle(&self, id: AssetId) -> bool {
        self.multicams.iter().any(|g| g.angles.iter().any(|a| a.asset == id))
    }

    // ---- compositions -------------------------------------------------

    pub fn add_composition(
        &mut self,
        name: impl Into<String>,
        settings: CompositionSettings,
    ) -> CompositionId {
        let id = self.ids.alloc::<crate::id::CompositionTag>();
        self.compositions.push(Composition::new(id, name, settings));
        id
    }

    pub fn composition(&self, id: CompositionId) -> Option<&Composition> {
        self.compositions.iter().find(|c| c.id == id)
    }

    pub fn composition_mut(&mut self, id: CompositionId) -> Option<&mut Composition> {
        self.compositions.iter_mut().find(|c| c.id == id)
    }

    /// Removes a composition, refusing while anything still draws from it.
    ///
    /// The same rule as an asset in use: a dangling source would render as a
    /// hole in someone's edit, and the count is what makes the refusal
    /// actionable rather than mysterious.
    pub fn remove_composition(&mut self, id: CompositionId) -> Result<Composition, CoreError> {
        let places = self.uses_of(Source::Composition(id));
        if places > 0 {
            return Err(CoreError::CompositionInUse { id, places });
        }
        let index = self
            .compositions
            .iter()
            .position(|c| c.id == id)
            .ok_or(CoreError::CompositionNotFound(id))?;
        Ok(self.compositions.remove(index))
    }

    /// Whether putting `inner` inside `outer` would close a nesting cycle.
    ///
    /// The one check that has to exist in the model rather than in a command:
    /// every path that can create a nested reference goes through it, and a
    /// cycle is not a rejected edit the user can reason about — it is a render
    /// that never finishes.
    pub fn nesting_would_cycle(&self, outer: CompositionId, inner: CompositionId) -> bool {
        match self.composition(outer) {
            Some(comp) => comp.would_cycle(inner, |id| self.composition(id)),
            // No such composition to put anything inside; the caller will fail
            // on that instead, and answering "yes, a cycle" here would be a lie.
            None => false,
        }
    }

    /// A composition's nesting depth, counting itself as one.
    ///
    /// Reported to the user when a nest is refused, and used by the engine's
    /// depth limit. Cycles are already impossible by construction, but the
    /// visited set means a hand-edited file cannot hang this either.
    pub fn nesting_depth(&self, id: CompositionId) -> usize {
        fn walk(project: &Project, id: CompositionId, seen: &mut Vec<CompositionId>) -> usize {
            if seen.contains(&id) {
                return 0;
            }
            seen.push(id);
            let deepest = project
                .composition(id)
                .map(|c| c.nested().iter().map(|n| walk(project, *n, seen)).max().unwrap_or(0))
                .unwrap_or(0);
            1 + deepest
        }
        walk(self, id, &mut Vec::new())
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
    pub fn new_layer_id(&mut self) -> LayerId {
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

    /// How much material a source offers, whichever kind it is.
    ///
    /// A composition's length is its own setting rather than a property of a
    /// file, which is what lets a trim past the end of a nested composition be
    /// refused for the same reason as a trim past the end of media.
    pub fn source_duration(&self, source: Source) -> Ticks {
        match source {
            Source::Asset(id) => self.asset_duration(id),
            Source::Composition(id) => {
                self.composition(id).map(|c| c.duration()).unwrap_or(Ticks::ZERO)
            }
            // A multicam clip is trimmed against the *group*, not against the
            // angle it happens to be showing. Bounding it by the current angle
            // would make the same clip trimmable to different lengths depending
            // on which camera was on screen when the trim began.
            Source::Multicam { group, .. } => self.multicam_duration(group),
        }
    }

    /// How much group time a multicam group covers: from zero to the last frame
    /// any of its angles recorded.
    ///
    /// The **union** rather than the intersection. Cameras in a real shoot start
    /// and stop at different moments, and bounding the group to the stretch
    /// every camera covers would refuse edits over footage that plainly exists.
    /// An angle with nothing at a given instant draws nothing, and the viewer
    /// says which those are.
    pub fn multicam_duration(&self, id: MulticamId) -> Ticks {
        let Some(group) = self.multicam(id) else { return Ticks::ZERO };
        group
            .angles
            .iter()
            .map(|a| a.coverage(self.asset_duration(a.asset)).end())
            .max()
            .unwrap_or(Ticks::ZERO)
            .clamp_non_negative()
    }

    /// Turns a source and a time in that source's own domain into the file to
    /// decode and the instant to decode from.
    ///
    /// The one place the multicam indirection is resolved. Everything
    /// downstream — the plan, the decode scheduler, the exporter — asks this
    /// rather than matching on [`Source`] itself, so there is exactly one copy
    /// of the offset arithmetic and exactly one answer to "which camera is this
    /// clip showing".
    ///
    /// `None` when the group, the angle or the camera is missing, and when the
    /// angle was not rolling yet at that instant.
    pub fn resolve_source(&self, source: Source, at: Ticks) -> Option<(AssetId, Ticks)> {
        match source {
            Source::Asset(id) => Some((id, at)),
            Source::Composition(_) => None,
            Source::Multicam { group, angle } => {
                let group = self.multicam(group)?;
                let source_time = group.source_time_at(angle, at)?;
                Some((group.angle(angle)?.asset, source_time))
            }
        }
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
        // Referenced IDs count too, not just owned ones. A clip pointing at a
        // source that is missing from the file must not have that ID handed to a
        // future import, or the orphan would silently bind to unrelated media.
        let source_raw = |source: Source| match source {
            Source::Asset(id) => id.raw(),
            Source::Composition(id) => id.raw(),
            // Both halves, because both are IDs from the same allocator and
            // either could be the highest one the file mentions.
            Source::Multicam { group, angle } => group.raw().max(angle.raw()),
        };
        for s in &self.sequences {
            max_id = max_id.max(s.id.raw());
            for m in &s.markers {
                max_id = max_id.max(m.id.raw());
            }
            for t in &s.tracks {
                max_id = max_id.max(t.id.raw());
                for c in t.clips() {
                    max_id = max_id.max(c.id.raw());
                    max_id = max_id.max(source_raw(c.source));
                    for e in &c.effects {
                        max_id = max_id.max(e.id.raw());
                    }
                }
            }
        }
        for c in &self.compositions {
            max_id = max_id.max(c.id.raw());
            for l in &c.layers {
                max_id = max_id.max(l.id.raw());
                max_id = max_id.max(source_raw(l.source));
                for e in &l.effects {
                    max_id = max_id.max(e.id.raw());
                }
            }
        }
        for g in &self.multicams {
            max_id = max_id.max(g.id.raw());
            for a in &g.angles {
                max_id = max_id.max(a.id.raw());
                max_id = max_id.max(a.asset.raw());
            }
        }
        self.ids.bump_past(max_id);

        // Groups first, so a clip's report of a missing angle reflects any angle
        // this pass drops rather than contradicting it.
        for group in &mut self.multicams {
            warnings.extend(group.normalise());
        }
        let known_assets: Vec<AssetId> = self.assets.iter().map(|a| a.id).collect();
        for group in &mut self.multicams {
            let name = group.name.clone();
            group.angles.retain(|angle| {
                let kept = known_assets.contains(&angle.asset);
                if !kept {
                    warnings.push(format!(
                        "multicam '{name}': angle '{}' names missing asset {}; removed",
                        angle.name, angle.asset
                    ));
                }
                kept
            });
        }

        // Compositions first: breaking a nesting cycle can only be done with
        // every composition in view, and a sequence's report of a missing source
        // should already reflect the repair.
        for comp in &mut self.compositions {
            warnings.extend(comp.normalise());
        }
        warnings.extend(self.break_nesting_cycles());

        let known: Vec<Source> = self
            .assets
            .iter()
            .map(|a| Source::Asset(a.id))
            .chain(self.compositions.iter().map(|c| Source::Composition(c.id)))
            .chain(self.multicams.iter().flat_map(|g| {
                g.angles.iter().map(move |a| Source::Multicam { group: g.id, angle: a.id })
            }))
            .collect();

        for seq in &mut self.sequences {
            let seq_name = seq.name.clone();
            for id in seq.normalise() {
                warnings
                    .push(format!("sequence '{seq_name}': clip {id} overlaps its neighbour"));
            }
            for track in &seq.tracks {
                for clip in track.clips() {
                    if !known.contains(&clip.source) {
                        warnings.push(format!(
                            "sequence '{seq_name}': clip '{}' references missing {}",
                            clip.name, clip.source
                        ));
                    }
                }
            }
        }
        for comp in &self.compositions {
            for layer in &comp.layers {
                if !known.contains(&layer.source) {
                    warnings.push(format!(
                        "composition '{}': layer '{}' references missing {}",
                        comp.name, layer.name, layer.source
                    ));
                }
            }
        }

        self.conform_effects(crate::registry::builtin_registry());

        if let Some(active) = self.active_sequence {
            if self.sequence(active).is_none() {
                warnings
                    .push("active sequence does not exist; falling back to the first".into());
                self.active_sequence = self.sequences.first().map(|s| s.id);
            }
        } else {
            self.active_sequence = self.sequences.first().map(|s| s.id);
        }

        warnings
    }

    /// Brings every effect parameter into line with what `registry` declares.
    ///
    /// A project file is untrusted input: a parameter can be missing, of the
    /// wrong type, or named something this build has never declared. Conforming
    /// on load means a shader is never handed a colour where it expects a
    /// radius, while a parameter the registry does not know about is kept so
    /// that a file written by a newer build survives a round trip through this
    /// one. See [`EffectRegistry::conform`].
    ///
    /// Taken as an argument rather than always using the built-ins, so that a
    /// host which has loaded plugin effects conforms against those too.
    ///
    /// [`EffectRegistry::conform`]: crate::registry::EffectRegistry::conform
    pub fn conform_effects(&mut self, registry: &crate::registry::EffectRegistry) {
        for sequence in &mut self.sequences {
            for track in &mut sequence.tracks {
                for clip in track.clips_mut() {
                    registry.conform(&mut clip.effects);
                }
            }
        }
        for composition in &mut self.compositions {
            for layer in &mut composition.layers {
                registry.conform(&mut layer.effects);
            }
        }
    }

    /// Removes the layers that close a nesting cycle, reporting each one.
    ///
    /// Only reachable from a hand-edited or corrupt file — every path through
    /// the model refuses a cycle before it exists — but "only reachable from a
    /// bad file" is exactly the case a loader has to survive, and the
    /// alternative to breaking the cycle is a render that never returns.
    fn break_nesting_cycles(&mut self) -> Vec<String> {
        let mut warnings = Vec::new();
        loop {
            // One offending (composition, layer) pair per pass, re-testing
            // afterwards: removing a layer can resolve several cycles at once,
            // and the graph is tiny. Document order decides which edge of a
            // cycle goes, so the same file always loads the same way — with two
            // compositions each holding the other there is no principled choice
            // between the two edges, only a repeatable one.
            let mut doomed = None;
            'search: for comp in &self.compositions {
                for layer in &comp.layers {
                    if let Some(inner) = layer.source.composition() {
                        if self.nesting_would_cycle_excluding(comp.id, inner, layer.id) {
                            doomed = Some((
                                comp.id,
                                layer.id,
                                comp.name.clone(),
                                layer.name.clone(),
                            ));
                            break 'search;
                        }
                    }
                }
            }
            let Some((comp_id, layer_id, comp_name, layer_name)) = doomed else { break };
            if let Some(comp) = self.composition_mut(comp_id) {
                comp.remove_layer(layer_id);
            }
            warnings.push(format!(
                "composition '{comp_name}': layer '{layer_name}' closed a nesting cycle; removed"
            ));
        }
        warnings
    }

    /// Cycle test that ignores one existing layer, which is what asking "is
    /// *this* layer the one closing the loop?" requires: the layer is already in
    /// place, so the plain test would always say yes.
    fn nesting_would_cycle_excluding(
        &self,
        outer: CompositionId,
        inner: CompositionId,
        ignore: LayerId,
    ) -> bool {
        if outer == inner {
            return true;
        }
        let mut stack = vec![inner];
        let mut seen: Vec<CompositionId> = Vec::new();
        while let Some(next) = stack.pop() {
            if next == outer {
                return true;
            }
            if seen.contains(&next) {
                continue;
            }
            seen.push(next);
            if let Some(comp) = self.composition(next) {
                stack.extend(
                    comp.layers
                        .iter()
                        .filter(|l| l.id != ignore)
                        .filter_map(|l| l.source.composition()),
                );
            }
        }
        false
    }

    /// Total clip count, used by the benchmark suite and the status bar.
    pub fn clip_count(&self) -> usize {
        self.sequences.iter().flat_map(|s| s.tracks.iter()).map(|t| t.len()).sum()
    }
}
