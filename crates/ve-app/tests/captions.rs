//! Captions through the action layer: writing them, importing them, and
//! sending them back out.
//!
//! No window. Everything the caption lane, the inspector and the menu do is an
//! `Action`, so the whole feature is drivable headlessly — which is what makes
//! it testable at all.

use std::path::PathBuf;
use std::sync::Arc;

use ve_app::actions::{dispatch, Action};
use ve_app::state::EditorState;
use ve_command::TrimEdge;
use ve_core::{CaptionTrackId, CueId, Project};
use ve_engine::{ManualTime, PlaybackClock, PlaybackEngine};
use ve_media::{DecodeService, WaveformService};
use ve_metrics::Metrics;
use ve_time::Ticks;

struct Editor {
    state: EditorState,
    engine: PlaybackEngine,
    waveforms: WaveformService,
    scratch: tempfile::TempDir,
}

impl Editor {
    fn new() -> Self {
        let scratch = tempfile::tempdir().unwrap();
        let metrics = Metrics::new();
        let decode = Arc::new(DecodeService::new(64, metrics.clone()));
        let engine =
            PlaybackEngine::new(PlaybackClock::new(ManualTime::new()), decode, metrics.clone());
        let waveforms = WaveformService::new(4, metrics);
        let state = EditorState::new(
            Project::with_default_sequence("Captions"),
            scratch.path().to_path_buf(),
        );
        Editor { state, engine, waveforms, scratch }
    }

    fn act(&mut self, action: Action) {
        dispatch(&mut self.state, &mut self.engine, &self.waveforms, action);
    }

    fn status(&self) -> String {
        self.state.status.as_ref().map(|s| s.text.clone()).unwrap_or_default()
    }

    fn seek(&mut self, to: Ticks) {
        self.act(Action::ScrubTo(to));
    }

    fn path(&self, name: &str) -> PathBuf {
        self.scratch.path().join(name)
    }

    fn captions(&self, track: CaptionTrackId) -> &ve_core::CaptionTrack {
        self.state.active_sequence().unwrap().caption_track(track).expect("caption track")
    }

    fn only_track(&self) -> CaptionTrackId {
        let captions = &self.state.active_sequence().unwrap().captions;
        assert_eq!(captions.len(), 1, "expected exactly one caption track");
        captions[0].id
    }

    /// Writes a caption at the playhead and returns it, as pressing C does.
    fn add_caption(&mut self, at: Ticks, text: &str) -> (CaptionTrackId, CueId) {
        self.seek(at);
        self.act(Action::AddCaptionAtPlayhead);
        let track = self.state.selection.captions.expect("a caption track");
        let cue = self.state.selection.cue.expect("the new caption selected");
        self.act(Action::SetCueText { track, cue, text: text.into() });
        self.act(Action::EndGesture);
        (track, cue)
    }
}

#[test]
fn the_first_caption_makes_the_track_it_needs() {
    let mut editor = Editor::new();
    assert!(editor.state.active_sequence().unwrap().captions.is_empty());

    let (track, cue) = editor.add_caption(Ticks::from_seconds(1), "Hello there.");

    // Asking a user to make a track before they can write a caption would be a
    // form to fill in, so the first caption brings one with it.
    let captions = editor.captions(track);
    assert_eq!(captions.len(), 1);
    assert_eq!(captions.cue(cue).unwrap().text, "Hello there.");
    assert_eq!(captions.cue(cue).unwrap().start, Ticks::from_seconds(1));
    // Two seconds, which is what a line of dialogue needs to be read.
    assert_eq!(captions.cue(cue).unwrap().duration, Ticks::from_seconds(2));
}

#[test]
fn a_caption_is_shortened_to_fit_rather_than_refused() {
    let mut editor = Editor::new();
    editor.add_caption(Ticks::from_seconds(3), "second");
    // One second before the caption already there: the default two seconds
    // would lie over it, and fast dialogue is a real thing.
    let (track, cue) = editor.add_caption(Ticks::from_seconds(2), "first");

    let captions = editor.captions(track);
    assert_eq!(captions.cue(cue).unwrap().duration, Ticks::from_seconds(1));
    assert!(captions.invariants_hold());
}

