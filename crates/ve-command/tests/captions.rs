//! Caption editing through the history.
//!
//! The command contract, on cues: a refused command touches nothing, an undo
//! restores the project exactly, a redo reproduces the edit including the IDs,
//! and a gesture collapses into one entry however many times it was re-issued.

use ve_command::*;
use ve_core::{CaptionTrackId, Cue, CueId, Project, SequenceId};
use ve_time::{Ticks, TimeRange};

/// A project with its ID allocator zeroed, for comparing two states — undo does
/// not rewind the allocator, by design.
fn shape(project: &Project) -> Project {
    let mut project = project.clone();
    project.ids = ve_core::IdAllocator::new();
    project
}

fn project() -> (Project, SequenceId) {
    let project = Project::with_default_sequence("Captions");
    let sequence = project.active_sequence.unwrap();
    (project, sequence)
}

fn span(start_ms: i64, length_ms: i64) -> TimeRange {
    TimeRange::new(Ticks::from_millis(start_ms), Ticks::from_millis(length_ms))
}

#[test]
fn a_caption_track_comes_and_goes_with_the_same_id() {
    let (mut project, sequence) = project();
    let mut history = History::default();

    let mut command = AddCaptionTrack::new(sequence);
    command.apply(&mut project).unwrap();
    let track = command.track_id().unwrap();
    command.undo(&mut project).unwrap();
    assert!(project.sequence(sequence).unwrap().captions.is_empty());
    command.apply(&mut project).unwrap();
    // Redo must reproduce the ID: a selection or an import pointing at the
    // track has to survive the round trip.
    assert_eq!(command.track_id(), Some(track));

    history
        .execute(&mut project, Box::new(AddCaptionTrack::new(sequence).with_language("pt-BR")))
        .unwrap();
    let captions = &project.sequence(sequence).unwrap().captions;
    assert_eq!(captions.len(), 2);
    assert_eq!(captions[1].language, "pt-BR");
    assert_eq!(captions[1].name, "C2");
}

#[test]
fn deleting_a_caption_track_puts_every_cue_back_on_undo() {
    let (mut project, sequence) = project();
    let mut history = History::default();

    let mut add = AddCaptionTrack::new(sequence);
    add.apply(&mut project).unwrap();
    let track = add.track_id().unwrap();
    for i in 0..5 {
        history
            .execute(
                &mut project,
                Box::new(AddCue::new(
                    sequence,
                    track,
                    span(i * 3_000, 2_000),
                    format!("line {i}"),
                )),
            )
            .unwrap();
    }
    let before = shape(&project);

    history.execute(&mut project, Box::new(RemoveCaptionTrack::new(sequence, track))).unwrap();
    assert!(project.sequence(sequence).unwrap().captions.is_empty());

    history.undo(&mut project).unwrap();
    assert_eq!(shape(&project), before);
}

#[test]
fn a_caption_laid_over_another_is_refused_and_the_project_is_untouched() {
    let (mut project, sequence) = project();
    let mut history = History::default();
    let mut add = AddCaptionTrack::new(sequence);
    add.apply(&mut project).unwrap();
    let track = add.track_id().unwrap();

    history
        .execute(
            &mut project,
            Box::new(AddCue::new(sequence, track, span(1_000, 2_000), "one")),
        )
        .unwrap();
    let before = shape(&project);

    let refused = history.execute(
        &mut project,
        Box::new(AddCue::new(sequence, track, span(2_000, 1_000), "two")),
    );
    assert!(refused.is_err(), "a cue may not be laid over another");
    assert_eq!(shape(&project), before);
    // A failed command is not pushed, so there is nothing of it to undo.
    assert_eq!(history.undo_depth(), 1);
}

#[test]
fn dragging_a_caption_is_one_undo_step_however_many_pointer_moves_it_took() {
    let (mut project, sequence) = project();
    let mut history = History::default();
    let mut add = AddCaptionTrack::new(sequence);
    add.apply(&mut project).unwrap();
    let track = add.track_id().unwrap();

    let mut add_cue = AddCue::new(sequence, track, span(1_000, 2_000), "dialogue");
    add_cue.apply(&mut project).unwrap();
    let cue = add_cue.cue_id().unwrap();
    let before = shape(&project);
    let depth = history.undo_depth();

    for step in 1..=20 {
        history
            .execute_coalesced(
                &mut project,
                Box::new(RetimeCue::moving(
                    sequence,
                    track,
                    cue,
                    Ticks::from_millis(1_000 + step * 50),
                    Ticks::from_millis(2_000),
                )),
            )
            .unwrap();
    }
    history.break_merge();

    assert_eq!(history.undo_depth(), depth + 1, "a drag is one entry");
    let moved = project.sequence(sequence).unwrap().captions[0].cues()[0].start;
    assert_eq!(moved, Ticks::from_millis(2_000));

    // And undo goes back to where the gesture started, not to the previous
    // pointer position.
    history.undo(&mut project).unwrap();
    assert_eq!(shape(&project), before);
}

