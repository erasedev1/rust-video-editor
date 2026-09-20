//! The animation editor: keyframes in time, and the curves between them.
//!
//! Drawn against the **timeline's own geometry** — the same scroll, the same
//! zoom, the same header width — so a keyframe sits directly under the frame it
//! happens on. Sharing `TimelineView` rather than keeping a second one is what
//! makes that true by construction: there is no second scroll position to keep
//! in step, because there is no second scroll position.
//!
//! Painted rather than built from widgets, for the same reason the timeline is:
//! a property with two hundred keyframes should cost two hundred diamonds, not
//! two hundred retained widgets laid out every frame.
//!
//! # Two views of the same keyframes
//!
//! The sheet answers *when*: keyframes as diamonds on the time axis, which is
//! what retiming, deleting and copying work on. The curve editor answers
//! *what*: the value between keyframes and the handles that shape it. They are
//! two views of one selection rather than two modes with state of their own, so
//! switching between them never loses what is in hand.

use egui::{Align2, Color32, CornerRadius, FontId, Pos2, Rect, Sense, Stroke, Ui};
use ve_command::{
    animatable_properties, property_ref, ClipProperty, KeyframePoint, PropertyValue,
};
use ve_core::{Clip, Interpolation};
use ve_time::Ticks;

use crate::actions::Action;
use crate::panels::curves;
use crate::state::{AnimationMode, EditorState, KeyframeGesture, PropertyTimes};
use crate::theme;

/// Height of one property row.
const ROW_H: f32 = 19.0;
/// Half the diagonal of a keyframe diamond.
const KEY_R: f32 = 4.5;
/// How near a keyframe the pointer has to be to take hold of it.
const KEY_HIT_PX: f32 = 6.0;
/// The strip above the rows where a selection's scale grips live.
const SCALE_STRIP_H: f32 = 11.0;
/// Side of a square grip.
const GRIP_PX: f32 = 7.0;
/// Side of a header button.
const BUTTON_PX: f32 = 14.0;

pub fn show(ui: &mut Ui, state: &mut EditorState, playhead: Ticks, actions: &mut Vec<Action>) {
    toolbar(ui, state, actions);

    let Some(sequence) = state.active_sequence() else { return };
    let Some(clip_id) = state.selection.only() else {
        empty(ui, "Select one clip to animate it");
        return;
    };
    let Some((_, clip)) = sequence.find_clip(clip_id) else { return };
    let clip = clip.clone();
    let properties = animatable_properties(&clip);

    // The curve editor draws one property at a time or every animated one at
    // once; the sheet always lists them all, because an un-animated property
    // is exactly what the user is looking for when they come here to animate
    // one.
    let rows: Vec<ClipProperty> = match state.animation.mode {
        AnimationMode::Sheet => properties,
        AnimationMode::Curves => properties
            .into_iter()
            .filter(|p| {
                property_ref(&clip, p).map(|v| v.is_animated()).unwrap_or(false)
                    && state.animation.focus.as_ref().map(|f| f == p).unwrap_or(true)
            })
            .collect(),
    };

    if rows.is_empty() {
        empty(ui, "Nothing on this clip is animated yet");
        return;
    }

    let available = ui.available_rect_before_wrap();
    if available.width() <= state.timeline.header_width + 20.0 || available.height() <= 8.0 {
        return;
    }

    egui::ScrollArea::vertical().auto_shrink([false, false]).show(ui, |ui| {
        let content_height = match state.animation.mode {
            AnimationMode::Sheet => SCALE_STRIP_H + rows.len() as f32 * ROW_H,
            // The curve editor wants room for a value axis, so it takes
            // whatever the panel has rather than a row's worth.
            AnimationMode::Curves => {
                (available.height() - 4.0).max(rows.len() as f32 * ROW_H * 2.0)
            }
        };
        let (rect, response) = ui.allocate_exact_size(
            egui::vec2(ui.available_width(), content_height),
            Sense::click_and_drag(),
        );
        let painter = ui.painter_at(rect);
        painter.rect_filled(rect, 0.0, theme::TRACK_LANE);

        let lanes_left = rect.left() + state.timeline.header_width;
        let lane_area = Rect::from_min_max(Pos2::new(lanes_left, rect.top()), rect.max);

        handle_scroll_and_zoom(ui, state, &response, lanes_left, lane_area);
        draw_grid(&painter, lane_area, state, &clip, lanes_left);

        match state.animation.mode {
            AnimationMode::Sheet => {
                let lanes =
                    draw_sheet(&painter, rect, state, &clip, &rows, playhead, lanes_left);
                draw_playhead(&painter, lane_area, state, playhead, lanes_left);
                sheet_pointer(ui, state, &clip, &lanes, &response, lanes_left, actions);
            }
            AnimationMode::Curves => {
                let curves = curves::draw(&painter, lane_area, state, &clip, &rows, lanes_left);
                draw_playhead(&painter, lane_area, state, playhead, lanes_left);
                curves::pointer(ui, state, &clip, &curves, &response, lanes_left, actions);
            }
        }
    });
}

