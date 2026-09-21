//! Creating and editing graphics through the history.
//!
//! What is asserted here is the command contract, the same one cutting obeys:
//! a refused command touches nothing, an undo restores the project exactly, a
//! redo reproduces the edit including its IDs, and a gesture that restates
//! itself — a slider drag, a run of typing — collapses into one undo step.
//!
//! The point of a graphic being a project-level object edited through ordinary
//! commands is that "typed a letter into a title" and "trimmed a shot" land in
//! one history. Several of these tests interleave the two to show that they do.

use ve_command::*;
use ve_core::{
    ClipId, FontSpec, Graphic, GraphicContent, GraphicId, IdAllocator, Project, Rgba,
    SequenceId, Shape, ShapeKind, Source, Text, TextAlign, TrackId, TrackKind, Vec2,
};
use ve_time::Ticks;

/// A project with its ID allocator zeroed, for comparing two states.
///
/// Undo deliberately does not rewind the allocator — an ID handed out once is
/// never handed out again — so an undone edit leaves a project equal to the
/// original in every way except how many IDs have been spent.
fn shape_of(project: &Project) -> Project {
    let mut project = project.clone();
    project.ids = IdAllocator::new();
    project
}

struct Fixture {
    project: Project,
    sequence: SequenceId,
    track: TrackId,
    history: History,
}

fn fixture() -> Fixture {
    let project = Project::with_default_sequence("Graphics");
    let sequence = project.active_sequence.unwrap();
    let track = project
        .sequence(sequence)
        .unwrap()
        .tracks
        .iter()
        .find(|t| t.kind == TrackKind::Video)
        .unwrap()
        .id;
    Fixture { project, sequence, track, history: History::default() }
}

fn rectangle() -> Shape {
    Shape::new(ShapeKind::Rectangle, Vec2::new(400.0, 300.0), Rgba::WHITE)
}

impl Fixture {
    /// Adds a graphic through the history and returns its ID.
    fn add(&mut self, name: &str, content: GraphicContent) -> GraphicId {
        let mut command = AddGraphic::new(name, content);
        command.apply(&mut self.project).unwrap();
        let id = command.graphic_id().unwrap();
        command.undo(&mut self.project).unwrap();
        self.history.execute(&mut self.project, Box::new(command)).unwrap();
        id
    }

    fn add_shape(&mut self) -> GraphicId {
        self.add("Rectangle", GraphicContent::Shape(rectangle()))
    }

    fn add_text(&mut self) -> GraphicId {
        self.add("Title", GraphicContent::Text(Text::new("Hello", 72.0, Rgba::WHITE)))
    }

    fn place(&mut self, graphic: GraphicId, at: Ticks) -> ClipId {
        let clip = graphic_clip(&mut self.project, graphic, at).unwrap();
        let id = clip.id;
        self.history
            .execute(&mut self.project, Box::new(AddClip::new(self.sequence, self.track, clip)))
            .unwrap();
        id
    }

    fn text_of(&self, id: GraphicId) -> String {
        self.project.graphic(id).unwrap().as_text().unwrap().text.clone()
    }

    fn shape_size(&self, id: GraphicId) -> Vec2 {
        self.project.graphic(id).unwrap().as_shape().unwrap().size.value
    }

    /// Keyframes a shape's size at each `(second, width)`, through the history.
    fn keyframe_sizes(&mut self, id: GraphicId, points: &[(i64, f64)]) {
        for (second, width) in points {
            self.history
                .execute(
                    &mut self.project,
                    Box::new(EditGraphicKeyframes::new(
                        id,
                        vec![(
                            GraphicProperty::Size,
                            KeyframeEdit::Set {
                                time: Ticks::from_seconds(*second),
                                value: PropertyValue::Point(Vec2::new(*width, 300.0)),
                                interpolation: ve_core::Interpolation::Linear,
                            },
                        )],
                    )),
                )
                .unwrap();
        }
    }

    fn size_times(&self, id: GraphicId) -> Vec<Ticks> {
        self.project
            .graphic(id)
            .unwrap()
            .as_shape()
            .unwrap()
            .size
            .keyframes()
            .iter()
            .map(|k| k.time)
            .collect()
    }

    fn clip_count(&self) -> usize {
        self.project.sequence(self.sequence).unwrap().track(self.track).unwrap().clips().len()
    }
}

