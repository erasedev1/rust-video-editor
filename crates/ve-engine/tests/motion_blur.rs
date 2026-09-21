//! What the plan says about a moving layer: the transform resolved once, or
//! once per instant the shutter is open.
//!
//! The engine is where motion blur is *decided* — the renderer only averages
//! what it is handed — so the interesting cases are the ones where the answer is
//! "no samples at all": a still layer, a closed shutter, a clip that never asked
//! to be blurred, and a clip that is animated but is not moving during this
//! particular frame.

use ve_core::{
    Clip, ClipId, Interpolation, MediaInfo, MotionBlur, Project, SequenceId, Size, TrackId,
    Vec2, VideoStreamInfo,
};
use ve_engine::evaluate_project;
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

/// A ten-second clip on V1, unanimated, with the default shutter.
fn fixture() -> Fixture {
    let mut project = Project::with_default_sequence("Blur");
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
            Ticks::ZERO,
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

    /// Animates the clip's position from the origin to `to` over `seconds`.
    fn animate(&mut self, to: Vec2, seconds: i64) {
        let clip = self.clip_mut();
        clip.transform.position.set_keyframe(Ticks::ZERO, Vec2::ZERO, Interpolation::Linear);
        clip.transform.position.set_keyframe(
            Ticks::from_seconds(seconds),
            to,
            Interpolation::Linear,
        );
    }

    fn set_shutter(&mut self, blur: MotionBlur) {
        self.project.sequence_mut(self.sequence).unwrap().settings.motion_blur = blur;
    }

    /// The samples the plan resolved for the clip at `at`.
    fn samples_at(&self, at: Ticks) -> Vec<ve_core::TransformState> {
        let sequence = self.project.sequence(self.sequence).unwrap();
        let plan = evaluate_project(&self.project, sequence, at);
        plan.root().items.first().map(|i| i.samples.clone()).unwrap_or_default()
    }
}

#[test]
fn a_moving_clip_with_blur_on_is_sampled_across_the_shutter() {
    let mut f = fixture();
    f.animate(Vec2::new(300.0, 0.0), 10);
    f.clip_mut().motion_blur = true;
    f.set_shutter(MotionBlur::new(180.0, 8));

    let samples = f.samples_at(Ticks::from_seconds(5));
    assert_eq!(samples.len(), 8);

    // The samples straddle the frame: the layer's position at the instant the
    // frame is drawn sits between the first and the last.
    let at_frame = 300.0 / 10.0 * 5.0;
    assert!(samples[0].position.x < at_frame, "{:?}", samples[0]);
    assert!(samples[7].position.x > at_frame, "{:?}", samples[7]);

    // Half a frame at 30fps, moving 30 px a second: the smear is about half a
    // pixel long, which is what a shutter is supposed to give.
    let span = samples[7].position.x - samples[0].position.x;
    assert!((span - 0.4375).abs() < 1e-6, "smear was {span} px");

    // And they are in order, so the average is over a path rather than a cloud.
    assert!(samples.windows(2).all(|p| p[0].position.x < p[1].position.x));
}

#[test]
fn a_clip_that_never_asked_for_blur_is_drawn_once() {
    let mut f = fixture();
    f.animate(Vec2::new(300.0, 0.0), 10);
    assert!(f.samples_at(Ticks::from_seconds(5)).is_empty());
}

#[test]
fn a_closed_shutter_turns_blur_off_across_the_whole_sequence() {
    let mut f = fixture();
    f.animate(Vec2::new(300.0, 0.0), 10);
    f.clip_mut().motion_blur = true;

    f.set_shutter(MotionBlur { enabled: false, ..MotionBlur::default() });
    assert!(
        f.samples_at(Ticks::from_seconds(5)).is_empty(),
        "the canvas switch has to win over the clip's"
    );

    f.set_shutter(MotionBlur::new(0.0, 8));
    assert!(f.samples_at(Ticks::from_seconds(5)).is_empty(), "a shutter that never opens");
}

