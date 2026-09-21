//! Building, syncing and cutting with a multicam group, through the history.
//!
//! What is asserted here is the command contract: a refused command touches
//! nothing, an undo restores the project exactly, and a redo reproduces the
//! edit — including the IDs, so a selection survives the round trip.

use ve_command::*;
use ve_core::{
    AngleId, AssetId, AudioStreamInfo, Clip, ClipId, MediaInfo, MulticamId, Project,
    SequenceId, Size, Source, SyncMethod, TrackId, TrackKind, VideoStreamInfo,
};
use ve_time::{Rate, SampleRate, Ticks};

/// A project with its ID allocator zeroed, for comparing two states.
///
/// Undo deliberately does **not** rewind the allocator — see `ve-core`'s ID
/// notes: an ID handed out once is never handed out again, so that a stale
/// reference resolves to "missing" rather than to whatever was created next.
/// That means an undone edit leaves a project equal to the original in every
/// way except how many IDs have been spent, and these comparisons say so
/// explicitly rather than working around it with a snapshot taken at just the
/// right moment.
fn shape(project: &Project) -> Project {
    let mut project = project.clone();
    project.ids = ve_core::IdAllocator::new();
    project
}

fn info(seconds: i64) -> MediaInfo {
    MediaInfo {
        duration: Ticks::from_seconds(seconds),
        video: Some(VideoStreamInfo {
            size: Size::new(1920, 1080),
            rate: Rate::FPS_25,
            duration: Ticks::from_seconds(seconds),
            frame_count: Some(seconds * 25),
            codec: "h264".into(),
            pixel_format: "yuv420p".into(),
            sample_aspect_ratio: (1, 1),
        }),
        audio: Some(AudioStreamInfo {
            sample_rate: SampleRate::HZ_48000,
            channels: 2,
            duration: Ticks::from_seconds(seconds),
            codec: "aac".into(),
        }),
        container: "mp4".into(),
        timecode: None,
    }
}

struct Fixture {
    project: Project,
    sequence: SequenceId,
    track: TrackId,
    assets: Vec<AssetId>,
    history: History,
}

fn fixture() -> Fixture {
    let mut project = Project::with_default_sequence("Multicam");
    let assets: Vec<AssetId> = ["a.mp4", "b.mp4", "c.mp4"]
        .iter()
        .map(|name| project.add_asset(format!("/media/{name}"), info(120)))
        .collect();
    let sequence = project.active_sequence.unwrap();
    let track = project
        .sequence(sequence)
        .unwrap()
        .tracks
        .iter()
        .find(|t| t.kind == TrackKind::Video)
        .unwrap()
        .id;
    Fixture { project, sequence, track, assets, history: History::default() }
}

impl Fixture {
    fn angles(&self) -> Vec<NewAngle> {
        self.assets
            .iter()
            .enumerate()
            .map(|(i, a)| NewAngle::new(*a, format!("Cam {}", i + 1)))
            .collect()
    }

    /// Creates a group and returns it with its angle IDs.
    fn group(&mut self) -> (MulticamId, Vec<AngleId>) {
        let mut command =
            CreateMulticamGroup::new("Interview", self.angles(), SyncMethod::Start);
        command.apply(&mut self.project).unwrap();
        let ids = (command.group_id().unwrap(), command.angle_ids().to_vec());
        // Re-run it through the history so the undo stack knows about it.
        command.undo(&mut self.project).unwrap();
        self.history.execute(&mut self.project, Box::new(command)).unwrap();
        ids
    }

    fn place(&mut self, source: Source, start_s: i64, dur_s: i64) -> ClipId {
        let id = self.project.new_clip_id();
        let clip = Clip::new(
            id,
            source,
            "cut",
            Ticks::ZERO,
            Ticks::from_seconds(start_s),
            Ticks::from_seconds(dur_s),
        );
        self.project
            .sequence_mut(self.sequence)
            .unwrap()
            .track_mut(self.track)
            .unwrap()
            .insert_clip(clip)
            .unwrap();
        id
    }

    fn clip_source(&self, clip: ClipId) -> Source {
        self.project.sequence(self.sequence).unwrap().find_clip(clip).unwrap().1.source
    }

    fn clips(&self) -> Vec<(ClipId, Ticks, Ticks, Option<AngleId>)> {
        self.project
            .sequence(self.sequence)
            .unwrap()
            .track(self.track)
            .unwrap()
            .clips()
            .iter()
            .map(|c| (c.id, c.timeline_start, c.duration, c.source.multicam().map(|(_, a)| a)))
            .collect()
    }
}

// ---- building a group ---------------------------------------------------

