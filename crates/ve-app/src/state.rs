//! Editor state that is not the project itself.

use std::path::PathBuf;
use std::time::Duration;

use ve_command::{ClipProperty, History, KeyframePoint};
use ve_core::{AssetId, Clip, ClipId, CompositionId, LayerId, Project, SequenceId, TrackId};
use ve_engine::{Timebase, Viewing};
use ve_project::Autosave;
use ve_time::Ticks;

/// What the user currently has selected.
#[derive(Debug, Default, Clone)]
pub struct Selection {
    pub clips: Vec<ClipId>,
    /// Layers selected inside an open composition. Separate from `clips`
    /// because the two live in different places and a command has to know which
    /// it is being handed.
    pub layers: Vec<LayerId>,
    /// The track most recently interacted with, which is where a paste or an
    /// import-to-timeline lands.
    pub track: Option<TrackId>,
    pub asset: Option<AssetId>,
}

impl Selection {
    pub fn is_selected(&self, clip: ClipId) -> bool {
        self.clips.contains(&clip)
    }

    pub fn select_only(&mut self, clip: ClipId, track: TrackId) {
        self.clips.clear();
        self.clips.push(clip);
        self.track = Some(track);
    }

    pub fn toggle(&mut self, clip: ClipId, track: TrackId) {
        match self.clips.iter().position(|c| *c == clip) {
            Some(i) => {
                self.clips.remove(i);
            }
            None => self.clips.push(clip),
        }
        self.track = Some(track);
    }

    pub fn clear(&mut self) {
        self.clips.clear();
        self.layers.clear();
    }

    pub fn select_only_layer(&mut self, layer: LayerId) {
        self.layers.clear();
        self.layers.push(layer);
    }

    pub fn toggle_layer(&mut self, layer: LayerId) {
        match self.layers.iter().position(|l| *l == layer) {
            Some(i) => {
                self.layers.remove(i);
            }
            None => self.layers.push(layer),
        }
    }

    pub fn is_layer_selected(&self, layer: LayerId) -> bool {
        self.layers.contains(&layer)
    }

    pub fn only_layer(&self) -> Option<LayerId> {
        match self.layers.as_slice() {
            [one] => Some(*one),
            _ => None,
        }
    }

    pub fn only(&self) -> Option<ClipId> {
        match self.clips.as_slice() {
            [one] => Some(*one),
            _ => None,
        }
    }
}

/// Clips lifted from the timeline, waiting to be pasted.
///
/// Positions are stored *relative* — each clip's offset from the earliest one
/// copied, and its track's offset from the topmost track involved — rather than
/// absolutely. That is what makes a paste reconstruct the shape of the copy
/// wherever the playhead and the target track happen to be, instead of only
/// working where the clips came from.
///
/// The clips keep their original IDs here and are given fresh ones on paste, so
/// the same copy can be pasted any number of times.
#[derive(Debug, Default, Clone)]
pub struct Clipboard {
    entries: Vec<ClipboardEntry>,
}

#[derive(Debug, Clone)]
pub struct ClipboardEntry {
    /// How many tracks below the paste target this clip belongs.
    pub track_offset: usize,
    /// How far after the paste point this clip starts.
    pub offset: Ticks,
    pub clip: Clip,
}

impl Clipboard {
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn entries(&self) -> &[ClipboardEntry] {
        &self.entries
    }

    /// Replaces the contents with `(track index, clip)` pairs, in any order.
    pub fn fill(&mut self, clips: Vec<(usize, Clip)>) {
        self.entries.clear();
        let Some(origin) = clips.iter().map(|(_, c)| c.timeline_start).min() else { return };
        let top = clips.iter().map(|(i, _)| *i).min().unwrap_or(0);
        self.entries = clips
            .into_iter()
            .map(|(index, clip)| ClipboardEntry {
                track_offset: index - top,
                offset: clip.timeline_start - origin,
                clip,
            })
            .collect();
    }

    pub fn clear(&mut self) {
        self.entries.clear();
    }
}

