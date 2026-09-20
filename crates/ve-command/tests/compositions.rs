//! Undo and redo for compositions: the same guarantee cutting gets, which is
//! that after undoing an edit the project is *exactly* what it was.

use ve_command::*;
use ve_core::{
    BlendMode, Clip, ClipId, CompositionId, CompositionLayer, CompositionSettings, CoreError,
    LayerId, MediaInfo, Project, SequenceId, Size, Source, TrackId, VideoStreamInfo,
};
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
    }
}

struct Fixture {
    project: Project,
    sequence: SequenceId,
    track: TrackId,
    asset: ve_core::AssetId,
}

fn fixture() -> Fixture {
    let mut project = Project::with_default_sequence("Comp Commands");
    let asset = project.add_asset("/media/a.mp4", video_info(60));
    let sequence = project.active_sequence.unwrap();
    let track = project.sequence(sequence).unwrap().tracks[0].id;
    Fixture { project, sequence, track, asset }
}

impl Fixture {
    fn add_clip(&mut self, start_s: i64, dur_s: i64) -> ClipId {
        let id = self.project.new_clip_id();
        let clip = Clip::new(
            id,
            self.asset,
            format!("clip@{start_s}"),
            Ticks::ZERO,
            Ticks::from_seconds(start_s),
            Ticks::from_seconds(dur_s),
        );
        self.project
            .sequence_mut(self.sequence)
            .unwrap()
            .track_mut(self.track)
            .unwrap()
            .insert_clip(clip)
            .unwrap();
        id
    }

    fn composition(&mut self, history: &mut History) -> CompositionId {
        let mut command = AddComposition::new("Titles", CompositionSettings::default());
        command.apply(&mut self.project).unwrap();
        let id = command.composition_id().unwrap();
        // Put it in the history the way the action layer would, so undoing the
        // test's setup is possible where a test wants that.
        history
            .execute(&mut self.project, Box::new(RenameComposition::new(id, "Titles")))
            .unwrap();
        id
    }

    /// A three-second layer, not yet added to anything.
    fn layer(&mut self, source: impl Into<Source>) -> CompositionLayer {
        let id = self.project.new_layer_id();
        CompositionLayer::new(id, "layer", source, Ticks::ZERO, Ticks::from_seconds(3))
    }
}

#[test]
fn creating_a_composition_undoes_and_redoes_to_the_same_id() {
    let mut f = fixture();
    let before = f.project.clone();
    let mut h = History::default();

    let mut command = AddComposition::new("Titles", CompositionSettings::default());
    command.apply(&mut f.project).unwrap();
    let id = command.composition_id().unwrap();
    h.execute(&mut f.project, Box::new(RemoveComposition::new(id))).unwrap();
    assert_eq!(f.project.compositions.len(), 0);

    // Redoing the removal's undo puts back the same composition, which matters
    // because a clip may already reference that ID.
    h.undo(&mut f.project).unwrap();
    assert_eq!(f.project.compositions.len(), 1);
    assert_eq!(f.project.compositions[0].id, id);

    h.redo(&mut f.project).unwrap();
    assert_eq!(f.project.compositions.len(), 0);
    // The allocator has moved on, so `before` is not equality-comparable; the
    // shape is what matters here.
    assert_eq!(f.project.sequences, before.sequences);
}

#[test]
fn a_new_composition_is_removed_again_by_undo() {
    let mut f = fixture();
    let mut h = History::default();
    h.execute(
        &mut f.project,
        Box::new(AddComposition::new("Titles", CompositionSettings::default())),
    )
    .unwrap();
    assert_eq!(f.project.compositions.len(), 1);

    h.undo(&mut f.project).unwrap();
    assert!(f.project.compositions.is_empty(), "undo takes it away again");

    h.redo(&mut f.project).unwrap();
    assert_eq!(f.project.compositions.len(), 1, "and redo brings it back");
}

