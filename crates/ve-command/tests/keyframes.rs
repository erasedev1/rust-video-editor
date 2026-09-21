//! Keyframe editing: adding, deleting, retiming, easing, pasting and clearing,
//! and what each of those has to leave behind when it is undone.

use ve_command::*;
use ve_core::{
    Clip, ClipId, Interpolation, MediaInfo, Project, SequenceId, Size, Vec2, VideoStreamInfo,
};
use ve_time::{Rate, Ticks};

fn fixture() -> (Project, SequenceId, ClipId, History) {
    let mut p = Project::with_default_sequence("Keyframes");
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
    let clip = Clip::new(id, asset, "clip", Ticks::ZERO, Ticks::ZERO, Ticks::from_seconds(10));
    let mut h = History::default();
    h.execute(&mut p, Box::new(AddClip::new(seq, track, clip))).unwrap();
    (p, seq, id, h)
}

fn clip_of(p: &Project, id: ClipId) -> &Clip {
    p.active().unwrap().find_clip(id).unwrap().1
}

fn opacity_times(p: &Project, id: ClipId) -> Vec<Ticks> {
    clip_of(p, id).transform.opacity.keyframes().iter().map(|k| k.time).collect()
}

/// Three opacity keyframes at 0, 1 and 2 seconds.
fn animate_opacity(p: &mut Project, seq: SequenceId, id: ClipId, h: &mut History) {
    for (second, value) in [(0, 0.0), (1, 0.5), (2, 1.0)] {
        h.execute(
            p,
            Box::new(EditKeyframes::one(
                seq,
                id,
                ClipProperty::Opacity,
                KeyframeEdit::Set {
                    time: Ticks::from_seconds(second),
                    value: PropertyValue::Scalar(value),
                    interpolation: Interpolation::Linear,
                },
            )),
        )
        .unwrap();
    }
}

#[test]
fn setting_a_keyframe_animates_the_property_and_undoes_to_a_constant() {
    let (mut p, seq, id, mut h) = fixture();
    let before = p.clone();

    h.execute(
        &mut p,
        Box::new(EditKeyframes::one(
            seq,
            id,
            ClipProperty::Opacity,
            KeyframeEdit::Set {
                time: Ticks::from_seconds(1),
                value: PropertyValue::Scalar(0.25),
                interpolation: Interpolation::EaseOut,
            },
        )),
    )
    .unwrap();

    let opacity = &clip_of(&p, id).transform.opacity;
    assert!(opacity.is_animated());
    assert_eq!(opacity.keyframes()[0].interpolation, Interpolation::EaseOut);

    h.undo(&mut p).unwrap();
    assert_eq!(p, before, "undo must leave no trace of the keyframe");
}

#[test]
fn a_value_of_the_wrong_type_is_refused_and_changes_nothing() {
    let (mut p, seq, id, mut h) = fixture();
    let before = p.clone();

    let result = h.execute(
        &mut p,
        Box::new(EditKeyframes::one(
            seq,
            id,
            ClipProperty::Opacity,
            KeyframeEdit::Set {
                time: Ticks::ZERO,
                // Opacity is a scalar; a point cannot be written to it.
                value: PropertyValue::Point(Vec2::ONE),
                interpolation: Interpolation::Linear,
            },
        )),
    );
    assert!(matches!(result, Err(CommandError::Rejected(_))), "{result:?}");
    assert_eq!(p, before);
}

#[test]
fn one_command_can_edit_several_properties_as_one_undo_step() {
    let (mut p, seq, id, mut h) = fixture();
    let before = p.clone();

    h.execute(
        &mut p,
        Box::new(EditKeyframes::new(
            seq,
            id,
            vec![
                (
                    ClipProperty::Opacity,
                    KeyframeEdit::Set {
                        time: Ticks::ZERO,
                        value: PropertyValue::Scalar(0.0),
                        interpolation: Interpolation::Linear,
                    },
                ),
                (
                    ClipProperty::Position,
                    KeyframeEdit::Set {
                        time: Ticks::ZERO,
                        value: PropertyValue::Point(Vec2::new(10.0, 20.0)),
                        interpolation: Interpolation::Linear,
                    },
                ),
            ],
        )),
    )
    .unwrap();

    let clip = clip_of(&p, id);
    assert!(clip.transform.opacity.is_animated() && clip.transform.position.is_animated());
    assert_eq!(h.undo_depth(), 2, "the clip, then one step for both keyframes");

    h.undo(&mut p).unwrap();
    assert_eq!(p, before);
}

