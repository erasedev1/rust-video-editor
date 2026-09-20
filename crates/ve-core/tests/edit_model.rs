//! Behavioural tests for the edit model: track invariants, non-destructive
//! trimming, splitting, and the shared animation system.

use ve_core::animation::Interpolation;
use ve_core::clip::Speed;
use ve_core::id::EffectId;
use ve_core::*;
use ve_time::{Rate, SampleRate, Ticks, TimeRange};

/// A project with one 60-second video asset and a single video track.
fn fixture() -> (Project, SequenceId, TrackId, AssetId) {
    let mut p = Project::with_default_sequence("Test");
    let seq = p.active_sequence.unwrap();
    let asset = p.add_asset(
        "/media/movie.mp4",
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
            audio: Some(AudioStreamInfo {
                sample_rate: SampleRate::HZ_48000,
                channels: 2,
                duration: Ticks::from_seconds(60),
                codec: "aac".into(),
            }),
            container: "mov".into(),
        },
    );
    let track = p.sequence(seq).unwrap().tracks[0].id;
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
        "clip",
        Ticks::ZERO,
        Ticks::from_seconds(start_s),
        Ticks::from_seconds(dur_s),
    );
    p.sequence_mut(seq).unwrap().track_mut(track).unwrap().insert_clip(clip).unwrap();
    id
}

#[test]
fn a_new_project_has_a_usable_sequence() {
    let p = Project::with_default_sequence("My Film");
    let seq = p.active().expect("default sequence");
    assert_eq!(seq.tracks.len(), 4);
    assert_eq!(seq.video_tracks().count(), 2);
    assert_eq!(seq.audio_tracks().count(), 2);
    assert_eq!(seq.tracks[0].name, "V1");
    assert_eq!(seq.tracks[2].name, "A1");
    assert_eq!(seq.duration(), Ticks::ZERO);
}

#[test]
fn ids_are_unique_and_never_reused() {
    let (mut p, seq, track, asset) = fixture();
    let a = add_clip(&mut p, seq, track, asset, 0, 5);
    let b = add_clip(&mut p, seq, track, asset, 5, 5);
    assert_ne!(a, b);
    p.sequence_mut(seq).unwrap().track_mut(track).unwrap().remove_clip(a).unwrap();
    let c = add_clip(&mut p, seq, track, asset, 0, 5);
    assert_ne!(c, a, "a deleted ID must never be handed out again");
    assert_ne!(c, b);
}

#[test]
fn overlapping_inserts_are_refused_and_leave_the_track_untouched() {
    let (mut p, seq, track, asset) = fixture();
    add_clip(&mut p, seq, track, asset, 0, 10);

    let id = p.new_clip_id();
    let overlapping = Clip::new(
        id,
        asset,
        "overlap",
        Ticks::ZERO,
        Ticks::from_seconds(5),
        Ticks::from_seconds(10),
    );
    let t = p.sequence_mut(seq).unwrap().track_mut(track).unwrap();
    assert_eq!(t.insert_clip(overlapping), Err(CoreError::ClipOverlap));
    assert_eq!(t.len(), 1, "the refused edit must not have been applied");
    assert!(t.invariants_hold());
}

#[test]
fn butt_joined_clips_are_allowed() {
    let (mut p, seq, track, asset) = fixture();
    add_clip(&mut p, seq, track, asset, 0, 10);
    add_clip(&mut p, seq, track, asset, 10, 10);
    let t = p.sequence(seq).unwrap().track(track).unwrap();
    assert_eq!(t.len(), 2);
    assert!(t.invariants_hold());
    assert_eq!(t.content_end(), Ticks::from_seconds(20));
}

#[test]
fn clips_stay_sorted_after_moves() {
    let (mut p, seq, track, asset) = fixture();
    let a = add_clip(&mut p, seq, track, asset, 0, 5);
    let b = add_clip(&mut p, seq, track, asset, 10, 5);
    let c = add_clip(&mut p, seq, track, asset, 20, 5);

    let t = p.sequence_mut(seq).unwrap().track_mut(track).unwrap();
    // Move the first clip past the last one.
    t.move_clip(a, Ticks::from_seconds(30)).unwrap();
    assert!(t.invariants_hold());
    assert_eq!(t.clips().iter().map(|c| c.id).collect::<Vec<_>>(), vec![b, c, a]);

    // And back to the front.
    t.move_clip(a, Ticks::ZERO).unwrap();
    assert_eq!(t.clips().iter().map(|c| c.id).collect::<Vec<_>>(), vec![a, b, c]);
}

#[test]
fn a_move_onto_a_neighbour_is_refused_and_reports_the_old_position() {
    let (mut p, seq, track, asset) = fixture();
    let a = add_clip(&mut p, seq, track, asset, 0, 5);
    add_clip(&mut p, seq, track, asset, 10, 5);

    let t = p.sequence_mut(seq).unwrap().track_mut(track).unwrap();
    assert_eq!(t.move_clip(a, Ticks::from_seconds(12)), Err(CoreError::ClipOverlap));
    assert_eq!(t.clip(a).unwrap().timeline_start, Ticks::ZERO);

    // Moving a clip within its own footprint must not self-collide.
    let old = t.move_clip(a, Ticks::from_seconds(1)).unwrap();
    assert_eq!(old, Ticks::ZERO);
}

#[test]
fn a_clip_cannot_be_moved_before_zero() {
    let (mut p, seq, track, asset) = fixture();
    let a = add_clip(&mut p, seq, track, asset, 10, 5);
    let t = p.sequence_mut(seq).unwrap().track_mut(track).unwrap();
    t.move_clip(a, -Ticks::from_seconds(5)).unwrap();
    assert_eq!(t.clip(a).unwrap().timeline_start, Ticks::ZERO);
}

