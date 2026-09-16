//! The timeline.
//!
//! Drawn with a painter rather than assembled from widgets. A sequence can hold
//! thousands of clips, and a retained widget per clip would cost layout and
//! allocation on every frame for objects that are mostly off screen. Painting
//! means one culled pass: only clips intersecting the visible time range are
//! touched at all, so drawing cost tracks what is on screen rather than what is
//! in the project.

use egui::{Align2, Color32, CornerRadius, FontId, Pos2, Rect, Sense, Stroke, Ui};
use ve_command::TrimEdge;
use ve_core::{Sequence, TrackKind};
use ve_time::{Rate, Ticks, TimeRange};

use crate::actions::Action;
use crate::state::{EditorState, TimelineDrag, TrimEdgeKind};
use crate::theme;

/// How close to a clip edge the pointer must be to start a trim instead of a move.
const TRIM_HANDLE_PX: f32 = 6.0;
/// How near a snap target has to be, in pixels, for snapping to take hold.
const SNAP_RADIUS_PX: f32 = 8.0;

pub fn show(ui: &mut Ui, state: &mut EditorState, playhead: Ticks, actions: &mut Vec<Action>) {
    let Some(sequence) = state.active_sequence().cloned() else { return };

    toolbar(ui, state, actions);

    let full = ui.available_rect_before_wrap();
    if full.width() <= state.timeline.header_width || full.height() <= 0.0 {
        return;
    }

    let header_width = state.timeline.header_width;
    let ruler_height = state.timeline.ruler_height;
    let lanes_left = full.left() + header_width;
    let lane_area =
        Rect::from_min_max(Pos2::new(lanes_left, full.top() + ruler_height), full.max);
    let ruler_rect = Rect::from_min_max(
        Pos2::new(lanes_left, full.top()),
        Pos2::new(full.right(), full.top() + ruler_height),
    );

    let response = ui.allocate_rect(full, Sense::click_and_drag());
    let painter = ui.painter_at(full);
    painter.rect_filled(full, 0.0, theme::TRACK_LANE);

    handle_scroll_and_zoom(ui, state, &response, lanes_left, lane_area);

    draw_ruler(&painter, ruler_rect, state, &sequence, lanes_left);
    let (lanes, content_height) =
        draw_tracks(&painter, lane_area, state, &sequence, header_width, full.left());
    state.timeline.clamp_scroll_y(content_height, lane_area.height());
    draw_vertical_scrollbar(&painter, lane_area, state, content_height);
    draw_markers(&painter, lane_area, state, &sequence, lanes_left);
    draw_playhead(&painter, full, ruler_rect, state, playhead, lanes_left);

    handle_pointer(ui, state, &sequence, &response, &lanes, ruler_rect, lanes_left, actions);
}

fn toolbar(ui: &mut Ui, state: &mut EditorState, actions: &mut Vec<Action>) {
    ui.horizontal(|ui| {
        ui.label(egui::RichText::new("TIMELINE").small().color(theme::TEXT_FAINT));
        ui.add_space(8.0);

        if ui.small_button("−").on_hover_text("Zoom out (-)").clicked() {
            actions.push(Action::ZoomOut);
        }
        if ui.small_button("+").on_hover_text("Zoom in (+)").clicked() {
            actions.push(Action::ZoomIn);
        }
        let width = ui.available_width();
        if ui.small_button("Fit").on_hover_text("Zoom to fit (Shift+Z)").clicked() {
            actions.push(Action::ZoomToFit(width));
        }

        let mut snapping = state.timeline.snapping;
        if ui
            .toggle_value(&mut snapping, "Snap")
            .on_hover_text("Snap to clip edges and markers (S)")
            .clicked()
        {
            actions.push(Action::ToggleSnapping);
        }

        ui.add_space(8.0);
        if ui.small_button("Split").on_hover_text("Split at playhead (Ctrl+K)").clicked() {
            actions.push(Action::SplitAtPlayhead);
        }
        if ui.small_button("Delete").on_hover_text("Delete selected (Del)").clicked() {
            actions.push(Action::DeleteSelected);
        }

        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            ui.label(
                egui::RichText::new(format!("{:.0} px/s", state.timeline.pixels_per_second))
                    .small()
                    .color(theme::TEXT_FAINT),
            );
        });
    });
}

