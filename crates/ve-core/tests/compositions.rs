//! The composition model: layer stacks, nesting, and the rules that keep a
//! nested graph renderable.

use ve_core::{
    BlendMode, Clip, Composition, CompositionLayer, CompositionSettings, CoreError, MediaInfo,
    Project, Size, Source, Speed, VideoStreamInfo,
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
        timecode: None,
    }
}

/// A project with one asset and one empty composition.
fn fixture() -> (Project, ve_core::AssetId, ve_core::CompositionId) {
    let mut project = Project::with_default_sequence("Comps");
    let asset = project.add_asset("/media/a.mp4", video_info(60));
    let comp = project.add_composition("Title", CompositionSettings::default());
    (project, asset, comp)
}

fn layer(
    project: &mut Project,
    comp: ve_core::CompositionId,
    source: impl Into<Source>,
    start_s: i64,
    dur_s: i64,
) -> ve_core::LayerId {
    let id = project.new_layer_id();
    let layer = CompositionLayer::new(
        id,
        format!("layer@{start_s}"),
        source,
        Ticks::from_seconds(start_s),
        Ticks::from_seconds(dur_s),
    );
    project.composition_mut(comp).unwrap().push_layer(layer);
    id
}

#[test]
fn layers_may_overlap_which_is_the_whole_point() {
    let (mut project, asset, comp) = fixture();
    layer(&mut project, comp, asset, 0, 5);
    layer(&mut project, comp, asset, 1, 5);
    layer(&mut project, comp, asset, 2, 5);

    // A track would have refused the second of these. A composition takes all
    // three, because three things covering the same instant is what compositing
    // means.
    let c = project.composition(comp).unwrap();
    assert_eq!(c.layers.len(), 3);
    assert_eq!(c.layers_at(Ticks::from_seconds(3)).count(), 3);
}

#[test]
fn the_stack_order_is_the_vector_order_bottom_first() {
    let (mut project, asset, comp) = fixture();
    let bottom = layer(&mut project, comp, asset, 0, 5);
    let top = layer(&mut project, comp, asset, 0, 5);

    let c = project.composition(comp).unwrap();
    assert_eq!(c.layer_index(bottom), Some(0));
    assert_eq!(c.layer_index(top), Some(1));
    let at: Vec<_> = c.layers_at(Ticks::from_seconds(1)).map(|l| l.id).collect();
    assert_eq!(at, vec![bottom, top], "the compositor draws index 0 first");
}

#[test]
fn moving_a_layer_reports_where_it_came_from_so_undo_can_put_it_back() {
    let (mut project, asset, comp) = fixture();
    let a = layer(&mut project, comp, asset, 0, 5);
    let b = layer(&mut project, comp, asset, 0, 5);
    let c = layer(&mut project, comp, asset, 0, 5);

    let composition = project.composition_mut(comp).unwrap();
    assert_eq!(composition.move_layer(c, 0), Some(2));
    assert_eq!(composition.layers.iter().map(|l| l.id).collect::<Vec<_>>(), vec![c, a, b]);

    // Putting it back is the same call with the reported index.
    composition.move_layer(c, 2);
    assert_eq!(composition.layers.iter().map(|l| l.id).collect::<Vec<_>>(), vec![a, b, c]);

    // A move to where it already is succeeds and changes nothing, so a drag
    // that ends where it started is not an error.
    assert_eq!(composition.move_layer(a, 0), Some(0));
    assert_eq!(composition.layers.iter().map(|l| l.id).collect::<Vec<_>>(), vec![a, b, c]);
    assert_eq!(composition.move_layer(ve_core::LayerId::from_raw(999), 0), None);
}

#[test]
fn a_disabled_layer_and_a_solo_layer_decide_what_contributes() {
    let (mut project, asset, comp) = fixture();
    let a = layer(&mut project, comp, asset, 0, 5);
    let b = layer(&mut project, comp, asset, 0, 5);
    let at = Ticks::from_seconds(1);

    project.composition_mut(comp).unwrap().layer_mut(a).unwrap().enabled = false;
    let ids: Vec<_> = project.composition(comp).unwrap().layers_at(at).map(|l| l.id).collect();
    assert_eq!(ids, vec![b], "a disabled layer contributes nothing");

    project.composition_mut(comp).unwrap().layer_mut(a).unwrap().enabled = true;
    project.composition_mut(comp).unwrap().layer_mut(a).unwrap().solo = true;
    let ids: Vec<_> = project.composition(comp).unwrap().layers_at(at).map(|l| l.id).collect();
    assert_eq!(ids, vec![a], "soloing one layer excludes the rest");
}

