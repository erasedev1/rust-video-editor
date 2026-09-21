//! Multicam through the action layer: grouping, syncing, cutting.
//!
//! No window. Every operation the project panel, the angle viewer and the
//! number keys perform is an `Action`, so the whole feature is drivable
//! headlessly — which is the point of routing everything through one dispatch.
//!
//! The audio sync test is not a simulation: it imports two real recordings of
//! one event, waits for the same background analysis that draws the timeline's
//! waveforms, and asserts on the offset that comes back.

use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

use ve_app::actions::{dispatch, finish_pending_sync, Action};
use ve_app::state::EditorState;
use ve_command::TrackLevel;
use ve_core::{AssetId, Project, Source, SyncMethod, TrackId, TrackKind};
use ve_engine::{multicam_at, ManualTime, PlaybackClock, PlaybackEngine};
use ve_media::{DecodeService, WaveformService};
use ve_metrics::Metrics;
use ve_time::Ticks;

fn testdata(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../testdata").join(name)
}

struct Editor {
    state: EditorState,
    engine: PlaybackEngine,
    waveforms: WaveformService,
    _scratch: tempfile::TempDir,
}

impl Editor {
    fn new() -> Self {
        let scratch = tempfile::tempdir().unwrap();
        let metrics = Metrics::new();
        let decode = Arc::new(DecodeService::new(64, metrics.clone()));
        let engine =
            PlaybackEngine::new(PlaybackClock::new(ManualTime::new()), decode, metrics.clone());
        let waveforms = WaveformService::new(8, metrics);
        let state = EditorState::new(
            Project::with_default_sequence("Multicam"),
            scratch.path().to_path_buf(),
        );
        Editor { state, engine, waveforms, _scratch: scratch }
    }

    fn act(&mut self, action: Action) {
        dispatch(&mut self.state, &mut self.engine, &self.waveforms, action);
    }

    fn status(&self) -> String {
        self.state.status.as_ref().map(|s| s.text.clone()).unwrap_or_default()
    }

    fn import(&mut self, names: &[&str]) -> Vec<AssetId> {
        let paths: Vec<PathBuf> = names.iter().map(|n| testdata(n)).collect();
        self.act(Action::ImportMedia(paths));
        assert_eq!(self.state.project.assets.len(), names.len(), "{}", self.status());
        self.state.project.assets.iter().map(|a| a.id).collect()
    }

    fn video_track(&self) -> TrackId {
        self.state
            .active_sequence()
            .unwrap()
            .tracks
            .iter()
            .find(|t| t.kind == TrackKind::Video)
            .unwrap()
            .id
    }

    /// Ticks the given media and groups it.
    fn group(&mut self, assets: &[AssetId]) -> ve_core::MulticamId {
        for asset in assets {
            self.act(Action::ToggleMulticamPick(*asset));
        }
        self.act(Action::CreateMulticamGroup);
        *self
            .state
            .project
            .multicams
            .last()
            .map(|g| &g.id)
            .unwrap_or_else(|| panic!("no group was created: {}", self.status()))
    }

    fn clips(&self) -> Vec<(Ticks, Ticks, Option<ve_core::AngleId>)> {
        let track = self.video_track();
        self.state
            .active_sequence()
            .unwrap()
            .track(track)
            .unwrap()
            .clips()
            .iter()
            .map(|c| (c.timeline_start, c.duration, c.source.multicam().map(|(_, a)| a)))
            .collect()
    }

    /// Pumps until an audio sync finishes, or gives up.
    fn settle_sync(&mut self) {
        let deadline = Instant::now() + Duration::from_secs(30);
        while self.state.pending_sync.is_some() {
            if Instant::now() > deadline {
                panic!("the sync never finished: {}", self.status());
            }
            self.waveforms.drain();
            finish_pending_sync(&mut self.state, &self.waveforms);
            std::thread::sleep(Duration::from_millis(10));
        }
    }
}

// ---- grouping -----------------------------------------------------------

