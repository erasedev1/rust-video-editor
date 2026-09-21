//! What the plan says about a clip's effect chain.
//!
//! The engine is where a chain is *resolved*: parameters are evaluated at the
//! clip's own local time, effects that are switched off or that this build has
//! no program for are dropped, and what reaches the renderer is a flat list of
//! numbers. So the interesting cases here are the ones where an effect does not
//! arrive at all, and the ones where the same chain resolves differently at two
//! instants.

use ve_core::registry::kinds;
use ve_core::{
    builtin_registry, Clip, ClipId, CompositionLayer, CompositionSettings, Effect, EffectId,
    Interpolation, MediaInfo, ParamValue, Project, SequenceId, Size, TrackId, VideoStreamInfo,
};
use ve_engine::{evaluate_composition, evaluate_project, RenderPlan};
use ve_time::{Rate, Ticks};

fn video_info(seconds: i64) -> MediaInfo {
    MediaInfo {
        duration: Ticks::from_seconds(seconds),
        video: Some(VideoStreamInfo {
            size: Size::new(1920, 1080),
            rate: Rate::FPS_30,
            duration: Ticks::from_seconds(seconds),
            frame_count: Some(seconds * 30),
            codec: "h264".into(),
            pixel_format: "yuv420p".into(),
            sample_aspect_ratio: (1, 1),
        }),
        audio: None,
        container: "mp4".into(),
        timecode: None,
    }
}

struct Fixture {
    project: Project,
    sequence: SequenceId,
    v1: TrackId,
    clip: ClipId,
}

/// A ten-second clip on V1, starting at the top of the timeline.
fn fixture() -> Fixture {
    fixture_starting_at(Ticks::ZERO)
}

fn fixture_starting_at(start: Ticks) -> Fixture {
    let mut project = Project::with_default_sequence("Effects");
    let asset = project.add_asset("/media/a.mp4", video_info(60));
    let sequence = project.active_sequence.unwrap();
    let v1 = project.sequence(sequence).unwrap().tracks[0].id;
    let clip = project.new_clip_id();
    project
        .sequence_mut(sequence)
        .unwrap()
        .track_mut(v1)
        .unwrap()
        .insert_clip(Clip::new(
            clip,
            asset,
            "clip",
            Ticks::ZERO,
            start,
            Ticks::from_seconds(10),
        ))
        .unwrap();
    Fixture { project, sequence, v1, clip }
}

impl Fixture {
    fn clip_mut(&mut self) -> &mut Clip {
        self.project
            .sequence_mut(self.sequence)
            .unwrap()
            .track_mut(self.v1)
            .unwrap()
            .clip_mut(self.clip)
            .unwrap()
    }

    fn add(&mut self, kind: &str) -> EffectId {
        let id = self.project.new_effect_id();
        let effect = builtin_registry().instantiate(kind, id).expect("a built-in");
        self.clip_mut().effects.push(effect);
        id
    }

    fn plan_at(&self, seconds: i64) -> RenderPlan {
        let sequence = self.project.sequence(self.sequence).unwrap();
        evaluate_project(&self.project, sequence, Ticks::from_seconds(seconds))
    }
}

fn chain_kinds(plan: &RenderPlan) -> Vec<String> {
    plan.root().items[0].effects.iter().map(|e| e.kind.clone()).collect()
}

#[test]
fn a_clips_chain_reaches_the_plan_in_order() {
    let mut f = fixture();
    f.add(kinds::GAUSSIAN_BLUR);
    f.add(kinds::COLOR_ADJUST);

    let plan = f.plan_at(1);
    assert_eq!(chain_kinds(&plan), vec![kinds::GAUSSIAN_BLUR, kinds::COLOR_ADJUST]);
    assert!(plan.root().items[0].has_effects());
    assert_eq!(plan.root().items[0].effects[0].scalar("radius", 0.0), 8.0);
}

#[test]
fn a_clip_with_no_effects_carries_an_empty_chain() {
    let f = fixture();
    let plan = f.plan_at(1);
    assert!(!plan.root().items[0].has_effects());
    assert!(plan.root().items[0].effects.is_empty());
}

#[test]
fn a_switched_off_effect_does_not_reach_the_plan() {
    let mut f = fixture();
    let blur = f.add(kinds::GAUSSIAN_BLUR);
    f.add(kinds::SHARPEN);
    f.clip_mut().effects.iter_mut().find(|e| e.id == blur).unwrap().enabled = false;

    assert_eq!(chain_kinds(&f.plan_at(1)), vec![kinds::SHARPEN]);
}

