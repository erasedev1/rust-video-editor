//! Every editor operation, as data.
//!
//! Menus, keyboard shortcuts and direct manipulation all build an [`Action`]
//! and hand it to [`dispatch`]. Nothing mutates the project any other way. That
//! buys three things: the shortcuts and the menu can never drift apart, the
//! whole editor is drivable without a window (which is how the tests exercise
//! it), and a scripting or macro layer later gets a surface that already exists.

use std::path::PathBuf;

use ve_command::{
    property_ref, AddClip, AddEffect, AddMarker, AddTrack, ClipProperty, Command, Compound,
    CrossfadeClips, EditKeyframes, EffectHost, KeyframeEdit, KeyframePoint, MoveClip,
    MoveEffect, MoveTrack, PropertyValue, RemoveClip, RemoveEffect, RemoveMarker, RemoveTrack,
    RenameEffect, RollEdit, SetAssetProxy, SetClipBlendMode, SetClipEnabled, SetClipFade,
    SetClipMotionBlur, SetClipProperty, SetClipSpeed, SetCompositionSettings, SetEffectEnabled,
    SetEffectOption, SetSequenceColorSpace, SetSequenceFormat, SetSequenceMotionBlur,
    SetTrackFlag, SetTrackLevel, SetUseProxies, ShiftClips, SlideClip, SlipClip, SplitClip,
    TrackFlag, TrackLevel, TrimClip, TrimEdge,
};
use ve_core::{
    AssetId, BlendMode, Clip, ClipId, ColorSpace, EffectId, Fade, FadeCurve, FadeEdge,
    Interpolation, MarkerId, MotionBlur, ParamValue, Project, SequenceId, Speed, TrackId,
    TrackKind,
};
use ve_engine::PlaybackEngine;
use ve_media::WaveformService;
use ve_project::{autosave, store};
use ve_time::Ticks;

use crate::state::{AnimationMode, EditorState, ScopeKind, Status, TimelineTool};
use ve_render::WaveformMode;

/// Something the user asked for.
#[derive(Debug, Clone)]
pub enum Action {
    // File
    NewProject,
    OpenProject(PathBuf),
    SaveProject,
    SaveProjectAs(PathBuf),
    ImportMedia(Vec<PathBuf>),
    // Export. The dialogue is state rather than a modal loop, so the editor
    // keeps running — and keeps playing — while it is open.
    OpenExportDialog,
    CloseExportDialog,
    /// What the dialogue is now showing. One action for the whole value rather
    /// than one per control, because the settings are a single thing and the
    /// dialogue is a view of it.
    SetExportSettings(Box<ve_export::ExportSettings>),
    /// Starts rendering the active sequence to a file. Carries the whole
    /// request, so an export can be started from a test, a script or a future
    /// command line without the dialogue existing at all.
    StartExport(Box<ve_export::ExportSettings>),
    CancelExport,

    // Proxies. Building is a background job like an export; using them is a
    // switch on the project, so it survives being closed and reopened.
    /// Builds a proxy for every video asset that has not got a usable one.
    BuildProxies,
    /// Builds one again whether or not it already has a proxy, for footage
    /// that was replaced on disk or a size that turned out wrong.
    RebuildProxies,
    CancelProxyBuild,
    SetProxyScale(ve_export::ProxyScale),
    SetUseProxies(bool),
    /// Forgets every proxy reference. The files on disk are left alone: this
    /// is the project pointing at them, not the things themselves.
    ForgetProxies,

    // Edit
    Undo,
    Redo,
    // `DeleteSelected` lifts the clips and leaves the gap they occupied;
    // `RippleDeleteSelected` closes the holes behind them, and
    // `CloseGapAtPlayhead` closes one that is already there.
    DeleteSelected,
    RippleDeleteSelected,
    CloseGapAtPlayhead,
    Copy,
    // Copy, then lift: the clipboard keeps the clips, the timeline does not.
    Cut,
    // Places the clipboard at the playhead, on the selected track.
    Paste,
    SplitAtPlayhead,
    SelectClip {
        clip: ClipId,
        track: TrackId,
        additive: bool,
    },
    SelectAll,
    // Takes every clip inside a rectangle swept across the timeline.
    SelectClipsIn {
        range: ve_time::TimeRange,
        tracks: Vec<TrackId>,
        additive: bool,
    },
    ClearSelection,
    ToggleSelectedEnabled,
    SetClipProperty {
        clip: ClipId,
        property: ClipProperty,
        value: PropertyValue,
    },
    // Keeps the clip's frames and changes how long it takes to play them, so
    // the clip's length on the timeline changes with it.
    SetClipSpeed {
        clip: ClipId,
        speed: Speed,
    },
    // How the clip's picture combines with the layers beneath it.
    SetClipBlendMode {
        clip: ClipId,
        blend: BlendMode,
    },
    // The envelope at one end of a clip. `coalesce` is set while a fade handle
    // is being dragged, so the whole drag is one undo step.
    SetClipFade {
        clip: ClipId,
        edge: FadeEdge,
        length: Ticks,
        curve: FadeCurve,
        coalesce: bool,
    },
    // Fades two selected clips into each other across the time they overlap,
    // which means across two tracks — clips on one track never sound together.
    CrossfadeSelection(FadeCurve),
    /// Applies to whatever canvas is being viewed: the sequence, or a
    /// composition when one is open.
    SetColorSpace(ColorSpace),
    // Whether one clip is smeared across the shutter when it moves.
    SetClipMotionBlur {
        clip: ClipId,
        blurred: bool,
    },
    // The shutter itself, which belongs to the canvas being viewed. Coalesced
    // while the angle or the sample count is dragged.
    SetMotionBlur(MotionBlur),

    // Effects. Every one of these names the clip whose chain is being edited;
    // a parameter that can be animated is set through `SetClipProperty` like
    // any other property, which is what gives effect parameters keyframes
    // without a command of their own.
    AddEffect {
        clip: ClipId,
        kind: String,
    },
    RemoveEffect {
        clip: ClipId,
        effect: EffectId,
    },
    // Order is the chain, so this is a real edit: `to` is where the effect
    // ends up, stated absolutely.
    MoveEffect {
        clip: ClipId,
        effect: EffectId,
        to: usize,
    },
    SetEffectEnabled {
        clip: ClipId,
        effect: EffectId,
        enabled: bool,
    },
    RenameEffect {
        clip: ClipId,
        effect: EffectId,
        name: String,
    },
    // A switch or a choice, which have no curve to sit on.
    SetEffectOption {
        clip: ClipId,
        effect: EffectId,
        key: String,
        value: ParamValue,
    },

    // Animation
    ToggleAnimationEditor,
    SetAnimationMode(AnimationMode),
    // Which property the curve editor draws; `None` draws every animated one.
    FocusProperty(Option<ClipProperty>),
    // Pins the value a property already has at the playhead, or takes the
    // keyframe that is there away again.
    ToggleKeyframeAtPlayhead {
        clip: ClipId,
        property: ClipProperty,
    },
    // Keyframes a property at a clip-local time with an explicit value: what
    // dragging a point in the curve editor does.
    SetKeyframe {
        clip: ClipId,
        property: ClipProperty,
        at: Ticks,
        value: PropertyValue,
        coalesce: bool,
    },
    // Drops a property's animation, leaving it at the value it has now.
    RemoveAnimation {
        clip: ClipId,
        property: ClipProperty,
    },
    SelectKeyframe {
        clip: ClipId,
        property: ClipProperty,
        at: Ticks,
        additive: bool,
    },
    ClearKeyframeSelection,
    DeleteSelectedKeyframes,
    // Retiming, stated absolutely: where every keyframe of each named property
    // now is. One drag re-issues this on every pointer move.
    SetKeyframeTimes {
        clip: ClipId,
        times: Vec<(ClipProperty, Vec<Ticks>)>,
        coalesce: bool,
    },
    SetKeyframeEasing {
        clip: ClipId,
        property: ClipProperty,
        at: Ticks,
        interpolation: Interpolation,
        coalesce: bool,
    },
    CopyKeyframes,
    PasteKeyframes,