/// Where each track's lane ended up, so pointer handling can map Y to a track.
struct Lane {
    track: ve_core::TrackId,
    rect: Rect,
    locked: bool,
}

fn draw_ruler(
    painter: &egui::Painter,
    rect: Rect,
    state: &EditorState,
    sequence: &Sequence,
    lanes_left: f32,
) {
    painter.rect_filled(rect, 0.0, theme::RULER_BACKGROUND);
    painter.line_segment(
        [rect.left_bottom(), rect.right_bottom()],
        Stroke::new(1.0, theme::SEPARATOR),
    );

    let view = &state.timeline;
    let step = tick_step(view.pixels_per_second, sequence.rate());
    let first = Ticks::new((view.scroll.raw() / step.raw()) * step.raw());

    let mut at = first;
    while view.x_of(at) < rect.width() + step.as_secs_f32() * view.pixels_per_second {
        let x = lanes_left + view.x_of(at);
        if x >= rect.left() - 60.0 && x <= rect.right() {
            painter.line_segment(
                [Pos2::new(x, rect.bottom() - 6.0), Pos2::new(x, rect.bottom())],
                Stroke::new(1.0, theme::TEXT_FAINT),
            );
            painter.text(
                Pos2::new(x + 3.0, rect.top() + 2.0),
                Align2::LEFT_TOP,
                sequence.timecode_at(at).to_string(),
                FontId::monospace(9.5),
                theme::TEXT_DIM,
            );
        }
        at += step;
        if at.raw() <= first.raw() {
            break; // guards against a degenerate zero step
        }
    }
}

/// Picks a ruler interval that keeps labels readable at the current zoom.
///
/// Walks a ladder of musically sensible durations rather than computing one, so
/// the labels land on round numbers a person can read at a glance.
fn tick_step(pixels_per_second: f32, rate: Rate) -> Ticks {
    const MIN_LABEL_SPACING_PX: f32 = 70.0;
    let seconds_per_label = (MIN_LABEL_SPACING_PX / pixels_per_second.max(0.001)) as f64;

    const LADDER: &[f64] = &[
        1.0 / 60.0,
        0.1,
        0.25,
        0.5,
        1.0,
        2.0,
        5.0,
        10.0,
        15.0,
        30.0,
        60.0,
        120.0,
        300.0,
        600.0,
        900.0,
        1800.0,
        3600.0,
    ];
    for &candidate in LADDER {
        if candidate >= seconds_per_label {
            // Below a second, fall back to whole frames so the ruler never
            // labels a position that is not on the frame grid.
            if candidate < 1.0 {
                let frames = (candidate * rate.as_f64()).round().max(1.0) as i64;
                return rate.frame_to_ticks(frames);
            }
            return Ticks::from_secs_f64(candidate);
        }
    }
    Ticks::from_secs_f64(*LADDER.last().unwrap())
}

fn draw_tracks(
    painter: &egui::Painter,
    area: Rect,
    state: &EditorState,
    sequence: &Sequence,
    header_width: f32,
    left: f32,
) -> (Vec<Lane>, f32) {
    let view = &state.timeline;
    let lanes_left = left + header_width;
    let visible =
        TimeRange::from_bounds(view.scroll, view.time_at(area.width()) + Ticks::from_millis(1));

    let mut lanes = Vec::new();
    // Tracks are laid out from a virtual origin above the viewport, so the
    // stack can be scrolled without changing any of the geometry below.
    let mut y = area.top() - view.scroll_y;
    let mut content_height = 0.0f32;

    // Video tracks are drawn top-down in reverse, so V2 sits above V1 and the
    // picture's layer order reads the way it composites.
    let ordered: Vec<&ve_core::Track> = sequence
        .tracks
        .iter()
        .filter(|t| t.kind == TrackKind::Video)
        .rev()
        .chain(sequence.tracks.iter().filter(|t| t.kind == TrackKind::Audio))
        .collect();

    for (index, track) in ordered.iter().enumerate() {
        let height = track.height.clamp(28.0, 300.0);
        content_height += height;

        // Skip rows scrolled entirely out of view rather than drawing and
        // clipping them: the cost of a big track stack should follow what is
        // visible, exactly as clip culling does horizontally.
        if y + height < area.top() || y > area.bottom() {
            y += height;
            continue;
        }

        let top = y.max(area.top());
        let bottom = (y + height).min(area.bottom());
        let lane_rect =
            Rect::from_min_max(Pos2::new(lanes_left, top), Pos2::new(area.right(), bottom));
        let header_rect =
            Rect::from_min_max(Pos2::new(left, top), Pos2::new(lanes_left, bottom));

        let lane_fill = if index % 2 == 0 { theme::TRACK_LANE } else { theme::TRACK_LANE_ALT };
        painter.rect_filled(lane_rect, 0.0, lane_fill);
        painter.rect_filled(header_rect, 0.0, theme::TRACK_HEADER);
        painter.line_segment(
            [lane_rect.left_bottom(), lane_rect.right_bottom()],
            Stroke::new(1.0, theme::SEPARATOR),
        );

        draw_track_header(painter, header_rect, track);
        draw_grid(painter, lane_rect, state, sequence, lanes_left);

        // The culling that makes a big timeline cheap: only clips actually
        // overlapping the visible range are drawn.
        for clip in track.clips_in_range(visible) {
            draw_clip(painter, lane_rect, state, sequence, clip, track.kind, lanes_left);
        }

        lanes.push(Lane { track: track.id, rect: lane_rect, locked: track.locked });
        y += height;
    }
    (lanes, content_height)
}