// ---- creating and deleting ---------------------------------------------

#[test]
fn adding_a_graphic_undoes_and_redoes_with_the_same_id() {
    let mut f = fixture();
    let before = f.project.clone();

    let mut command = AddGraphic::shape("Rectangle", rectangle());
    command.apply(&mut f.project).unwrap();
    let id = command.graphic_id().unwrap();
    command.undo(&mut f.project).unwrap();
    assert_eq!(shape_of(&f.project), shape_of(&before), "undo restores the project");

    f.history.execute(&mut f.project, Box::new(command)).unwrap();
    let after = f.project.clone();
    assert_eq!(f.project.graphics.len(), 1);

    f.history.undo(&mut f.project).unwrap();
    assert!(f.project.graphic(id).is_none());
    f.history.redo(&mut f.project).unwrap();
    assert_eq!(f.project, after, "a redone graphic is the same graphic");
    assert!(f.project.graphic(id).is_some(), "so a clip drawing it still does");
}

/// The case the "as it now stands" comment in `AddGraphic::undo` is about: an
/// undo that reached back past later edits would lose them on redo.
#[test]
fn undoing_a_create_after_editing_it_redoes_the_edits_too() {
    let mut f = fixture();
    let id = f.add_text();

    f.history.execute(&mut f.project, Box::new(SetGraphicText::new(id, "Edited"))).unwrap();
    assert_eq!(f.text_of(id), "Edited");

    // Undo the edit, then the create, then redo both.
    f.history.undo(&mut f.project).unwrap();
    f.history.undo(&mut f.project).unwrap();
    assert!(f.project.graphic(id).is_none());

    f.history.redo(&mut f.project).unwrap();
    assert_eq!(f.text_of(id), "Hello", "the create comes back as it was created");
    f.history.redo(&mut f.project).unwrap();
    assert_eq!(f.text_of(id), "Edited", "and the edit on top of it");
}

#[test]
fn a_graphic_on_the_timeline_cannot_be_deleted() {
    let mut f = fixture();
    let id = f.add_shape();
    f.place(id, Ticks::ZERO);
    let before = f.project.clone();

    let result = f.history.execute(&mut f.project, Box::new(RemoveGraphic::new(id)));
    assert!(result.is_err(), "a dangling source would be a hole in someone's edit");
    assert_eq!(f.project, before);
}

#[test]
fn deleting_an_unused_graphic_puts_it_back_where_it_was() {
    let mut f = fixture();
    let first = f.add_shape();
    let middle = f.add_text();
    let last = f.add_shape();

    f.history.execute(&mut f.project, Box::new(RemoveGraphic::new(middle))).unwrap();
    assert_eq!(f.project.graphics.len(), 2);

    f.history.undo(&mut f.project).unwrap();
    let order: Vec<GraphicId> = f.project.graphics.iter().map(|g| g.id).collect();
    assert_eq!(order, vec![first, middle, last], "back in place, not at the end");
}

#[test]
fn duplicating_a_graphic_copies_it_without_sharing_it() {
    let mut f = fixture();
    let id = f.add_shape();

    let mut command = DuplicateGraphic::new(id);
    command.apply(&mut f.project).unwrap();
    command.undo(&mut f.project).unwrap();
    f.history.execute(&mut f.project, Box::new(command)).unwrap();

    assert_eq!(f.project.graphics.len(), 2);
    let copy = f.project.graphics.last().unwrap().id;
    assert_ne!(copy, id);

    // Editing the copy must leave the original alone, or it was a reference
    // rather than a duplicate.
    f.history
        .execute(
            &mut f.project,
            Box::new(SetGraphicProperty::new(
                copy,
                GraphicProperty::Size,
                PropertyValue::Point(Vec2::new(10.0, 10.0)),
            )),
        )
        .unwrap();
    assert_eq!(f.shape_size(copy), Vec2::new(10.0, 10.0));
    assert_eq!(f.shape_size(id), Vec2::new(400.0, 300.0));
}

#[test]
fn renaming_a_graphic_undoes() {
    let mut f = fixture();
    let id = f.add_shape();
    let before = f.project.clone();

    f.history.execute(&mut f.project, Box::new(RenameGraphic::new(id, "Lower third"))).unwrap();
    assert_eq!(f.project.graphic(id).unwrap().name, "Lower third");

    f.history.undo(&mut f.project).unwrap();
    assert_eq!(f.project, before);
}

