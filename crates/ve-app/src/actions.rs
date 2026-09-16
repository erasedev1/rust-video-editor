//! Every editor operation, as data.
//!
//! Menus, keyboard shortcuts and direct manipulation all build an [`Action`]
//! and hand it to [`dispatch`]. Nothing mutates the project any other way. That
//! buys three things: the shortcuts and the menu can never drift apart, the
//! whole editor is drivable without a window (which is how the tests exercise
//! it), and a scripting or macro layer later gets a surface that already exists.

use std::path::PathBuf;

use ve_command::{
    AddClip, ClipProperty, MoveClip, PropertyValue, RemoveClip, SetClipEnabled,
    SetClipProperty, SetSequenceFormat, SplitClip, TrimClip, TrimEdge,
};
use ve_core::{AssetId, Clip, ClipId, Project, TrackId, TrackKind};
use ve_engine::PlaybackEngine;
use ve_project::{autosave, store};
use ve_time::Ticks;

use crate::state::{EditorState, Status};

/// Something the user asked for.
#[derive(Debug, Clone)]
pub enum Action {
    // File
    NewProject,
    OpenProject(PathBuf),
    SaveProject,
    SaveProjectAs(PathBuf),
    ImportMedia(Vec<PathBuf>),

    // Edit
    Undo,
    Redo,
    DeleteSelected,
    SplitAtPlayhead,
    SelectClip { clip: ClipId, track: TrackId, additive: bool },
    ClearSelection,
    ToggleSelectedEnabled,
    SetClipProperty { clip: ClipId, property: ClipProperty, value: PropertyValue },

    // Timeline
    AddAssetToTimeline { asset: AssetId, track: TrackId, at: Ticks },
    MoveClipTo { clip: ClipId, track: TrackId, to: Ticks, coalesce: bool },
    TrimClipTo { clip: ClipId, track: TrackId, edge: TrimEdge, to: Ticks, coalesce: bool },
    EndGesture,

    // Transport
    TogglePlayback,
    StepFrames(i64),
    ScrubTo(Ticks),
    GoToStart,
    GoToEnd,

    // View
    ZoomIn,
    ZoomOut,
    ZoomToFit(f32),
    ToggleSnapping,
    TogglePerformanceOverlay,
}

