//! The first milestone's twelve steps, end to end, driven through the same
//! action layer the interface uses.
//!
//! No window is created: every operation the menus and keyboard perform is an
//! `Action`, so the whole editor can be driven headlessly. That is the point of
//! routing everything through one dispatch — the workflow is testable, not just
//! demonstrable.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use ve_app::actions::{dispatch, Action};
use ve_app::state::EditorState;
use ve_command::{ClipProperty, PropertyValue, TrimEdge};
use ve_core::{Project, TrackKind};
use ve_engine::{ManualTime, PlaybackClock, PlaybackEngine};
use ve_media::DecodeService;
use ve_metrics::Metrics;
use ve_time::{Rate, Ticks};

fn testdata(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../testdata").join(name)
}

struct Editor {
    state: EditorState,
    engine: PlaybackEngine,
    time: Arc<ManualTime>,
    _scratch: tempfile::TempDir,
}

impl Editor {
    fn new() -> Self {
        let scratch = tempfile::tempdir().unwrap();
        let metrics = Metrics::new();
        let decode = Arc::new(DecodeService::new(64, metrics.clone()));
        let time = ManualTime::new();
        let engine = PlaybackEngine::new(PlaybackClock::new(time.clone()), decode, metrics);
        let state = EditorState::new(
            Project::with_default_sequence("Untitled"),
            scratch.path().to_path_buf(),
        );
        Editor { state, engine, time, _scratch: scratch }
    }

    fn act(&mut self, action: Action) {
        dispatch(&mut self.state, &mut self.engine, action);
    }

    fn status(&self) -> String {
        self.state.status.as_ref().map(|s| s.text.clone()).unwrap_or_default()
    }

    fn sequence(&self) -> &ve_core::Sequence {
        self.state.active_sequence().unwrap()
    }

    fn first_video_track(&self) -> ve_core::TrackId {
        self.sequence().tracks.iter().find(|t| t.kind == TrackKind::Video).unwrap().id
    }

    fn first_audio_track(&self) -> ve_core::TrackId {
        self.sequence().tracks.iter().find(|t| t.kind == TrackKind::Audio).unwrap().id
    }

    /// Pumps the engine until the picture is complete, or gives up.
    fn settle(&mut self) -> ve_engine::EngineUpdate {
        let viewing = self.state.viewing().expect("a project always has something to view");
        let mut update = self.engine.update(&self.state.project, viewing);
        for _ in 0..200 {
            if update.is_complete() {
                break;
            }
            std::thread::sleep(Duration::from_millis(5));
            update = self.engine.update(&self.state.project, viewing);
        }
        update
    }
}