#[test]
fn a_command_naming_a_graphic_that_is_not_there_is_refused() {
    let mut f = fixture();
    let before = f.project.clone();
    let ghost = GraphicId::from_raw(9999);

    for command in [
        Box::new(RenameGraphic::new(ghost, "x")) as Box<dyn Command>,
        Box::new(SetGraphicText::new(ghost, "x")),
        Box::new(SetGraphicProperty::new(
            ghost,
            GraphicProperty::Fill,
            PropertyValue::Color(Rgba::WHITE),
        )),
    ] {
        let result = f.history.execute(&mut f.project, command);
        assert!(matches!(result, Err(CommandError::GraphicNotFound(_))), "{result:?}");
    }
    assert_eq!(f.project, before);
    assert!(!f.history.can_undo(), "nothing refused reached the history");
}

// ---- properties ---------------------------------------------------------

#[test]
fn setting_a_property_undoes_to_what_it_was() {
    let mut f = fixture();
    let id = f.add_shape();
    let before = f.project.clone();

    f.history
        .execute(
            &mut f.project,
            Box::new(SetGraphicProperty::new(
                id,
                GraphicProperty::Size,
                PropertyValue::Point(Vec2::new(800.0, 200.0)),
            )),
        )
        .unwrap();
    assert_eq!(f.shape_size(id), Vec2::new(800.0, 200.0));

    f.history.undo(&mut f.project).unwrap();
    assert_eq!(f.project, before);
}

/// A slider drag restates itself on every pointer move, so it has to collapse.
#[test]
fn dragging_a_slider_is_one_undo_step() {
    let mut f = fixture();
    let id = f.add_shape();

    for width in [410.0, 450.0, 512.0] {
        f.history
            .execute_coalesced(
                &mut f.project,
                Box::new(SetGraphicProperty::new(
                    id,
                    GraphicProperty::Size,
                    PropertyValue::Point(Vec2::new(width, 300.0)),
                )),
            )
            .unwrap();
    }
    f.history.break_merge();
    assert_eq!(f.shape_size(id), Vec2::new(512.0, 300.0));

    f.history.undo(&mut f.project).unwrap();
    assert_eq!(f.shape_size(id), Vec2::new(400.0, 300.0), "the whole drag, not a third of it");
}

/// Two different properties are two edits even when dragged one after the
/// other, or undo would rewind a corner radius the user never touched.
#[test]
fn two_different_properties_do_not_merge_into_one_step() {
    let mut f = fixture();
    let id = f.add_shape();

    f.history
        .execute_coalesced(
            &mut f.project,
            Box::new(SetGraphicProperty::new(
                id,
                GraphicProperty::Size,
                PropertyValue::Point(Vec2::new(500.0, 300.0)),
            )),
        )
        .unwrap();
    f.history
        .execute_coalesced(
            &mut f.project,
            Box::new(SetGraphicProperty::new(
                id,
                GraphicProperty::CornerRadius,
                PropertyValue::Scalar(12.0),
            )),
        )
        .unwrap();
    f.history.break_merge();

    f.history.undo(&mut f.project).unwrap();
    assert_eq!(f.shape_size(id), Vec2::new(500.0, 300.0), "the size edit survives");
}

#[test]
fn a_property_that_does_not_apply_to_this_graphic_is_refused() {
    let mut f = fixture();
    let shape = f.add_shape();
    let text = f.add_text();
    let before = f.project.clone();

    // A shape has no font size, and text has no corner radius.
    let bad = [
        (shape, GraphicProperty::FontSize, PropertyValue::Scalar(40.0)),
        (text, GraphicProperty::CornerRadius, PropertyValue::Scalar(4.0)),
    ];
    for (id, target, value) in bad {
        let result = f
            .history
            .execute(&mut f.project, Box::new(SetGraphicProperty::new(id, target, value)));
        assert!(result.is_err(), "{target:?} should not apply here");
    }
    assert_eq!(f.project, before);
}

