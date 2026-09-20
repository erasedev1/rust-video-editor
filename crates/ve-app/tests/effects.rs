//! Phase 6 driven through the same action layer the inspector uses: building a
//! chain, reordering it, switching effects off, and animating a parameter.
//!
//! No window. What is asserted is what a screenshot cannot show — that an
//! effect parameter keyframes through exactly the machinery every other
//! property uses, that a rename while typing is one undo step, and that undo
//! puts a removed effect back where it was rather than on the end.

use std::path::PathBuf;
use std::sync::Arc;

use ve_app::actions::{dispatch, Action};
use ve_app::state::EditorState;
use ve_command::{animatable_properties, ClipProperty, PropertyValue};
use ve_core::registry::kinds;
use ve_core::{Clip, ClipId, Effect, EffectId, ParamValue, Project, TrackId, TrackKind};
use ve_engine::{evaluate_project, ManualTime, PlaybackClock, PlaybackEngine};
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

    fn video_track(&self) -> TrackId {
        self.sequence().tracks.iter().find(|t| t.kind == TrackKind::Video).unwrap().id
    }

    fn place(&mut self, track: TrackId, at: Ticks) -> ClipId {
        if self.state.project.assets.is_empty() {
            self.act(Action::ImportMedia(vec![testdata("counter_30fps.mp4")]));
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

    fn clip(&self, id: ClipId) -> &Clip {
        self.sequence().find_clip(id).expect("the clip is still there").1
    }

    fn effects(&self, clip: ClipId) -> &[Effect] {
        &self.clip(clip).effects
    }

    fn kinds(&self, clip: ClipId) -> Vec<String> {
        self.effects(clip).iter().map(|e| e.kind.clone()).collect()
    }

    fn add(&mut self, clip: ClipId, kind: &str) -> EffectId {
        self.act(Action::AddEffect { clip, kind: kind.to_string() });
        self.effects(clip).last().unwrap_or_else(|| panic!("{}", self.status())).id
    }
}

#[test]
fn an_effect_is_added_switched_off_and_removed_through_the_action_layer() {
    let mut editor = Editor::new();
    let v1 = editor.video_track();
    let clip = editor.place(v1, Ticks::ZERO);

    let blur = editor.add(clip, kinds::GAUSSIAN_BLUR);
    assert_eq!(editor.kinds(clip), vec![kinds::GAUSSIAN_BLUR]);

    editor.act(Action::SetEffectEnabled { clip, effect: blur, enabled: false });
    assert!(!editor.effects(clip)[0].enabled);

    editor.act(Action::RemoveEffect { clip, effect: blur });
    assert!(editor.effects(clip).is_empty());

    editor.act(Action::Undo);
    assert_eq!(editor.kinds(clip), vec![kinds::GAUSSIAN_BLUR]);
    assert!(!editor.effects(clip)[0].enabled, "it came back as it was, not as new");
}

#[test]
fn reordering_from_the_inspector_changes_what_the_renderer_is_asked_to_draw() {
    let mut editor = Editor::new();
    let v1 = editor.video_track();
    let clip = editor.place(v1, Ticks::ZERO);

    editor.add(clip, kinds::GAUSSIAN_BLUR);
    let colour = editor.add(clip, kinds::COLOR_ADJUST);
    editor.act(Action::MoveEffect { clip, effect: colour, to: 0 });

    let sequence = editor.sequence();
    let plan = evaluate_project(&editor.state.project, sequence, Ticks::ZERO);
    let chain: Vec<String> =
        plan.root().items[0].effects.iter().map(|e| e.kind.clone()).collect();
    assert_eq!(chain, vec![kinds::COLOR_ADJUST, kinds::GAUSSIAN_BLUR]);
}

#[test]
fn a_switched_off_effect_is_not_in_the_plan_but_is_still_on_the_clip() {
    let mut editor = Editor::new();
    let v1 = editor.video_track();
    let clip = editor.place(v1, Ticks::ZERO);
    let blur = editor.add(clip, kinds::GAUSSIAN_BLUR);
    editor.act(Action::SetEffectEnabled { clip, effect: blur, enabled: false });

    let sequence = editor.sequence();
    let plan = evaluate_project(&editor.state.project, sequence, Ticks::ZERO);
    assert!(!plan.root().items[0].has_effects(), "nothing is drawn for it");
    assert_eq!(editor.effects(clip).len(), 1, "and nothing was lost");
}

#[test]
fn an_effect_parameter_keyframes_at_the_playhead_like_every_other_property() {
    let mut editor = Editor::new();
    let v1 = editor.video_track();
    let clip = editor.place(v1, Ticks::ZERO);
    let blur = editor.add(clip, kinds::GAUSSIAN_BLUR);
    let radius = ClipProperty::EffectParam { effect: blur, key: "radius".into() };

    // The ◆ button, twice, at two instants: exactly what an animated transform
    // property does, with no effect-specific path anywhere.
    for second in [0, 2] {
        editor.act(Action::ScrubTo(Ticks::from_seconds(second)));
        editor.act(Action::ToggleKeyframeAtPlayhead { clip, property: radius.clone() });
    }
    assert!(editor.effects(clip)[0].is_animated(), "{}", editor.status());

    // With the property animated, an inspector drag writes a keyframe at the
    // playhead rather than a static value the animation would override.
    editor.act(Action::ScrubTo(Ticks::from_seconds(2)));
    editor.act(Action::SetClipProperty {
        clip,
        property: radius.clone(),
        value: PropertyValue::Scalar(64.0),
    });

    let sequence = editor.sequence();
    let plan = evaluate_project(&editor.state.project, sequence, Ticks::from_seconds(2));
    assert_eq!(plan.root().items[0].effects[0].scalar("radius", -1.0), 64.0);
    // Halfway between the two keyframes, halfway between the two radii.
    let plan = evaluate_project(&editor.state.project, sequence, Ticks::from_seconds(1));
    let midpoint = plan.root().items[0].effects[0].scalar("radius", -1.0);
    assert!((midpoint - 36.0).abs() < 1e-6, "got {midpoint}");
}

#[test]
fn an_effect_parameter_is_offered_to_the_animation_editor() {
    let mut editor = Editor::new();
    let v1 = editor.video_track();
    let clip = editor.place(v1, Ticks::ZERO);
    let blur = editor.add(clip, kinds::GAUSSIAN_BLUR);

    let properties = animatable_properties(editor.clip(clip));
    assert!(
        properties.contains(&ClipProperty::EffectParam { effect: blur, key: "radius".into() })
    );
    // The direction is a choice, so it is not something the graph editor can
    // draw a curve for.
    assert!(!properties
        .contains(&ClipProperty::EffectParam { effect: blur, key: "direction".into() }));
}

#[test]
fn a_switch_is_set_through_its_own_action_and_undoes() {
    let mut editor = Editor::new();
    let v1 = editor.video_track();
    let clip = editor.place(v1, Ticks::ZERO);
    let mask = editor.add(clip, kinds::SHAPE_MASK);

    editor.act(Action::SetEffectOption {
        clip,
        effect: mask,
        key: "invert".into(),
        value: ParamValue::Bool(true),
    });
    assert_eq!(editor.effects(clip)[0].param("invert"), Some(&ParamValue::Bool(true)));

    editor.act(Action::SetEffectOption {
        clip,
        effect: mask,
        key: "shape".into(),
        value: ParamValue::Choice(0),
    });
    assert_eq!(editor.effects(clip)[0].param("shape"), Some(&ParamValue::Choice(0)));

    editor.act(Action::Undo);
    assert_eq!(editor.effects(clip)[0].param("shape"), Some(&ParamValue::Choice(1)));
    editor.act(Action::Undo);
    assert_eq!(editor.effects(clip)[0].param("invert"), Some(&ParamValue::Bool(false)));
}

#[test]
fn typing_a_new_name_is_one_undo_step() {
    let mut editor = Editor::new();
    let v1 = editor.video_track();
    let clip = editor.place(v1, Ticks::ZERO);
    let blur = editor.add(clip, kinds::GAUSSIAN_BLUR);
    let depth = editor.state.history.undo_depth();

    for name in ["S", "Sk", "Ski", "Skin softener"] {
        editor.act(Action::RenameEffect { clip, effect: blur, name: name.to_string() });
    }
    assert_eq!(editor.state.history.undo_depth(), depth + 1);
    assert_eq!(editor.effects(clip)[0].name, "Skin softener");

    editor.act(Action::Undo);
    assert_eq!(editor.effects(clip)[0].name, "Gaussian Blur");
}

#[test]
fn an_effect_survives_a_save_and_reopen_with_what_was_dialled_into_it() {
    let mut editor = Editor::new();
    let v1 = editor.video_track();
    let clip = editor.place(v1, Ticks::ZERO);
    let mask = editor.add(clip, kinds::SHAPE_MASK);
    editor.act(Action::SetEffectOption {
        clip,
        effect: mask,
        key: "invert".into(),
        value: ParamValue::Bool(true),
    });
    editor.act(Action::SetClipProperty {
        clip,
        property: ClipProperty::EffectParam { effect: mask, key: "feather".into() },
        value: PropertyValue::Scalar(24.0),
    });

    let path = editor._scratch.path().join("masked.verge");
    editor.act(Action::SaveProjectAs(path.clone()));
    assert!(path.exists(), "{}", editor.status());
    editor.act(Action::OpenProject(path));

    let effect = &editor.effects(clip)[0];
    assert_eq!(effect.kind, kinds::SHAPE_MASK);
    assert_eq!(effect.param("invert"), Some(&ParamValue::Bool(true)));
    assert_eq!(effect.param("feather").unwrap().as_scalar_at(Ticks::ZERO), Some(24.0));
}
