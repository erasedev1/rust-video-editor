//! Commands for multicam groups: building one, syncing it, and cutting with it.
//!
//! # Where the measuring happens
//!
//! Nowhere in here. Syncing by audio means decoding two files; syncing by
//! timecode means reading metadata. Both are I/O, and the command layer sits on
//! top of a pure edit model that does none. So [`ResyncMulticamGroup`] takes the
//! offsets *already measured* and records them, and the interface is what runs
//! the measurement and hands the answer over.
//!
//! That division is also what makes syncing undoable in the ordinary way: the
//! command is a plain before-and-after on a handful of numbers, whatever
//! expensive thing produced them.

use std::any::Any;

use ve_core::{
    AngleId, AssetId, ClipId, MulticamAngle, MulticamGroup, MulticamId, Project, SequenceId,
    Source, SyncMethod, TrackId,
};
use ve_time::Ticks;

use crate::clip_commands::SplitClip;
use crate::{clip_mut, Command, CommandError};

/// A camera to put in a new group.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NewAngle {
    pub asset: AssetId,
    pub name: String,
    pub offset: Ticks,
}

impl NewAngle {
    pub fn new(asset: AssetId, name: impl Into<String>) -> Self {
        NewAngle { asset, name: name.into(), offset: Ticks::ZERO }
    }

    pub fn with_offset(mut self, offset: Ticks) -> Self {
        self.offset = offset;
        self
    }
}

/// Builds a multicam group from a set of cameras.
#[derive(Debug)]
pub struct CreateMulticamGroup {
    name: String,
    angles: Vec<NewAngle>,
    sync: SyncMethod,
    /// Minted on the first apply and reused on redo, so a redone create
    /// produces the same IDs and anything holding one stays valid.
    group_id: Option<MulticamId>,
    angle_ids: Vec<AngleId>,
}

impl CreateMulticamGroup {
    pub fn new(name: impl Into<String>, angles: Vec<NewAngle>, sync: SyncMethod) -> Self {
        CreateMulticamGroup {
            name: name.into(),
            angles,
            sync,
            group_id: None,
            angle_ids: Vec::new(),
        }
    }

    /// The group's ID, available once the command has been applied.
    pub fn group_id(&self) -> Option<MulticamId> {
        self.group_id
    }

    pub fn angle_ids(&self) -> &[AngleId] {
        &self.angle_ids
    }
}

impl Command for CreateMulticamGroup {
    fn name(&self) -> &str {
        "Create Multicam Group"
    }

    fn apply(&mut self, project: &mut Project) -> Result<(), CommandError> {
        // Two is the smallest number of cameras that can be cut between, and a
        // group of one is a clip with extra steps.
        if self.angles.len() < 2 {
            return Err(CommandError::Core(ve_core::CoreError::MulticamTooFewAngles));
        }
        // Everything is checked before a single ID is minted. A command that
        // fails has to leave the project exactly as it was, and the ID
        // allocator is part of the project: minting first and validating after
        // would burn IDs on every rejected attempt.
        for angle in &self.angles {
            if project.asset(angle.asset).is_none() {
                return Err(CommandError::Rejected(format!(
                    "asset {} is not in this project",
                    angle.asset
                )));
            }
        }
        for (index, angle) in self.angles.iter().enumerate() {
            if self.angles[..index].iter().any(|a| a.asset == angle.asset) {
                return Err(CommandError::Core(ve_core::CoreError::AngleDuplicated(
                    angle.asset,
                )));
            }
        }

        while self.angle_ids.len() < self.angles.len() {
            let id = project.new_angle_id();
            self.angle_ids.push(id);
        }
        let group_id = match self.group_id {
            Some(id) => id,
            None => {
                let id = project.ids.alloc::<ve_core::id::MulticamTag>();
                self.group_id = Some(id);
                id
            }
        };

        let mut group = MulticamGroup::new(group_id, self.name.clone());
        group.sync = self.sync;
        for (angle, id) in self.angles.iter().zip(&self.angle_ids) {
            group
                .push_angle(
                    MulticamAngle::new(*id, angle.name.clone(), angle.asset)
                        .with_offset(angle.offset),
                )
                .map_err(CommandError::from)?;
        }
        project.multicams.push(group);
        Ok(())
    }

    fn undo(&mut self, project: &mut Project) -> Result<(), CommandError> {
        let id = self
            .group_id
            .ok_or_else(|| CommandError::Rejected("group was never created".into()))?;
        project.remove_multicam_group(id)?;
        Ok(())
    }

