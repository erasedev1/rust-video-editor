//! Multicam: several cameras covering the same event, cut between as one clip.
//!
//! # What a group is
//!
//! A [`MulticamGroup`] is a set of angles sharing one **group timeline**. Each
//! angle carries the offset from that shared timeline into its own media, so
//! group time `t` reads angle `a` at `t + a.offset`. Establishing those offsets
//! is what "syncing" means; everything after it is arithmetic.
//!
//! # Why a clip names the angle
//!
//! A clip draws [`crate::Source::Multicam`], which carries *both* the group and
//! the angle. Cutting between angles therefore changes one field and nothing
//! else: the clip keeps its position, its length, its source window, its
//! effects and its animation, because none of those are about which camera is
//! being watched. That is what makes a multicam cut instant and what makes it
//! undoable as a single, tiny command.
//!
//! The alternative — swapping the clip's source asset and rewriting `source_in`
//! by the difference of two offsets — would mean every angle change was a
//! different edit depending on which angle it came from, and a mis-synced group
//! would leave a trail of clips already rewritten with the wrong numbers.
//!
//! # An angle is a camera
//!
//! [`MulticamAngle::asset`] is an [`AssetId`], not a [`crate::Source`]. A
//! composition cannot be an angle, and neither can another group.
//!
//! That restriction is deliberate rather than incidental. Allowing a
//! composition would add a second kind of edge to the nesting graph — a clip
//! reaching a composition *through* a group — and the cycle check that keeps a
//! render from never finishing would have to know about groups to stay correct.
//! Nothing is lost: an angle that needs work done to it is a clip with effects
//! on it, or a multicam clip nested inside a composition, which is the
//! direction nesting already runs.

use serde::{Deserialize, Serialize};
use ve_time::Ticks;

use crate::id::{AngleId, AssetId, MulticamId};
use crate::CoreError;

/// How a group's offsets were arrived at.
///
/// Recorded so the interface can say what it did and the user can tell a
/// measured offset from one they typed. It is a fact about the last sync, not
/// an instruction: editing an offset by hand leaves the method as it was and
/// marks the group [`SyncMethod::Manual`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SyncMethod {
    /// Offsets were entered or adjusted by hand.
    #[default]
    Manual,
    /// Offsets came from the start timecode each file records.
    Timecode,
    /// Offsets were measured by correlating the angles' sound.
    Audio,
    /// Every angle starts at zero: the files are already aligned, or nothing
    /// better was available.
    Start,
}

impl SyncMethod {
    pub const ALL: [SyncMethod; 4] =
        [SyncMethod::Manual, SyncMethod::Timecode, SyncMethod::Audio, SyncMethod::Start];

    pub fn label(self) -> &'static str {
        match self {
            SyncMethod::Manual => "Manual",
            SyncMethod::Timecode => "Timecode",
            SyncMethod::Audio => "Audio",
            SyncMethod::Start => "Start of file",
        }
    }

    pub fn description(self) -> &'static str {
        match self {
            SyncMethod::Manual => "Offsets as they were entered.",
            SyncMethod::Timecode => "From the start timecode each file records.",
            SyncMethod::Audio => "Measured by correlating what the cameras heard.",
            SyncMethod::Start => "Every angle starts together.",
        }
    }
}

/// One camera in a group.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct MulticamAngle {
    pub id: AngleId,
    /// Display name, defaulting to the asset's but user-editable — "Wide",
    /// "Close", "Camera B".
    pub name: String,
    pub asset: AssetId,
    /// Where group time zero sits inside this angle's own media.
    ///
    /// A camera that started rolling ten seconds before the group's zero has an
    /// offset of ten seconds: at group time zero it is ten seconds into its own
    /// recording. A camera that started *after* group zero has a negative
    /// offset and simply has no picture until group time reaches it.
    #[serde(default)]
    pub offset: Ticks,
    /// Unset to leave an angle out of the viewer and out of cutting without
    /// removing it, which would renumber every angle after it.
    #[serde(default = "crate::default_true")]
    pub enabled: bool,
}

impl MulticamAngle {
    pub fn new(id: AngleId, name: impl Into<String>, asset: AssetId) -> Self {
        MulticamAngle { id, name: name.into(), asset, offset: Ticks::ZERO, enabled: true }
    }