#[test]
fn creating_a_group_undoes_and_redoes_with_the_same_ids() {
    let mut f = fixture();
    let before = f.project.clone();

    let mut command = CreateMulticamGroup::new("Interview", f.angles(), SyncMethod::Start);
    command.apply(&mut f.project).unwrap();
    let group = command.group_id().unwrap();
    let angles = command.angle_ids().to_vec();
    command.undo(&mut f.project).unwrap();
    assert_eq!(shape(&f.project), shape(&before), "undo must restore the project exactly");

    f.history.execute(&mut f.project, Box::new(command)).unwrap();
    let after = f.project.clone();
    assert_eq!(f.project.multicam(group).unwrap().len(), 3);

    f.history.undo(&mut f.project).unwrap();
    assert!(f.project.multicam(group).is_none());
    f.history.redo(&mut f.project).unwrap();
    assert_eq!(f.project, after, "a redo must reproduce the same group and the same IDs");
    assert_eq!(
        f.project.multicam(group).unwrap().angles.iter().map(|a| a.id).collect::<Vec<_>>(),
        angles
    );
}

#[test]
fn a_group_of_one_camera_is_refused() {
    let mut f = fixture();
    let before = f.project.clone();
    let one = vec![NewAngle::new(f.assets[0], "Only")];

    let result = f.history.execute(
        &mut f.project,
        Box::new(CreateMulticamGroup::new("Lonely", one, SyncMethod::Start)),
    );
    assert!(result.is_err());
    assert_eq!(f.project, before);
    assert!(!f.history.can_undo());
}

#[test]
fn a_group_naming_media_this_project_does_not_have_is_refused() {
    let mut f = fixture();
    let before = f.project.clone();
    let angles = vec![
        NewAngle::new(f.assets[0], "Cam 1"),
        NewAngle::new(AssetId::from_raw(9999), "Ghost"),
    ];

    let result = f.history.execute(
        &mut f.project,
        Box::new(CreateMulticamGroup::new("Broken", angles, SyncMethod::Start)),
    );
    assert!(result.is_err());
    assert_eq!(f.project, before, "a refused create must leave no half-built group");
}

#[test]
fn the_same_camera_twice_is_refused() {
    let mut f = fixture();
    let before = f.project.clone();
    let angles =
        vec![NewAngle::new(f.assets[0], "Cam 1"), NewAngle::new(f.assets[0], "Cam 1 again")];

    let result = f.history.execute(
        &mut f.project,
        Box::new(CreateMulticamGroup::new("Doubled", angles, SyncMethod::Start)),
    );
    assert!(result.is_err());
    assert_eq!(f.project, before);
}

#[test]
fn deleting_a_group_puts_it_back_exactly_on_undo() {
    let mut f = fixture();
    let (group, _) = f.group();
    let with_group = f.project.clone();

    f.history.execute(&mut f.project, Box::new(RemoveMulticamGroup::new(group))).unwrap();
    assert!(f.project.multicam(group).is_none());

    f.history.undo(&mut f.project).unwrap();
    assert_eq!(f.project, with_group, "offsets, names and IDs all have to come back");
}

#[test]
fn a_group_still_being_cut_with_cannot_be_deleted() {
    let mut f = fixture();
    let (group, angles) = f.group();
    f.place(Source::Multicam { group, angle: angles[0] }, 0, 10);
    let before = f.project.clone();

    let result = f.history.execute(&mut f.project, Box::new(RemoveMulticamGroup::new(group)));
    assert!(result.is_err());
    assert_eq!(f.project, before);
}

// ---- syncing ------------------------------------------------------------

#[test]
fn syncing_records_the_offsets_and_the_method() {
    let mut f = fixture();
    let (group, angles) = f.group();
    let before = f.project.clone();

    // As a measurement would report it: camera 2 was rolling first.
    let offsets = vec![
        (angles[0], Ticks::ZERO),
        (angles[1], Ticks::from_seconds(12)),
        (angles[2], Ticks::from_seconds(-3)),
    ];
    f.history
        .execute(
            &mut f.project,
            Box::new(ResyncMulticamGroup::new(group, offsets, SyncMethod::Audio)),
        )
        .unwrap();

    let g = f.project.multicam(group).unwrap();
    assert_eq!(g.sync, SyncMethod::Audio);
    // Rebased so the earliest camera sits at group zero.
    let found: Vec<i64> =
        g.angles.iter().map(|a| a.offset.as_secs_f64().round() as i64).collect();
    assert_eq!(found, vec![-12, 0, -15]);

    f.history.undo(&mut f.project).unwrap();
    assert_eq!(f.project, before, "undo must restore both the offsets and the method");
}