fn empty(ui: &mut Ui, message: &str) {
    ui.add_space(10.0);
    ui.vertical_centered(|ui| {
        ui.label(egui::RichText::new(message).color(theme::TEXT_DIM));
        ui.label(
            egui::RichText::new(
                "Keyframe a property with ♦, then drag its keyframes to retime them",
            )
            .small()
            .color(theme::TEXT_FAINT),
        );
    });
}

fn toolbar(ui: &mut Ui, state: &EditorState, actions: &mut Vec<Action>) {
    ui.horizontal(|ui| {
        ui.label(egui::RichText::new("ANIMATION").small().color(theme::TEXT_FAINT));
        ui.add_space(6.0);
        for mode in AnimationMode::ALL {
            if ui
                .selectable_label(state.animation.mode == mode, mode.label())
                .on_hover_text(mode.hint())
                .clicked()
            {
                actions.push(Action::SetAnimationMode(mode));
            }
        }

        ui.add_space(10.0);
        let selected = state.keyframes.len();
        let any = selected > 0;
        if ui
            .add_enabled(any, egui::Button::new("Copy"))
            .on_hover_text("Copy the selected keyframes (Ctrl+C)")
            .clicked()
        {
            actions.push(Action::CopyKeyframes);
        }
        if ui
            .add_enabled(!state.keyframe_clipboard.is_empty(), egui::Button::new("Paste"))
            .on_hover_text("Paste keyframes at the playhead (Ctrl+V)")
            .on_disabled_hover_text("Copy some keyframes first")
            .clicked()
        {
            actions.push(Action::PasteKeyframes);
        }
        if ui
            .add_enabled(any, egui::Button::new("Delete"))
            .on_hover_text("Delete the selected keyframes (Del)")
            .clicked()
        {
            actions.push(Action::DeleteSelectedKeyframes);
        }

        // Easing applies to the selection, so one menu serves however many
        // keyframes are in hand rather than needing a menu per keyframe.
        ui.add_space(10.0);
        ui.add_enabled_ui(any, |ui| {
            egui::ComboBox::from_id_salt("keyframe-easing")
                .width(104.0)
                .selected_text(selected_easing(state).map(|e| e.label()).unwrap_or("Easing"))
                .show_ui(ui, |ui| {
                    for mode in Interpolation::ALL {
                        if ui
                            .selectable_label(false, mode.label())
                            .on_hover_text(mode.description())
                            .clicked()
                        {
                            for (property, at) in state.keyframes.keys() {
                                let Some(clip) = state.keyframes.clip else { continue };
                                actions.push(Action::SetKeyframeEasing {
                                    clip,
                                    property: property.clone(),
                                    at: *at,
                                    interpolation: mode,
                                    coalesce: false,
                                });
                            }
                        }
                    }
                });
        });

        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            let text = if any {
                format!("{selected} keyframe{} selected", if selected == 1 { "" } else { "s" })
            } else {
                "Nothing selected".to_string()
            };
            ui.label(egui::RichText::new(text).small().color(theme::TEXT_FAINT));
        });
    });
}

