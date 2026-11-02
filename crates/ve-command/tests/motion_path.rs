//! Motion paths through the command layer: the shape is one command, the
//! timing is an ordinary property, and neither needs the other to undo.

use ve_command::*;
use ve_core::{
    Clip, ClipId, Interpolation, MediaInfo, MotionPath, Project, SequenceId, Size, Vec2,
    VideoStreamInfo,
};
use ve_time::{Rate, Ticks};

fn fixture() -> (Project, SequenceId, ClipId) {
    let mut p = Project::with_default_sequence("Path");
    let asset = p.add_asset(
        "/media/a.mp4",
        MediaInfo {
            duration: Ticks::from_seconds(60),
            video: Some(VideoStreamInfo {
                size: Size::new(1920, 1080),
                rate: Rate::FPS_30,
                duration: Ticks::from_seconds(60),
                frame_count: Some(1800),
                codec: "h264".into(),
                pixel_format: "yuv420p".into(),
                sample_aspect_ratio: (1, 1),
            }),
            audio: None,
            container: "mp4".into(),
            timecode: None,
        },
    );
    let seq = p.active_sequence.unwrap();
    let track = p.sequence(seq).unwrap().tracks[0].id;
    let id = p.new_clip_id();
    let clip = Clip::new(id, asset, "c", Ticks::ZERO, Ticks::ZERO, Ticks::from_seconds(10));
    let mut h = History::default();
    h.execute(&mut p, Box::new(AddClip::new(seq, track, clip))).unwrap();
    (p, seq, id)
}

fn transform_at(p: &Project, seq: SequenceId, id: ClipId, t: Ticks) -> ve_core::TransformState {
    p.sequence(seq).unwrap().find_clip(id).unwrap().1.transform.evaluate(t)
}

#[test]
fn a_clip_follows_its_path_as_progress_advances() {
    let (mut p, seq, id) = fixture();
    let mut h = History::default();
    let path = MotionPath::line(Vec2::new(-100.0, 0.0), Vec2::new(100.0, 0.0));
    h.execute(&mut p, Box::new(SetMotionPath::new(seq, id, Some(path)))).unwrap();

    // Progress 0 → 1 over the ten seconds, linear.
    for (time, value) in [(Ticks::ZERO, 0.0), (Ticks::from_seconds(10), 1.0)] {
        h.execute(
            &mut p,
            Box::new(EditKeyframes::one(
                seq,
                id,
                ClipProperty::PathProgress,
                KeyframeEdit::Set {
                    time,
                    value: PropertyValue::Scalar(value),
                    interpolation: Interpolation::Linear,
                },
            )),
        )
        .unwrap();
    }

    assert_eq!(transform_at(&p, seq, id, Ticks::ZERO).position, Vec2::new(-100.0, 0.0));
    let mid = transform_at(&p, seq, id, Ticks::from_seconds(5)).position;
    assert!(mid.x.abs() < 1e-6 && mid.y.abs() < 1e-6, "{mid:?}");
    assert_eq!(
        transform_at(&p, seq, id, Ticks::from_seconds(10)).position,
        Vec2::new(100.0, 0.0)
    );
}

#[test]
fn the_path_is_added_to_the_position_property() {
    let (mut p, seq, id) = fixture();
    let mut h = History::default();
    h.execute(
        &mut p,
        Box::new(SetClipProperty::new(
            seq,
            id,
            ClipProperty::Position,
            PropertyValue::Point(Vec2::new(0.0, 50.0)),
        )),
    )
    .unwrap();
    h.execute(
        &mut p,
        Box::new(SetMotionPath::new(
            seq,
            id,
            Some(MotionPath::line(Vec2::new(10.0, 0.0), Vec2::new(20.0, 0.0))),
        )),
    )
    .unwrap();
    assert_eq!(transform_at(&p, seq, id, Ticks::ZERO).position, Vec2::new(10.0, 50.0));
}

#[test]
fn dragging_a_point_is_one_undo_step_and_undo_restores_exactly() {
    let (mut p, seq, id) = fixture();
    let mut h = History::default();
    let before = p.clone();
    h.execute(
        &mut p,
        Box::new(SetMotionPath::new(
            seq,
            id,
            Some(MotionPath::line(Vec2::ZERO, Vec2::new(100.0, 0.0))),
        )),
    )
    .unwrap();
    let added = p.clone();
    h.break_merge();

    for x in [110.0, 130.0, 160.0] {
        h.execute_coalesced(
            &mut p,
            Box::new(SetMotionPath::new(
                seq,
                id,
                Some(MotionPath::line(Vec2::ZERO, Vec2::new(x, 0.0))),
            )),
        )
        .unwrap();
    }
    h.break_merge();

    h.undo(&mut p).unwrap();
    assert_eq!(p, added, "the whole drag was one step");
    h.undo(&mut p).unwrap();
    assert_eq!(p, before);
    h.redo(&mut p).unwrap();
    assert_eq!(p, added);
}

#[test]
fn progress_is_only_offered_while_there_is_a_path() {
    let (mut p, seq, id) = fixture();
    let mut h = History::default();
    let has = |p: &Project| {
        let clip = p.sequence(seq).unwrap().find_clip(id).unwrap().1;
        animatable_properties(clip).contains(&ClipProperty::PathProgress)
    };
    assert!(!has(&p));
    h.execute(
        &mut p,
        Box::new(SetMotionPath::new(seq, id, Some(MotionPath::line(Vec2::ZERO, Vec2::ONE)))),
    )
    .unwrap();
    assert!(has(&p));
    h.execute(&mut p, Box::new(SetMotionPath::new(seq, id, None))).unwrap();
    assert!(!has(&p));
}

#[test]
fn a_clip_that_is_not_there_is_refused() {
    let (mut p, seq, _) = fixture();
    let ghost = p.new_clip_id();
    let mut h = History::default();
    let before = p.clone();
    assert!(h.execute(&mut p, Box::new(SetMotionPath::new(seq, ghost, None))).is_err());
    assert_eq!(p, before);
}