#[test]
fn trimming_the_head_keeps_the_visible_frames_pinned_to_the_timeline() {
    let (mut p, seq, track, asset) = fixture();
    let id = p.new_clip_id();
    // A clip showing source 10s..20s, sitting at timeline 0s..10s.
    let clip = Clip::new(
        id,
        asset,
        "c",
        Ticks::from_seconds(10),
        Ticks::ZERO,
        Ticks::from_seconds(10),
    );
    let min = p.min_clip_duration(seq);
    let t = p.sequence_mut(seq).unwrap().track_mut(track).unwrap();
    t.insert_clip(clip).unwrap();

    // The frame at timeline 4s is source 14s. After trimming the head to 4s,
    // it must *still* be source 14s.
    let before = t.clip(id).unwrap().source_time_at(Ticks::from_seconds(4)).unwrap();
    assert_eq!(before, Ticks::from_seconds(14));

    t.trim_clip_start(id, Ticks::from_seconds(4), min).unwrap();
    let c = t.clip(id).unwrap();
    assert_eq!(c.source_in, Ticks::from_seconds(14));
    assert_eq!(c.duration, Ticks::from_seconds(6));
    assert_eq!(c.source_time_at(Ticks::from_seconds(4)).unwrap(), before);
}

#[test]
fn trimming_is_non_destructive_and_fully_reversible() {
    let (mut p, seq, track, asset) = fixture();
    let id = p.new_clip_id();
    let original = Clip::new(
        id,
        asset,
        "c",
        Ticks::from_seconds(5),
        Ticks::from_seconds(0),
        Ticks::from_seconds(20),
    );
    let min = p.min_clip_duration(seq);
    let source_len = p.asset_duration(asset);
    let t = p.sequence_mut(seq).unwrap().track_mut(track).unwrap();
    t.insert_clip(original.clone()).unwrap();

    t.trim_clip_start(id, Ticks::from_seconds(8), min).unwrap();
    t.trim_clip_end(id, Ticks::from_seconds(12), min, source_len).unwrap();
    assert_eq!(t.clip(id).unwrap().duration, Ticks::from_seconds(4));

    // Widening the window back out recovers exactly the original clip: no
    // frames were destroyed by the trims.
    t.trim_clip_start(id, Ticks::ZERO, min).unwrap();
    t.trim_clip_end(id, Ticks::from_seconds(20), min, source_len).unwrap();
    assert_eq!(t.clip(id).unwrap(), &original);
}

#[test]
fn a_trim_may_not_go_below_one_frame_or_past_the_source() {
    let (mut p, seq, track, asset) = fixture();
    let id = add_clip(&mut p, seq, track, asset, 0, 10);
    let min = p.min_clip_duration(seq);
    let source_len = p.asset_duration(asset);
    let t = p.sequence_mut(seq).unwrap().track_mut(track).unwrap();

    assert_eq!(
        t.trim_clip_start(id, Ticks::from_seconds(10), min),
        Err(CoreError::TrimTooShort)
    );
    assert_eq!(t.trim_clip_end(id, Ticks::ZERO, min, source_len), Err(CoreError::TrimTooShort));
    // The source is 60 s, so a 70 s clip cannot exist.
    assert_eq!(
        t.trim_clip_end(id, Ticks::from_seconds(70), min, source_len),
        Err(CoreError::TrimBeyondSource)
    );
    assert_eq!(t.clip(id).unwrap().duration, Ticks::from_seconds(10), "nothing changed");

    // Exactly one frame is legal.
    let one_frame = Rate::FPS_30.frame_to_ticks(1);
    t.trim_clip_end(id, one_frame, min, source_len).unwrap();
    assert_eq!(t.clip(id).unwrap().duration, one_frame);
}

#[test]
fn trimming_the_head_cannot_run_off_the_front_of_the_media() {
    let (mut p, seq, track, asset) = fixture();
    let id = p.new_clip_id();
    // Source window starts at 2 s, so there are only 2 s of head available.
    let clip = Clip::new(
        id,
        asset,
        "c",
        Ticks::from_seconds(2),
        Ticks::from_seconds(10),
        Ticks::from_seconds(5),
    );
    let min = p.min_clip_duration(seq);
    let t = p.sequence_mut(seq).unwrap().track_mut(track).unwrap();
    t.insert_clip(clip).unwrap();

    assert_eq!(
        t.trim_clip_start(id, Ticks::from_seconds(7), min),
        Err(CoreError::TrimBeyondSource)
    );
    // Exactly to the head of the media is fine.
    t.trim_clip_start(id, Ticks::from_seconds(8), min).unwrap();
    assert_eq!(t.clip(id).unwrap().source_in, Ticks::ZERO);
}

#[test]
fn splitting_conserves_duration_and_source_coverage() {
    let (mut p, seq, track, asset) = fixture();
    let id = p.new_clip_id();
    let clip = Clip::new(
        id,
        asset,
        "c",
        Ticks::from_seconds(10),
        Ticks::from_seconds(0),
        Ticks::from_seconds(20),
    );
    let new_id = p.new_clip_id();
    let mut next_effect = {
        let mut n = 10_000u64;
        move || {
            n += 1;
            EffectId::from_raw(n)
        }
    };
    let t = p.sequence_mut(seq).unwrap().track_mut(track).unwrap();
    t.insert_clip(clip).unwrap();

    let right_id = t.split_clip(id, Ticks::from_seconds(7), new_id, &mut next_effect).unwrap();
    assert!(t.invariants_hold());

    let left = t.clip(id).unwrap().clone();
    let right = t.clip(right_id).unwrap().clone();

    assert_eq!(left.duration + right.duration, Ticks::from_seconds(20));
    assert_eq!(left.timeline_end(), right.timeline_start, "no gap, no overlap");
    // The halves cover the source contiguously: no frame is lost or repeated.
    assert_eq!(left.source_out(), right.source_in);
    assert_eq!(left.source_in, Ticks::from_seconds(10));
    assert_eq!(right.source_out(), Ticks::from_seconds(30));

    // Every timeline position maps to the same source frame as before the split.
    for s in 0..20 {
        let at = Ticks::from_seconds(s);
        let via_halves = left.source_time_at(at).or_else(|| right.source_time_at(at)).unwrap();
        assert_eq!(via_halves, Ticks::from_seconds(10 + s), "split changed frame at {s}s");
    }
}