/// The easing every selected keyframe shares, or `None` when they differ.
fn selected_easing(state: &EditorState) -> Option<Interpolation> {
    let clip = state.keyframes.clip?;
    let (_, clip) = state.active_sequence()?.find_clip(clip)?;
    let mut common: Option<Interpolation> = None;
    for (property, at) in state.keyframes.keys() {
        let found = property_ref(clip, property)?.keyframe_at(*at)?;
        match common {
            None => common = Some(found.interpolation),
            Some(c) if c == found.interpolation => {}
            Some(_) => return None,
        }
    }
    common
}

/// Where a property's row ended up, so the pointer pass can map a position back
/// to a property and its buttons.
pub struct Row {
    pub property: ClipProperty,
    pub rect: Rect,
    pub lane: Rect,
    /// Keyframe-at-the-playhead, and clear-animation. The second is
    /// [`Rect::NOTHING`] when the property is not animated and there is nothing
    /// to clear.
    pub buttons: [Rect; 2],
}

/// Draws the property rows and returns where each one landed.
#[allow(clippy::too_many_arguments)]
fn draw_sheet(
    painter: &egui::Painter,
    area: Rect,
    state: &EditorState,
    clip: &Clip,
    rows: &[ClipProperty],
    playhead: Ticks,
    lanes_left: f32,
) -> Vec<Row> {
    let local = clip.local_time_at(playhead);
    let mut y = area.top() + SCALE_STRIP_H;
    let mut lanes = Vec::with_capacity(rows.len());

    for (index, property) in rows.iter().enumerate() {
        let rect =
            Rect::from_min_max(Pos2::new(area.left(), y), Pos2::new(area.right(), y + ROW_H));
        let lane = Rect::from_min_max(Pos2::new(lanes_left, y), rect.max);
        let Some(view) = property_ref(clip, property) else {
            y += ROW_H;
            continue;
        };

        if index % 2 == 1 {
            painter.rect_filled(lane, 0.0, theme::TRACK_LANE_ALT);
        }
        painter.rect_filled(
            Rect::from_min_max(rect.left_top(), Pos2::new(lanes_left, rect.bottom())),
            0.0,
            theme::TRACK_HEADER,
        );
        painter.line_segment(
            [rect.left_bottom(), rect.right_bottom()],
            Stroke::new(1.0, theme::SEPARATOR),
        );

        let animated = view.is_animated();
        let keyed_here = view.keyframe_at(local).is_some();
        let focused = state.animation.focus.as_ref() == Some(property);

        // The keyframe button, which is the one control that both adds and
        // removes: filled when there is a keyframe under the playhead.
        let key_button = Rect::from_center_size(
            Pos2::new(rect.left() + 12.0, rect.center().y),
            egui::Vec2::splat(BUTTON_PX),
        );
        draw_diamond(
            painter,
            key_button.center(),
            KEY_R,
            if keyed_here { theme::PLAYHEAD } else { Color32::TRANSPARENT },
            Stroke::new(1.0, if animated { theme::PLAYHEAD } else { theme::TEXT_FAINT }),
        );

        painter.text(
            Pos2::new(rect.left() + 24.0, rect.center().y),
            Align2::LEFT_CENTER,
            property.label(),
            FontId::proportional(10.5),
            match (animated, focused) {
                (_, true) => theme::ACCENT,
                (true, false) => theme::TEXT,
                (false, false) => theme::TEXT_DIM,
            },
        );

        // The value at the playhead, which is what makes the row worth reading
        // when the keyframes themselves are off screen.
        let mut clear_button = Rect::NOTHING;
        let mut value_right = lanes_left - 6.0;
        if animated {
            clear_button = Rect::from_center_size(
                Pos2::new(lanes_left - 10.0, rect.center().y),
                egui::Vec2::splat(BUTTON_PX),
            );
            painter.text(
                clear_button.center(),
                Align2::CENTER_CENTER,
                // The multiplication sign rather than a cross: the proportional
                // font the painter uses has it, and a missing glyph draws as a
                // hollow box that reads as another keyframe.
                "×",
                FontId::proportional(9.5),
                theme::TEXT_FAINT,
            );
            value_right = clear_button.left() - 4.0;
        }
        // Clipped to what is left of the row after the name, so a long value is
        // cut rather than drawn through the thing it belongs to.
        let name_width = property.label().chars().count() as f32 * 5.6;
        painter
            .with_clip_rect(Rect::from_min_max(
                Pos2::new(rect.left() + 26.0 + name_width, rect.top()),
                Pos2::new(value_right, rect.bottom()),
            ))
            .text(
                Pos2::new(value_right, rect.center().y),
                Align2::RIGHT_CENTER,
                value_text(view.evaluate(local)),
                FontId::monospace(9.0),
                theme::TEXT_FAINT,
            );

        // The keyframes themselves, culled to the visible time range.
        for kf in view.keyframes() {
            let x = lanes_left + state.timeline.x_of(clip.timeline_start + kf.time);
            if x < lane.left() - KEY_R || x > lane.right() + KEY_R {
                continue;
            }
            let selected = state.keyframes.clip == Some(clip.id)
                && state.keyframes.contains(property, kf.time);
            draw_keyframe(painter, Pos2::new(x, rect.center().y), &kf, selected);
        }

        lanes.push(Row {
            property: property.clone(),
            rect,
            lane,
            buttons: [key_button, clear_button],
        });
        y += ROW_H;
    }

    draw_scale_strip(painter, area, state, clip, lanes_left);
    lanes
}