/// A slim indicator of how far down the track stack is scrolled.
fn draw_vertical_scrollbar(
    painter: &egui::Painter,
    area: Rect,
    state: &EditorState,
    content_height: f32,
) {
    if content_height <= area.height() + 0.5 {
        return;
    }
    let track_rect = Rect::from_min_max(
        Pos2::new(area.right() - 4.0, area.top()),
        Pos2::new(area.right(), area.bottom()),
    );
    let visible_fraction = (area.height() / content_height).clamp(0.05, 1.0);
    let offset_fraction = state.timeline.scroll_y / content_height;
    let thumb = Rect::from_min_max(
        Pos2::new(track_rect.left(), area.top() + area.height() * offset_fraction),
        Pos2::new(
            track_rect.right(),
            area.top() + area.height() * (offset_fraction + visible_fraction),
        ),
    );
    painter.rect_filled(track_rect, CornerRadius::same(2), theme::SUNKEN);
    painter.rect_filled(thumb, CornerRadius::same(2), theme::SEPARATOR);
}

fn draw_track_header(painter: &egui::Painter, rect: Rect, track: &ve_core::Track) {
    let accent = match track.kind {
        TrackKind::Video => theme::CLIP_VIDEO_TOP,
        TrackKind::Audio => theme::CLIP_AUDIO_TOP,
    };
    painter.rect_filled(
        Rect::from_min_max(rect.left_top(), Pos2::new(rect.left() + 3.0, rect.bottom())),
        0.0,
        accent,
    );
    painter.text(
        Pos2::new(rect.left() + 10.0, rect.top() + 5.0),
        Align2::LEFT_TOP,
        &track.name,
        FontId::proportional(11.5),
        if track.muted { theme::TEXT_FAINT } else { theme::TEXT },
    );

    let mut flags = Vec::new();
    if track.muted {
        flags.push("M");
    }
    if track.solo {
        flags.push("S");
    }
    if track.locked {
        flags.push("L");
    }
    if !flags.is_empty() {
        painter.text(
            Pos2::new(rect.right() - 8.0, rect.top() + 5.0),
            Align2::RIGHT_TOP,
            flags.join(" "),
            FontId::monospace(10.0),
            theme::WARNING,
        );
    }
    painter.text(
        Pos2::new(rect.left() + 10.0, rect.bottom() - 5.0),
        Align2::LEFT_BOTTOM,
        format!("{} clip{}", track.len(), if track.len() == 1 { "" } else { "s" }),
        FontId::proportional(9.5),
        theme::TEXT_FAINT,
    );
}

fn draw_grid(
    painter: &egui::Painter,
    lane: Rect,
    state: &EditorState,
    sequence: &Sequence,
    lanes_left: f32,
) {
    let view = &state.timeline;
    let step = tick_step(view.pixels_per_second, sequence.rate());
    let first = Ticks::new((view.scroll.raw() / step.raw()) * step.raw());
    let mut at = first;
    let mut guard = 0;
    while guard < 512 {
        let x = lanes_left + view.x_of(at);
        if x > lane.right() {
            break;
        }
        if x >= lane.left() {
            painter.line_segment(
                [Pos2::new(x, lane.top()), Pos2::new(x, lane.bottom())],
                Stroke::new(1.0, theme::GRID),
            );
        }
        at += step;
        guard += 1;
    }
}