#[test]
fn splitting_outside_the_clip_is_refused() {
    let (mut p, seq, track, asset) = fixture();
    let id = add_clip(&mut p, seq, track, asset, 5, 10);
    let new_id = p.new_clip_id();
    let t = p.sequence_mut(seq).unwrap().track_mut(track).unwrap();
    let mut eff = || EffectId::from_raw(1);
    // At the very edges a split would produce a zero-length half.
    assert_eq!(
        t.split_clip(id, Ticks::from_seconds(5), new_id, &mut eff),
        Err(CoreError::SplitOutsideClip)
    );
    assert_eq!(
        t.split_clip(id, Ticks::from_seconds(15), new_id, &mut eff),
        Err(CoreError::SplitOutsideClip)
    );
    assert_eq!(t.len(), 1);
}

#[test]
fn splitting_rebases_keyframes_onto_the_new_clips_local_time() {
    let (mut p, seq, track, asset) = fixture();
    let id = p.new_clip_id();
    let mut clip = Clip::new(id, asset, "c", Ticks::ZERO, Ticks::ZERO, Ticks::from_seconds(10));
    // Fade from transparent at 0 s to opaque at 8 s, clip-relative.
    clip.transform.opacity.set_keyframe(Ticks::ZERO, 0.0, Interpolation::Linear);
    clip.transform.opacity.set_keyframe(Ticks::from_seconds(8), 1.0, Interpolation::Linear);

    let new_id = p.new_clip_id();
    let t = p.sequence_mut(seq).unwrap().track_mut(track).unwrap();
    t.insert_clip(clip).unwrap();

    // Opacity at timeline 6 s is 0.75 before the split.
    let before = t.clip(id).unwrap().transform.opacity.evaluate(Ticks::from_seconds(6));
    assert!((before - 0.75).abs() < 1e-9);

    let mut eff = || EffectId::from_raw(1);
    let right_id = t.split_clip(id, Ticks::from_seconds(4), new_id, &mut eff).unwrap();

    // ... and must still be 0.75 afterwards, now evaluated on the right-hand
    // clip at its own local time of 2 s.
    let right = t.clip(right_id).unwrap();
    let local = right.local_time_at(Ticks::from_seconds(6));
    assert_eq!(local, Ticks::from_seconds(2));
    let after = right.transform.opacity.evaluate(local);
    assert!((after - 0.75).abs() < 1e-9, "opacity jumped from {before} to {after}");
}

#[test]
fn speed_maps_timeline_duration_onto_source_duration_exactly() {
    let (mut p, seq, track, asset) = fixture();
    let id = p.new_clip_id();
    let mut clip = Clip::new(id, asset, "c", Ticks::ZERO, Ticks::ZERO, Ticks::from_seconds(5));
    clip.speed = Speed::new(2, 1).unwrap();
    let t = p.sequence_mut(seq).unwrap().track_mut(track).unwrap();
    t.insert_clip(clip).unwrap();

    let c = t.clip(id).unwrap();
    // 5 s of timeline at 2x consumes 10 s of source.
    assert_eq!(c.source_duration(), Ticks::from_seconds(10));
    assert_eq!(c.source_out(), Ticks::from_seconds(10));
    assert_eq!(c.source_time_at(Ticks::from_seconds(1)).unwrap(), Ticks::from_seconds(2));

    assert!(Speed::new(0, 1).is_err());
    assert!(Speed::new(1, 0).is_err());
    assert_eq!(Speed::from_f64(0.5).unwrap(), Speed::new(1, 2).unwrap());
    assert_eq!(Speed::new(4, 2).unwrap(), Speed::new(2, 1).unwrap(), "stored reduced");
}

#[test]
fn clip_lookup_by_time_uses_the_right_boundaries() {
    let (mut p, seq, track, asset) = fixture();
    let a = add_clip(&mut p, seq, track, asset, 0, 10);
    let b = add_clip(&mut p, seq, track, asset, 10, 10);
    let t = p.sequence(seq).unwrap().track(track).unwrap();

    assert_eq!(t.clip_at(Ticks::ZERO).unwrap().id, a);
    assert_eq!(t.clip_at(Ticks::from_seconds(10) - Ticks(1)).unwrap().id, a);
    assert_eq!(t.clip_at(Ticks::from_seconds(10)).unwrap().id, b, "half-open boundary");
    assert_eq!(t.clip_at(Ticks::from_seconds(19)).unwrap().id, b);
    assert!(t.clip_at(Ticks::from_seconds(20)).is_none());
}

#[test]
fn range_queries_return_every_overlapping_clip_including_partial_ones() {
    let (mut p, seq, track, asset) = fixture();
    for i in 0..10 {
        add_clip(&mut p, seq, track, asset, i * 10, 10);
    }
    let t = p.sequence(seq).unwrap().track(track).unwrap();

    let window = TimeRange::from_bounds(Ticks::from_seconds(25), Ticks::from_seconds(55));
    let hit: Vec<_> = t.clips_in_range(window).map(|c| c.timeline_start).collect();
    // Clips at 20 (partially), 30, 40 and 50 (partially).
    assert_eq!(
        hit,
        vec![
            Ticks::from_seconds(20),
            Ticks::from_seconds(30),
            Ticks::from_seconds(40),
            Ticks::from_seconds(50),
        ]
    );

    assert_eq!(t.clips_in_range(TimeRange::EMPTY).count(), 0);
}

#[test]
fn locked_tracks_refuse_every_mutation() {
    let (mut p, seq, track, asset) = fixture();
    let id = add_clip(&mut p, seq, track, asset, 0, 10);
    let min = p.min_clip_duration(seq);
    let source_len = p.asset_duration(asset);
    let new_id = p.new_clip_id();
    let t = p.sequence_mut(seq).unwrap().track_mut(track).unwrap();
    t.locked = true;

    assert_eq!(t.move_clip(id, Ticks::from_seconds(20)), Err(CoreError::TrackLocked));
    assert_eq!(t.trim_clip_start(id, Ticks::from_seconds(2), min), Err(CoreError::TrackLocked));
    assert_eq!(
        t.trim_clip_end(id, Ticks::from_seconds(8), min, source_len),
        Err(CoreError::TrackLocked)
    );
    assert_eq!(
        t.split_clip(id, Ticks::from_seconds(5), new_id, || EffectId::from_raw(1)),
        Err(CoreError::TrackLocked)
    );
    // Removal too: `insert_clip` refuses on a locked track, so a removal that
    // went through here could never be undone.
    assert_eq!(t.remove_clip(id).unwrap_err(), CoreError::TrackLocked);
    assert_eq!(t.shift_clips_from(Ticks::ZERO, Ticks::SECOND), Err(CoreError::TrackLocked));
    assert_eq!(
        t.clip(id).unwrap().range(),
        TimeRange::new(Ticks::ZERO, Ticks::from_seconds(10))
    );
}

