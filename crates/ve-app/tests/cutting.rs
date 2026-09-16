//! Phase 2 cutting: ripple delete, close gap, and the clipboard.
//!
//! Driven through the same action layer the menus and keyboard use, with no
//! window — see `editor_workflow.rs` for why that is the point of routing
//! everything through one dispatch.

use std::path::PathBuf;
use std::sync::Arc;

use ve_app::actions::{dispatch, Action};
use ve_app::state::EditorState;
use ve_command::TrackFlag;
use ve_core::{AssetId, ClipId, Project, TrackId, TrackKind};
use ve_engine::{ManualTime, PlaybackClock, PlaybackEngine};
use ve_media::DecodeService;
use ve_metrics::Metrics;
use ve_time::Ticks;

/// The committed fixture is exactly three seconds long.
const CLIP: i64 = 3;

fn testdata(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../testdata").join(name)
}

fn secs(n: i64) -> Ticks {
    Ticks::from_seconds(n)
}

struct Editor {
    state: EditorState,
    engine: PlaybackEngine,
    _scratch: tempfile::TempDir,
}

impl Editor {
    fn new() -> Self {
        let scratch = tempfile::tempdir().unwrap();
        let metrics = Metrics::new();
        let decode = Arc::new(DecodeService::new(64, metrics.clone()));
        let engine =
            PlaybackEngine::new(PlaybackClock::new(ManualTime::new()), decode, metrics);
        let state = EditorState::new(
            Project::with_default_sequence("Untitled"),
            scratch.path().to_path_buf(),
        );
        Editor { state, engine, _scratch: scratch }
    }

    /// An editor with `n` back-to-back video clips on V1, starting at zero.
    fn with_clips(n: usize) -> Self {
        let mut editor = Editor::new();
        editor.act(Action::ImportMedia(vec![testdata("counter_30fps.mp4")]));
        let asset = editor.video_asset();
        let v1 = editor.track(TrackKind::Video, 0);
        for _ in 0..n {
            editor.act(Action::AddAssetToTimeline { asset, track: v1, at: Ticks::ZERO });
        }
        assert_eq!(editor.state.project.clip_count(), n, "{}", editor.status());
        editor.act(Action::ClearSelection);
        editor
    }

    /// `n` one-second clips end to end, each showing the middle second of the
    /// three-second fixture.
    ///
    /// Roll, slip and slide all spend *unused source*: a clip already showing
    /// every frame its media has cannot give a neighbour any more, so an editor
    /// built from whole fixture clips has nothing for them to move.
    fn with_spare_source(n: usize) -> Self {
        let mut editor = Editor::new();
        editor.act(Action::ImportMedia(vec![testdata("counter_30fps.mp4")]));
        let asset = editor.video_asset();
        let v1 = editor.track(TrackKind::Video, 0);
        for i in 0..n {
            let id = editor.state.project.new_clip_id();
            let clip = ve_core::Clip::new(id, asset, "clip", secs(1), secs(i as i64), secs(1));
            let track = editor.state.project.active_mut().unwrap().track_mut(v1).unwrap();
            track.insert_clip(clip).unwrap();
        }
        editor
    }

    fn act(&mut self, action: Action) {
        dispatch(&mut self.state, &mut self.engine, action);
    }

    fn status(&self) -> String {
        self.state.status.as_ref().map(|s| s.text.clone()).unwrap_or_default()
    }

    fn sequence(&self) -> &ve_core::Sequence {
        self.state.active_sequence().unwrap()
    }

    fn video_asset(&self) -> AssetId {
        self.state.project.assets.iter().find(|a| a.info.has_video()).unwrap().id
    }

    fn track(&self, kind: TrackKind, nth: usize) -> TrackId {
        self.sequence().tracks.iter().filter(|t| t.kind == kind).nth(nth).unwrap().id
    }

    /// Clip start times on a track, in whole seconds.
    fn starts(&self, track: TrackId) -> Vec<i64> {
        self.sequence()
            .track(track)
            .unwrap()
            .clips()
            .iter()
            .map(|c| c.timeline_start.as_secs_f64().round() as i64)
            .collect()
    }

    fn clip_ids(&self, track: TrackId) -> Vec<ClipId> {
        self.sequence().track(track).unwrap().clips().iter().map(|c| c.id).collect()
    }