#[test]
fn a_composition_in_use_cannot_be_deleted() {
    let mut f = fixture();
    let mut h = History::default();
    let comp = f.composition(&mut h);

    let clip = composition_clip(&mut f.project, comp, Ticks::ZERO).unwrap();
    h.execute(&mut f.project, Box::new(AddClip::new(f.sequence, f.track, clip))).unwrap();

    match h.execute(&mut f.project, Box::new(RemoveComposition::new(comp))) {
        Err(CommandError::Core(CoreError::CompositionInUse { places, .. })) => {
            assert_eq!(places, 1)
        }
        other => panic!("expected a refusal, got {other:?}"),
    }
    assert_eq!(f.project.compositions.len(), 1, "and the composition is still there");
}

#[test]
fn a_deleted_composition_comes_back_at_its_old_position() {
    let mut f = fixture();
    let mut h = History::default();
    let first = f.composition(&mut h);
    let mut second = AddComposition::new("Second", CompositionSettings::default());
    second.apply(&mut f.project).unwrap();
    let mut third = AddComposition::new("Third", CompositionSettings::default());
    third.apply(&mut f.project).unwrap();
    let before = f.project.compositions.clone();

    h.execute(
        &mut f.project,
        Box::new(RemoveComposition::new(second.composition_id().unwrap())),
    )
    .unwrap();
    assert_eq!(f.project.compositions.len(), 2);

    h.undo(&mut f.project).unwrap();
    assert_eq!(f.project.compositions, before, "order restored, not appended");
    assert_eq!(f.project.compositions[0].id, first);
}

#[test]
fn adding_and_removing_layers_undoes_exactly() {
    let mut f = fixture();
    let mut h = History::default();
    let comp = f.composition(&mut h);
    let before = f.project.clone();

    let layer = f.layer(f.asset);
    let layer_id = layer.id;
    h.execute(&mut f.project, Box::new(AddLayer::new(comp, layer))).unwrap();
    assert_eq!(f.project.composition(comp).unwrap().layers.len(), 1);

    h.undo(&mut f.project).unwrap();
    assert_eq!(f.project.composition(comp).unwrap().layers.len(), 0);
    h.redo(&mut f.project).unwrap();
    assert_eq!(f.project.composition(comp).unwrap().layer(layer_id).unwrap().id, layer_id);

    // And removing it puts the project back where it started, layers and all.
    h.execute(&mut f.project, Box::new(RemoveLayer::new(comp, layer_id))).unwrap();
    assert_eq!(f.project.composition(comp).unwrap().layers.len(), 0);
    h.undo(&mut f.project).unwrap();
    h.undo(&mut f.project).unwrap();
    assert_eq!(f.project.compositions, before.compositions);
}

#[test]
fn a_removed_layer_returns_to_its_own_depth_in_the_stack() {
    let mut f = fixture();
    let mut h = History::default();
    let comp = f.composition(&mut h);
    let mut ids = Vec::new();
    for _ in 0..3 {
        let layer = f.layer(f.asset);
        ids.push(layer.id);
        h.execute(&mut f.project, Box::new(AddLayer::new(comp, layer))).unwrap();
    }

    // Remove the middle one. Putting it back on top would silently reorder the
    // composite, which is a different picture.
    h.execute(&mut f.project, Box::new(RemoveLayer::new(comp, ids[1]))).unwrap();
    h.undo(&mut f.project).unwrap();
    let order: Vec<LayerId> =
        f.project.composition(comp).unwrap().layers.iter().map(|l| l.id).collect();
    assert_eq!(order, ids);
}

#[test]
fn a_layer_that_would_close_a_nesting_cycle_is_refused() {
    let mut f = fixture();
    let mut h = History::default();
    let outer = f.composition(&mut h);
    let mut inner_cmd = AddComposition::new("Inner", CompositionSettings::default());
    inner_cmd.apply(&mut f.project).unwrap();
    let inner = inner_cmd.composition_id().unwrap();

    // outer contains inner.
    let layer = f.layer(inner);
    h.execute(&mut f.project, Box::new(AddLayer::new(outer, layer))).unwrap();

    // inner containing outer would close the loop.
    let layer = f.layer(outer);
    match h.execute(&mut f.project, Box::new(AddLayer::new(inner, layer))) {
        Err(CommandError::Core(CoreError::CompositionCycle)) => {}
        other => panic!("expected a cycle refusal, got {other:?}"),
    }
    assert!(f.project.composition(inner).unwrap().layers.is_empty());

    // A composition cannot hold itself either.
    let layer = f.layer(inner);
    assert!(h.execute(&mut f.project, Box::new(AddLayer::new(inner, layer))).is_err());
    assert!(f.project.composition(inner).unwrap().layers.is_empty());
}

