//! The central guarantee: after undoing an edit the project must be *exactly*
//! what it was, and redoing must reproduce the edit exactly. `Project` derives
//! `PartialEq`, so most of these tests assert on whole-project equality rather
//! than spot-checking fields.

use ve_command::*;
use ve_core::{
    Clip, ClipId, Interpolation, Marker, MediaInfo, Project, SequenceId, Size, TrackId, TrackKind,
    Vec2, VideoStreamInfo,
};
use ve_time::{Rate, Ticks};

fn fixture() -> (Project, SequenceId, TrackId, ve_core::AssetId) {
    let mut p = Project::with_default_sequence("Undo Test");
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
        },
    );
    let seq = p.active_sequence.unwrap();
    let track = p.sequence(seq).unwrap().tracks[0].id;
    (p, seq, track, asset)
}

fn clip(p: &mut Project, asset: ve_core::AssetId, start_s: i64, dur_s: i64) -> Clip {
    let id = p.new_clip_id();
    Clip::new(
        id,
        asset,
        format!("clip@{start_s}"),
        Ticks::ZERO,
        Ticks::from_seconds(start_s),
        Ticks::from_seconds(dur_s),
    )
}

#[test]
fn adding_a_clip_undoes_and_redoes_exactly() {
    let (mut p, seq, track, asset) = fixture();
    let before = p.clone();
    let mut h = History::default();

    let c = clip(&mut p, asset, 0, 10);
    let expected_id = c.id;
    // Cloning the project after the ID was minted, so the allocator state
    // matches what undo will leave behind.
    let before_with_id = p.clone();

    h.execute(&mut p, Box::new(AddClip::new(seq, track, c))).unwrap();
    let after_add = p.clone();
    assert_eq!(p.clip_count(), 1);

    h.undo(&mut p).unwrap();
    assert_eq!(p, before_with_id, "undo did not restore the project exactly");
    assert_eq!(p.clip_count(), 0);

    h.redo(&mut p).unwrap();
    assert_eq!(p, after_add, "redo did not reproduce the edit exactly");
    assert_eq!(p.active().unwrap().tracks[0].clips()[0].id, expected_id);

    assert_ne!(before, after_add);
}

#[test]
fn deleting_a_clip_restores_every_field_including_animation() {
    let (mut p, seq, track, asset) = fixture();
    let mut c = clip(&mut p, asset, 5, 10);
    c.transform.opacity.set_keyframe(Ticks::ZERO, 0.0, Interpolation::EaseIn);
    c.transform.position.set_keyframe(Ticks::from_seconds(1), Vec2::new(4.0, 2.0), Interpolation::Linear);
    let clip_id = c.id;
    p.sequence_mut(seq).unwrap().track_mut(track).unwrap().insert_clip(c).unwrap();

    let before = p.clone();
    let mut h = History::default();
    h.execute(&mut p, Box::new(RemoveClip::new(seq, track, clip_id))).unwrap();
    assert_eq!(p.clip_count(), 0);

    h.undo(&mut p).unwrap();
    assert_eq!(p, before, "a deleted clip must come back byte-identical");
}

#[test]
fn a_move_returns_to_where_it_started() {
    let (mut p, seq, track, asset) = fixture();
    let c = clip(&mut p, asset, 0, 10);
    let id = c.id;
    let mut h = History::default();
    h.execute(&mut p, Box::new(AddClip::new(seq, track, c))).unwrap();
    let before = p.clone();

    h.execute(&mut p, Box::new(MoveClip::new(seq, track, id, Ticks::from_seconds(30)))).unwrap();
    assert_eq!(
        p.active().unwrap().tracks[0].clips()[0].timeline_start,
        Ticks::from_seconds(30)
    );

    h.undo(&mut p).unwrap();
    assert_eq!(p, before);
}