#[test]
fn ticking_two_cameras_and_grouping_them_makes_a_group() {
    let mut editor = Editor::new();
    let assets = editor.import(&["counter_30fps.mp4", "counter_25fps.mp4"]);

    editor.act(Action::ToggleMulticamPick(assets[0]));
    editor.act(Action::ToggleMulticamPick(assets[1]));
    assert_eq!(editor.state.multicam_picks.len(), 2);

    editor.act(Action::CreateMulticamGroup);
    assert_eq!(editor.state.project.multicams.len(), 1, "{}", editor.status());
    let group = &editor.state.project.multicams[0];
    assert_eq!(group.len(), 2);
    assert_eq!(group.sync, SyncMethod::Start, "nothing has been measured yet");
    assert!(editor.state.multicam_picks.is_empty(), "the picks are spent");
}

#[test]
fn a_tick_is_a_toggle() {
    let mut editor = Editor::new();
    let assets = editor.import(&["counter_30fps.mp4"]);

    editor.act(Action::ToggleMulticamPick(assets[0]));
    assert_eq!(editor.state.multicam_picks, vec![assets[0]]);
    editor.act(Action::ToggleMulticamPick(assets[0]));
    assert!(editor.state.multicam_picks.is_empty());
}

#[test]
fn grouping_one_camera_says_what_is_missing() {
    let mut editor = Editor::new();
    let assets = editor.import(&["counter_30fps.mp4"]);
    editor.act(Action::ToggleMulticamPick(assets[0]));
    editor.act(Action::CreateMulticamGroup);

    assert!(editor.state.project.multicams.is_empty());
    assert!(editor.status().contains("two"), "{}", editor.status());
    assert_eq!(editor.state.multicam_picks.len(), 1, "the tick survives the refusal");
}

// ---- placing and cutting ------------------------------------------------

#[test]
fn a_group_goes_onto_the_timeline_as_one_clip() {
    let mut editor = Editor::new();
    let assets = editor.import(&["counter_30fps.mp4", "counter_25fps.mp4"]);
    let group = editor.group(&assets);
    let track = editor.video_track();

    editor.act(Action::AddMulticamToTimeline { group, track, at: Ticks::ZERO });

    let clips = editor.clips();
    assert_eq!(clips.len(), 1, "{}", editor.status());
    assert_eq!(clips[0].0, Ticks::ZERO);
    // The group covers the longer of the two cameras.
    let expected = editor.state.project.multicam_duration(group);
    assert_eq!(clips[0].1, expected);
    assert!(clips[0].2.is_some(), "it is showing an angle");
}

#[test]
fn a_number_key_cuts_the_clip_under_the_playhead() {
    let mut editor = Editor::new();
    let assets = editor.import(&["counter_30fps.mp4", "counter_25fps.mp4"]);
    let group = editor.group(&assets);
    let track = editor.video_track();
    editor.act(Action::AddMulticamToTimeline { group, track, at: Ticks::ZERO });

    editor.act(Action::ScrubTo(Ticks::from_seconds(1)));
    editor.act(Action::CutToAngle(2));

    let clips = editor.clips();
    assert_eq!(clips.len(), 2, "{}", editor.status());
    assert_eq!(clips[1].0, Ticks::from_seconds(1));
    assert_ne!(clips[0].2, clips[1].2, "the two halves show different cameras");

    let angles = &editor.state.project.multicam(group).unwrap().angles;
    assert_eq!(clips[1].2, Some(angles[1].id));
    assert_eq!(clips[0].2, Some(angles[0].id), "what came before is untouched");
}

#[test]
fn switching_changes_the_whole_clip_without_cutting() {
    let mut editor = Editor::new();
    let assets = editor.import(&["counter_30fps.mp4", "counter_25fps.mp4"]);
    let group = editor.group(&assets);
    let track = editor.video_track();
    editor.act(Action::AddMulticamToTimeline { group, track, at: Ticks::ZERO });

    editor.act(Action::ScrubTo(Ticks::from_seconds(1)));
    editor.act(Action::SwitchToAngle(2));

    let clips = editor.clips();
    assert_eq!(clips.len(), 1, "switching is not a cut");
    let angles = &editor.state.project.multicam(group).unwrap().angles;
    assert_eq!(clips[0].2, Some(angles[1].id));
}

