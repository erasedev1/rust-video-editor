//! Graphics through the action layer: making them, editing them, placing them.
//!
//! No window. Every operation the Graphic menu, the media panel and the
//! inspector perform is an `Action`, so the whole feature is drivable
//! headlessly — which is the point of routing everything through one dispatch.

use std::sync::Arc;

use ve_app::actions::{dispatch, Action};
use ve_app::state::EditorState;
use ve_command::{GraphicOption, GraphicProperty, PropertyValue};
use ve_core::{
    GraphicId, Project, Rgba, ShapeKind, Source, TextAlign, TrackId, TrackKind, Vec2,
};
use ve_engine::{ManualTime, PlaybackClock, PlaybackEngine};
use ve_media::{DecodeService, WaveformService};
use ve_metrics::Metrics;
use ve_time::Ticks;

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
            Project::with_default_sequence("Graphics"),
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

    fn only_graphic(&self) -> GraphicId {
        assert_eq!(self.state.project.graphics.len(), 1, "{}", self.status());
        self.state.project.graphics[0].id
    }

    fn clips(&self) -> Vec<(Ticks, Ticks, Source)> {
        let track = self.video_track();
        self.state
            .active_sequence()
            .unwrap()
            .track(track)
            .unwrap()
            .clips()
            .iter()
            .map(|c| (c.timeline_start, c.duration, c.source))
            .collect()
    }

    fn set(&mut self, graphic: GraphicId, target: GraphicProperty, value: PropertyValue) {
        self.act(Action::SetGraphicProperty { graphic, target, value, coalesce: false });
    }
}

// ---- making them --------------------------------------------------------

#[test]
fn a_new_shape_is_created_and_selected() {
    let mut editor = Editor::new();
    editor.act(Action::AddShape(ShapeKind::Rectangle));

    let id = editor.only_graphic();
    assert_eq!(editor.state.selection.graphic, Some(id), "it is what you are editing");
    let shape = editor.state.project.graphic(id).unwrap().as_shape().unwrap();
    assert_eq!(shape.kind, ShapeKind::Rectangle);
}

/// A new shape is sized against the canvas, so it is a sensible fraction of the
/// frame whatever the sequence's resolution — not a fixed pixel count that is
/// enormous on a 720p timeline and a speck on a 4K one.
#[test]
fn a_new_shape_is_sized_against_the_canvas() {
    let mut editor = Editor::new();
    let canvas = editor.state.active_sequence().unwrap().settings.resolution;
    editor.act(Action::AddShape(ShapeKind::Ellipse));

    let id = editor.only_graphic();
    let size = editor.state.project.graphic(id).unwrap().as_shape().unwrap().size.value;
    assert!(size.x > 0.0 && size.x < canvas.width as f64, "{size:?} in {canvas:?}");
    assert!((size.x / canvas.width as f64 - size.y / canvas.height as f64).abs() < 0.01);
}

#[test]
fn a_new_title_carries_placeholder_words() {
    let mut editor = Editor::new();
    editor.act(Action::AddTitle);

    let id = editor.only_graphic();
    let text = editor.state.project.graphic(id).unwrap().as_text().unwrap();
    assert!(!text.text.is_empty(), "an empty title would draw nothing and look broken");
    assert!(text.size.value > 0.0);
}

#[test]
fn graphics_of_the_same_kind_get_names_that_tell_them_apart() {
    let mut editor = Editor::new();
    for _ in 0..3 {
        editor.act(Action::AddShape(ShapeKind::Rectangle));
    }

    let names: Vec<String> =
        editor.state.project.graphics.iter().map(|g| g.name.clone()).collect();
    assert_eq!(names.len(), 3);
    let unique: std::collections::HashSet<&String> = names.iter().collect();
    assert_eq!(unique.len(), 3, "duplicate labels in the list would be useless: {names:?}");
}

#[test]
fn making_a_graphic_undoes() {
    let mut editor = Editor::new();
    editor.act(Action::AddShape(ShapeKind::Rectangle));
    assert_eq!(editor.state.project.graphics.len(), 1);

    editor.act(Action::Undo);
    assert!(editor.state.project.graphics.is_empty());
    assert_eq!(editor.state.selection.graphic, None, "the inspector lets go of it too");
}

#[test]
fn duplicating_selects_the_copy_rather_than_the_original() {
    let mut editor = Editor::new();
    editor.act(Action::AddShape(ShapeKind::Rectangle));
    let original = editor.only_graphic();

    editor.act(Action::DuplicateGraphic(original));
    assert_eq!(editor.state.project.graphics.len(), 2);
    let copy = editor.state.project.graphics[1].id;
    assert_eq!(editor.state.selection.graphic, Some(copy));
}

// ---- editing them -------------------------------------------------------