#[test]
fn a_drag_collapses_into_one_undo_step_that_returns_to_the_origin() {
    let (mut p, seq, track, asset) = fixture();
    let c = clip(&mut p, asset, 0, 10);
    let id = c.id;
    let mut h = History::default();
    h.execute(&mut p, Box::new(AddClip::new(seq, track, c))).unwrap();
    let before_drag = p.clone();
    let depth = h.undo_depth();

    // Simulate a drag: many small moves as the mouse travels.
    for s in 1..=40 {
        h.execute_coalesced(
            &mut p,
            Box::new(MoveClip::new(seq, track, id, Ticks::from_seconds(s))),
        )
        .unwrap();
    }
    assert_eq!(h.undo_depth(), depth + 1, "40 mouse moves should be one undo entry");
    assert_eq!(
        p.active().unwrap().tracks[0].clips()[0].timeline_start,
        Ticks::from_seconds(40)
    );

    // One undo returns to the start of the gesture, not to the previous frame.
    h.undo(&mut p).unwrap();
    assert_eq!(p, before_drag);
}

#[test]
fn a_gesture_boundary_starts_a_new_undo_entry() {
    let (mut p, seq, track, asset) = fixture();
    let c = clip(&mut p, asset, 0, 10);
    let id = c.id;
    let mut h = History::default();
    h.execute(&mut p, Box::new(AddClip::new(seq, track, c))).unwrap();

    h.execute_coalesced(&mut p, Box::new(MoveClip::new(seq, track, id, Ticks::from_seconds(20))))
        .unwrap();
    let after_first_drag = p.clone();
    h.break_merge(); // mouse up

    h.execute_coalesced(&mut p, Box::new(MoveClip::new(seq, track, id, Ticks::from_seconds(40))))
        .unwrap();

    h.undo(&mut p).unwrap();
    assert_eq!(p, after_first_drag, "the second drag must be its own undo step");
}

#[test]
fn moves_of_different_clips_never_merge() {
    let (mut p, seq, track, asset) = fixture();
    let a = clip(&mut p, asset, 0, 5);
    let b = clip(&mut p, asset, 10, 5);
    let (a_id, b_id) = (a.id, b.id);
    let mut h = History::default();
    h.execute(&mut p, Box::new(AddClip::new(seq, track, a))).unwrap();
    h.execute(&mut p, Box::new(AddClip::new(seq, track, b))).unwrap();
    let depth = h.undo_depth();

    h.execute_coalesced(&mut p, Box::new(MoveClip::new(seq, track, a_id, Ticks::from_seconds(30))))
        .unwrap();
    h.execute_coalesced(&mut p, Box::new(MoveClip::new(seq, track, b_id, Ticks::from_seconds(50))))
        .unwrap();
    assert_eq!(h.undo_depth(), depth + 2, "different clips must not share an entry");
}

#[test]
fn trimming_restores_the_source_window_as_well_as_the_timeline_position() {
    let (mut p, seq, track, asset) = fixture();
    let id = p.new_clip_id();
    let c = Clip::new(
        id,
        asset,
        "c",
        Ticks::from_seconds(10),
        Ticks::ZERO,
        Ticks::from_seconds(20),
    );
    let mut h = History::default();
    h.execute(&mut p, Box::new(AddClip::new(seq, track, c))).unwrap();
    let before = p.clone();

    h.execute(
        &mut p,
        Box::new(TrimClip::new(seq, track, id, TrimEdge::Start, Ticks::from_seconds(6))),
    )
    .unwrap();
    let trimmed = &p.active().unwrap().tracks[0].clips()[0];
    assert_eq!(trimmed.source_in, Ticks::from_seconds(16));
    assert_eq!(trimmed.duration, Ticks::from_seconds(14));

    h.undo(&mut p).unwrap();
    assert_eq!(p, before, "the source in-point must be restored, not just the position");
}

#[test]
fn trimming_the_tail_undoes_exactly() {
    let (mut p, seq, track, asset) = fixture();
    let c = clip(&mut p, asset, 0, 20);
    let id = c.id;
    let mut h = History::default();
    h.execute(&mut p, Box::new(AddClip::new(seq, track, c))).unwrap();
    let before = p.clone();

    h.execute(
        &mut p,
        Box::new(TrimClip::new(seq, track, id, TrimEdge::End, Ticks::from_seconds(8))),
    )
    .unwrap();
    assert_eq!(p.active().unwrap().tracks[0].clips()[0].duration, Ticks::from_seconds(8));

    h.undo(&mut p).unwrap();
    assert_eq!(p, before);
}