#[test]
fn a_number_with_no_camera_behind_it_says_so() {
    let mut editor = Editor::new();
    let assets = editor.import(&["counter_30fps.mp4", "counter_25fps.mp4"]);
    let group = editor.group(&assets);
    let track = editor.video_track();
    editor.act(Action::AddMulticamToTimeline { group, track, at: Ticks::ZERO });

    editor.act(Action::CutToAngle(7));
    assert!(editor.status().contains("2 camera"), "{}", editor.status());
    assert_eq!(editor.clips().len(), 1, "nothing was cut");
}

#[test]
fn cutting_with_no_multicam_under_the_playhead_says_so() {
    let mut editor = Editor::new();
    let assets = editor.import(&["counter_30fps.mp4"]);
    let track = editor.video_track();
    editor.act(Action::AddAssetToTimeline { asset: assets[0], track, at: Ticks::ZERO });

    editor.act(Action::CutToAngle(1));
    assert!(editor.status().contains("no multicam"), "{}", editor.status());
    assert_eq!(editor.clips().len(), 1);
}

#[test]
fn a_disabled_angle_refuses_to_be_cut_to() {
    let mut editor = Editor::new();
    let assets = editor.import(&["counter_30fps.mp4", "counter_25fps.mp4"]);
    let group = editor.group(&assets);
    let track = editor.video_track();
    editor.act(Action::AddMulticamToTimeline { group, track, at: Ticks::ZERO });

    let angle = editor.state.project.multicam(group).unwrap().angles[1].id;
    editor.act(Action::SetAngleEnabled { group, angle, enabled: false });

    editor.act(Action::ScrubTo(Ticks::from_seconds(1)));
    editor.act(Action::CutToAngle(2));
    assert!(editor.status().contains("switched off"), "{}", editor.status());
    assert_eq!(editor.clips().len(), 1);
}

#[test]
fn cuts_undo_one_at_a_time() {
    let mut editor = Editor::new();
    let assets = editor.import(&["counter_30fps.mp4", "counter_25fps.mp4"]);
    let group = editor.group(&assets);
    let track = editor.video_track();
    editor.act(Action::AddMulticamToTimeline { group, track, at: Ticks::ZERO });

    for (at, number) in [(500i64, 2), (1000, 1), (1500, 2)] {
        editor.act(Action::ScrubTo(Ticks::from_millis(at)));
        editor.act(Action::CutToAngle(number));
    }
    assert_eq!(editor.clips().len(), 4, "{}", editor.status());

    editor.act(Action::Undo);
    assert_eq!(editor.clips().len(), 3);
    editor.act(Action::Undo);
    editor.act(Action::Undo);
    assert_eq!(editor.clips().len(), 1);
}

// ---- the viewer ---------------------------------------------------------

#[test]
fn the_viewer_resolves_every_camera_at_the_playhead() {
    let mut editor = Editor::new();
    let assets = editor.import(&["counter_30fps.mp4", "counter_25fps.mp4"]);
    let group = editor.group(&assets);
    let track = editor.video_track();
    editor.act(Action::AddMulticamToTimeline { group, track, at: Ticks::ZERO });
    editor.act(Action::ScrubTo(Ticks::from_seconds(1)));

    let sequence = editor.state.active_sequence().unwrap();
    let view = multicam_at(
        &editor.state.project,
        sequence,
        Ticks::from_seconds(1),
        editor.state.selection.track,
    )
    .expect("a multicam clip is under the playhead");

    assert_eq!(view.angles.len(), 2);
    assert_eq!(view.angles.iter().filter(|a| a.active).count(), 1);
    assert_eq!(view.requests().len(), 2, "one decode per camera");
}