/// The whole first milestone, in order, in one test.
#[test]
fn the_first_milestone_works_end_to_end() {
    let mut editor = Editor::new();

    // 2. Create a project.
    editor.act(Action::NewProject);
    assert_eq!(editor.sequence().tracks.len(), 4);
    assert_eq!(editor.state.project.clip_count(), 0);

    // 3 and 4. Import a video and an audio file.
    editor.act(Action::ImportMedia(vec![
        testdata("counter_30fps.mp4"),
        testdata("tone_48k.wav"),
    ]));
    assert_eq!(editor.state.project.assets.len(), 2, "{}", editor.status());

    // 5. Display imported assets: the metadata the browser shows is real.
    let video = editor
        .state
        .project
        .assets
        .iter()
        .find(|a| a.info.has_video())
        .expect("the video asset");
    assert_eq!(video.info.video.as_ref().unwrap().rate, Rate::FPS_30);
    assert_eq!(video.duration(), Ticks::from_seconds(3));
    assert!(!video.offline);

    let audio = editor
        .state
        .project
        .assets
        .iter()
        .find(|a| a.info.has_audio() && !a.info.has_video())
        .expect("the audio asset");
    assert_eq!(audio.info.audio.as_ref().unwrap().channels, 2);

    // 6. A sequence already exists, created with the project.
    let sequence_id = editor.state.active_sequence_id().unwrap();

    // 7. Place a video clip on the timeline.
    let video_id = video.id;
    let audio_id = audio.id;
    let v1 = editor.first_video_track();
    let a1 = editor.first_audio_track();
    editor.act(Action::AddAssetToTimeline { asset: video_id, track: v1, at: Ticks::ZERO });
    assert_eq!(editor.state.project.clip_count(), 1, "{}", editor.status());
    assert_eq!(editor.sequence().duration(), Ticks::from_seconds(3));

    editor.act(Action::AddAssetToTimeline { asset: audio_id, track: a1, at: Ticks::ZERO });
    assert_eq!(editor.state.project.clip_count(), 2);

    // 8. Play the sequence.
    editor.act(Action::TogglePlayback);
    assert!(editor.engine.is_playing());
    editor.time.advance(Duration::from_millis(500));
    let update = editor.settle();
    assert_eq!(update.position, Rate::FPS_30.snap_round(Ticks::from_millis(500)));
    assert_eq!(update.root().layers.len(), 1, "a decoded frame should be ready to draw");
    // The tone fixture is one second long, so half a second in is inside it.
    // At exactly one second it would not be: clip ranges are half-open, and
    // the instant a clip ends belongs to whatever comes next.
    assert_eq!(update.plan.audio.len(), 1, "the audio clip should be audible");

    editor.time.advance(Duration::from_millis(600));
    let update = editor.settle();
    assert!(update.position > Ticks::from_seconds(1));
    assert_eq!(update.root().layers.len(), 1, "the 3s video clip still covers this instant");
    assert!(update.plan.audio.is_empty(), "the 1s tone has ended");

    // 10. Stop and resume.
    editor.act(Action::TogglePlayback);
    assert!(!editor.engine.is_playing());
    let stopped_at = editor.engine.clock().position();
    editor.time.advance(Duration::from_secs(5));
    assert_eq!(editor.engine.clock().position(), stopped_at, "stopped means stopped");
    editor.act(Action::TogglePlayback);
    assert!(editor.engine.is_playing());
    editor.act(Action::TogglePlayback);

    // 9. Scrub through the timeline.
    editor.act(Action::ScrubTo(Ticks::from_millis(2_100)));
    let update = editor.settle();
    assert_eq!(update.position, Rate::FPS_30.snap_round(Ticks::from_millis(2_100)));
    assert_eq!(update.root().layers.len(), 1, "scrubbing should land on a decoded frame");

    // 11. Save the project.
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("milestone.verge");
    editor.act(Action::SaveProjectAs(path.clone()));
    assert!(path.exists(), "{}", editor.status());
    assert!(!editor.state.history.is_dirty());

    // 12. Reopen it and find everything where it was left.
    let mut reopened = Editor::new();
    reopened.act(Action::OpenProject(path.clone()));
    assert!(reopened.state.warnings.is_empty(), "{:?}", reopened.state.warnings);
    assert_eq!(reopened.state.project.assets.len(), 2);
    assert_eq!(reopened.state.project.clip_count(), 2);
    assert_eq!(reopened.sequence().duration(), Ticks::from_seconds(3));
    assert_eq!(reopened.state.active_sequence_id(), Some(sequence_id));
    assert!(reopened.state.project.assets.iter().all(|a| !a.offline), "media must relink");

    // And it plays from there, which is the real proof the reopen was complete.
    let update = reopened.settle();
    assert_eq!(update.root().layers.len(), 1);
}

#[test]
fn importing_the_same_file_twice_reuses_one_asset() {
    let mut editor = Editor::new();
    editor.act(Action::ImportMedia(vec![testdata("counter_30fps.mp4")]));
    editor.act(Action::ImportMedia(vec![testdata("counter_30fps.mp4")]));
    assert_eq!(editor.state.project.assets.len(), 1, "media must not be duplicated");
}