    fn select(&mut self, clips: &[ClipId], track: TrackId) {
        self.act(Action::ClearSelection);
        for clip in clips {
            self.act(Action::SelectClip { clip: *clip, track, additive: true });
        }
    }
}

// ---- deleting ---------------------------------------------------------

#[test]
fn deleting_several_clips_is_a_single_undo_step() {
    let mut editor = Editor::with_clips(3);
    let v1 = editor.track(TrackKind::Video, 0);
    let ids = editor.clip_ids(v1);
    editor.select(&ids, v1);

    let depth = editor.state.history.undo_depth();
    editor.act(Action::DeleteSelected);
    assert_eq!(editor.state.project.clip_count(), 0, "{}", editor.status());
    assert_eq!(
        editor.state.history.undo_depth(),
        depth + 1,
        "three deletes should be one entry"
    );

    editor.act(Action::Undo);
    assert_eq!(editor.state.project.clip_count(), 3);
    assert_eq!(editor.starts(v1), vec![0, CLIP, CLIP * 2]);
}

#[test]
fn a_plain_delete_leaves_the_gap_where_the_clip_was() {
    let mut editor = Editor::with_clips(3);
    let v1 = editor.track(TrackKind::Video, 0);
    let ids = editor.clip_ids(v1);
    editor.select(&ids[1..2], v1);

    editor.act(Action::DeleteSelected);
    assert_eq!(editor.starts(v1), vec![0, CLIP * 2]);
}

#[test]
fn a_ripple_delete_closes_the_hole_behind_it() {
    let mut editor = Editor::with_clips(3);
    let v1 = editor.track(TrackKind::Video, 0);
    let ids = editor.clip_ids(v1);
    editor.select(&ids[1..2], v1);

    editor.act(Action::RippleDeleteSelected);
    assert_eq!(editor.starts(v1), vec![0, CLIP], "{}", editor.status());
    assert_eq!(editor.clip_ids(v1), vec![ids[0], ids[2]]);

    editor.act(Action::Undo);
    assert_eq!(editor.starts(v1), vec![0, CLIP, CLIP * 2], "one undo should restore both");
}

#[test]
fn a_ripple_delete_of_several_clips_lands_them_all_correctly() {
    let mut editor = Editor::with_clips(4);
    let v1 = editor.track(TrackKind::Video, 0);
    let ids = editor.clip_ids(v1);
    // Two non-adjacent clips at once: removing the later one first is what
    // keeps the earlier one's captured position true when its turn comes.
    editor.select(&[ids[0], ids[2]], v1);

    editor.act(Action::RippleDeleteSelected);
    assert_eq!(editor.starts(v1), vec![0, CLIP], "{}", editor.status());
    assert_eq!(editor.clip_ids(v1), vec![ids[1], ids[3]]);

    editor.act(Action::Undo);
    assert_eq!(editor.clip_ids(v1), ids);
    assert_eq!(editor.starts(v1), vec![0, CLIP, CLIP * 2, CLIP * 3]);
}

#[test]
fn deleting_nothing_says_so_and_changes_nothing() {
    let mut editor = Editor::with_clips(1);
    let depth = editor.state.history.undo_depth();

    editor.act(Action::DeleteSelected);
    assert!(editor.status().contains("nothing selected"), "{}", editor.status());
    editor.act(Action::RippleDeleteSelected);
    assert!(editor.status().contains("nothing selected"), "{}", editor.status());
    assert_eq!(editor.state.history.undo_depth(), depth);
    assert_eq!(editor.state.project.clip_count(), 1);
}

// ---- closing gaps -----------------------------------------------------

#[test]
fn closing_a_gap_pulls_the_rest_of_the_track_back() {
    let mut editor = Editor::with_clips(3);
    let v1 = editor.track(TrackKind::Video, 0);
    let ids = editor.clip_ids(v1);
    editor.select(&ids[1..2], v1);
    editor.act(Action::DeleteSelected);
    assert_eq!(editor.starts(v1), vec![0, CLIP * 2]);

    editor.act(Action::ScrubTo(secs(CLIP + 1)));
    editor.act(Action::CloseGapAtPlayhead);
    assert_eq!(editor.starts(v1), vec![0, CLIP], "{}", editor.status());

    editor.act(Action::Undo);
    assert_eq!(editor.starts(v1), vec![0, CLIP * 2]);
}