/// Applies an action.
///
/// Never panics and never leaves the project half-changed: a rejected edit sets
/// the status line and returns, because the command layer guarantees a failed
/// command touched nothing.
pub fn dispatch(state: &mut EditorState, engine: &mut PlaybackEngine, action: Action) {
    let Some(sequence_id) = state.active_sequence_id() else {
        state.set_status(Status::error("this project has no sequence"));
        return;
    };

    match action {
        Action::NewProject => {
            state.project = Project::with_default_sequence("Untitled");
            state.history.clear();
            state.path = None;
            state.selection.clear();
            state.warnings.clear();
            state.autosave.set_project_path(None);
            engine.clock_mut().seek(Ticks::ZERO);
            state.set_status(Status::info("new project"));
        }

        Action::OpenProject(path) => match store::load(&path) {
            Ok(outcome) => {
                state.project = outcome.project;
                state.history.clear();
                state.selection.clear();
                state.warnings = outcome.warnings;
                state.path = Some(path.clone());
                state.autosave.set_project_path(Some(path.clone()));
                state.autosave.mark_saved();
                engine
                    .clock_mut()
                    .seek(state.active_sequence().map(|s| s.playhead).unwrap_or(Ticks::ZERO));
                let failures = engine.open_project_assets(&state.project);
                for (asset, e) in failures {
                    state.warnings.push(format!("asset {asset}: {e}"));
                }
                let n = state.project.clip_count();
                state.set_status(Status::info(format!(
                    "opened {} ({n} clip{})",
                    path.display(),
                    if n == 1 { "" } else { "s" }
                )));
            }
            Err(e) => state.set_status(Status::error(format!("could not open: {e}"))),
        },

        Action::SaveProject => match state.path.clone() {
            Some(path) => save_to(state, engine, path),
            // Save with no path is Save As; the caller supplies one after asking.
            None => state.set_status(Status::warning("this project has no location yet")),
        },

        Action::SaveProjectAs(path) => save_to(state, engine, path),

        Action::ImportMedia(paths) => {
            let mut imported = 0usize;
            for path in paths {
                match import_one(state, engine, &path) {
                    Ok(true) => imported += 1,
                    Ok(false) => {}
                    Err(e) => {
                        state.set_status(Status::error(format!(
                            "could not import {}: {e}",
                            path.display()
                        )));
                        return;
                    }
                }
            }
            if imported > 0 {
                state.mark_edited();
                state.set_status(Status::info(format!(
                    "imported {imported} file{}",
                    if imported == 1 { "" } else { "s" }
                )));
            }
        }

        Action::Undo => match state.history.undo(&mut state.project) {
            Ok(name) => {
                state.mark_edited();
                prune_selection(state);
                state.set_status(Status::info(format!("undo {name}")));
            }
            Err(e) => state.set_status(Status::warning(e.to_string())),
        },

        Action::Redo => match state.history.redo(&mut state.project) {
            Ok(name) => {
                state.mark_edited();
                prune_selection(state);
                state.set_status(Status::info(format!("redo {name}")));
            }
            Err(e) => state.set_status(Status::warning(e.to_string())),
        },

        Action::DeleteSelected => {
            let targets: Vec<(ClipId, TrackId)> = state
                .selection
                .clips
                .iter()
                .filter_map(|c| {
                    state.project.sequence(sequence_id)?.find_clip(*c).map(|(t, _)| (*c, t))
                })
                .collect();
            if targets.is_empty() {
                state.set_status(Status::warning("nothing selected"));
                return;
            }
            let count = targets.len();
            for (clip, track) in targets {
                let command = Box::new(RemoveClip::new(sequence_id, track, clip));
                if let Err(e) = state.history.execute(&mut state.project, command) {
                    state.set_status(Status::error(e.to_string()));
                    return;
                }
            }
            state.selection.clear();
            state.mark_edited();
            state.set_status(Status::info(format!(
                "deleted {count} clip{}",
                if count == 1 { "" } else { "s" }
            )));
        }

        Action::SplitAtPlayhead => {
            let at = engine.clock().position();
            // Split every track that has a clip under the playhead, which is
            // what a razor at the playhead is expected to do.
            let targets: Vec<(ClipId, TrackId)> = match state.project.sequence(sequence_id) {
                Some(seq) => seq
                    .tracks
                    .iter()
                    .filter(|t| !t.locked)
                    .filter_map(|t| t.clip_at(at).map(|c| (c.id, t.id)))
                    .collect(),
                None => Vec::new(),
            };
            if targets.is_empty() {
                state.set_status(Status::warning("no clip under the playhead"));
                return;
            }
            let count = targets.len();
            for (clip, track) in targets {
                let command = Box::new(SplitClip::new(sequence_id, track, clip, at));
                if let Err(e) = state.history.execute(&mut state.project, command) {
                    state.set_status(Status::warning(e.to_string()));
                    return;
                }
            }
            state.mark_edited();
            state.set_status(Status::info(format!("split {count}")));
        }

        Action::SelectClip { clip, track, additive } => {
            if additive {
                state.selection.toggle(clip, track);
            } else {
                state.selection.select_only(clip, track);
            }
        }

        Action::ClearSelection => state.selection.clear(),

        Action::ToggleSelectedEnabled => {
            let clips = state.selection.clips.clone();
            for clip in clips {
                let enabled = state
                    .project
                    .sequence(sequence_id)
                    .and_then(|s| s.find_clip(clip))
                    .map(|(_, c)| c.enabled)
                    .unwrap_or(true);
                let command = Box::new(SetClipEnabled::new(sequence_id, clip, !enabled));
                let _ = state.history.execute(&mut state.project, command);
            }
            state.mark_edited();
        }

        Action::SetClipProperty { clip, property, value } => {
            let command = Box::new(SetClipProperty::new(sequence_id, clip, property, value));
            match state.history.execute_coalesced(&mut state.project, command) {
                Ok(()) => state.mark_edited(),
                Err(e) => state.set_status(Status::warning(e.to_string())),
            }
        }

        Action::AddAssetToTimeline { asset, track, at } => {
            add_asset_to_timeline(state, asset, track, at);
        }

        Action::MoveClipTo { clip, track, to, coalesce } => {
            let command = Box::new(MoveClip::new(sequence_id, track, clip, to));
            let result = if coalesce {
                state.history.execute_coalesced(&mut state.project, command)
            } else {
                state.history.execute(&mut state.project, command)
            };
            // A refused move is ordinary during a drag (the pointer is over a
            // neighbour), so it is not worth a status message.
            if result.is_ok() {
                state.mark_edited();
            }
        }

        Action::TrimClipTo { clip, track, edge, to, coalesce } => {
            let command = Box::new(TrimClip::new(sequence_id, track, clip, edge, to));
            let result = if coalesce {
                state.history.execute_coalesced(&mut state.project, command)
            } else {
                state.history.execute(&mut state.project, command)
            };
            if result.is_ok() {
                state.mark_edited();
            }
        }

        Action::EndGesture => state.history.break_merge(),

        Action::TogglePlayback => {
            if let Some(seq) = state.project.sequence(sequence_id) {
                engine.toggle_playback(seq);
            }
        }

        Action::StepFrames(delta) => {
            if let Some(seq) = state.project.sequence(sequence_id) {
                engine.step_frames(seq, delta);
            }
            sync_playhead(state, engine);
        }

        Action::ScrubTo(to) => {
            if let Some(seq) = state.project.sequence(sequence_id) {
                engine.scrub_to(seq, to);
            }
            sync_playhead(state, engine);
        }

        Action::GoToStart => {
            if let Some(seq) = state.project.sequence(sequence_id) {
                engine.scrub_to(seq, Ticks::ZERO);
            }
            sync_playhead(state, engine);
        }

        Action::GoToEnd => {
            let end = state.project.sequence(sequence_id).map(|s| s.duration());
            if let (Some(seq), Some(end)) = (state.project.sequence(sequence_id), end) {
                engine.scrub_to(seq, end);
            }
            sync_playhead(state, engine);
        }

        Action::ZoomIn => {
            let at = engine.clock().position();
            state.timeline.zoom_about(1.25, at, state.timeline.x_of(at));
        }
        Action::ZoomOut => {
            let at = engine.clock().position();
            state.timeline.zoom_about(0.8, at, state.timeline.x_of(at));
        }
        Action::ZoomToFit(width) => {
            let duration = state
                .project
                .sequence(sequence_id)
                .map(|s| s.duration())
                .unwrap_or(Ticks::ZERO);
            state.timeline.zoom_to_fit(duration, width);
        }
        Action::ToggleSnapping => {
            state.timeline.snapping = !state.timeline.snapping;
            let on = state.timeline.snapping;
            state.set_status(Status::info(if on { "snapping on" } else { "snapping off" }));
        }
        Action::TogglePerformanceOverlay => {
            state.show_performance_overlay = !state.show_performance_overlay;
        }
    }
}