#[test]
fn importing_something_that_is_not_media_reports_an_error() {
    let dir = tempfile::tempdir().unwrap();
    let junk = dir.path().join("notes.mp4");
    std::fs::write(&junk, b"not a video").unwrap();

    let mut editor = Editor::new();
    editor.act(Action::ImportMedia(vec![junk]));
    assert!(editor.state.project.assets.is_empty());
    assert!(editor.status().contains("could not import"), "{}", editor.status());
}

#[test]
fn a_clip_cannot_be_put_on_a_track_of_the_wrong_kind() {
    let mut editor = Editor::new();
    editor.act(Action::ImportMedia(vec![testdata("tone_48k.wav")]));
    let asset = editor.state.project.assets[0].id;
    let v1 = editor.first_video_track();

    editor.act(Action::AddAssetToTimeline { asset, track: v1, at: Ticks::ZERO });
    assert_eq!(editor.state.project.clip_count(), 0);
    assert!(editor.status().contains("no video"), "{}", editor.status());
}

#[test]
fn dropping_a_clip_onto_occupied_space_slides_it_to_the_next_gap() {
    let mut editor = Editor::new();
    editor.act(Action::ImportMedia(vec![testdata("counter_30fps.mp4")]));
    let asset = editor.state.project.assets[0].id;
    let v1 = editor.first_video_track();

    editor.act(Action::AddAssetToTimeline { asset, track: v1, at: Ticks::ZERO });
    // Asking for the same spot again must not be refused; it should land after.
    editor.act(Action::AddAssetToTimeline { asset, track: v1, at: Ticks::ZERO });

    assert_eq!(editor.state.project.clip_count(), 2, "{}", editor.status());
    let clips = editor.sequence().track(v1).unwrap().clips();
    assert_eq!(clips[0].timeline_start, Ticks::ZERO);
    assert_eq!(clips[1].timeline_start, Ticks::from_seconds(3));
}

#[test]
fn splitting_at_the_playhead_cuts_every_unlocked_track() {
    let mut editor = Editor::new();
    editor.act(Action::ImportMedia(vec![
        testdata("counter_30fps.mp4"),
        testdata("tone_48k.wav"),
    ]));
    let video = editor.state.project.assets.iter().find(|a| a.info.has_video()).unwrap().id;
    let v1 = editor.first_video_track();
    editor.act(Action::AddAssetToTimeline { asset: video, track: v1, at: Ticks::ZERO });

    editor.act(Action::ScrubTo(Ticks::from_seconds(1)));
    editor.act(Action::SplitAtPlayhead);
    assert_eq!(editor.state.project.clip_count(), 2, "{}", editor.status());

    let clips = editor.sequence().track(v1).unwrap().clips();
    assert_eq!(clips[0].duration, Ticks::from_seconds(1));
    assert_eq!(clips[1].timeline_start, Ticks::from_seconds(1));
    assert_eq!(clips[0].duration + clips[1].duration, Ticks::from_seconds(3));

    // Undo puts the two halves back together as one clip.
    editor.act(Action::Undo);
    assert_eq!(editor.state.project.clip_count(), 1);
    assert_eq!(
        editor.sequence().track(v1).unwrap().clips()[0].duration,
        Ticks::from_seconds(3)
    );
}

#[test]
fn splitting_with_nothing_under_the_playhead_says_so() {
    let mut editor = Editor::new();
    editor.act(Action::ScrubTo(Ticks::from_seconds(5)));
    editor.act(Action::SplitAtPlayhead);
    assert!(editor.status().contains("no clip"), "{}", editor.status());
}

