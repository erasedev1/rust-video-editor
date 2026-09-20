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
use ve_core::{FadeEdge, Sequence, TrackKind};
use ve_engine::AudioLevels;
use ve_media::WaveformService;
use ve_time::{Rate, Ticks, TimeRange};

use crate::actions::Action;
use crate::panels::waveform;
use crate::state::{EditorState, FadeEdgeKind, TimelineDrag, TimelineTool, TrimEdgeKind};
use crate::theme;

/// How close to a clip edge the pointer must be to start a trim instead of a move.
const TRIM_HANDLE_PX: f32 = 6.0;
/// How near a snap target has to be, in pixels, for snapping to take hold.
const SNAP_RADIUS_PX: f32 = 8.0;
/// Side of a mute/solo/lock button in a track header.
const SWITCH_PX: f32 = 15.0;
/// Height of the level meter strip in an audio track's header.
const METER_PX: f32 = 5.0;
/// How near a fade grip the pointer must be to take hold of it.
const FADE_GRIP_PX: f32 = 7.0;
/// How far down a clip the fade grips reach. Shallow, so the rest of the clip's
/// top edge still starts a move and the corners still start a trim.
const FADE_BAND_PX: f32 = 11.0;

#[allow(clippy::too_many_arguments)]
pub fn show(
    ui: &mut Ui,
    state: &mut EditorState,
    waveforms: &WaveformService,
    playhead: Ticks,
    levels: &AudioLevels,
    actions: &mut Vec<Action>,
) {
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
    let hover = response.hover_pos();
    let (lanes, content_height) = draw_tracks(
        &painter,
        lane_area,
        state,
        &sequence,
        waveforms,
        levels,
        header_width,
        full.left(),
        hover,
    );
    state.timeline.clamp_scroll_y(content_height, lane_area.height());
    draw_vertical_scrollbar(&painter, lane_area, state, content_height);
    draw_markers(&painter, lane_area, ruler_rect, state, &sequence, lanes_left);
    draw_playhead(&painter, full, ruler_rect, state, playhead, lanes_left);
    draw_marquee(&painter, lane_area, state, &response, lanes_left);

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

        ui.add_space(8.0);
        for tool in TimelineTool::ALL {
            let (key, what) = tool.hint();
            if ui
                .selectable_label(state.tool == tool, tool.label())
                .on_hover_text(format!("{what} ({key})"))
                .clicked()
            {
                actions.push(Action::SetTool(tool));
            }
        }
        ui.add_space(8.0);

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
        if ui
            .small_button("Ripple")
            .on_hover_text("Delete selected and close the gap (Shift+Del)")
            .clicked()
        {
            actions.push(Action::RippleDeleteSelected);
        }
        if ui
            .add_enabled(state.selection.clips.len() == 2, egui::Button::new("Crossfade"))
            .on_hover_text("Fade two overlapping clips into each other (Ctrl+Shift+F)")
            .on_disabled_hover_text("Select two clips that overlap in time")
            .clicked()
        {
            actions.push(Action::CrossfadeSelection(ve_core::FadeCurve::EqualPower));
        }

        ui.add_space(8.0);
        if ui.small_button("♦").on_hover_text("Add a marker at the playhead (M)").clicked() {
            actions.push(Action::AddMarkerAtPlayhead);
        }
        if ui.small_button("+V").on_hover_text("Add a video track").clicked() {
            actions.push(Action::AddTrack(TrackKind::Video));
        }
        if ui.small_button("+A").on_hover_text("Add an audio track").clicked() {
            actions.push(Action::AddTrack(TrackKind::Audio));
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
    /// The mute, solo and lock buttons, in that order.
    ///
    /// The header is painted rather than built from widgets, like the rest of
    /// the timeline, so the rectangles the buttons occupy are handed back here
    /// for the pointer pass to hit-test. Only visible rows produce a lane, so
    /// this costs nothing for a track stack that is scrolled off screen.
    switches: [Rect; 3],
    locked: bool,
}

/// The three header switches, in the order [`Lane::switches`] holds them.
const SWITCHES: [(&str, ve_command::TrackFlag); 3] = [
    ("M", ve_command::TrackFlag::Muted),
    ("S", ve_command::TrackFlag::Solo),
    ("L", ve_command::TrackFlag::Locked),
];

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

#[allow(clippy::too_many_arguments)]
fn draw_tracks(
    painter: &egui::Painter,
    area: Rect,
    state: &EditorState,
    sequence: &Sequence,
    waveforms: &WaveformService,
    levels: &AudioLevels,
    header_width: f32,
    left: f32,
    hover: Option<Pos2>,
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

        let switches =
            draw_track_header(painter, header_rect, track, levels.track(track.id), hover);
        draw_grid(painter, lane_rect, state, sequence, lanes_left);

        // The culling that makes a big timeline cheap: only clips actually
        // overlapping the visible range are drawn.
        for clip in track.clips_in_range(visible) {
            draw_clip(
                painter, lane_rect, state, sequence, waveforms, clip, track.kind, lanes_left,
            );
        }

        lanes.push(Lane { track: track.id, rect: lane_rect, switches, locked: track.locked });
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

/// Paints a track header, returning where its three switches ended up.
fn draw_track_header(
    painter: &egui::Painter,
    rect: Rect,
    track: &ve_core::Track,
    level: ve_engine::Meter,
    hover: Option<Pos2>,
) -> [Rect; 3] {
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
    // An audio track gives the bottom of its header to a meter, which is where
    // the eye goes while mixing. The strip is drawn whether or not anything is
    // playing: an empty meter in a fixed place reads as silence, whereas one
    // that appears and disappears reads as a glitch.
    let metered = track.kind == TrackKind::Audio && rect.height() >= 38.0;
    let mut footer = rect.bottom() - 5.0;
    if metered {
        let strip = Rect::from_min_max(
            Pos2::new(rect.left() + 6.0, rect.bottom() - METER_PX - 3.0),
            Pos2::new(rect.right() - 6.0, rect.bottom() - 3.0),
        );
        crate::meter::draw(painter, strip, level);
        footer -= METER_PX + 3.0;
    }

    let summary = if track.is_unity() {
        format!("{} clip{}", track.len(), if track.len() == 1 { "" } else { "s" })
    } else {
        // A track that is not passing its audio through untouched has to say
        // so: a mix set up last week and reopened today is otherwise a mystery.
        format!(
            "{} clip{}   {}  {}",
            track.len(),
            if track.len() == 1 { "" } else { "s" },
            crate::meter::gain_label(track.volume),
            crate::meter::pan_label(track.pan),
        )
    };
    painter.text(
        Pos2::new(rect.left() + 10.0, footer),
        Align2::LEFT_BOTTOM,
        summary,
        FontId::proportional(9.5),
        if track.is_unity() { theme::TEXT_FAINT } else { theme::TEXT_DIM },
    );

    let on = [track.muted, track.solo, track.locked];
    let mut switches = [Rect::NOTHING; 3];
    for (i, ((label, _), lit)) in SWITCHES.iter().zip(on).enumerate() {
        // Laid out from the right so the row reads M S L, and clamped inside
        // the header so a short track still gets usable targets.
        let right = rect.right() - 6.0 - (2 - i) as f32 * (SWITCH_PX + 3.0);
        let size = SWITCH_PX.min(rect.height() - 4.0);
        let centre = Pos2::new(right - size / 2.0, rect.center().y);
        let button = Rect::from_center_size(centre, egui::Vec2::splat(size));
        switches[i] = button;

        let hovered = hover.is_some_and(|p| button.contains(p));
        let fill = match (lit, hovered) {
            (true, _) => theme::WARNING.gamma_multiply(0.85),
            (false, true) => theme::TRACK_LANE_ALT,
            (false, false) => Color32::TRANSPARENT,
        };
        painter.rect_filled(button, CornerRadius::same(2), fill);
        painter.rect_stroke(
            button,
            CornerRadius::same(2),
            Stroke::new(1.0, theme::SEPARATOR),
            egui::StrokeKind::Inside,
        );
        painter.text(
            button.center(),
            Align2::CENTER_CENTER,
            label,
            FontId::monospace(9.5),
            if lit { theme::TRACK_HEADER } else { theme::TEXT_DIM },
        );
    }
    switches
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

#[allow(clippy::too_many_arguments)]
fn draw_clip(
    painter: &egui::Painter,
    lane: Rect,
    state: &EditorState,
    sequence: &Sequence,
    waveforms: &WaveformService,
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

    // A clip holding a composition is never offline: there is no file to
    // relink, and the composition either exists in the project or the loader
    // already warned about it.
    let offline = match clip.source {
        ve_core::Source::Asset(id) => {
            state.project.asset(id).map(|a| a.offline).unwrap_or(true)
        }
        ve_core::Source::Composition(id) => state.project.composition(id).is_none(),
    };
    if offline {
        painter.rect_filled(rect, CornerRadius::same(3), theme::OFFLINE.gamma_multiply(0.45));
    }

    // Labels are drawn after the waveform and would be unreadable over it, so
    // the waveform is given the body between them rather than the whole clip.
    let labelled = rect.width() > 34.0;
    let timecoded = rect.width() > 90.0 && rect.height() > 34.0;
    if kind == TrackKind::Audio && !offline && clip.enabled {
        if let ve_core::Source::Asset(id) = clip.source {
            if let Some(asset) = state.project.asset(id) {
                let body = Rect::from_min_max(
                    Pos2::new(
                        rect.left() + 1.0,
                        rect.top() + if labelled { 12.0 } else { 2.0 },
                    ),
                    Pos2::new(
                        rect.right() - 1.0,
                        rect.bottom() - if timecoded { 12.0 } else { 2.0 },
                    ),
                );
                let area = waveform_area(state, clip, body, x0..x1, left..right, lanes_left);
                waveform::draw(painter, &area, clip, asset, waveforms);
            }
        }
    }

    if kind == TrackKind::Audio {
        draw_fades(painter, rect, clip, x0..x1);
    }

    // Only label a clip wide enough to read, so a dense timeline does not turn
    // into overlapping text.
    if labelled {
        let label = if offline { format!("⚠ {}", clip.name) } else { clip.name.clone() };
        painter.text(
            Pos2::new(rect.left() + 5.0, rect.top() + 6.0),
            Align2::LEFT_TOP,
            elide(&label, rect.width() - 10.0),
            FontId::proportional(10.5),
            if clip.enabled { theme::TEXT } else { theme::TEXT_FAINT },
        );
    }
    if timecoded {
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
            "♦",
            FontId::proportional(9.0),
            theme::ACCENT,
        );
    }
    // And one for an effect chain, beside the animation marker: both answer the
    // same question — whether this clip is doing something to its picture that
    // a glance at the timeline would otherwise not show.
    if clip.effects.iter().any(|e| e.enabled) && rect.width() > 34.0 {
        let crowded = if clip.is_animated() { 12.0 } else { 0.0 };
        painter.text(
            Pos2::new(rect.right() - 5.0 - crowded, rect.top() + 6.0),
            Align2::RIGHT_TOP,
            "fx",
            FontId::proportional(9.0),
            theme::ACCENT,
        );
    }
    // And one for a blend mode, because a clip that composites unusually is
    // otherwise indistinguishable from its neighbours until it is selected.
    if !clip.blend.is_normal() && rect.width() > 46.0 {
        painter.text(
            Pos2::new(rect.right() - 5.0, rect.bottom() - 5.0),
            Align2::RIGHT_BOTTOM,
            clip.blend.label().to_lowercase(),
            FontId::proportional(9.0),
            theme::ACCENT,
        );
    }
}

/// Draws a clip's fades, and the grips that set them.
///
/// The envelope is drawn as a line falling to the clip's silent corner, which
/// is the shape every editor uses and the one the waveform underneath is
/// already scaled by — the two agree because both read the same evaluated gain.
///
/// A clip with no fade still gets its grips, sitting in its top corners. That is
/// what makes a fade discoverable: there is something to pull rather than a
/// menu to find.
fn draw_fades(
    painter: &egui::Painter,
    rect: Rect,
    clip: &ve_core::Clip,
    whole: std::ops::Range<f32>,
) {
    if rect.width() < 12.0 || rect.height() < FADE_BAND_PX + 4.0 {
        return;
    }
    let pixels_per_tick = (whole.end - whole.start) / clip.duration.raw().max(1) as f32;
    let (fade_in, fade_out) = clip.audio.fitted_fades(clip.duration);

    for (edge, fade) in [(FadeEdge::In, fade_in), (FadeEdge::Out, fade_out)] {
        let span = fade.length.raw() as f32 * pixels_per_tick;
        let (anchor, tip) = match edge {
            FadeEdge::In => (whole.start, whole.start + span),
            FadeEdge::Out => (whole.end, whole.end - span),
        };

        if fade.is_active() && span >= 2.0 {
            // The line from silence to full level, plus a wash under it so the
            // fade reads at a glance rather than only on inspection.
            painter.line_segment(
                [
                    Pos2::new(anchor.clamp(rect.left(), rect.right()), rect.bottom()),
                    Pos2::new(tip.clamp(rect.left(), rect.right()), rect.top()),
                ],
                Stroke::new(1.5, theme::PLAYHEAD),
            );
            let wedge = Rect::from_min_max(
                Pos2::new(anchor.min(tip).max(rect.left()), rect.top()),
                Pos2::new(anchor.max(tip).min(rect.right()), rect.bottom()),
            );
            if wedge.width() > 0.0 {
                painter.rect_filled(
                    wedge,
                    CornerRadius::ZERO,
                    theme::PLAYHEAD.gamma_multiply(0.12),
                );
            }
        }

        let grip = fade_grip(rect, tip);
        if rect.x_range().contains(grip.center().x) {
            painter.rect_filled(
                grip,
                CornerRadius::same(2),
                if fade.is_active() {
                    theme::PLAYHEAD
                } else {
                    theme::TEXT_FAINT.gamma_multiply(0.7)
                },
            );
        }
    }
}

/// Where a fade grip sits, given where the fade reaches full level.
fn fade_grip(rect: Rect, tip: f32) -> Rect {
    Rect::from_center_size(
        Pos2::new(tip, rect.top() + FADE_GRIP_PX * 0.5),
        egui::Vec2::splat(FADE_GRIP_PX),
    )
}

/// The fade grip under the pointer, if any.
///
/// Checked before the trim handles, because the grips sit in the clip's top
/// corners where a trim would otherwise start. They are confined to a shallow
/// band so the rest of the edge still trims.
fn hit_test_fade(
    state: &EditorState,
    sequence: &Sequence,
    lane: &Lane,
    pointer: Pos2,
    lanes_left: f32,
) -> Option<(ve_core::ClipId, FadeEdge)> {
    let track = sequence.track(lane.track)?;
    if track.kind != TrackKind::Audio {
        return None;
    }
    if pointer.y > lane.rect.top() + FADE_BAND_PX {
        return None;
    }
    let clip = track.clip_at(state.timeline.time_at(pointer.x - lanes_left))?;

    let x0 = lanes_left + state.timeline.x_of(clip.timeline_start);
    let x1 = lanes_left + state.timeline.x_of(clip.timeline_end());
    if (x1 - x0) < 12.0 {
        return None;
    }
    let pixels_per_tick = (x1 - x0) / clip.duration.raw().max(1) as f32;
    let (fade_in, fade_out) = clip.audio.fitted_fades(clip.duration);

    for (edge, tip) in [
        (FadeEdge::In, x0 + fade_in.length.raw() as f32 * pixels_per_tick),
        (FadeEdge::Out, x1 - fade_out.length.raw() as f32 * pixels_per_tick),
    ] {
        if (pointer.x - tip).abs() <= FADE_GRIP_PX {
            return Some((clip.id, edge));
        }
    }
    None
}

/// Works out which part of the source file a clip's visible body shows.
///
/// `whole` is where the clip's two edges fall, off screen included; `visible`
/// is the part of that the lane actually shows.
///
/// A clip stretching off both edges of the screen is clamped to the lane before
/// it is drawn, so the waveform has to be asked for the source range that the
/// *clamped* rectangle covers rather than the whole clip's. Interpolating
/// between the unclamped edges is what makes that exact at any scroll position,
/// and it carries clip speed for free: a clip playing at half rate covers half
/// as much source in the same width, and this says so without knowing about
/// speed at all.
fn waveform_area(
    state: &EditorState,
    clip: &ve_core::Clip,
    body: Rect,
    whole: std::ops::Range<f32>,
    visible: std::ops::Range<f32>,
    lanes_left: f32,
) -> waveform::Area {
    let span = (whole.end - whole.start).max(f32::EPSILON);
    let from = ((visible.start - whole.start) / span) as f64;
    let to = ((visible.end - whole.start) / span) as f64;
    let source = clip.source_duration().as_secs_f64();
    waveform::Area {
        rect: body,
        source: TimeRange::from_bounds(
            clip.source_in + Ticks::from_secs_f64(source * from),
            clip.source_in + Ticks::from_secs_f64(source * to),
        ),
        timeline: TimeRange::from_bounds(
            state.timeline.time_at(visible.start - lanes_left),
            state.timeline.time_at(visible.end - lanes_left),
        ),
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
    ruler: Rect,
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
        // A tab in the ruler, so a marker can be found and aimed at rather
        // than only noticed as a line behind the clips.
        let tab = Rect::from_min_max(
            Pos2::new(x, ruler.bottom() - 6.0),
            Pos2::new(x + 7.0, ruler.bottom()),
        );
        painter.rect_filled(tab, CornerRadius::same(1), colour);
    }
}

/// The selection rectangle, while one is being swept out.
fn draw_marquee(
    painter: &egui::Painter,
    area: Rect,
    state: &EditorState,
    response: &egui::Response,
    lanes_left: f32,
) {
    let TimelineDrag::Marquee { origin_time, origin_y } = state.drag else { return };
    let Some(pointer) = response.interact_pointer_pos() else { return };
    // Derived from the origin *time* rather than a stored X, so the rectangle
    // stays over the same clips if the view scrolls mid-drag.
    let origin_x = lanes_left + state.timeline.x_of(origin_time);
    let rect = Rect::from_two_pos(Pos2::new(origin_x, origin_y), pointer).intersect(area);
    painter.rect_filled(rect, CornerRadius::same(2), theme::ACCENT.gamma_multiply(0.18));
    painter.rect_stroke(
        rect,
        CornerRadius::same(2),
        Stroke::new(1.0, theme::ACCENT),
        egui::StrokeKind::Inside,
    );
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

    // A click on a track header switch is not a gesture: it toggles and stops
    // there, so it is answered before any drag can be started.
    if response.clicked() && pointer.x < lanes_left && pointer.y > ruler.bottom() {
        if let Some((track, flag, value)) = hit_test_switch(sequence, lanes, pointer) {
            actions.push(Action::SetTrackFlag { track, flag, value });
        }
        state.selection.track = lanes
            .iter()
            .find(|l| l.rect.y_range().contains(pointer.y))
            .map(|l| l.track)
            .or(state.selection.track);
        state.drag = TimelineDrag::None;
        return;
    }

    // Starting a gesture.
    if response.drag_started() || response.clicked() {
        // Where the *press* landed, not where the pointer is now. A drag is only
        // reported once the pointer has travelled past egui's threshold, by
        // which time it has left whatever small target it took hold of — which
        // is how a fade grip or a trim handle gets missed and a move begins
        // instead. The press origin is what the user aimed at.
        let grab = ui.input(|i| i.pointer.press_origin()).unwrap_or(pointer);
        let grab_time = state.timeline.time_at(grab.x - lanes_left);

        if grab.y <= ruler.bottom() {
            state.drag = TimelineDrag::Playhead;
        } else if let Some(lane) = lanes.iter().find(|l| l.rect.contains(grab)) {
            let additive = ui.input(|i| i.modifiers.shift || i.modifiers.command);
            // A fade grip is answered before anything else: it sits in a clip's
            // top corner, where a trim would otherwise take hold.
            if !lane.locked && state.tool == TimelineTool::Select {
                if let Some((clip, edge)) =
                    hit_test_fade(state, sequence, lane, grab, lanes_left)
                {
                    actions.push(Action::SelectClip { clip, track: lane.track, additive });
                    state.drag = TimelineDrag::FadeHandle {
                        clip,
                        edge: match edge {
                            FadeEdge::In => FadeEdgeKind::In,
                            FadeEdge::Out => FadeEdgeKind::Out,
                        },
                    };
                    return;
                }
            }
            match hit_test_clip(state, sequence, lane, grab, lanes_left) {
                Some((clip, edge)) => {
                    actions.push(Action::SelectClip { clip, track: lane.track, additive });
                    state.drag = if lane.locked {
                        TimelineDrag::None
                    } else {
                        begin_clip_drag(state, sequence, lane, clip, edge, grab_time)
                    };
                }
                None => {
                    if !additive {
                        actions.push(Action::ClearSelection);
                    }
                    state.selection.track = Some(lane.track);
                    if response.drag_started() {
                        // Dragging from empty track space sweeps out a
                        // selection rectangle.
                        state.drag =
                            TimelineDrag::Marquee { origin_time: grab_time, origin_y: grab.y };
                    } else {
                        // A click that never moves still moves the playhead,
                        // as clicking a timeline background always has.
                        let to = snap(state, sequence, grab_time, None);
                        actions.push(Action::ScrubTo(to));
                        state.drag = TimelineDrag::None;
                    }
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
            TimelineDrag::RollEdge { left, right, track } => {
                // The cut itself is the thing being dragged, so neither clip is
                // excluded from snapping: their far edges are legitimate limits.
                let to = sequence.snap_to_frame(snap(state, sequence, time_at_pointer, None));
                actions.push(Action::RollEditTo { left, right, track, to, coalesce: true });
            }
            TimelineDrag::SlipClip { clip, track, grab_time, source_in_at_grab } => {
                // Dragging right pulls the clip's contents right, which means
                // showing *earlier* frames — hence the sign.
                let Some((_, c)) = sequence.find_clip(clip) else { return };
                let shift = c.speed.timeline_to_source(time_at_pointer - grab_time);
                actions.push(Action::SlipClipTo {
                    clip,
                    track,
                    to_source_in: source_in_at_grab - shift,
                    coalesce: true,
                });
            }
            TimelineDrag::SlideClip { clip, track, grab_offset } => {
                let raw = (time_at_pointer - grab_offset).clamp_non_negative();
                let to = sequence.snap_to_frame(snap(state, sequence, raw, Some(clip)));
                actions.push(Action::SlideClipTo { clip, track, to, coalesce: true });
            }
            TimelineDrag::FadeHandle { clip, edge } => {
                let Some((_, c)) = sequence.find_clip(clip) else { return };
                // The pointer names where the fade reaches full level; the
                // length is the distance back to the clip's own end.
                let length = match edge {
                    FadeEdgeKind::In => time_at_pointer - c.timeline_start,
                    FadeEdgeKind::Out => c.timeline_end() - time_at_pointer,
                };
                let length = Ticks::new(length.raw().clamp(0, c.duration.raw()));
                actions.push(Action::SetClipFade {
                    clip,
                    edge: match edge {
                        FadeEdgeKind::In => FadeEdge::In,
                        FadeEdgeKind::Out => FadeEdge::Out,
                    },
                    length,
                    // Keeping the curve the clip already has means dragging a
                    // handle changes the length and nothing else.
                    curve: c
                        .audio
                        .fade(match edge {
                            FadeEdgeKind::In => FadeEdge::In,
                            FadeEdgeKind::Out => FadeEdge::Out,
                        })
                        .curve,
                    coalesce: true,
                });
            }
            TimelineDrag::Marquee { origin_time, origin_y } => {
                let range = TimeRange::from_bounds(
                    origin_time.min(time_at_pointer),
                    origin_time.max(time_at_pointer),
                );
                let (top, bottom) = (origin_y.min(pointer.y), origin_y.max(pointer.y));
                let tracks = lanes
                    .iter()
                    .filter(|l| l.rect.top() <= bottom && l.rect.bottom() >= top)
                    .map(|l| l.track)
                    .collect();
                // Rebuilt from scratch on every pointer move rather than added
                // to, so shrinking the rectangle lets clips go again.
                actions.push(Action::SelectClipsIn { range, tracks, additive: false });
            }
            TimelineDrag::None => {}
        }
    }

    if response.drag_stopped() {
        finish_drag(state, actions);
    }

    if !response.dragged()
        && pointer.x < lanes_left
        && pointer.y > ruler.bottom()
        && hit_test_switch(sequence, lanes, pointer).is_some()
    {
        ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
    }

    // A cursor that tells the user which gesture a press would begin.
    if !response.dragged() && pointer.y > ruler.bottom() {
        if let Some(lane) = lanes.iter().find(|l| l.rect.contains(pointer)) {
            if hit_test_fade(state, sequence, lane, pointer, lanes_left).is_some() {
                ui.ctx().set_cursor_icon(egui::CursorIcon::ResizeHorizontal);
                return;
            }
            if let Some((_, edge)) = hit_test_clip(state, sequence, lane, pointer, lanes_left) {
                let horizontal = match state.tool {
                    TimelineTool::Select => edge.is_some(),
                    TimelineTool::Roll | TimelineTool::Slip | TimelineTool::Slide => true,
                };
                ui.ctx().set_cursor_icon(if horizontal {
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

/// Decides which gesture a press on a clip begins, from the active tool.
///
/// The select tool splits by where on the clip the press landed — edges trim,
/// the body moves. The other three each do one thing wherever they are pressed,
/// which is the point of having them.
fn begin_clip_drag(
    state: &EditorState,
    sequence: &Sequence,
    lane: &Lane,
    clip: ve_core::ClipId,
    edge: Option<TrimEdgeKind>,
    time_at_pointer: Ticks,
) -> TimelineDrag {
    let Some((_, c)) = sequence.find_clip(clip) else { return TimelineDrag::None };
    match state.tool {
        TimelineTool::Select => match edge {
            Some(edge) => TimelineDrag::TrimClip { clip, track: lane.track, edge },
            None => TimelineDrag::MoveClip {
                clip,
                track: lane.track,
                grab_offset: time_at_pointer - c.timeline_start,
            },
        },
        TimelineTool::Roll => match roll_pair(sequence, lane.track, clip, time_at_pointer) {
            Some((left, right)) => TimelineDrag::RollEdge { left, right, track: lane.track },
            // A clip with nothing butted against the near end has no cut to
            // roll, so the press does nothing rather than doing something else.
            None => TimelineDrag::None,
        },
        TimelineTool::Slip => TimelineDrag::SlipClip {
            clip,
            track: lane.track,
            grab_time: time_at_pointer,
            source_in_at_grab: c.source_in,
        },
        TimelineTool::Slide => TimelineDrag::SlideClip {
            clip,
            track: lane.track,
            grab_offset: time_at_pointer - c.timeline_start,
        },
    }
}

/// The two clips meeting at the cut nearest the pointer, if there is one.
fn roll_pair(
    sequence: &Sequence,
    track: ve_core::TrackId,
    clip: ve_core::ClipId,
    at: Ticks,
) -> Option<(ve_core::ClipId, ve_core::ClipId)> {
    let track = sequence.track(track)?;
    let this = track.clip(clip)?;
    let (before, after) = track.neighbours_of(clip);
    // Whichever end of this clip the press was nearer decides which cut is
    // meant; a neighbour separated by a gap is not a cut at all.
    let nearer_start = (at - this.timeline_start) < (this.timeline_end() - at);
    if nearer_start {
        let left = before.filter(|l| l.timeline_end() == this.timeline_start)?;
        Some((left.id, clip))
    } else {
        let right = after.filter(|r| r.timeline_start == this.timeline_end())?;
        Some((clip, right.id))
    }
}

/// The track header switch under the pointer, and the value clicking it sets.
fn hit_test_switch(
    sequence: &Sequence,
    lanes: &[Lane],
    pointer: Pos2,
) -> Option<(ve_core::TrackId, ve_command::TrackFlag, bool)> {
    let lane = lanes.iter().find(|l| l.rect.y_range().contains(pointer.y))?;
    let index = lane.switches.iter().position(|r| r.contains(pointer))?;
    let track = sequence.track(lane.track)?;
    let flag = SWITCHES[index].1;
    let current = match flag {
        ve_command::TrackFlag::Muted => track.muted,
        ve_command::TrackFlag::Solo => track.solo,
        ve_command::TrackFlag::Locked => track.locked,
    };
    Some((lane.track, flag, !current))
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