/// The bar above the rows that scales a selection.
///
/// Only drawn with two or more keyframes in hand, because scaling one keyframe
/// about itself does nothing: with a single keyframe selected the gesture that
/// makes sense is a move, which is what dragging the keyframe already does.
fn draw_scale_strip(
    painter: &egui::Painter,
    area: Rect,
    state: &EditorState,
    clip: &Clip,
    lanes_left: f32,
) {
    let Some((first, last)) = scale_span(state, clip) else { return };
    let strip = Rect::from_min_max(
        Pos2::new(
            (lanes_left + state.timeline.x_of(clip.timeline_start + first)).max(lanes_left),
            area.top() + 2.0,
        ),
        Pos2::new(
            lanes_left + state.timeline.x_of(clip.timeline_start + last),
            area.top() + SCALE_STRIP_H - 3.0,
        ),
    );
    if strip.width() <= 0.0 {
        return;
    }
    painter.rect_filled(strip, CornerRadius::same(2), theme::ACCENT.gamma_multiply(0.25));
    for grip in scale_grips(state, clip, area, lanes_left) {
        painter.rect_filled(grip, CornerRadius::same(2), theme::ACCENT);
    }
}

/// The span a scale gesture works on: the first and last selected keyframe.
fn scale_span(state: &EditorState, clip: &Clip) -> Option<(Ticks, Ticks)> {
    if state.keyframes.clip != Some(clip.id) || state.keyframes.len() < 2 {
        return None;
    }
    let (first, last) = state.keyframes.span()?;
    (first != last).then_some((first, last))
}

/// Where the two scale grips sit, left then right.
fn scale_grips(state: &EditorState, clip: &Clip, area: Rect, lanes_left: f32) -> Vec<Rect> {
    let Some((first, last)) = scale_span(state, clip) else { return Vec::new() };
    [first, last]
        .into_iter()
        .map(|at| {
            Rect::from_center_size(
                Pos2::new(
                    lanes_left + state.timeline.x_of(clip.timeline_start + at),
                    area.top() + SCALE_STRIP_H / 2.0 - 1.0,
                ),
                egui::Vec2::splat(GRIP_PX),
            )
        })
        .collect()
}