#[test]
fn a_failure_part_way_through_a_list_leaves_the_project_untouched() {
    let (mut p, seq, id, mut h) = fixture();
    let before = p.clone();

    let result = h.execute(
        &mut p,
        Box::new(EditKeyframes::new(
            seq,
            id,
            vec![
                (
                    ClipProperty::Opacity,
                    KeyframeEdit::Set {
                        time: Ticks::ZERO,
                        value: PropertyValue::Scalar(0.5),
                        interpolation: Interpolation::Linear,
                    },
                ),
                // Rotation is a scalar, so this one is refused — and the
                // opacity keyframe the first edit wrote has to go back.
                (
                    ClipProperty::Rotation,
                    KeyframeEdit::Set {
                        time: Ticks::ZERO,
                        value: PropertyValue::Point(Vec2::ONE),
                        interpolation: Interpolation::Linear,
                    },
                ),
            ],
        )),
    );
    assert!(result.is_err());
    assert_eq!(p, before, "a half-applied command is worse than a refused one");
}

#[test]
fn retiming_states_absolute_times_so_a_drag_is_one_undo_step() {
    let (mut p, seq, id, mut h) = fixture();
    animate_opacity(&mut p, seq, id, &mut h);
    let before_drag = p.clone();
    let depth = h.undo_depth();

    // A drag: the same gesture restated on every pointer move.
    for destination in [1_500, 2_000, 2_500] {
        let times = vec![Ticks::ZERO, Ticks::from_seconds(1), Ticks::from_millis(destination)];
        h.execute_coalesced(
            &mut p,
            Box::new(EditKeyframes::one(
                seq,
                id,
                ClipProperty::Opacity,
                KeyframeEdit::SetTimes(times),
            )),
        )
        .unwrap();
    }

    assert_eq!(h.undo_depth(), depth + 1, "a drag is one entry, not three");
    assert_eq!(
        opacity_times(&p, id),
        vec![Ticks::ZERO, Ticks::from_seconds(1), Ticks::from_millis(2_500)]
    );

    h.undo(&mut p).unwrap();
    assert_eq!(p, before_drag, "undo takes the whole gesture back");
    h.redo(&mut p).unwrap();
    assert_eq!(
        opacity_times(&p, id),
        vec![Ticks::ZERO, Ticks::from_seconds(1), Ticks::from_millis(2_500)],
        "redo must land where the drag ended, not where it started"
    );
}

#[test]
fn a_retime_that_collapses_two_keyframes_still_undoes_exactly() {
    let (mut p, seq, id, mut h) = fixture();
    animate_opacity(&mut p, seq, id, &mut h);
    let before = p.clone();

    // The last keyframe dragged onto the middle one. An inverse retime could
    // not bring the lost keyframe back, which is why undo restores the list.
    h.execute(
        &mut p,
        Box::new(EditKeyframes::one(
            seq,
            id,
            ClipProperty::Opacity,
            KeyframeEdit::SetTimes(vec![
                Ticks::ZERO,
                Ticks::from_seconds(1),
                Ticks::from_seconds(1),
            ]),
        )),
    )
    .unwrap();
    assert_eq!(opacity_times(&p, id).len(), 2);

    h.undo(&mut p).unwrap();
    assert_eq!(p, before);
}

#[test]
fn a_retime_with_the_wrong_number_of_times_is_refused() {
    let (mut p, seq, id, mut h) = fixture();
    animate_opacity(&mut p, seq, id, &mut h);
    let before = p.clone();

    let result = h.execute(
        &mut p,
        Box::new(EditKeyframes::one(
            seq,
            id,
            ClipProperty::Opacity,
            KeyframeEdit::SetTimes(vec![Ticks::ZERO]),
        )),
    );
    assert!(matches!(result, Err(CommandError::Rejected(_))), "{result:?}");
    assert_eq!(p, before);
}

