//! Grading driven through the same action layer the inspector uses.
//!
//! What is asserted here is what neither a unit test on a shader nor a
//! screenshot can show: that a grade added from the menu is a keyframable
//! effect like any other, that it survives a round trip through the project
//! file, and that the scopes are switched from one place and cost nothing while
//! they are closed.

use std::path::PathBuf;
use std::sync::Arc;

use ve_app::actions::{dispatch, Action};
use ve_app::state::{EditorState, ScopeKind};
use ve_command::{animatable_properties, ClipProperty, PropertyValue};
use ve_core::registry::{builtin_registry, kinds, EffectCategory};
use ve_core::{Clip, ClipId, Effect, Project, Rgba, TrackId, TrackKind};
use ve_engine::{ManualTime, PlaybackClock, PlaybackEngine};
use ve_media::{DecodeService, WaveformService};
use ve_metrics::Metrics;
use ve_render::WaveformMode;
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

    fn place(&mut self) -> ClipId {
        self.act(Action::ImportMedia(vec![testdata("counter_30fps.mp4")]));
        let asset = self.state.project.assets[0].id;
        let track = self.video_track();
        self.act(Action::AddAssetToTimeline { asset, track, at: Ticks::ZERO });
        self.sequence()
            .tracks
            .iter()
            .flat_map(|t| t.clips())
            .map(|c| c.id)
            .next()
            .unwrap_or_else(|| panic!("the clip was not placed: {}", self.status()))
    }

    fn clip(&self, id: ClipId) -> &Clip {
        self.sequence().find_clip(id).expect("the clip is still there").1
    }

    fn effects(&self, clip: ClipId) -> &[Effect] {
        &self.clip(clip).effects
    }
}

#[test]
fn a_grade_is_added_and_undone_like_any_other_effect() {
    let mut editor = Editor::new();
    let clip = editor.place();
    for kind in [kinds::THREE_WAY, kinds::WHITE_BALANCE, kinds::HSL_SECONDARY] {
        editor.act(Action::AddEffect { clip, kind: kind.to_string() });
    }
    assert_eq!(editor.effects(clip).len(), 3, "{}", editor.status());

    editor.act(Action::Undo);
    assert_eq!(editor.effects(clip).len(), 2, "one undo step per effect");
}

#[test]
fn the_three_grades_are_offered_from_the_colour_menu() {
    // The inspector's menu is built from the registry, so an effect that is
    // registered is an effect the user can reach. That is the whole of what
    // adding one took.
    let offered: Vec<&str> = builtin_registry()
        .in_category(EffectCategory::Color)
        .map(|d| d.kind.as_str())
        .collect();
    assert!(offered.contains(&kinds::THREE_WAY));
    assert!(offered.contains(&kinds::WHITE_BALANCE));
    assert!(offered.contains(&kinds::HSL_SECONDARY));
}

#[test]
fn a_wheel_keyframes_through_the_animation_system_it_shares() {
    // The point of the parameters being `Property<T>` is that a colour wheel
    // gets keyframes, a graph editor and an undo step for nothing. If a wheel
    // shows up as an animatable property, all of that already works.
    let mut editor = Editor::new();
    let clip = editor.place();
    editor.act(Action::AddEffect { clip, kind: kinds::THREE_WAY.to_string() });
    let effect = editor.effects(clip)[0].id;

    let animatable = animatable_properties(editor.clip(clip));
    assert!(
        animatable.contains(&ClipProperty::EffectParam { effect, key: "shadows".to_string() }),
        "a wheel is animatable: {animatable:?}"
    );
    assert!(animatable
        .contains(&ClipProperty::EffectParam { effect, key: "shadow_level".to_string() }));
    // The switch is not, which is the rule for every stepped parameter.
    assert!(!animatable.iter().any(|p| matches!(
        p,
        ClipProperty::EffectParam { key, .. } if key == "show_matte"
    )));
}

#[test]
fn a_grade_survives_being_saved_and_reopened() {
    let mut editor = Editor::new();
    let clip = editor.place();
    editor.act(Action::AddEffect { clip, kind: kinds::THREE_WAY.to_string() });
    let effect = editor.effects(clip)[0].id;
    let warm = Rgba::new(0.7, 0.5, 0.3, 1.0);
    editor.act(Action::SetClipProperty {
        clip,
        property: ClipProperty::EffectParam { effect, key: "shadows".to_string() },
        value: PropertyValue::Color(warm),
    });

    let path = editor._scratch.path().join("graded.verge");
    editor.act(Action::SaveProjectAs(path.clone()));
    assert!(path.exists(), "{}", editor.status());

    let mut reopened = Editor::new();
    reopened.act(Action::OpenProject(path));
    let clip = reopened
        .sequence()
        .tracks
        .iter()
        .flat_map(|t| t.clips())
        .map(|c| c.id)
        .next()
        .expect("the clip came back");
    let effect = &reopened.effects(clip)[0];
    assert_eq!(effect.kind, kinds::THREE_WAY);
    assert_eq!(
        effect.param("shadows").unwrap().evaluate(Ticks::ZERO),
        ve_core::ParamState::Color(warm),
        "the wheel came back where it was left"
    );
}

#[test]
fn the_scopes_are_closed_until_they_are_asked_for() {
    // Closed is the default, and closed is what costs nothing: the picture is
    // only read back off the GPU while a scope is on screen to read it.
    let mut editor = Editor::new();
    assert!(!editor.state.scopes.open);

    editor.act(Action::ToggleScopes);
    assert!(editor.state.scopes.open);
    editor.act(Action::ToggleScopes);
    assert!(!editor.state.scopes.open);
}

#[test]
fn picking_a_scope_opens_the_panel_on_it() {
    // Otherwise the menu entry for a vectorscope would do nothing visible until
    // the panel had separately been opened, which is a setting rather than a
    // command.
    let mut editor = Editor::new();
    editor.act(Action::ShowScope(ScopeKind::Vectorscope));
    assert!(editor.state.scopes.open);
    assert_eq!(editor.state.scopes.kind, ScopeKind::Vectorscope);

    editor.act(Action::ShowScope(ScopeKind::Histogram));
    assert_eq!(editor.state.scopes.kind, ScopeKind::Histogram);
    assert!(editor.state.scopes.open, "and leaves it open");
}

#[test]
fn the_waveform_mode_is_remembered_across_scopes() {
    let mut editor = Editor::new();
    assert_eq!(editor.state.scopes.waveform, WaveformMode::Luma);
    editor.act(Action::SetWaveformMode(WaveformMode::Parade));
    editor.act(Action::ShowScope(ScopeKind::Vectorscope));
    editor.act(Action::ShowScope(ScopeKind::Waveform));
    assert_eq!(
        editor.state.scopes.waveform,
        WaveformMode::Parade,
        "coming back to the waveform shows the one that was being read"
    );
}
