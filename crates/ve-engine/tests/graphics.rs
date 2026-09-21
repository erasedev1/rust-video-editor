//! What the plan says about a clip that draws a graphic.
//!
//! The interesting question is *when*: a graphic has a timeline of its own and
//! the clip is a window onto it, so trimming, slipping and speed change which
//! instant of the graphic is drawn in exactly the way they do for footage.

use ve_core::{
    Clip, Graphic, GraphicContent, Interpolation, Project, Rgba, SequenceId, Shape, ShapeKind,
    Source, TrackId, TrackKind, Vec2,
};
use ve_engine::{evaluate_project, Draw};
use ve_time::Ticks;

/// A rectangle that widens from 100 to 300 pixels over its first four seconds.
fn widening_shape() -> GraphicContent {
    let mut shape = Shape::new(ShapeKind::Rectangle, Vec2::new(100.0, 50.0), Rgba::WHITE);
    shape.size.set_keyframe(Ticks::ZERO, Vec2::new(100.0, 50.0), Interpolation::Linear);
    shape.size.set_keyframe(
        Ticks::from_seconds(4),
        Vec2::new(300.0, 50.0),
        Interpolation::Linear,
    );
    GraphicContent::Shape(shape)
}

struct Fixture {
    project: Project,
    sequence: SequenceId,
    v1: TrackId,
}

fn fixture() -> Fixture {
    let project = Project::with_default_sequence("Graphics");
    let sequence = project.active_sequence.expect("a default sequence");
    let v1 = project.sequence(sequence).expect("a sequence").tracks[0].id;
    Fixture { project, sequence, v1 }
}

impl Fixture {
    /// Places a clip on V1 drawing `source`, reading from `source_in`.
    fn place(&mut self, source: Source, source_in: Ticks, duration: Ticks) {
        let id = self.project.new_clip_id();
        self.project
            .sequence_mut(self.sequence)
            .expect("a sequence")
            .track_mut(self.v1)
            .expect("a track")
            .insert_clip(Clip::new(id, source, "graphic", source_in, Ticks::ZERO, duration))
            .expect("an empty track");
    }

    /// The width of the rectangle the plan draws at `at`, if it draws one.
    fn width_at(&self, at: Ticks) -> Option<f64> {
        let sequence = self.project.sequence(self.sequence).expect("a sequence");
        let plan = evaluate_project(&self.project, sequence, at);
        let item = plan.root().items.first()?;
        let state = item.graphic()?;
        Some(state.as_shape().expect("a shape").size.x)
    }
}

#[test]
fn a_clip_on_a_graphic_plans_a_drawn_item_rather_than_a_decode() {
    let mut f = fixture();
    let id = f.project.add_graphic("Box", widening_shape());
    f.place(Source::Graphic(id), Ticks::ZERO, Ticks::from_seconds(10));

    let sequence = f.project.sequence(f.sequence).expect("a sequence");
    let plan = evaluate_project(&f.project, sequence, Ticks::from_seconds(1));
    let item = plan.root().items.first().expect("something on screen");
    match &item.draw {
        Draw::Graphic { graphic, .. } => assert_eq!(*graphic, id),
        other => panic!("expected a graphic, got {other:?}"),
    }
    assert_eq!(item.asset(), None, "there is no file to decode");
}

#[test]
fn a_graphic_is_evaluated_at_its_own_time_not_the_playhead() {
    let mut f = fixture();
    let id = f.project.add_graphic("Box", widening_shape());
    // The clip starts at the top of the timeline and reads from the top of the
    // graphic, so the two agree and the animation runs as authored.
    f.place(Source::Graphic(id), Ticks::ZERO, Ticks::from_seconds(10));

    assert_eq!(f.width_at(Ticks::ZERO), Some(100.0));
    assert_eq!(f.width_at(Ticks::from_seconds(2)), Some(200.0));
    assert_eq!(f.width_at(Ticks::from_seconds(4)), Some(300.0));
    // Past the last keyframe the value is held, as every property is.
    assert_eq!(f.width_at(Ticks::from_seconds(8)), Some(300.0));
}

#[test]
fn trimming_into_a_build_trims_the_build() {
    // A clip that starts two seconds into the graphic shows the graphic two
    // seconds in — the same thing trimming the head of a shot does to footage,
    // which is the whole reason a graphic is a source rather than a clip.
    let mut f = fixture();
    let id = f.project.add_graphic("Box", widening_shape());
    f.place(Source::Graphic(id), Ticks::from_seconds(2), Ticks::from_seconds(10));

    assert_eq!(f.width_at(Ticks::ZERO), Some(200.0));
    assert_eq!(f.width_at(Ticks::from_seconds(1)), Some(250.0));
}