fn draw_keyframe(painter: &egui::Painter, centre: Pos2, kf: &KeyframePoint, selected: bool) {
    let fill = if selected { theme::CLIP_SELECTED } else { theme::ACCENT };
    let stroke = Stroke::new(1.0, if selected { theme::ACCENT } else { theme::ACCENT_DIM });
    // A held keyframe is drawn square: the value steps at it rather than
    // easing through it, and that difference is worth seeing without clicking.
    if kf.interpolation.is_hold() {
        painter.rect_filled(
            Rect::from_center_size(centre, egui::Vec2::splat(KEY_R * 1.6)),
            CornerRadius::same(1),
            fill,
        );
    } else {
        draw_diamond(painter, centre, KEY_R, fill, stroke);
    }
}

fn draw_diamond(painter: &egui::Painter, centre: Pos2, r: f32, fill: Color32, stroke: Stroke) {
    painter.add(egui::Shape::convex_polygon(
        vec![
            Pos2::new(centre.x, centre.y - r),
            Pos2::new(centre.x + r, centre.y),
            Pos2::new(centre.x, centre.y + r),
            Pos2::new(centre.x - r, centre.y),
        ],
        fill,
        stroke,
    ));
}

/// A value, short enough to sit in a row header.
///
/// Large numbers drop their decimals rather than their digits: a position of
/// 1020.3 is a position of 1020, and the three characters that saves are the
/// difference between the value fitting beside the property's name and running
/// into it.
pub fn value_text(value: PropertyValue) -> String {
    fn scalar(v: f64) -> String {
        match v.abs() {
            x if x >= 100.0 => format!("{v:.0}"),
            x if x >= 10.0 => format!("{v:.1}"),
            _ => format!("{v:.3}"),
        }
    }
    // A point is two numbers in the space one gets, so it is rounded harder
    // still: the row says roughly where the thing is, and the inspector beside
    // it says exactly.
    fn coarse(v: f64) -> String {
        if v.abs() >= 10.0 {
            format!("{v:.0}")
        } else {
            format!("{v:.1}")
        }
    }
    match value {
        PropertyValue::Scalar(v) => scalar(v),
        PropertyValue::Point(p) => format!("{}, {}", coarse(p.x), coarse(p.y)),
        PropertyValue::Color(c) => format!("{:.2} {:.2} {:.2}", c.r, c.g, c.b),
    }
}

/// The clip's own extent, so it is obvious which part of the axis belongs to
/// the thing being animated.
fn draw_grid(
    painter: &egui::Painter,
    lane: Rect,
    state: &EditorState,
    clip: &Clip,
    lanes_left: f32,
) {
    let x0 = lanes_left + state.timeline.x_of(clip.timeline_start);
    let x1 = lanes_left + state.timeline.x_of(clip.timeline_end());
    let inside = Rect::from_min_max(
        Pos2::new(x0.max(lane.left()), lane.top()),
        Pos2::new(x1.min(lane.right()), lane.bottom()),
    );
    if inside.width() > 0.0 {
        painter.rect_filled(inside, 0.0, theme::TRACK_LANE_ALT.gamma_multiply(0.6));
    }
    for x in [x0, x1] {
        if lane.x_range().contains(x) {
            painter.line_segment(
                [Pos2::new(x, lane.top()), Pos2::new(x, lane.bottom())],
                Stroke::new(1.0, theme::SEPARATOR),
            );
        }
    }
}

pub fn draw_playhead(
    painter: &egui::Painter,
    lane: Rect,
    state: &EditorState,
    playhead: Ticks,
    lanes_left: f32,
) {
    let x = lanes_left + state.timeline.x_of(playhead);
    if x < lanes_left || x > lane.right() {
        return;
    }
    painter.line_segment(
        [Pos2::new(x, lane.top()), Pos2::new(x, lane.bottom())],
        Stroke::new(1.0, theme::PLAYHEAD.gamma_multiply(0.8)),
    );
}

