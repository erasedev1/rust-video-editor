//! Fades, crossfades and the track level, from the command layer down.
//!
//! The fade *shapes* are `ve-core`'s to test; what is asserted here is that the
//! edits are undoable to the byte, that a drag collapses into one history entry,
//! and that a crossfade refuses to pretend two sequential clips overlap.

use ve_command::*;
use ve_core::{
    AssetId, Clip, ClipId, Fade, FadeCurve, FadeEdge, MediaInfo, Project, SequenceId, Track,
    TrackId, TrackKind,
};
use ve_time::Ticks;

fn fixture() -> (Project, SequenceId, TrackId, AssetId) {
    let mut p = Project::with_default_sequence("Audio Test");
    let asset = p.add_asset(
        "/media/a.wav",
        MediaInfo {
            duration: Ticks::from_seconds(60),
            video: None,
            audio: Some(ve_core::AudioStreamInfo {
                sample_rate: ve_time::SampleRate::HZ_48000,
                channels: 2,
                duration: Ticks::from_seconds(60),
                codec: "pcm_s16le".into(),
            }),
            container: "wav".into(),
            timecode: None,
        },
    );
    let seq = p.active_sequence.unwrap();
    let track = p
        .sequence(seq)
        .unwrap()
        .tracks
        .iter()
        .find(|t| t.kind == TrackKind::Audio)
        .expect("the default sequence has an audio track")
        .id;
    (p, seq, track, asset)
}

fn add_clip(
    p: &mut Project,
    seq: SequenceId,
    track: TrackId,
    asset: AssetId,
    start_s: i64,
    dur_s: i64,
) -> ClipId {
    let id = p.new_clip_id();
    let clip = Clip::new(
        id,
        asset,
        format!("clip@{start_s}"),
        Ticks::ZERO,
        Ticks::from_seconds(start_s),
        Ticks::from_seconds(dur_s),
    );
    p.sequence_mut(seq).unwrap().track_mut(track).unwrap().insert_clip(clip).unwrap();
    id
}

fn fade_of(p: &Project, seq: SequenceId, clip: ClipId, edge: FadeEdge) -> Fade {
    p.sequence(seq).unwrap().find_clip(clip).unwrap().1.audio.fade(edge)
}

#[test]
fn setting_a_fade_undoes_and_redoes_exactly() {
    let (mut p, seq, track, asset) = fixture();
    let clip = add_clip(&mut p, seq, track, asset, 0, 10);
    let before = p.clone();
    let mut h = History::default();

    h.execute(
        &mut p,
        Box::new(SetClipFade::new(
            seq,
            clip,
            FadeEdge::In,
            Fade::new(Ticks::from_seconds(2), FadeCurve::EqualPower),
        )),
    )
    .unwrap();
    let after = p.clone();
    assert_eq!(fade_of(&p, seq, clip, FadeEdge::In).length, Ticks::from_seconds(2));

    h.undo(&mut p).unwrap();
    assert_eq!(p, before, "undo must restore the project exactly");
    h.redo(&mut p).unwrap();
    assert_eq!(p, after, "redo must reproduce the edit exactly");
}

#[test]
fn a_fade_is_never_longer_than_the_clip_it_is_on() {
    let (mut p, seq, track, asset) = fixture();
    let clip = add_clip(&mut p, seq, track, asset, 0, 4);
    let mut h = History::default();

    h.execute(
        &mut p,
        Box::new(SetClipFade::new(
            seq,
            clip,
            FadeEdge::Out,
            Fade::new(Ticks::from_seconds(30), FadeCurve::Linear),
        )),
    )
    .unwrap();

    assert_eq!(fade_of(&p, seq, clip, FadeEdge::Out).length, Ticks::from_seconds(4));
}

#[test]
fn dragging_a_fade_handle_is_one_undo_step() {
    let (mut p, seq, track, asset) = fixture();
    let clip = add_clip(&mut p, seq, track, asset, 0, 10);
    let before = p.clone();
    let mut h = History::default();

    for ms in [200, 400, 600, 800] {
        h.execute_coalesced(
            &mut p,
            Box::new(SetClipFade::new(
                seq,
                clip,
                FadeEdge::In,
                Fade::new(Ticks::from_millis(ms), FadeCurve::EqualPower),
            )),
        )
        .unwrap();
    }
    h.break_merge();

    assert_eq!(h.undo_depth(), 1, "a drag must not fill the undo stack");
    assert_eq!(fade_of(&p, seq, clip, FadeEdge::In).length, Ticks::from_millis(800));

    h.undo(&mut p).unwrap();
    assert_eq!(p, before, "undoing the gesture must take all of it back");
}

