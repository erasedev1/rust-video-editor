//! Nested compositions, as the engine sees them: one instant becomes a tree of
//! nodes, flattened into render order.

use ve_core::{
    AssetId, BlendMode, Clip, CompositionId, CompositionLayer, CompositionSettings, MediaInfo,
    Project, SequenceId, Size, Source, TrackId, VideoStreamInfo,
};
use ve_engine::*;
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
        audio: Some(ve_core::AudioStreamInfo {
            sample_rate: ve_time::SampleRate::HZ_48000,
            channels: 2,
            duration: Ticks::from_seconds(seconds),
            codec: "aac".into(),
        }),
        container: "mp4".into(),
        timecode: None,
    }
}

struct Fixture {
    project: Project,
    sequence: SequenceId,
    v1: TrackId,
    a1: TrackId,
    asset: AssetId,
}

fn fixture() -> Fixture {
    let mut project = Project::with_default_sequence("Nesting");
    let asset = project.add_asset("/media/a.mp4", video_info(60));
    let sequence = project.active_sequence.unwrap();
    let s = project.sequence(sequence).unwrap();
    let (v1, a1) = (s.tracks[0].id, s.tracks[2].id);
    Fixture { project, sequence, v1, a1, asset }
}

impl Fixture {
    fn sequence(&self) -> &ve_core::Sequence {
        self.project.sequence(self.sequence).unwrap()
    }

    /// A composition of `size`, ten seconds long.
    fn composition(&mut self, name: &str, size: Size) -> CompositionId {
        self.project.add_composition(
            name,
            CompositionSettings { resolution: size, ..CompositionSettings::default() },
        )
    }

    /// Adds a layer to a composition and returns its id.
    fn layer(
        &mut self,
        comp: CompositionId,
        source: impl Into<Source>,
        start_s: i64,
        dur_s: i64,
    ) -> ve_core::LayerId {
        let id = self.project.new_layer_id();
        let layer = CompositionLayer::new(
            id,
            "layer",
            source,
            Ticks::from_seconds(start_s),
            Ticks::from_seconds(dur_s),
        );
        self.project.composition_mut(comp).unwrap().push_layer(layer);
        id
    }

    /// Puts a source on the sequence's first video track.
    fn clip(&mut self, source: impl Into<Source>, start_s: i64, dur_s: i64) -> ve_core::ClipId {
        let id = self.project.new_clip_id();
        let clip = Clip::new(
            id,
            source,
            "clip",
            Ticks::ZERO,
            Ticks::from_seconds(start_s),
            Ticks::from_seconds(dur_s),
        );
        self.project
            .sequence_mut(self.sequence)
            .unwrap()
            .track_mut(self.v1)
            .unwrap()
            .insert_clip(clip)
            .unwrap();
        id
    }

    fn audio_clip(&mut self, source: impl Into<Source>, start_s: i64, dur_s: i64) {
        let id = self.project.new_clip_id();
        let clip = Clip::new(
            id,
            source,
            "sound",
            Ticks::ZERO,
            Ticks::from_seconds(start_s),
            Ticks::from_seconds(dur_s),
        );
        self.project
            .sequence_mut(self.sequence)
            .unwrap()
            .track_mut(self.a1)
            .unwrap()
            .insert_clip(clip)
            .unwrap();
    }

    fn plan(&self, at_s: i64) -> RenderPlan {
        evaluate_project(&self.project, self.sequence(), Ticks::from_seconds(at_s))
    }
}

#[test]
fn a_clip_holding_a_composition_becomes_two_nodes_child_first() {
    let mut f = fixture();
    let comp = f.composition("Title", Size::new(640, 360));
    f.layer(comp, f.asset, 0, 10);
    f.clip(comp, 0, 5);

    let plan = f.plan(1);
    assert_eq!(plan.nodes.len(), 2, "one for the composition, one for the sequence");
    assert_eq!(plan.nested_count(), 1);

    // The child comes first, which is what lets a renderer walk the list once.
    let child = &plan.nodes[0];
    assert_eq!(child.composition, Some(comp));
    assert_eq!(child.size, Size::new(640, 360), "a composition keeps its own canvas");
    assert_eq!(child.items.len(), 1);

    let root = plan.root();
    assert_eq!(root.composition, None);
    assert_eq!(root.size, Size::new(1920, 1080));
    assert_eq!(root.items.len(), 1);
    assert_eq!(root.items[0].draw, Draw::Nested { composition: comp, node: 0 });
}