#[test]
fn a_trim_drag_collapses_and_undoes_to_the_original_window() {
    let (mut p, seq, track, asset) = fixture();
    let c = clip(&mut p, asset, 0, 20);
    let id = c.id;
    let mut h = History::default();
    h.execute(&mut p, Box::new(AddClip::new(seq, track, c))).unwrap();
    let before = p.clone();
    let depth = h.undo_depth();

    for s in (5..=15).rev() {
        h.execute_coalesced(
            &mut p,
            Box::new(TrimClip::new(seq, track, id, TrimEdge::End, Ticks::from_seconds(s))),
        )
        .unwrap();
    }
    assert_eq!(h.undo_depth(), depth + 1);
    h.undo(&mut p).unwrap();
    assert_eq!(p, before);
}

#[test]
fn trims_of_opposite_edges_do_not_merge() {
    let (mut p, seq, track, asset) = fixture();
    let c = clip(&mut p, asset, 0, 20);
    let id = c.id;
    let mut h = History::default();
    h.execute(&mut p, Box::new(AddClip::new(seq, track, c))).unwrap();
    let depth = h.undo_depth();

    h.execute_coalesced(
        &mut p,
        Box::new(TrimClip::new(seq, track, id, TrimEdge::End, Ticks::from_seconds(15))),
    )
    .unwrap();
    h.execute_coalesced(
        &mut p,
        Box::new(TrimClip::new(seq, track, id, TrimEdge::Start, Ticks::from_seconds(2))),
    )
    .unwrap();
    assert_eq!(h.undo_depth(), depth + 2);
}

#[test]
fn splitting_undoes_and_redoes_with_stable_ids() {
    let (mut p, seq, track, asset) = fixture();
    let id = p.new_clip_id();
    let c = Clip::new(
        id,
        asset,
        "c",
        Ticks::from_seconds(10),
        Ticks::ZERO,
        Ticks::from_seconds(20),
    );
    let mut h = History::default();
    h.execute(&mut p, Box::new(AddClip::new(seq, track, c))).unwrap();
    let before = p.clone();

    let mut split = SplitClip::new(seq, track, id, Ticks::from_seconds(8));
    h.execute(&mut p, Box::new(std::mem::replace(
        &mut split,
        SplitClip::new(seq, track, id, Ticks::from_seconds(8)),
    )))
    .unwrap();
    assert_eq!(p.clip_count(), 2);
    let after_split = p.clone();
    let right_id_first = p.active().unwrap().tracks[0].clips()[1].id;

    h.undo(&mut p).unwrap();
    assert_eq!(p.clip_count(), 1);
    assert_eq!(p.active().unwrap().tracks[0].clips()[0].duration, Ticks::from_seconds(20));

    h.redo(&mut p).unwrap();
    assert_eq!(p, after_split, "a redone split must reproduce the same project");
    let right_id_second = p.active().unwrap().tracks[0].clips()[1].id;
    assert_eq!(
        right_id_first, right_id_second,
        "redo must reuse the original ID so selections stay valid"
    );

    // And undoing back out returns to the pre-split state.
    h.undo(&mut p).unwrap();
    assert_eq!(p.active().unwrap().tracks[0].clips()[0].duration, before.active().unwrap().tracks[0].clips()[0].duration);
}

#[test]
fn a_refused_command_leaves_the_project_untouched_and_is_not_pushed() {
    let (mut p, seq, track, asset) = fixture();
    let a = clip(&mut p, asset, 0, 10);
    let mut h = History::default();
    h.execute(&mut p, Box::new(AddClip::new(seq, track, a))).unwrap();
    let before = p.clone();
    let depth = h.undo_depth();

    // An overlapping insert must be rejected.
    let b = clip(&mut p, asset, 5, 10);
    let p_with_id = p.clone(); // the ID was minted, so account for it
    let err = h.execute(&mut p, Box::new(AddClip::new(seq, track, b)));
    assert!(err.is_err(), "overlapping insert should have been refused");
    assert_eq!(p, p_with_id, "a failed command must not modify the project");
    assert_eq!(h.undo_depth(), depth, "a failed command must not enter the history");

    // The history is still coherent: undo reverts the *successful* add.
    h.undo(&mut p).unwrap();
    assert_eq!(p.clip_count(), 0);
    assert_ne!(before, p);
}