#[test]
fn a_move_gesture_is_one_undo_step() {
    let mut editor = Editor::new();
    editor.act(Action::ImportMedia(vec![testdata("counter_30fps.mp4")]));
    let asset = editor.state.project.assets[0].id;
    let v1 = editor.first_video_track();
    editor.act(Action::AddAssetToTimeline { asset, track: v1, at: Ticks::ZERO });
    let clip = editor.sequence().track(v1).unwrap().clips()[0].id;
    let depth = editor.state.history.undo_depth();

    // A drag: many small moves, then the pointer comes up.
    for ms in (100..=2000).step_by(100) {
        editor.act(Action::MoveClipTo {
            clip,
            track: v1,
            to: Ticks::from_millis(ms),
            coalesce: true,
        });
    }
    editor.act(Action::EndGesture);
    assert_eq!(editor.state.history.undo_depth(), depth + 1, "a drag is one undo step");

    editor.act(Action::Undo);
    assert_eq!(
        editor.sequence().track(v1).unwrap().clips()[0].timeline_start,
        Ticks::ZERO,
        "undo must return to where the drag began"
    );
}

#[test]
fn a_trim_gesture_is_one_undo_step_and_is_reversible() {
    let mut editor = Editor::new();
    editor.act(Action::ImportMedia(vec![testdata("counter_30fps.mp4")]));
    let asset = editor.state.project.assets[0].id;
    let v1 = editor.first_video_track();
    editor.act(Action::AddAssetToTimeline { asset, track: v1, at: Ticks::ZERO });
    let clip = editor.sequence().track(v1).unwrap().clips()[0].id;
    let depth = editor.state.history.undo_depth();

    for ms in (1000..=2500).step_by(100) {
        editor.act(Action::TrimClipTo {
            clip,
            track: v1,
            edge: TrimEdge::End,
            to: Ticks::from_millis(ms),
            coalesce: true,
        });
    }
    editor.act(Action::EndGesture);
    assert_eq!(editor.state.history.undo_depth(), depth + 1);
    assert_eq!(
        editor.sequence().track(v1).unwrap().clips()[0].duration,
        Rate::FPS_30.snap_round(Ticks::from_millis(2500))
    );

    editor.act(Action::Undo);
    assert_eq!(
        editor.sequence().track(v1).unwrap().clips()[0].duration,
        Ticks::from_seconds(3),
        "trimming is non-destructive: undo restores the full clip"
    );
}

#[test]
fn deleting_a_selected_clip_is_undoable() {
    let mut editor = Editor::new();
    editor.act(Action::ImportMedia(vec![testdata("counter_30fps.mp4")]));
    let asset = editor.state.project.assets[0].id;
    let v1 = editor.first_video_track();
    editor.act(Action::AddAssetToTimeline { asset, track: v1, at: Ticks::ZERO });
    let clip = editor.sequence().track(v1).unwrap().clips()[0].id;

    editor.act(Action::SelectClip { clip, track: v1, additive: false });
    editor.act(Action::DeleteSelected);
    assert_eq!(editor.state.project.clip_count(), 0);
    assert!(editor.state.selection.clips.is_empty());

    editor.act(Action::Undo);
    assert_eq!(editor.state.project.clip_count(), 1);
}

#[test]
fn deleting_with_nothing_selected_says_so() {
    let mut editor = Editor::new();
    editor.act(Action::DeleteSelected);
    assert!(editor.status().contains("nothing selected"), "{}", editor.status());
}

#[test]
fn undoing_past_a_delete_does_not_leave_a_stale_selection() {
    let mut editor = Editor::new();
    editor.act(Action::ImportMedia(vec![testdata("counter_30fps.mp4")]));
    let asset = editor.state.project.assets[0].id;
    let v1 = editor.first_video_track();
    editor.act(Action::AddAssetToTimeline { asset, track: v1, at: Ticks::ZERO });
    let clip = editor.sequence().track(v1).unwrap().clips()[0].id;
    editor.act(Action::SelectClip { clip, track: v1, additive: false });

    // Undoing the add removes the clip the selection points at.
    editor.act(Action::Undo);
    assert!(
        editor.state.selection.clips.is_empty(),
        "a selection must not outlive the clip it refers to"
    );
}