/// Stores the playhead on the sequence, so reopening the project restores it.
fn sync_playhead(state: &mut EditorState, engine: &PlaybackEngine) {
    let position = engine.clock().position();
    if let Some(seq) = state.project.active_mut() {
        seq.playhead = position;
    }
}

/// Drops selected clips that no longer exist, which an undo can cause.
fn prune_selection(state: &mut EditorState) {
    let Some(seq) = state.project.active() else { return };
    state.selection.clips.retain(|c| seq.find_clip(*c).is_some());
}

fn save_to(state: &mut EditorState, engine: &PlaybackEngine, path: PathBuf) {
    sync_playhead(state, engine);
    match store::save(&state.project, &path) {
        Ok(()) => {
            state.path = Some(path.clone());
            state.history.mark_saved();
            state.autosave.set_project_path(Some(path.clone()));
            state.autosave.mark_saved();
            // The autosave held work the project file now contains.
            autosave::discard(&path);
            state.set_status(Status::info(format!("saved {}", path.display())));
        }
        Err(e) => state.set_status(Status::error(format!("could not save: {e}"))),
    }
}

/// Sets an empty sequence's format from the media about to land on it.
fn adopt_format_from(
    state: &mut EditorState,
    sequence_id: ve_core::SequenceId,
    asset: AssetId,
) {
    let Some(media) = state.project.asset(asset) else { return };
    let Some(video) = media.info.video.as_ref() else { return };

    let resolution = video.display_size();
    let rate = video.rate;
    let sample_rate = media.info.audio.as_ref().map(|a| a.sample_rate);

    let command = SetSequenceFormat::new(sequence_id, resolution, rate, sample_rate);
    if !command.would_change(&state.project) {
        return;
    }
    if state.history.execute(&mut state.project, Box::new(command)).is_ok() {
        state.set_status(Status::info(format!(
            "sequence set to {}×{} at {rate}",
            resolution.width, resolution.height
        )));
    }
}