#[test]
fn reordering_a_layer_merges_into_one_undo_step_per_drag() {
    let mut f = fixture();
    let mut h = History::default();
    let comp = f.composition(&mut h);
    let mut ids = Vec::new();
    for _ in 0..3 {
        let layer = f.layer(f.asset);
        ids.push(layer.id);
        h.execute(&mut f.project, Box::new(AddLayer::new(comp, layer))).unwrap();
    }
    let depth = h.undo_depth();

    // Dragging the bottom layer up through the stack, one position at a time.
    for to in 1..=2 {
        h.execute_coalesced(&mut f.project, Box::new(MoveLayer::new(comp, ids[0], to)))
            .unwrap();
    }
    assert_eq!(h.undo_depth(), depth + 1, "one gesture, one entry");
    let order: Vec<LayerId> =
        f.project.composition(comp).unwrap().layers.iter().map(|l| l.id).collect();
    assert_eq!(order, vec![ids[1], ids[2], ids[0]]);

    h.undo(&mut f.project).unwrap();
    let order: Vec<LayerId> =
        f.project.composition(comp).unwrap().layers.iter().map(|l| l.id).collect();
    assert_eq!(order, ids, "back to where the drag started, not one step back");
}

#[test]
fn moving_and_trimming_a_layer_merge_per_gesture_and_undo_whole() {
    let mut f = fixture();
    let mut h = History::default();
    let comp = f.composition(&mut h);
    let mut layer = f.layer(f.asset);
    layer.start = Ticks::from_seconds(1);
    layer.duration = Ticks::from_seconds(4);
    layer.source_in = Ticks::from_seconds(5);
    let layer_id = layer.id;
    h.execute(&mut f.project, Box::new(AddLayer::new(comp, layer))).unwrap();
    let before = f.project.composition(comp).unwrap().layer(layer_id).unwrap().clone();

    for at in [2, 3, 4] {
        h.execute_coalesced(
            &mut f.project,
            Box::new(MoveLayerInTime::new(comp, layer_id, Ticks::from_seconds(at))),
        )
        .unwrap();
    }
    let moved = f.project.composition(comp).unwrap().layer(layer_id).unwrap();
    assert_eq!(moved.start, Ticks::from_seconds(4));
    assert_eq!(moved.source_in, before.source_in, "moving does not change the frames shown");

    h.undo(&mut f.project).unwrap();
    assert_eq!(
        f.project.composition(comp).unwrap().layer(layer_id).unwrap().start,
        before.start
    );

    // A head trim rolls the source window with it, and undo restores both.
    h.break_merge();
    h.execute_coalesced(
        &mut f.project,
        Box::new(TrimLayer::new(comp, layer_id, LayerEdge::Start, Ticks::from_seconds(2))),
    )
    .unwrap();
    let trimmed = f.project.composition(comp).unwrap().layer(layer_id).unwrap();
    assert_eq!(trimmed.start, Ticks::from_seconds(2));
    assert_eq!(trimmed.duration, Ticks::from_seconds(3));
    assert_eq!(trimmed.source_in, Ticks::from_seconds(6), "the window rolled with the head");

    h.undo(&mut f.project).unwrap();
    assert_eq!(f.project.composition(comp).unwrap().layer(layer_id).unwrap(), &before);
}