#[test]
fn a_new_edit_discards_the_redo_branch() {
    let (mut p, seq, track, asset) = fixture();
    let a = clip(&mut p, asset, 0, 5);
    let mut h = History::default();
    h.execute(&mut p, Box::new(AddClip::new(seq, track, a))).unwrap();
    h.undo(&mut p).unwrap();
    assert!(h.can_redo());

    let b = clip(&mut p, asset, 20, 5);
    h.execute(&mut p, Box::new(AddClip::new(seq, track, b))).unwrap();
    assert!(!h.can_redo(), "a new edit must invalidate the redo branch");
}

#[test]
fn undo_and_redo_labels_track_the_stacks() {
    let (mut p, seq, track, asset) = fixture();
    let mut h = History::default();
    assert!(!h.can_undo());
    assert_eq!(h.undo_name(), None);
    assert!(matches!(h.undo(&mut p), Err(CommandError::NothingToUndo)));
    assert!(matches!(h.redo(&mut p), Err(CommandError::NothingToRedo)));

    let c = clip(&mut p, asset, 0, 5);
    let id = c.id;
    h.execute(&mut p, Box::new(AddClip::new(seq, track, c))).unwrap();
    assert_eq!(h.undo_name(), Some("Add Clip"));

    h.execute(&mut p, Box::new(MoveClip::new(seq, track, id, Ticks::from_seconds(10)))).unwrap();
    assert_eq!(h.undo_name(), Some("Move Clip"));

    assert_eq!(h.undo(&mut p).unwrap(), "Move Clip");
    assert_eq!(h.redo_name(), Some("Move Clip"));
    assert_eq!(h.undo_name(), Some("Add Clip"));
}

#[test]
fn a_long_edit_session_undoes_all_the_way_back_to_the_start() {
    let (mut p, seq, track, asset) = fixture();
    let start = p.clone();
    let mut h = History::default();

    // Build up a timeline with a mixture of every command type, capturing the
    // project after each step so we can walk back through them.
    let mut snapshots = vec![p.clone()];
    for i in 0..12i64 {
        let c = clip(&mut p, asset, i * 10, 6);
        let id = c.id;
        h.execute(&mut p, Box::new(AddClip::new(seq, track, c))).unwrap();
        snapshots.push(p.clone());

        h.execute(
            &mut p,
            Box::new(TrimClip::new(
                seq,
                track,
                id,
                TrimEdge::End,
                Ticks::from_seconds(i * 10 + 4),
            )),
        )
        .unwrap();
        snapshots.push(p.clone());

        h.execute(&mut p, Box::new(SplitClip::new(seq, track, id, Ticks::from_seconds(i * 10 + 2))))
            .unwrap();
        snapshots.push(p.clone());
    }
    assert_eq!(p.clip_count(), 24);

    // Undo everything, checking each intermediate state on the way.
    //
    // The ID allocator is normalised out of the comparison because it is the
    // one thing undo deliberately does *not* rewind: a command that minted an
    // ID keeps it, so that redo reproduces the same IDs and so that a new edit
    // after an undo can never collide with an object the redo branch still
    // refers to. `ids_are_never_reused_after_an_undo` pins that down directly.
    let edits_equal = |a: &Project, b: &Project| {
        let (mut a, mut b) = (a.clone(), b.clone());
        let high = a.ids.peek().max(b.ids.peek());
        a.ids.bump_past(high);
        b.ids.bump_past(high);
        a == b
    };

    snapshots.pop();
    while let Some(expected) = snapshots.pop() {
        h.undo(&mut p).unwrap();
        assert!(
            edits_equal(&p, &expected),
            "mismatch at undo depth {}:\n  got      {:#?}\n  expected {:#?}",
            h.undo_depth(),
            p.active().unwrap().tracks[0].clips().iter().map(|c| (c.id, c.timeline_start, c.duration)).collect::<Vec<_>>(),
            expected.active().unwrap().tracks[0].clips().iter().map(|c| (c.id, c.timeline_start, c.duration)).collect::<Vec<_>>(),
        );
    }
    assert!(!h.can_undo());
    assert_eq!(p.clip_count(), 0);

    // The allocator has moved on, which is deliberate: IDs are never reused.
    assert!(p.ids.peek() > start.ids.peek());

    // Redo everything back to the full timeline.
    while h.can_redo() {
        h.redo(&mut p).unwrap();
    }
    assert_eq!(p.clip_count(), 24);
}