    pub fn with_offset(mut self, offset: Ticks) -> Self {
        self.offset = offset;
        self
    }

    /// Where in this angle's media group time `at` falls.
    ///
    /// May be negative, which means the camera was not rolling yet. Callers
    /// decide what to do about that; [`MulticamGroup::source_time_at`] is the
    /// one that refuses.
    #[inline]
    pub fn source_time_at(&self, at: Ticks) -> Ticks {
        at + self.offset
    }

    /// The span of group time this angle has picture for, given how long its
    /// media runs.
    ///
    /// Starts at `-offset` — the group time at which the angle's own zero
    /// falls — and runs for the length of the media.
    pub fn coverage(&self, media_duration: Ticks) -> ve_time::TimeRange {
        ve_time::TimeRange::new(Ticks::ZERO - self.offset, media_duration)
    }
}

/// Several angles on one timeline.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct MulticamGroup {
    pub id: MulticamId,
    pub name: String,
    /// In the order they are numbered: index 0 is angle 1, which is what the
    /// interface's number keys cut to.
    pub angles: Vec<MulticamAngle>,
    #[serde(default)]
    pub sync: SyncMethod,
}

impl MulticamGroup {
    pub fn new(id: MulticamId, name: impl Into<String>) -> Self {
        MulticamGroup { id, name: name.into(), angles: Vec::new(), sync: SyncMethod::default() }
    }

    #[inline]
    pub fn len(&self) -> usize {
        self.angles.len()
    }

    #[inline]
    pub fn is_empty(&self) -> bool {
        self.angles.is_empty()
    }

    pub fn angle(&self, id: AngleId) -> Option<&MulticamAngle> {
        self.angles.iter().find(|a| a.id == id)
    }

    pub fn angle_mut(&mut self, id: AngleId) -> Option<&mut MulticamAngle> {
        self.angles.iter_mut().find(|a| a.id == id)
    }

    pub fn index_of(&self, id: AngleId) -> Option<usize> {
        self.angles.iter().position(|a| a.id == id)
    }

    /// The angle a one-based number selects, as the interface's number keys and
    /// the viewer's tiles label them.
    ///
    /// Numbered over *every* angle rather than only the enabled ones, so
    /// disabling an angle does not silently renumber the keys under the user's
    /// fingers mid-edit.
    pub fn angle_numbered(&self, number: usize) -> Option<&MulticamAngle> {
        self.angles.get(number.checked_sub(1)?)
    }

    /// The angle a newly cut clip should show: the first enabled one.
    pub fn default_angle(&self) -> Option<AngleId> {
        self.angles.iter().find(|a| a.enabled).or(self.angles.first()).map(|a| a.id)
    }

    /// Every angle that is switched on, in number order.
    pub fn enabled_angles(&self) -> impl Iterator<Item = &MulticamAngle> {
        self.angles.iter().filter(|a| a.enabled)
    }

    /// Where in `angle`'s media group time `at` falls, or `None` when that
    /// angle was not rolling yet.
    pub fn source_time_at(&self, angle: AngleId, at: Ticks) -> Option<Ticks> {
        let source = self.angle(angle)?.source_time_at(at);
        (!source.is_negative()).then_some(source)
    }

    /// Adds an angle, refusing a duplicate camera.
    ///
    /// The same file twice is not two angles; it is one angle and a mistake,
    /// and a group holding it would offer two identical tiles in the viewer.
    pub fn push_angle(&mut self, angle: MulticamAngle) -> Result<AngleId, CoreError> {
        if self.angles.iter().any(|a| a.asset == angle.asset) {
            return Err(CoreError::AngleDuplicated(angle.asset));
        }
        let id = angle.id;
        self.angles.push(angle);
        Ok(id)
    }

    pub fn remove_angle(&mut self, id: AngleId) -> Result<MulticamAngle, CoreError> {
        let index = self.index_of(id).ok_or(CoreError::AngleNotFound(id))?;
        Ok(self.angles.remove(index))
    }

    /// Shifts every offset so the earliest angle sits at group time zero.
    ///
    /// Sync measures *relative* offsets; where the shared zero lands is a free
    /// choice, and putting it at the first frame any camera recorded means a
    /// group's timeline starts where its coverage does rather than at a
    /// negative number nobody chose.
    ///
    /// Returns how far everything moved, so a caller holding clip positions in
    /// group time can shift them to match.
    pub fn rebase_to_earliest(&mut self) -> Ticks {
        let Some(latest) = self.angles.iter().map(|a| a.offset).max() else {
            return Ticks::ZERO;
        };
        if latest.is_zero() {
            return Ticks::ZERO;
        }
        for angle in &mut self.angles {
            angle.offset -= latest;
        }
        latest
    }

