//! The audio half of Phase 4, driven through the same action layer the
//! interface uses: fade handles, the crossfade command and the track strip.
//!
//! No window, no device. What is asserted is that the interface's operations do
//! what they say, refuse what they cannot do, and leave one undo step behind a
//! gesture — the parts a screenshot cannot show.

use std::path::PathBuf;
use std::sync::Arc;

use ve_app::actions::{dispatch, Action};
use ve_app::state::EditorState;
use ve_command::TrackLevel;
use ve_core::{ClipId, FadeCurve, FadeEdge, Project, Track, TrackId, TrackKind};
use ve_engine::{ManualTime, PlaybackClock, PlaybackEngine};
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
        let waveforms = WaveformService::new(4, metrics);
        let state = EditorState::new(
            Project::with_default_sequence("Untitled"),
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

    fn sequence(&self) -> &ve_core::Sequence {
        self.state.active_sequence().unwrap()
    }

    fn audio_track(&self) -> TrackId {
        self.sequence().tracks.iter().find(|t| t.kind == TrackKind::Audio).unwrap().id
    }

    fn add_audio_track(&mut self) -> TrackId {
        let id = self.state.project.new_track_id();
        let sequence = self.state.active_sequence_id().unwrap();
        self.state.project.sequence_mut(sequence).unwrap().tracks.push(Track::new(
            id,
            TrackKind::Audio,
            "A9",
        ));
        id
    }

    /// Imports the tone fixture and places it, returning the clip.
    fn place_tone(&mut self, track: TrackId, at: Ticks) -> ClipId {
        if self.state.project.assets.is_empty() {
            self.act(Action::ImportMedia(vec![testdata("tone_48k.wav")]));
            assert_eq!(self.state.project.assets.len(), 1, "{}", self.status());
        }
        let asset = self.state.project.assets[0].id;
        let before: Vec<ClipId> =
            self.sequence().tracks.iter().flat_map(|t| t.clips()).map(|c| c.id).collect();
        self.act(Action::AddAssetToTimeline { asset, track, at });
        self.sequence()
            .tracks
            .iter()
            .flat_map(|t| t.clips())
            .map(|c| c.id)
            .find(|id| !before.contains(id))
            .unwrap_or_else(|| panic!("the clip was not placed: {}", self.status()))
    }

    fn fade(&self, clip: ClipId, edge: FadeEdge) -> ve_core::Fade {
        self.sequence().find_clip(clip).unwrap().1.audio.fade(edge)
    }

    fn track(&self, track: TrackId) -> &Track {
        self.sequence().track(track).unwrap()
    }
}

#[test]
fn a_fade_handle_drag_sets_one_fade_and_leaves_one_undo_step() {
    let mut editor = Editor::new();
    let a1 = editor.audio_track();
    let clip = editor.place_tone(a1, Ticks::ZERO);

    // Three pointer moves, as a drag produces.
    for ms in [100, 300, 600] {
        editor.act(Action::SetClipFade {
            clip,
            edge: FadeEdge::In,
            length: Ticks::from_millis(ms),
            curve: FadeCurve::EqualPower,
            coalesce: true,
        });
    }
    editor.act(Action::EndGesture);

    assert_eq!(editor.fade(clip, FadeEdge::In).length, Ticks::from_millis(600));
    assert_eq!(editor.fade(clip, FadeEdge::In).curve, FadeCurve::EqualPower);
    assert!(!editor.fade(clip, FadeEdge::Out).is_active(), "only one end was dragged");
    assert_eq!(editor.state.history.undo_depth(), 2, "one for the clip, one for the drag");

    editor.act(Action::Undo);
    assert!(!editor.fade(clip, FadeEdge::In).is_active());
}

#[test]
fn a_fade_changes_what_the_mixer_will_apply_not_just_what_is_stored() {
    let mut editor = Editor::new();
    let a1 = editor.audio_track();
    let clip = editor.place_tone(a1, Ticks::ZERO);
    let duration = editor.sequence().find_clip(clip).unwrap().1.duration;

    editor.act(Action::SetClipFade {
        clip,
        edge: FadeEdge::In,
        length: Ticks::from_millis(400),
        curve: FadeCurve::Linear,
        coalesce: false,
    });

    let audio = &editor.sequence().find_clip(clip).unwrap().1.audio;
    assert_eq!(audio.evaluate(Ticks::ZERO, duration).0, 0.0);
    let halfway = audio.evaluate(Ticks::from_millis(200), duration).0;
    assert!((halfway - 0.5).abs() < 1e-6, "got {halfway}");
    assert_eq!(audio.evaluate(Ticks::from_millis(400), duration).0, 1.0);
}

#[test]
fn a_fade_can_be_dragged_back_to_nothing() {
    let mut editor = Editor::new();
    let a1 = editor.audio_track();
    let clip = editor.place_tone(a1, Ticks::ZERO);

    editor.act(Action::SetClipFade {
        clip,
        edge: FadeEdge::Out,
        length: Ticks::from_millis(500),
        curve: FadeCurve::Smooth,
        coalesce: false,
    });
    assert!(editor.fade(clip, FadeEdge::Out).is_active());

    editor.act(Action::SetClipFade {
        clip,
        edge: FadeEdge::Out,
        length: Ticks::ZERO,
        curve: FadeCurve::Smooth,
        coalesce: false,
    });
    assert!(!editor.fade(clip, FadeEdge::Out).is_active());
}