#[test]
fn a_clip_standing_still_this_frame_costs_nothing_even_though_it_is_animated() {
    let mut f = fixture();
    // Keyframed, but both keyframes hold the same place: the clip is animated
    // and yet nothing moves, so there is nothing to smear.
    {
        let clip = f.clip_mut();
        clip.transform.position.set_keyframe(
            Ticks::ZERO,
            Vec2::new(10.0, 10.0),
            Interpolation::Linear,
        );
        clip.transform.position.set_keyframe(
            Ticks::from_seconds(10),
            Vec2::new(10.0, 10.0),
            Interpolation::Linear,
        );
        clip.motion_blur = true;
    }
    assert!(f.samples_at(Ticks::from_seconds(5)).is_empty());

    // Held keyframes are the same story: the value steps at a keyframe and is
    // still in between, so a frame that is not the step costs one draw.
    {
        let clip = f.clip_mut();
        clip.transform.position.set_keyframe(Ticks::ZERO, Vec2::ZERO, Interpolation::Hold);
        clip.transform.position.set_keyframe(
            Ticks::from_seconds(10),
            Vec2::new(300.0, 0.0),
            Interpolation::Hold,
        );
    }
    assert!(f.samples_at(Ticks::from_seconds(5)).is_empty());
}

#[test]
fn an_unanimated_clip_is_never_blurred_however_the_switches_are_set() {
    let mut f = fixture();
    f.clip_mut().motion_blur = true;
    f.clip_mut().transform.position.value = Vec2::new(50.0, 0.0);
    f.set_shutter(MotionBlur::new(360.0, 16));
    assert!(f.samples_at(Ticks::from_seconds(5)).is_empty());
}

#[test]
fn a_wider_shutter_makes_a_longer_smear() {
    let mut f = fixture();
    f.animate(Vec2::new(3000.0, 0.0), 10);
    f.clip_mut().motion_blur = true;

    let span = |f: &Fixture| {
        let s = f.samples_at(Ticks::from_seconds(5));
        s.last().unwrap().position.x - s[0].position.x
    };

    f.set_shutter(MotionBlur::new(90.0, 8));
    let narrow = span(&f);
    f.set_shutter(MotionBlur::new(360.0, 8));
    let wide = span(&f);

    assert!(wide > narrow * 3.5, "{wide} should be about four times {narrow}");
}

#[test]
fn a_composition_is_exposed_through_its_own_shutter() {
    let mut f = fixture();
    let comp = f.project.add_composition(
        "Title",
        ve_core::CompositionSettings {
            resolution: Size::new(640, 360),
            motion_blur: MotionBlur::new(360.0, 4),
            ..Default::default()
        },
    );
    let layer_id = f.project.new_layer_id();
    let asset = f.project.assets[0].id;
    let mut layer = ve_core::CompositionLayer::new(
        layer_id,
        "moving",
        asset,
        Ticks::ZERO,
        Ticks::from_seconds(10),
    );
    layer.transform.position.set_keyframe(Ticks::ZERO, Vec2::ZERO, Interpolation::Linear);
    layer.transform.position.set_keyframe(
        Ticks::from_seconds(10),
        Vec2::new(300.0, 0.0),
        Interpolation::Linear,
    );
    layer.motion_blur = true;
    f.project.composition_mut(comp).unwrap().push_layer(layer);

    // The sequence's own shutter is deliberately different, and does not apply
    // inside the composition.
    f.set_shutter(MotionBlur::new(90.0, 16));
    f.clip_mut().source = ve_core::Source::Composition(comp);

    let sequence = f.project.sequence(f.sequence).unwrap();
    let plan = evaluate_project(&f.project, sequence, Ticks::from_seconds(5));
    let nested = &plan.nodes[0];
    assert_eq!(nested.composition, Some(comp));
    assert_eq!(
        nested.items[0].samples.len(),
        4,
        "the composition's four samples, not the sequence's sixteen"
    );
    // And the clip holding it is not blurred: it is not moving.
    assert!(plan.root().items[0].samples.is_empty());
}