    fn as_any(&self) -> &dyn Any {
        self
    }
}

/// Deletes a group, which the model refuses while anything still cuts with it.
#[derive(Debug)]
pub struct RemoveMulticamGroup {
    group: MulticamId,
    /// The whole group, kept so undo restores it exactly — offsets, names,
    /// sync method and angle IDs included.
    removed: Option<MulticamGroup>,
}

impl RemoveMulticamGroup {
    pub fn new(group: MulticamId) -> Self {
        RemoveMulticamGroup { group, removed: None }
    }
}

impl Command for RemoveMulticamGroup {
    fn name(&self) -> &str {
        "Delete Multicam Group"
    }

    fn apply(&mut self, project: &mut Project) -> Result<(), CommandError> {
        self.removed = Some(project.remove_multicam_group(self.group)?);
        Ok(())
    }

    fn undo(&mut self, project: &mut Project) -> Result<(), CommandError> {
        let group = self
            .removed
            .take()
            .ok_or_else(|| CommandError::Rejected("group was never removed".into()))?;
        project.multicams.push(group);
        Ok(())
    }

    fn as_any(&self) -> &dyn Any {
        self
    }
}

/// Replaces every offset in a group with a freshly measured set.
///
/// The offsets arrive already measured — see this module's note on why. Applied
/// wholesale rather than one angle at a time, because a sync is one answer about
/// the whole group and undoing half of it would leave it in a state nothing
/// produced.
#[derive(Debug)]
pub struct ResyncMulticamGroup {
    group: MulticamId,
    offsets: Vec<(AngleId, Ticks)>,
    method: SyncMethod,
    /// Whether to shift the result so the earliest camera sits at group zero.
    rebase: bool,
    previous: Option<(Vec<(AngleId, Ticks)>, SyncMethod)>,
}

impl ResyncMulticamGroup {
    pub fn new(group: MulticamId, offsets: Vec<(AngleId, Ticks)>, method: SyncMethod) -> Self {
        ResyncMulticamGroup { group, offsets, method, rebase: true, previous: None }
    }

    /// Leaves the shared zero where the measurement put it, rather than moving
    /// it to the earliest camera. Used when the offsets are being entered by
    /// hand and the user means them literally.
    pub fn without_rebase(mut self) -> Self {
        self.rebase = false;
        self
    }
}

impl Command for ResyncMulticamGroup {
    fn name(&self) -> &str {
        "Sync Multicam Group"
    }

    fn apply(&mut self, project: &mut Project) -> Result<(), CommandError> {
        let group = project
            .multicam_mut(self.group)
            .ok_or(CommandError::Core(ve_core::CoreError::MulticamNotFound(self.group)))?;

        // Every named angle has to exist before anything is written, so a
        // rejected sync leaves the group exactly as it was.
        for (angle, _) in &self.offsets {
            if group.angle(*angle).is_none() {
                return Err(CommandError::Core(ve_core::CoreError::AngleNotFound(*angle)));
            }
        }

        if self.previous.is_none() {
            self.previous =
                Some((group.angles.iter().map(|a| (a.id, a.offset)).collect(), group.sync));
        }

        for (angle, offset) in &self.offsets {
            if let Some(a) = group.angle_mut(*angle) {
                a.offset = *offset;
            }
        }
        group.sync = self.method;
        if self.rebase {
            group.rebase_to_earliest();
        }
        Ok(())
    }

    fn undo(&mut self, project: &mut Project) -> Result<(), CommandError> {
        let (offsets, method) = self
            .previous
            .clone()
            .ok_or_else(|| CommandError::Rejected("group was never synced".into()))?;
        let group = project
            .multicam_mut(self.group)
            .ok_or(CommandError::Core(ve_core::CoreError::MulticamNotFound(self.group)))?;
        for (angle, offset) in offsets {
            if let Some(a) = group.angle_mut(angle) {
                a.offset = offset;
            }
        }
        group.sync = method;
        Ok(())
    }

    fn as_any(&self) -> &dyn Any {
        self
    }
}

/// Nudges one angle's offset.
///
/// Merges, so dragging an angle into place in the viewer is one undo step. Sets
/// the group's method to [`SyncMethod::Manual`]: once a number has been typed
/// over, saying the group was synced by audio is no longer true.
#[derive(Debug)]
pub struct SetAngleOffset {
    group: MulticamId,
    angle: AngleId,
    offset: Ticks,
    previous: Option<(Ticks, SyncMethod)>,
}