#[test]
fn shifting_the_tail_moves_exactly_the_clips_at_or_after_the_point() {
    let (mut p, seq, track, asset) = fixture();
    add_clip(&mut p, seq, track, asset, 0, 10);
    add_clip(&mut p, seq, track, asset, 10, 10);
    add_clip(&mut p, seq, track, asset, 20, 10);
    let t = p.sequence_mut(seq).unwrap().track_mut(track).unwrap();

    assert_eq!(t.shift_clips_from(Ticks::from_seconds(10), Ticks::from_seconds(5)), Ok(2));
    let starts: Vec<i64> =
        t.clips().iter().map(|c| c.timeline_start.as_secs_f64() as i64).collect();
    assert_eq!(starts, vec![0, 15, 25]);
    assert!(t.invariants_hold());

    // A clip starting exactly at the point is included; one merely overlapping
    // it is not, because ripples move whole clips rather than cutting them.
    assert_eq!(t.shift_clips_from(Ticks::from_seconds(20), Ticks::from_seconds(1)), Ok(1));
    assert_eq!(t.content_end(), Ticks::from_seconds(36));
}

#[test]
fn shifting_the_tail_back_stops_at_the_clip_left_behind() {
    let (mut p, seq, track, asset) = fixture();
    add_clip(&mut p, seq, track, asset, 0, 10);
    add_clip(&mut p, seq, track, asset, 12, 10);
    let t = p.sequence_mut(seq).unwrap().track_mut(track).unwrap();

    // Two seconds of gap: pulling back two closes it exactly, three collides.
    assert_eq!(
        t.shift_clips_from(Ticks::from_seconds(12), Ticks::from_seconds(-3)),
        Err(CoreError::ClipOverlap)
    );
    assert_eq!(
        t.clips()[1].timeline_start,
        Ticks::from_seconds(12),
        "a refused shift moved a clip"
    );
    assert_eq!(t.shift_clips_from(Ticks::from_seconds(12), Ticks::from_seconds(-2)), Ok(1));
    assert_eq!(t.clips()[1].timeline_start, Ticks::from_seconds(10));
}

#[test]
fn shifting_the_first_clip_back_stops_at_the_start_of_the_timeline() {
    let (mut p, seq, track, asset) = fixture();
    add_clip(&mut p, seq, track, asset, 4, 10);
    let t = p.sequence_mut(seq).unwrap().track_mut(track).unwrap();

    assert_eq!(
        t.shift_clips_from(Ticks::ZERO, Ticks::from_seconds(-5)),
        Err(CoreError::ClipOverlap)
    );
    assert_eq!(t.shift_clips_from(Ticks::ZERO, Ticks::from_seconds(-4)), Ok(1));
    assert_eq!(t.clips()[0].timeline_start, Ticks::ZERO);
}

#[test]
fn shifting_past_the_last_clip_moves_nothing() {
    let (mut p, seq, track, asset) = fixture();
    add_clip(&mut p, seq, track, asset, 0, 10);
    let t = p.sequence_mut(seq).unwrap().track_mut(track).unwrap();

    assert_eq!(t.shift_clips_from(Ticks::from_seconds(50), Ticks::from_seconds(5)), Ok(0));
    assert_eq!(t.shift_clips_from(Ticks::ZERO, Ticks::ZERO), Ok(0));
    assert_eq!(t.content_end(), Ticks::from_seconds(10));
}

#[test]
fn a_gap_is_the_empty_span_between_two_clips() {
    let (mut p, seq, track, asset) = fixture();
    add_clip(&mut p, seq, track, asset, 5, 5);
    add_clip(&mut p, seq, track, asset, 20, 5);
    let t = p.sequence(seq).unwrap().track(track).unwrap();

    // Inside a clip there is no gap.
    assert_eq!(t.gap_at(Ticks::from_seconds(7)), None);
    // Between the two clips.
    assert_eq!(
        t.gap_at(Ticks::from_seconds(15)),
        Some(TimeRange::from_bounds(Ticks::from_seconds(10), Ticks::from_seconds(20)))
    );
    // Before the first clip, bounded by the start of the timeline.
    assert_eq!(
        t.gap_at(Ticks::from_seconds(2)),
        Some(TimeRange::from_bounds(Ticks::ZERO, Ticks::from_seconds(5)))
    );
    // Past the last clip there is nothing to pull back, so no gap.
    assert_eq!(t.gap_at(Ticks::from_seconds(40)), None);
    // A butt join is not a gap.
    assert_eq!(t.clip_at(Ticks::from_seconds(10)), None);
    assert_eq!(
        t.gap_at(Ticks::from_seconds(10)).map(|g| g.duration),
        Some(Ticks::from_seconds(10))
    );
}

#[test]
fn neighbours_are_positional_and_survive_gaps() {
    let (mut p, seq, track, asset) = fixture();
    let a = add_clip(&mut p, seq, track, asset, 0, 5);
    let b = add_clip(&mut p, seq, track, asset, 20, 5);
    let c = add_clip(&mut p, seq, track, asset, 30, 5);
    let t = p.sequence(seq).unwrap().track(track).unwrap();

    let (left, right) = t.neighbours_of(b);
    assert_eq!(left.map(|c| c.id), Some(a), "a gap should not hide the neighbour");
    assert_eq!(right.map(|c| c.id), Some(c));

    assert_eq!(t.neighbours_of(a).0.map(|c| c.id), None);
    assert_eq!(t.neighbours_of(c).1.map(|c| c.id), None);
    assert!(matches!(t.neighbours_of(ClipId::from_raw(9_999)), (None, None)));
}