#[test]
fn deleting_several_keyframes_is_one_step_and_ignores_times_that_are_empty() {
    let (mut p, seq, id, mut h) = fixture();
    animate_opacity(&mut p, seq, id, &mut h);
    let before = p.clone();

    h.execute(
        &mut p,
        Box::new(EditKeyframes::one(
            seq,
            id,
            ClipProperty::Opacity,
            KeyframeEdit::Remove(vec![
                Ticks::ZERO,
                Ticks::from_seconds(2),
                // Nothing here: a selection that undo already emptied is not an
                // error, it is simply nothing to delete.
                Ticks::from_seconds(7),
            ]),
        )),
    )
    .unwrap();
    assert_eq!(opacity_times(&p, id), vec![Ticks::from_seconds(1)]);

    h.undo(&mut p).unwrap();
    assert_eq!(p, before);
}

#[test]
fn easing_is_set_on_the_keyframe_the_segment_leaves_and_merges_while_dragged() {
    let (mut p, seq, id, mut h) = fixture();
    animate_opacity(&mut p, seq, id, &mut h);
    let before = p.clone();
    let depth = h.undo_depth();

    // Dragging a bezier handle in the graph editor: many small edits to the
    // same keyframe, one undo step.
    for x in [0.3, 0.4, 0.5] {
        h.execute_coalesced(
            &mut p,
            Box::new(EditKeyframes::one(
                seq,
                id,
                ClipProperty::Opacity,
                KeyframeEdit::SetInterpolation {
                    time: Ticks::ZERO,
                    interpolation: Interpolation::Bezier { x1: x, y1: 0.0, x2: 1.0, y2: 1.0 },
                },
            )),
        )
        .unwrap();
    }
    assert_eq!(h.undo_depth(), depth + 1);
    assert_eq!(
        clip_of(&p, id).transform.opacity.keyframes()[0].interpolation,
        Interpolation::Bezier { x1: 0.5, y1: 0.0, x2: 1.0, y2: 1.0 }
    );

    h.undo(&mut p).unwrap();
    assert_eq!(p, before);
}

#[test]
fn easing_a_keyframe_that_is_not_there_is_refused() {
    let (mut p, seq, id, mut h) = fixture();
    animate_opacity(&mut p, seq, id, &mut h);
    let before = p.clone();

    let result = h.execute(
        &mut p,
        Box::new(EditKeyframes::one(
            seq,
            id,
            ClipProperty::Opacity,
            KeyframeEdit::SetInterpolation {
                time: Ticks::from_seconds(9),
                interpolation: Interpolation::Hold,
            },
        )),
    );
    assert!(matches!(result, Err(CommandError::Rejected(_))), "{result:?}");
    assert_eq!(p, before);
}

#[test]
fn pasting_offsets_every_keyframe_from_the_paste_point() {
    let (mut p, seq, id, mut h) = fixture();
    animate_opacity(&mut p, seq, id, &mut h);

    // Lifted from the clip, exactly as a copy would.
    let copied = property_ref(clip_of(&p, id), &ClipProperty::Opacity).unwrap().keyframes();
    let before = p.clone();

    h.execute(
        &mut p,
        Box::new(EditKeyframes::one(
            seq,
            id,
            ClipProperty::Opacity,
            KeyframeEdit::Insert { keyframes: copied, at: Ticks::from_seconds(5) },
        )),
    )
    .unwrap();

    assert_eq!(
        opacity_times(&p, id),
        vec![
            Ticks::ZERO,
            Ticks::from_seconds(1),
            Ticks::from_seconds(2),
            Ticks::from_seconds(5),
            Ticks::from_seconds(6),
            Ticks::from_seconds(7),
        ],
        "the shape of the copy has to survive the move"
    );

    h.undo(&mut p).unwrap();
    assert_eq!(p, before);
}