#[test]
fn a_nested_composition_is_evaluated_at_its_own_time() {
    let mut f = fixture();
    let comp = f.composition("Title", Size::new(640, 360));
    // Two layers, one after the other inside the composition.
    let first = f.layer(comp, f.asset, 0, 2);
    let second = f.layer(comp, f.asset, 2, 2);

    // The clip starts at 10s on the timeline and reads the composition from 2s
    // in, so the sequence's 11s is the composition's 3s.
    let clip_id = f.clip(comp, 10, 4);
    {
        let (_, clip) =
            f.project.sequence_mut(f.sequence).unwrap().find_clip_mut(clip_id).unwrap();
        clip.source_in = Ticks::from_seconds(2);
    }

    let plan = f.plan(11);
    let child = &plan.nodes[0];
    assert_eq!(child.time, Ticks::from_seconds(3), "the layer's source time, not the playhead");
    assert_eq!(child.items.len(), 1);
    assert_eq!(child.items[0].origin, Origin::Layer(second), "the layer covering 3s");
    assert_ne!(child.items[0].origin, Origin::Layer(first));
}

#[test]
fn nesting_three_deep_produces_three_nodes_in_render_order() {
    let mut f = fixture();
    let inner = f.composition("Inner", Size::new(320, 180));
    let middle = f.composition("Middle", Size::new(640, 360));
    f.layer(inner, f.asset, 0, 10);
    f.layer(middle, inner, 0, 10);
    f.clip(middle, 0, 5);

    let plan = f.plan(1);
    assert_eq!(plan.nodes.len(), 3);
    // Deepest first: every node's children come before it, so walking the list
    // front to back means nothing samples a target that has not been drawn.
    assert_eq!(plan.nodes[0].composition, Some(inner));
    assert_eq!(plan.nodes[1].composition, Some(middle));
    assert_eq!(plan.nodes[2].composition, None);

    assert_eq!(plan.nodes[1].items[0].draw, Draw::Nested { composition: inner, node: 0 });
    assert_eq!(plan.nodes[2].items[0].draw, Draw::Nested { composition: middle, node: 1 });
}

#[test]
fn one_composition_used_twice_is_evaluated_once_per_use() {
    let mut f = fixture();
    let comp = f.composition("Title", Size::new(640, 360));
    f.layer(comp, f.asset, 0, 10);
    f.clip(comp, 0, 5);
    f.clip(comp, 5, 5);

    // At an instant covered by only one of the two clips there is one nested
    // node; the other clip is not on screen, so its composition is not
    // evaluated at all.
    let plan = f.plan(1);
    assert_eq!(plan.nested_count(), 1);

    // Both clips are on the same track, so they cannot overlap — but a second
    // track can hold the same composition at the same instant, and then it is
    // evaluated twice, because the two uses can be at different times.
    let v2 = f.project.sequence(f.sequence).unwrap().tracks[1].id;
    let id = f.project.new_clip_id();
    let clip = Clip::new(id, comp, "again", Ticks::ZERO, Ticks::ZERO, Ticks::from_seconds(5));
    f.project
        .sequence_mut(f.sequence)
        .unwrap()
        .track_mut(v2)
        .unwrap()
        .insert_clip(clip)
        .unwrap();

    let plan = f.plan(1);
    assert_eq!(plan.nested_count(), 2, "two uses, two nodes");
    assert_eq!(plan.root().items.len(), 2);
}

#[test]
fn a_composition_is_only_reachable_through_the_project() {
    let mut f = fixture();
    let comp = f.composition("Title", Size::new(640, 360));
    f.layer(comp, f.asset, 0, 10);
    f.clip(comp, 0, 5);

    // `evaluate` takes a sequence alone, which cannot resolve a composition —
    // compositions live on the project. It says so by leaving the clip out
    // rather than by pretending the clip is media.
    let alone = evaluate(f.sequence(), Ticks::from_seconds(1));
    assert_eq!(alone.nodes.len(), 1);
    assert!(alone.root().items.is_empty());

    let with_project = f.plan(1);
    assert_eq!(with_project.nested_count(), 1);
}