/// Which keyframes the user has hold of.
///
/// Separate from the clip selection rather than folded into it: the two answer
/// different questions, and a keyframe being selected must not make Delete take
/// the clip it belongs to. A keyframe is named by the property it sits on and
/// the clip-relative time it sits at, which is exactly what a command needs.
#[derive(Debug, Default, Clone)]
pub struct KeyframeSelection {
    /// The clip whose keyframes these are. Clearing the clip selection or
    /// picking another clip empties this, because a keyframe from a clip that
    /// is no longer open is not something the user can see or act on.
    pub clip: Option<ClipId>,
    keys: Vec<(ClipProperty, Ticks)>,
}

impl KeyframeSelection {
    pub fn is_empty(&self) -> bool {
        self.keys.is_empty()
    }

    pub fn len(&self) -> usize {
        self.keys.len()
    }

    pub fn keys(&self) -> &[(ClipProperty, Ticks)] {
        &self.keys
    }

    pub fn contains(&self, property: &ClipProperty, time: Ticks) -> bool {
        self.keys.iter().any(|(p, t)| p == property && *t == time)
    }

    /// The selected times on one property, in order.
    pub fn times_on(&self, property: &ClipProperty) -> Vec<Ticks> {
        let mut times: Vec<Ticks> =
            self.keys.iter().filter(|(p, _)| p == property).map(|(_, t)| *t).collect();
        times.sort_unstable();
        times
    }

    /// The properties with anything selected on them, in the order they were
    /// first touched.
    pub fn properties(&self) -> Vec<ClipProperty> {
        let mut properties: Vec<ClipProperty> = Vec::new();
        for (property, _) in &self.keys {
            if !properties.contains(property) {
                properties.push(property.clone());
            }
        }
        properties
    }

    /// The span the selection covers, which is what a scale gesture works on.
    pub fn span(&self) -> Option<(Ticks, Ticks)> {
        let first = self.keys.iter().map(|(_, t)| *t).min()?;
        let last = self.keys.iter().map(|(_, t)| *t).max()?;
        Some((first, last))
    }

    pub fn select_only(&mut self, clip: ClipId, property: ClipProperty, time: Ticks) {
        self.clip = Some(clip);
        self.keys.clear();
        self.keys.push((property, time));
    }

    pub fn toggle(&mut self, clip: ClipId, property: ClipProperty, time: Ticks) {
        if self.clip != Some(clip) {
            self.select_only(clip, property, time);
            return;
        }
        match self.keys.iter().position(|(p, t)| *p == property && *t == time) {
            Some(i) => {
                self.keys.remove(i);
            }
            None => self.keys.push((property, time)),
        }
    }

    pub fn add(&mut self, clip: ClipId, property: ClipProperty, time: Ticks) {
        if self.clip != Some(clip) {
            self.clear();
            self.clip = Some(clip);
        }
        if !self.contains(&property, time) {
            self.keys.push((property, time));
        }
    }

    pub fn remove(&mut self, property: &ClipProperty, time: Ticks) {
        self.keys.retain(|(p, t)| p != property || *t != time);
    }

    /// Keeps only these keys, which is how a selection follows a delete or an
    /// undo that took some of its keyframes away.
    pub fn retain(&mut self, keys: Vec<(ClipProperty, Ticks)>) {
        self.keys = keys;
        if self.keys.is_empty() {
            self.clip = None;
        }
    }

    pub fn clear(&mut self) {
        self.clip = None;
        self.keys.clear();
    }

    /// Follows a retime: the keyframes are the same ones, at new times.
    pub fn retimed(&mut self, property: &ClipProperty, from: Ticks, to: Ticks) {
        for (p, t) in &mut self.keys {
            if p == property && *t == from {
                *t = to;
            }
        }
    }
}

/// Keyframes lifted from a clip, waiting to be pasted.
///
/// Times are stored **relative to the earliest keyframe copied**, across every
/// property at once rather than per property. That is what makes a paste
/// reproduce the shape of the copy: a position curve and an opacity curve that
/// were a second apart stay a second apart, wherever the playhead is when they
/// land.
#[derive(Debug, Default, Clone)]
pub struct KeyframeClipboard {
    entries: Vec<(ClipProperty, Vec<KeyframePoint>)>,
}