/// Zooming and scrolling here move the timeline, because they *are* the
/// timeline's: there is one axis, shown twice.
pub fn handle_scroll_and_zoom(
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
    let zoom = ui.input(|i| i.modifiers.ctrl || i.modifiers.command);
    if zoom && scroll.y != 0.0 {
        let pointer_x = ui
            .input(|i| i.pointer.hover_pos())
            .map(|p| p.x - lanes_left)
            .unwrap_or(lane_area.width() / 2.0);
        let anchor = state.timeline.time_at(pointer_x);
        state.timeline.zoom_about(if scroll.y > 0.0 { 1.1 } else { 0.9 }, anchor, pointer_x);
    } else if scroll.x != 0.0 || (ui.input(|i| i.modifiers.shift) && scroll.y != 0.0) {
        let delta = if scroll.x != 0.0 { scroll.x } else { scroll.y };
        let seconds = (-delta / state.timeline.pixels_per_second) as f64;
        state.timeline.scroll =
            (state.timeline.scroll + Ticks::from_secs_f64(seconds)).clamp_non_negative();
    }
}

/// Pointer handling for the sheet.
#[allow(clippy::too_many_arguments)]
fn sheet_pointer(
    ui: &Ui,
    state: &mut EditorState,
    clip: &Clip,
    rows: &[Row],
    response: &egui::Response,
    lanes_left: f32,
    actions: &mut Vec<Action>,
) {
    let pointer = response.interact_pointer_pos().or(response.hover_pos());
    let Some(pointer) = pointer else {
        if response.drag_stopped() {
            finish(state, actions);
        }
        return;
    };
    let at_pointer = snap(state, state.timeline.time_at(pointer.x - lanes_left));

    if response.drag_started() || response.clicked() || response.double_clicked() {
        // Where the press landed, not where the pointer has since travelled: a
        // keyframe is six pixels wide, and a drag is only reported once the
        // pointer has left it.
        let grab = ui.input(|i| i.pointer.press_origin()).unwrap_or(pointer);
        let grab_time = snap(state, state.timeline.time_at(grab.x - lanes_left));
        let additive = ui.input(|i| i.modifiers.shift || i.modifiers.command);

        // A scale grip sits above the rows, where nothing else is, so it is
        // answered first and cheaply.
        if let Some(gesture) = grip_at(state, clip, rows, grab, lanes_left) {
            state.animation.drag = Some(gesture);
            return;
        }

        if let Some(row) = rows.iter().find(|r| r.rect.contains(grab)) {
            if grab.x < lanes_left {
                if row.buttons[0].contains(grab) {
                    actions.push(Action::ToggleKeyframeAtPlayhead {
                        clip: clip.id,
                        property: row.property.clone(),
                    });
                } else if row.buttons[1].contains(grab) {
                    actions.push(Action::RemoveAnimation {
                        clip: clip.id,
                        property: row.property.clone(),
                    });
                } else {
                    actions.push(Action::FocusProperty(Some(row.property.clone())));
                }
                state.animation.drag = None;
                return;
            }

            match keyframe_at(state, clip, &row.property, grab, lanes_left) {
                Some(time) => {
                    actions.push(Action::SelectKeyframe {
                        clip: clip.id,
                        property: row.property.clone(),
                        at: time,
                        additive,
                    });
                    // The gesture moves what the selection will be once that
                    // action lands, not what it is now: the panel never edits
                    // the selection itself, or the action would apply on top of
                    // it and a shift-click would cancel itself out.
                    let moving = selection_after(state, clip, &row.property, time, additive);
                    if response.drag_started() {
                        state.animation.drag = Some(KeyframeGesture::Move {
                            grab: grab_time,
                            snapshot: snapshot(state, clip, &moving),
                        });
                    }
                }
                None if response.double_clicked() => {
                    // Empty lane space, twice: keyframe the value the property
                    // already has there, which is how a curve gets its first
                    // point without moving anything.
                    if let Some(view) = property_ref(clip, &row.property) {
                        let local = clip.local_time_at(at_pointer);
                        actions.push(Action::SetKeyframe {
                            clip: clip.id,
                            property: row.property.clone(),
                            at: local,
                            value: view.evaluate(local),
                            coalesce: false,
                        });
                    }
                }
                None => {
                    if !additive {
                        actions.push(Action::ClearKeyframeSelection);
                    }
                    if response.drag_started() {
                        state.animation.drag = Some(KeyframeGesture::Move {
                            grab: grab_time,
                            snapshot: Vec::new(),
                        });
                    } else {
                        // A click in empty space moves the playhead, exactly as
                        // it does in the timeline.
                        actions.push(Action::ScrubTo(at_pointer));
                    }
                }
            }
        }
    }

    if response.dragged() {
        match state.animation.drag.clone() {
            Some(KeyframeGesture::Move { grab, snapshot }) if !snapshot.is_empty() => {
                let delta = at_pointer - grab;
                let times = snapshot
                    .iter()
                    .map(|p| {
                        let moved = p
                            .times
                            .iter()
                            .zip(&p.moving)
                            .map(|(t, moving)| if *moving { *t + delta } else { *t })
                            .collect();
                        (p.property.clone(), moved)
                    })
                    .collect();
                actions.push(Action::SetKeyframeTimes { clip: clip.id, times, coalesce: true });
            }
            Some(KeyframeGesture::Scale { anchor, grab, snapshot }) => {
                let times = scaled(&snapshot, anchor, grab, at_pointer - clip.timeline_start);
                actions.push(Action::SetKeyframeTimes { clip: clip.id, times, coalesce: true });
            }
            _ => {}
        }
    }

    if response.drag_stopped() {
        finish(state, actions);
    }

    if !response.dragged()
        && rows.iter().any(|r| {
            r.lane.contains(pointer)
                && keyframe_at(state, clip, &r.property, pointer, lanes_left).is_some()
        })
    {
        ui.ctx().set_cursor_icon(egui::CursorIcon::ResizeHorizontal);
    }
}