#[test]
fn a_gesture_that_ended_does_not_merge_into_the_next_one() {
    let (mut p, seq, track, asset) = fixture();
    let clip = add_clip(&mut p, seq, track, asset, 0, 10);
    let mut h = History::default();

    h.execute_coalesced(
        &mut p,
        Box::new(SetClipFade::new(
            seq,
            clip,
            FadeEdge::In,
            Fade::new(Ticks::from_seconds(1), FadeCurve::Linear),
        )),
    )
    .unwrap();
    h.break_merge();
    h.execute_coalesced(
        &mut p,
        Box::new(SetClipFade::new(
            seq,
            clip,
            FadeEdge::In,
            Fade::new(Ticks::from_seconds(3), FadeCurve::Linear),
        )),
    )
    .unwrap();

    assert_eq!(h.undo_depth(), 2);
    h.undo(&mut p).unwrap();
    assert_eq!(fade_of(&p, seq, clip, FadeEdge::In).length, Ticks::from_seconds(1));
}

#[test]
fn the_two_edges_of_one_clip_are_separate_history_entries() {
    let (mut p, seq, track, asset) = fixture();
    let clip = add_clip(&mut p, seq, track, asset, 0, 10);
    let mut h = History::default();

    let fade = Fade::new(Ticks::from_seconds(1), FadeCurve::Smooth);
    h.execute_coalesced(&mut p, Box::new(SetClipFade::new(seq, clip, FadeEdge::In, fade)))
        .unwrap();
    h.execute_coalesced(&mut p, Box::new(SetClipFade::new(seq, clip, FadeEdge::Out, fade)))
        .unwrap();

    assert_eq!(h.undo_depth(), 2, "a fade-out must not absorb a fade-in");
}

#[test]
fn clearing_a_fade_restores_the_clip_to_having_none() {
    let (mut p, seq, track, asset) = fixture();
    let clip = add_clip(&mut p, seq, track, asset, 0, 10);
    let mut h = History::default();

    h.execute(
        &mut p,
        Box::new(SetClipFade::new(
            seq,
            clip,
            FadeEdge::In,
            Fade::new(Ticks::from_seconds(2), FadeCurve::Linear),
        )),
    )
    .unwrap();
    h.execute(&mut p, Box::new(SetClipFade::clear(seq, clip, FadeEdge::In))).unwrap();

    assert!(!fade_of(&p, seq, clip, FadeEdge::In).is_active());
}

/// The refusal that keeps the feature honest: two clips at a cut are never
/// audible at the same instant, so there is nothing to cross.
#[test]
fn a_crossfade_refuses_two_clips_that_only_meet_at_a_cut() {
    let (mut p, seq, track, asset) = fixture();
    let left = add_clip(&mut p, seq, track, asset, 0, 10);
    let right = add_clip(&mut p, seq, track, asset, 10, 10);
    let before = p.clone();
    let mut h = History::default();

    assert!(CrossfadeClips::overlap(&p, seq, left, right).is_none());
    let result = h.execute(
        &mut p,
        Box::new(CrossfadeClips::new(seq, left, right, FadeCurve::EqualPower)),
    );
    assert!(result.is_err(), "clips that never sound together cannot be crossfaded");
    assert_eq!(p, before, "a refused command must leave the project untouched");
    assert!(!h.can_undo());
}