#[test]
fn ids_are_never_reused_after_an_undo() {
    // Undoing frees an object but not its ID. If it did, a new edit made after
    // an undo could mint the same ID as a clip still sitting in the redo
    // branch, and redoing would then bind caches and selections to the wrong
    // object.
    let (mut p, seq, track, asset) = fixture();
    let mut h = History::default();

    let first = clip(&mut p, asset, 0, 5);
    let first_id = first.id;
    h.execute(&mut p, Box::new(AddClip::new(seq, track, first))).unwrap();
    h.undo(&mut p).unwrap();

    let second = clip(&mut p, asset, 0, 5);
    assert_ne!(second.id, first_id, "an undone ID must not be handed out again");
    assert!(p.ids.peek() > first_id.raw());
}

#[test]
fn the_history_is_bounded_and_drops_the_oldest_entries() {
    let (mut p, seq, track, asset) = fixture();
    let mut h = History::new(8);
    for i in 0..20i64 {
        let c = clip(&mut p, asset, i * 10, 5);
        h.execute(&mut p, Box::new(AddClip::new(seq, track, c))).unwrap();
    }
    assert_eq!(h.undo_depth(), 8, "the stack must stay within its limit");
    assert_eq!(p.clip_count(), 20);

    // The most recent work is what remains undoable.
    for _ in 0..8 {
        h.undo(&mut p).unwrap();
    }
    assert_eq!(p.clip_count(), 12);
    assert!(!h.can_undo());
}

#[test]
fn dirty_tracking_follows_the_undo_stack() {
    let (mut p, seq, track, asset) = fixture();
    let mut h = History::default();
    assert!(!h.is_dirty());

    let c = clip(&mut p, asset, 0, 5);
    h.execute(&mut p, Box::new(AddClip::new(seq, track, c))).unwrap();
    assert!(h.is_dirty());

    h.mark_saved();
    assert!(!h.is_dirty());

    let c2 = clip(&mut p, asset, 10, 5);
    h.execute(&mut p, Box::new(AddClip::new(seq, track, c2))).unwrap();
    assert!(h.is_dirty());

    // Undoing back to the saved state makes it clean again.
    h.undo(&mut p).unwrap();
    assert!(!h.is_dirty());

    // And undoing past it makes it dirty once more.
    h.undo(&mut p).unwrap();
    assert!(h.is_dirty());
}

#[test]
fn a_saved_state_evicted_from_the_stack_leaves_the_project_dirty() {
    let (mut p, seq, track, asset) = fixture();
    let mut h = History::new(4);
    let c = clip(&mut p, asset, 0, 5);
    h.execute(&mut p, Box::new(AddClip::new(seq, track, c))).unwrap();
    h.mark_saved();
    assert!(!h.is_dirty());

    // Push enough work that the saved point falls off the bottom.
    for i in 1..10i64 {
        let c = clip(&mut p, asset, i * 10, 5);
        h.execute(&mut p, Box::new(AddClip::new(seq, track, c))).unwrap();
    }
    assert!(h.is_dirty());
    // Undoing everything still cannot reach the saved state, so it stays dirty.
    while h.can_undo() {
        h.undo(&mut p).unwrap();
    }
    assert!(h.is_dirty(), "an unreachable saved state must be assumed dirty");
}

#[test]
fn clearing_the_history_resets_everything() {
    let (mut p, seq, track, asset) = fixture();
    let mut h = History::default();
    let c = clip(&mut p, asset, 0, 5);
    h.execute(&mut p, Box::new(AddClip::new(seq, track, c))).unwrap();
    h.undo(&mut p).unwrap();
    h.clear();
    assert!(!h.can_undo() && !h.can_redo() && !h.is_dirty());
}

// ---- property and keyframe commands -----------------------------------