#[test]
fn first_free_slot_skips_occupied_space() {
    let (mut p, seq, track, asset) = fixture();
    add_clip(&mut p, seq, track, asset, 0, 10);
    add_clip(&mut p, seq, track, asset, 15, 10);
    let t = p.sequence(seq).unwrap().track(track).unwrap();

    // A 3 s clip fits in the 10..15 gap.
    assert_eq!(t.first_free_slot(Ticks::ZERO, Ticks::from_seconds(3)), Ticks::from_seconds(10));
    // A 10 s clip does not, so it lands after everything.
    assert_eq!(
        t.first_free_slot(Ticks::ZERO, Ticks::from_seconds(10)),
        Ticks::from_seconds(25)
    );
}

#[test]
fn assets_in_use_cannot_be_removed() {
    let (mut p, seq, track, asset) = fixture();
    add_clip(&mut p, seq, track, asset, 0, 10);
    assert_eq!(p.remove_asset(asset), Err(CoreError::AssetInUse { id: asset, clips: 1 }));

    let clip_id = p.sequence(seq).unwrap().track(track).unwrap().clips()[0].id;
    p.sequence_mut(seq).unwrap().track_mut(track).unwrap().remove_clip(clip_id).unwrap();
    assert!(p.remove_asset(asset).is_ok());
}

#[test]
fn importing_the_same_path_twice_can_be_deduplicated() {
    let (p, _, _, _) = fixture();
    assert!(p.asset_by_path(std::path::Path::new("/media/movie.mp4")).is_some());
    assert!(p.asset_by_path(std::path::Path::new("/media/other.mp4")).is_none());
}

#[test]
fn sequence_duration_is_the_longest_track() {
    let (mut p, seq, _, asset) = fixture();
    let v1 = p.sequence(seq).unwrap().tracks[0].id;
    let v2 = p.sequence(seq).unwrap().tracks[1].id;
    add_clip(&mut p, seq, v1, asset, 0, 10);
    add_clip(&mut p, seq, v2, asset, 5, 20);
    assert_eq!(p.sequence(seq).unwrap().duration(), Ticks::from_seconds(25));
}

#[test]
fn the_playhead_always_lands_on_a_frame_boundary() {
    let (mut p, seq, _, _) = fixture();
    let s = p.sequence_mut(seq).unwrap();
    s.settings.rate = Rate::FPS_29_97;
    s.set_playhead(Ticks::from_secs_f64(1.234_567));
    assert_eq!(s.playhead, s.settings.rate.snap_round(s.playhead));
    s.set_playhead(-Ticks::from_seconds(5));
    assert_eq!(s.playhead, Ticks::ZERO);
}

#[test]
fn snapping_prefers_the_nearest_edge_and_ignores_the_dragged_clip() {
    let (mut p, seq, track, asset) = fixture();
    let a = add_clip(&mut p, seq, track, asset, 0, 10);
    add_clip(&mut p, seq, track, asset, 20, 10);
    let s = p.sequence(seq).unwrap();
    let tol = Ticks::from_seconds(1);

    // Just short of the second clip's head: snaps to it.
    assert_eq!(
        s.snap_candidate(Ticks::from_millis(19_600), tol, None),
        Some(Ticks::from_seconds(20))
    );
    // Nothing within tolerance.
    assert_eq!(s.snap_candidate(Ticks::from_seconds(15), tol, None), None);
    // The dragged clip's own edges are excluded.
    assert_eq!(s.snap_candidate(Ticks::from_millis(9_800), tol, Some(a)), None);
    assert_eq!(
        s.snap_candidate(Ticks::from_millis(9_800), tol, None),
        Some(Ticks::from_seconds(10))
    );
}

#[test]
fn markers_stay_sorted() {
    let (mut p, seq, _, _) = fixture();
    let ids: Vec<_> = (0..4).map(|_| p.new_marker_id()).collect();
    let s = p.sequence_mut(seq).unwrap();
    for (i, &t) in [30i64, 10, 20, 0].iter().enumerate() {
        s.add_marker(Marker::new(ids[i], format!("m{t}"), Ticks::from_seconds(t)));
    }
    let times: Vec<_> = s.markers.iter().map(|m| m.time).collect();
    assert!(times.windows(2).all(|w| w[0] <= w[1]), "{times:?}");
    assert!(s.remove_marker(ids[0]).is_some());
    assert_eq!(s.markers.len(), 3);
}

// ---- animation ---------------------------------------------------------

#[test]
fn a_property_with_no_keyframes_is_a_constant() {
    let p = Property::constant(0.5f64);
    assert!(!p.is_animated());
    assert_eq!(p.evaluate(Ticks::ZERO), 0.5);
    assert_eq!(p.evaluate(Ticks::from_seconds(100)), 0.5);
}

#[test]
fn linear_interpolation_hits_the_expected_midpoint() {
    let mut p = Property::constant(0.0f64);
    p.set_keyframe(Ticks::ZERO, 0.0, Interpolation::Linear);
    p.set_keyframe(Ticks::from_seconds(2), 1.0, Interpolation::Linear);
    assert_eq!(p.evaluate(Ticks::from_seconds(1)), 0.5);
    assert_eq!(p.evaluate(Ticks::from_millis(500)), 0.25);
}

#[test]
fn values_are_clamped_outside_the_keyframed_range_never_extrapolated() {
    let mut p = Property::constant(0.0f64);
    p.set_keyframe(Ticks::from_seconds(1), 10.0, Interpolation::Linear);
    p.set_keyframe(Ticks::from_seconds(2), 20.0, Interpolation::Linear);
    assert_eq!(p.evaluate(Ticks::ZERO), 10.0);
    assert_eq!(p.evaluate(-Ticks::from_seconds(100)), 10.0);
    assert_eq!(p.evaluate(Ticks::from_seconds(100)), 20.0);
}

#[test]
fn hold_interpolation_steps_rather_than_ramps() {
    let mut p = Property::constant(0.0f64);
    p.set_keyframe(Ticks::ZERO, 0.0, Interpolation::Hold);
    p.set_keyframe(Ticks::from_seconds(2), 1.0, Interpolation::Linear);
    assert_eq!(p.evaluate(Ticks::from_seconds(1)), 0.0);
    assert_eq!(p.evaluate(Ticks::from_seconds(2) - Ticks(1)), 0.0);
    assert_eq!(p.evaluate(Ticks::from_seconds(2)), 1.0);
}