#[test]
fn a_layer_maps_composition_time_to_source_time_through_its_speed() {
    let (mut project, asset, comp) = fixture();
    let id = layer(&mut project, comp, asset, 2, 4);
    {
        let l = project.composition_mut(comp).unwrap().layer_mut(id).unwrap();
        l.source_in = Ticks::from_seconds(10);
        l.speed = Speed::new(2, 1).unwrap();
    }
    let l = project.composition(comp).unwrap().layer(id).unwrap();

    assert_eq!(l.source_time_at(Ticks::from_seconds(1)), None, "before the layer starts");
    assert_eq!(l.source_time_at(Ticks::from_seconds(2)), Some(Ticks::from_seconds(10)));
    // One second into a 2x layer consumes two seconds of source.
    assert_eq!(l.source_time_at(Ticks::from_seconds(3)), Some(Ticks::from_seconds(12)));
    assert_eq!(l.source_time_at(Ticks::from_seconds(6)), None, "the end is exclusive");
    assert_eq!(l.source_duration(), Ticks::from_seconds(8));
    assert_eq!(l.local_time_at(Ticks::from_seconds(3)), Ticks::from_seconds(1));
}

#[test]
fn a_compositions_duration_is_its_own_not_its_contents() {
    let (mut project, asset, comp) = fixture();
    layer(&mut project, comp, asset, 0, 30);

    let c = project.composition(comp).unwrap();
    // The canvas is ten seconds by default even though a layer runs to thirty:
    // a composition is a source, and a source's length cannot depend on what
    // someone drops into it.
    assert_eq!(c.duration(), Ticks::from_seconds(10));
    assert_eq!(c.content_end(), Ticks::from_seconds(30), "but the content is still findable");
}

#[test]
fn a_composition_cannot_contain_itself() {
    let (project, _, comp) = fixture();
    assert!(project.nesting_would_cycle(comp, comp));
}

#[test]
fn a_cycle_through_other_compositions_is_caught() {
    let (mut project, asset, a) = fixture();
    let b = project.add_composition("B", CompositionSettings::default());
    let c = project.add_composition("C", CompositionSettings::default());

    // a contains b, b contains c.
    layer(&mut project, a, b, 0, 5);
    layer(&mut project, b, c, 0, 5);
    layer(&mut project, c, asset, 0, 5);

    // Putting a inside c would close the loop a → b → c → a.
    assert!(project.nesting_would_cycle(c, a), "c containing a closes the loop");
    // The other direction is fine: c inside a is a diamond, not a cycle.
    assert!(!project.nesting_would_cycle(a, c));
    assert_eq!(project.nesting_depth(a), 3);
    assert_eq!(project.nesting_depth(c), 1);
}

#[test]
fn using_one_composition_twice_is_not_a_cycle() {
    let (mut project, asset, a) = fixture();
    let shared = project.add_composition("Shared", CompositionSettings::default());
    layer(&mut project, shared, asset, 0, 5);
    layer(&mut project, a, shared, 0, 5);
    layer(&mut project, a, shared, 5, 5);

    assert!(!project.nesting_would_cycle(a, shared), "reuse is not recursion");
    assert_eq!(project.composition(a).unwrap().nested(), vec![shared], "deduplicated");
    assert_eq!(project.nesting_depth(a), 2);
}

#[test]
fn a_composition_still_in_use_cannot_be_removed() {
    let (mut project, asset, comp) = fixture();
    let sequence = project.active_sequence.unwrap();
    let track = project.sequence(sequence).unwrap().tracks[0].id;

    // Once on a timeline…
    let clip_id = project.new_clip_id();
    let clip =
        Clip::new(clip_id, comp, "title", Ticks::ZERO, Ticks::ZERO, Ticks::from_seconds(5));
    project
        .sequence_mut(sequence)
        .unwrap()
        .track_mut(track)
        .unwrap()
        .insert_clip(clip)
        .unwrap();

    match project.remove_composition(comp) {
        Err(CoreError::CompositionInUse { places, .. }) => assert_eq!(places, 1),
        other => panic!("expected a refusal naming one use, got {other:?}"),
    }

    // …and once inside another composition.
    let outer = project.add_composition("Outer", CompositionSettings::default());
    layer(&mut project, outer, comp, 0, 5);
    match project.remove_composition(comp) {
        Err(CoreError::CompositionInUse { places, .. }) => assert_eq!(places, 2),
        other => panic!("expected a refusal naming two uses, got {other:?}"),
    }

    // Nothing uses the empty one, so it goes.
    let unused = project.add_composition("Unused", CompositionSettings::default());
    assert!(project.remove_composition(unused).is_ok());
    assert!(matches!(
        project.remove_composition(unused),
        Err(CoreError::CompositionNotFound(_))
    ));

    // An asset used only by a composition layer is also still in use.
    layer(&mut project, comp, asset, 0, 5);
    assert_eq!(project.uses_of(Source::Asset(asset)), 1);
}