#[test]
fn setting_a_property_through_the_inspector_is_undoable() {
    let mut editor = Editor::new();
    editor.act(Action::ImportMedia(vec![testdata("counter_30fps.mp4")]));
    let asset = editor.state.project.assets[0].id;
    let v1 = editor.first_video_track();
    editor.act(Action::AddAssetToTimeline { asset, track: v1, at: Ticks::ZERO });
    let clip = editor.sequence().track(v1).unwrap().clips()[0].id;

    for step in 1..=10 {
        editor.act(Action::SetClipProperty {
            clip,
            property: ClipProperty::Opacity,
            value: PropertyValue::Scalar(step as f64 / 10.0),
        });
    }
    editor.act(Action::EndGesture);
    let opacity = editor.sequence().find_clip(clip).unwrap().1.transform.opacity.value;
    assert!((opacity - 1.0).abs() < 1e-9);

    editor.act(Action::Undo);
    let opacity = editor.sequence().find_clip(clip).unwrap().1.transform.opacity.value;
    assert!((opacity - 1.0).abs() < 1e-9, "a slider drag is one undo step");
}

#[test]
fn the_playhead_survives_a_save_and_reopen() {
    let mut editor = Editor::new();
    editor.act(Action::ImportMedia(vec![testdata("counter_30fps.mp4")]));
    let asset = editor.state.project.assets[0].id;
    let v1 = editor.first_video_track();
    editor.act(Action::AddAssetToTimeline { asset, track: v1, at: Ticks::ZERO });
    editor.act(Action::ScrubTo(Ticks::from_seconds(2)));

    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("playhead.verge");
    editor.act(Action::SaveProjectAs(path.clone()));

    let mut reopened = Editor::new();
    reopened.act(Action::OpenProject(path));
    assert_eq!(reopened.engine.clock().position(), Ticks::from_seconds(2));
}

#[test]
fn opening_a_project_whose_media_has_moved_still_opens() {
    let dir = tempfile::tempdir().unwrap();
    let media = dir.path().join("clip.mp4");
    std::fs::copy(testdata("counter_30fps.mp4"), &media).unwrap();

    let mut editor = Editor::new();
    editor.act(Action::ImportMedia(vec![media.clone()]));
    let asset = editor.state.project.assets[0].id;
    let v1 = editor.first_video_track();
    editor.act(Action::AddAssetToTimeline { asset, track: v1, at: Ticks::ZERO });

    let path = dir.path().join("moved.verge");
    editor.act(Action::SaveProjectAs(path.clone()));
    std::fs::remove_file(&media).unwrap();

    let mut reopened = Editor::new();
    reopened.act(Action::OpenProject(path));
    assert_eq!(reopened.state.project.clip_count(), 1, "the edit must survive");
    assert!(reopened.state.project.assets[0].offline);
    assert!(
        reopened.state.warnings.iter().any(|w| w.contains("offline")),
        "{:?}",
        reopened.state.warnings
    );
}

#[test]
fn saving_reports_a_failure_rather_than_losing_the_project() {
    let mut editor = Editor::new();
    editor.act(Action::ImportMedia(vec![testdata("counter_30fps.mp4")]));

    // A path inside a file, which cannot be created as a directory.
    let dir = tempfile::tempdir().unwrap();
    let blocker = dir.path().join("blocker");
    std::fs::write(&blocker, b"x").unwrap();
    editor.act(Action::SaveProjectAs(blocker.join("nested").join("p.verge")));

    assert!(editor.status().contains("could not save"), "{}", editor.status());
    assert_eq!(editor.state.project.assets.len(), 1, "the project is untouched");
}

