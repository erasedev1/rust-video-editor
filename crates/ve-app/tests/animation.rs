//! Phase 5 driven through the same action layer the animation editor uses:
//! keyframing from the playhead, retiming, easing, and the two clipboards.
//!
//! No window. What is asserted is what a screenshot cannot show — that a drag
//! leaves one undo step, that Delete takes the keyframes rather than the clip,
//! and that a paste lands where the playhead is rather than where the copy came
//! from.

use std::path::PathBuf;
use std::sync::Arc;

use ve_app::actions::{dispatch, Action};
use ve_app::state::EditorState;
use ve_command::{property_ref, ClipProperty, PropertyValue};
use ve_core::{ClipId, Interpolation, Project, TrackId, TrackKind, Vec2};
use ve_engine::{ManualTime, PlaybackClock, PlaybackEngine};
use ve_media::{DecodeService, WaveformService};
use ve_metrics::Metrics;
use ve_time::Ticks;

fn testdata(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../testdata").join(name)
}

struct Editor {
    state: EditorState,
    engine: PlaybackEngine,
    waveforms: WaveformService,
    _scratch: tempfile::TempDir,
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
            Project::with_default_sequence("Untitled"),
            scratch.path().to_path_buf(),
        );
        Editor { state, engine, waveforms, _scratch: scratch }
    }

    fn act(&mut self, action: Action) {
        dispatch(&mut self.state, &mut self.engine, &self.waveforms, action);
    }

    fn status(&self) -> String {
        self.state.status.as_ref().map(|s| s.text.clone()).unwrap_or_default()
    }

    fn sequence(&self) -> &ve_core::Sequence {
        self.state.active_sequence().unwrap()
    }

    fn video_track(&self) -> TrackId {
        self.sequence().tracks.iter().find(|t| t.kind == TrackKind::Video).unwrap().id
    }

    /// Imports the counter fixture and places it, returning the clip.
    fn place(&mut self, track: TrackId, at: Ticks) -> ClipId {
        if self.state.project.assets.is_empty() {
            self.act(Action::ImportMedia(vec![testdata("counter_30fps.mp4")]));
            assert_eq!(self.state.project.assets.len(), 1, "{}", self.status());
        }
        let asset = self.state.project.assets[0].id;
        let before: Vec<ClipId> =
            self.sequence().tracks.iter().flat_map(|t| t.clips()).map(|c| c.id).collect();
        self.act(Action::AddAssetToTimeline { asset, track, at });
        self.sequence()
            .tracks
            .iter()
            .flat_map(|t| t.clips())
            .map(|c| c.id)
            .find(|id| !before.contains(id))
            .unwrap_or_else(|| panic!("the clip was not placed: {}", self.status()))
    }

    fn keyframe_times(&self, clip: ClipId, property: &ClipProperty) -> Vec<Ticks> {
        let (_, clip) = self.sequence().find_clip(clip).unwrap();
        property_ref(clip, property).unwrap().keyframe_times()
    }

    fn value_at(&self, clip: ClipId, property: &ClipProperty, at: Ticks) -> PropertyValue {
        let (_, clip) = self.sequence().find_clip(clip).unwrap();
        property_ref(clip, property).unwrap().evaluate(at)
    }

    /// Keyframes opacity at three points, scrubbing to each one first, as the
    /// ◆ button does.
    fn animate_opacity(&mut self, clip: ClipId) {
        for second in [0, 1, 2] {
            self.act(Action::ScrubTo(Ticks::from_seconds(second)));
            self.act(Action::ToggleKeyframeAtPlayhead {
                clip,
                property: ClipProperty::Opacity,
            });
        }
    }
}

#[test]
fn the_keyframe_button_both_adds_and_removes_at_the_playhead() {
    let mut editor = Editor::new();
    let v1 = editor.video_track();
    let clip = editor.place(v1, Ticks::ZERO);

    editor.act(Action::ScrubTo(Ticks::from_seconds(1)));
    editor.act(Action::ToggleKeyframeAtPlayhead { clip, property: ClipProperty::Opacity });
    assert_eq!(
        editor.keyframe_times(clip, &ClipProperty::Opacity),
        vec![Ticks::from_seconds(1)],
        "{}",
        editor.status()
    );
    // Pinning a value changes nothing about the picture until something else
    // moves, so the property still reads what it did.
    assert_eq!(
        editor.value_at(clip, &ClipProperty::Opacity, Ticks::from_seconds(1)),
        PropertyValue::Scalar(1.0)
    );
    // The new keyframe is what the user is now holding.
    assert!(editor.state.keyframes.contains(&ClipProperty::Opacity, Ticks::from_seconds(1)));

    editor.act(Action::ToggleKeyframeAtPlayhead { clip, property: ClipProperty::Opacity });
    assert!(editor.keyframe_times(clip, &ClipProperty::Opacity).is_empty());
    assert!(editor.state.keyframes.is_empty(), "a keyframe that is gone cannot stay selected");
}