#[test]
fn closing_a_gap_where_there_is_none_says_so() {
    let mut editor = Editor::with_clips(2);
    editor.act(Action::ScrubTo(secs(1)));
    let depth = editor.state.history.undo_depth();

    editor.act(Action::CloseGapAtPlayhead);
    assert!(editor.status().contains("no gap"), "{}", editor.status());
    assert_eq!(editor.state.history.undo_depth(), depth);
}

#[test]
fn closing_a_gap_skips_locked_tracks() {
    let mut editor = Editor::with_clips(3);
    let v1 = editor.track(TrackKind::Video, 0);
    let ids = editor.clip_ids(v1);
    editor.select(&ids[1..2], v1);
    editor.act(Action::DeleteSelected);

    editor.state.project.active_mut().unwrap().track_mut(v1).unwrap().locked = true;
    editor.act(Action::ScrubTo(secs(CLIP + 1)));
    editor.act(Action::CloseGapAtPlayhead);

    assert!(editor.status().contains("no gap"), "{}", editor.status());
    assert_eq!(editor.starts(v1), vec![0, CLIP * 2], "a locked track was rippled");
}

// ---- the clipboard ----------------------------------------------------

#[test]
fn copy_and_paste_makes_an_independent_clip() {
    let mut editor = Editor::with_clips(1);
    let v1 = editor.track(TrackKind::Video, 0);
    let original = editor.clip_ids(v1)[0];
    editor.select(&[original], v1);

    editor.act(Action::Copy);
    assert_eq!(editor.state.clipboard.len(), 1);

    editor.act(Action::ScrubTo(secs(10)));
    editor.act(Action::Paste);
    assert_eq!(editor.state.project.clip_count(), 2, "{}", editor.status());

    let ids = editor.clip_ids(v1);
    assert_eq!(editor.starts(v1), vec![0, 10]);
    assert_ne!(ids[1], original, "a paste must mint a new clip, not alias the original");
    assert_eq!(editor.state.selection.clips, vec![ids[1]], "the paste should be selected");

    // The same copy pastes again; the clipboard is not consumed.
    editor.act(Action::ScrubTo(secs(20)));
    editor.act(Action::Paste);
    assert_eq!(editor.starts(v1), vec![0, 10, 20]);
    let ids = editor.clip_ids(v1);
    assert_ne!(ids[1], ids[2]);
}

#[test]
fn pasting_keeps_the_shape_of_what_was_copied() {
    let mut editor = Editor::with_clips(3);
    let v1 = editor.track(TrackKind::Video, 0);
    let v2 = editor.track(TrackKind::Video, 1);
    let asset = editor.video_asset();
    // A clip on the track below, offset from the first — the copy has to
    // reproduce both the spacing and the track relationship.
    editor.act(Action::AddAssetToTimeline { asset, track: v2, at: secs(CLIP) });

    let mut selection = vec![editor.clip_ids(v1)[0]];
    selection.extend(editor.clip_ids(v2));
    editor.select(&selection, v1);
    editor.act(Action::Copy);
    assert_eq!(editor.state.clipboard.len(), 2);

    // Paste with V1 as the target, well clear of the originals.
    editor.state.selection.track = Some(v1);
    editor.act(Action::ScrubTo(secs(30)));
    editor.act(Action::Paste);

    assert_eq!(editor.starts(v1), vec![0, CLIP, CLIP * 2, 30], "{}", editor.status());
    assert_eq!(editor.starts(v2), vec![CLIP, 30 + CLIP], "the offset was not preserved");
}

#[test]
fn cutting_lifts_the_clips_but_keeps_them_on_the_clipboard() {
    let mut editor = Editor::with_clips(2);
    let v1 = editor.track(TrackKind::Video, 0);
    let ids = editor.clip_ids(v1);
    editor.select(&ids[0..1], v1);

    editor.act(Action::Cut);
    assert_eq!(editor.starts(v1), vec![CLIP], "cut should lift, not ripple");
    assert_eq!(editor.state.clipboard.len(), 1);

    editor.act(Action::ScrubTo(secs(20)));
    editor.act(Action::Paste);
    assert_eq!(editor.starts(v1), vec![CLIP, 20], "{}", editor.status());
}

#[test]
fn pasting_onto_occupied_space_is_refused_whole() {
    let mut editor = Editor::with_clips(2);
    let v1 = editor.track(TrackKind::Video, 0);
    let ids = editor.clip_ids(v1);
    editor.select(&ids, v1);
    editor.act(Action::Copy);

    editor.act(Action::ScrubTo(secs(1)));
    let before = editor.state.project.clone();
    editor.act(Action::Paste);

    assert!(editor.status().contains("land on top"), "{}", editor.status());
    assert_eq!(
        editor.state.project, before,
        "a refused paste changed the project, allocator included"
    );
}