    // Tracks
    AddTrack(TrackKind),
    RemoveTrack(TrackId),
    // Swaps a track with its neighbour of the same kind. `toward_top` is the
    // direction the timeline draws, not the order tracks are stored in.
    MoveTrack {
        track: TrackId,
        toward_top: bool,
    },
    SetTrackFlag {
        track: TrackId,
        flag: TrackFlag,
        value: bool,
    },
    SetTrackLevel {
        track: TrackId,
        which: TrackLevel,
        value: f64,
    },

    // Markers
    AddMarkerAtPlayhead,
    RemoveMarker(MarkerId),
    // Jumps to the nearest marker in a direction; `1` is forwards.
    GoToMarker(i32),

    // Timeline
    AddAssetToTimeline {
        asset: AssetId,
        track: TrackId,
        at: Ticks,
    },
    MoveClipTo {
        clip: ClipId,
        track: TrackId,
        to: Ticks,
        coalesce: bool,
    },
    TrimClipTo {
        clip: ClipId,
        track: TrackId,
        edge: TrimEdge,
        to: Ticks,
        coalesce: bool,
    },
    RollEditTo {
        left: ClipId,
        right: ClipId,
        track: TrackId,
        to: Ticks,
        coalesce: bool,
    },
    SlipClipTo {
        clip: ClipId,
        track: TrackId,
        to_source_in: Ticks,
        coalesce: bool,
    },
    SlideClipTo {
        clip: ClipId,
        track: TrackId,
        to: Ticks,
        coalesce: bool,
    },
    EndGesture,
    SetTool(TimelineTool),

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

    // Scopes
    ToggleScopes,
    ShowScope(ScopeKind),
    SetWaveformMode(WaveformMode),
}