#[test]
fn keyframing_away_from_a_clip_says_so_rather_than_keying_nothing() {
    let mut editor = Editor::new();
    let v1 = editor.video_track();
    let clip = editor.place(v1, Ticks::from_seconds(5));

    editor.act(Action::ScrubTo(Ticks::from_seconds(1)));
    editor.act(Action::ToggleKeyframeAtPlayhead { clip, property: ClipProperty::Opacity });
    assert!(editor.keyframe_times(clip, &ClipProperty::Opacity).is_empty());
    assert!(editor.status().contains("not over that clip"), "{}", editor.status());
}

#[test]
fn a_keyframe_lands_on_the_frame_grid_even_when_the_playhead_does_not() {
    let mut editor = Editor::new();
    let v1 = editor.video_track();
    let clip = editor.place(v1, Ticks::ZERO);

    // A scrub leaves the playhead wherever the pointer was, which is not on a
    // frame.
    let rate = editor.sequence().rate();
    let between = rate.frame_to_ticks(30) + Ticks::new(1234);
    editor.act(Action::ScrubTo(between));
    editor.act(Action::ToggleKeyframeAtPlayhead { clip, property: ClipProperty::Position });

    assert_eq!(
        editor.keyframe_times(clip, &ClipProperty::Position),
        vec![rate.frame_to_ticks(30)],
        "a keyframe off the grid never quite lands on the frame it looks like"
    );
}

#[test]
fn dragging_keyframes_is_one_undo_step_and_the_selection_follows_them() {
    let mut editor = Editor::new();
    let v1 = editor.video_track();
    let clip = editor.place(v1, Ticks::ZERO);
    editor.animate_opacity(clip);
    let depth = editor.state.history.undo_depth();

    // Select the last two and slide them right, as a drag does: one action per
    // pointer move, each stating where every keyframe now is.
    editor.act(Action::SelectKeyframe {
        clip,
        property: ClipProperty::Opacity,
        at: Ticks::from_seconds(1),
        additive: false,
    });
    editor.act(Action::SelectKeyframe {
        clip,
        property: ClipProperty::Opacity,
        at: Ticks::from_seconds(2),
        additive: true,
    });

    for offset in [200, 400, 500] {
        let shift = Ticks::from_millis(offset);
        editor.act(Action::SetKeyframeTimes {
            clip,
            times: vec![(
                ClipProperty::Opacity,
                vec![
                    Ticks::ZERO,
                    Ticks::from_seconds(1) + shift,
                    Ticks::from_seconds(2) + shift,
                ],
            )],
            coalesce: true,
        });
    }
    editor.act(Action::EndGesture);

    assert_eq!(
        editor.keyframe_times(clip, &ClipProperty::Opacity),
        vec![Ticks::ZERO, Ticks::from_millis(1_500), Ticks::from_millis(2_500)]
    );
    assert_eq!(editor.state.history.undo_depth(), depth + 1, "a drag is one step");
    // The keyframes in hand are the same ones, at their new times.
    assert_eq!(
        editor.state.keyframes.times_on(&ClipProperty::Opacity),
        vec![Ticks::from_millis(1_500), Ticks::from_millis(2_500)]
    );

    editor.act(Action::Undo);
    assert_eq!(
        editor.keyframe_times(clip, &ClipProperty::Opacity),
        vec![Ticks::ZERO, Ticks::from_seconds(1), Ticks::from_seconds(2)]
    );
}

#[test]
fn delete_takes_the_selected_keyframes_rather_than_the_clip() {
    let mut editor = Editor::new();
    let v1 = editor.video_track();
    let clip = editor.place(v1, Ticks::ZERO);
    editor.animate_opacity(clip);

    editor.act(Action::SelectKeyframe {
        clip,
        property: ClipProperty::Opacity,
        at: Ticks::from_seconds(1),
        additive: false,
    });
    editor.act(Action::DeleteSelected);

    assert!(editor.sequence().find_clip(clip).is_some(), "the clip must survive");
    assert_eq!(
        editor.keyframe_times(clip, &ClipProperty::Opacity),
        vec![Ticks::ZERO, Ticks::from_seconds(2)]
    );
    assert!(editor.state.keyframes.is_empty());

    // With nothing keyframed in hand, Delete means the clip again.
    editor.act(Action::DeleteSelected);
    assert!(editor.sequence().find_clip(clip).is_none(), "{}", editor.status());
}