impl KeyframeClipboard {
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// How many keyframes are held, across every property.
    pub fn len(&self) -> usize {
        self.entries.iter().map(|(_, kfs)| kfs.len()).sum()
    }

    pub fn entries(&self) -> &[(ClipProperty, Vec<KeyframePoint>)] {
        &self.entries
    }

    /// Replaces the contents, re-basing every time on the earliest of them.
    pub fn fill(&mut self, entries: Vec<(ClipProperty, Vec<KeyframePoint>)>) {
        self.entries.clear();
        let origin = entries.iter().flat_map(|(_, kfs)| kfs.iter().map(|k| k.time)).min();
        let Some(origin) = origin else { return };
        self.entries = entries
            .into_iter()
            .map(|(property, keyframes)| {
                let rebased = keyframes
                    .into_iter()
                    .map(|mut k| {
                        k.time -= origin;
                        k
                    })
                    .collect();
                (property, rebased)
            })
            .collect();
    }

    pub fn clear(&mut self) {
        self.entries.clear();
    }
}

/// What the animation editor is showing.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum AnimationMode {
    /// Keyframes as diamonds on the timeline's own time axis: where things
    /// happen, and when.
    #[default]
    Sheet,
    /// The curves themselves: what the value does between keyframes, and the
    /// handles that shape it.
    Curves,
}

impl AnimationMode {
    pub const ALL: [AnimationMode; 2] = [AnimationMode::Sheet, AnimationMode::Curves];

    pub fn label(self) -> &'static str {
        match self {
            AnimationMode::Sheet => "Sheet",
            AnimationMode::Curves => "Curves",
        }
    }

    pub fn hint(self) -> &'static str {
        match self {
            AnimationMode::Sheet => "Keyframes in time, aligned with the timeline",
            AnimationMode::Curves => "The value between keyframes, and its handles",
        }
    }
}

/// A gesture in progress in the animation editor.
///
/// Held across frames for the same reason a timeline drag is: the keyframes
/// being moved and where they started have to survive until the pointer comes
/// up. Every variant carries the times as they were **at the grab**, so each
/// pointer move states an absolute destination rather than accumulating deltas
/// — which is what lets the whole gesture merge into one undo step.
#[derive(Debug, Clone, PartialEq)]
pub enum KeyframeGesture {
    /// Sliding the selected keyframes along the timeline.
    Move { grab: Ticks, snapshot: Vec<PropertyTimes> },
    /// Scaling the selected span about its far end.
    Scale { anchor: Ticks, grab: Ticks, snapshot: Vec<PropertyTimes> },
    /// Dragging one end of a curve segment's bezier handle.
    Handle { property: ClipProperty, time: Ticks, outgoing: bool },
    /// Dragging a keyframe's value in the curve editor.
    Value { property: ClipProperty, time: Ticks, channel: usize },
}

/// Every keyframe time on one property as a gesture began, and which of them
/// the gesture moves.
///
/// The whole list is kept, not only the moving ones: retiming states where
/// *all* of a property's keyframes now are, so the ones standing still have to
/// be restated too.
#[derive(Debug, Clone, PartialEq)]
pub struct PropertyTimes {
    pub property: ClipProperty,
    pub times: Vec<Ticks>,
    pub moving: Vec<bool>,
}

/// The animation editor's own state: whether it is open, which of its two
/// views it is showing, and the gesture in progress.
#[derive(Debug, Clone, Default)]
pub struct AnimationView {
    pub open: bool,
    pub mode: AnimationMode,
    /// The property the curve editor draws. `None` draws every animated
    /// property at once, which is how a graph editor usually starts.
    pub focus: Option<ClipProperty>,
    pub drag: Option<KeyframeGesture>,
}

/// How the timeline is scrolled and zoomed.
#[derive(Debug, Clone)]
pub struct TimelineView {
    /// Horizontal scale. The single number the whole timeline geometry derives
    /// from, so zooming is one multiply rather than a layout pass.
    pub pixels_per_second: f32,
    /// Timeline position at the left edge of the clip area.
    pub scroll: Ticks,
    /// Pixels the track stack is scrolled down by. A sequence can hold more
    /// tracks than fit, and a clipped bottom row is worse than a scrollbar.
    pub scroll_y: f32,
    pub header_width: f32,
    pub ruler_height: f32,
    /// Snapping to clip edges, markers and the origin.
    pub snapping: bool,
}