#[test]
fn the_viewer_is_off_until_it_is_asked_for() {
    let mut editor = Editor::new();
    assert!(!editor.state.show_angle_viewer, "it costs a decode per camera per frame");
    editor.act(Action::ToggleAngleViewer);
    assert!(editor.state.show_angle_viewer);
    assert!(editor.status().contains("1-9"), "{}", editor.status());
}

// ---- syncing ------------------------------------------------------------

/// The whole audio sync path, on two real recordings of one event: import,
/// group, sync, and assert on the offset the correlation produced.
#[test]
fn syncing_on_sound_lines_up_two_real_recordings() {
    let mut editor = Editor::new();
    let assets = editor.import(&["sync_cam_a.wav", "sync_cam_b.wav"]);
    let group = editor.group(&assets);

    editor.act(Action::SyncMulticam { group, method: SyncMethod::Audio });
    assert!(editor.state.pending_sync.is_some(), "it waits on analysis");
    editor.settle_sync();

    let g = editor.state.project.multicam(group).unwrap();
    assert_eq!(g.sync, SyncMethod::Audio, "{}", editor.status());

    // Camera B joined 1.5 seconds late, so with the earliest camera at group
    // zero its own offset is -1.5 seconds.
    let offsets: Vec<f64> = g.angles.iter().map(|a| a.offset.as_secs_f64()).collect();
    assert!(offsets[0].abs() < 0.01, "the earliest camera sits at zero: {offsets:?}");
    assert!(
        (offsets[1] + 1.5).abs() < 0.05,
        "camera B should be a second and a half late: {offsets:?}"
    );
    assert!(editor.status().contains("synced on sound"), "{}", editor.status());
}

#[test]
fn a_sync_is_one_undo_step() {
    let mut editor = Editor::new();
    let assets = editor.import(&["sync_cam_a.wav", "sync_cam_b.wav"]);
    let group = editor.group(&assets);

    editor.act(Action::SyncMulticam { group, method: SyncMethod::Audio });
    editor.settle_sync();
    assert_ne!(editor.state.project.multicam(group).unwrap().angles[1].offset, Ticks::ZERO);

    editor.act(Action::Undo);
    let g = editor.state.project.multicam(group).unwrap();
    assert_eq!(g.angles[1].offset, Ticks::ZERO);
    assert_eq!(g.sync, SyncMethod::Start, "the method comes back too");
}

#[test]
fn syncing_on_timecode_that_is_not_there_says_so_rather_than_guessing() {
    let mut editor = Editor::new();
    let assets = editor.import(&["counter_30fps.mp4", "counter_25fps.mp4"]);
    let group = editor.group(&assets);

    editor.act(Action::SyncMulticam { group, method: SyncMethod::Timecode });
    assert!(editor.status().contains("timecode"), "{}", editor.status());
    // Nothing moved, and the group still says it was never measured.
    let g = editor.state.project.multicam(group).unwrap();
    assert_eq!(g.sync, SyncMethod::Start);
    assert!(g.angles.iter().all(|a| a.offset == Ticks::ZERO));
}

#[test]
fn nudging_an_offset_marks_the_group_manual() {
    let mut editor = Editor::new();
    let assets = editor.import(&["sync_cam_a.wav", "sync_cam_b.wav"]);
    let group = editor.group(&assets);
    editor.act(Action::SyncMulticam { group, method: SyncMethod::Audio });
    editor.settle_sync();

    let angle = editor.state.project.multicam(group).unwrap().angles[1].id;
    editor.act(Action::SetAngleOffset {
        group,
        angle,
        offset: Ticks::from_millis(-1400),
        coalesce: false,
    });

    let g = editor.state.project.multicam(group).unwrap();
    assert_eq!(g.angles[1].offset, Ticks::from_millis(-1400));
    assert_eq!(g.sync, SyncMethod::Manual);
}