fn draw_clip(
    painter: &egui::Painter,
    lane: Rect,
    state: &EditorState,
    sequence: &Sequence,
    clip: &ve_core::Clip,
    kind: TrackKind,
    lanes_left: f32,
) {
    let view = &state.timeline;
    let x0 = lanes_left + view.x_of(clip.timeline_start);
    let x1 = lanes_left + view.x_of(clip.timeline_end());
    // Clamp to the lane so a clip stretching far off screen still costs one
    // rectangle rather than an enormous one.
    let left = x0.max(lane.left());
    let right = x1.min(lane.right());
    if right <= left {
        return;
    }

    let rect = Rect::from_min_max(
        Pos2::new(left, lane.top() + 2.0),
        Pos2::new(right, lane.bottom() - 2.0),
    );
    let selected = state.selection.is_selected(clip.id);
    let (body, top) = theme::clip_colour(kind, selected, clip.enabled);

    painter.rect_filled(rect, CornerRadius::same(3), body);
    // A brighter cap, so clip boundaries stay legible when clips abut.
    painter.rect_filled(
        Rect::from_min_max(rect.left_top(), Pos2::new(rect.right(), rect.top() + 3.0)),
        CornerRadius::same(3),
        top,
    );
    painter.rect_stroke(
        rect,
        CornerRadius::same(3),
        Stroke::new(
            if selected { 1.5 } else { 1.0 },
            if selected { theme::CLIP_SELECTED } else { theme::SEPARATOR },
        ),
        egui::StrokeKind::Inside,
    );

    let offline = state.project.asset(clip.asset).map(|a| a.offline).unwrap_or(true);
    if offline {
        painter.rect_filled(rect, CornerRadius::same(3), theme::OFFLINE.gamma_multiply(0.45));
    }

    // Only label a clip wide enough to read, so a dense timeline does not turn
    // into overlapping text.
    if rect.width() > 34.0 {
        let label = if offline { format!("⚠ {}", clip.name) } else { clip.name.clone() };
        painter.text(
            Pos2::new(rect.left() + 5.0, rect.top() + 6.0),
            Align2::LEFT_TOP,
            elide(&label, rect.width() - 10.0),
            FontId::proportional(10.5),
            if clip.enabled { theme::TEXT } else { theme::TEXT_FAINT },
        );
    }
    if rect.width() > 90.0 && rect.height() > 34.0 {
        painter.text(
            Pos2::new(rect.left() + 5.0, rect.bottom() - 5.0),
            Align2::LEFT_BOTTOM,
            sequence.timecode_at(clip.timeline_start).to_string(),
            FontId::monospace(9.0),
            theme::TEXT_FAINT,
        );
    }
    // A marker for animation, so a clip that will change over time says so.
    if clip.is_animated() && rect.width() > 20.0 {
        painter.text(
            Pos2::new(rect.right() - 5.0, rect.top() + 6.0),
            Align2::RIGHT_TOP,
            "◆",
            FontId::proportional(9.0),
            theme::ACCENT,
        );
    }
}

/// Truncates a label to fit, with an ellipsis.
fn elide(text: &str, width: f32) -> String {
    // Roughly 5.6 px per character at 10.5 pt proportional; close enough to
    // avoid measuring every label every frame.
    let max_chars = (width / 5.6).floor().max(1.0) as usize;
    if text.chars().count() <= max_chars {
        return text.to_string();
    }
    let keep = max_chars.saturating_sub(1);
    text.chars().take(keep).collect::<String>() + "…"
}

fn draw_markers(
    painter: &egui::Painter,
    area: Rect,
    state: &EditorState,
    sequence: &Sequence,
    lanes_left: f32,
) {
    for marker in &sequence.markers {
        let x = lanes_left + state.timeline.x_of(marker.time);
        if x < area.left() || x > area.right() {
            continue;
        }
        let colour = Color32::from_rgb(
            (marker.color.r * 255.0) as u8,
            (marker.color.g * 255.0) as u8,
            (marker.color.b * 255.0) as u8,
        );
        painter.line_segment(
            [Pos2::new(x, area.top()), Pos2::new(x, area.bottom())],
            Stroke::new(1.0, colour.gamma_multiply(0.6)),
        );
    }
}