#[test]
fn a_value_of_the_wrong_type_is_refused_rather_than_coerced() {
    let mut f = fixture();
    let id = f.add_shape();
    let before = f.project.clone();

    // Size is a point; handing it a single number is a programming error, and
    // guessing which axis it meant would be worse than saying so.
    let result = f.history.execute(
        &mut f.project,
        Box::new(SetGraphicProperty::new(
            id,
            GraphicProperty::Size,
            PropertyValue::Scalar(400.0),
        )),
    );
    assert!(result.is_err());
    assert_eq!(f.project, before);
}

#[test]
fn fill_and_stroke_are_shared_between_a_shape_and_a_run_of_text() {
    let mut f = fixture();
    let shape = f.add_shape();
    let text = f.add_text();

    for id in [shape, text] {
        f.history
            .execute(
                &mut f.project,
                Box::new(SetGraphicProperty::new(
                    id,
                    GraphicProperty::Fill,
                    PropertyValue::Color(Rgba::new(1.0, 0.0, 0.0, 1.0)),
                )),
            )
            .expect("fill means the same thing on both");
        f.history
            .execute(
                &mut f.project,
                Box::new(SetGraphicProperty::new(
                    id,
                    GraphicProperty::StrokeWidth,
                    PropertyValue::Scalar(3.0),
                )),
            )
            .unwrap();
    }

    let ref_shape =
        graphic_property_ref(f.project.graphic(shape).unwrap(), GraphicProperty::Fill);
    let ref_text =
        graphic_property_ref(f.project.graphic(text).unwrap(), GraphicProperty::Fill);
    assert_eq!(ref_shape.unwrap().value(), ref_text.unwrap().value());
}

#[test]
fn the_animatable_properties_are_the_ones_this_graphic_actually_has() {
    let mut f = fixture();
    let shape = f.add_shape();
    let text = f.add_text();

    let shape_props = animatable_graphic_properties(f.project.graphic(shape).unwrap());
    assert!(shape_props.contains(&GraphicProperty::Size));
    assert!(!shape_props.contains(&GraphicProperty::FontSize));

    let text_props = animatable_graphic_properties(f.project.graphic(text).unwrap());
    assert!(text_props.contains(&GraphicProperty::FontSize));
    assert!(!text_props.contains(&GraphicProperty::Size));

    // Every one it reports must actually resolve, or the inspector would draw
    // a row for a property no command can reach.
    for (id, props) in [(shape, shape_props), (text, text_props)] {
        let graphic = f.project.graphic(id).unwrap();
        for target in props {
            assert!(
                graphic_property_ref(graphic, target).is_some(),
                "{target:?} is listed but does not resolve"
            );
        }
    }
}

// ---- text and options ---------------------------------------------------

#[test]
fn typing_a_sentence_is_one_undo_step() {
    let mut f = fixture();
    let id = f.add_text();

    for text in ["H", "He", "Hel", "Hell", "Hello ", "Hello t", "Hello there"] {
        f.history
            .execute_coalesced(&mut f.project, Box::new(SetGraphicText::new(id, text)))
            .unwrap();
    }
    f.history.break_merge();
    assert_eq!(f.text_of(id), "Hello there");

    f.history.undo(&mut f.project).unwrap();
    assert_eq!(f.text_of(id), "Hello", "the whole sentence, not the last letter");
}

#[test]
fn a_shape_has_no_words_to_set() {
    let mut f = fixture();
    let id = f.add_shape();
    let before = f.project.clone();

    let result = f.history.execute(&mut f.project, Box::new(SetGraphicText::new(id, "nope")));
    assert!(result.is_err());
    assert_eq!(f.project, before);
}

#[test]
fn changing_the_shape_keeps_everything_else_and_undoes() {
    let mut f = fixture();
    let id = f.add_shape();
    f.history
        .execute(
            &mut f.project,
            Box::new(SetGraphicProperty::new(
                id,
                GraphicProperty::Size,
                PropertyValue::Point(Vec2::new(640.0, 480.0)),
            )),
        )
        .unwrap();
    let before = f.project.clone();

    f.history
        .execute(
            &mut f.project,
            Box::new(SetGraphicOption::new(
                id,
                GraphicOption::Kind(ShapeKind::Star { points: 5 }),
            )),
        )
        .unwrap();

    let shape = f.project.graphic(id).unwrap().as_shape().unwrap();
    assert_eq!(shape.kind, ShapeKind::Star { points: 5 });
    assert_eq!(shape.size.value, Vec2::new(640.0, 480.0), "the size it was given stays");

    f.history.undo(&mut f.project).unwrap();
    assert_eq!(f.project, before);
}