/// Scales a snapshot about `anchor`, sending the grabbed end to `to`.
///
/// Both times are clip-relative. A span collapsed to nothing would scale every
/// keyframe onto the anchor, so the factor is left at zero only when the user
/// really has dragged the grip onto it.
pub fn scaled(
    snapshot: &[PropertyTimes],
    anchor: Ticks,
    grab: Ticks,
    to: Ticks,
) -> Vec<(ClipProperty, Vec<Ticks>)> {
    let span = (grab - anchor).raw();
    let factor = if span == 0 { 1.0 } else { (to - anchor).raw() as f64 / span as f64 };
    snapshot
        .iter()
        .map(|p| {
            let times = p
                .times
                .iter()
                .zip(&p.moving)
                .map(|(t, moving)| {
                    if *moving {
                        anchor + Ticks::new(((*t - anchor).raw() as f64 * factor) as i64)
                    } else {
                        *t
                    }
                })
                .collect();
            (p.property.clone(), times)
        })
        .collect()
}

/// The scale gesture a press on one of the grips begins.
fn grip_at(
    state: &EditorState,
    clip: &Clip,
    rows: &[Row],
    grab: Pos2,
    lanes_left: f32,
) -> Option<KeyframeGesture> {
    let area = rows.first()?.rect;
    let strip = Rect::from_min_max(
        Pos2::new(lanes_left, area.top() - SCALE_STRIP_H),
        Pos2::new(area.right(), area.top()),
    );
    if !strip.contains(grab) {
        return None;
    }
    let (first, last) = scale_span(state, clip)?;
    let grips = scale_grips(
        state,
        clip,
        Rect::from_min_max(
            Pos2::new(strip.left(), strip.top()),
            Pos2::new(strip.right(), strip.bottom()),
        ),
        lanes_left,
    );
    let index = grips.iter().position(|g| g.expand(2.0).contains(grab))?;
    // Dragging one end scales about the other, which is what keeps the end you
    // are not touching where it is.
    let (anchor, grabbed) = if index == 0 { (last, first) } else { (first, last) };
    let held = state.keyframes.keys().to_vec();
    Some(KeyframeGesture::Scale {
        anchor,
        grab: grabbed,
        snapshot: snapshot(state, clip, &held),
    })
}