impl Default for TimelineView {
    fn default() -> Self {
        TimelineView {
            pixels_per_second: 60.0,
            scroll: Ticks::ZERO,
            scroll_y: 0.0,
            header_width: 148.0,
            ruler_height: 22.0,
            snapping: true,
        }
    }
}

impl TimelineView {
    pub const MIN_ZOOM: f32 = 0.5;
    pub const MAX_ZOOM: f32 = 4000.0;

    /// X offset, relative to the start of the clip area, for a timeline position.
    pub fn x_of(&self, at: Ticks) -> f32 {
        (at - self.scroll).as_secs_f32() * self.pixels_per_second
    }

    /// The timeline position an X offset corresponds to.
    pub fn time_at(&self, x: f32) -> Ticks {
        self.scroll + Ticks::from_secs_f64((x / self.pixels_per_second) as f64)
    }

    /// Zooms about a fixed timeline position, so the thing under the pointer
    /// stays under the pointer.
    pub fn zoom_about(&mut self, factor: f32, anchor_time: Ticks, anchor_x: f32) {
        let new_zoom = (self.pixels_per_second * factor).clamp(Self::MIN_ZOOM, Self::MAX_ZOOM);
        if new_zoom == self.pixels_per_second {
            return;
        }
        self.pixels_per_second = new_zoom;
        // Re-derive the scroll so `anchor_time` lands back on `anchor_x`.
        let seconds_before_anchor = (anchor_x / self.pixels_per_second) as f64;
        self.scroll =
            (anchor_time - Ticks::from_secs_f64(seconds_before_anchor)).clamp_non_negative();
    }

    /// Fits `duration` into `width` pixels with a little breathing room.
    pub fn zoom_to_fit(&mut self, duration: Ticks, width: f32) {
        let seconds = duration.as_secs_f32().max(1.0);
        self.pixels_per_second =
            ((width * 0.95) / seconds).clamp(Self::MIN_ZOOM, Self::MAX_ZOOM);
        self.scroll = Ticks::ZERO;
    }

    /// Clamps the vertical scroll so it can never leave blank space below the
    /// last track.
    pub fn clamp_scroll_y(&mut self, content_height: f32, viewport_height: f32) {
        let max = (content_height - viewport_height).max(0.0);
        self.scroll_y = self.scroll_y.clamp(0.0, max);
    }

    /// Scrolls the minimum amount needed to bring `at` into view.
    pub fn scroll_to_show(&mut self, at: Ticks, width: f32) {
        let visible = Ticks::from_secs_f64((width / self.pixels_per_second) as f64);
        // A margin, so the playhead does not sit right against the edge as it
        // moves during playback.
        let margin = Ticks::new(visible.raw() / 10);
        if at < self.scroll + margin {
            self.scroll = (at - margin).clamp_non_negative();
        } else if at > self.scroll + visible - margin {
            self.scroll = (at + margin - visible).clamp_non_negative();
        }
    }
}

/// Which gesture a drag in the timeline performs.
///
/// A mode rather than a pile of modifier keys. Roll, slip and slide all start
/// from a press on a clip, so telling them apart by modifier would mean three
/// chords nobody can discover; a tool that is visible in the toolbar, named in
/// the status line and switchable with one key is how every editor does this,
/// and for the same reason.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum TimelineTool {
    #[default]
    Select,
    Roll,
    Slip,
    Slide,
}

impl TimelineTool {
    pub const ALL: [TimelineTool; 4] =
        [TimelineTool::Select, TimelineTool::Roll, TimelineTool::Slip, TimelineTool::Slide];