impl SetAngleOffset {
    pub fn new(group: MulticamId, angle: AngleId, offset: Ticks) -> Self {
        SetAngleOffset { group, angle, offset, previous: None }
    }
}

impl Command for SetAngleOffset {
    fn name(&self) -> &str {
        "Set Angle Offset"
    }

    fn apply(&mut self, project: &mut Project) -> Result<(), CommandError> {
        let (group_id, offset) = (self.group, self.offset);
        let group = project
            .multicam_mut(group_id)
            .ok_or(CommandError::Core(ve_core::CoreError::MulticamNotFound(group_id)))?;
        let method = group.sync;
        let angle = group
            .angle_mut(self.angle)
            .ok_or(CommandError::Core(ve_core::CoreError::AngleNotFound(self.angle)))?;
        let was = angle.offset;
        angle.offset = offset;
        group.sync = SyncMethod::Manual;
        self.previous.get_or_insert((was, method));
        Ok(())
    }

    fn undo(&mut self, project: &mut Project) -> Result<(), CommandError> {
        let (offset, method) = self
            .previous
            .ok_or_else(|| CommandError::Rejected("offset was never set".into()))?;
        let group = project
            .multicam_mut(self.group)
            .ok_or(CommandError::Core(ve_core::CoreError::MulticamNotFound(self.group)))?;
        if let Some(angle) = group.angle_mut(self.angle) {
            angle.offset = offset;
        }
        group.sync = method;
        Ok(())
    }

    fn merge(&mut self, next: &dyn Command) -> bool {
        match next.as_any().downcast_ref::<SetAngleOffset>() {
            Some(other) if other.group == self.group && other.angle == self.angle => {
                self.offset = other.offset;
                true
            }
            _ => false,
        }
    }

    fn as_any(&self) -> &dyn Any {
        self
    }
}

/// Leaves an angle out of the viewer and out of cutting, without removing it.
#[derive(Debug)]
pub struct SetAngleEnabled {
    group: MulticamId,
    angle: AngleId,
    enabled: bool,
    previous: Option<bool>,
}

impl SetAngleEnabled {
    pub fn new(group: MulticamId, angle: AngleId, enabled: bool) -> Self {
        SetAngleEnabled { group, angle, enabled, previous: None }
    }
}

impl Command for SetAngleEnabled {
    fn name(&self) -> &str {
        if self.enabled {
            "Enable Angle"
        } else {
            "Disable Angle"
        }
    }

    fn apply(&mut self, project: &mut Project) -> Result<(), CommandError> {
        let angle = angle_mut(project, self.group, self.angle)?;
        self.previous.get_or_insert(angle.enabled);
        angle.enabled = self.enabled;
        Ok(())
    }

    fn undo(&mut self, project: &mut Project) -> Result<(), CommandError> {
        let previous =
            self.previous.ok_or_else(|| CommandError::Rejected("never applied".into()))?;
        angle_mut(project, self.group, self.angle)?.enabled = previous;
        Ok(())
    }

    fn as_any(&self) -> &dyn Any {
        self
    }
}

fn angle_mut(
    project: &mut Project,
    group: MulticamId,
    angle: AngleId,
) -> Result<&mut MulticamAngle, CommandError> {
    project
        .multicam_mut(group)
        .ok_or(CommandError::Core(ve_core::CoreError::MulticamNotFound(group)))?
        .angle_mut(angle)
        .ok_or(CommandError::Core(ve_core::CoreError::AngleNotFound(angle)))
}

/// Switches which camera a clip shows, over its whole length.
///
/// The smallest edit in the whole feature: one field, and everything else about
/// the clip stays exactly as it was.
#[derive(Debug)]
pub struct SetClipAngle {
    sequence: SequenceId,
    clip: ClipId,
    angle: AngleId,
    previous: Option<Source>,
}

impl SetClipAngle {
    pub fn new(sequence: SequenceId, clip: ClipId, angle: AngleId) -> Self {
        SetClipAngle { sequence, clip, angle, previous: None }
    }
}

impl Command for SetClipAngle {
    fn name(&self) -> &str {
        "Set Angle"
    }

    fn apply(&mut self, project: &mut Project) -> Result<(), CommandError> {
        let angle = self.angle;
        let clip = clip_mut(project, self.sequence, self.clip)?;
        let switched = clip
            .source
            .with_angle(angle)
            .ok_or_else(|| CommandError::Rejected("that clip is not a multicam clip".into()))?;
        self.previous.get_or_insert(clip.source);
        clip.source = switched;
        Ok(())
    }