#[test]
fn the_layers_transform_and_blend_mode_travel_with_the_nested_node() {
    let mut f = fixture();
    let comp = f.composition("Title", Size::new(640, 360));
    let layer = f.layer(comp, f.asset, 0, 10);
    {
        let l = f.project.composition_mut(comp).unwrap().layer_mut(layer).unwrap();
        l.blend = BlendMode::Screen;
        l.transform.opacity.value = 0.5;
    }
    let clip_id = f.clip(comp, 0, 5);
    {
        let (_, clip) =
            f.project.sequence_mut(f.sequence).unwrap().find_clip_mut(clip_id).unwrap();
        clip.blend = BlendMode::Add;
        clip.transform.opacity.value = 0.25;
    }

    let plan = f.plan(1);
    // The layer inside the composition composites against the composition's own
    // canvas; the clip holding it composites against the sequence.
    assert_eq!(plan.nodes[0].items[0].blend, BlendMode::Screen);
    assert_eq!(plan.nodes[0].items[0].transform.opacity, 0.5);
    assert_eq!(plan.root().items[0].blend, BlendMode::Add);
    assert_eq!(plan.root().items[0].transform.opacity, 0.25);
}

#[test]
fn every_asset_in_the_tree_is_reported_as_required() {
    let mut f = fixture();
    let second = f.project.add_asset("/media/b.mp4", video_info(60));
    let comp = f.composition("Title", Size::new(640, 360));
    f.layer(comp, second, 0, 10);
    f.clip(comp, 0, 5);

    // The nested composition's media has to be decoded too, or the composite
    // would be a hole. What is *not* in the list is the composition itself: it
    // is rendered, not decoded.
    let plan = f.plan(1);
    assert_eq!(plan.required_assets(), vec![second]);
    assert_eq!(plan.item_count(), 2, "one nested item plus the clip holding it");
}

#[test]
fn nesting_deeper_than_the_limit_stops_rather_than_descending_forever() {
    let mut f = fixture();
    // A chain longer than the limit. Cycles are impossible by construction, so
    // this is about cost, not termination: every level is another render target.
    let mut previous: Option<CompositionId> = None;
    let mut deepest = None;
    for i in 0..(MAX_NESTING_DEPTH + 4) {
        let comp = f.composition(&format!("Level {i}"), Size::new(320, 180));
        match previous {
            Some(inner) => {
                f.layer(comp, inner, 0, 10);
            }
            None => {
                f.layer(comp, f.asset, 0, 10);
            }
        }
        previous = Some(comp);
        deepest = Some(comp);
    }
    f.clip(deepest.unwrap(), 0, 5);

    let plan = f.plan(1);
    // The chain is cut at the limit, and what is above it still draws.
    assert!(
        plan.nodes.len() <= MAX_NESTING_DEPTH + 1,
        "descended {} levels, past the limit of {MAX_NESTING_DEPTH}",
        plan.nodes.len()
    );
    assert!(!plan.root().items.is_empty(), "the outermost composition still draws");
}

// ---- audio --------------------------------------------------------------

#[test]
fn a_nested_compositions_sound_is_lifted_into_the_mix() {
    let mut f = fixture();
    let comp = f.composition("Title", Size::new(640, 360));
    let layer = f.layer(comp, f.asset, 0, 10);
    f.project.composition_mut(comp).unwrap().layer_mut(layer).unwrap().audio.volume.value = 0.5;
    f.audio_clip(comp, 0, 5);
    {
        let seq = f.project.sequence_mut(f.sequence).unwrap();
        let clip_id = seq.track(f.a1).unwrap().clips()[0].id;
        let (_, clip) = seq.find_clip_mut(clip_id).unwrap();
        clip.audio.volume.value = 0.5;
    }

    let plan = f.plan(1);
    assert_eq!(plan.audio.len(), 1, "the nested layer's sound reaches the mixer");
    // Gain multiplies down the chain: the clip's half times the layer's half.
    assert!((plan.audio[0].gain - 0.25).abs() < 1e-9, "gain was {}", plan.audio[0].gain);
    assert_eq!(plan.audio[0].asset, f.asset, "media, not the composition");
}

