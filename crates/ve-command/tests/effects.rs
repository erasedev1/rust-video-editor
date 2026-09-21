//! Building an effect chain: adding, removing, reordering, switching off and
//! renaming, and what each of those has to put back when it is undone.

use ve_command::*;
use ve_core::registry::kinds;
use ve_core::{
    Clip, ClipId, CompositionId, CompositionLayer, CompositionSettings, Effect, Interpolation,
    LayerId, MediaInfo, ParamValue, Project, SequenceId, Size, VideoStreamInfo,
};
use ve_time::{Rate, Ticks};

fn fixture() -> (Project, SequenceId, ClipId, History) {
    let mut p = Project::with_default_sequence("Effects");
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

fn chain(p: &Project, host: EffectHost) -> Vec<Effect> {
    host.effects(p).unwrap().to_vec()
}

fn kinds_of(p: &Project, host: EffectHost) -> Vec<String> {
    chain(p, host).iter().map(|e| e.kind.clone()).collect()
}

#[test]
fn an_added_effect_arrives_with_every_parameter_at_its_default() {
    let (mut p, seq, clip, mut h) = fixture();
    let host = EffectHost::clip(seq, clip);
    h.execute(&mut p, Box::new(AddEffect::new(host, kinds::GAUSSIAN_BLUR))).unwrap();

    let effects = chain(&p, host);
    assert_eq!(effects.len(), 1);
    assert_eq!(effects[0].name, "Gaussian Blur");
    assert!(effects[0].enabled);
    assert_eq!(effects[0].param("radius").unwrap().as_scalar_at(Ticks::ZERO), Some(8.0));

    h.undo(&mut p).unwrap();
    assert!(chain(&p, host).is_empty());
    h.redo(&mut p).unwrap();
    assert_eq!(chain(&p, host).len(), 1);
}

#[test]
fn redo_re_adds_the_effect_under_the_id_it_first_had() {
    // Anything holding onto an effect — a keyframe command, the inspector's
    // selection — is holding an ID, so a redo that minted a new one would leave
    // those references pointing at nothing.
    let (mut p, seq, clip, mut h) = fixture();
    let host = EffectHost::clip(seq, clip);
    h.execute(&mut p, Box::new(AddEffect::new(host, kinds::SHARPEN))).unwrap();
    let id = chain(&p, host)[0].id;

    h.undo(&mut p).unwrap();
    h.redo(&mut p).unwrap();
    assert_eq!(chain(&p, host)[0].id, id);
}

#[test]
fn an_effect_can_be_inserted_part_way_along_the_chain() {
    let (mut p, seq, clip, mut h) = fixture();
    let host = EffectHost::clip(seq, clip);
    h.execute(&mut p, Box::new(AddEffect::new(host, kinds::GAUSSIAN_BLUR))).unwrap();
    h.execute(&mut p, Box::new(AddEffect::new(host, kinds::SHARPEN))).unwrap();
    h.execute(&mut p, Box::new(AddEffect::new(host, kinds::COLOR_ADJUST).at(1))).unwrap();

    assert_eq!(
        kinds_of(&p, host),
        vec![kinds::GAUSSIAN_BLUR, kinds::COLOR_ADJUST, kinds::SHARPEN]
    );
    // An index past the end appends rather than being refused.
    h.execute(&mut p, Box::new(AddEffect::new(host, kinds::LUMA_KEY).at(99))).unwrap();
    assert_eq!(kinds_of(&p, host).last().unwrap(), kinds::LUMA_KEY);
}

#[test]
fn an_effect_nothing_answers_to_is_refused_and_costs_nothing() {
    let (mut p, seq, clip, mut h) = fixture();
    let host = EffectHost::clip(seq, clip);
    let before = p.new_effect_id().raw();

    let depth = h.undo_depth();
    let result = h.execute(&mut p, Box::new(AddEffect::new(host, "nobody.at.all")));
    assert!(result.is_err());
    assert!(chain(&p, host).is_empty());
    // The refused add did not burn an ID, and did not land in the history.
    assert_eq!(p.new_effect_id().raw(), before + 1);
    assert_eq!(h.undo_depth(), depth);
}

#[test]
fn removing_an_effect_puts_it_back_where_it_was_with_its_keyframes() {
    let (mut p, seq, clip, mut h) = fixture();
    let host = EffectHost::clip(seq, clip);
    for kind in [kinds::GAUSSIAN_BLUR, kinds::COLOR_ADJUST, kinds::SHARPEN] {
        h.execute(&mut p, Box::new(AddEffect::new(host, kind))).unwrap();
    }
    let colour = chain(&p, host)[1].id;
    // Animate it, so undo has something a rebuild from the registry would lose.
    h.execute(
        &mut p,
        Box::new(SetClipKeyframe::new(
            seq,
            clip,
            ClipProperty::EffectParam { effect: colour, key: "saturation".into() },
            Ticks::from_seconds(1),
            PropertyValue::Scalar(2.5),
            Interpolation::Linear,
        )),
    )
    .unwrap();

    h.execute(&mut p, Box::new(RemoveEffect::new(host, colour))).unwrap();
    assert_eq!(kinds_of(&p, host), vec![kinds::GAUSSIAN_BLUR, kinds::SHARPEN]);

    h.undo(&mut p).unwrap();
    let effects = chain(&p, host);
    assert_eq!(effects[1].id, colour, "it came back in the middle, not at the end");
    assert_eq!(
        effects[1].param("saturation").unwrap().as_scalar_at(Ticks::from_seconds(1)),
        Some(2.5),
        "the keyframe came back with it"
    );
}

#[test]
fn reordering_is_a_real_edit_and_undoes_to_where_it_started() {
    let (mut p, seq, clip, mut h) = fixture();
    let host = EffectHost::clip(seq, clip);
    for kind in [kinds::GAUSSIAN_BLUR, kinds::COLOR_ADJUST, kinds::SHARPEN] {
        h.execute(&mut p, Box::new(AddEffect::new(host, kind))).unwrap();
    }
    let sharpen = chain(&p, host)[2].id;

    h.execute(&mut p, Box::new(MoveEffect::new(host, sharpen, 0))).unwrap();
    assert_eq!(
        kinds_of(&p, host),
        vec![kinds::SHARPEN, kinds::GAUSSIAN_BLUR, kinds::COLOR_ADJUST]
    );

    h.undo(&mut p).unwrap();
    assert_eq!(
        kinds_of(&p, host),
        vec![kinds::GAUSSIAN_BLUR, kinds::COLOR_ADJUST, kinds::SHARPEN]
    );
}

#[test]
fn a_drag_across_the_chain_is_one_undo_step_back_to_the_original_place() {
    let (mut p, seq, clip, mut h) = fixture();
    let host = EffectHost::clip(seq, clip);
    for kind in [kinds::GAUSSIAN_BLUR, kinds::COLOR_ADJUST, kinds::SHARPEN, kinds::LUMA_KEY] {
        h.execute(&mut p, Box::new(AddEffect::new(host, kind))).unwrap();
    }
    let before = h.undo_depth();
    let blur = chain(&p, host)[0].id;

    // One command per pointer move, each restating where the effect now is,
    // coalesced the way the interface coalesces a drag.
    for to in [1, 2, 3, 2] {
        h.execute_coalesced(&mut p, Box::new(MoveEffect::new(host, blur, to))).unwrap();
    }
    h.break_merge();
    assert_eq!(h.undo_depth(), before + 1, "a drag is one entry, not four");
    assert_eq!(kinds_of(&p, host)[2], kinds::GAUSSIAN_BLUR);

    h.undo(&mut p).unwrap();
    assert_eq!(kinds_of(&p, host)[0], kinds::GAUSSIAN_BLUR);
}

#[test]
fn moving_an_effect_past_the_end_lands_it_last_rather_than_failing() {
    let (mut p, seq, clip, mut h) = fixture();
    let host = EffectHost::clip(seq, clip);
    for kind in [kinds::GAUSSIAN_BLUR, kinds::SHARPEN] {
        h.execute(&mut p, Box::new(AddEffect::new(host, kind))).unwrap();
    }
    let blur = chain(&p, host)[0].id;
    h.execute(&mut p, Box::new(MoveEffect::new(host, blur, 12))).unwrap();
    assert_eq!(kinds_of(&p, host), vec![kinds::SHARPEN, kinds::GAUSSIAN_BLUR]);
}

#[test]
fn switching_an_effect_off_keeps_it_in_place_with_everything_it_had() {
    let (mut p, seq, clip, mut h) = fixture();
    let host = EffectHost::clip(seq, clip);
    h.execute(&mut p, Box::new(AddEffect::new(host, kinds::GAUSSIAN_BLUR))).unwrap();
    let blur = chain(&p, host)[0].id;

    h.execute(&mut p, Box::new(SetEffectEnabled::new(host, blur, false))).unwrap();
    let effects = chain(&p, host);
    assert!(!effects[0].enabled);
    assert_eq!(effects[0].param("radius").unwrap().as_scalar_at(Ticks::ZERO), Some(8.0));

    h.undo(&mut p).unwrap();
    assert!(chain(&p, host)[0].enabled);
}

#[test]
fn renaming_merges_while_typing_and_refuses_an_empty_name() {
    let (mut p, seq, clip, mut h) = fixture();
    let host = EffectHost::clip(seq, clip);
    h.execute(&mut p, Box::new(AddEffect::new(host, kinds::GAUSSIAN_BLUR))).unwrap();
    let blur = chain(&p, host)[0].id;
    let before = h.undo_depth();

    for name in ["E", "Ed", "Edg", "Edge softener"] {
        h.execute_coalesced(&mut p, Box::new(RenameEffect::new(host, blur, name))).unwrap();
    }
    h.break_merge();
    assert_eq!(h.undo_depth(), before + 1);
    assert_eq!(chain(&p, host)[0].name, "Edge softener");

    assert!(h.execute(&mut p, Box::new(RenameEffect::new(host, blur, "   "))).is_err());
    assert_eq!(chain(&p, host)[0].name, "Edge softener");

    h.undo(&mut p).unwrap();
    assert_eq!(chain(&p, host)[0].name, "Gaussian Blur");
}

#[test]
fn a_command_naming_an_effect_that_is_not_there_is_refused() {
    let (mut p, seq, clip, mut h) = fixture();
    let host = EffectHost::clip(seq, clip);
    let ghost = p.new_effect_id();
    assert!(h.execute(&mut p, Box::new(RemoveEffect::new(host, ghost))).is_err());
    assert!(h.execute(&mut p, Box::new(MoveEffect::new(host, ghost, 0))).is_err());
    assert!(h.execute(&mut p, Box::new(SetEffectEnabled::new(host, ghost, false))).is_err());
    assert!(h.execute(&mut p, Box::new(RenameEffect::new(host, ghost, "x"))).is_err());
}

/// A composition with one layer, to check the layer half of every command.
fn layer_fixture() -> (Project, CompositionId, LayerId, History) {
    let (mut p, _seq, _clip, mut h) = fixture();
    let asset = p.assets[0].id;

    let mut add = AddComposition::new("Comp", CompositionSettings::default());
    add.apply(&mut p).unwrap();
    let comp = add.composition_id().unwrap();

    let layer_id = p.new_layer_id();
    let layer =
        CompositionLayer::new(layer_id, "layer", asset, Ticks::ZERO, Ticks::from_seconds(5));
    h.execute(&mut p, Box::new(AddLayer::new(comp, layer))).unwrap();
    (p, comp, layer_id, h)
}

#[test]
fn the_same_commands_edit_a_composition_layers_chain() {
    let (mut p, comp, layer, mut h) = layer_fixture();
    let host = EffectHost::layer(comp, layer);

    h.execute(&mut p, Box::new(AddEffect::new(host, kinds::GAUSSIAN_BLUR))).unwrap();
    h.execute(&mut p, Box::new(AddEffect::new(host, kinds::SHAPE_MASK))).unwrap();
    let mask = chain(&p, host)[1].id;
    h.execute(&mut p, Box::new(MoveEffect::new(host, mask, 0))).unwrap();
    assert_eq!(kinds_of(&p, host), vec![kinds::SHAPE_MASK, kinds::GAUSSIAN_BLUR]);

    h.execute(&mut p, Box::new(SetEffectEnabled::new(host, mask, false))).unwrap();
    assert!(!chain(&p, host)[0].enabled);

    h.execute(&mut p, Box::new(RemoveEffect::new(host, mask))).unwrap();
    assert_eq!(kinds_of(&p, host), vec![kinds::GAUSSIAN_BLUR]);
    h.undo(&mut p).unwrap();
    assert_eq!(kinds_of(&p, host), vec![kinds::SHAPE_MASK, kinds::GAUSSIAN_BLUR]);
}

#[test]
fn an_effect_parameter_animates_through_the_ordinary_keyframe_commands() {
    // The point of parameters being `Property<T>`: nothing effect-specific is
    // needed to keyframe one.
    let (mut p, seq, clip, mut h) = fixture();
    let host = EffectHost::clip(seq, clip);
    h.execute(&mut p, Box::new(AddEffect::new(host, kinds::GAUSSIAN_BLUR))).unwrap();
    let blur = chain(&p, host)[0].id;
    let target = ClipProperty::EffectParam { effect: blur, key: "radius".into() };

    for (at, value) in [(0, 0.0), (2, 100.0)] {
        h.execute(
            &mut p,
            Box::new(SetClipKeyframe::new(
                seq,
                clip,
                target.clone(),
                Ticks::from_seconds(at),
                PropertyValue::Scalar(value),
                Interpolation::Linear,
            )),
        )
        .unwrap();
    }

    let effect = chain(&p, host)[0].clone();
    assert!(effect.is_animated());
    let ParamValue::Scalar(radius) = effect.param("radius").unwrap() else { panic!() };
    assert!((radius.evaluate(Ticks::from_seconds(1)) - 50.0).abs() < 1e-9);

    // And the animation editor sees it as an ordinary animatable property.
    let properties = animatable_properties(&effect_host_clip(&p, seq, clip));
    assert!(properties.contains(&target));
}

fn effect_host_clip(p: &Project, seq: SequenceId, clip: ClipId) -> Clip {
    p.sequence(seq).unwrap().find_clip(clip).unwrap().1.clone()
}