    fn undo(&mut self, project: &mut Project) -> Result<(), CommandError> {
        let previous =
            self.previous.ok_or_else(|| CommandError::Rejected("never applied".into()))?;
        clip_mut(project, self.sequence, self.clip)?.source = previous;
        Ok(())
    }

    fn as_any(&self) -> &dyn Any {
        self
    }
}

/// Cuts to another camera at a point in time.
///
/// The gesture multicam exists for: while watching the angle viewer, press a
/// number and the clip splits there with everything after it on that camera.
/// Pressing a number at the very start of a clip switches it whole rather than
/// leaving a zero-length fragment behind.
///
/// One command rather than a [`crate::Compound`] of a split and an angle change,
/// because the split mints the right-hand clip's ID as it runs and a compound
/// built in advance has no way to name it.
#[derive(Debug)]
pub struct CutToAngle {
    sequence: SequenceId,
    track: TrackId,
    clip: ClipId,
    at: Ticks,
    angle: AngleId,
    /// The split, once it is known one is needed. `None` means the cut landed on
    /// the clip's own start and the whole clip was switched instead.
    split: Option<SplitClip>,
    /// What the switched clip showed before, for the no-split case.
    previous: Option<Source>,
    label: String,
}

impl CutToAngle {
    pub fn new(
        sequence: SequenceId,
        track: TrackId,
        clip: ClipId,
        at: Ticks,
        angle: AngleId,
        number: usize,
    ) -> Self {
        CutToAngle {
            sequence,
            track,
            clip,
            at,
            angle,
            split: None,
            previous: None,
            label: format!("Cut to Angle {number}"),
        }
    }

    /// The clip carrying the new angle once the command has run: the right-hand
    /// half of the split, or the original clip when no split was needed.
    pub fn cut_clip(&self) -> ClipId {
        self.split.as_ref().and_then(|s| s.right_id()).unwrap_or(self.clip)
    }
}

impl Command for CutToAngle {
    fn name(&self) -> &str {
        &self.label
    }

    fn apply(&mut self, project: &mut Project) -> Result<(), CommandError> {
        // On redo the decision has already been made; repeating it would be a
        // second chance to decide differently, which is exactly what a redo
        // must not be.
        if let Some(split) = self.split.as_mut() {
            split.apply(project)?;
            let right = split.right_id().expect("a split that applied has an id");
            return set_angle(project, self.sequence, right, self.angle);
        }
        if self.previous.is_some() {
            return set_angle(project, self.sequence, self.clip, self.angle);
        }

        let clip = crate::clip_of(project, self.sequence, self.track, self.clip)?;
        let (start, end) = (clip.timeline_start, clip.timeline_end());
        if clip.source.multicam().is_none() {
            return Err(CommandError::Rejected("that clip is not a multicam clip".into()));
        }
        if clip.source.multicam().map(|(_, a)| a) == Some(self.angle) {
            return Err(CommandError::Rejected("already on that angle".into()));
        }

        // A cut at or before the clip's own start, or past its end, has no
        // right-hand part to make: switch the clip whole.
        if self.at <= start || self.at >= end {
            self.previous = Some(clip.source);
            return set_angle(project, self.sequence, self.clip, self.angle);
        }

        let mut split = SplitClip::new(self.sequence, self.track, self.clip, self.at);
        split.apply(project)?;
        let right = split.right_id().expect("a split that applied has an id");
        // The angle is set after the split so that a failure to split leaves
        // nothing changed, rather than a switched clip that was never cut.
        let result = set_angle(project, self.sequence, right, self.angle);
        if result.is_err() {
            split.undo(project)?;
            return result;
        }
        self.split = Some(split);
        Ok(())
    }

    fn undo(&mut self, project: &mut Project) -> Result<(), CommandError> {
        // Undoing the split removes the right-hand clip outright, which takes
        // the angle change with it; there is nothing else to put back.
        if let Some(split) = self.split.as_mut() {
            return split.undo(project);
        }
        let previous =
            self.previous.ok_or_else(|| CommandError::Rejected("never applied".into()))?;
        clip_mut(project, self.sequence, self.clip)?.source = previous;
        Ok(())
    }

    fn as_any(&self) -> &dyn Any {
        self
    }
}

fn set_angle(
    project: &mut Project,
    sequence: SequenceId,
    clip: ClipId,
    angle: AngleId,
) -> Result<(), CommandError> {
    let clip = clip_mut(project, sequence, clip)?;
    clip.source = clip
        .source
        .with_angle(angle)
        .ok_or_else(|| CommandError::Rejected("that clip is not a multicam clip".into()))?;
    Ok(())
}