#[test]
fn keyframes_can_be_copied_onto_another_clip_at_the_playhead() {
    let mut editor = Editor::new();
    let v1 = editor.video_track();
    let source = editor.place(v1, Ticks::ZERO);
    editor.animate_opacity(source);

    // Two keyframes a second apart, copied.
    for (at, additive) in [(Ticks::from_seconds(1), false), (Ticks::from_seconds(2), true)] {
        editor.act(Action::SelectKeyframe {
            clip: source,
            property: ClipProperty::Opacity,
            at,
            additive,
        });
    }
    editor.act(Action::CopyKeyframes);
    assert!(editor.status().contains("copied 2 keyframes"), "{}", editor.status());

    // Onto a second clip, from its own half-second mark.
    let target = editor.place(v1, Ticks::from_seconds(10));
    editor.act(Action::SelectClip { clip: target, track: v1, additive: false });
    editor.act(Action::ScrubTo(Ticks::from_millis(10_500)));
    editor.act(Action::PasteKeyframes);

    assert_eq!(
        editor.keyframe_times(target, &ClipProperty::Opacity),
        vec![Ticks::from_millis(500), Ticks::from_millis(1_500)],
        "the copy keeps its shape and lands where the playhead is: {}",
        editor.status()
    );
    // What was just pasted is what is now in hand.
    assert_eq!(editor.state.keyframes.len(), 2);
    assert_eq!(editor.state.keyframes.clip, Some(target));
}

#[test]
fn the_clipboard_that_was_filled_last_is_the_one_a_paste_reads() {
    let mut editor = Editor::new();
    let v1 = editor.video_track();
    let clip = editor.place(v1, Ticks::ZERO);
    editor.animate_opacity(clip);

    editor.act(Action::SelectKeyframe {
        clip,
        property: ClipProperty::Opacity,
        at: Ticks::ZERO,
        additive: false,
    });
    editor.act(Action::Copy);
    assert!(editor.state.clipboard.is_empty(), "copying keyframes empties the clip clipboard");
    assert_eq!(editor.state.keyframe_clipboard.len(), 1);

    // Copying clips takes the clipboard back.
    editor.act(Action::SelectClip { clip, track: v1, additive: false });
    editor.act(Action::Copy);
    assert!(editor.state.keyframe_clipboard.is_empty());
    assert_eq!(editor.state.clipboard.len(), 1);

    let clips_before = editor.sequence().tracks.iter().flat_map(|t| t.clips()).count();
    editor.act(Action::ScrubTo(Ticks::from_seconds(10)));
    editor.act(Action::Paste);
    assert_eq!(
        editor.sequence().tracks.iter().flat_map(|t| t.clips()).count(),
        clips_before + 1,
        "a clip paste, not a keyframe paste: {}",
        editor.status()
    );
}

#[test]
fn an_inspector_edit_keyframes_an_animated_property_instead_of_its_static_value() {
    let mut editor = Editor::new();
    let v1 = editor.video_track();
    let clip = editor.place(v1, Ticks::ZERO);

    // Not animated: the static value is what changes.
    editor.act(Action::SetClipProperty {
        clip,
        property: ClipProperty::Position,
        value: PropertyValue::Point(Vec2::new(10.0, 0.0)),
    });
    editor.act(Action::EndGesture);
    let (_, found) = editor.sequence().find_clip(clip).unwrap();
    assert_eq!(found.transform.position.value, Vec2::new(10.0, 0.0));
    assert!(!found.transform.position.is_animated());

    // Animated: the same drag writes a keyframe at the playhead, because the
    // animation would otherwise go on overriding the value underneath it.
    editor.act(Action::ScrubTo(Ticks::ZERO));
    editor.act(Action::ToggleKeyframeAtPlayhead { clip, property: ClipProperty::Position });
    editor.act(Action::ScrubTo(Ticks::from_seconds(2)));
    for x in [20.0, 40.0, 60.0] {
        editor.act(Action::SetClipProperty {
            clip,
            property: ClipProperty::Position,
            value: PropertyValue::Point(Vec2::new(x, 0.0)),
        });
    }
    editor.act(Action::EndGesture);

    assert_eq!(
        editor.keyframe_times(clip, &ClipProperty::Position),
        vec![Ticks::ZERO, Ticks::from_seconds(2)]
    );
    assert_eq!(
        editor.value_at(clip, &ClipProperty::Position, Ticks::from_seconds(2)),
        PropertyValue::Point(Vec2::new(60.0, 0.0))
    );
    assert_eq!(
        editor.value_at(clip, &ClipProperty::Position, Ticks::from_seconds(1)),
        PropertyValue::Point(Vec2::new(35.0, 0.0)),
        "halfway between the two keyframes, linearly"
    );
}