#[test]
fn the_window_title_marks_unsaved_changes() {
    let mut editor = Editor::new();
    assert!(!editor.state.window_title().starts_with('•'));

    editor.act(Action::ImportMedia(vec![testdata("counter_30fps.mp4")]));
    let asset = editor.state.project.assets[0].id;
    let v1 = editor.first_video_track();
    editor.act(Action::AddAssetToTimeline { asset, track: v1, at: Ticks::ZERO });
    assert!(editor.state.window_title().starts_with('•'), "an edit should show as unsaved");

    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("titled.verge");
    editor.act(Action::SaveProjectAs(path));
    assert!(!editor.state.window_title().starts_with('•'));
    assert!(editor.state.window_title().contains("titled"));
}

#[test]
fn zoom_and_snapping_toggles_behave() {
    let mut editor = Editor::new();
    let before = editor.state.timeline.pixels_per_second;
    editor.act(Action::ZoomIn);
    assert!(editor.state.timeline.pixels_per_second > before);
    editor.act(Action::ZoomOut);
    assert!((editor.state.timeline.pixels_per_second - before).abs() < 1.0);

    assert!(editor.state.timeline.snapping);
    editor.act(Action::ToggleSnapping);
    assert!(!editor.state.timeline.snapping);
}

#[test]
fn zoom_to_fit_shows_the_whole_sequence() {
    let mut editor = Editor::new();
    editor.act(Action::ImportMedia(vec![testdata("counter_30fps.mp4")]));
    let asset = editor.state.project.assets[0].id;
    let v1 = editor.first_video_track();
    editor.act(Action::AddAssetToTimeline { asset, track: v1, at: Ticks::ZERO });

    editor.act(Action::ZoomToFit(1000.0));
    let view = &editor.state.timeline;
    let width = editor.sequence().duration().as_secs_f32() * view.pixels_per_second;
    assert!(width <= 1000.0 && width > 800.0, "fitted width was {width}");
    assert_eq!(view.scroll, Ticks::ZERO);
}

#[test]
fn transport_keys_move_the_playhead_by_whole_frames() {
    let mut editor = Editor::new();
    editor.act(Action::ImportMedia(vec![testdata("counter_30fps.mp4")]));
    let asset = editor.state.project.assets[0].id;
    let v1 = editor.first_video_track();
    editor.act(Action::AddAssetToTimeline { asset, track: v1, at: Ticks::ZERO });

    editor.act(Action::GoToStart);
    assert_eq!(editor.engine.clock().position(), Ticks::ZERO);

    editor.act(Action::StepFrames(1));
    assert_eq!(editor.engine.clock().position(), Rate::FPS_30.frame_to_ticks(1));

    editor.act(Action::StepFrames(10));
    assert_eq!(editor.engine.clock().position(), Rate::FPS_30.frame_to_ticks(11));

    editor.act(Action::GoToEnd);
    assert_eq!(editor.engine.clock().position(), Ticks::from_seconds(3));

    // Stepping back past the start clamps rather than going negative.
    editor.act(Action::StepFrames(-10_000));
    assert_eq!(editor.engine.clock().position(), Ticks::ZERO);
}

#[test]
fn a_new_project_discards_the_previous_one_cleanly() {
    let mut editor = Editor::new();
    editor.act(Action::ImportMedia(vec![testdata("counter_30fps.mp4")]));
    let asset = editor.state.project.assets[0].id;
    let v1 = editor.first_video_track();
    editor.act(Action::AddAssetToTimeline { asset, track: v1, at: Ticks::ZERO });

    editor.act(Action::NewProject);
    assert_eq!(editor.state.project.clip_count(), 0);
    assert!(editor.state.project.assets.is_empty());
    assert!(!editor.state.history.can_undo(), "history must not survive a new project");
    assert!(editor.state.path.is_none());
    assert_eq!(editor.engine.clock().position(), Ticks::ZERO);
}

#[test]
fn opening_a_file_that_is_not_a_project_reports_it_and_keeps_the_current_one() {
    let dir = tempfile::tempdir().unwrap();
    let junk = dir.path().join("junk.verge");
    std::fs::write(&junk, b"{}").unwrap();

    let mut editor = Editor::new();
    editor.act(Action::ImportMedia(vec![testdata("counter_30fps.mp4")]));
    editor.act(Action::OpenProject(junk));

    assert!(editor.status().contains("could not open"), "{}", editor.status());
    assert_eq!(editor.state.project.assets.len(), 1, "the open project is untouched");
}

