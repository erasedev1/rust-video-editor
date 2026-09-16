//! Ripple, roll, slip and slide.
//!
//! These are the edits that touch more than one clip, so the thing worth
//! asserting is not only "the clip changed" but "everything around it is still
//! coherent, and undo puts all of it back". As in `undo_redo.rs`, most tests
//! compare whole projects rather than spot-checking fields.

use ve_command::*;
use ve_core::{
    AssetId, Clip, ClipId, MediaInfo, Project, SequenceId, Size, TrackId, TrackKind,
};
use ve_time::{Rate, Ticks};

fn secs(n: i64) -> Ticks {
    Ticks::from_seconds(n)
}

fn fixture() -> (Project, SequenceId, TrackId, AssetId) {
    let mut p = Project::with_default_sequence("Ripple Test");
    let asset = p.add_asset(
        "/media/a.mp4",
        MediaInfo {
            duration: secs(60),
            video: Some(ve_core::VideoStreamInfo {
                size: Size::new(1920, 1080),
                rate: Rate::FPS_30,
                duration: secs(60),
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

/// Lays down clips as `(source_in, timeline_start, duration)` in seconds.
fn lay_out(
    p: &mut Project,
    seq: SequenceId,
    track: TrackId,
    asset: AssetId,
    spans: &[(i64, i64, i64)],
) -> Vec<ClipId> {
    spans
        .iter()
        .map(|&(source_in, start, duration)| {
            let id = p.new_clip_id();
            let clip = Clip::new(
                id,
                asset,
                format!("clip@{start}"),
                secs(source_in),
                secs(start),
                secs(duration),
            );
            p.sequence_mut(seq).unwrap().track_mut(track).unwrap().insert_clip(clip).unwrap();
            id
        })
        .collect()
}

fn starts(p: &Project, seq: SequenceId, track: TrackId) -> Vec<i64> {
    p.sequence(seq)
        .unwrap()
        .track(track)
        .unwrap()
        .clips()
        .iter()
        .map(|c| c.timeline_start.as_secs_f64() as i64)
        .collect()
}

fn window(p: &Project, seq: SequenceId, clip: ClipId) -> (i64, i64, i64) {
    let (_, c) = p.sequence(seq).unwrap().find_clip(clip).unwrap();
    (
        c.source_in.as_secs_f64() as i64,
        c.timeline_start.as_secs_f64() as i64,
        c.duration.as_secs_f64() as i64,
    )
}

// ---- compound ---------------------------------------------------------

#[test]
fn a_compound_is_one_undo_step() {
    let (mut p, seq, track, asset) = fixture();
    let ids = lay_out(&mut p, seq, track, asset, &[(0, 0, 5), (0, 5, 5), (0, 10, 5)]);
    let before = p.clone();
    let mut h = History::default();

    let mut compound = Compound::new("Delete Clips");
    for id in &ids {
        compound.push(Box::new(RemoveClip::new(seq, track, *id)));
    }
    h.execute(&mut p, Box::new(compound)).unwrap();

    assert_eq!(p.clip_count(), 0);
    assert_eq!(h.undo_depth(), 1, "three deletes should be one history entry");

    h.undo(&mut p).unwrap();
    assert_eq!(p, before, "undoing the compound did not restore the project exactly");
}

#[test]
fn a_failing_compound_rolls_back_what_it_already_did() {
    let (mut p, seq, track, asset) = fixture();
    let ids = lay_out(&mut p, seq, track, asset, &[(0, 0, 5), (0, 5, 5)]);
    let before = p.clone();
    let mut h = History::default();

    // The second clip is deletable; the third target does not exist, so the
    // compound must fail *and* put the first clip back.
    let missing = p.new_clip_id();
    let after_mint = p.clone();
    let compound = Compound::new("Delete Clips")
        .with(Box::new(RemoveClip::new(seq, track, ids[0])))
        .with(Box::new(RemoveClip::new(seq, track, missing)));

    let err = h.execute(&mut p, Box::new(compound)).unwrap_err();
    assert!(matches!(err, CommandError::ClipNotFound(id) if id == missing));
    assert_eq!(p, after_mint, "a failed compound left the project half-edited");
    assert_eq!(h.undo_depth(), 0, "a failed compound must not reach the undo stack");
    assert_ne!(before.ids, p.ids, "the fixture should have minted an ID");
}

// ---- ripple -----------------------------------------------------------

#[test]
fn ripple_delete_closes_the_hole_and_undoes_as_one_step() {
    let (mut p, seq, track, asset) = fixture();
    let ids = lay_out(&mut p, seq, track, asset, &[(0, 0, 5), (0, 5, 5), (0, 10, 5)]);
    let before = p.clone();
    let mut h = History::default();

    let ripple = Compound::new("Ripple Delete")
        .with(Box::new(RemoveClip::new(seq, track, ids[1])))
        .with(Box::new(ShiftClips::new(seq, track, secs(10), secs(-5))));
    h.execute(&mut p, Box::new(ripple)).unwrap();

    assert_eq!(starts(&p, seq, track), vec![0, 5]);
    assert_eq!(window(&p, seq, ids[2]), (0, 5, 5), "the trailing clip kept its source");

    h.undo(&mut p).unwrap();
    assert_eq!(p, before);
}

#[test]
fn shifting_forward_makes_room_and_shifting_back_closes_it() {
    let (mut p, seq, track, asset) = fixture();
    lay_out(&mut p, seq, track, asset, &[(0, 0, 5), (0, 5, 5)]);
    let before = p.clone();
    let mut h = History::default();

    h.execute(&mut p, Box::new(ShiftClips::new(seq, track, secs(5), secs(3)))).unwrap();
    assert_eq!(starts(&p, seq, track), vec![0, 8]);

    h.undo(&mut p).unwrap();
    assert_eq!(p, before);
    h.redo(&mut p).unwrap();
    assert_eq!(starts(&p, seq, track), vec![0, 8]);
}

#[test]
fn a_shift_that_would_overlap_is_refused() {
    let (mut p, seq, track, asset) = fixture();
    lay_out(&mut p, seq, track, asset, &[(0, 0, 5), (0, 6, 5)]);
    let before = p.clone();
    let mut h = History::default();

    // Only one second of gap, so pulling the second clip back two is an overlap.
    let err = h
        .execute(&mut p, Box::new(ShiftClips::new(seq, track, secs(6), secs(-2))))
        .unwrap_err();
    assert!(matches!(err, CommandError::Core(ve_core::CoreError::ClipOverlap)));
    assert_eq!(p, before);
}

#[test]
fn undoing_a_shift_leaves_earlier_clips_alone() {
    let (mut p, seq, track, asset) = fixture();
    // A one-second gap before the shifted run: undoing must not sweep the
    // clip at 0 back along with it, which a naive `from + delta` anchor would.
    lay_out(&mut p, seq, track, asset, &[(0, 0, 5), (0, 6, 5)]);
    let before = p.clone();
    let mut h = History::default();

    h.execute(&mut p, Box::new(ShiftClips::new(seq, track, secs(6), secs(-1)))).unwrap();
    assert_eq!(starts(&p, seq, track), vec![0, 5]);
    h.undo(&mut p).unwrap();
    assert_eq!(p, before);
}

#[test]
fn closing_a_gap_uses_the_gap_the_track_reports() {
    let (mut p, seq, track, asset) = fixture();
    lay_out(&mut p, seq, track, asset, &[(0, 0, 5), (0, 9, 5)]);
    let mut h = History::default();

    let gap = p.sequence(seq).unwrap().track(track).unwrap().gap_at(secs(7)).unwrap();
    assert_eq!((gap.start, gap.duration), (secs(5), secs(4)));

    h.execute(&mut p, Box::new(ShiftClips::new(seq, track, gap.end(), -gap.duration))).unwrap();
    assert_eq!(starts(&p, seq, track), vec![0, 5]);
}

// ---- roll -------------------------------------------------------------

#[test]
fn a_roll_moves_the_cut_without_changing_the_total_length() {
    let (mut p, seq, track, asset) = fixture();
    let ids = lay_out(&mut p, seq, track, asset, &[(0, 0, 10), (10, 10, 10)]);
    let before = p.clone();
    let length = p.sequence(seq).unwrap().duration();
    let mut h = History::default();

    h.execute(&mut p, Box::new(RollEdit::new(seq, track, ids[0], ids[1], secs(13)))).unwrap();

    assert_eq!(window(&p, seq, ids[0]), (0, 0, 13), "the left clip should have grown");
    assert_eq!(window(&p, seq, ids[1]), (13, 13, 7), "the right clip rolled its source");
    assert_eq!(p.sequence(seq).unwrap().duration(), length, "a roll changed the length");

    h.undo(&mut p).unwrap();
    assert_eq!(p, before);
}

#[test]
fn a_roll_stops_at_the_end_of_the_source_media() {
    let (mut p, seq, track, asset) = fixture();
    // The left clip already ends at the last frame of a 60s asset, so there is
    // nothing left to roll into.
    let ids = lay_out(&mut p, seq, track, asset, &[(50, 0, 10), (0, 10, 10)]);
    let before = p.clone();
    let mut h = History::default();

    let err = h
        .execute(&mut p, Box::new(RollEdit::new(seq, track, ids[0], ids[1], secs(12))))
        .unwrap_err();
    assert!(matches!(err, CommandError::Core(ve_core::CoreError::TrimBeyondSource)));
    assert_eq!(p, before);
}

#[test]
fn a_roll_needs_two_clips_that_actually_meet() {
    let (mut p, seq, track, asset) = fixture();
    let ids = lay_out(&mut p, seq, track, asset, &[(0, 0, 10), (0, 15, 10)]);
    let mut h = History::default();

    let err = h
        .execute(&mut p, Box::new(RollEdit::new(seq, track, ids[0], ids[1], secs(12))))
        .unwrap_err();
    assert!(matches!(err, CommandError::Rejected(_)));
}

#[test]
fn rolling_a_drag_collapses_into_one_undo_step() {
    let (mut p, seq, track, asset) = fixture();
    let ids = lay_out(&mut p, seq, track, asset, &[(0, 0, 10), (10, 10, 10)]);
    let before = p.clone();
    let mut h = History::default();

    for at in [11, 12, 13] {
        h.execute_coalesced(
            &mut p,
            Box::new(RollEdit::new(seq, track, ids[0], ids[1], secs(at))),
        )
        .unwrap();
    }
    h.break_merge();

    assert_eq!(h.undo_depth(), 1, "a roll drag should be one entry");
    h.undo(&mut p).unwrap();
    assert_eq!(p, before, "undo went back to an intermediate pointer position");
}

// ---- slip -------------------------------------------------------------

#[test]
fn a_slip_changes_the_frames_but_not_the_position() {
    let (mut p, seq, track, asset) = fixture();
    let ids = lay_out(&mut p, seq, track, asset, &[(10, 0, 10)]);
    let before = p.clone();
    let mut h = History::default();

    h.execute(&mut p, Box::new(SlipClip::new(seq, track, ids[0], secs(20)))).unwrap();
    assert_eq!(window(&p, seq, ids[0]), (20, 0, 10));

    h.undo(&mut p).unwrap();
    assert_eq!(p, before);
}

#[test]
fn a_slip_cannot_run_off_either_end_of_the_source() {
    let (mut p, seq, track, asset) = fixture();
    let ids = lay_out(&mut p, seq, track, asset, &[(10, 0, 10)]);
    let before = p.clone();
    let mut h = History::default();

    // Before the first frame.
    assert!(h.execute(&mut p, Box::new(SlipClip::new(seq, track, ids[0], secs(-1)))).is_err());
    // Past the last: a 10s window cannot start at 55s of a 60s asset.
    assert!(h.execute(&mut p, Box::new(SlipClip::new(seq, track, ids[0], secs(55)))).is_err());
    assert_eq!(p, before);
}

// ---- slide ------------------------------------------------------------

#[test]
fn a_slide_pays_for_the_move_out_of_its_neighbours() {
    let (mut p, seq, track, asset) = fixture();
    let ids = lay_out(&mut p, seq, track, asset, &[(0, 0, 10), (10, 10, 10), (20, 20, 10)]);
    let before = p.clone();
    let length = p.sequence(seq).unwrap().duration();
    let mut h = History::default();

    h.execute(&mut p, Box::new(SlideClip::new(seq, track, ids[1], secs(13)))).unwrap();

    assert_eq!(window(&p, seq, ids[0]), (0, 0, 13), "the left neighbour should have grown");
    assert_eq!(
        window(&p, seq, ids[1]),
        (10, 13, 10),
        "the slid clip kept its frames and its length"
    );
    assert_eq!(window(&p, seq, ids[2]), (23, 23, 7), "the right neighbour gave up its head");
    assert_eq!(p.sequence(seq).unwrap().duration(), length, "a slide changed the length");

    h.undo(&mut p).unwrap();
    assert_eq!(p, before);
}

#[test]
fn a_slide_carries_its_gaps_along() {
    let (mut p, seq, track, asset) = fixture();
    // Two seconds of gap on each side of the middle clip.
    let ids = lay_out(&mut p, seq, track, asset, &[(0, 0, 10), (10, 12, 10), (20, 24, 10)]);
    let mut h = History::default();

    h.execute(&mut p, Box::new(SlideClip::new(seq, track, ids[1], secs(15)))).unwrap();

    assert_eq!(window(&p, seq, ids[0]), (0, 0, 13));
    assert_eq!(window(&p, seq, ids[1]), (10, 15, 10));
    assert_eq!(window(&p, seq, ids[2]), (23, 27, 7), "the trailing gap should be preserved");
}

#[test]
fn a_slide_stops_when_a_neighbour_would_vanish() {
    let (mut p, seq, track, asset) = fixture();
    let ids = lay_out(&mut p, seq, track, asset, &[(0, 0, 10), (10, 10, 10), (20, 20, 10)]);
    let before = p.clone();
    let mut h = History::default();

    // Sliding 11 seconds right would leave the right neighbour with nothing.
    let err =
        h.execute(&mut p, Box::new(SlideClip::new(seq, track, ids[1], secs(21)))).unwrap_err();
    assert!(matches!(err, CommandError::Core(ve_core::CoreError::TrimTooShort)));
    assert_eq!(p, before);
}

#[test]
fn a_slide_with_no_left_neighbour_stops_at_the_start_of_the_timeline() {
    let (mut p, seq, track, asset) = fixture();
    let ids = lay_out(&mut p, seq, track, asset, &[(10, 5, 10), (20, 15, 10)]);
    let before = p.clone();
    let mut h = History::default();

    assert!(h.execute(&mut p, Box::new(SlideClip::new(seq, track, ids[0], secs(-1)))).is_err());
    assert_eq!(p, before);

    h.execute(&mut p, Box::new(SlideClip::new(seq, track, ids[0], secs(0)))).unwrap();
    assert_eq!(window(&p, seq, ids[0]), (10, 0, 10));
    assert_eq!(window(&p, seq, ids[1]), (15, 10, 15), "the right neighbour took up the slack");
}

#[test]
fn a_locked_track_refuses_every_multi_clip_edit() {
    let (mut p, seq, track, asset) = fixture();
    let ids = lay_out(&mut p, seq, track, asset, &[(0, 0, 10), (10, 10, 10), (20, 20, 10)]);
    p.sequence_mut(seq).unwrap().track_mut(track).unwrap().locked = true;
    let before = p.clone();
    let mut h = History::default();

    let attempts: Vec<Box<dyn Command>> = vec![
        Box::new(ShiftClips::new(seq, track, secs(10), secs(1))),
        Box::new(RollEdit::new(seq, track, ids[0], ids[1], secs(12))),
        Box::new(SlipClip::new(seq, track, ids[1], secs(12))),
        Box::new(SlideClip::new(seq, track, ids[1], secs(12))),
    ];
    for attempt in attempts {
        let name = attempt.name().to_string();
        let err = h.execute(&mut p, attempt).unwrap_err();
        assert!(
            matches!(err, CommandError::Core(ve_core::CoreError::TrackLocked)),
            "{name} ignored the lock: {err}"
        );
    }
    assert_eq!(p, before);
}

#[test]
fn every_new_edit_survives_an_undo_redo_round_trip() {
    let (mut p, seq, track, asset) = fixture();
    let ids = lay_out(&mut p, seq, track, asset, &[(0, 0, 10), (10, 10, 10), (20, 20, 10)]);
    let mut h = History::default();

    let edits: Vec<Box<dyn Command>> = vec![
        Box::new(ShiftClips::new(seq, track, secs(20), secs(2))),
        Box::new(RollEdit::new(seq, track, ids[0], ids[1], secs(12))),
        Box::new(SlipClip::new(seq, track, ids[1], secs(14))),
        Box::new(SlideClip::new(seq, track, ids[1], secs(13))),
    ];
    for edit in edits {
        let name = edit.name().to_string();
        let before = p.clone();
        h.execute(&mut p, edit).unwrap();
        let after = p.clone();
        assert_ne!(before, after, "{name} changed nothing");
        h.undo(&mut p).unwrap();
        assert_eq!(p, before, "{name} did not undo exactly");
        h.redo(&mut p).unwrap();
        assert_eq!(p, after, "{name} did not redo exactly");
    }
    // Every track invariant still holds after the whole run.
    let seq_ref = p.sequence(seq).unwrap();
    assert!(seq_ref.tracks.iter().all(|t| t.invariants_hold()));
    assert_eq!(seq_ref.track(track).unwrap().len(), 3);
    assert_eq!(TrackKind::Video, seq_ref.track(track).unwrap().kind);
}