#[test]
fn setting_a_property_undoes_to_the_previous_value() {
    let (mut p, seq, track, asset) = fixture();
    let c = clip(&mut p, asset, 0, 10);
    let id = c.id;
    let mut h = History::default();
    h.execute(&mut p, Box::new(AddClip::new(seq, track, c))).unwrap();
    let before = p.clone();

    h.execute(
        &mut p,
        Box::new(SetClipProperty::new(
            seq,
            id,
            ClipProperty::Opacity,
            PropertyValue::Scalar(0.25),
        )),
    )
    .unwrap();
    assert_eq!(p.active().unwrap().tracks[0].clips()[0].transform.opacity.value, 0.25);

    h.undo(&mut p).unwrap();
    assert_eq!(p, before);
}

#[test]
fn one_command_type_covers_scalar_and_point_properties() {
    let (mut p, seq, track, asset) = fixture();
    let c = clip(&mut p, asset, 0, 10);
    let id = c.id;
    let mut h = History::default();
    h.execute(&mut p, Box::new(AddClip::new(seq, track, c))).unwrap();

    for (target, value) in [
        (ClipProperty::Position, PropertyValue::Point(Vec2::new(10.0, 20.0))),
        (ClipProperty::Scale, PropertyValue::Point(Vec2::new(2.0, 2.0))),
        (ClipProperty::Anchor, PropertyValue::Point(Vec2::new(0.0, 0.0))),
        (ClipProperty::Rotation, PropertyValue::Scalar(45.0)),
        (ClipProperty::Opacity, PropertyValue::Scalar(0.5)),
        (ClipProperty::Volume, PropertyValue::Scalar(0.7)),
        (ClipProperty::Pan, PropertyValue::Scalar(-0.3)),
    ] {
        h.execute(&mut p, Box::new(SetClipProperty::new(seq, id, target.clone(), value)))
            .unwrap();
    }

    let c = &p.active().unwrap().tracks[0].clips()[0];
    assert_eq!(c.transform.position.value, Vec2::new(10.0, 20.0));
    assert_eq!(c.transform.rotation.value, 45.0);
    assert_eq!(c.audio.volume.value, 0.7);
    assert_eq!(c.audio.pan.value, -0.3);
}

#[test]
fn a_type_mismatch_is_refused_rather_than_silently_ignored() {
    let (mut p, seq, track, asset) = fixture();
    let c = clip(&mut p, asset, 0, 10);
    let id = c.id;
    let mut h = History::default();
    h.execute(&mut p, Box::new(AddClip::new(seq, track, c))).unwrap();
    let before = p.clone();

    // Opacity is a scalar; handing it a point must fail.
    let err = h.execute(
        &mut p,
        Box::new(SetClipProperty::new(
            seq,
            id,
            ClipProperty::Opacity,
            PropertyValue::Point(Vec2::ONE),
        )),
    );
    assert!(matches!(err, Err(CommandError::Rejected(_))), "{err:?}");
    assert_eq!(p, before);
}

#[test]
fn a_slider_drag_collapses_into_one_property_undo_step() {
    let (mut p, seq, track, asset) = fixture();
    let c = clip(&mut p, asset, 0, 10);
    let id = c.id;
    let mut h = History::default();
    h.execute(&mut p, Box::new(AddClip::new(seq, track, c))).unwrap();
    let before = p.clone();
    let depth = h.undo_depth();

    for i in 1..=20 {
        h.execute_coalesced(
            &mut p,
            Box::new(SetClipProperty::new(
                seq,
                id,
                ClipProperty::Opacity,
                PropertyValue::Scalar(i as f64 / 20.0),
            )),
        )
        .unwrap();
    }
    assert_eq!(h.undo_depth(), depth + 1);
    h.undo(&mut p).unwrap();
    assert_eq!(p, before);
}

#[test]
fn different_properties_never_merge_with_each_other() {
    let (mut p, seq, track, asset) = fixture();
    let c = clip(&mut p, asset, 0, 10);
    let id = c.id;
    let mut h = History::default();
    h.execute(&mut p, Box::new(AddClip::new(seq, track, c))).unwrap();
    let depth = h.undo_depth();

    h.execute_coalesced(
        &mut p,
        Box::new(SetClipProperty::new(seq, id, ClipProperty::Opacity, PropertyValue::Scalar(0.5))),
    )
    .unwrap();
    h.execute_coalesced(
        &mut p,
        Box::new(SetClipProperty::new(seq, id, ClipProperty::Rotation, PropertyValue::Scalar(90.0))),
    )
    .unwrap();
    assert_eq!(h.undo_depth(), depth + 2);
}