/// Writes a project the graphical run can open, so the interface is exercised
/// against real media rather than an empty timeline.
#[test]
fn write_demo_project_for_manual_run() {
    let Ok(out) = std::env::var("VERGE_DEMO_PROJECT") else {
        return; // only runs when asked for
    };
    let out = Path::new(&out);

    let mut editor = Editor::new();
    editor.act(Action::ImportMedia(vec![
        testdata("counter_30fps.mp4"),
        testdata("counter_25fps.mp4"),
        testdata("tone_48k.wav"),
        testdata("av_30fps.mp4"),
    ]));

    let assets: Vec<_> =
        editor.state.project.assets.iter().map(|a| (a.id, a.name.clone())).collect();
    let video_tracks: Vec<_> = editor
        .sequence()
        .tracks
        .iter()
        .filter(|t| t.kind == TrackKind::Video)
        .map(|t| t.id)
        .collect();
    let audio_track = editor.first_audio_track();

    let mut at = Ticks::ZERO;
    for (asset, name) in &assets {
        let is_audio_only = name.ends_with(".wav");
        let track = if is_audio_only { audio_track } else { video_tracks[0] };
        editor.act(Action::AddAssetToTimeline { asset: *asset, track, at });
        if !is_audio_only {
            at += Ticks::from_seconds(2);
        }
    }
    // A second video layer, to show compositing.
    editor.act(Action::AddAssetToTimeline {
        asset: assets[1].0,
        track: video_tracks[1],
        at: Ticks::from_millis(1500),
    });

    editor.act(Action::SaveProjectAs(out.to_path_buf()));
    assert!(out.exists(), "{}", editor.status());
    eprintln!("wrote demo project to {}", out.display());
}

#[test]
fn an_empty_sequence_adopts_the_first_clips_format() {
    let mut editor = Editor::new();
    // The default sequence is 1920x1080 at 30 fps; the fixture is 160x120.
    assert_eq!(editor.sequence().settings.resolution, ve_core::Size::new(1920, 1080));

    editor.act(Action::ImportMedia(vec![testdata("counter_25fps.mp4")]));
    let asset = editor.state.project.assets[0].id;
    let v1 = editor.first_video_track();
    editor.act(Action::AddAssetToTimeline { asset, track: v1, at: Ticks::ZERO });

    assert_eq!(editor.sequence().settings.resolution, ve_core::Size::new(160, 120));
    assert_eq!(editor.sequence().settings.rate, Rate::FPS_25);

    // A second clip must not change the format again.
    editor.act(Action::ImportMedia(vec![testdata("counter_30fps.mp4")]));
    let other =
        editor.state.project.assets.iter().find(|a| a.name.contains("30fps")).unwrap().id;
    editor.act(Action::AddAssetToTimeline {
        asset: other,
        track: v1,
        at: Ticks::from_seconds(2),
    });
    assert_eq!(
        editor.sequence().settings.rate,
        Rate::FPS_25,
        "an established sequence keeps its format"
    );
}

#[test]
fn undoing_the_first_add_restores_the_sequence_format_too() {
    let mut editor = Editor::new();
    let original = editor.sequence().settings.clone();

    editor.act(Action::ImportMedia(vec![testdata("counter_25fps.mp4")]));
    let asset = editor.state.project.assets[0].id;
    let v1 = editor.first_video_track();
    editor.act(Action::AddAssetToTimeline { asset, track: v1, at: Ticks::ZERO });

    // Two steps: the clip, then the format change that preceded it.
    editor.act(Action::Undo);
    editor.act(Action::Undo);
    assert_eq!(editor.state.project.clip_count(), 0);
    assert_eq!(editor.sequence().settings, original, "the format must come back too");
}