#[test]
fn speed_carries_a_graphics_animation_with_it() {
    let mut f = fixture();
    let id = f.project.add_graphic("Box", widening_shape());
    f.place(Source::Graphic(id), Ticks::ZERO, Ticks::from_seconds(10));
    {
        let clip = f.project.sequence_mut(f.sequence).expect("a sequence").tracks[0]
            .clips_mut()
            .next()
            .expect("the clip");
        clip.speed = ve_core::Speed::new(2, 1).expect("a valid speed");
    }

    // Two seconds of timeline is four seconds of graphic at double speed, and
    // four seconds is the end of the build.
    assert_eq!(f.width_at(Ticks::from_seconds(2)), Some(300.0));
    assert_eq!(f.width_at(Ticks::from_seconds(1)), Some(200.0));
}

#[test]
fn two_clips_drawing_one_graphic_each_run_it_from_their_own_start() {
    let mut f = fixture();
    let id = f.project.add_graphic("Box", widening_shape());
    f.place(Source::Graphic(id), Ticks::ZERO, Ticks::from_seconds(4));

    // A second clip further down the timeline, reading the graphic from zero.
    let second = f.project.new_clip_id();
    f.project
        .sequence_mut(f.sequence)
        .expect("a sequence")
        .track_mut(f.v1)
        .expect("a track")
        .insert_clip(Clip::new(
            second,
            Source::Graphic(id),
            "again",
            Ticks::ZERO,
            Ticks::from_seconds(10),
            Ticks::from_seconds(4),
        ))
        .expect("room on the track");

    assert_eq!(f.width_at(Ticks::from_seconds(2)), Some(200.0));
    assert_eq!(f.width_at(Ticks::from_seconds(12)), Some(200.0), "the second runs too");
}

#[test]
fn a_graphic_the_project_has_lost_draws_nothing() {
    let mut f = fixture();
    let id = f.project.add_graphic("Box", widening_shape());
    f.place(Source::Graphic(id), Ticks::ZERO, Ticks::from_seconds(10));
    f.project.graphics.clear();

    let sequence = f.project.sequence(f.sequence).expect("a sequence");
    let plan = evaluate_project(&f.project, sequence, Ticks::from_seconds(1));
    assert!(plan.root().items.is_empty(), "a hole, not a panic");
}

#[test]
fn a_graphic_on_an_audio_track_is_silent_rather_than_a_problem() {
    let mut f = fixture();
    let id = f.project.add_graphic("Box", widening_shape());
    let audio_track = f
        .project
        .sequence(f.sequence)
        .expect("a sequence")
        .tracks
        .iter()
        .find(|t| t.kind == TrackKind::Audio)
        .expect("a default audio track")
        .id;
    let clip = f.project.new_clip_id();
    f.project
        .sequence_mut(f.sequence)
        .expect("a sequence")
        .track_mut(audio_track)
        .expect("a track")
        .insert_clip(Clip::new(
            clip,
            Source::Graphic(id),
            "silent",
            Ticks::ZERO,
            Ticks::ZERO,
            Ticks::from_seconds(5),
        ))
        .expect("an empty track");

    let sequence = f.project.sequence(f.sequence).expect("a sequence");
    let plan = evaluate_project(&f.project, sequence, Ticks::from_seconds(1));
    assert!(plan.audio.is_empty());
}

#[test]
fn a_graphic_is_not_deleted_while_a_clip_still_draws_it() {
    let mut f = fixture();
    let id = f.project.add_graphic("Box", widening_shape());
    f.place(Source::Graphic(id), Ticks::ZERO, Ticks::from_seconds(10));

    assert!(f.project.remove_graphic(id).is_err());
    assert_eq!(f.project.uses_of(Source::Graphic(id)), 1);
}

#[test]
fn a_duplicate_goes_its_own_way() {
    let mut f = fixture();
    let id = f.project.add_graphic("Box", widening_shape());
    let copy = f.project.duplicate_graphic(id).expect("a copy");
    assert_ne!(copy, id);

    let original = f.project.graphic(id).expect("the original").clone();
    if let Some(shape) = f.project.graphic_mut(copy).and_then(Graphic::as_shape_mut) {
        shape.fill = ve_core::Property::constant(Rgba::BLACK);
    }
    assert_eq!(f.project.graphic(id), Some(&original), "editing the copy left it alone");
}