#[test]
fn adding_a_keyframe_undoes_cleanly() {
    let (mut p, seq, track, asset) = fixture();
    let c = clip(&mut p, asset, 0, 10);
    let id = c.id;
    let mut h = History::default();
    h.execute(&mut p, Box::new(AddClip::new(seq, track, c))).unwrap();
    let before = p.clone();

    h.execute(
        &mut p,
        Box::new(SetClipKeyframe::new(
            seq,
            id,
            ClipProperty::Opacity,
            Ticks::from_seconds(2),
            PropertyValue::Scalar(0.0),
            Interpolation::EaseOut,
        )),
    )
    .unwrap();
    let opacity = &p.active().unwrap().tracks[0].clips()[0].transform.opacity;
    assert_eq!(opacity.keyframes().len(), 1);
    assert_eq!(opacity.keyframes()[0].interpolation, Interpolation::EaseOut);

    h.undo(&mut p).unwrap();
    assert_eq!(p, before, "removing the keyframe must restore the un-animated property");
}

#[test]
fn replacing_a_keyframe_restores_the_displaced_one_including_its_easing() {
    let (mut p, seq, track, asset) = fixture();
    let c = clip(&mut p, asset, 0, 10);
    let id = c.id;
    let mut h = History::default();
    h.execute(&mut p, Box::new(AddClip::new(seq, track, c))).unwrap();

    let at = Ticks::from_seconds(2);
    h.execute(
        &mut p,
        Box::new(SetClipKeyframe::new(
            seq,
            id,
            ClipProperty::Opacity,
            at,
            PropertyValue::Scalar(0.2),
            Interpolation::EaseIn,
        )),
    )
    .unwrap();
    let after_first = p.clone();

    h.execute(
        &mut p,
        Box::new(SetClipKeyframe::new(
            seq,
            id,
            ClipProperty::Opacity,
            at,
            PropertyValue::Scalar(0.9),
            Interpolation::Hold,
        )),
    )
    .unwrap();
    let kfs = p.active().unwrap().tracks[0].clips()[0].transform.opacity.keyframes();
    assert_eq!(kfs.len(), 1, "the same time must replace, not duplicate");
    assert_eq!(kfs[0].value, 0.9);

    h.undo(&mut p).unwrap();
    assert_eq!(p, after_first, "the displaced keyframe and its easing must come back");
}

#[test]
fn removing_a_keyframe_undoes_to_the_exact_keyframe() {
    let (mut p, seq, track, asset) = fixture();
    let c = clip(&mut p, asset, 0, 10);
    let id = c.id;
    let mut h = History::default();
    h.execute(&mut p, Box::new(AddClip::new(seq, track, c))).unwrap();

    let at = Ticks::from_seconds(3);
    h.execute(
        &mut p,
        Box::new(SetClipKeyframe::new(
            seq,
            id,
            ClipProperty::Position,
            at,
            PropertyValue::Point(Vec2::new(7.0, 8.0)),
            Interpolation::Bezier { x1: 0.1, y1: 0.2, x2: 0.3, y2: 0.4 },
        )),
    )
    .unwrap();
    let before = p.clone();

    h.execute(
        &mut p,
        Box::new(RemoveClipKeyframe::new(seq, id, ClipProperty::Position, at)),
    )
    .unwrap();
    assert!(!p.active().unwrap().tracks[0].clips()[0].transform.position.is_animated());

    h.undo(&mut p).unwrap();
    assert_eq!(p, before);
}

#[test]
fn removing_a_keyframe_that_is_not_there_is_refused() {
    let (mut p, seq, track, asset) = fixture();
    let c = clip(&mut p, asset, 0, 10);
    let id = c.id;
    let mut h = History::default();
    h.execute(&mut p, Box::new(AddClip::new(seq, track, c))).unwrap();
    let before = p.clone();

    let err = h.execute(
        &mut p,
        Box::new(RemoveClipKeyframe::new(
            seq,
            id,
            ClipProperty::Opacity,
            Ticks::from_seconds(1),
        )),
    );
    assert!(matches!(err, Err(CommandError::Rejected(_))));
    assert_eq!(p, before);
    // Only the earlier AddClip is on the stack; the refused command is not.
    assert_eq!(h.undo_depth(), 1);
}