#[test]
fn easing_modes_are_monotonic_and_pinned_at_both_ends() {
    for mode in [
        Interpolation::Linear,
        Interpolation::EaseIn,
        Interpolation::EaseOut,
        Interpolation::EaseInOut,
        Interpolation::Bezier { x1: 0.9, y1: 0.0, x2: 0.1, y2: 1.0 },
    ] {
        assert!(mode.ease(0.0).abs() < 1e-6, "{mode:?} does not start at 0");
        assert!((mode.ease(1.0) - 1.0).abs() < 1e-6, "{mode:?} does not end at 1");
        let mut prev = f64::NEG_INFINITY;
        for i in 0..=100 {
            let v = mode.ease(i as f64 / 100.0);
            assert!(v >= prev - 1e-9, "{mode:?} is not monotonic at t={i}");
            prev = v;
        }
    }
    // Ease-in starts slow, so it lags linear at the midpoint.
    assert!(Interpolation::EaseIn.ease(0.5) < 0.5);
    assert!(Interpolation::EaseOut.ease(0.5) > 0.5);
    // Out-of-range input is clamped rather than extrapolated.
    assert_eq!(Interpolation::Linear.ease(-1.0), 0.0);
    assert_eq!(Interpolation::Linear.ease(2.0), 1.0);
}

#[test]
fn keyframes_stay_sorted_and_a_repeat_time_replaces() {
    let mut p = Property::constant(0.0f64);
    p.set_keyframe(Ticks::from_seconds(3), 3.0, Interpolation::Linear);
    p.set_keyframe(Ticks::from_seconds(1), 1.0, Interpolation::Linear);
    p.set_keyframe(Ticks::from_seconds(2), 2.0, Interpolation::Linear);
    let times: Vec<_> = p.keyframes().iter().map(|k| k.time).collect();
    assert_eq!(
        times,
        vec![Ticks::from_seconds(1), Ticks::from_seconds(2), Ticks::from_seconds(3)]
    );

    let displaced = p.set_keyframe(Ticks::from_seconds(2), 9.0, Interpolation::Linear);
    assert_eq!(displaced.unwrap().value, 2.0);
    assert_eq!(p.keyframes().len(), 3);
    assert_eq!(p.evaluate(Ticks::from_seconds(2)), 9.0);

    assert!(p.remove_keyframe(Ticks::from_seconds(2)).is_some());
    assert!(p.remove_keyframe(Ticks::from_seconds(2)).is_none());
    assert_eq!(p.keyframes().len(), 2);
}

#[test]
fn one_animation_system_serves_every_value_type() {
    let mut pos = Property::constant(Vec2::ZERO);
    pos.set_keyframe(Ticks::ZERO, Vec2::new(0.0, 0.0), Interpolation::Linear);
    pos.set_keyframe(Ticks::from_seconds(2), Vec2::new(100.0, -50.0), Interpolation::Linear);
    assert_eq!(pos.evaluate(Ticks::from_seconds(1)), Vec2::new(50.0, -25.0));

    let mut col = Property::constant(Rgba::BLACK);
    col.set_keyframe(Ticks::ZERO, Rgba::BLACK, Interpolation::Linear);
    col.set_keyframe(Ticks::from_seconds(2), Rgba::WHITE, Interpolation::Linear);
    let mid = col.evaluate(Ticks::from_seconds(1));
    assert!((mid.r - 0.5).abs() < 1e-9 && (mid.a - 1.0).abs() < 1e-9);
}

#[test]
fn freezing_replaces_animation_with_the_value_at_that_moment() {
    let mut p = Property::constant(0.0f64);
    p.set_keyframe(Ticks::ZERO, 0.0, Interpolation::Linear);
    p.set_keyframe(Ticks::from_seconds(2), 1.0, Interpolation::Linear);
    p.freeze_at(Ticks::from_seconds(1));
    assert!(!p.is_animated());
    assert_eq!(p.value, 0.5);
    assert_eq!(p.evaluate(Ticks::from_seconds(100)), 0.5);
}

#[test]
fn set_keyframes_repairs_unsorted_and_duplicated_input() {
    let mut p = Property::constant(0.0f64);
    p.set_keyframes(vec![
        Keyframe::new(Ticks::from_seconds(2), 2.0),
        Keyframe::new(Ticks::from_seconds(1), 1.0),
        Keyframe::new(Ticks::from_seconds(2), 99.0),
    ]);
    let times: Vec<_> = p.keyframes().iter().map(|k| k.time).collect();
    assert_eq!(times, vec![Ticks::from_seconds(1), Ticks::from_seconds(2)]);
}

#[test]
fn a_default_transform_is_the_identity() {
    let t = Transform::default();
    assert!(t.is_identity());
    let s = t.evaluate(Ticks::ZERO);
    assert_eq!(s.scale, Vec2::ONE);
    assert_eq!(s.opacity, 1.0);

    let mut animated = Transform::default();
    animated.opacity.set_keyframe(Ticks::ZERO, 0.0, Interpolation::Linear);
    animated.opacity.set_keyframe(Ticks::from_seconds(1), 1.0, Interpolation::Linear);
    assert!(!animated.is_identity());
    assert!(animated.is_animated());
}

#[test]
fn opacity_is_clamped_when_evaluated() {
    let over = Transform { opacity: Property::constant(5.0), ..Default::default() };
    assert_eq!(over.evaluate(Ticks::ZERO).opacity, 1.0);
    let under = Transform { opacity: Property::constant(-1.0), ..Default::default() };
    assert_eq!(under.evaluate(Ticks::ZERO).opacity, 0.0);
}

#[test]
fn effect_parameters_are_animatable_through_the_same_system() {
    let mut effect = Effect::new(EffectId::from_raw(1), "verge.blur.gaussian", "Gaussian Blur")
        .with_param("radius", ParamValue::scalar(0.0));
    assert!(!effect.is_animated());

    if let Some(ParamValue::Scalar(p)) = effect.param_mut("radius") {
        p.set_keyframe(Ticks::ZERO, 0.0, Interpolation::Linear);
        p.set_keyframe(Ticks::from_seconds(1), 20.0, Interpolation::Linear);
    }
    assert!(effect.is_animated());
    assert_eq!(
        effect.param("radius").unwrap().as_scalar_at(Ticks::from_millis(500)),
        Some(10.0)
    );
    assert!(effect.param("missing").is_none());
}