#[test]
fn pasting_an_empty_clipboard_says_so() {
    let mut editor = Editor::with_clips(1);
    let depth = editor.state.history.undo_depth();
    editor.act(Action::Paste);
    assert!(editor.status().contains("clipboard is empty"), "{}", editor.status());
    assert_eq!(editor.state.history.undo_depth(), depth);
}

#[test]
fn a_paste_that_needs_more_tracks_than_there_are_is_refused() {
    let mut editor = Editor::with_clips(1);
    let v1 = editor.track(TrackKind::Video, 0);
    let v2 = editor.track(TrackKind::Video, 1);
    let asset = editor.video_asset();
    editor.act(Action::AddAssetToTimeline { asset, track: v2, at: Ticks::ZERO });

    let mut selection = editor.clip_ids(v1);
    selection.extend(editor.clip_ids(v2));
    editor.select(&selection, v1);
    editor.act(Action::Copy);

    // Two tracks' worth of clips, aimed at the last track of all: there is
    // nothing below it to take the second one, and finding that out must not
    // consume IDs.
    let last = editor.sequence().tracks.last().unwrap().id;
    editor.state.selection.track = Some(last);
    let ids_before = editor.state.project.ids.peek();
    editor.act(Action::ScrubTo(secs(20)));
    editor.act(Action::Paste);

    assert!(editor.status().contains("more tracks"), "{}", editor.status());
    assert_eq!(editor.state.project.ids.peek(), ids_before, "a refused paste minted IDs");
}

#[test]
fn a_clip_cannot_be_pasted_onto_a_track_of_the_wrong_kind() {
    let mut editor = Editor::with_clips(1);
    let v1 = editor.track(TrackKind::Video, 0);
    let a1 = editor.track(TrackKind::Audio, 0);
    editor.select(&editor.clip_ids(v1).clone(), v1);
    editor.act(Action::Copy);

    editor.state.selection.track = Some(a1);
    editor.act(Action::ScrubTo(secs(20)));
    editor.act(Action::Paste);

    assert!(editor.status().contains("cannot be pasted"), "{}", editor.status());
    assert_eq!(editor.state.project.clip_count(), 1);
}

#[test]
fn copying_nothing_leaves_the_clipboard_alone() {
    let mut editor = Editor::with_clips(1);
    let v1 = editor.track(TrackKind::Video, 0);
    editor.select(&editor.clip_ids(v1).clone(), v1);
    editor.act(Action::Copy);
    assert_eq!(editor.state.clipboard.len(), 1);

    editor.act(Action::ClearSelection);
    editor.act(Action::Copy);
    assert!(editor.status().contains("nothing selected"), "{}", editor.status());
    assert_eq!(
        editor.state.clipboard.len(),
        1,
        "a copy with nothing selected threw away what was already on the clipboard"
    );
}

// ---- selection --------------------------------------------------------

#[test]
fn select_all_takes_every_clip_on_every_track() {
    let mut editor = Editor::with_clips(2);
    let v2 = editor.track(TrackKind::Video, 1);
    let asset = editor.video_asset();
    editor.act(Action::AddAssetToTimeline { asset, track: v2, at: Ticks::ZERO });

    editor.act(Action::SelectAll);
    assert_eq!(editor.state.selection.clips.len(), 3, "{}", editor.status());

    editor.act(Action::DeleteSelected);
    assert_eq!(editor.state.project.clip_count(), 0);
    editor.act(Action::Undo);
    assert_eq!(editor.state.project.clip_count(), 3, "one undo should bring all three back");
}

#[test]
fn splitting_across_tracks_is_a_single_undo_step() {
    let mut editor = Editor::with_clips(1);
    let v2 = editor.track(TrackKind::Video, 1);
    let asset = editor.video_asset();
    editor.act(Action::AddAssetToTimeline { asset, track: v2, at: Ticks::ZERO });

    let depth = editor.state.history.undo_depth();
    editor.act(Action::ScrubTo(secs(1)));
    editor.act(Action::SplitAtPlayhead);
    assert_eq!(editor.state.project.clip_count(), 4, "{}", editor.status());
    assert_eq!(editor.state.history.undo_depth(), depth + 1);

    editor.act(Action::Undo);
    assert_eq!(editor.state.project.clip_count(), 2, "one undo should rejoin both tracks");
}