#[test]
fn editing_a_property_from_the_inspector_undoes() {
    let mut editor = Editor::new();
    editor.act(Action::AddShape(ShapeKind::Rectangle));
    let id = editor.only_graphic();
    let before = editor.state.project.graphic(id).unwrap().as_shape().unwrap().size.value;

    editor.set(id, GraphicProperty::Size, PropertyValue::Point(Vec2::new(123.0, 456.0)));
    let size = editor.state.project.graphic(id).unwrap().as_shape().unwrap().size.value;
    assert_eq!(size, Vec2::new(123.0, 456.0));

    editor.act(Action::Undo);
    assert_eq!(
        editor.state.project.graphic(id).unwrap().as_shape().unwrap().size.value,
        before
    );
}

/// The inspector's rows fire on every move of a drag, so they coalesce.
#[test]
fn dragging_a_slider_in_the_inspector_is_one_undo_step() {
    let mut editor = Editor::new();
    editor.act(Action::AddShape(ShapeKind::Rectangle));
    let id = editor.only_graphic();
    let before = editor.state.project.graphic(id).unwrap().as_shape().unwrap().size.value;

    for width in [200.0, 240.0, 301.0] {
        editor.act(Action::SetGraphicProperty {
            graphic: id,
            target: GraphicProperty::Size,
            value: PropertyValue::Point(Vec2::new(width, 300.0)),
            coalesce: true,
        });
    }
    // A different action ends the gesture, as releasing the mouse would.
    editor.act(Action::SelectGraphic(Some(id)));
    editor.act(Action::Undo);

    assert_eq!(
        editor.state.project.graphic(id).unwrap().as_shape().unwrap().size.value,
        before,
        "the whole drag comes back, not a third of it"
    );
}

#[test]
fn typing_into_a_title_is_one_undo_step() {
    let mut editor = Editor::new();
    editor.act(Action::AddTitle);
    let id = editor.only_graphic();
    let before = editor.state.project.graphic(id).unwrap().as_text().unwrap().text.clone();

    for text in ["A", "Ad", "Ada"] {
        editor.act(Action::SetGraphicText { graphic: id, text: text.to_string() });
    }
    editor.act(Action::SelectGraphic(Some(id)));
    assert_eq!(editor.state.project.graphic(id).unwrap().as_text().unwrap().text, "Ada");

    editor.act(Action::Undo);
    assert_eq!(editor.state.project.graphic(id).unwrap().as_text().unwrap().text, before);
}

#[test]
fn changing_the_shape_and_the_alignment_go_through_the_history() {
    let mut editor = Editor::new();
    editor.act(Action::AddShape(ShapeKind::Rectangle));
    let shape = editor.only_graphic();
    editor.act(Action::SetGraphicOption {
        graphic: shape,
        option: GraphicOption::Kind(ShapeKind::Star { points: 7 }),
    });
    assert_eq!(
        editor.state.project.graphic(shape).unwrap().as_shape().unwrap().kind,
        ShapeKind::Star { points: 7 }
    );
    editor.act(Action::Undo);
    assert_eq!(
        editor.state.project.graphic(shape).unwrap().as_shape().unwrap().kind,
        ShapeKind::Rectangle
    );

    editor.act(Action::AddTitle);
    let title = editor.state.project.graphics[1].id;
    editor.act(Action::SetGraphicOption {
        graphic: title,
        option: GraphicOption::Align(TextAlign::Center),
    });
    assert_eq!(
        editor.state.project.graphic(title).unwrap().as_text().unwrap().align,
        TextAlign::Center
    );
}

#[test]
fn an_edit_that_does_not_apply_says_so_and_changes_nothing() {
    let mut editor = Editor::new();
    editor.act(Action::AddShape(ShapeKind::Rectangle));
    let id = editor.only_graphic();
    let before = editor.state.project.clone();

    // A shape has no words.
    editor.act(Action::SetGraphicText { graphic: id, text: "nope".into() });
    assert_eq!(editor.state.project, before);
    assert!(!editor.status().is_empty(), "the refusal is reported, not swallowed");
}

// ---- placing them -------------------------------------------------------

#[test]
fn a_graphic_goes_onto_the_timeline_at_the_playhead() {
    let mut editor = Editor::new();
    editor.act(Action::AddTitle);
    let id = editor.only_graphic();
    let track = editor.video_track();

    editor.act(Action::AddGraphicToTimeline { graphic: id, track, at: Ticks::from_seconds(4) });

    let clips = editor.clips();
    assert_eq!(clips.len(), 1, "{}", editor.status());
    assert_eq!(clips[0].0, Ticks::from_seconds(4));
    assert_eq!(clips[0].2, Source::Graphic(id));
    assert!(clips[0].1 > Ticks::ZERO, "a still gets a default length");
}