fn draw_playhead(
    painter: &egui::Painter,
    full: Rect,
    ruler: Rect,
    state: &EditorState,
    playhead: Ticks,
    lanes_left: f32,
) {
    let x = lanes_left + state.timeline.x_of(playhead);
    if x < lanes_left || x > full.right() {
        return;
    }
    painter.line_segment(
        [Pos2::new(x, ruler.top()), Pos2::new(x, full.bottom())],
        Stroke::new(1.0, theme::PLAYHEAD),
    );
    // A handle in the ruler, wide enough to grab.
    painter.add(egui::Shape::convex_polygon(
        vec![
            Pos2::new(x - 5.0, ruler.top()),
            Pos2::new(x + 5.0, ruler.top()),
            Pos2::new(x, ruler.top() + 8.0),
        ],
        theme::PLAYHEAD,
        Stroke::NONE,
    ));
}

#[allow(clippy::too_many_arguments)]
fn handle_scroll_and_zoom(
    ui: &Ui,
    state: &mut EditorState,
    response: &egui::Response,
    lanes_left: f32,
    lane_area: Rect,
) {
    if !response.hovered() {
        return;
    }
    let scroll = ui.input(|i| i.smooth_scroll_delta);
    let zoom_modifier = ui.input(|i| i.modifiers.ctrl || i.modifiers.command);

    let horizontal_modifier = ui.input(|i| i.modifiers.shift);

    if zoom_modifier && scroll.y != 0.0 {
        // Zoom about the pointer, so the frame under the cursor stays put —
        // the behaviour that makes zooming feel like moving a lens rather than
        // jumping somewhere else.
        let pointer_x = ui
            .input(|i| i.pointer.hover_pos())
            .map(|p| p.x - lanes_left)
            .unwrap_or(lane_area.width() / 2.0);
        let anchor_time = state.timeline.time_at(pointer_x);
        let factor = if scroll.y > 0.0 { 1.1 } else { 0.9 };
        state.timeline.zoom_about(factor, anchor_time, pointer_x);
    } else if scroll.x != 0.0 || (horizontal_modifier && scroll.y != 0.0) {
        // Shift, or a genuinely horizontal wheel, moves along the timeline.
        let delta = if scroll.x != 0.0 { scroll.x } else { scroll.y };
        let seconds = (-delta / state.timeline.pixels_per_second) as f64;
        state.timeline.scroll =
            (state.timeline.scroll + Ticks::from_secs_f64(seconds)).clamp_non_negative();
    } else if scroll.y != 0.0 {
        // A plain wheel moves through the track stack, matching the
        // convention every established editor uses.
        state.timeline.scroll_y -= scroll.y;
    }
}