#[test]
fn crossfading_two_clips_on_one_track_is_refused_with_a_reason() {
    let mut editor = Editor::new();
    let a1 = editor.audio_track();
    let first = editor.place_tone(a1, Ticks::ZERO);
    let second = editor.place_tone(a1, Ticks::from_seconds(1));

    editor.act(Action::SelectClip { clip: first, track: a1, additive: false });
    editor.act(Action::SelectClip { clip: second, track: a1, additive: true });
    editor.act(Action::CrossfadeSelection(FadeCurve::EqualPower));

    assert!(
        editor.status().contains("overlapping"),
        "the refusal must say what is wrong: {}",
        editor.status()
    );
    assert!(!editor.fade(first, FadeEdge::Out).is_active());
    assert!(!editor.fade(second, FadeEdge::In).is_active());
}

#[test]
fn crossfading_two_overlapping_clips_fades_both_across_the_overlap() {
    let mut editor = Editor::new();
    let a1 = editor.audio_track();
    let a2 = editor.add_audio_track();
    let outgoing = editor.place_tone(a1, Ticks::ZERO);
    let incoming = editor.place_tone(a2, Ticks::from_millis(600));

    let overlap = {
        let out = editor.sequence().find_clip(outgoing).unwrap().1;
        let inc = editor.sequence().find_clip(incoming).unwrap().1;
        out.timeline_end() - inc.timeline_start
    };
    assert!(overlap.raw() > 0, "the fixture must actually overlap");

    editor.act(Action::SelectClip { clip: outgoing, track: a1, additive: false });
    editor.act(Action::SelectClip { clip: incoming, track: a2, additive: true });
    editor.act(Action::CrossfadeSelection(FadeCurve::EqualPower));

    assert_eq!(editor.fade(outgoing, FadeEdge::Out).length, overlap, "{}", editor.status());
    assert_eq!(editor.fade(incoming, FadeEdge::In).length, overlap);
    assert_eq!(editor.fade(outgoing, FadeEdge::Out).curve, FadeCurve::EqualPower);
}

#[test]
fn crossfading_needs_exactly_two_clips() {
    let mut editor = Editor::new();
    let a1 = editor.audio_track();
    let clip = editor.place_tone(a1, Ticks::ZERO);

    editor.act(Action::ClearSelection);
    editor.act(Action::CrossfadeSelection(FadeCurve::EqualPower));
    assert!(editor.status().contains("two clips"), "{}", editor.status());

    editor.act(Action::SelectClip { clip, track: a1, additive: false });
    editor.act(Action::CrossfadeSelection(FadeCurve::EqualPower));
    assert!(editor.status().contains("two clips"), "{}", editor.status());
}

#[test]
fn a_track_fader_drag_is_one_undo_step_and_is_bounded() {
    let mut editor = Editor::new();
    let a1 = editor.audio_track();

    for value in [0.9, 0.6, 0.3] {
        editor.act(Action::SetTrackLevel { track: a1, which: TrackLevel::Volume, value });
    }
    editor.act(Action::EndGesture);
    assert_eq!(editor.track(a1).volume, 0.3);
    assert_eq!(editor.state.history.undo_depth(), 1);

    editor.act(Action::SetTrackLevel { track: a1, which: TrackLevel::Volume, value: 99.0 });
    editor.act(Action::EndGesture);
    assert_eq!(editor.track(a1).volume, 4.0, "a fader cannot run past +12 dB");

    editor.act(Action::SetTrackLevel { track: a1, which: TrackLevel::Pan, value: -0.4 });
    editor.act(Action::EndGesture);
    assert_eq!(editor.track(a1).audio_level(), (4.0, -0.4));
}

#[test]
fn the_track_level_reaches_the_mix_through_the_plan() {
    let mut editor = Editor::new();
    let a1 = editor.audio_track();
    let clip = editor.place_tone(a1, Ticks::ZERO);
    editor.act(Action::SetTrackLevel { track: a1, which: TrackLevel::Volume, value: 0.5 });

    let sequence = editor.sequence();
    let plan =
        ve_engine::evaluate_project(&editor.state.project, sequence, Ticks::from_millis(100));
    assert_eq!(plan.audio.len(), 1);
    assert_eq!(plan.audio[0].gain, 0.5);
    assert_eq!(plan.audio[0].track, Some(a1));
    // And the item points back at the clip the user can select.
    assert_eq!(plan.audio[0].origin, ve_engine::Origin::Clip(clip));
}

#[test]
fn undoing_a_track_level_restores_it_and_marks_the_project_changed() {
    let mut editor = Editor::new();
    let a1 = editor.audio_track();
    let before = editor.state.revision();

    editor.act(Action::SetTrackLevel { track: a1, which: TrackLevel::Volume, value: 0.25 });
    editor.act(Action::EndGesture);
    assert!(editor.state.revision() > before, "the mixer has to learn about this");

    editor.act(Action::Undo);
    assert_eq!(editor.track(a1).volume, 1.0);
}