#[test]
fn normalise_repairs_a_hand_edited_project() {
    let (mut p, seq, track, asset) = fixture();
    add_clip(&mut p, seq, track, asset, 0, 10);
    // Simulate a file that references an asset that is not in the project and
    // that claims IDs above the allocator's counter.
    let orphan = p.new_clip_id();
    {
        let t = p.sequence_mut(seq).unwrap().track_mut(track).unwrap();
        t.insert_clip(Clip::new(
            orphan,
            AssetId::from_raw(9_999),
            "orphan",
            Ticks::ZERO,
            Ticks::from_seconds(20),
            Ticks::from_seconds(5),
        ))
        .unwrap();
    }
    let warnings = p.normalise();
    assert_eq!(warnings.len(), 1, "{warnings:?}");
    assert!(warnings[0].contains("missing asset"), "{warnings:?}");
    // The clip is kept, not silently dropped.
    assert_eq!(p.sequence(seq).unwrap().track(track).unwrap().len(), 2);
    assert!(p.ids.peek() > 9_999, "allocator must move past IDs seen in the file");
}

#[test]
fn clip_count_reflects_every_track() {
    let (mut p, seq, _, asset) = fixture();
    let v1 = p.sequence(seq).unwrap().tracks[0].id;
    let v2 = p.sequence(seq).unwrap().tracks[1].id;
    for i in 0..5 {
        add_clip(&mut p, seq, v1, asset, i * 10, 5);
        add_clip(&mut p, seq, v2, asset, i * 10, 5);
    }
    assert_eq!(p.clip_count(), 10);
}

#[test]
fn retiming_states_the_destination_rather_than_a_delta() {
    let mut p = Property::constant(0.0f64);
    p.set_keyframe(Ticks::ZERO, 0.0, Interpolation::Linear);
    p.set_keyframe(Ticks::from_seconds(1), 1.0, Interpolation::Linear);
    p.set_keyframe(Ticks::from_seconds(2), 2.0, Interpolation::Linear);

    // The same gesture restated twice lands in the same place, which is what
    // lets a drag re-issue itself on every pointer move.
    let stretched = [Ticks::ZERO, Ticks::from_seconds(2), Ticks::from_seconds(4)];
    assert!(p.set_keyframe_times(&stretched));
    assert!(p.set_keyframe_times(&stretched));
    let times: Vec<_> = p.keyframes().iter().map(|k| k.time).collect();
    assert_eq!(times, stretched);
    // Values travel with their keyframes rather than staying put in time.
    assert_eq!(p.evaluate(Ticks::from_seconds(2)), 1.0);

    // A count that does not match is refused outright, leaving the property as
    // it was rather than half-retimed.
    assert!(!p.set_keyframe_times(&[Ticks::ZERO]));
    assert_eq!(p.keyframes().len(), 3);
}

#[test]
fn retiming_two_keyframes_onto_one_tick_collapses_them() {
    let mut p = Property::constant(0.0f64);
    p.set_keyframe(Ticks::ZERO, 0.0, Interpolation::Linear);
    p.set_keyframe(Ticks::from_seconds(1), 1.0, Interpolation::Linear);
    assert!(p.set_keyframe_times(&[Ticks::ZERO, Ticks::ZERO]));
    assert_eq!(p.keyframes().len(), 1);
    // The later of the pair wins, as it does everywhere a time collides.
    assert_eq!(p.keyframes()[0].value, 1.0);
}

#[test]
fn interpolation_is_changed_on_the_keyframe_the_segment_leaves() {
    let mut p = Property::constant(0.0f64);
    p.set_keyframe(Ticks::ZERO, 0.0, Interpolation::Linear);
    p.set_keyframe(Ticks::from_seconds(2), 1.0, Interpolation::Linear);

    assert_eq!(
        p.set_interpolation(Ticks::ZERO, Interpolation::Hold),
        Some(Interpolation::Linear)
    );
    assert_eq!(p.evaluate(Ticks::from_seconds(1)), 0.0);
    // Nothing is at one second, so there is nothing to set there.
    assert_eq!(p.set_interpolation(Ticks::from_seconds(1), Interpolation::Linear), None);
    assert_eq!(p.keyframe_at(Ticks::ZERO).unwrap().interpolation, Interpolation::Hold);
    assert!(p.keyframe_at(Ticks::from_seconds(1)).is_none());
}

#[test]
fn a_preset_promotes_to_the_bezier_that_draws_the_same_curve() {
    for preset in Interpolation::ALL {
        let promoted = preset.to_bezier();
        for step in 0..=10 {
            let t = step as f64 / 10.0;
            assert!(
                (preset.ease(t) - promoted.ease(t)).abs() < 1e-9,
                "{preset:?} changed shape when promoted"
            );
        }
    }
    // Hold has no curve to promote: it stays stepped rather than quietly
    // becoming an interpolation.
    assert_eq!(Interpolation::Hold.to_bezier(), Interpolation::Hold);
    assert_eq!(Interpolation::Hold.with_handle(true, 0.5, 0.5), Interpolation::Hold);
}

#[test]
fn dragging_a_handle_keeps_the_other_one_and_stays_inside_the_segment() {
    let curve = Interpolation::Linear.with_handle(true, 2.0, 1.5);
    let Interpolation::Bezier { x1, y1, x2, y2 } = curve else { panic!("{curve:?}") };
    // Time may not run backwards inside a segment, so x is clamped...
    assert_eq!((x1, y1), (1.0, 1.5));
    // ...but the value may overshoot, which is what an elastic move is made of.
    assert_eq!((x2, y2), (1.0, 1.0));

    let both = curve.with_handle(false, 0.25, -0.5);
    let Interpolation::Bezier { x1, y1, x2, y2 } = both else { panic!("{both:?}") };
    assert_eq!((x1, y1, x2, y2), (1.0, 1.5, 0.25, -0.5));
}

// ---- motion blur --------------------------------------------------------