#[test]
fn trimming_a_caption_is_bounded_by_its_neighbours_and_not_by_any_media() {
    let (mut project, sequence) = project();
    let mut history = History::default();
    let mut add = AddCaptionTrack::new(sequence);
    add.apply(&mut project).unwrap();
    let track = add.track_id().unwrap();

    let mut first = AddCue::new(sequence, track, span(0, 1_000), "one");
    first.apply(&mut project).unwrap();
    let mut second = AddCue::new(sequence, track, span(2_000, 1_000), "two");
    second.apply(&mut project).unwrap();
    let second_id = second.cue_id().unwrap();

    // A cue has no source to run out of, so it may be stretched to any length
    // the neighbours allow — here, right up to the cue before it.
    history
        .execute(
            &mut project,
            Box::new(RetimeCue::trimming(sequence, track, second_id, span(1_000, 4_000))),
        )
        .unwrap();
    let cue = project.sequence(sequence).unwrap().captions[0].cue(second_id).unwrap();
    assert_eq!(cue.range(), span(1_000, 4_000));

    // One tick further back and it would be over the cue before it.
    let before = shape(&project);
    assert!(history
        .execute(
            &mut project,
            Box::new(RetimeCue::trimming(sequence, track, second_id, span(999, 4_001)))
        )
        .is_err());
    assert_eq!(shape(&project), before);
}

#[test]
fn typing_a_line_of_dialogue_is_one_undo_step() {
    let (mut project, sequence) = project();
    let mut history = History::default();
    let mut add = AddCaptionTrack::new(sequence);
    add.apply(&mut project).unwrap();
    let track = add.track_id().unwrap();
    let mut add_cue = AddCue::new(sequence, track, span(0, 2_000), "");
    add_cue.apply(&mut project).unwrap();
    let cue = add_cue.cue_id().unwrap();

    let line = "- Who's there?";
    let depth = history.undo_depth();
    for i in 1..=line.len() {
        history
            .execute_coalesced(
                &mut project,
                Box::new(SetCueText::new(sequence, track, cue, &line[..i])),
            )
            .unwrap();
    }
    history.break_merge();

    assert_eq!(history.undo_depth(), depth + 1);
    assert_eq!(project.sequence(sequence).unwrap().captions[0].cue(cue).unwrap().text, line);

    history.undo(&mut project).unwrap();
    assert_eq!(project.sequence(sequence).unwrap().captions[0].cue(cue).unwrap().text, "");
}

#[test]
fn importing_a_transcription_is_one_command_and_one_undo() {
    let (mut project, sequence) = project();
    let mut history = History::default();
    let mut add = AddCaptionTrack::new(sequence);
    add.apply(&mut project).unwrap();
    let track = add.track_id().unwrap();

    // Something already on the track, so the undo has to restore rather than
    // merely empty.
    let mut existing = AddCue::new(sequence, track, span(0, 1_000), "was here first");
    existing.apply(&mut project).unwrap();
    let before = shape(&project);

    let cues: Vec<Cue> = (0..2_000)
        .map(|i| {
            Cue::new(
                project.new_cue_id(),
                Ticks::from_millis(i * 3_000),
                Ticks::from_millis(2_400),
                format!("line {i}"),
            )
        })
        .collect();
    let depth = history.undo_depth();
    history.execute(&mut project, Box::new(ReplaceCues::new(sequence, track, cues))).unwrap();

    assert_eq!(history.undo_depth(), depth + 1, "one entry, not two thousand");
    assert_eq!(project.sequence(sequence).unwrap().captions[0].len(), 2_000);

    history.undo(&mut project).unwrap();
    assert_eq!(shape(&project), before);
    history.redo(&mut project).unwrap();
    assert_eq!(project.sequence(sequence).unwrap().captions[0].len(), 2_000);
}

#[test]
fn an_import_of_overlapping_cues_lands_legally_rather_than_failing() {
    let (mut project, sequence) = project();
    let mut history = History::default();
    let mut add = AddCaptionTrack::new(sequence);
    add.apply(&mut project).unwrap();
    let track = add.track_id().unwrap();

    // A file where the first cue runs 500 ms into the second, and the third is
    // out of order — all ordinary, all repaired on the way in.
    let cues = vec![
        Cue::new(CueId::from_raw(90), Ticks::from_millis(0), Ticks::from_millis(1_500), "one"),
        Cue::new(
            CueId::from_raw(91),
            Ticks::from_millis(1_000),
            Ticks::from_millis(1_000),
            "two",
        ),
        Cue::new(
            CueId::from_raw(92),
            Ticks::from_millis(500),
            Ticks::from_millis(100),
            "aside",
        ),
    ];
    history.execute(&mut project, Box::new(ReplaceCues::new(sequence, track, cues))).unwrap();

    let captions = &project.sequence(sequence).unwrap().captions[0];
    assert_eq!(captions.len(), 3);
    assert!(captions.invariants_hold());
}

#[test]
fn a_caption_taken_off_a_track_comes_back_with_its_text() {
    let (mut project, sequence) = project();
    let mut history = History::default();
    let mut add = AddCaptionTrack::new(sequence);
    add.apply(&mut project).unwrap();
    let track = add.track_id().unwrap();
    let mut add_cue = AddCue::new(sequence, track, span(1_000, 2_000), "- It's me.");
    add_cue.apply(&mut project).unwrap();
    let cue = add_cue.cue_id().unwrap();
    let before = shape(&project);

    history.execute(&mut project, Box::new(RemoveCue::new(sequence, track, cue))).unwrap();
    assert!(project.sequence(sequence).unwrap().captions[0].is_empty());

    history.undo(&mut project).unwrap();
    assert_eq!(shape(&project), before);
}

#[test]
fn a_command_naming_a_caption_track_that_is_gone_fails_rather_than_panicking() {
    let (mut project, sequence) = project();
    let missing = CaptionTrackId::from_raw(999);
    let mut command = AddCue::new(sequence, missing, span(0, 1_000), "nowhere");
    assert!(matches!(
        command.apply(&mut project),
        Err(CommandError::CaptionTrackNotFound(id)) if id == missing
    ));
}
