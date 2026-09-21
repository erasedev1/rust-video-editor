//! Multicam through the project: resolution, bounds, deletion and load repair.
//!
//! The group's own arithmetic is unit-tested where it lives. What is asserted
//! here is how a group behaves as part of a project — which file a clip
//! actually reads, what a trim is bounded by, and what a hand-edited file loads
//! as.

use ve_core::{
    AngleId, AssetId, AudioStreamInfo, Clip, CoreError, MediaInfo, MulticamAngle,
    MulticamGroup, MulticamId, Project, Size, Source, SyncMethod, TrackKind, VideoStreamInfo,
};
use ve_time::{Rate, SampleRate, Ticks};

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

/// Three cameras: A rolling from the start, B ten seconds of pre-roll, C
/// joining twenty seconds late.
struct Shoot {
    project: Project,
    group: MulticamId,
    angles: Vec<AngleId>,
    assets: Vec<AssetId>,
}

fn shoot() -> Shoot {
    let mut project = Project::with_default_sequence("Multicam");
    let assets: Vec<AssetId> = [("a.mp4", 120i64), ("b.mp4", 130), ("c.mp4", 100)]
        .iter()
        .map(|(name, secs)| project.add_asset(format!("/media/{name}"), info(*secs)))
        .collect();

    let group = project.add_multicam_group("Interview");
    let mut angles = Vec::new();
    for (index, offset) in [0i64, 10, -20].iter().enumerate() {
        let id = project.new_angle_id();
        let angle = MulticamAngle::new(id, format!("Cam {}", index + 1), assets[index])
            .with_offset(Ticks::from_seconds(*offset));
        project.multicam_mut(group).unwrap().push_angle(angle).unwrap();
        angles.push(id);
    }
    project.multicam_mut(group).unwrap().sync = SyncMethod::Audio;

    Shoot { project, group, angles, assets }
}

impl Shoot {
    fn source(&self, index: usize) -> Source {
        Source::Multicam { group: self.group, angle: self.angles[index] }
    }

    fn place(&mut self, source: Source, start_s: i64, dur_s: i64) -> ve_core::ClipId {
        let id = self.project.new_clip_id();
        let sequence = self.project.active_sequence.unwrap();
        let track = self
            .project
            .sequence(sequence)
            .unwrap()
            .tracks
            .iter()
            .find(|t| t.kind == TrackKind::Video)
            .unwrap()
            .id;
        let clip = Clip::new(
            id,
            source,
            "cut",
            Ticks::ZERO,
            Ticks::from_seconds(start_s),
            Ticks::from_seconds(dur_s),
        );
        self.project
            .sequence_mut(sequence)
            .unwrap()
            .track_mut(track)
            .unwrap()
            .insert_clip(clip)
            .unwrap();
        id
    }
}

#[test]
fn a_multicam_source_resolves_to_the_camera_it_names() {
    let s = shoot();
    let at = Ticks::from_seconds(30);

    let (asset, time) = s.project.resolve_source(s.source(0), at).unwrap();
    assert_eq!(asset, s.assets[0]);
    assert_eq!(time, Ticks::from_seconds(30));

    let (asset, time) = s.project.resolve_source(s.source(1), at).unwrap();
    assert_eq!(asset, s.assets[1]);
    assert_eq!(time, Ticks::from_seconds(40), "ten seconds of pre-roll");

    let (asset, time) = s.project.resolve_source(s.source(2), at).unwrap();
    assert_eq!(asset, s.assets[2]);
    assert_eq!(time, Ticks::from_seconds(10), "joined twenty seconds late");
}

#[test]
fn an_angle_that_was_not_rolling_resolves_to_nothing_rather_than_frame_zero() {
    let s = shoot();
    assert_eq!(s.project.resolve_source(s.source(2), Ticks::from_seconds(5)), None);
    assert_eq!(
        s.project.resolve_source(s.source(2), Ticks::from_seconds(20)),
        Some((s.assets[2], Ticks::ZERO))
    );
}

#[test]
fn a_plain_asset_source_resolves_to_itself_unchanged() {
    let s = shoot();
    let at = Ticks::from_seconds(7);
    assert_eq!(
        s.project.resolve_source(Source::Asset(s.assets[0]), at),
        Some((s.assets[0], at))
    );
    // A composition has no single file behind it, and saying so is the point.
    let comp = {
        let mut p = s.project.clone();
        p.add_composition("Comp", ve_core::CompositionSettings::default())
    };
    assert_eq!(s.project.resolve_source(Source::Composition(comp), at), None);
}

/// The clip is trimmed against the group, not against whichever camera happens
/// to be on screen — otherwise the same clip would have different limits
/// depending on what the user was looking at when they grabbed its edge.
#[test]
fn a_multicam_clip_is_bounded_by_the_group_not_the_visible_angle() {
    let s = shoot();
    // Cam 1: 0..120. Cam 2: -10..120. Cam 3: 20..120.
    let expected = Ticks::from_seconds(120);
    assert_eq!(s.project.multicam_duration(s.group), expected);
    assert_eq!(s.project.source_duration(s.source(0)), expected);
    assert_eq!(s.project.source_duration(s.source(1)), expected);
    assert_eq!(
        s.project.source_duration(s.source(2)),
        expected,
        "the shortest camera must not shrink the group"
    );
}