#[test]
fn the_shutter_samples_the_middle_of_each_slice_it_stands_for() {
    use ve_core::MotionBlur;

    let frame = Ticks::from_rational(1, 30);
    let blur = MotionBlur::new(180.0, 4);
    let offsets = blur.offsets(frame);

    assert_eq!(offsets.len(), 4);
    // Half a frame, centred on it: the smear sits about where the layer would
    // have been drawn rather than trailing behind it.
    assert_eq!(blur.open_for(frame), frame.scale(1, 2));
    let first = offsets[0];
    let last = offsets[3];
    assert!(first.is_negative() && !last.is_negative(), "{offsets:?}");
    assert_eq!(first.raw(), -last.raw(), "the samples must be symmetric");
    // Sampling the ends instead would weight the extremes twice, so the first
    // sample sits an eighth of the open interval in, not at its edge.
    assert_eq!(first, -frame.scale(3, 16));
}

#[test]
fn a_closed_shutter_asks_for_no_samples_at_all() {
    use ve_core::MotionBlur;

    let frame = Ticks::from_rational(1, 30);
    assert!(MotionBlur { enabled: false, ..MotionBlur::default() }.offsets(frame).is_empty());
    assert!(MotionBlur::new(0.0, 8).offsets(frame).is_empty());
    assert!(!MotionBlur::new(0.0, 8).is_active());

    // The bounds are enforced on the way in rather than being trusted.
    assert_eq!(MotionBlur::new(1000.0, 999).shutter_angle, MotionBlur::MAX_ANGLE);
    assert_eq!(MotionBlur::new(180.0, 999).samples, MotionBlur::MAX_SAMPLES);
    assert_eq!(MotionBlur::new(180.0, 0).samples, MotionBlur::MIN_SAMPLES);
}

#[test]
fn a_full_shutter_covers_the_whole_frame_interval() {
    use ve_core::MotionBlur;

    let frame = Ticks::from_rational(1, 24);
    let blur = MotionBlur::new(360.0, 2);
    assert_eq!(blur.open_for(frame), frame);
    let offsets = blur.offsets(frame);
    assert_eq!(offsets, vec![-frame.scale(1, 4), frame.scale(1, 4)]);
}

// ---- proxies ----------------------------------------------------------
//
// A proxy is a stand-in for an asset's *picture*. These assert the rules that
// decide which file the editor decodes from, which is the one piece of proxy
// behaviour the rest of the editor depends on being right.

/// An asset whose proxy points at `proxy_path`, which need not exist.
fn asset_with_proxy(proxy_path: &std::path::Path, original: &std::path::Path) -> MediaAsset {
    let mut asset = MediaAsset::new(AssetId::from_raw(1), original, MediaInfo::default());
    asset.proxy = Some(ProxyMedia::new(proxy_path, Size::new(640, 360)));
    asset
}

#[test]
fn the_editor_decodes_a_proxy_only_when_proxies_are_switched_on() {
    let dir = std::env::temp_dir().join("verge-proxy-model-on");
    std::fs::create_dir_all(&dir).unwrap();
    let proxy = dir.join("clip.proxy.mov");
    let original = dir.join("clip.mp4");
    std::fs::write(&proxy, b"proxy").unwrap();
    std::fs::write(&original, b"original").unwrap();

    let asset = asset_with_proxy(&proxy, &original);

    let on = asset.picture_source(None, true);
    assert!(on.is_proxy);
    assert_eq!(on.path, proxy);

    // The switch is the whole point: the real picture is still one toggle away.
    let off = asset.picture_source(None, false);
    assert!(!off.is_proxy);
    assert_eq!(off.path, original);

    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn a_proxy_whose_file_has_gone_falls_back_to_the_original() {
    let dir = std::env::temp_dir().join("verge-proxy-model-gone");
    std::fs::create_dir_all(&dir).unwrap();
    let original = dir.join("clip.mp4");
    std::fs::write(&original, b"original").unwrap();
    // Never created: a cleared cache directory, or a project moved without it.
    let proxy = dir.join("clip.proxy.mov");

    let asset = asset_with_proxy(&proxy, &original);
    let source = asset.picture_source(None, true);

    // Not offline, and not an error — the editor loses its own scratch file,
    // not the user's footage, so it carries on at full resolution.
    assert!(!source.is_proxy);
    assert_eq!(source.path, original);
    assert!(asset.has_proxy(), "the proxy is remembered, so restoring it works");

    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn an_asset_with_no_proxy_is_unaffected_by_the_switch() {
    let asset = MediaAsset::new(AssetId::from_raw(1), "/media/movie.mp4", MediaInfo::default());
    assert!(!asset.has_proxy());
    for enabled in [true, false] {
        let source = asset.picture_source(None, enabled);
        assert!(!source.is_proxy);
        assert_eq!(source.path, std::path::Path::new("/media/movie.mp4"));
    }
}

#[test]
fn relinking_moves_a_proxy_with_its_asset() {
    let root = std::path::Path::new("/projects/cut");
    let mut asset = asset_with_proxy(
        std::path::Path::new("/projects/cut/proxies/movie.mov"),
        std::path::Path::new("/projects/cut/media/movie.mp4"),
    );

    asset.relink_relative_to(root);

    assert_eq!(asset.relative_path.as_deref(), Some(std::path::Path::new("media/movie.mp4")));
    assert_eq!(
        asset.proxy.as_ref().unwrap().relative_path.as_deref(),
        Some(std::path::Path::new("proxies/movie.mov")),
        "a proxy left absolute is the one file a moved project cannot find"
    );
}

#[test]
fn a_project_written_before_proxies_existed_opens_with_none() {
    // The field is defaulted rather than versioned, so this is the whole
    // compatibility story: an asset object with no `proxy` key at all.
    let json = r#"{
        "id": 1,
        "name": "movie.mp4",
        "path": "/media/movie.mp4",
        "info": {"duration": 0, "container": ""}
    }"#;
    let asset: MediaAsset = serde_json::from_str(json).unwrap();
    assert!(!asset.has_proxy());

    // And an absent proxy is not written back out, so saving does not grow
    // every asset in every older project by an empty field.
    let text = serde_json::to_string(&asset).unwrap();
    assert!(!text.contains("proxy"), "{text}");
}