#[test]
fn a_clip_can_hold_a_composition_and_is_bounded_by_its_duration() {
    let (mut project, _, comp) = fixture();
    let clip_id = project.new_clip_id();
    let clip =
        Clip::new(clip_id, comp, "title", Ticks::ZERO, Ticks::ZERO, Ticks::from_seconds(5));

    assert_eq!(clip.source, Source::Composition(comp));
    assert_eq!(clip.asset(), None, "there is no file behind a composition");
    assert_eq!(clip.composition(), Some(comp));
    // The bound a trim is checked against is the composition's own length, the
    // same way media duration bounds a trim of a file.
    assert_eq!(project.source_duration(clip.source), Ticks::from_seconds(10));
}

#[test]
fn loading_removes_a_layer_that_contains_its_own_composition() {
    let (mut project, asset, comp) = fixture();
    layer(&mut project, comp, asset, 0, 5);
    // Only reachable by hand-editing a file, which is exactly what the loader
    // has to survive: the alternative to removing it is a render that recurses
    // until the stack runs out.
    layer(&mut project, comp, comp, 0, 5);

    let warnings = project.normalise();
    assert_eq!(project.composition(comp).unwrap().layers.len(), 1);
    assert!(
        warnings.iter().any(|w| w.contains("contained the composition itself")),
        "the repair has to be reported, not silent: {warnings:?}"
    );
}

#[test]
fn loading_breaks_a_cycle_between_compositions() {
    let (mut project, asset, a) = fixture();
    let b = project.add_composition("B", CompositionSettings::default());
    layer(&mut project, a, b, 0, 5);
    layer(&mut project, b, asset, 0, 5);
    // Hand-edited into a loop: b now contains a, which contains b.
    layer(&mut project, b, a, 0, 5);

    let before = project.compositions.iter().map(|c| c.layers.len()).sum::<usize>();
    let warnings = project.normalise();
    assert!(
        warnings.iter().any(|w| w.contains("nesting cycle")),
        "the broken cycle has to be reported: {warnings:?}"
    );

    // Exactly one edge removed, and the first one in document order, so the same
    // file always loads the same way.
    let after = project.compositions.iter().map(|c| c.layers.len()).sum::<usize>();
    assert_eq!(after, before - 1, "one edge is enough; nothing else is thrown away");
    assert!(project.composition(a).unwrap().nested().is_empty(), "a's layer was the first");
    assert_eq!(project.composition(b).unwrap().nested(), vec![a]);
    // And what is left is renderable: nothing can reach itself any more.
    assert!(
        !project.nesting_would_cycle(b, a)
            || project.composition(a).unwrap().nested().is_empty()
    );
    assert_eq!(project.nesting_depth(b), 2);
}

#[test]
fn loading_advances_the_allocator_past_composition_and_layer_ids() {
    let mut project = Project::new("Hand edited");
    // A file whose IDs came from a much later session. Every one of these has to
    // be counted, or a newly created layer could be handed an ID that is already
    // in the document.
    let mut comp = Composition::new(
        ve_core::CompositionId::from_raw(400),
        "Title",
        CompositionSettings::default(),
    );
    comp.push_layer(CompositionLayer::new(
        ve_core::LayerId::from_raw(700),
        "layer",
        ve_core::AssetId::from_raw(900),
        Ticks::ZERO,
        Ticks::from_seconds(1),
    ));
    project.compositions.push(comp);

    project.normalise();
    assert!(
        project.ids.peek() > 900,
        "allocation must not collide with a referenced id, got {}",
        project.ids.peek()
    );
}

#[test]
fn loading_reports_a_layer_whose_source_is_missing() {
    let mut project = Project::new("Dangling");
    let comp = project.add_composition("Title", CompositionSettings::default());
    layer(&mut project, comp, ve_core::AssetId::from_raw(77), 0, 5);

    let warnings = project.normalise();
    assert!(
        warnings.iter().any(|w| w.contains("references missing asset")),
        "a dangling source is kept but reported: {warnings:?}"
    );
    assert_eq!(
        project.composition(comp).unwrap().layers.len(),
        1,
        "nothing is discarded; the user can relink it"
    );
}

#[test]
fn a_layer_carries_the_compositing_state_the_renderer_needs() {
    let (mut project, asset, comp) = fixture();
    let id = layer(&mut project, comp, asset, 0, 5);
    {
        let l = project.composition_mut(comp).unwrap().layer_mut(id).unwrap();
        l.blend = BlendMode::Screen;
        l.transform.opacity.value = 0.5;
    }
    let l = project.composition(comp).unwrap().layer(id).unwrap();
    assert_eq!(l.blend, BlendMode::Screen);
    assert!(!l.is_animated(), "a constant property is not animation");

    let l = project.composition_mut(comp).unwrap().layer_mut(id).unwrap();
    l.transform.opacity.set_keyframe(Ticks::ZERO, 0.0, ve_core::Interpolation::Linear);
    l.transform.opacity.set_keyframe(
        Ticks::from_seconds(1),
        1.0,
        ve_core::Interpolation::Linear,
    );
    assert!(l.is_animated());
}