/// What the selection will be once the click that started this gesture has
/// been dispatched.
///
/// The panel pushes an action and reads the model; it never edits the selection
/// itself, so the two cannot disagree. This is the one place that has to look
/// one step ahead, because a drag has to know what it is dragging before the
/// click that chose it has landed.
fn selection_after(
    state: &EditorState,
    clip: &Clip,
    property: &ClipProperty,
    time: Ticks,
    additive: bool,
) -> Vec<(ClipProperty, Ticks)> {
    let held: Vec<(ClipProperty, Ticks)> = if state.keyframes.clip == Some(clip.id) {
        state.keyframes.keys().to_vec()
    } else {
        Vec::new()
    };
    match (additive, held.iter().any(|(p, t)| p == property && *t == time)) {
        // Shift-clicking a keyframe that is already held lets go of it, so the
        // drag moves whatever is left.
        (true, true) => held.into_iter().filter(|(p, t)| p != property || *t != time).collect(),
        (true, false) => {
            let mut all = held;
            all.push((property.clone(), time));
            all
        }
        // Dragging a keyframe that is not held takes it alone, which is what
        // pointing at a thing and pulling it has to mean.
        (false, false) => vec![(property.clone(), time)],
        (false, true) => held,
    }
}

/// Every keyframe time on the properties `moving` touches, and which of them
/// the gesture moves.
///
/// The whole list per property, not only the moving ones: a retime states where
/// *all* of a property's keyframes now are, so the ones standing still have to
/// be restated too.
pub fn snapshot(
    state: &EditorState,
    clip: &Clip,
    moving: &[(ClipProperty, Ticks)],
) -> Vec<PropertyTimes> {
    let _ = state;
    let mut properties: Vec<ClipProperty> = Vec::new();
    for (property, _) in moving {
        if !properties.contains(property) {
            properties.push(property.clone());
        }
    }
    properties
        .into_iter()
        .filter_map(|property| {
            let view = property_ref(clip, &property)?;
            let times = view.keyframe_times();
            let flags = times
                .iter()
                .map(|t| moving.iter().any(|(p, mt)| *p == property && mt == t))
                .collect();
            Some(PropertyTimes { property, times, moving: flags })
        })
        .collect()
}

/// The keyframe under the pointer on one row, if there is one.
fn keyframe_at(
    state: &EditorState,
    clip: &Clip,
    property: &ClipProperty,
    pointer: Pos2,
    lanes_left: f32,
) -> Option<Ticks> {
    let view = property_ref(clip, property)?;
    view.keyframe_times().into_iter().find(|time| {
        let x = lanes_left + state.timeline.x_of(clip.timeline_start + *time);
        (pointer.x - x).abs() <= KEY_HIT_PX
    })
}

/// Snaps a sequence time to the frame grid, so a keyframe lands on a frame
/// rather than between two.
///
/// Keyframe times are exact rationals like every other time in the editor, so
/// nothing forces them onto the grid — but a keyframe a few thousand ticks off
/// the frame it looks like it is on is a keyframe that never quite lands, and
/// every later one inherits the error.
pub fn snap(state: &EditorState, at: Ticks) -> Ticks {
    match state.active_sequence() {
        Some(sequence) => sequence.snap_to_frame(at),
        None => at,
    }
}

pub fn finish(state: &mut EditorState, actions: &mut Vec<Action>) {
    if state.animation.drag.take().is_some() {
        actions.push(Action::EndGesture);
    }
}