#[test]
fn syncing_without_a_rebase_stores_what_it_was_given() {
    let mut f = fixture();
    let (group, angles) = f.group();
    let offsets = vec![(angles[1], Ticks::from_seconds(5))];

    f.history
        .execute(
            &mut f.project,
            Box::new(
                ResyncMulticamGroup::new(group, offsets, SyncMethod::Manual).without_rebase(),
            ),
        )
        .unwrap();

    let g = f.project.multicam(group).unwrap();
    assert_eq!(g.angles[1].offset, Ticks::from_seconds(5));
    assert_eq!(g.angles[0].offset, Ticks::ZERO, "the others are left alone");
}

#[test]
fn a_sync_naming_an_angle_that_is_not_in_the_group_changes_nothing() {
    let mut f = fixture();
    let (group, angles) = f.group();
    let before = f.project.clone();

    let offsets = vec![
        (angles[0], Ticks::from_seconds(4)),
        (AngleId::from_raw(9999), Ticks::from_seconds(9)),
    ];
    let result = f.history.execute(
        &mut f.project,
        Box::new(ResyncMulticamGroup::new(group, offsets, SyncMethod::Audio)),
    );
    assert!(result.is_err());
    assert_eq!(f.project, before, "the first offset must not have been written either");
}

#[test]
fn nudging_an_angle_is_one_undo_step_and_marks_the_group_manual() {
    let mut f = fixture();
    let (group, angles) = f.group();
    f.history
        .execute(
            &mut f.project,
            Box::new(ResyncMulticamGroup::new(
                group,
                vec![(angles[1], Ticks::from_seconds(2))],
                SyncMethod::Audio,
            )),
        )
        .unwrap();
    assert_eq!(f.project.multicam(group).unwrap().sync, SyncMethod::Audio);

    for ms in [100, 200, 300] {
        f.history
            .execute_coalesced(
                &mut f.project,
                Box::new(SetAngleOffset::new(group, angles[1], Ticks::from_millis(ms))),
            )
            .unwrap();
    }
    f.history.break_merge();

    let g = f.project.multicam(group).unwrap();
    assert_eq!(g.angles[1].offset, Ticks::from_millis(300));
    assert_eq!(g.sync, SyncMethod::Manual, "a typed-over offset is no longer a measurement");

    f.history.undo(&mut f.project).unwrap();
    let g = f.project.multicam(group).unwrap();
    assert_eq!(g.sync, SyncMethod::Audio, "undo restores what the method was");
}

#[test]
fn disabling_an_angle_undoes() {
    let mut f = fixture();
    let (group, angles) = f.group();
    let before = f.project.clone();

    f.history
        .execute(&mut f.project, Box::new(SetAngleEnabled::new(group, angles[1], false)))
        .unwrap();
    assert!(!f.project.multicam(group).unwrap().angles[1].enabled);
    assert_eq!(f.project.multicam(group).unwrap().enabled_angles().count(), 2);

    f.history.undo(&mut f.project).unwrap();
    assert_eq!(f.project, before);
}

// ---- cutting ------------------------------------------------------------

#[test]
fn switching_a_clips_angle_leaves_everything_else_alone() {
    let mut f = fixture();
    let (group, angles) = f.group();
    let clip = f.place(Source::Multicam { group, angle: angles[0] }, 0, 30);
    let before = f.project.clone();

    f.history
        .execute(&mut f.project, Box::new(SetClipAngle::new(f.sequence, clip, angles[2])))
        .unwrap();

    assert_eq!(f.clip_source(clip), Source::Multicam { group, angle: angles[2] });
    assert_eq!(f.clips().len(), 1, "switching is not a cut");

    f.history.undo(&mut f.project).unwrap();
    assert_eq!(f.project, before);
}

#[test]
fn switching_the_angle_of_a_plain_clip_is_refused() {
    let mut f = fixture();
    let (_, angles) = f.group();
    let clip = f.place(Source::Asset(f.assets[0]), 0, 10);
    let before = f.project.clone();

    let result = f
        .history
        .execute(&mut f.project, Box::new(SetClipAngle::new(f.sequence, clip, angles[1])));
    assert!(result.is_err());
    assert_eq!(f.project, before);
}

/// The gesture the whole feature is for.
#[test]
fn cutting_to_an_angle_splits_there_and_switches_only_what_follows() {
    let mut f = fixture();
    let (group, angles) = f.group();
    let clip = f.place(Source::Multicam { group, angle: angles[0] }, 0, 30);
    let before = f.project.clone();

    let command =
        CutToAngle::new(f.sequence, f.track, clip, Ticks::from_seconds(10), angles[1], 2);
    assert_eq!(command.name(), "Cut to Angle 2");
    f.history.execute(&mut f.project, Box::new(command)).unwrap();

    let clips = f.clips();
    assert_eq!(clips.len(), 2, "the cut makes two clips");
    assert_eq!(clips[0].1, Ticks::ZERO);
    assert_eq!(clips[0].2, Ticks::from_seconds(10));
    assert_eq!(clips[0].3, Some(angles[0]), "what came before the cut is untouched");
    assert_eq!(clips[1].1, Ticks::from_seconds(10));
    assert_eq!(clips[1].2, Ticks::from_seconds(20));
    assert_eq!(clips[1].3, Some(angles[1]), "and everything after is on the new camera");

    f.history.undo(&mut f.project).unwrap();
    assert_eq!(shape(&f.project), shape(&before), "undoing a cut takes the split back too");
}