// ---- structural commands ----------------------------------------------

#[test]
fn adding_and_removing_tracks_undoes_with_layer_order_intact() {
    let (mut p, seq, _, asset) = fixture();
    let mut h = History::default();

    let mut add = AddTrack::new(seq, TrackKind::Video);
    add.apply(&mut p).unwrap();
    let new_track = add.track_id().unwrap();
    add.undo(&mut p).unwrap();

    h.execute(&mut p, Box::new(AddTrack::new(seq, TrackKind::Video))).unwrap();
    assert_eq!(p.sequence(seq).unwrap().video_tracks().count(), 3);
    h.undo(&mut p).unwrap();
    assert_eq!(p.sequence(seq).unwrap().video_tracks().count(), 2);
    let _ = new_track;

    // Removing a middle track must restore it to the same index, because index
    // order is compositing order.
    let middle = p.sequence(seq).unwrap().tracks[1].id;
    let c = clip(&mut p, asset, 0, 5);
    p.sequence_mut(seq).unwrap().track_mut(middle).unwrap().insert_clip(c).unwrap();
    let before = p.clone();

    h.execute(&mut p, Box::new(RemoveTrack::new(seq, middle))).unwrap();
    assert_eq!(p.sequence(seq).unwrap().tracks.len(), 3);
    assert_eq!(p.clip_count(), 0, "the track's clips went with it");

    h.undo(&mut p).unwrap();
    assert_eq!(p, before, "the track, its clips and its position must all return");
}

#[test]
fn markers_add_and_remove_undoably() {
    let (mut p, seq, _, _) = fixture();
    let mut h = History::default();
    let before = p.clone();

    let mid = p.new_marker_id();
    let with_id = p.clone();
    h.execute(
        &mut p,
        Box::new(AddMarker::new(seq, Marker::new(mid, "Act 2", Ticks::from_seconds(42)))),
    )
    .unwrap();
    assert_eq!(p.sequence(seq).unwrap().markers.len(), 1);
    let after_add = p.clone();

    h.undo(&mut p).unwrap();
    assert_eq!(p, with_id);
    h.redo(&mut p).unwrap();
    assert_eq!(p, after_add);

    h.execute(&mut p, Box::new(RemoveMarker::new(seq, mid))).unwrap();
    assert!(p.sequence(seq).unwrap().markers.is_empty());
    h.undo(&mut p).unwrap();
    assert_eq!(p, after_add);

    assert_ne!(before, p);
}

#[test]
fn toggling_a_clip_undoes() {
    let (mut p, seq, track, asset) = fixture();
    let c = clip(&mut p, asset, 0, 10);
    let id = c.id;
    let mut h = History::default();
    h.execute(&mut p, Box::new(AddClip::new(seq, track, c))).unwrap();
    let before = p.clone();

    h.execute(&mut p, Box::new(SetClipEnabled::new(seq, id, false))).unwrap();
    assert!(!p.active().unwrap().tracks[0].clips()[0].enabled);
    h.undo(&mut p).unwrap();
    assert_eq!(p, before);
}

#[test]
fn commands_against_a_missing_object_report_a_typed_error() {
    let (mut p, seq, track, _) = fixture();
    let mut h = History::default();
    let ghost = ClipId::from_raw(999_999);

    assert!(matches!(
        h.execute(&mut p, Box::new(RemoveClip::new(seq, track, ghost))),
        Err(CommandError::ClipNotFound(_))
    ));
    assert!(matches!(
        h.execute(&mut p, Box::new(MoveClip::new(seq, track, ghost, Ticks::ZERO))),
        Err(CommandError::Core(ve_core::CoreError::ClipNotFound(_)))
    ));
    assert!(matches!(
        h.execute(
            &mut p,
            Box::new(MoveClip::new(SequenceId::from_raw(12345), track, ghost, Ticks::ZERO))
        ),
        Err(CommandError::SequenceNotFound(_))
    ));
    assert_eq!(h.undo_depth(), 0);
}