// ---- tracks -----------------------------------------------------------

#[test]
fn adding_and_deleting_a_track_undoes_with_its_clips_intact() {
    let mut editor = Editor::with_clips(2);
    let v1 = editor.track(TrackKind::Video, 0);
    let before = editor.sequence().tracks.len();

    editor.act(Action::AddTrack(TrackKind::Video));
    assert_eq!(editor.sequence().tracks.len(), before + 1, "{}", editor.status());
    assert!(editor.status().contains("V3"), "{}", editor.status());

    // Deleting a track takes its clips with it, and undo brings both back.
    editor.act(Action::RemoveTrack(v1));
    assert_eq!(editor.state.project.clip_count(), 0, "{}", editor.status());
    editor.act(Action::Undo);
    assert_eq!(editor.state.project.clip_count(), 2);
    assert_eq!(editor.starts(v1), vec![0, CLIP]);
}

#[test]
fn deleting_a_track_drops_its_clips_from_the_selection() {
    let mut editor = Editor::with_clips(2);
    let v1 = editor.track(TrackKind::Video, 0);
    editor.act(Action::SelectAll);
    assert_eq!(editor.state.selection.clips.len(), 2);

    editor.act(Action::RemoveTrack(v1));
    assert!(editor.state.selection.clips.is_empty(), "a deleted clip stayed selected");
    assert_eq!(editor.state.selection.track, None);
}

#[test]
fn track_switches_are_undoable_and_lock_really_locks() {
    let mut editor = Editor::with_clips(1);
    let v1 = editor.track(TrackKind::Video, 0);

    editor.act(Action::SetTrackFlag { track: v1, flag: TrackFlag::Muted, value: true });
    assert!(editor.sequence().track(v1).unwrap().muted);
    editor.act(Action::Undo);
    assert!(!editor.sequence().track(v1).unwrap().muted);

    editor.act(Action::SetTrackFlag { track: v1, flag: TrackFlag::Locked, value: true });
    let clip = editor.clip_ids(v1)[0];
    editor.select(&[clip], v1);
    editor.act(Action::DeleteSelected);
    assert_eq!(editor.state.project.clip_count(), 1, "a locked track was edited");
    assert!(editor.status().contains("locked"), "{}", editor.status());
}

#[test]
fn moving_a_track_swaps_it_with_its_neighbour_of_the_same_kind() {
    let mut editor = Editor::with_clips(1);
    let v1 = editor.track(TrackKind::Video, 0);
    let v2 = editor.track(TrackKind::Video, 1);
    let order = |e: &Editor| -> Vec<String> {
        e.sequence().tracks.iter().map(|t| t.name.clone()).collect()
    };
    assert_eq!(order(&editor), vec!["V1", "V2", "A1", "A2"]);

    // Video is drawn in reverse, so "toward the top" is a later index.
    editor.act(Action::MoveTrack { track: v1, toward_top: true });
    assert_eq!(order(&editor), vec!["V2", "V1", "A1", "A2"], "{}", editor.status());
    editor.act(Action::Undo);
    assert_eq!(order(&editor), vec!["V1", "V2", "A1", "A2"]);

    // Audio is drawn in order, so for it the directions are the other way
    // round — and neither kind is ever moved into the other's half.
    let a1 = editor.track(TrackKind::Audio, 0);
    editor.act(Action::MoveTrack { track: a1, toward_top: false });
    assert_eq!(order(&editor), vec!["V1", "V2", "A2", "A1"], "{}", editor.status());

    editor.act(Action::MoveTrack { track: v2, toward_top: true });
    assert!(editor.status().contains("already at the end"), "{}", editor.status());
}

// ---- markers ----------------------------------------------------------