/// Placing it moves the selection to the clip: a graphic in hand and a clip in
/// hand answer different questions, and the inspector can only show one.
#[test]
fn placing_a_graphic_hands_the_selection_to_the_clip() {
    let mut editor = Editor::new();
    editor.act(Action::AddShape(ShapeKind::Rectangle));
    let id = editor.only_graphic();
    let track = editor.video_track();
    assert_eq!(editor.state.selection.graphic, Some(id));

    editor.act(Action::AddGraphicToTimeline { graphic: id, track, at: Ticks::ZERO });
    assert_eq!(editor.state.selection.graphic, None);
    assert_eq!(editor.state.selection.clips.len(), 1);
}

#[test]
fn a_graphic_on_the_timeline_cannot_be_deleted_from_the_panel() {
    let mut editor = Editor::new();
    editor.act(Action::AddShape(ShapeKind::Rectangle));
    let id = editor.only_graphic();
    let track = editor.video_track();
    editor.act(Action::AddGraphicToTimeline { graphic: id, track, at: Ticks::ZERO });

    editor.act(Action::RemoveGraphic(id));
    assert!(editor.state.project.graphic(id).is_some());
    assert!(editor.status().contains("used"), "{}", editor.status());
}

#[test]
fn deleting_an_unused_graphic_works_and_undoes() {
    let mut editor = Editor::new();
    editor.act(Action::AddShape(ShapeKind::Rectangle));
    let id = editor.only_graphic();

    editor.act(Action::RemoveGraphic(id));
    assert!(editor.state.project.graphics.is_empty());
    editor.act(Action::Undo);
    assert!(editor.state.project.graphic(id).is_some());
}

/// Editing a graphic changes what every clip drawing it shows — that is what
/// makes it one object rather than a copy per clip.
#[test]
fn editing_a_graphic_changes_every_clip_that_draws_it() {
    let mut editor = Editor::new();
    editor.act(Action::AddTitle);
    let id = editor.only_graphic();
    let track = editor.video_track();

    editor.act(Action::AddGraphicToTimeline { graphic: id, track, at: Ticks::ZERO });
    editor.act(Action::AddGraphicToTimeline {
        graphic: id,
        track,
        at: Ticks::from_seconds(20),
    });
    assert_eq!(editor.clips().len(), 2);

    editor.act(Action::SetGraphicText { graphic: id, text: "Ada Lovelace".into() });
    for (_, _, source) in editor.clips() {
        assert_eq!(source, Source::Graphic(id), "both clips still draw the one graphic");
    }
    assert_eq!(
        editor.state.project.graphic(id).unwrap().as_text().unwrap().text,
        "Ada Lovelace"
    );
}

#[test]
fn a_graphic_survives_a_save_and_reopen() {
    let mut editor = Editor::new();
    editor.act(Action::AddShape(ShapeKind::Star { points: 5 }));
    let id = editor.only_graphic();
    editor.set(id, GraphicProperty::Fill, PropertyValue::Color(Rgba::new(1.0, 0.2, 0.2, 1.0)));
    let track = editor.video_track();
    editor.act(Action::AddGraphicToTimeline { graphic: id, track, at: Ticks::ZERO });

    let path = editor._scratch.path().join("graphics.verge");
    editor.act(Action::SaveProjectAs(path.clone()));

    editor.act(Action::NewProject);
    assert!(editor.state.project.graphics.is_empty());
    editor.act(Action::OpenProject(path));

    assert_eq!(editor.state.project.graphics.len(), 1, "{}", editor.status());
    let shape = editor.state.project.graphic(id).unwrap().as_shape().unwrap();
    assert_eq!(shape.kind, ShapeKind::Star { points: 5 });
    assert_eq!(shape.fill.value, Rgba::new(1.0, 0.2, 0.2, 1.0));
    assert_eq!(editor.clips().len(), 1);
    assert!(editor.state.warnings.is_empty(), "{:?}", editor.state.warnings);
}

/// Selecting a graphic and selecting a clip are exclusive, because the
/// inspector shows one thing and Delete has to know what it is taking.
#[test]
fn picking_a_graphic_lets_go_of_the_clip_selection() {
    let mut editor = Editor::new();
    editor.act(Action::AddShape(ShapeKind::Rectangle));
    let id = editor.only_graphic();
    let track = editor.video_track();
    editor.act(Action::AddGraphicToTimeline { graphic: id, track, at: Ticks::ZERO });
    assert_eq!(editor.state.selection.clips.len(), 1);

    editor.act(Action::SelectGraphic(Some(id)));
    assert!(editor.state.selection.clips.is_empty());
    assert_eq!(editor.state.selection.graphic, Some(id));
}