#[test]
fn a_trim_past_the_end_of_a_nested_composition_is_refused() {
    let mut f = fixture();
    let mut h = History::default();
    let comp = f.composition(&mut h);
    let mut inner_cmd = AddComposition::new("Inner", CompositionSettings::default());
    inner_cmd.apply(&mut f.project).unwrap();
    let inner = inner_cmd.composition_id().unwrap();
    // The inner composition is ten seconds long by default.
    let layer = f.layer(inner);
    let layer_id = layer.id;
    h.execute(&mut f.project, Box::new(AddLayer::new(comp, layer))).unwrap();

    // Eight seconds fits inside ten.
    h.execute(
        &mut f.project,
        Box::new(TrimLayer::new(comp, layer_id, LayerEdge::End, Ticks::from_seconds(8))),
    )
    .unwrap();
    // Twelve does not: a composition's own duration bounds a trim exactly the
    // way a file's length does.
    match h.execute(
        &mut f.project,
        Box::new(TrimLayer::new(comp, layer_id, LayerEdge::End, Ticks::from_seconds(12))),
    ) {
        Err(CommandError::Core(CoreError::TrimBeyondSource)) => {}
        other => panic!("expected TrimBeyondSource, got {other:?}"),
    }
    assert_eq!(
        f.project.composition(comp).unwrap().layer(layer_id).unwrap().duration,
        Ticks::from_seconds(8)
    );
}

#[test]
fn layer_flags_blend_modes_and_properties_all_undo() {
    let mut f = fixture();
    let mut h = History::default();
    let comp = f.composition(&mut h);
    let layer = f.layer(f.asset);
    let layer_id = layer.id;
    h.execute(&mut f.project, Box::new(AddLayer::new(comp, layer))).unwrap();
    let before = f.project.composition(comp).unwrap().layer(layer_id).unwrap().clone();

    h.execute(
        &mut f.project,
        Box::new(SetLayerFlag::new(comp, layer_id, LayerFlag::Enabled, false)),
    )
    .unwrap();
    h.execute(
        &mut f.project,
        Box::new(SetLayerFlag::new(comp, layer_id, LayerFlag::Solo, true)),
    )
    .unwrap();
    h.execute(&mut f.project, Box::new(SetLayerBlendMode::new(comp, layer_id, BlendMode::Add)))
        .unwrap();
    h.execute(
        &mut f.project,
        Box::new(SetLayerProperty::new(
            comp,
            layer_id,
            ClipProperty::Opacity,
            PropertyValue::Scalar(0.25),
        )),
    )
    .unwrap();

    let after = f.project.composition(comp).unwrap().layer(layer_id).unwrap();
    assert!(!after.enabled);
    assert!(after.solo);
    assert_eq!(after.blend, BlendMode::Add);
    assert_eq!(after.transform.opacity.value, 0.25);

    for _ in 0..4 {
        h.undo(&mut f.project).unwrap();
    }
    assert_eq!(f.project.composition(comp).unwrap().layer(layer_id).unwrap(), &before);
}

#[test]
fn composition_settings_change_and_undo_as_one() {
    let mut f = fixture();
    let mut h = History::default();
    let comp = f.composition(&mut h);
    let before = f.project.composition(comp).unwrap().settings.clone();

    let settings = CompositionSettings {
        resolution: Size::new(3840, 2160),
        rate: Rate::FPS_24,
        duration: Ticks::from_seconds(30),
        background: ve_core::Rgba::WHITE,
        color_space: ve_core::ColorSpace::Linear,
        motion_blur: ve_core::MotionBlur::new(90.0, 8),
    };
    h.execute(&mut f.project, Box::new(SetCompositionSettings::new(comp, settings.clone())))
        .unwrap();
    assert_eq!(f.project.composition(comp).unwrap().settings, settings);

    h.undo(&mut f.project).unwrap();
    assert_eq!(f.project.composition(comp).unwrap().settings, before);
}

// ---- nesting ------------------------------------------------------------