/// Applies an action.
///
/// Never panics and never leaves the project half-changed: a rejected edit sets
/// the status line and returns, because the command layer guarantees a failed
/// command touched nothing.
pub fn dispatch(
    state: &mut EditorState,
    engine: &mut PlaybackEngine,
    waveforms: &WaveformService,
    action: Action,
) {
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
            // Asset ids are per project, so peaks from the old one would be
            // read as belonging to whatever asset inherits the id next.
            waveforms.clear();
            state.project_replaced();
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
                waveforms.clear();
                engine
                    .clock_mut()
                    .seek(state.active_sequence().map(|s| s.playhead).unwrap_or(Ticks::ZERO));
                let failures = engine.open_project_assets(&state.project);
                for (asset, e) in failures {
                    state.warnings.push(format!("asset {asset}: {e}"));
                }
                state.project_replaced();
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
                match import_one(state, engine, waveforms, &path) {
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

        Action::OpenExportDialog => {
            state.export.prepare(&state.project, state.path.as_ref());
            if state.export.settings.is_none() {
                state.set_status(Status::error("there is no sequence to export"));
                return;
            }
            state.export.open = true;
        }

        Action::CloseExportDialog => state.export.open = false,

        Action::SetExportSettings(settings) => state.export.settings = Some(*settings),

        Action::StartExport(settings) => {
            if state.export.is_running() {
                state.set_status(Status::warning("an export is already running"));
                return;
            }
            if let Err(e) = settings.validate() {
                state.set_status(Status::error(e.to_string()));
                return;
            }
            let Some(gpu) = state.export.gpu.clone() else {
                state.set_status(Status::error(
                    "there is no GPU device to render an export with",
                ));
                return;
            };
            // A snapshot of the project as it is now: editing goes on while the
            // export runs, and what is written is what was on screen when the
            // button was pressed.
            let project = std::sync::Arc::new(state.project.clone());
            let settings = *settings;
            state.export.last = None;
            state
                .set_status(Status::info(format!("exporting to {}…", settings.path.display())));
            state.export.job = Some(ve_export::ExportJob::start(
                project,
                sequence_id,
                settings.clone(),
                gpu,
                state.export.metrics.clone(),
            ));
            state.export.settings = Some(settings);
        }

        Action::CancelExport => match &state.export.job {
            Some(job) => {
                job.cancel();
                state.set_status(Status::info("stopping the export…"));
            }
            None => state.set_status(Status::warning("no export is running")),
        },

        Action::BuildProxies => start_proxy_build(state, false),
        Action::RebuildProxies => start_proxy_build(state, true),

        Action::CancelProxyBuild => match &state.proxies.job {
            Some(job) => {
                job.cancel();
                state.set_status(Status::info("stopping the proxy build…"));
            }
            None => state.set_status(Status::warning("no proxy build is running")),
        },

        Action::SetProxyScale(scale) => {
            if state.proxies.is_running() {
                state.set_status(Status::warning(
                    "a proxy build is already running at the other size",
                ));
                return;
            }
            state.proxies.scale = scale;
        }

        Action::SetUseProxies(enabled) => {
            let command = SetUseProxies::new(enabled);
            if !command.would_change(&state.project) {
                return;
            }
            if state.history.execute(&mut state.project, Box::new(command)).is_err() {
                return;
            }
            // Which file a worker decodes is fixed when it is opened, so the
            // switch only means anything once they have been replaced.
            let failures = engine.reopen_project_assets(&state.project);
            for (asset, why) in &failures {
                log::warn!("could not reopen asset {asset}: {why}");
            }
            state.set_status(Status::info(if enabled {
                "cutting on proxies".to_string()
            } else {
                "cutting at full resolution".to_string()
            }));
        }

        Action::ForgetProxies => {
            let assets: Vec<AssetId> =
                state.project.assets.iter().filter(|a| a.has_proxy()).map(|a| a.id).collect();
            if assets.is_empty() {
                state.set_status(Status::warning("no proxies to forget"));
                return;
            }
            let count = assets.len();
            // One undo step for the lot: the user asked once.
            let mut compound = Compound::new("Forget Proxies");
            for asset in assets {
                compound.push(Box::new(SetAssetProxy::detach(asset)));
            }
            if state.history.execute(&mut state.project, Box::new(compound)).is_ok() {
                let failures = engine.reopen_project_assets(&state.project);
                for (asset, why) in &failures {
                    log::warn!("could not reopen asset {asset}: {why}");
                }
                state.set_status(Status::info(format!(
                    "forgot {count} {}; the files are still on disk",
                    if count == 1 { "proxy" } else { "proxies" }
                )));
            }
        }

        Action::Undo => match state.history.undo(&mut state.project) {
            Ok(name) => {
                state.mark_edited();
                prune_selection(state);
                prune_keyframe_selection(state, sequence_id);
                state.set_status(Status::info(format!("undo {name}")));
            }
            Err(e) => state.set_status(Status::warning(e.to_string())),
        },

        Action::Redo => match state.history.redo(&mut state.project) {
            Ok(name) => {
                state.mark_edited();
                prune_selection(state);
                prune_keyframe_selection(state, sequence_id);
                state.set_status(Status::info(format!("redo {name}")));
            }
            Err(e) => state.set_status(Status::warning(e.to_string())),
        },

        // Keyframes first: with some in hand, Delete is unambiguously about
        // them, and taking the whole clip instead would be a surprise the
        // user cannot see coming.
        Action::DeleteSelected if !state.keyframes.is_empty() => {
            delete_selected_keyframes(state, sequence_id)
        }

        Action::DeleteSelected => delete_selected(state, sequence_id, false),

        Action::RippleDeleteSelected => delete_selected(state, sequence_id, true),

        Action::CloseGapAtPlayhead => {
            let at = engine.clock().position();
            let Some(seq) = state.project.sequence(sequence_id) else { return };
            // Every unlocked track with a hole under the playhead closes it, so
            // one keystroke tidies a straight cut across the whole sequence.
            let mut command = Compound::new("Close Gap");
            for track in seq.tracks.iter().filter(|t| !t.locked) {
                if let Some(gap) = track.gap_at(at) {
                    command.push(Box::new(ShiftClips::new(
                        sequence_id,
                        track.id,
                        gap.end(),
                        -gap.duration,
                    )));
                }
            }
            if command.is_empty() {
                state.set_status(Status::warning("no gap under the playhead"));
                return;
            }
            let count = command.len();
            match state.history.execute(&mut state.project, Box::new(command)) {
                Ok(()) => {
                    state.mark_edited();
                    state.set_status(Status::info(format!(
                        "closed {count} gap{}",
                        plural(count)
                    )));
                }
                Err(e) => state.set_status(Status::warning(e.to_string())),
            }
        }

        // Copy follows the same rule: keyframes when there are any selected,
        // clips otherwise.
        Action::Copy if !state.keyframes.is_empty() => {
            match copy_keyframes(state, sequence_id) {
                0 => state.set_status(Status::warning("no keyframes selected")),
                n => {
                    state.set_status(Status::info(format!("copied {n} keyframe{}", plural(n))))
                }
            }
        }

        Action::Copy => match copy_selection(state, sequence_id) {
            0 => state.set_status(Status::warning("nothing selected")),
            n => state.set_status(Status::info(format!("copied {n} clip{}", plural(n)))),
        },

        Action::Cut => {
            if copy_selection(state, sequence_id) == 0 {
                state.set_status(Status::warning("nothing selected"));
                return;
            }
            delete_selected(state, sequence_id, false);
        }

        // Whichever clipboard was filled last is the one a paste reads, so
        // Ctrl+V never has to guess which of the two was meant.
        Action::Paste if !state.keyframe_clipboard.is_empty() => {
            paste_keyframes(state, engine, sequence_id)
        }

        Action::Paste => paste(state, engine, sequence_id),

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
            // One razor stroke is one undo step, however many tracks it crossed.
            let mut command = Compound::new("Split Clip");
            for (clip, track) in targets {
                command.push(Box::new(SplitClip::new(sequence_id, track, clip, at)));
            }
            match state.history.execute(&mut state.project, Box::new(command)) {
                Ok(()) => {
                    state.mark_edited();
                    state.set_status(Status::info(format!("split {count}")));
                }
                Err(e) => state.set_status(Status::warning(e.to_string())),
            }
        }

        Action::SelectClip { clip, track, additive } => {
            if additive {
                state.selection.toggle(clip, track);
            } else {
                state.selection.select_only(clip, track);
            }
            // Taking hold of a clip lets go of any keyframes. Whichever of the
            // two was touched last is what Delete, Copy and Paste are about,
            // and that rule is only predictable if pointing at a clip really
            // does mean the clip.
            state.keyframes.clear();
        }

        Action::SelectAll => {
            let Some(seq) = state.project.sequence(sequence_id) else { return };
            state.selection.clips =
                seq.tracks.iter().flat_map(|t| t.clips()).map(|c| c.id).collect();
            let n = state.selection.clips.len();
            state.set_status(Status::info(format!("selected {n} clip{}", plural(n))));
        }

        Action::SelectClipsIn { range, tracks, additive } => {
            let Some(seq) = state.project.sequence(sequence_id) else { return };
            let caught: Vec<ClipId> = tracks
                .iter()
                .filter_map(|t| seq.track(*t))
                .flat_map(|t| t.clips_in_range(range))
                .map(|c| c.id)
                .collect();
            if !additive {
                state.selection.clips.clear();
            }
            for clip in caught {
                if !state.selection.is_selected(clip) {
                    state.selection.clips.push(clip);
                }
            }
        }

        Action::ClearSelection => {
            state.selection.clear();
            state.keyframes.clear();
        }

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
            // Setting a value on a property that is animated writes a keyframe
            // at the playhead instead of the static value underneath the
            // animation — otherwise an inspector drag would appear to do
            // nothing, because the keyframes would go on overriding it.
            let animated = state
                .project
                .sequence(sequence_id)
                .and_then(|s| s.find_clip(clip))
                .and_then(|(_, c)| property_ref(c, &property))
                .map(|view| view.is_animated())
                .unwrap_or(false);
            let at = engine.clock().position();
            match (animated, clip_local(state, sequence_id, clip, at)) {
                (true, Some(local)) => {
                    let interpolation =
                        keyframe_easing(state, sequence_id, clip, &property, local);
                    let command = Box::new(EditKeyframes::one(
                        sequence_id,
                        clip,
                        property.clone(),
                        KeyframeEdit::Set { time: local, value, interpolation },
                    ));
                    match state.history.execute_coalesced(&mut state.project, command) {
                        Ok(()) => {
                            state.mark_edited();
                            state.keyframes.add(clip, property, local);
                        }
                        Err(e) => state.set_status(Status::warning(e.to_string())),
                    }
                }
                // Not animated, or the playhead is not over the clip: there is
                // no instant to key, so the static value is what changes.
                _ => {
                    let command =
                        Box::new(SetClipProperty::new(sequence_id, clip, property, value));
                    match state.history.execute_coalesced(&mut state.project, command) {
                        Ok(()) => state.mark_edited(),
                        Err(e) => state.set_status(Status::warning(e.to_string())),
                    }
                }
            }
        }

        Action::AddEffect { clip, kind } => {
            let host = EffectHost::clip(sequence_id, clip);
            let command = Box::new(AddEffect::new(host, kind));
            match state.history.execute(&mut state.project, command) {
                Ok(()) => state.mark_edited(),
                Err(e) => state.set_status(Status::warning(e.to_string())),
            }
        }

        Action::RemoveEffect { clip, effect } => {
            let host = EffectHost::clip(sequence_id, clip);
            let command = Box::new(RemoveEffect::new(host, effect));
            match state.history.execute(&mut state.project, command) {
                Ok(()) => state.mark_edited(),
                Err(e) => state.set_status(Status::warning(e.to_string())),
            }
        }

        Action::MoveEffect { clip, effect, to } => {
            let host = EffectHost::clip(sequence_id, clip);
            let command = Box::new(MoveEffect::new(host, effect, to));
            match state.history.execute(&mut state.project, command) {
                Ok(()) => state.mark_edited(),
                Err(e) => state.set_status(Status::warning(e.to_string())),
            }
        }

        Action::SetEffectEnabled { clip, effect, enabled } => {
            let host = EffectHost::clip(sequence_id, clip);
            let command = Box::new(SetEffectEnabled::new(host, effect, enabled));
            match state.history.execute(&mut state.project, command) {
                Ok(()) => state.mark_edited(),
                Err(e) => state.set_status(Status::warning(e.to_string())),
            }
        }

        Action::RenameEffect { clip, effect, name } => {
            let host = EffectHost::clip(sequence_id, clip);
            let command = Box::new(RenameEffect::new(host, effect, name));
            // Coalesced: a rename arrives one keystroke at a time.
            match state.history.execute_coalesced(&mut state.project, command) {
                Ok(()) => state.mark_edited(),
                Err(e) => state.set_status(Status::warning(e.to_string())),
            }
        }

        Action::SetEffectOption { clip, effect, key, value } => {
            let host = EffectHost::clip(sequence_id, clip);
            let command = Box::new(SetEffectOption::new(host, effect, key, value));
            match state.history.execute(&mut state.project, command) {
                Ok(()) => state.mark_edited(),
                Err(e) => state.set_status(Status::warning(e.to_string())),
            }
        }

        Action::SetClipFade { clip, edge, length, curve, coalesce } => {
            let fade = Fade::new(length, curve);
            let command = Box::new(SetClipFade::new(sequence_id, clip, edge, fade));
            let result = if coalesce {
                state.history.execute_coalesced(&mut state.project, command)
            } else {
                state.history.execute(&mut state.project, command)
            };
            match result {
                Ok(()) => state.mark_edited(),
                Err(e) => state.set_status(Status::warning(e.to_string())),
            }
        }

        Action::CrossfadeSelection(curve) => {
            let mut selected = state.selection.clips.iter().copied();
            let (Some(a), Some(b), None) = (selected.next(), selected.next(), selected.next())
            else {
                state.set_status(Status::warning("select exactly two clips to crossfade"));
                return;
            };
            let length = match CrossfadeClips::overlap(&state.project, sequence_id, a, b) {
                Some((_, _, length)) => length,
                None => {
                    state.set_status(Status::warning(
                        "those clips never sound together — a crossfade needs them \
                         overlapping, which means on different tracks",
                    ));
                    return;
                }
            };
            let command = Box::new(CrossfadeClips::new(sequence_id, a, b, curve));
            match state.history.execute(&mut state.project, command) {
                Ok(()) => {
                    state.mark_edited();
                    state.set_status(Status::info(format!(
                        "crossfaded over {:.2}s",
                        length.as_secs_f64()
                    )));
                }
                Err(e) => state.set_status(Status::warning(e.to_string())),
            }
        }

        Action::ToggleAnimationEditor => {
            state.animation.open = !state.animation.open;
            if !state.animation.open {
                state.keyframes.clear();
                state.animation.drag = None;
            }
        }

        Action::SetAnimationMode(mode) => state.animation.mode = mode,

        Action::FocusProperty(property) => state.animation.focus = property,

        Action::ToggleKeyframeAtPlayhead { clip, property } => {
            let at = engine.clock().position();
            let Some(local) = clip_local(state, sequence_id, clip, at) else {
                state.set_status(Status::warning(
                    "the playhead is not over that clip, so there is nothing to keyframe",
                ));
                return;
            };
            let Some(clip_ref) =
                state.project.sequence(sequence_id).and_then(|s| s.find_clip(clip))
            else {
                return;
            };
            let Some(view) = property_ref(clip_ref.1, &property) else {
                state.set_status(Status::warning("that property cannot be animated"));
                return;
            };
            // A keyframe already there is the user asking for it to go, which
            // is what makes one button both add and remove.
            let edit = match view.keyframe_at(local) {
                Some(_) => KeyframeEdit::Remove(vec![local]),
                None => KeyframeEdit::Set {
                    time: local,
                    // The value it already shows at this instant, so pinning it
                    // changes nothing until something else moves.
                    value: view.evaluate(local),
                    interpolation: Interpolation::Linear,
                },
            };
            let removing = matches!(edit, KeyframeEdit::Remove(_));
            let command =
                Box::new(EditKeyframes::one(sequence_id, clip, property.clone(), edit));
            match state.history.execute(&mut state.project, command) {
                Ok(()) => {
                    state.mark_edited();
                    if removing {
                        state.keyframes.remove(&property, local);
                    } else {
                        state.keyframes.add(clip, property, local);
                    }
                }
                Err(e) => state.set_status(Status::warning(e.to_string())),
            }
        }

        Action::SetKeyframe { clip, property, at, value, coalesce } => {
            let interpolation = keyframe_easing(state, sequence_id, clip, &property, at);
            let command = Box::new(EditKeyframes::one(
                sequence_id,
                clip,
                property,
                // An existing keyframe keeps its easing: this sets a value,
                // not a curve.
                KeyframeEdit::Set { time: at, value, interpolation },
            ));
            let result = if coalesce {
                state.history.execute_coalesced(&mut state.project, command)
            } else {
                state.history.execute(&mut state.project, command)
            };
            match result {
                Ok(()) => state.mark_edited(),
                Err(e) => state.set_status(Status::warning(e.to_string())),
            }
        }

        Action::RemoveAnimation { clip, property } => {
            let at = engine.clock().position();
            let local = clip_local(state, sequence_id, clip, at).unwrap_or(Ticks::ZERO);
            let command = Box::new(EditKeyframes::one(
                sequence_id,
                clip,
                property.clone(),
                KeyframeEdit::Freeze { at: local },
            ));
            match state.history.execute(&mut state.project, command) {
                Ok(()) => {
                    state.mark_edited();
                    prune_keyframe_selection(state, sequence_id);
                    state.set_status(Status::info(format!(
                        "{} is no longer animated",
                        property.label()
                    )));
                }
                Err(e) => state.set_status(Status::warning(e.to_string())),
            }
        }

        Action::SelectKeyframe { clip, property, at, additive } => {
            if additive {
                state.keyframes.toggle(clip, property, at);
            } else {
                state.keyframes.select_only(clip, property, at);
            }
        }

        Action::ClearKeyframeSelection => state.keyframes.clear(),

        Action::DeleteSelectedKeyframes => delete_selected_keyframes(state, sequence_id),

        Action::SetKeyframeTimes { clip, times, coalesce } => {
            // The keyframes keep their order through a retime, so where each
            // selected one ended up is read off the two lists rather than
            // guessed at afterwards.
            let was = times
                .iter()
                .map(|(property, _)| {
                    let old = state
                        .project
                        .sequence(sequence_id)
                        .and_then(|s| s.find_clip(clip))
                        .and_then(|(_, c)| property_ref(c, property))
                        .map(|v| v.keyframe_times())
                        .unwrap_or_default();
                    (property.clone(), old)
                })
                .collect::<Vec<_>>();

            let edits = times
                .iter()
                .map(|(property, times)| {
                    (property.clone(), KeyframeEdit::SetTimes(times.clone()))
                })
                .collect();
            let command = Box::new(EditKeyframes::new(sequence_id, clip, edits));
            let result = if coalesce {
                state.history.execute_coalesced(&mut state.project, command)
            } else {
                state.history.execute(&mut state.project, command)
            };
            match result {
                Ok(()) => {
                    state.mark_edited();
                    for ((property, old), (_, new)) in was.iter().zip(&times) {
                        for (before, after) in old.iter().zip(new) {
                            state.keyframes.retimed(property, *before, *after);
                        }
                    }
                }
                Err(e) => state.set_status(Status::warning(e.to_string())),
            }
        }

        Action::SetKeyframeEasing { clip, property, at, interpolation, coalesce } => {
            let command = Box::new(EditKeyframes::one(
                sequence_id,
                clip,
                property,
                KeyframeEdit::SetInterpolation { time: at, interpolation },
            ));
            let result = if coalesce {
                state.history.execute_coalesced(&mut state.project, command)
            } else {
                state.history.execute(&mut state.project, command)
            };
            match result {
                Ok(()) => state.mark_edited(),
                Err(e) => state.set_status(Status::warning(e.to_string())),
            }
        }

        Action::CopyKeyframes => match copy_keyframes(state, sequence_id) {
            0 => state.set_status(Status::warning("no keyframes selected")),
            n => state.set_status(Status::info(format!("copied {n} keyframe{}", plural(n)))),
        },

        Action::PasteKeyframes => paste_keyframes(state, engine, sequence_id),

        Action::SetTrackLevel { track, which, value } => {
            let command = Box::new(SetTrackLevel::new(sequence_id, track, which, value));
            match state.history.execute_coalesced(&mut state.project, command) {
                Ok(()) => state.mark_edited(),
                Err(e) => state.set_status(Status::warning(e.to_string())),
            }
        }

        Action::SetClipMotionBlur { clip, blurred } => {
            let command = Box::new(SetClipMotionBlur::new(sequence_id, clip, blurred));
            match state.history.execute(&mut state.project, command) {
                Ok(()) => {
                    state.mark_edited();
                    state.set_status(Status::info(if blurred {
                        "motion blur on — the clip is now exposed across the shutter"
                    } else {
                        "motion blur off"
                    }));
                }
                Err(e) => state.set_status(Status::warning(e.to_string())),
            }
        }

        Action::SetMotionBlur(motion_blur) => {
            // The shutter belongs to whichever canvas is on screen, exactly as
            // the colour space does.
            let command: Box<dyn ve_command::Command> = match state.viewing() {
                Some(ve_engine::Viewing::Composition(id)) => {
                    let Some(composition) = state.project.composition(id) else { return };
                    if composition.settings.motion_blur == motion_blur {
                        return;
                    }
                    let mut settings = composition.settings.clone();
                    settings.motion_blur = motion_blur;
                    Box::new(SetCompositionSettings::new(id, settings))
                }
                _ => {
                    let current =
                        state.project.sequence(sequence_id).map(|s| s.settings.motion_blur);
                    if current == Some(motion_blur) {
                        return;
                    }
                    Box::new(SetSequenceMotionBlur::new(sequence_id, motion_blur))
                }
            };
            match state.history.execute_coalesced(&mut state.project, command) {
                Ok(()) => state.mark_edited(),
                Err(e) => state.set_status(Status::warning(e.to_string())),
            }
        }

        Action::AddTrack(kind) => {
            let command = Box::new(AddTrack::new(sequence_id, kind));
            match state.history.execute(&mut state.project, command) {
                Ok(()) => {
                    state.mark_edited();
                    let name = state
                        .project
                        .sequence(sequence_id)
                        .and_then(|s| s.tracks.last())
                        .map(|t| t.name.clone())
                        .unwrap_or_default();
                    state.set_status(Status::info(format!("added {name}")));
                }
                Err(e) => state.set_status(Status::error(e.to_string())),
            }
        }

        Action::RemoveTrack(track) => {
            let Some(seq) = state.project.sequence(sequence_id) else { return };
            let Some(doomed) = seq.track(track) else {
                state.set_status(Status::error("no such track"));
                return;
            };
            let (name, clips) = (doomed.name.clone(), doomed.len());
            // Anything selected on the track is about to stop existing.
            let going: Vec<ClipId> = doomed.clips().iter().map(|c| c.id).collect();

            let command = Box::new(RemoveTrack::new(sequence_id, track));
            match state.history.execute(&mut state.project, command) {
                Ok(()) => {
                    state.selection.clips.retain(|c| !going.contains(c));
                    if state.selection.track == Some(track) {
                        state.selection.track = None;
                    }
                    state.mark_edited();
                    state.set_status(Status::info(format!(
                        "deleted {name} and {clips} clip{}",
                        plural(clips)
                    )));
                }
                Err(e) => state.set_status(Status::error(e.to_string())),
            }
        }

        Action::MoveTrack { track, toward_top } => {
            let Some(to) = neighbour_of_same_kind(state, sequence_id, track, toward_top) else {
                state.set_status(Status::warning("the track is already at the end"));
                return;
            };
            let command = Box::new(MoveTrack::new(sequence_id, track, to));
            match state.history.execute(&mut state.project, command) {
                Ok(()) => {
                    state.mark_edited();
                    state.set_status(Status::info("moved track"));
                }
                Err(e) => state.set_status(Status::warning(e.to_string())),
            }
        }

        Action::SetTrackFlag { track, flag, value } => {
            let command = Box::new(SetTrackFlag::new(sequence_id, track, flag, value));
            let label = command.name().to_string();
            match state.history.execute(&mut state.project, command) {
                Ok(()) => {
                    state.mark_edited();
                    state.set_status(Status::info(label.to_lowercase()));
                }
                Err(e) => state.set_status(Status::warning(e.to_string())),
            }
        }

        Action::AddMarkerAtPlayhead => {
            let at = engine.clock().position();
            let Some(seq) = state.project.sequence(sequence_id) else { return };
            if seq.markers.iter().any(|m| m.time == at) {
                state.set_status(Status::warning("there is already a marker here"));
                return;
            }
            let number = seq.markers.len() + 1;
            let id = state.project.new_marker_id();
            let marker = ve_core::Marker::new(id, format!("Marker {number}"), at);
            let command = Box::new(AddMarker::new(sequence_id, marker));
            match state.history.execute(&mut state.project, command) {
                Ok(()) => {
                    state.mark_edited();
                    state.set_status(Status::info("marker added"));
                }
                Err(e) => state.set_status(Status::warning(e.to_string())),
            }
        }

        Action::RemoveMarker(marker) => {
            let command = Box::new(RemoveMarker::new(sequence_id, marker));
            match state.history.execute(&mut state.project, command) {
                Ok(()) => {
                    state.mark_edited();
                    state.set_status(Status::info("marker deleted"));
                }
                Err(e) => state.set_status(Status::warning(e.to_string())),
            }
        }

        Action::GoToMarker(direction) => {
            let at = engine.clock().position();
            let Some(seq) = state.project.sequence(sequence_id) else { return };
            // Markers are kept sorted by time, so the next one in a direction
            // is the first past the playhead from that side.
            let target = if direction >= 0 {
                seq.markers.iter().find(|m| m.time > at).map(|m| m.time)
            } else {
                seq.markers.iter().rev().find(|m| m.time < at).map(|m| m.time)
            };
            match target {
                Some(to) => {
                    if let Some(viewing) = state.viewing() {
                        engine.scrub_to(&state.project, viewing, to);
                    }
                    sync_playhead(state, engine);
                }
                None => state.set_status(Status::warning("no marker that way")),
            }
        }

        Action::SetClipSpeed { clip, speed } => {
            let Some(track) = state
                .project
                .sequence(sequence_id)
                .and_then(|s| s.find_clip(clip))
                .map(|(t, _)| t)
            else {
                state.set_status(Status::warning("no such clip"));
                return;
            };
            let command = Box::new(SetClipSpeed::new(sequence_id, track, clip, speed));
            match state.history.execute_coalesced(&mut state.project, command) {
                Ok(()) => {
                    state.mark_edited();
                    state.set_status(Status::info(format!("speed {:.2}×", speed.as_f64())));
                }
                Err(e) => state.set_status(Status::warning(e.to_string())),
            }
        }

        Action::SetColorSpace(color_space) => {
            // Whichever canvas is on screen is the one being configured, so the
            // control in the inspector always means what is in the preview.
            let command: Box<dyn ve_command::Command> = match state.viewing() {
                Some(ve_engine::Viewing::Composition(id)) => {
                    let Some(composition) = state.project.composition(id) else { return };
                    if composition.settings.color_space == color_space {
                        return;
                    }
                    let mut settings = composition.settings.clone();
                    settings.color_space = color_space;
                    Box::new(SetCompositionSettings::new(id, settings))
                }
                _ => {
                    let current =
                        state.project.sequence(sequence_id).map(|s| s.settings.color_space);
                    if current == Some(color_space) {
                        return;
                    }
                    Box::new(SetSequenceColorSpace::new(sequence_id, color_space))
                }
            };
            match state.history.execute(&mut state.project, command) {
                Ok(()) => {
                    state.mark_edited();
                    state.set_status(Status::info(format!(
                        "compositing in {}",
                        color_space.label().to_lowercase()
                    )));
                }
                Err(e) => state.set_status(Status::warning(e.to_string())),
            }
        }

        Action::SetClipBlendMode { clip, blend } => {
            let command = Box::new(SetClipBlendMode::new(sequence_id, clip, blend));
            match state.history.execute_coalesced(&mut state.project, command) {
                Ok(()) => {
                    state.mark_edited();
                    state.set_status(Status::info(format!("blend {}", blend.label())));
                }
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

        Action::RollEditTo { left, right, track, to, coalesce } => {
            let command = Box::new(RollEdit::new(sequence_id, track, left, right, to));
            gesture(state, command, coalesce);
        }

        Action::SlipClipTo { clip, track, to_source_in, coalesce } => {
            let command = Box::new(SlipClip::new(sequence_id, track, clip, to_source_in));
            gesture(state, command, coalesce);
        }

        Action::SlideClipTo { clip, track, to, coalesce } => {
            let command = Box::new(SlideClip::new(sequence_id, track, clip, to));
            gesture(state, command, coalesce);
        }

        Action::EndGesture => state.history.break_merge(),

        Action::SetTool(tool) => {
            state.tool = tool;
            let (key, what) = tool.hint();
            state.set_status(Status::info(format!("{} tool ({key}) — {what}", tool.label())));
        }

        Action::TogglePlayback => {
            if let Some(viewing) = state.viewing() {
                engine.toggle_playback(&state.project, viewing);
            }
        }

        Action::StepFrames(delta) => {
            if let Some(viewing) = state.viewing() {
                engine.step_frames(&state.project, viewing, delta);
            }
            sync_playhead(state, engine);
        }

        Action::ScrubTo(to) => {
            if let Some(viewing) = state.viewing() {
                engine.scrub_to(&state.project, viewing, to);
            }
            sync_playhead(state, engine);
        }

        Action::GoToStart => {
            if let Some(viewing) = state.viewing() {
                engine.scrub_to(&state.project, viewing, Ticks::ZERO);
            }
            sync_playhead(state, engine);
        }

        Action::GoToEnd => {
            if let Some((viewing, end)) =
                state.viewing().zip(state.timebase().map(|t| t.duration))
            {
                engine.scrub_to(&state.project, viewing, end);
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

        Action::ToggleScopes => {
            state.scopes.open = !state.scopes.open;
        }
        Action::ShowScope(kind) => {
            // Picking a scope opens the panel, so the menu entry for one is
            // also the way to get to it rather than a setting that does nothing
            // until the panel is opened separately.
            state.scopes.kind = kind;
            state.scopes.open = true;
        }
        Action::SetWaveformMode(mode) => state.scopes.waveform = mode,
    }
}

/// The "s" in "3 clips".
fn plural(n: usize) -> &'static str {
    if n == 1 {
        ""
    } else {
        "s"
    }
}

/// The selected clips as `(clip, track, range)`, latest first.
///
/// Reverse timeline order is what makes a ripple delete of several clips work:
/// each removal shifts everything after it, so taking them from the back means
/// the positions captured for the earlier ones are still true when their turn
/// comes.
fn selected_spans(
    state: &EditorState,
    sequence: SequenceId,
) -> Vec<(ClipId, TrackId, ve_time::TimeRange)> {
    let Some(seq) = state.project.sequence(sequence) else { return Vec::new() };
    let mut spans: Vec<_> = state
        .selection
        .clips
        .iter()
        .filter_map(|id| seq.find_clip(*id).map(|(track, clip)| (*id, track, clip.range())))
        .collect();
    spans.sort_by_key(|(_, _, range)| std::cmp::Reverse(range.start));
    spans
}

/// Deletes the selection, optionally closing the holes behind it.
///
/// One compound either way, so however many clips went, undo brings them all
/// back in one step.
fn delete_selected(state: &mut EditorState, sequence: SequenceId, ripple: bool) {
    let targets = selected_spans(state, sequence);
    if targets.is_empty() {
        state.set_status(Status::warning("nothing selected"));
        return;
    }

    let count = targets.len();
    let mut command = Compound::new(if ripple { "Ripple Delete" } else { "Delete Clip" });
    for (clip, track, range) in targets {
        command.push(Box::new(RemoveClip::new(sequence, track, clip)));
        if ripple {
            command.push(Box::new(ShiftClips::new(
                sequence,
                track,
                range.end(),
                -range.duration,
            )));
        }
    }

    match state.history.execute(&mut state.project, Box::new(command)) {
        Ok(()) => {
            state.selection.clear();
            state.mark_edited();
            state.set_status(Status::info(format!("deleted {count} clip{}", plural(count))));
        }
        Err(e) => state.set_status(Status::error(e.to_string())),
    }
}

/// Runs one step of a drag gesture.
///
/// A refused step is ordinary during a drag — the pointer is over a neighbour,
/// or past the end of the media — so it is not worth a status message. The
/// command layer guarantees a failed command touched nothing, so the edit
/// simply does not move until the pointer comes back into range.
fn gesture(state: &mut EditorState, command: Box<dyn Command>, coalesce: bool) {
    let result = if coalesce {
        state.history.execute_coalesced(&mut state.project, command)
    } else {
        state.history.execute(&mut state.project, command)
    };
    if result.is_ok() {
        state.mark_edited();
    }
}

/// Where a track lands when it swaps with its neighbour of the same kind.
///
/// Video and audio stay grouped, so reordering V2 never sends it into the audio
/// half of the stack. "Toward the top" is the direction the timeline draws
/// rather than the order tracks are stored in: video is drawn in reverse, so
/// that V2 sits above V1 and the picture reads the way it composites, while
/// audio is drawn in order.
fn neighbour_of_same_kind(
    state: &EditorState,
    sequence: SequenceId,
    track: TrackId,
    toward_top: bool,
) -> Option<usize> {
    let seq = state.project.sequence(sequence)?;
    let index = seq.track_index(track)?;
    let kind = seq.tracks[index].kind;
    let later = match kind {
        TrackKind::Video => toward_top,
        TrackKind::Audio => !toward_top,
    };
    if later {
        seq.tracks.iter().enumerate().skip(index + 1).find(|(_, t)| t.kind == kind)
    } else {
        seq.tracks.iter().enumerate().take(index).rev().find(|(_, t)| t.kind == kind)
    }
    .map(|(i, _)| i)
}

/// Puts the selected clips on the clipboard. Returns how many were captured.
///
/// A copy with nothing selected leaves the clipboard as it was: pressing the
/// key with no selection is a slip, and losing what was already there over it
/// would be the worst possible answer.
fn copy_selection(state: &mut EditorState, sequence: SequenceId) -> usize {
    let Some(seq) = state.project.sequence(sequence) else { return 0 };
    let clips: Vec<(usize, Clip)> = seq
        .tracks
        .iter()
        .enumerate()
        .flat_map(|(index, track)| {
            track
                .clips()
                .iter()
                .filter(|c| state.selection.is_selected(c.id))
                .map(move |c| (index, c.clone()))
        })
        .collect();
    if clips.is_empty() {
        return 0;
    }
    let count = clips.len();
    state.clipboard.fill(clips);
    // The clip clipboard is now the one a paste reads. See `copy_keyframes`.
    state.keyframe_clipboard.clear();
    count
}

/// Places the clipboard at the playhead, starting on the selected track.
///
/// All or nothing: a paste that would land on top of an existing clip is
/// refused outright rather than dropping the clips that did not fit, because a
/// partial paste is the kind of thing a user only notices much later.
fn paste(state: &mut EditorState, engine: &PlaybackEngine, sequence: SequenceId) {
    if state.clipboard.is_empty() {
        state.set_status(Status::warning("the clipboard is empty"));
        return;
    }
    let at = engine.clock().position();

    let placements = match plan_paste(state, sequence, at) {
        Ok(placements) => placements,
        Err(why) => {
            state.set_status(Status::warning(why));
            return;
        }
    };

    let mut command = Compound::new("Paste");
    let mut pasted = Vec::with_capacity(placements.len());
    for (track, start, mut clip) in placements {
        // Fresh IDs, so the same copy can be pasted any number of times and
        // each result is its own clip.
        clip.id = state.project.new_clip_id();
        for effect in &mut clip.effects {
            effect.id = state.project.new_effect_id();
        }
        clip.timeline_start = start;
        pasted.push((clip.id, track));
        command.push(Box::new(AddClip::new(sequence, track, clip)));
    }

    match state.history.execute(&mut state.project, Box::new(command)) {
        Ok(()) => {
            let count = pasted.len();
            state.selection.clips = pasted.iter().map(|(clip, _)| *clip).collect();
            state.selection.track = pasted.first().map(|(_, track)| *track);
            state.mark_edited();
            state.set_status(Status::info(format!("pasted {count} clip{}", plural(count))));
        }
        Err(e) => state.set_status(Status::warning(format!("could not paste: {e}"))),
    }
}

/// Works out where every clipboard clip would land, or why it cannot.
///
/// Separate from [`paste`] so the whole plan is checked before a single ID is
/// minted: a refused paste must leave the allocator exactly as it found it.
fn plan_paste(
    state: &EditorState,
    sequence: SequenceId,
    at: Ticks,
) -> Result<Vec<(TrackId, Ticks, Clip)>, String> {
    let seq = state
        .project
        .sequence(sequence)
        .ok_or_else(|| "this project has no sequence".to_string())?;
    let base = state
        .selection
        .track
        .and_then(|t| seq.track_index(t))
        .or_else(|| (!seq.tracks.is_empty()).then_some(0))
        .ok_or_else(|| "this sequence has no tracks".to_string())?;

    // Whether the stack of tracks is deep enough is a property of the paste
    // point rather than of any one clip, so it is answered first and as a
    // whole.
    let depth = state.clipboard.entries().iter().map(|e| e.track_offset).max().unwrap_or(0);
    if base + depth >= seq.tracks.len() {
        return Err(
            "the clipboard needs more tracks than there are below the paste point".to_string()
        );
    }

    state
        .clipboard
        .entries()
        .iter()
        .map(|entry| {
            let track = &seq.tracks[base + entry.track_offset];
            // A clip does not record whether it was video or audio — that was
            // its track's business — so what it can be pasted onto is decided
            // by its source, exactly as a fresh import is.
            let usable = match entry.clip.asset().and_then(|a| state.project.asset(a)) {
                Some(asset) => match track.kind {
                    TrackKind::Video => asset.info.has_video(),
                    TrackKind::Audio => asset.info.has_audio(),
                },
                // A composition is picture, so it belongs on a video track.
                // Media missing from the project cannot be judged here; the
                // loader's missing-asset warning already covers that case.
                None => match entry.clip.composition() {
                    Some(_) => track.kind == TrackKind::Video,
                    None => true,
                },
            };
            if !usable {
                return Err(format!(
                    "{} cannot be pasted onto {}",
                    entry.clip.name, track.name
                ));
            }
            // Space is checked here rather than being left to `AddClip` so that
            // a paste that cannot land is refused before a single ID has been
            // minted for it. Clips from one track keep their spacing, so they
            // can only collide with what is already on the timeline.
            let landing = ve_time::TimeRange::new(at + entry.offset, entry.clip.duration);
            if !track.is_range_free(landing, None) {
                return Err(format!(
                    "{} would land on top of a clip on {}",
                    entry.clip.name, track.name
                ));
            }
            Ok((track.id, landing.start, entry.clip.clone()))
        })
        .collect()
}

/// Stores the playhead on whatever is being viewed, so reopening the project
/// restores where the user was — in the sequence *and* in each composition.
fn sync_playhead(state: &mut EditorState, engine: &PlaybackEngine) {
    let position = engine.clock().position();
    match state.viewing() {
        Some(ve_engine::Viewing::Composition(id)) => {
            if let Some(comp) = state.project.composition_mut(id) {
                comp.playhead = position;
            }
        }
        _ => {
            if let Some(seq) = state.project.active_mut() {
                seq.playhead = position;
            }
        }
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

// ---- proxies ----------------------------------------------------------

/// Starts building proxies for the project's video assets.
///
/// `rebuild` decides whether assets that already have a usable proxy are done
/// again — which is what "the footage on disk changed" or "a quarter was too
/// small" needs, and what an ordinary build should not waste minutes on.
fn start_proxy_build(state: &mut EditorState, rebuild: bool) {
    if state.proxies.is_running() {
        state.set_status(Status::warning("a proxy build is already running"));
        return;
    }

    // Proxies live beside the project file, so there has to be one. Writing
    // them somewhere temporary instead would mean a folder of orphaned files
    // that nothing ever points at again.
    let Some(project_path) = state.path.clone() else {
        state.set_status(Status::warning(
            "save the project first, so its proxies have somewhere to live beside it",
        ));
        return;
    };
    let dir = ve_export::proxy_dir_for(&project_path);
    let scale = state.proxies.scale;

    let items: Vec<_> = state
        .project
        .assets
        .iter()
        .filter(|a| !a.offline && a.info.has_video())
        // An asset whose proxy is already on disk is skipped unless this is a
        // rebuild. One whose proxy has *gone* is built again either way: that
        // is exactly the case this is for.
        .filter(|a| rebuild || !a.picture_source(None, true).is_proxy)
        .filter_map(|a| ve_export::ProxySettings::for_asset(a, &dir, scale).map(|s| (a.id, s)))
        .collect();

    if items.is_empty() {
        state.set_status(Status::info(if state.project.assets.is_empty() {
            "there is no media to build proxies for".to_string()
        } else {
            "every clip already has a proxy".to_string()
        }));
        return;
    }

    let count = items.len();
    state.proxies.last = None;
    state.set_status(Status::info(format!(
        "building {count} {} at {}…",
        if count == 1 { "proxy" } else { "proxies" },
        scale.label().to_lowercase()
    )));
    state.proxies.job =
        Some(ve_export::ProxyJob::start(items, state.proxies.metrics.clone(), dir));
}

/// Takes whatever the running proxy build has said and acts on it.
///
/// Called once a frame from the shell's housekeeping, and directly by tests —
/// which is why it lives here rather than in the window, and why it takes the
/// state and the engine rather than the application.
///
/// Each finished file is attached to its asset through a command, so the
/// reference is in the undo history and marks the project dirty: a proxy built
/// and then not saved would be a file on disk that nothing points at.
pub fn poll_proxies(state: &mut EditorState, engine: &mut PlaybackEngine) {
    let Some(job) = state.proxies.job.as_mut() else { return };

    let events = job.poll();
    let mut done = false;
    let mut built = 0usize;
    let mut failed = 0usize;
    let mut attached = false;

    for event in events {
        match event {
            ve_export::ProxyEvent::Progress(_) => {}
            ve_export::ProxyEvent::Built { asset, media, report } => {
                log::info!(
                    "built {} — {}×{}, {} frames in {:.1}s",
                    report.path.display(),
                    report.size.width,
                    report.size.height,
                    report.frames,
                    report.elapsed.as_secs_f64()
                );
                let command = SetAssetProxy::attach(asset, media);
                if state.history.execute(&mut state.project, Box::new(command)).is_ok() {
                    built += 1;
                    attached = true;
                }
            }
            ve_export::ProxyEvent::Failed { asset, message } => {
                let name = state
                    .project
                    .asset(asset)
                    .map(|a| a.name.clone())
                    .unwrap_or_else(|| format!("asset {asset}"));
                let note = format!("no proxy for {name}: {message}");
                log::warn!("{note}");
                state.warnings.push(note);
                failed += 1;
            }
            ve_export::ProxyEvent::Finished => done = true,
            ve_export::ProxyEvent::Cancelled => {
                state.set_status(Status::info("proxy build cancelled"));
                state.proxies.last = Some("cancelled".into());
                done = true;
            }
        }
    }

    // Attaching a proxy changes which file an asset should decode from, but
    // only while proxies are switched on — otherwise the workers are already
    // reading the right thing and replacing them would be churn for nothing.
    if attached && state.project.settings.use_proxies {
        let failures = engine.reopen_project_assets(&state.project);
        for (asset, why) in &failures {
            log::warn!("could not reopen asset {asset}: {why}");
        }
    }

    if built > 0 || failed > 0 {
        let mut summary = format!("{built} built");
        if failed > 0 {
            summary.push_str(&format!(", {failed} failed"));
        }
        state.proxies.last = Some(summary);
    }

    if done {
        // Dropping the job joins its thread, which is where a cancelled build
        // removes the file it was part way through.
        state.proxies.job = None;
        if built > 0 {
            let line = format!(
                "{built} {} built{}",
                if built == 1 { "proxy" } else { "proxies" },
                if failed > 0 { format!(", {failed} failed") } else { String::new() }
            );
            // A build that lost a file says so as a warning rather than
            // reporting a round number that quietly leaves one out.
            state.set_status(if failed > 0 {
                Status::warning(line)
            } else {
                Status::info(line)
            });
        } else if failed > 0 {
            state.set_status(Status::error(format!("no proxies were built; {failed} failed")));
        }
    }
}

/// Probes a file and adds it as an asset. Returns whether anything was added.
fn import_one(
    state: &mut EditorState,
    engine: &PlaybackEngine,
    waveforms: &WaveformService,
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
    let audio = info.audio.as_ref().map(|a| a.sample_rate);
    let asset = state.project.add_asset(&path, info);
    if has_video {
        if let Err(e) = engine.decode_service().open(asset, &path, None) {
            log::warn!("imported {} but could not open a decoder: {e}", path.display());
        }
    }
    // Analyse on import rather than waiting for the clip to be drawn. The user
    // has just chosen this file and is about to lay it down, so the analysis
    // and the decision about where to cut it overlap instead of queueing.
    // Opening a *project* deliberately does not do this: a hundred assets would
    // be analysed to draw the few that are on screen, and evict each other
    // doing it. Those are asked for by the timeline as it paints them.
    if let Some(sample_rate) = audio {
        waveforms.request(asset, &path, sample_rate);
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

/// A sequence time as the clip's own properties see it, or `None` when the
/// playhead is not over the clip at all.
///
/// Snapped to the frame grid first: a keyframe dropped where a scrub happened
/// to leave the playhead would sit a few thousand ticks off the frame it looks
/// like it is on, and every later one would too.
fn clip_local(
    state: &EditorState,
    sequence: SequenceId,
    clip: ClipId,
    at: Ticks,
) -> Option<Ticks> {
    let seq = state.project.sequence(sequence)?;
    let (_, c) = seq.find_clip(clip)?;
    let at = seq.snap_to_frame(at);
    c.range().contains(at).then(|| c.local_time_at(at))
}

/// The easing a keyframe already at `at` carries, so that setting its value
/// does not quietly straighten the curve leaving it.
fn keyframe_easing(
    state: &EditorState,
    sequence: SequenceId,
    clip: ClipId,
    property: &ClipProperty,
    at: Ticks,
) -> Interpolation {
    state
        .project
        .sequence(sequence)
        .and_then(|s| s.find_clip(clip))
        .and_then(|(_, c)| property_ref(c, property))
        .and_then(|view| view.keyframe_at(at))
        .map(|k| k.interpolation)
        .unwrap_or_default()
}

/// Drops selected keyframes that no longer exist, which an undo or a delete can
/// cause.
fn prune_keyframe_selection(state: &mut EditorState, sequence: SequenceId) {
    let Some(clip) = state.keyframes.clip else { return };
    let Some(found) = state.project.sequence(sequence).and_then(|s| s.find_clip(clip)) else {
        state.keyframes.clear();
        return;
    };
    let alive: Vec<(ClipProperty, Ticks)> = state
        .keyframes
        .keys()
        .iter()
        .filter(|(property, time)| {
            property_ref(found.1, property)
                .map(|view| view.keyframe_at(*time).is_some())
                .unwrap_or(false)
        })
        .cloned()
        .collect();
    state.keyframes.retain(alive);
}

/// Deletes every selected keyframe, as one undo step however many properties
/// they were spread across.
fn delete_selected_keyframes(state: &mut EditorState, sequence: SequenceId) {
    let Some(clip) = state.keyframes.clip else {
        state.set_status(Status::warning("no keyframes selected"));
        return;
    };
    let edits: Vec<(ClipProperty, KeyframeEdit)> = state
        .keyframes
        .properties()
        .into_iter()
        .map(|property| {
            let times = state.keyframes.times_on(&property);
            (property, KeyframeEdit::Remove(times))
        })
        .collect();
    if edits.is_empty() {
        state.set_status(Status::warning("no keyframes selected"));
        return;
    }
    let count = state.keyframes.len();
    let command = Box::new(EditKeyframes::new(sequence, clip, edits));
    match state.history.execute(&mut state.project, command) {
        Ok(()) => {
            state.mark_edited();
            state.keyframes.clear();
            state
                .set_status(Status::info(format!("deleted {count} keyframe{}", plural(count))));
        }
        Err(e) => state.set_status(Status::warning(e.to_string())),
    }
}

/// Lifts the selected keyframes onto the keyframe clipboard, and returns how
/// many were taken.
///
/// The clip clipboard is emptied by this, and filling the clip clipboard empties
/// this one: whichever was copied last is what a paste pastes, so Ctrl+V never
/// has to guess which of two clipboards the user meant.
fn copy_keyframes(state: &mut EditorState, sequence: SequenceId) -> usize {
    let Some(clip) = state.keyframes.clip else { return 0 };
    let Some((_, found)) = state.project.sequence(sequence).and_then(|s| s.find_clip(clip))
    else {
        return 0;
    };
    let entries: Vec<(ClipProperty, Vec<KeyframePoint>)> = state
        .keyframes
        .properties()
        .into_iter()
        .filter_map(|property| {
            let view = property_ref(found, &property)?;
            let taken: Vec<KeyframePoint> = state
                .keyframes
                .times_on(&property)
                .into_iter()
                .filter_map(|time| view.keyframe_at(time))
                .collect();
            (!taken.is_empty()).then_some((property, taken))
        })
        .collect();

    let count = entries.iter().map(|(_, kfs)| kfs.len()).sum();
    state.keyframe_clipboard.fill(entries);
    if count > 0 {
        state.clipboard.clear();
    }
    count
}

/// Pastes the copied keyframes onto the selected clip, starting at the
/// playhead.
///
/// Onto the *selected* clip rather than the one they came from: copying a move
/// from one clip onto another is most of what a keyframe clipboard is for.
fn paste_keyframes(state: &mut EditorState, engine: &PlaybackEngine, sequence: SequenceId) {
    if state.keyframe_clipboard.is_empty() {
        state.set_status(Status::warning("no keyframes copied"));
        return;
    }
    let Some(clip) = state.selection.only() else {
        state.set_status(Status::warning("select one clip to paste keyframes onto"));
        return;
    };
    let at = engine.clock().position();
    let Some(local) = clip_local(state, sequence, clip, at) else {
        state.set_status(Status::warning("the playhead is not over the selected clip"));
        return;
    };

    let entries = state.keyframe_clipboard.entries().to_vec();
    let count: usize = entries.iter().map(|(_, kfs)| kfs.len()).sum();
    let edits: Vec<(ClipProperty, KeyframeEdit)> = entries
        .iter()
        .map(|(property, keyframes)| {
            (property.clone(), KeyframeEdit::Insert { keyframes: keyframes.clone(), at: local })
        })
        .collect();

    let command = Box::new(EditKeyframes::new(sequence, clip, edits));
    match state.history.execute(&mut state.project, command) {
        Ok(()) => {
            state.mark_edited();
            // What was just pasted is what the user is now holding, so it can
            // be moved, retimed or deleted without hunting for it.
            state.keyframes.clear();
            for (property, keyframes) in &entries {
                for kf in keyframes {
                    state.keyframes.add(clip, property.clone(), local + kf.time);
                }
            }
            state.set_status(Status::info(format!("pasted {count} keyframe{}", plural(count))));
        }
        Err(e) => state.set_status(Status::warning(e.to_string())),
    }
}