#[test]
fn setting_a_font_and_an_alignment_undoes() {
    let mut f = fixture();
    let id = f.add_text();
    let before = f.project.clone();

    f.history
        .execute(
            &mut f.project,
            Box::new(SetGraphicOption::new(id, GraphicOption::Align(TextAlign::Center))),
        )
        .unwrap();
    f.history
        .execute(
            &mut f.project,
            Box::new(SetGraphicOption::new(
                id,
                GraphicOption::Font(FontSpec::new("Liberation Sans")),
            )),
        )
        .unwrap();

    let text = f.project.graphic(id).unwrap().as_text().unwrap();
    assert_eq!(text.align, TextAlign::Center);
    assert_eq!(text.font.family, "Liberation Sans");

    f.history.undo(&mut f.project).unwrap();
    f.history.undo(&mut f.project).unwrap();
    assert_eq!(f.project, before);
}

#[test]
fn an_option_that_belongs_to_the_other_kind_is_refused() {
    let mut f = fixture();
    let shape = f.add_shape();
    let text = f.add_text();
    let before = f.project.clone();

    let bad: [(GraphicId, GraphicOption); 2] = [
        (shape, GraphicOption::Align(TextAlign::Center)),
        (text, GraphicOption::Kind(ShapeKind::Ellipse)),
    ];
    for (id, option) in bad {
        let result =
            f.history.execute(&mut f.project, Box::new(SetGraphicOption::new(id, option)));
        assert!(result.is_err());
    }
    assert_eq!(f.project, before);
}

// ---- keyframes ----------------------------------------------------------

/// The reason the keyframe machinery was made generic rather than copied: a
/// graphic's properties are the same `Property<T>` a clip's transform uses, so
/// they keyframe through the same code.
#[test]
fn a_graphic_property_keyframes_through_the_shared_machinery() {
    let mut f = fixture();
    let id = f.add_shape();
    let before = f.project.clone();

    let edits = vec![(
        GraphicProperty::Size,
        KeyframeEdit::Set {
            time: Ticks::from_seconds(1),
            value: PropertyValue::Point(Vec2::new(800.0, 600.0)),
            interpolation: ve_core::Interpolation::Linear,
        },
    )];
    f.history.execute(&mut f.project, Box::new(EditGraphicKeyframes::new(id, edits))).unwrap();

    let size = &f.project.graphic(id).unwrap().as_shape().unwrap().size;
    assert!(size.is_animated());
    assert_eq!(size.keyframes().len(), 1);
    assert_eq!(size.evaluate(Ticks::from_seconds(1)), Vec2::new(800.0, 600.0));

    f.history.undo(&mut f.project).unwrap();
    assert_eq!(f.project, before, "undo restores the property whole");
}

/// Retiming restates itself on every pointer move, so a drag is one step.
///
/// Each move names as many times as the property has keyframes — that is what
/// [`KeyframeEdit::SetTimes`] means, and a stale count is refused rather than
/// guessed at, exactly as it is for a clip.
#[test]
fn dragging_a_graphics_keyframes_is_one_undo_step() {
    let mut f = fixture();
    let id = f.add_shape();
    f.keyframe_sizes(id, &[(0, 100.0), (1, 200.0), (2, 300.0)]);
    let before_drag = f.project.clone();
    let depth = f.history.undo_depth();

    // One keyframe slid right, restated against its neighbours on every move.
    for last in [2_200i64, 2_500, 2_800] {
        f.history
            .execute_coalesced(
                &mut f.project,
                Box::new(EditGraphicKeyframes::new(
                    id,
                    vec![(
                        GraphicProperty::Size,
                        KeyframeEdit::SetTimes(vec![
                            Ticks::ZERO,
                            Ticks::from_seconds(1),
                            Ticks::from_millis(last),
                        ]),
                    )],
                )),
            )
            .unwrap();
    }
    f.history.break_merge();

    assert_eq!(f.history.undo_depth(), depth + 1, "a drag is one entry, not three");
    assert_eq!(f.size_times(id).last(), Some(&Ticks::from_millis(2_800)));

    f.history.undo(&mut f.project).unwrap();
    assert_eq!(f.project, before_drag, "undo takes the whole gesture back");
}