#[test]
fn nesting_clips_replaces_them_with_one_clip_holding_a_composition() {
    let mut f = fixture();
    let a = f.add_clip(2, 3);
    let b = f.add_clip(5, 2);
    let before = f.project.clone();
    let mut h = History::default();

    h.execute(
        &mut f.project,
        Box::new(NestClips::new(f.sequence, f.track, vec![a, b], "Nested")),
    )
    .unwrap();

    let track = f.project.sequence(f.sequence).unwrap().track(f.track).unwrap();
    assert_eq!(track.clips().len(), 1, "two clips became one");
    let clip = &track.clips()[0];
    let comp = clip.composition().expect("the replacement holds a composition");
    // The replacement occupies exactly the span the originals did.
    assert_eq!(clip.timeline_start, Ticks::from_seconds(2));
    assert_eq!(clip.duration, Ticks::from_seconds(5));

    // The composition holds both clips as layers, bottom-first in timeline order
    // and positioned relative to the start of the selection.
    let composition = f.project.composition(comp).unwrap();
    assert_eq!(composition.duration(), Ticks::from_seconds(5));
    assert_eq!(composition.layers.len(), 2);
    assert_eq!(composition.layers[0].start, Ticks::ZERO);
    assert_eq!(composition.layers[0].duration, Ticks::from_seconds(3));
    assert_eq!(composition.layers[1].start, Ticks::from_seconds(3));
    assert_eq!(composition.layers[1].source, Source::Asset(f.asset));

    // And undo restores the edit exactly, composition and all.
    h.undo(&mut f.project).unwrap();
    assert!(f.project.compositions.is_empty(), "the composition goes with the nest");
    assert_eq!(
        f.project.sequence(f.sequence).unwrap().track(f.track).unwrap().clips(),
        before.sequence(f.sequence).unwrap().track(f.track).unwrap().clips()
    );
}

#[test]
fn nesting_carries_each_clips_compositing_state_into_its_layer() {
    let mut f = fixture();
    let a = f.add_clip(0, 4);
    {
        let (_, clip) = f.project.sequence_mut(f.sequence).unwrap().find_clip_mut(a).unwrap();
        clip.blend = BlendMode::Screen;
        clip.transform.opacity.value = 0.4;
        clip.source_in = Ticks::from_seconds(7);
        clip.enabled = false;
    }
    let mut h = History::default();

    h.execute(&mut f.project, Box::new(NestClips::new(f.sequence, f.track, vec![a], "Nested")))
        .unwrap();

    let comp = f.project.compositions[0].id;
    let layer = &f.project.composition(comp).unwrap().layers[0];
    // Anything dropped here would silently change the picture the moment the
    // clips became layers.
    assert_eq!(layer.blend, BlendMode::Screen);
    assert_eq!(layer.transform.opacity.value, 0.4);
    assert_eq!(layer.source_in, Ticks::from_seconds(7));
    assert!(!layer.enabled);
}

#[test]
fn nesting_a_clip_that_already_holds_a_composition_nests_deeper() {
    let mut f = fixture();
    let mut h = History::default();
    let inner = f.composition(&mut h);
    let clip = composition_clip(&mut f.project, inner, Ticks::ZERO).unwrap();
    let clip_id = clip.id;
    h.execute(&mut f.project, Box::new(AddClip::new(f.sequence, f.track, clip))).unwrap();

    h.execute(
        &mut f.project,
        Box::new(NestClips::new(f.sequence, f.track, vec![clip_id], "Outer")),
    )
    .unwrap();

    let outer = f
        .project
        .compositions
        .iter()
        .find(|c| c.name == "Outer")
        .expect("the new composition")
        .id;
    assert_eq!(
        f.project.composition(outer).unwrap().layers[0].source,
        Source::Composition(inner)
    );
    assert_eq!(f.project.nesting_depth(outer), 2, "two levels deep, and no cycle");
}

#[test]
fn nesting_nothing_is_refused_rather_than_creating_an_empty_composition() {
    let mut f = fixture();
    let mut h = History::default();
    assert!(h
        .execute(&mut f.project, Box::new(NestClips::new(f.sequence, f.track, vec![], "Empty")))
        .is_err());
    assert!(f.project.compositions.is_empty(), "nothing created by a refused nest");

    // A selection naming a clip that is not there is refused the same way, and
    // leaves no composition behind either.
    assert!(h
        .execute(
            &mut f.project,
            Box::new(NestClips::new(
                f.sequence,
                f.track,
                vec![ClipId::from_raw(4242)],
                "Ghost"
            ))
        )
        .is_err());
    assert!(f.project.compositions.is_empty());
}