#[test]
fn the_group_covers_the_union_of_its_cameras() {
    let mut s = shoot();
    // Push camera 3 out past the others: 100 seconds of media starting at
    // group time 60 reaches 160, further than anything else.
    s.project.multicam_mut(s.group).unwrap().angles[2].offset = Ticks::from_seconds(-60);
    assert_eq!(s.project.multicam_duration(s.group), Ticks::from_seconds(160));
}

#[test]
fn an_empty_or_missing_group_has_no_duration_rather_than_panicking() {
    let mut project = Project::with_default_sequence("Empty");
    let group = project.add_multicam_group("Nothing");
    assert_eq!(project.multicam_duration(group), Ticks::ZERO);
    assert_eq!(project.multicam_duration(MulticamId::from_raw(9999)), Ticks::ZERO);
}

#[test]
fn cutting_to_another_angle_changes_only_which_camera_is_read() {
    let mut s = shoot();
    let clip_id = s.place(s.source(0), 0, 30);
    let sequence = s.project.active_sequence.unwrap();

    let before = s.project.sequence(sequence).unwrap().find_clip(clip_id).unwrap().1.clone();
    let switched = before.source.with_angle(s.angles[1]).unwrap();
    s.project.sequence_mut(sequence).unwrap().find_clip_mut(clip_id).unwrap().1.source =
        switched;
    let after = s.project.sequence(sequence).unwrap().find_clip(clip_id).unwrap().1.clone();

    assert_eq!(after.timeline_start, before.timeline_start);
    assert_eq!(after.duration, before.duration);
    assert_eq!(after.source_in, before.source_in, "the window into the group is untouched");
    assert_eq!(after.source.multicam().map(|(g, _)| g), Some(s.group));
    assert_ne!(after.source, before.source);
}

#[test]
fn an_angle_change_cannot_turn_a_plain_clip_into_a_multicam_one() {
    let s = shoot();
    assert_eq!(Source::Asset(s.assets[0]).with_angle(s.angles[1]), None);
}

#[test]
fn a_group_in_use_cannot_be_deleted() {
    let mut s = shoot();
    s.place(s.source(0), 0, 10);
    s.place(s.source(2), 20, 10);

    assert_eq!(s.project.uses_of_multicam(s.group), 2, "counted across angles");
    let group = s.group;
    match s.project.remove_multicam_group(group) {
        Err(CoreError::MulticamInUse { places, .. }) => assert_eq!(places, 2),
        other => panic!("expected a refusal, got {other:?}"),
    }
}

#[test]
fn a_group_nothing_cuts_with_can_be_deleted() {
    let mut s = shoot();
    let group = s.group;
    let removed = s.project.remove_multicam_group(group).unwrap();
    assert_eq!(removed.angles.len(), 3);
    assert!(s.project.multicam(group).is_none());
}

#[test]
fn a_camera_cannot_be_deleted_out_from_under_its_group() {
    let mut s = shoot();
    let asset = s.assets[1];
    assert!(s.project.clips_using_asset(asset).next().is_none(), "nothing is on a timeline");
    assert!(matches!(s.project.remove_asset(asset), Err(CoreError::AssetIsAnAngle(_))));
}

#[test]
fn ids_from_a_loaded_file_cannot_be_handed_out_again() {
    let mut project = Project::with_default_sequence("Loaded");
    let asset = project.add_asset("/media/a.mp4", info(10));

    // A file mentioning far higher IDs than the allocator has reached.
    let mut group = MulticamGroup::new(MulticamId::from_raw(5000), "Hand written");
    group.push_angle(MulticamAngle::new(AngleId::from_raw(6000), "Cam", asset)).unwrap();
    project.multicams.push(group);

    project.normalise();
    assert!(
        project.ids.peek() > 6000,
        "a future angle would otherwise collide with one already in the file"
    );
}

#[test]
fn an_angle_naming_a_missing_asset_is_dropped_with_a_warning() {
    let mut project = Project::with_default_sequence("Broken");
    let real = project.add_asset("/media/a.mp4", info(10));

    let group_id = project.add_multicam_group("Half there");
    let good = project.new_angle_id();
    let bad = project.new_angle_id();
    let group = project.multicam_mut(group_id).unwrap();
    group.push_angle(MulticamAngle::new(good, "Cam 1", real)).unwrap();
    group.push_angle(MulticamAngle::new(bad, "Ghost", AssetId::from_raw(4242))).unwrap();

    let warnings = project.normalise();
    assert_eq!(project.multicam(group_id).unwrap().len(), 1);
    assert!(
        warnings.iter().any(|w| w.contains("Ghost") && w.contains("missing")),
        "{warnings:?}"
    );
}

#[test]
fn a_clip_on_an_angle_that_no_longer_exists_is_reported_not_silently_dropped() {
    let mut s = shoot();
    s.place(s.source(0), 0, 10);
    // Remove the angle behind it, as a hand-edited file might.
    let angle = s.angles[0];
    s.project.multicam_mut(s.group).unwrap().remove_angle(angle).unwrap();

    let warnings = s.project.normalise();
    assert!(warnings.iter().any(|w| w.contains("references missing multicam")), "{warnings:?}");
    // The clip is still there; nothing was thrown away behind the user's back.
    assert_eq!(s.project.clip_count(), 1);
}

#[test]
fn the_sync_method_survives_being_recorded() {
    let s = shoot();
    assert_eq!(s.project.multicam(s.group).unwrap().sync, SyncMethod::Audio);
    assert_eq!(SyncMethod::Audio.label(), "Audio");
    assert_eq!(SyncMethod::ALL.len(), 4);
}