#[test]
fn a_paste_keeps_two_properties_the_distance_apart_they_were_copied() {
    let (mut p, seq, id, mut h) = fixture();
    // A copy held as offsets from a shared origin: opacity at the origin,
    // rotation a second later.
    let opacity = KeyframePoint {
        time: Ticks::ZERO,
        value: PropertyValue::Scalar(1.0),
        interpolation: Interpolation::Linear,
    };
    let rotation = KeyframePoint {
        time: Ticks::from_seconds(1),
        value: PropertyValue::Scalar(45.0),
        interpolation: Interpolation::Linear,
    };

    h.execute(
        &mut p,
        Box::new(EditKeyframes::new(
            seq,
            id,
            vec![
                (
                    ClipProperty::Opacity,
                    KeyframeEdit::Insert {
                        keyframes: vec![opacity],
                        at: Ticks::from_seconds(4),
                    },
                ),
                (
                    ClipProperty::Rotation,
                    KeyframeEdit::Insert {
                        keyframes: vec![rotation],
                        at: Ticks::from_seconds(4),
                    },
                ),
            ],
        )),
    )
    .unwrap();

    let clip = clip_of(&p, id);
    assert_eq!(clip.transform.opacity.keyframes()[0].time, Ticks::from_seconds(4));
    assert_eq!(
        clip.transform.rotation.keyframes()[0].time,
        Ticks::from_seconds(5),
        "the second property has to stay a second behind the first"
    );
}

#[test]
fn pasting_onto_an_existing_keyframe_overwrites_it_and_undoes_to_it() {
    let (mut p, seq, id, mut h) = fixture();
    animate_opacity(&mut p, seq, id, &mut h);
    let before = p.clone();

    h.execute(
        &mut p,
        Box::new(EditKeyframes::one(
            seq,
            id,
            ClipProperty::Opacity,
            KeyframeEdit::Insert {
                keyframes: vec![KeyframePoint {
                    time: Ticks::ZERO,
                    value: PropertyValue::Scalar(0.75),
                    interpolation: Interpolation::Hold,
                }],
                at: Ticks::from_seconds(1),
            },
        )),
    )
    .unwrap();

    let kfs = clip_of(&p, id).transform.opacity.keyframes();
    assert_eq!(kfs.len(), 3, "an overwrite is not an insert");
    assert_eq!(kfs[1].value, 0.75);
    assert_eq!(kfs[1].interpolation, Interpolation::Hold);

    h.undo(&mut p).unwrap();
    assert_eq!(p, before, "the overwritten keyframe has to come back");
}

#[test]
fn freezing_keeps_the_value_at_that_instant_and_undo_brings_the_curve_back() {
    let (mut p, seq, id, mut h) = fixture();
    animate_opacity(&mut p, seq, id, &mut h);
    let before = p.clone();

    h.execute(
        &mut p,
        Box::new(EditKeyframes::one(
            seq,
            id,
            ClipProperty::Opacity,
            KeyframeEdit::Freeze { at: Ticks::from_seconds(1) },
        )),
    )
    .unwrap();

    let opacity = &clip_of(&p, id).transform.opacity;
    assert!(!opacity.is_animated());
    assert_eq!(opacity.value, 0.5, "the property stays where the playhead left it");

    h.undo(&mut p).unwrap();
    assert_eq!(p, before);
}

#[test]
fn two_separate_edits_do_not_merge_into_one_undo_step() {
    let (mut p, seq, id, mut h) = fixture();
    let depth = h.undo_depth();

    for second in [1, 2] {
        h.execute_coalesced(
            &mut p,
            Box::new(EditKeyframes::one(
                seq,
                id,
                ClipProperty::Opacity,
                KeyframeEdit::Set {
                    time: Ticks::from_seconds(second),
                    value: PropertyValue::Scalar(0.5),
                    interpolation: Interpolation::Linear,
                },
            )),
        )
        .unwrap();
    }
    assert_eq!(h.undo_depth(), depth + 2, "adding two keyframes is two things the user did");
}