    pub fn label(self) -> &'static str {
        match self {
            TimelineTool::Select => "Select",
            TimelineTool::Roll => "Roll",
            TimelineTool::Slip => "Slip",
            TimelineTool::Slide => "Slide",
        }
    }

    /// The key that selects it, and what it does, for tooltips and the
    /// shortcuts table.
    pub fn hint(self) -> (&'static str, &'static str) {
        match self {
            TimelineTool::Select => ("V", "Select, move and trim clips"),
            TimelineTool::Roll => ("N", "Drag a cut to move it, keeping the sequence length"),
            TimelineTool::Slip => {
                ("Y", "Drag a clip to change which frames it shows, in place")
            }
            TimelineTool::Slide => {
                ("U", "Drag a clip to move it, taking the time from its neighbours")
            }
        }
    }
}

/// What a drag in the timeline is doing.
///
/// Held across frames because a drag is a gesture, not an event: the clip being
/// moved and where it was grabbed have to survive until the pointer is released.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub enum TimelineDrag {
    #[default]
    None,
    /// Scrubbing, from the ruler or from empty timeline space.
    Playhead,
    /// Sliding a clip. `grab_offset` is how far into the clip the pointer took
    /// hold, so the clip does not jump to centre itself under the cursor.
    MoveClip { clip: ClipId, track: TrackId, grab_offset: Ticks },
    /// Dragging a clip edge.
    TrimClip { clip: ClipId, track: TrackId, edge: TrimEdgeKind },
    /// Sweeping out a selection rectangle. The origin is kept as a timeline
    /// position and a screen Y rather than a point, so the rectangle survives a
    /// horizontal scroll mid-drag.
    Marquee { origin_time: Ticks, origin_y: f32 },
    /// Dragging the cut between two clips that meet.
    RollEdge { left: ClipId, right: ClipId, track: TrackId },
    /// Dragging a clip's contents within its own window. `source_in_at_grab`
    /// is captured so every pointer move states an absolute destination, which
    /// is what lets the gesture merge into one undo step.
    SlipClip { clip: ClipId, track: TrackId, grab_time: Ticks, source_in_at_grab: Ticks },
    /// Dragging a clip into its neighbours.
    SlideClip { clip: ClipId, track: TrackId, grab_offset: Ticks },
    /// Pulling a fade grip along a clip's top edge.
    ///
    /// Only the clip and the end are kept: the fade's length is whatever the
    /// pointer says it is, measured from that end, so the gesture states an
    /// absolute destination and merges into one undo step like every other drag.
    FadeHandle { clip: ClipId, edge: FadeEdgeKind },
}

/// Which end of a clip a fade drag has hold of. Mirrors `ve_core::FadeEdge`
/// without making the state module depend on an enum it only forwards.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FadeEdgeKind {
    In,
    Out,
}

/// Which edge a trim drag has hold of. Mirrors `ve_command::TrimEdge` without
/// making the state module depend on the command crate.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TrimEdgeKind {
    Start,
    End,
}

/// A message shown in the status bar.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StatusLevel {
    Info,
    Warning,
    Error,
}

#[derive(Debug, Clone)]
pub struct Status {
    pub text: String,
    pub level: StatusLevel,
}

impl Status {
    pub fn info(text: impl Into<String>) -> Self {
        Status { text: text.into(), level: StatusLevel::Info }
    }
    pub fn warning(text: impl Into<String>) -> Self {
        Status { text: text.into(), level: StatusLevel::Warning }
    }
    pub fn error(text: impl Into<String>) -> Self {
        Status { text: text.into(), level: StatusLevel::Error }
    }
}

/// Everything the editor knows, apart from the GPU and the transport.
///
/// Deliberately free of any egui type, so the whole action layer can be driven
/// and asserted on without a window.
pub struct EditorState {
    pub project: Project,
    pub history: History,
    pub path: Option<PathBuf>,
    pub autosave: Autosave,
    pub selection: Selection,
    /// Which keyframes are in hand, and the editor that shows them.
    pub keyframes: KeyframeSelection,
    pub animation: AnimationView,
    pub clipboard: Clipboard,
    pub keyframe_clipboard: KeyframeClipboard,
    pub timeline: TimelineView,
    pub tool: TimelineTool,
    pub status: Option<Status>,
    /// The drag gesture in progress, if any.
    pub drag: TimelineDrag,
    /// Non-fatal problems from the last open, shown until dismissed.
    pub warnings: Vec<String>,
    pub show_performance_overlay: bool,
    /// The composition the user has open, if any. While one is open the
    /// timeline, the preview and the transport are all about *it* rather than
    /// about the sequence — which is what "open a composition" has to mean for
    /// it to be editable at all.
    pub open_composition: Option<CompositionId>,
    /// Bumped whenever the project changes.
    ///
    /// The audio mixer runs on its own thread and cannot borrow the project, so
    /// it works from a snapshot; this is how the frame loop knows a new one is
    /// due without comparing two whole projects.
    revision: u64,
}