    /// Repairs a group read from an untrusted file, reporting what it changed.
    pub(crate) fn normalise(&mut self) -> Vec<String> {
        let mut warnings = Vec::new();

        // Two angles on the same file would show the same picture under two
        // numbers, and a cut to either would be indistinguishable afterwards.
        let mut seen: Vec<AssetId> = Vec::new();
        let name = self.name.clone();
        self.angles.retain(|angle| {
            if seen.contains(&angle.asset) {
                warnings.push(format!(
                    "multicam '{name}': angle '{}' repeats a camera already in the group; \
                     removed",
                    angle.name
                ));
                false
            } else {
                seen.push(angle.asset);
                true
            }
        });

        // Duplicate IDs would make `angle()` return whichever came first and a
        // cut to the other silently land on it.
        let mut ids: Vec<AngleId> = Vec::new();
        self.angles.retain(|angle| {
            if ids.contains(&angle.id) {
                warnings.push(format!(
                    "multicam '{name}': angle '{}' repeats an id; removed",
                    angle.name
                ));
                false
            } else {
                ids.push(angle.id);
                true
            }
        });

        warnings
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::id::{AngleId, AssetId, MulticamId};

    fn group() -> MulticamGroup {
        let mut g = MulticamGroup::new(MulticamId::from_raw(1), "Interview");
        for (n, offset) in [(1i64, 0i64), (2, 5), (3, -2)] {
            g.push_angle(
                MulticamAngle::new(
                    AngleId::from_raw(10 + n as u64),
                    format!("Cam {n}"),
                    AssetId::from_raw(100 + n as u64),
                )
                .with_offset(Ticks::from_seconds(offset)),
            )
            .unwrap();
        }
        g
    }

    #[test]
    fn group_time_reads_each_angle_at_its_own_offset() {
        let g = group();
        let at = Ticks::from_seconds(10);
        assert_eq!(g.source_time_at(AngleId::from_raw(11), at), Some(Ticks::from_seconds(10)));
        assert_eq!(g.source_time_at(AngleId::from_raw(12), at), Some(Ticks::from_seconds(15)));
        assert_eq!(g.source_time_at(AngleId::from_raw(13), at), Some(Ticks::from_seconds(8)));
    }

    /// The case a negative offset exists for: a camera that started late has no
    /// picture at the beginning of the group, and saying so is not the same as
    /// reading frame zero.
    #[test]
    fn an_angle_that_was_not_rolling_yet_has_no_source_time() {
        let mut g = group();
        g.angle_mut(AngleId::from_raw(11)).unwrap().offset = Ticks::from_seconds(-4);

        assert_eq!(g.source_time_at(AngleId::from_raw(11), Ticks::ZERO), None);
        assert_eq!(g.source_time_at(AngleId::from_raw(11), Ticks::from_seconds(3)), None);
        assert_eq!(
            g.source_time_at(AngleId::from_raw(11), Ticks::from_seconds(4)),
            Some(Ticks::ZERO),
            "the first frame it has is exactly where the offset puts it"
        );
    }

    #[test]
    fn an_angles_coverage_starts_where_its_own_zero_lands_on_the_group() {
        let angle = MulticamAngle::new(AngleId::from_raw(1), "late", AssetId::from_raw(1))
            .with_offset(Ticks::from_seconds(-6));
        let coverage = angle.coverage(Ticks::from_seconds(30));
        assert_eq!(coverage.start, Ticks::from_seconds(6));
        assert_eq!(coverage.end(), Ticks::from_seconds(36));
        assert!(!coverage.contains(Ticks::from_seconds(2)));
        assert!(coverage.contains(Ticks::from_seconds(10)));
    }

    #[test]
    fn angles_are_numbered_from_one_in_the_order_they_were_added() {
        let g = group();
        assert_eq!(g.angle_numbered(1).map(|a| a.id), Some(AngleId::from_raw(11)));
        assert_eq!(g.angle_numbered(3).map(|a| a.id), Some(AngleId::from_raw(13)));
        assert_eq!(g.angle_numbered(4), None);
        assert_eq!(g.angle_numbered(0), None, "there is no angle zero");
    }

    /// Disabling an angle must not shuffle the keys under the user's fingers.
    #[test]
    fn disabling_an_angle_does_not_renumber_the_others() {
        let mut g = group();
        g.angle_mut(AngleId::from_raw(12)).unwrap().enabled = false;

        assert_eq!(g.angle_numbered(3).map(|a| a.id), Some(AngleId::from_raw(13)));
        assert_eq!(g.enabled_angles().count(), 2);
        assert_eq!(g.default_angle(), Some(AngleId::from_raw(11)));
    }

    #[test]
    fn the_default_angle_skips_a_disabled_first_angle() {
        let mut g = group();
        g.angle_mut(AngleId::from_raw(11)).unwrap().enabled = false;
        assert_eq!(g.default_angle(), Some(AngleId::from_raw(12)));
    }

    #[test]
    fn the_same_camera_cannot_be_two_angles() {
        let mut g = group();
        let duplicate =
            MulticamAngle::new(AngleId::from_raw(99), "Cam 1 again", AssetId::from_raw(101));
        assert!(matches!(g.push_angle(duplicate), Err(CoreError::AngleDuplicated(_))));
        assert_eq!(g.len(), 3);
    }

    #[test]
    fn rebasing_puts_the_earliest_camera_at_group_zero() {
        let mut g = group();
        // Offsets 0, 5 and -2: the camera that started earliest is the one with
        // the largest offset, because it has the most footage before the sync
        // point.
        let moved = g.rebase_to_earliest();
        assert_eq!(moved, Ticks::from_seconds(5));

        let offsets: Vec<i64> =
            g.angles.iter().map(|a| a.offset.as_secs_f64().round() as i64).collect();
        assert_eq!(offsets, vec![-5, 0, -7]);

        // And the relative alignment is untouched, which is the whole point.
        let at = Ticks::from_seconds(20);
        let a = g.source_time_at(AngleId::from_raw(11), at).unwrap();
        let b = g.source_time_at(AngleId::from_raw(12), at).unwrap();
        assert_eq!(b - a, Ticks::from_seconds(5));
    }

    #[test]
    fn rebasing_a_group_already_at_zero_changes_nothing() {
        let mut g = MulticamGroup::new(MulticamId::from_raw(1), "Aligned");
        for n in 1..=2u64 {
            g.push_angle(MulticamAngle::new(
                AngleId::from_raw(n),
                format!("Cam {n}"),
                AssetId::from_raw(n),
            ))
            .unwrap();
        }
        let before = g.clone();
        assert_eq!(g.rebase_to_earliest(), Ticks::ZERO);
        assert_eq!(g, before);
    }

    #[test]
    fn normalise_drops_a_repeated_camera_and_says_so() {
        let mut g = group();
        // Only a hand-edited file can get here; `push_angle` refuses it.
        g.angles.push(MulticamAngle::new(
            AngleId::from_raw(20),
            "Cam 1 again",
            AssetId::from_raw(101),
        ));
        let warnings = g.normalise();
        assert_eq!(g.len(), 3);
        assert_eq!(warnings.len(), 1);
        assert!(warnings[0].contains("repeats a camera"), "{}", warnings[0]);
    }

    #[test]
    fn normalise_drops_a_repeated_id() {
        let mut g = group();
        g.angles.push(MulticamAngle::new(
            AngleId::from_raw(11),
            "impostor",
            AssetId::from_raw(999),
        ));
        let warnings = g.normalise();
        assert_eq!(g.len(), 3);
        assert_eq!(g.angle(AngleId::from_raw(11)).map(|a| a.name.as_str()), Some("Cam 1"));
        assert!(warnings[0].contains("repeats an id"), "{}", warnings[0]);
    }

    #[test]
    fn removing_an_angle_returns_it_and_refuses_a_stranger() {
        let mut g = group();
        let gone = g.remove_angle(AngleId::from_raw(12)).unwrap();
        assert_eq!(gone.name, "Cam 2");
        assert_eq!(g.len(), 2);
        assert!(matches!(
            g.remove_angle(AngleId::from_raw(12)),
            Err(CoreError::AngleNotFound(_))
        ));
    }
}