#[test]
fn a_retime_of_a_different_property_does_not_absorb_the_previous_gesture() {
    let (mut p, seq, id, mut h) = fixture();
    animate_opacity(&mut p, seq, id, &mut h);
    h.execute(
        &mut p,
        Box::new(EditKeyframes::one(
            seq,
            id,
            ClipProperty::Rotation,
            KeyframeEdit::Set {
                time: Ticks::ZERO,
                value: PropertyValue::Scalar(0.0),
                interpolation: Interpolation::Linear,
            },
        )),
    )
    .unwrap();
    let depth = h.undo_depth();

    h.execute_coalesced(
        &mut p,
        Box::new(EditKeyframes::one(
            seq,
            id,
            ClipProperty::Opacity,
            KeyframeEdit::SetTimes(vec![
                Ticks::ZERO,
                Ticks::from_seconds(1),
                Ticks::from_seconds(3),
            ]),
        )),
    )
    .unwrap();
    h.execute_coalesced(
        &mut p,
        Box::new(EditKeyframes::one(
            seq,
            id,
            ClipProperty::Rotation,
            KeyframeEdit::SetTimes(vec![Ticks::from_seconds(1)]),
        )),
    )
    .unwrap();

    assert_eq!(h.undo_depth(), depth + 2, "different properties are different gestures");
}

#[test]
fn reading_a_property_back_erases_its_type_without_losing_anything() {
    let (mut p, seq, id, mut h) = fixture();
    h.execute(
        &mut p,
        Box::new(EditKeyframes::one(
            seq,
            id,
            ClipProperty::Position,
            KeyframeEdit::Set {
                time: Ticks::from_seconds(1),
                value: PropertyValue::Point(Vec2::new(3.0, 4.0)),
                interpolation: Interpolation::EaseIn,
            },
        )),
    )
    .unwrap();

    let clip = clip_of(&p, id);
    let position = property_ref(clip, &ClipProperty::Position).unwrap();
    assert!(position.is_animated());
    assert_eq!(position.keyframe_count(), 1);
    assert_eq!(position.keyframe_times(), vec![Ticks::from_seconds(1)]);
    assert_eq!(
        position.keyframe_at(Ticks::from_seconds(1)).unwrap().value,
        PropertyValue::Point(Vec2::new(3.0, 4.0))
    );
    assert_eq!(
        position.evaluate(Ticks::from_seconds(5)),
        PropertyValue::Point(Vec2::new(3.0, 4.0)),
        "outside the keyframed range the value is clamped, never extrapolated"
    );
    assert_eq!(position.value(), PropertyValue::Point(Vec2::ZERO), "the static value is kept");

    // Scale is untouched, and says so.
    assert!(!property_ref(clip, &ClipProperty::Scale).unwrap().is_animated());
    // A property on an effect that is not there does not resolve at all.
    assert!(property_ref(
        clip,
        &ClipProperty::EffectParam {
            effect: ve_core::EffectId::from_raw(404),
            key: "radius".into()
        }
    )
    .is_none());
}

#[test]
fn the_animatable_properties_of_a_clip_include_its_effect_parameters() {
    let (mut p, _seq, id, _h) = fixture();
    let plain = animatable_properties(clip_of(&p, id));
    assert_eq!(plain.len(), 7, "transform and sound: {plain:?}");

    let effect_id = p.new_effect_id();
    {
        let clip = p.active_mut().unwrap().find_clip_mut(id).unwrap().1;
        clip.effects.push(
            ve_core::Effect::new(effect_id, "verge.blur.gaussian", "Gaussian Blur")
                .with_param("radius", ve_core::ParamValue::scalar(0.0))
                // Not animatable, and so not offered.
                .with_param("repeat_edges", ve_core::ParamValue::Bool(true)),
        );
    }

    let with_effect = animatable_properties(clip_of(&p, id));
    assert_eq!(
        with_effect.last(),
        Some(&ClipProperty::EffectParam { effect: effect_id, key: "radius".into() }),
        "{with_effect:?}"
    );
    assert_eq!(with_effect.len(), 8);
}