#[allow(clippy::too_many_arguments)]
fn handle_pointer(
    ui: &Ui,
    state: &mut EditorState,
    sequence: &Sequence,
    response: &egui::Response,
    lanes: &[Lane],
    ruler: Rect,
    lanes_left: f32,
    actions: &mut Vec<Action>,
) {
    let pointer = response.interact_pointer_pos().or(response.hover_pos());
    let Some(pointer) = pointer else {
        if response.drag_stopped() {
            finish_drag(state, actions);
        }
        return;
    };
    let time_at_pointer = state.timeline.time_at(pointer.x - lanes_left);

    // Starting a gesture.
    if response.drag_started() || response.clicked() {
        if pointer.y <= ruler.bottom() {
            state.drag = TimelineDrag::Playhead;
        } else if let Some(lane) = lanes.iter().find(|l| l.rect.contains(pointer)) {
            match hit_test_clip(state, sequence, lane, pointer, lanes_left) {
                Some((clip, edge)) => {
                    let additive = ui.input(|i| i.modifiers.shift || i.modifiers.command);
                    actions.push(Action::SelectClip { clip, track: lane.track, additive });
                    if lane.locked {
                        state.drag = TimelineDrag::None;
                    } else {
                        state.drag = match edge {
                            Some(edge) => {
                                TimelineDrag::TrimClip { clip, track: lane.track, edge }
                            }
                            None => {
                                let start = sequence
                                    .find_clip(clip)
                                    .map(|(_, c)| c.timeline_start)
                                    .unwrap_or(Ticks::ZERO);
                                TimelineDrag::MoveClip {
                                    clip,
                                    track: lane.track,
                                    grab_offset: time_at_pointer - start,
                                }
                            }
                        };
                    }
                }
                // Clicking empty track space deselects and moves the playhead,
                // which is what a click in a timeline background should do.
                None => {
                    actions.push(Action::ClearSelection);
                    state.selection.track = Some(lane.track);
                    state.drag = TimelineDrag::Playhead;
                }
            }
        }
    }

    // Continuing one.
    if response.dragged() || (response.clicked() && state.drag == TimelineDrag::Playhead) {
        match state.drag {
            TimelineDrag::Playhead => {
                let to = snap(state, sequence, time_at_pointer, None);
                actions.push(Action::ScrubTo(to));
            }
            TimelineDrag::MoveClip { clip, track, grab_offset } => {
                let raw = (time_at_pointer - grab_offset).clamp_non_negative();
                let to = sequence.snap_to_frame(snap(state, sequence, raw, Some(clip)));
                actions.push(Action::MoveClipTo { clip, track, to, coalesce: true });
            }
            TimelineDrag::TrimClip { clip, track, edge } => {
                let to =
                    sequence.snap_to_frame(snap(state, sequence, time_at_pointer, Some(clip)));
                actions.push(Action::TrimClipTo {
                    clip,
                    track,
                    edge: match edge {
                        TrimEdgeKind::Start => TrimEdge::Start,
                        TrimEdgeKind::End => TrimEdge::End,
                    },
                    to,
                    coalesce: true,
                });
            }
            TimelineDrag::None => {}
        }
    }

    if response.drag_stopped() {
        finish_drag(state, actions);
    }

    // A cursor that tells the user which gesture a press would begin.
    if !response.dragged() && pointer.y > ruler.bottom() {
        if let Some(lane) = lanes.iter().find(|l| l.rect.contains(pointer)) {
            if let Some((_, edge)) = hit_test_clip(state, sequence, lane, pointer, lanes_left) {
                ui.ctx().set_cursor_icon(if edge.is_some() {
                    egui::CursorIcon::ResizeHorizontal
                } else {
                    egui::CursorIcon::Grab
                });
            }
        }
    }
}

fn finish_drag(state: &mut EditorState, actions: &mut Vec<Action>) {
    if state.drag != TimelineDrag::None {
        // Closes the coalescing run, so the next gesture is its own undo step.
        actions.push(Action::EndGesture);
    }
    state.drag = TimelineDrag::None;
}

/// What is under the pointer: a clip, and whether the pointer is on an edge.
fn hit_test_clip(
    state: &EditorState,
    sequence: &Sequence,
    lane: &Lane,
    pointer: Pos2,
    lanes_left: f32,
) -> Option<(ve_core::ClipId, Option<TrimEdgeKind>)> {
    let track = sequence.track(lane.track)?;
    let at = state.timeline.time_at(pointer.x - lanes_left);
    let clip = track.clip_at(at)?;

    let x0 = lanes_left + state.timeline.x_of(clip.timeline_start);
    let x1 = lanes_left + state.timeline.x_of(clip.timeline_end());
    // Only offer trim handles when the clip is wide enough that a handle does
    // not swallow the whole clip and make it impossible to move.
    let handles_fit = (x1 - x0) > TRIM_HANDLE_PX * 3.0;

    let edge = if handles_fit && (pointer.x - x0).abs() <= TRIM_HANDLE_PX {
        Some(TrimEdgeKind::Start)
    } else if handles_fit && (x1 - pointer.x).abs() <= TRIM_HANDLE_PX {
        Some(TrimEdgeKind::End)
    } else {
        None
    };
    Some((clip.id, edge))
}

/// Applies edge snapping, if it is on.
///
/// The tolerance is in *pixels* rather than in time, so snapping feels the same
/// at every zoom level instead of becoming unusable when zoomed in.
fn snap(
    state: &EditorState,
    sequence: &Sequence,
    at: Ticks,
    exclude: Option<ve_core::ClipId>,
) -> Ticks {
    if !state.timeline.snapping {
        return at.clamp_non_negative();
    }
    let tolerance =
        Ticks::from_secs_f64((SNAP_RADIUS_PX / state.timeline.pixels_per_second) as f64);
    sequence.snap_candidate(at, tolerance, exclude).unwrap_or(at).clamp_non_negative()
}