#[test]
fn a_nested_layers_own_audio_also_reaches_the_mix_from_the_picture_side() {
    let mut f = fixture();
    let comp = f.composition("Title", Size::new(640, 360));
    f.layer(comp, f.asset, 0, 10);
    // The composition is on a *video* track. Its layers still have sound, and a
    // nested composite that went silent when cut into the picture would be a
    // surprise.
    f.clip(comp, 0, 5);

    let plan = f.plan(1);
    assert_eq!(plan.audio.len(), 1);
    assert!((plan.audio[0].gain - 1.0).abs() < 1e-9);
}

#[test]
fn fading_a_nested_composition_out_takes_its_sound_with_it() {
    let mut f = fixture();
    let comp = f.composition("Title", Size::new(640, 360));
    f.layer(comp, f.asset, 0, 10);
    let clip_id = f.clip(comp, 0, 5);
    {
        let (_, clip) =
            f.project.sequence_mut(f.sequence).unwrap().find_clip_mut(clip_id).unwrap();
        clip.transform.opacity.value = 0.0;
    }

    let plan = f.plan(1);
    assert_eq!(plan.audio.len(), 1);
    assert_eq!(plan.audio[0].gain, 0.0, "invisible and inaudible together");
}

// ---- previewing a composition on its own -------------------------------

#[test]
fn opening_a_composition_makes_it_the_root() {
    let mut f = fixture();
    let comp = f.composition("Title", Size::new(640, 360));
    f.layer(comp, f.asset, 0, 10);
    f.clip(comp, 0, 5);

    let plan = evaluate_composition(&f.project, comp, Ticks::from_seconds(1));
    assert_eq!(plan.nodes.len(), 1, "no sequence above it");
    assert_eq!(plan.root().composition, Some(comp));
    assert_eq!(plan.size(), Size::new(640, 360), "at the composition's own size");
    assert_eq!(plan.root().items.len(), 1);
}

#[test]
fn opening_a_composition_that_nests_another_still_descends() {
    let mut f = fixture();
    let inner = f.composition("Inner", Size::new(320, 180));
    let outer = f.composition("Outer", Size::new(640, 360));
    f.layer(inner, f.asset, 0, 10);
    f.layer(outer, inner, 0, 10);

    let plan = evaluate_composition(&f.project, outer, Ticks::from_seconds(1));
    assert_eq!(plan.nodes.len(), 2);
    assert_eq!(plan.nodes[0].composition, Some(inner));
    assert_eq!(plan.root().composition, Some(outer));
}

#[test]
fn opening_a_composition_that_does_not_exist_gives_an_empty_plan() {
    let f = fixture();
    let plan = evaluate_composition(&f.project, CompositionId::from_raw(999), Ticks::ZERO);
    assert_eq!(plan.nodes.len(), 1);
    assert!(plan.is_empty());
}

#[test]
fn the_transport_reads_its_timebase_from_whatever_is_open() {
    let mut f = fixture();
    let comp = f.project.add_composition(
        "Title",
        CompositionSettings {
            resolution: Size::new(640, 360),
            rate: Rate::FPS_24,
            duration: Ticks::from_seconds(7),
            ..CompositionSettings::default()
        },
    );
    f.clip(f.asset, 0, 30);

    let sequence = Viewing::Sequence(f.sequence).timebase(&f.project).unwrap();
    assert_eq!(sequence.rate, Rate::FPS_30);
    assert_eq!(sequence.duration, Ticks::from_seconds(30));
    assert_eq!(sequence.size, Size::new(1920, 1080));

    // A composition has its own frame grid and length, so playing one plays it
    // rather than the sequence it happens to be used in.
    let composition = Viewing::Composition(comp).timebase(&f.project).unwrap();
    assert_eq!(composition.rate, Rate::FPS_24);
    assert_eq!(composition.duration, Ticks::from_seconds(7));
    assert_eq!(composition.size, Size::new(640, 360));

    assert!(Viewing::Composition(CompositionId::from_raw(999)).timebase(&f.project).is_none());
}