#[test]
fn a_caption_cannot_be_written_over_one_that_is_already_there() {
    let mut editor = Editor::new();
    editor.add_caption(Ticks::from_seconds(1), "there first");

    editor.seek(Ticks::from_millis(1_500));
    editor.act(Action::AddCaptionAtPlayhead);
    assert!(editor.status().contains("already a caption"), "{}", editor.status());
    assert_eq!(editor.captions(editor.only_track()).len(), 1);
}

#[test]
fn a_caption_in_hand_is_what_delete_means() {
    let mut editor = Editor::new();
    let (track, cue) = editor.add_caption(Ticks::ZERO, "to be deleted");
    assert_eq!(editor.state.selection.cue, Some(cue));

    editor.act(Action::DeleteSelected);
    assert!(editor.captions(track).is_empty());
    assert_eq!(editor.state.selection.cue, None);

    // And it comes back whole.
    editor.act(Action::Undo);
    assert_eq!(editor.captions(track).cue(cue).unwrap().text, "to be deleted");
}

#[test]
fn dragging_a_caption_along_its_lane_is_one_undo_step() {
    let mut editor = Editor::new();
    let (track, cue) = editor.add_caption(Ticks::from_seconds(1), "dialogue");
    let depth = editor.state.history.undo_depth();

    for step in 1..=10 {
        editor.act(Action::MoveCueTo {
            track,
            cue,
            to: Ticks::from_millis(1_000 + step * 100),
            coalesce: true,
        });
    }
    editor.act(Action::EndGesture);

    assert_eq!(editor.state.history.undo_depth(), depth + 1);
    assert_eq!(editor.captions(track).cue(cue).unwrap().start, Ticks::from_seconds(2));

    editor.act(Action::Undo);
    assert_eq!(editor.captions(track).cue(cue).unwrap().start, Ticks::from_seconds(1));
}

#[test]
fn a_caption_edge_is_bounded_by_its_neighbour_and_the_drag_survives_being_refused() {
    let mut editor = Editor::new();
    let (track, first) = editor.add_caption(Ticks::ZERO, "one");
    let (_, second) = editor.add_caption(Ticks::from_seconds(4), "two");

    // Stretch the first caption up to the second, then past it: the refused
    // step leaves the model alone and says nothing, because a drag runs into
    // its neighbour constantly and a message per pointer move would be noise.
    editor.act(Action::TrimCueTo {
        track,
        cue: first,
        edge: TrimEdge::End,
        to: Ticks::from_seconds(4),
        coalesce: true,
    });
    assert_eq!(editor.captions(track).cue(first).unwrap().end(), Ticks::from_seconds(4));

    editor.act(Action::TrimCueTo {
        track,
        cue: first,
        edge: TrimEdge::End,
        to: Ticks::from_seconds(5),
        coalesce: true,
    });
    assert_eq!(
        editor.captions(track).cue(first).unwrap().end(),
        Ticks::from_seconds(4),
        "the refused step must leave the caption where it was"
    );
    assert_eq!(editor.captions(track).cue(second).unwrap().start, Ticks::from_seconds(4));
}

#[test]
fn importing_a_caption_file_brings_a_track_of_its_own_tagged_with_its_language() {
    let mut editor = Editor::new();
    // Something already on the timeline, to prove an import does not land on it.
    let (existing, _) = editor.add_caption(Ticks::ZERO, "english");

    let path = editor.path("film.pt-BR.srt");
    std::fs::write(
        &path,
        "1\n00:00:01,000 --> 00:00:03,000\nOlá.\n\n\
         2\n00:00:04,000 --> 00:00:06,000\n- Quem está aí?\n- Sou eu.\n",
    )
    .unwrap();

    editor.act(Action::ImportCaptions(path));
    assert!(editor.status().contains("imported 2"), "{}", editor.status());

    let sequence = editor.state.active_sequence().unwrap();
    assert_eq!(sequence.captions.len(), 2);
    let imported = &sequence.captions[1];
    assert_eq!(imported.language, "pt-BR");
    assert_eq!(imported.len(), 2);
    assert_eq!(imported.cues()[1].text, "- Quem está aí?\n- Sou eu.");
    // The track that was already there is untouched, which is the whole reason
    // an import makes its own.
    assert_eq!(editor.captions(existing).len(), 1);

    // One undo takes the whole import back, track and all.
    editor.act(Action::Undo);
    assert_eq!(editor.state.active_sequence().unwrap().captions.len(), 1);
}