impl EditorState {
    pub fn new(project: Project, scratch_dir: PathBuf) -> Self {
        let interval =
            Duration::from_secs(project.settings.autosave_interval_seconds.max(5) as u64);
        EditorState {
            project,
            history: History::default(),
            path: None,
            autosave: Autosave::new(scratch_dir, interval),
            selection: Selection::default(),
            keyframes: KeyframeSelection::default(),
            animation: AnimationView::default(),
            clipboard: Clipboard::default(),
            keyframe_clipboard: KeyframeClipboard::default(),
            timeline: TimelineView::default(),
            tool: TimelineTool::default(),
            status: None,
            drag: TimelineDrag::None,
            warnings: Vec::new(),
            show_performance_overlay: cfg!(debug_assertions),
            open_composition: None,
            revision: 0,
        }
    }

    /// How many times the project has changed since the editor started.
    pub fn revision(&self) -> u64 {
        self.revision
    }

    /// Notes that the project is no longer what anything else may be holding.
    ///
    /// Separate from [`EditorState::mark_edited`] because opening or creating a
    /// project replaces it wholesale without that being a *dirty* edit.
    pub fn project_replaced(&mut self) {
        self.revision += 1;
    }

    /// What the transport and the preview are pointed at.
    ///
    /// Derived rather than stored, so a composition that is deleted — or undone
    /// out of existence — falls back to the sequence instead of leaving the
    /// editor pointed at nothing.
    pub fn viewing(&self) -> Option<Viewing> {
        match self.open_composition {
            Some(id) if self.project.composition(id).is_some() => {
                Some(Viewing::Composition(id))
            }
            _ => self.active_sequence_id().map(Viewing::Sequence),
        }
    }

    /// The frame grid, length and canvas of whatever is being viewed.
    pub fn timebase(&self) -> Option<Timebase> {
        self.viewing().and_then(|v| v.timebase(&self.project))
    }

    /// The composition the user has open, if it still exists.
    pub fn open_composition(&self) -> Option<&ve_core::Composition> {
        self.open_composition.and_then(|id| self.project.composition(id))
    }

    /// Opens a composition for editing, or returns to the sequence with `None`.
    pub fn open(&mut self, composition: Option<CompositionId>) {
        self.open_composition = composition;
        self.selection.clear();
        self.keyframes.clear();
        self.animation.drag = None;
        self.drag = TimelineDrag::None;
    }

    pub fn active_sequence_id(&self) -> Option<SequenceId> {
        self.project.active_sequence
    }

    pub fn active_sequence(&self) -> Option<&ve_core::Sequence> {
        self.project.active()
    }

    /// Title-bar text, with the conventional dot for unsaved changes.
    pub fn window_title(&self) -> String {
        let name = self
            .path
            .as_ref()
            .and_then(|p| p.file_stem())
            .map(|s| s.to_string_lossy().into_owned())
            .unwrap_or_else(|| self.project.name.clone());
        if self.history.is_dirty() {
            format!("• {name} — Verge")
        } else {
            format!("{name} — Verge")
        }
    }

    pub fn set_status(&mut self, status: Status) {
        if status.level != StatusLevel::Info {
            log::warn!("{}", status.text);
        }
        self.status = Some(status);
    }

    /// Marks the project changed. Called after every successful command, which
    /// is the one place autosave and the title bar need to learn about edits.
    pub fn mark_edited(&mut self) {
        self.autosave.mark_dirty();
        self.revision += 1;
    }
}