#[test]
fn a_crossfade_spans_exactly_the_overlap_between_two_tracks() {
    let (mut p, seq, track_a, asset) = fixture();
    let track_b = {
        let id = p.new_track_id();
        let track = Track::new(id, TrackKind::Audio, "A2");
        p.sequence_mut(seq).unwrap().tracks.push(track);
        id
    };
    // Six seconds of overlap: 0..10 against 4..14.
    let outgoing = add_clip(&mut p, seq, track_a, asset, 0, 10);
    let incoming = add_clip(&mut p, seq, track_b, asset, 4, 10);
    let before = p.clone();
    let mut h = History::default();

    let (a, b, length) = CrossfadeClips::overlap(&p, seq, incoming, outgoing)
        .expect("the clips overlap in time");
    assert_eq!((a, b), (outgoing, incoming), "the earlier clip is the one fading out");
    assert_eq!(length, Ticks::from_seconds(6));

    h.execute(
        &mut p,
        Box::new(CrossfadeClips::new(seq, incoming, outgoing, FadeCurve::EqualPower)),
    )
    .unwrap();

    let out = fade_of(&p, seq, outgoing, FadeEdge::Out);
    let into = fade_of(&p, seq, incoming, FadeEdge::In);
    assert_eq!(out.length, Ticks::from_seconds(6));
    assert_eq!(into.length, Ticks::from_seconds(6));
    assert_eq!(out.curve, FadeCurve::EqualPower);
    assert_eq!(into.curve, FadeCurve::EqualPower);

    h.undo(&mut p).unwrap();
    assert_eq!(p, before, "undoing a crossfade must take both fades back");
}

#[test]
fn a_crossfade_puts_back_the_fades_it_replaced() {
    let (mut p, seq, track_a, asset) = fixture();
    let track_b = {
        let id = p.new_track_id();
        p.sequence_mut(seq).unwrap().tracks.push(Track::new(id, TrackKind::Audio, "A2"));
        id
    };
    let outgoing = add_clip(&mut p, seq, track_a, asset, 0, 10);
    let incoming = add_clip(&mut p, seq, track_b, asset, 6, 10);
    let mut h = History::default();

    let original = Fade::new(Ticks::from_seconds(1), FadeCurve::Smooth);
    h.execute(&mut p, Box::new(SetClipFade::new(seq, outgoing, FadeEdge::Out, original)))
        .unwrap();
    let with_original = p.clone();

    h.execute(
        &mut p,
        Box::new(CrossfadeClips::new(seq, outgoing, incoming, FadeCurve::EqualPower)),
    )
    .unwrap();
    assert_eq!(fade_of(&p, seq, outgoing, FadeEdge::Out).length, Ticks::from_seconds(4));

    h.undo(&mut p).unwrap();
    assert_eq!(p, with_original, "the fade the crossfade displaced must come back");
}

#[test]
fn setting_a_track_level_undoes_and_redoes_exactly() {
    let (mut p, seq, track, _) = fixture();
    let before = p.clone();
    let mut h = History::default();

    h.execute(&mut p, Box::new(SetTrackLevel::new(seq, track, TrackLevel::Volume, 0.5)))
        .unwrap();
    h.execute(&mut p, Box::new(SetTrackLevel::new(seq, track, TrackLevel::Pan, -0.75)))
        .unwrap();

    let after = p.clone();
    let t = p.sequence(seq).unwrap().track(track).unwrap();
    assert_eq!(t.audio_level(), (0.5, -0.75));

    h.undo(&mut p).unwrap();
    h.undo(&mut p).unwrap();
    assert_eq!(p, before);
    h.redo(&mut p).unwrap();
    h.redo(&mut p).unwrap();
    assert_eq!(p, after);
}

#[test]
fn a_track_level_is_bounded_before_it_reaches_the_model() {
    let (mut p, seq, track, _) = fixture();
    let mut h = History::default();

    h.execute(&mut p, Box::new(SetTrackLevel::new(seq, track, TrackLevel::Volume, -3.0)))
        .unwrap();
    h.execute(&mut p, Box::new(SetTrackLevel::new(seq, track, TrackLevel::Pan, 9.0))).unwrap();

    let t = p.sequence(seq).unwrap().track(track).unwrap();
    assert_eq!(t.volume, 0.0, "a negative gain would invert the phase, not silence it");
    assert_eq!(t.pan, 1.0);
}

#[test]
fn dragging_a_track_fader_is_one_undo_step_and_pan_is_a_different_one() {
    let (mut p, seq, track, _) = fixture();
    let mut h = History::default();

    for v in [0.9, 0.8, 0.7] {
        h.execute_coalesced(
            &mut p,
            Box::new(SetTrackLevel::new(seq, track, TrackLevel::Volume, v)),
        )
        .unwrap();
    }
    h.execute_coalesced(&mut p, Box::new(SetTrackLevel::new(seq, track, TrackLevel::Pan, 0.2)))
        .unwrap();

    assert_eq!(h.undo_depth(), 2);
    assert_eq!(p.sequence(seq).unwrap().track(track).unwrap().volume, 0.7);
}