#[test]
fn markers_are_added_at_the_playhead_and_navigated_between() {
    let mut editor = Editor::with_clips(2);
    for at in [1, 4] {
        editor.act(Action::ScrubTo(secs(at)));
        editor.act(Action::AddMarkerAtPlayhead);
    }
    assert_eq!(editor.sequence().markers.len(), 2, "{}", editor.status());

    // A second marker on the same frame is refused rather than stacked.
    editor.act(Action::AddMarkerAtPlayhead);
    assert!(editor.status().contains("already a marker"), "{}", editor.status());
    assert_eq!(editor.sequence().markers.len(), 2);

    editor.act(Action::ScrubTo(Ticks::ZERO));
    editor.act(Action::GoToMarker(1));
    assert_eq!(editor.sequence().playhead, secs(1));
    editor.act(Action::GoToMarker(1));
    assert_eq!(editor.sequence().playhead, secs(4));
    editor.act(Action::GoToMarker(1));
    assert!(editor.status().contains("no marker"), "{}", editor.status());
    assert_eq!(editor.sequence().playhead, secs(4), "a failed jump moved the playhead");

    editor.act(Action::GoToMarker(-1));
    assert_eq!(editor.sequence().playhead, secs(1));
}

#[test]
fn deleting_a_marker_undoes() {
    let mut editor = Editor::with_clips(1);
    editor.act(Action::ScrubTo(secs(1)));
    editor.act(Action::AddMarkerAtPlayhead);
    let marker = editor.sequence().markers[0].id;

    editor.act(Action::RemoveMarker(marker));
    assert!(editor.sequence().markers.is_empty(), "{}", editor.status());
    editor.act(Action::Undo);
    assert_eq!(editor.sequence().markers.len(), 1);
    assert_eq!(editor.sequence().markers[0].time, secs(1));
}

// ---- gestures ---------------------------------------------------------

#[test]
fn a_roll_gesture_moves_the_cut_and_is_one_undo_step() {
    let mut editor = Editor::with_spare_source(2);
    let v1 = editor.track(TrackKind::Video, 0);
    let ids = editor.clip_ids(v1);
    let length = editor.sequence().duration();
    let half = Ticks::from_millis(500);

    // Three pointer moves, as a drag would produce.
    for at in [secs(1) + half, secs(1) - half, secs(1) + half] {
        editor.act(Action::RollEditTo {
            left: ids[0],
            right: ids[1],
            track: v1,
            to: at,
            coalesce: true,
        });
    }
    editor.act(Action::EndGesture);

    let clips = editor.sequence().track(v1).unwrap().clips();
    assert_eq!(clips[0].duration, secs(1) + half, "{}", editor.status());
    assert_eq!(clips[1].timeline_start, secs(1) + half);
    assert_eq!(clips[1].source_in, secs(1) + half, "the right clip did not roll its source");
    assert_eq!(editor.sequence().duration(), length, "a roll changed the sequence length");

    editor.act(Action::Undo);
    let clips = editor.sequence().track(v1).unwrap().clips();
    assert_eq!(clips[0].duration, secs(1), "undo went to a mid-drag position");
    assert_eq!(clips[1].timeline_start, secs(1));
}

#[test]
fn a_roll_stops_where_the_source_runs_out() {
    let mut editor = Editor::with_clips(2);
    let v1 = editor.track(TrackKind::Video, 0);
    let ids = editor.clip_ids(v1);
    let before = editor.state.project.clone();

    // Both clips use every frame their media has, so the cut cannot move in
    // either direction.
    for at in [secs(CLIP + 1), secs(CLIP - 1)] {
        editor.act(Action::RollEditTo {
            left: ids[0],
            right: ids[1],
            track: v1,
            to: at,
            coalesce: true,
        });
    }
    assert_eq!(editor.state.project, before);
}

#[test]
fn a_slip_gesture_changes_the_frames_and_leaves_the_clip_where_it_is() {
    let mut editor = Editor::with_clips(1);
    let v1 = editor.track(TrackKind::Video, 0);
    let clip = editor.clip_ids(v1)[0];
    // Trim the head first, so the clip has source on both sides to slip into.
    editor.act(Action::TrimClipTo {
        clip,
        track: v1,
        edge: ve_command::TrimEdge::Start,
        to: secs(1),
        coalesce: false,
    });
    let before = editor.sequence().find_clip(clip).unwrap().1.clone();
    assert_eq!(before.source_in, secs(1));

    editor.act(Action::SlipClipTo {
        clip,
        track: v1,
        to_source_in: Ticks::ZERO,
        coalesce: true,
    });
    editor.act(Action::EndGesture);

    let after = editor.sequence().find_clip(clip).unwrap().1;
    assert_eq!(after.source_in, Ticks::ZERO, "{}", editor.status());
    assert_eq!(after.timeline_start, before.timeline_start, "a slip moved the clip");
    assert_eq!(after.duration, before.duration, "a slip changed the clip's length");

    editor.act(Action::Undo);
    assert_eq!(editor.sequence().find_clip(clip).unwrap().1.source_in, secs(1));
}