#[test]
fn an_effect_this_build_cannot_run_is_skipped_and_the_rest_still_runs() {
    // A project made with a plugin that is not installed. The chain is short by
    // one effect; nothing else about the frame changes, and the effect is still
    // on the clip to be saved back.
    let mut f = fixture();
    let id = f.project.new_effect_id();
    f.clip_mut().effects.push(Effect::new(id, "someone.elses.glow", "Glow"));
    f.add(kinds::COLOR_ADJUST);

    assert_eq!(chain_kinds(&f.plan_at(1)), vec![kinds::COLOR_ADJUST]);
    assert_eq!(f.clip_mut().effects.len(), 2, "nothing was removed from the edit");
}

#[test]
fn an_animated_parameter_resolves_at_the_instant_being_drawn() {
    let mut f = fixture();
    let blur = f.add(kinds::GAUSSIAN_BLUR);
    {
        let clip = f.clip_mut();
        let effect = clip.effects.iter_mut().find(|e| e.id == blur).unwrap();
        let Some(ParamValue::Scalar(radius)) = effect.param_mut("radius") else { panic!() };
        radius.set_keyframe(Ticks::ZERO, 0.0, Interpolation::Linear);
        radius.set_keyframe(Ticks::from_seconds(10), 100.0, Interpolation::Linear);
    }

    let at = |s: i64| f.plan_at(s).root().items[0].effects[0].scalar("radius", -1.0);
    assert!((at(0) - 0.0).abs() < 1e-9);
    assert!((at(5) - 50.0).abs() < 1e-9);
    assert!((at(9) - 90.0).abs() < 1e-9);
}

#[test]
fn an_effects_animation_is_keyed_to_the_clip_rather_than_to_the_timeline() {
    // The same clip, the same keyframes, moved four seconds later: at second
    // six of the timeline it is two seconds into itself, so the parameter has
    // to read what it read at second two before the move.
    let mut f = fixture_starting_at(Ticks::from_seconds(4));
    let blur = f.add(kinds::GAUSSIAN_BLUR);
    {
        let clip = f.clip_mut();
        let effect = clip.effects.iter_mut().find(|e| e.id == blur).unwrap();
        let Some(ParamValue::Scalar(radius)) = effect.param_mut("radius") else { panic!() };
        radius.set_keyframe(Ticks::ZERO, 0.0, Interpolation::Linear);
        radius.set_keyframe(Ticks::from_seconds(10), 100.0, Interpolation::Linear);
    }

    let radius = f.plan_at(6).root().items[0].effects[0].scalar("radius", -1.0);
    assert!((radius - 20.0).abs() < 1e-9, "got {radius}");
}

#[test]
fn a_parameter_is_clamped_to_what_its_control_offers() {
    let mut f = fixture();
    let blur = f.add(kinds::GAUSSIAN_BLUR);
    {
        let clip = f.clip_mut();
        let effect = clip.effects.iter_mut().find(|e| e.id == blur).unwrap();
        *effect.param_mut("radius").unwrap() = ParamValue::scalar(1e9);
    }
    assert_eq!(f.plan_at(1).root().items[0].effects[0].scalar("radius", -1.0), 200.0);
}

#[test]
fn a_composition_layers_chain_reaches_the_plan_too() {
    let mut f = fixture();
    let asset = f.project.assets[0].id;
    let comp = f.project.add_composition(
        "Titles",
        CompositionSettings { duration: Ticks::from_seconds(10), ..Default::default() },
    );
    let layer_id = f.project.new_layer_id();
    let mut layer =
        CompositionLayer::new(layer_id, "layer", asset, Ticks::ZERO, Ticks::from_seconds(5));
    let effect_id = f.project.new_effect_id();
    layer.effects.push(builtin_registry().instantiate(kinds::SHAPE_MASK, effect_id).unwrap());
    f.project.composition_mut(comp).unwrap().push_layer(layer);

    let plan = evaluate_composition(&f.project, comp, Ticks::from_seconds(1));
    let item = &plan.root().items[0];
    assert_eq!(item.effects.len(), 1);
    assert_eq!(item.effects[0].kind, kinds::SHAPE_MASK);
    // A choice arrives as the index the registry declared, not as a number the
    // shader has to guess at.
    assert_eq!(item.effects[0].choice("shape", 99), 1);
}