/// Probes a file and adds it as an asset. Returns whether anything was added.
fn import_one(
    state: &mut EditorState,
    engine: &PlaybackEngine,
    path: &std::path::Path,
) -> Result<bool, ve_media::MediaError> {
    let path = path.canonicalize().unwrap_or_else(|_| path.to_path_buf());
    if state.project.asset_by_path(&path).is_some() {
        // Importing the same file twice reuses the one asset rather than
        // duplicating the media reference.
        return Ok(false);
    }

    let info = ve_media::probe(&path)?;
    let has_video = info.has_video();
    let asset = state.project.add_asset(&path, info);
    if has_video {
        if let Err(e) = engine.decode_service().open(asset, &path, None) {
            log::warn!("imported {} but could not open a decoder: {e}", path.display());
        }
    }
    Ok(true)
}

/// Places an asset on a track, at the first free slot from `at`.
fn add_asset_to_timeline(state: &mut EditorState, asset: AssetId, track: TrackId, at: Ticks) {
    let Some(sequence_id) = state.active_sequence_id() else { return };
    let Some(media) = state.project.asset(asset) else {
        state.set_status(Status::error("no such asset"));
        return;
    };
    let duration = media.duration();
    let name = media.name.clone();
    if duration.raw() <= 0 {
        state.set_status(Status::warning(format!("{name} has no usable duration")));
        return;
    }

    // Placing a clip where one already sits would just be refused, so slide it
    // to the first gap that fits instead of making the user find one.
    let start = match state.project.sequence(sequence_id).and_then(|s| s.track(track)) {
        Some(t) => t.first_free_slot(at, duration),
        None => {
            state.set_status(Status::error("no such track"));
            return;
        }
    };

    let kind = state.project.sequence(sequence_id).and_then(|s| s.track(track)).map(|t| t.kind);
    let has_video = state.project.asset(asset).map(|a| a.info.has_video()).unwrap_or(false);
    let has_audio = state.project.asset(asset).map(|a| a.info.has_audio()).unwrap_or(false);
    match (kind, has_video, has_audio) {
        (Some(TrackKind::Video), false, _) => {
            state.set_status(Status::warning(format!("{name} has no video")));
            return;
        }
        (Some(TrackKind::Audio), _, false) => {
            state.set_status(Status::warning(format!("{name} has no audio")));
            return;
        }
        _ => {}
    }

    // An empty sequence adopts the first clip's format, so a 4K clip is not
    // letterboxed into a 1080p frame and a small one is not stranded in the
    // middle of a large one. Done as its own command, so undoing the first edit
    // restores the project exactly, format included.
    if state.project.sequence(sequence_id).map(|s| s.duration().is_zero()).unwrap_or(false) {
        adopt_format_from(state, sequence_id, asset);
    }

    let clip_id = state.project.new_clip_id();
    let clip = Clip::new(clip_id, asset, name, Ticks::ZERO, start, duration);
    let command = Box::new(AddClip::new(sequence_id, track, clip));

    match state.history.execute(&mut state.project, command) {
        Ok(()) => {
            state.selection.select_only(clip_id, track);
            state.mark_edited();
            state.set_status(Status::info("clip added"));
        }
        Err(e) => state.set_status(Status::error(e.to_string())),
    }
}