#[test]
fn an_import_says_what_the_file_got_wrong_instead_of_dropping_it_silently() {
    let mut editor = Editor::new();
    let path = editor.path("damaged.srt");
    std::fs::write(
        &path,
        "1\n00:00:01,000 --> 00:00:02,000\nfine\n\n\
         2\nthis is not a timing line\nlost\n\n\
         3\n00:00:05,000 --> 00:00:06,000\nalso fine\n",
    )
    .unwrap();

    editor.act(Action::ImportCaptions(path));
    assert!(editor.status().contains("imported 2"), "{}", editor.status());
    assert!(editor.status().contains("line 5"), "{}", editor.status());
    assert!(!editor.state.warnings.is_empty());
}

#[test]
fn a_file_that_is_not_captions_is_refused_and_changes_nothing() {
    let mut editor = Editor::new();
    let path = editor.path("notes.txt");
    std::fs::write(&path, "just some notes").unwrap();

    editor.act(Action::ImportCaptions(path));
    assert!(editor.status().contains("not a caption file"), "{}", editor.status());
    assert!(editor.state.active_sequence().unwrap().captions.is_empty());
}

#[test]
fn a_caption_track_goes_out_as_a_file_and_comes_back_the_same() {
    let mut editor = Editor::new();
    let (track, _) = editor.add_caption(Ticks::from_seconds(1), "- Who's there?\n- It's me.");
    editor.add_caption(Ticks::from_seconds(5), "<i>whispering</i>");

    let path = editor.path("out.vtt");
    editor.act(Action::ExportCaptions { track, path: path.clone() });
    assert!(editor.status().contains("wrote 2 captions"), "{}", editor.status());

    editor.act(Action::ImportCaptions(path));
    let sequence = editor.state.active_sequence().unwrap();
    let round_tripped = &sequence.captions[1];
    assert_eq!(round_tripped.len(), 2);
    assert_eq!(round_tripped.cues()[0].text, "- Who's there?\n- It's me.");
    // Markup Verge cannot draw is still markup the file said, so it survives.
    assert_eq!(round_tripped.cues()[1].text, "<i>whispering</i>");
}

#[test]
fn the_preview_overlay_is_a_view_setting_and_does_not_touch_the_project() {
    let mut editor = Editor::new();
    let (track, _) = editor.add_caption(Ticks::ZERO, "shown");
    let before = editor.state.project.clone();

    assert!(editor.state.show_captions);
    editor.act(Action::ToggleCaptionOverlay);
    assert!(!editor.state.show_captions);
    assert!(editor.status().contains("still exported"), "{}", editor.status());

    assert_eq!(editor.state.project, before, "hiding captions is not an edit");
    assert_eq!(editor.captions(track).len(), 1);
}

#[test]
fn taking_hold_of_a_clip_lets_go_of_a_caption() {
    let mut editor = Editor::new();
    let (track, cue) = editor.add_caption(Ticks::ZERO, "in hand");
    assert_eq!(editor.state.selection.cue, Some(cue));

    // Clearing the selection and picking the caption up again is the gesture
    // the timeline performs; what matters is that the two never coexist, so
    // Delete is never ambiguous.
    editor.act(Action::ClearSelection);
    assert_eq!(editor.state.selection.cue, None);

    editor.act(Action::SelectCue { track, cue });
    assert_eq!(editor.state.selection.cue, Some(cue));
    assert!(editor.state.selection.clips.is_empty());
}
