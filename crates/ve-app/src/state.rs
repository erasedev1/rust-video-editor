//! Editor state that is not the project itself.

use std::path::PathBuf;
use std::time::Duration;

use ve_command::History;
use ve_core::{AssetId, Clip, ClipId, Project, SequenceId, TrackId};
use ve_project::Autosave;
use ve_time::Ticks;

/// What the user currently has selected.
#[derive(Debug, Default, Clone)]
pub struct Selection {
    pub clips: Vec<ClipId>,
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
    pub clipboard: Clipboard,
    pub timeline: TimelineView,
    pub tool: TimelineTool,
    pub status: Option<Status>,
    /// The drag gesture in progress, if any.
    pub drag: TimelineDrag,
    /// Non-fatal problems from the last open, shown until dismissed.
    pub warnings: Vec<String>,
    pub show_performance_overlay: bool,
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
            clipboard: Clipboard::default(),
            timeline: TimelineView::default(),
            tool: TimelineTool::default(),
            status: None,
            drag: TimelineDrag::None,
            warnings: Vec::new(),
            show_performance_overlay: cfg!(debug_assertions),
        }
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
    }
}