/// Undo restores the keyframe list wholesale rather than replaying an inverse,
/// which is the only way a retime that collapsed two keyframes onto one tick
/// can be undone at all — no inverse brings the lost one back.
#[test]
fn a_retime_that_collapses_keyframes_still_undoes_exactly() {
    let mut f = fixture();
    let id = f.add_shape();
    f.keyframe_sizes(id, &[(0, 100.0), (1, 200.0), (2, 300.0)]);
    let three_keys = f.project.clone();

    f.history
        .execute(
            &mut f.project,
            Box::new(EditGraphicKeyframes::new(
                id,
                vec![(
                    GraphicProperty::Size,
                    KeyframeEdit::SetTimes(vec![
                        Ticks::ZERO,
                        Ticks::from_seconds(1),
                        Ticks::from_seconds(1),
                    ]),
                )],
            )),
        )
        .unwrap();
    assert_eq!(f.size_times(id).len(), 2, "two keyframes on one tick become one");

    f.history.undo(&mut f.project).unwrap();
    assert_eq!(f.project, three_keys, "the lost keyframe comes back");
}

/// A stale count is a caller that has lost track of what it is dragging, and
/// guessing which keyframe it meant would be worse than saying so.
#[test]
fn a_retime_naming_the_wrong_number_of_times_is_refused() {
    let mut f = fixture();
    let id = f.add_shape();
    f.keyframe_sizes(id, &[(0, 100.0), (1, 200.0), (2, 300.0)]);
    let before = f.project.clone();

    let result = f.history.execute(
        &mut f.project,
        Box::new(EditGraphicKeyframes::new(
            id,
            vec![(GraphicProperty::Size, KeyframeEdit::SetTimes(vec![Ticks::ZERO]))],
        )),
    );
    assert!(matches!(result, Err(CommandError::Rejected(_))), "{result:?}");
    assert_eq!(f.project, before);
}

// ---- one history ---------------------------------------------------------

/// The whole reason a graphic is edited through ordinary commands.
#[test]
fn graphic_edits_and_timeline_edits_share_one_history() {
    let mut f = fixture();
    let id = f.add_text();
    let clip = f.place(id, Ticks::ZERO);

    f.history
        .execute(&mut f.project, Box::new(SetGraphicText::new(id, "Lower third")))
        .unwrap();
    f.history
        .execute(
            &mut f.project,
            Box::new(SplitClip::new(f.sequence, f.track, clip, Ticks::from_seconds(2))),
        )
        .unwrap();

    assert_eq!(f.clip_count(), 2);
    assert_eq!(f.text_of(id), "Lower third");

    // One stack, unwound in order: the split, then the words, then the clip,
    // then the graphic.
    f.history.undo(&mut f.project).unwrap();
    assert_eq!(f.clip_count(), 1);
    f.history.undo(&mut f.project).unwrap();
    assert_eq!(f.text_of(id), "Hello");
    f.history.undo(&mut f.project).unwrap();
    assert_eq!(f.clip_count(), 0);
    f.history.undo(&mut f.project).unwrap();
    assert!(f.project.graphic(id).is_none());
    assert!(!f.history.can_undo());
}

#[test]
fn a_placed_graphic_draws_it_and_gets_a_sensible_length() {
    let mut f = fixture();
    let id = f.add_shape();
    let clip_id = f.place(id, Ticks::from_seconds(3));

    let (_, clip) = f.project.sequence(f.sequence).unwrap().find_clip(clip_id).unwrap();
    assert_eq!(clip.source, Source::Graphic(id));
    assert_eq!(clip.timeline_start, Ticks::from_seconds(3));
    assert_eq!(
        clip.duration,
        Ticks::from_seconds(Graphic::DEFAULT_DURATION_SECONDS),
        "a rectangle is as long as you want it to be, so it gets a still's default"
    );
    assert_eq!(clip.name, "Rectangle", "named after what it draws");
}

#[test]
fn placing_a_graphic_that_is_not_there_is_refused() {
    let mut f = fixture();
    let result = graphic_clip(&mut f.project, GraphicId::from_raw(4242), Ticks::ZERO);
    assert!(matches!(result, Err(CommandError::GraphicNotFound(_))), "{result:?}");
}