#[test]
fn a_slide_gesture_moves_the_clip_into_its_neighbours() {
    let mut editor = Editor::with_spare_source(3);
    let v1 = editor.track(TrackKind::Video, 0);
    let ids = editor.clip_ids(v1);
    let length = editor.sequence().duration();
    let half = Ticks::from_millis(500);

    editor.act(Action::SlideClipTo {
        clip: ids[1],
        track: v1,
        to: secs(1) + half,
        coalesce: true,
    });
    editor.act(Action::EndGesture);

    let clips = editor.sequence().track(v1).unwrap().clips();
    assert_eq!(clips[1].timeline_start, secs(1) + half, "{}", editor.status());
    assert_eq!(clips[1].duration, secs(1), "the slid clip changed length");
    assert_eq!(clips[1].source_in, secs(1), "the slid clip changed which frames it shows");
    assert_eq!(clips[0].duration, secs(1) + half, "the left neighbour did not absorb it");
    assert_eq!(clips[2].timeline_start, secs(2) + half);
    assert_eq!(clips[2].source_in, secs(1) + half, "the right neighbour gave up its head");
    assert_eq!(editor.sequence().duration(), length, "a slide changed the sequence length");

    editor.act(Action::Undo);
    assert_eq!(editor.starts(v1), vec![0, 1, 2]);
}

#[test]
fn a_refused_gesture_step_leaves_the_project_alone() {
    let mut editor = Editor::with_spare_source(3);
    let v1 = editor.track(TrackKind::Video, 0);
    let ids = editor.clip_ids(v1);
    let before = editor.state.project.clone();
    let depth = editor.state.history.undo_depth();

    // Far beyond what the neighbours can pay for: during a real drag this is
    // just the pointer having run ahead, so it is refused in silence.
    editor.act(Action::SlideClipTo { clip: ids[1], track: v1, to: secs(100), coalesce: true });
    assert_eq!(editor.state.project, before);
    assert_eq!(editor.state.history.undo_depth(), depth);
}

#[test]
fn a_marquee_takes_every_clip_it_covers() {
    let mut editor = Editor::with_clips(3);
    let v1 = editor.track(TrackKind::Video, 0);
    let v2 = editor.track(TrackKind::Video, 1);
    let asset = editor.video_asset();
    editor.act(Action::AddAssetToTimeline { asset, track: v2, at: Ticks::ZERO });
    let ids = editor.clip_ids(v1);

    // A rectangle over V1 only, covering the first two clips.
    editor.act(Action::SelectClipsIn {
        range: ve_time::TimeRange::from_bounds(Ticks::ZERO, secs(CLIP + 1)),
        tracks: vec![v1],
        additive: false,
    });
    assert_eq!(editor.state.selection.clips, vec![ids[0], ids[1]]);

    // Shrinking the rectangle lets the second one go again, because each step
    // rebuilds the selection rather than adding to it.
    editor.act(Action::SelectClipsIn {
        range: ve_time::TimeRange::from_bounds(Ticks::ZERO, secs(1)),
        tracks: vec![v1],
        additive: false,
    });
    assert_eq!(editor.state.selection.clips, vec![ids[0]]);

    // Both tracks, and a clip only partly covered still counts.
    editor.act(Action::SelectClipsIn {
        range: ve_time::TimeRange::from_bounds(secs(1), secs(2)),
        tracks: vec![v1, v2],
        additive: false,
    });
    assert_eq!(editor.state.selection.clips.len(), 2);

    editor.act(Action::SelectClipsIn {
        range: ve_time::TimeRange::from_bounds(secs(CLIP), secs(CLIP * 2)),
        tracks: vec![v1],
        additive: true,
    });
    assert_eq!(editor.state.selection.clips.len(), 3, "an additive sweep replaced instead");
}

#[test]
fn the_tool_is_named_when_it_changes() {
    let mut editor = Editor::new();
    assert_eq!(editor.state.tool, ve_app::state::TimelineTool::Select);

    editor.act(Action::SetTool(ve_app::state::TimelineTool::Slip));
    assert_eq!(editor.state.tool, ve_app::state::TimelineTool::Slip);
    assert!(editor.status().contains("Slip tool (Y)"), "{}", editor.status());
}