#[test]
fn easing_applies_to_the_keyframe_the_segment_leaves() {
    let mut editor = Editor::new();
    let v1 = editor.video_track();
    let clip = editor.place(v1, Ticks::ZERO);
    editor.animate_opacity(clip);
    // Make the curve actually go somewhere.
    editor.act(Action::SetKeyframe {
        clip,
        property: ClipProperty::Opacity,
        at: Ticks::ZERO,
        value: PropertyValue::Scalar(0.0),
        coalesce: false,
    });

    editor.act(Action::SetKeyframeEasing {
        clip,
        property: ClipProperty::Opacity,
        at: Ticks::ZERO,
        interpolation: Interpolation::Hold,
        coalesce: false,
    });
    assert_eq!(
        editor.value_at(clip, &ClipProperty::Opacity, Ticks::from_millis(999)),
        PropertyValue::Scalar(0.0),
        "a held keyframe steps rather than ramping"
    );

    // Dragging a handle in the graph editor: many edits to one keyframe, one
    // undo step.
    let depth = editor.state.history.undo_depth();
    for x in [0.2, 0.4, 0.6] {
        editor.act(Action::SetKeyframeEasing {
            clip,
            property: ClipProperty::Opacity,
            at: Ticks::ZERO,
            interpolation: Interpolation::Bezier { x1: x, y1: 0.0, x2: 1.0, y2: 1.0 },
            coalesce: true,
        });
    }
    editor.act(Action::EndGesture);
    assert_eq!(editor.state.history.undo_depth(), depth + 1);
}

#[test]
fn clearing_a_propertys_animation_leaves_it_where_the_playhead_is() {
    let mut editor = Editor::new();
    let v1 = editor.video_track();
    let clip = editor.place(v1, Ticks::ZERO);

    editor.act(Action::ScrubTo(Ticks::ZERO));
    editor.act(Action::SetKeyframe {
        clip,
        property: ClipProperty::Opacity,
        at: Ticks::ZERO,
        value: PropertyValue::Scalar(0.0),
        coalesce: false,
    });
    editor.act(Action::SetKeyframe {
        clip,
        property: ClipProperty::Opacity,
        at: Ticks::from_seconds(2),
        value: PropertyValue::Scalar(1.0),
        coalesce: false,
    });

    editor.act(Action::ScrubTo(Ticks::from_seconds(1)));
    editor.act(Action::RemoveAnimation { clip, property: ClipProperty::Opacity });

    let (_, found) = editor.sequence().find_clip(clip).unwrap();
    assert!(!found.transform.opacity.is_animated());
    assert_eq!(found.transform.opacity.value, 0.5, "{}", editor.status());
}

#[test]
fn undo_drops_keyframes_from_the_selection_that_it_took_away() {
    let mut editor = Editor::new();
    let v1 = editor.video_track();
    let clip = editor.place(v1, Ticks::ZERO);
    editor.animate_opacity(clip);

    editor.act(Action::SelectKeyframe {
        clip,
        property: ClipProperty::Opacity,
        at: Ticks::from_seconds(2),
        additive: false,
    });
    assert_eq!(editor.state.keyframes.len(), 1);

    // Undoing the keyframe that is selected takes it out of the world, and so
    // out of the user's hands.
    editor.act(Action::Undo);
    assert!(editor.state.keyframes.is_empty());
}

#[test]
fn selecting_another_clip_lets_go_of_the_keyframes_of_the_first() {
    let mut editor = Editor::new();
    let v1 = editor.video_track();
    let first = editor.place(v1, Ticks::ZERO);
    editor.animate_opacity(first);
    editor.act(Action::SelectKeyframe {
        clip: first,
        property: ClipProperty::Opacity,
        at: Ticks::ZERO,
        additive: false,
    });

    let second = editor.place(v1, Ticks::from_seconds(10));
    editor.act(Action::SelectClip { clip: second, track: v1, additive: false });
    assert!(editor.state.keyframes.is_empty());
}

#[test]
fn the_animation_editor_opens_and_closes_and_lets_go_when_it_closes() {
    let mut editor = Editor::new();
    let v1 = editor.video_track();
    let clip = editor.place(v1, Ticks::ZERO);
    editor.animate_opacity(clip);
    editor.act(Action::SelectKeyframe {
        clip,
        property: ClipProperty::Opacity,
        at: Ticks::ZERO,
        additive: false,
    });

    assert!(!editor.state.animation.open);
    editor.act(Action::ToggleAnimationEditor);
    assert!(editor.state.animation.open);
    assert_eq!(editor.state.keyframes.len(), 1);

    editor.act(Action::ToggleAnimationEditor);
    assert!(!editor.state.animation.open);
    assert!(
        editor.state.keyframes.is_empty(),
        "keyframes cannot stay selected in an editor that is not on screen"
    );
}