/// A sync and the mixer are independent: the track level is not disturbed by
/// anything multicam does, which is the sort of thing that only shows up when
/// two features are driven through the same dispatch.
#[test]
fn syncing_leaves_the_rest_of_the_edit_alone() {
    let mut editor = Editor::new();
    let assets = editor.import(&["sync_cam_a.wav", "sync_cam_b.wav"]);
    let audio_track = editor
        .state
        .active_sequence()
        .unwrap()
        .tracks
        .iter()
        .find(|t| t.kind == TrackKind::Audio)
        .unwrap()
        .id;
    editor.act(Action::SetTrackLevel {
        track: audio_track,
        which: TrackLevel::Volume,
        value: 0.5,
    });

    let group = editor.group(&assets);
    editor.act(Action::SyncMulticam { group, method: SyncMethod::Audio });
    editor.settle_sync();

    let level = editor.state.active_sequence().unwrap().track(audio_track).unwrap().volume;
    assert_eq!(level, 0.5);
}

// ---- deleting -----------------------------------------------------------

#[test]
fn a_group_in_use_cannot_be_deleted_from_the_panel() {
    let mut editor = Editor::new();
    let assets = editor.import(&["counter_30fps.mp4", "counter_25fps.mp4"]);
    let group = editor.group(&assets);
    let track = editor.video_track();
    editor.act(Action::AddMulticamToTimeline { group, track, at: Ticks::ZERO });

    editor.act(Action::RemoveMulticamGroup(group));
    assert!(editor.state.project.multicam(group).is_some());
    assert!(editor.status().contains("still used"), "{}", editor.status());
}

#[test]
fn an_unused_group_is_deleted_and_comes_back_on_undo() {
    let mut editor = Editor::new();
    let assets = editor.import(&["counter_30fps.mp4", "counter_25fps.mp4"]);
    let group = editor.group(&assets);

    editor.act(Action::RemoveMulticamGroup(group));
    assert!(editor.state.project.multicam(group).is_none());
    editor.act(Action::Undo);
    assert!(editor.state.project.multicam(group).is_some());
}

#[test]
fn a_multicam_clip_survives_a_save_and_reopen() {
    let mut editor = Editor::new();
    let assets = editor.import(&["counter_30fps.mp4", "counter_25fps.mp4"]);
    let group = editor.group(&assets);
    let track = editor.video_track();
    editor.act(Action::AddMulticamToTimeline { group, track, at: Ticks::ZERO });
    editor.act(Action::ScrubTo(Ticks::from_seconds(1)));
    editor.act(Action::CutToAngle(2));

    let path = editor._scratch.path().join("multicam.verge");
    editor.act(Action::SaveProjectAs(path.clone()));
    let before = editor.clips();

    editor.act(Action::NewProject);
    assert!(editor.state.project.multicams.is_empty());
    editor.act(Action::OpenProject(path));

    assert_eq!(editor.state.project.multicams.len(), 1, "{}", editor.status());
    assert_eq!(editor.clips().len(), 2);
    let after = editor.clips();
    assert_eq!(
        before.iter().map(|c| (c.0, c.1)).collect::<Vec<_>>(),
        after.iter().map(|c| (c.0, c.1)).collect::<Vec<_>>()
    );
    // And the two halves still show different cameras.
    assert_ne!(after[0].2, after[1].2);
    assert!(editor.state.warnings.is_empty(), "{:?}", editor.state.warnings);
}

#[test]
fn a_multicam_clip_knows_which_file_it_is_reading() {
    let mut editor = Editor::new();
    let assets = editor.import(&["counter_30fps.mp4", "counter_25fps.mp4"]);
    let group = editor.group(&assets);
    let track = editor.video_track();
    editor.act(Action::AddMulticamToTimeline { group, track, at: Ticks::ZERO });

    let angles = &editor.state.project.multicam(group).unwrap().angles;
    let source = Source::Multicam { group, angle: angles[1].id };
    let (asset, at) = editor
        .state
        .project
        .resolve_source(source, Ticks::from_seconds(1))
        .expect("the second camera resolves");
    assert_eq!(asset, assets[1]);
    assert_eq!(at, Ticks::from_seconds(1), "with no offset, group time is source time");
}