#[test]
fn a_cut_redoes_to_the_same_clips() {
    let mut f = fixture();
    let (group, angles) = f.group();
    let clip = f.place(Source::Multicam { group, angle: angles[0] }, 0, 30);

    f.history
        .execute(
            &mut f.project,
            Box::new(CutToAngle::new(
                f.sequence,
                f.track,
                clip,
                Ticks::from_seconds(10),
                angles[1],
                2,
            )),
        )
        .unwrap();
    let after = f.project.clone();

    f.history.undo(&mut f.project).unwrap();
    f.history.redo(&mut f.project).unwrap();
    assert_eq!(f.project, after, "the redone cut must produce the same clip IDs");
}

/// Pressing a number with the playhead at the very start of a clip should
/// switch it, not leave a zero-length fragment behind.
#[test]
fn a_cut_at_the_start_of_a_clip_switches_it_whole() {
    let mut f = fixture();
    let (group, angles) = f.group();
    let clip = f.place(Source::Multicam { group, angle: angles[0] }, 5, 30);
    let before = f.project.clone();

    f.history
        .execute(
            &mut f.project,
            Box::new(CutToAngle::new(
                f.sequence,
                f.track,
                clip,
                Ticks::from_seconds(5),
                angles[2],
                3,
            )),
        )
        .unwrap();

    let clips = f.clips();
    assert_eq!(clips.len(), 1, "no split, and no empty fragment");
    assert_eq!(clips[0].3, Some(angles[2]));

    f.history.undo(&mut f.project).unwrap();
    assert_eq!(shape(&f.project), shape(&before));
}

#[test]
fn cutting_to_the_angle_already_showing_is_refused_rather_than_splitting() {
    let mut f = fixture();
    let (group, angles) = f.group();
    let clip = f.place(Source::Multicam { group, angle: angles[1] }, 0, 30);
    let before = f.project.clone();

    let result = f.history.execute(
        &mut f.project,
        Box::new(CutToAngle::new(
            f.sequence,
            f.track,
            clip,
            Ticks::from_seconds(10),
            angles[1],
            2,
        )),
    );
    assert!(result.is_err(), "a cut to the camera already on screen is not an edit");
    assert_eq!(f.project, before, "and it must not have split anything");
}

#[test]
fn cutting_a_plain_clip_is_refused_without_splitting_it() {
    let mut f = fixture();
    let (_, angles) = f.group();
    let clip = f.place(Source::Asset(f.assets[0]), 0, 30);
    let before = f.project.clone();

    let result = f.history.execute(
        &mut f.project,
        Box::new(CutToAngle::new(
            f.sequence,
            f.track,
            clip,
            Ticks::from_seconds(10),
            angles[1],
            2,
        )),
    );
    assert!(result.is_err());
    assert_eq!(f.project, before, "a refused cut must not leave the clip split");
}

#[test]
fn several_cuts_build_a_sequence_of_angles() {
    let mut f = fixture();
    let (group, angles) = f.group();
    let mut clip = f.place(Source::Multicam { group, angle: angles[0] }, 0, 60);

    for (at, angle) in [(10i64, 1usize), (25, 2), (40, 0)] {
        let mut command = CutToAngle::new(
            f.sequence,
            f.track,
            clip,
            Ticks::from_seconds(at),
            angles[angle],
            angle + 1,
        );
        command.apply(&mut f.project).unwrap();
        let next = command.cut_clip();
        command.undo(&mut f.project).unwrap();
        f.history.execute(&mut f.project, Box::new(command)).unwrap();
        clip = next;
    }

    let clips = f.clips();
    assert_eq!(clips.len(), 4);
    let shown: Vec<Option<usize>> =
        clips.iter().map(|c| c.3.and_then(|a| angles.iter().position(|x| *x == a))).collect();
    assert_eq!(shown, vec![Some(0), Some(1), Some(2), Some(0)]);

    let starts: Vec<i64> = clips.iter().map(|c| c.1.as_secs_f64() as i64).collect();
    assert_eq!(starts, vec![0, 10, 25, 40]);

    // And the whole sequence of cuts unwinds one at a time.
    for _ in 0..3 {
        f.history.undo(&mut f.project).unwrap();
    }
    assert_eq!(f.clips().len(), 1);
}
