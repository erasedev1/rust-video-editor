//! The graph editor: what a value does between its keyframes.
//!
//! # Drawn by evaluating, not by drawing beziers
//!
//! Each curve is sampled by asking the property what it is worth at a column of
//! pixels — the same [`Property::evaluate`] the compositor and the mixer call.
//! A curve therefore cannot draw something other than what will be rendered:
//! there is no second implementation of easing to disagree with the first, and
//! a held keyframe steps on screen because it steps in the model.
//!
//! [`Property::evaluate`]: ve_core::Property::evaluate
//!
//! # The sheet answers *when*, the curves answer *what*
//!
//! Dragging a point here changes its **value** and leaves its time alone;
//! retiming is what the sheet is for. That division is deliberate rather than a
//! shortcut: a gesture that changed both would be two edits merging into the
//! history on every pointer move, and the one the user did not intend would be
//! the one they noticed a minute later.
//!
//! # A channel per number
//!
//! Position is two curves, not one: x and y move independently and have to be
//! editable that way. Colour is four. A property is therefore drawn as one
//! curve per channel, each in its own colour, and a drag carries the channel it
//! took hold of.

use egui::{Align2, Color32, FontId, Pos2, Rect, Stroke, Ui};
use ve_command::{property_ref, ClipProperty, PropertyValue};
use ve_core::Clip;
use ve_time::Ticks;

use crate::actions::Action;
use crate::panels::animation;
use crate::state::{EditorState, KeyframeGesture};
use crate::theme;

/// How near a point or a handle the pointer has to be to take hold of it.
const HIT_PX: f32 = 7.0;
/// Space kept clear at the top and bottom of the plot, so a curve at its
/// extreme is not drawn on the edge.
const PADDING_PX: f32 = 14.0;
/// How far apart curve samples are taken, in pixels. Three keeps a curve smooth
/// at any zoom while costing a few hundred evaluations rather than a few
/// thousand.
const SAMPLE_PX: f32 = 3.0;
/// Radius of a bezier handle.
const HANDLE_R: f32 = 3.5;
/// The strip along the top that says which curve is which.
const LEGEND_H: f32 = 13.0;

/// One property's place in the plot: which vertical span it is mapped into, and
/// what range of values that span covers.
pub struct Curve {
    pub property: ClipProperty,
    pub plot: Rect,
    pub low: f64,
    pub high: f64,
}

impl Curve {
    /// Where a value lands vertically. Values grow upwards, as they do on every
    /// graph.
    fn y_of(&self, value: f64) -> f32 {
        let span = (self.high - self.low).max(f64::EPSILON);
        let t = ((value - self.low) / span).clamp(-1.0, 2.0) as f32;
        self.plot.bottom() - t * self.plot.height()
    }

    /// The value a vertical position means, which is what a drag produces.
    fn value_at(&self, y: f32) -> f64 {
        let t = ((self.plot.bottom() - y) / self.plot.height().max(1.0)) as f64;
        self.low + t * (self.high - self.low)
    }
}

/// Draws every curve, returning where each property was mapped.
pub fn draw(
    painter: &egui::Painter,
    area: Rect,
    state: &EditorState,
    clip: &Clip,
    rows: &[ClipProperty],
    lanes_left: f32,
) -> Vec<Curve> {
    // The legend takes the top strip, so a curve at its extreme is not drawn
    // through the names of the things it is being compared with.
    let plot = Rect::from_min_max(
        Pos2::new(area.left(), area.top() + LEGEND_H + PADDING_PX),
        Pos2::new(area.right(), area.bottom() - PADDING_PX),
    );
    if plot.height() <= 1.0 {
        return Vec::new();
    }

    let curves: Vec<Curve> = rows
        .iter()
        .filter_map(|property| {
            let view = property_ref(clip, property)?;
            let (low, high) = value_range(&view);
            Some(Curve { property: property.clone(), plot, low, high })
        })
        .collect();

    // A frame for the plot, and the two numbers that say what the height means.
    // Only when one property is in focus: several properties share the height,
    // each mapped to its own range, and one pair of numbers for all of them
    // would be a lie.
    painter.rect_filled(area, 0.0, theme::SUNKEN.gamma_multiply(0.5));
    if let [only] = curves.as_slice() {
        for (value, align) in [(only.high, Align2::LEFT_TOP), (only.low, Align2::LEFT_BOTTOM)] {
            let y = only.y_of(value);
            painter.line_segment(
                [Pos2::new(plot.left(), y), Pos2::new(plot.right(), y)],
                Stroke::new(1.0, theme::GRID),
            );
            painter.text(
                Pos2::new(lanes_left + 4.0, y),
                align,
                format!("{value:.3}"),
                FontId::monospace(9.0),
                theme::TEXT_FAINT,
            );
        }
    }

    for curve in &curves {
        let Some(view) = property_ref(clip, &curve.property) else { continue };
        let keyframes = view.keyframes();
        if keyframes.is_empty() {
            continue;
        }
        let channels = keyframes[0].value.channel_count();

        for channel in 0..channels {
            let colour = channel_colour(channel);
            // Sampled by evaluating the property, so the line is what the
            // renderer will read rather than a second opinion about it.
            let mut points = Vec::new();
            let mut x = curve.plot.left().max(lanes_left);
            while x <= curve.plot.right() {
                let at = state.timeline.time_at(x - lanes_left) - clip.timeline_start;
                let value = view.evaluate(at).channel(channel);
                points.push(Pos2::new(x, curve.y_of(value)));
                x += SAMPLE_PX;
            }
            if points.len() > 1 {
                painter.add(egui::Shape::line(points, Stroke::new(1.4, colour)));
            }

            // The points themselves, and the handles of the selected ones.
            for (i, kf) in keyframes.iter().enumerate() {
                let x = lanes_left + state.timeline.x_of(clip.timeline_start + kf.time);
                if x < curve.plot.left() - HIT_PX || x > curve.plot.right() + HIT_PX {
                    continue;
                }
                let centre = Pos2::new(x, curve.y_of(kf.value.channel(channel)));
                let selected = state.keyframes.clip == Some(clip.id)
                    && state.keyframes.contains(&curve.property, kf.time);
                painter.circle(
                    centre,
                    if selected { 4.0 } else { 3.0 },
                    if selected { theme::CLIP_SELECTED } else { colour },
                    Stroke::new(1.0, colour),
                );

                if selected {
                    if let Some(next) = keyframes.get(i + 1) {
                        for (outgoing, handle) in handles(
                            state,
                            clip,
                            curve,
                            channel,
                            kf.time,
                            kf.value,
                            next.time,
                            next.value,
                            kf.interpolation,
                            lanes_left,
                        ) {
                            let anchor = if outgoing {
                                centre
                            } else {
                                Pos2::new(
                                    lanes_left
                                        + state.timeline.x_of(clip.timeline_start + next.time),
                                    curve.y_of(next.value.channel(channel)),
                                )
                            };
                            painter.line_segment(
                                [anchor, handle],
                                Stroke::new(1.0, colour.gamma_multiply(0.7)),
                            );
                            painter.circle_filled(handle, HANDLE_R, colour);
                        }
                    }
                }
            }
        }
    }

    draw_legend(painter, area, state, clip, &curves, lanes_left);
    curves
}

/// Which curve is which, along the top of the plot.
///
/// One line rather than a label per curve at its own height: the curves are
/// each normalised to their own range and cross constantly, so a label anchored
/// to a line would move every time the value did.
fn draw_legend(
    painter: &egui::Painter,
    area: Rect,
    state: &EditorState,
    clip: &Clip,
    curves: &[Curve],
    lanes_left: f32,
) {
    let mut x = lanes_left + 6.0;
    let y = area.top() + 2.0;
    for curve in curves {
        let Some(view) = property_ref(clip, &curve.property) else { continue };
        let Some(first) = view.keyframes().first().map(|k| k.value) else { continue };

        let label = curve.property.label();
        painter.text(
            Pos2::new(x, y),
            Align2::LEFT_TOP,
            label,
            FontId::proportional(9.5),
            if state.animation.focus.as_ref() == Some(&curve.property) {
                theme::ACCENT
            } else {
                theme::TEXT_DIM
            },
        );
        // Measured by the same rule the timeline elides labels with, rather
        // than by laying the text out twice.
        x += label.chars().count() as f32 * 5.2 + 6.0;

        for channel in 0..first.channel_count() {
            let colour = channel_colour(channel);
            painter.circle_filled(Pos2::new(x + 3.0, y + 5.0), 3.0, colour);
            x += 8.0;
            let text = first.channel_label(channel);
            if !text.is_empty() {
                painter.text(
                    Pos2::new(x, y),
                    Align2::LEFT_TOP,
                    text,
                    FontId::monospace(9.0),
                    colour,
                );
                x += 7.0;
            }
        }
        x += 12.0;
        if x > area.right() - 40.0 {
            return;
        }
    }
}

/// Where a segment's two bezier handles are drawn, as `(outgoing, position)`.
///
/// The control points are fractions of the segment in both axes, which is what
/// [`Interpolation::Bezier`] stores, so a handle's position is simply that
/// fraction of the span between the two keyframes.
///
/// [`Interpolation::Bezier`]: ve_core::Interpolation::Bezier
#[allow(clippy::too_many_arguments)]
fn handles(
    state: &EditorState,
    clip: &Clip,
    curve: &Curve,
    channel: usize,
    time: Ticks,
    value: PropertyValue,
    next_time: Ticks,
    next_value: PropertyValue,
    interpolation: ve_core::Interpolation,
    lanes_left: f32,
) -> Vec<(bool, Pos2)> {
    let Some([x1, y1, x2, y2]) = interpolation.control_points() else { return Vec::new() };
    let (v0, v1) = (value.channel(channel), next_value.channel(channel));
    let span = (next_time - time).raw() as f64;
    [(true, x1, y1), (false, x2, y2)]
        .into_iter()
        .map(|(outgoing, fx, fy)| {
            let at = time + Ticks::new((span * fx) as i64);
            let position = Pos2::new(
                lanes_left + state.timeline.x_of(clip.timeline_start + at),
                curve.y_of(v0 + (v1 - v0) * fy),
            );
            (outgoing, position)
        })
        .collect()
}

/// The range a property's curve is drawn across: its own keyframed extremes,
/// with room above and below so a curve at its limit is not drawn on the frame.
fn value_range(view: &ve_command::PropertyRef<'_>) -> (f64, f64) {
    let mut low = f64::INFINITY;
    let mut high = f64::NEG_INFINITY;
    for kf in view.keyframes() {
        for channel in 0..kf.value.channel_count() {
            let v = kf.value.channel(channel);
            low = low.min(v);
            high = high.max(v);
        }
    }
    if !low.is_finite() || !high.is_finite() {
        return (0.0, 1.0);
    }
    // A property that never changes would otherwise map to a zero-high band and
    // be drawn as a line through the middle of nothing.
    if (high - low).abs() < 1e-9 {
        return (low - 0.5, high + 0.5);
    }
    let margin = (high - low) * 0.15;
    (low - margin, high + margin)
}

fn channel_colour(channel: usize) -> Color32 {
    match channel {
        0 => theme::ACCENT,
        1 => theme::WAVEFORM_PEAK,
        2 => theme::WARNING,
        _ => theme::TEXT_DIM,
    }
}

/// Pointer handling for the curve editor.
#[allow(clippy::too_many_arguments)]
pub fn pointer(
    ui: &Ui,
    state: &mut EditorState,
    clip: &Clip,
    curves: &[Curve],
    response: &egui::Response,
    lanes_left: f32,
    actions: &mut Vec<Action>,
) {
    let pointer = response.interact_pointer_pos().or(response.hover_pos());
    let Some(pointer) = pointer else {
        if response.drag_stopped() {
            animation::finish(state, actions);
        }
        return;
    };

    if response.drag_started() || response.clicked() {
        let grab = ui.input(|i| i.pointer.press_origin()).unwrap_or(pointer);
        let additive = ui.input(|i| i.modifiers.shift || i.modifiers.command);

        // Handles first: they sit near their keyframe, and a press that means
        // "bend this curve" must not be read as "move this point".
        if let Some(gesture) = handle_at(state, clip, curves, grab, lanes_left) {
            state.animation.drag = Some(gesture);
            return;
        }

        match point_at(state, clip, curves, grab, lanes_left) {
            Some((property, time, channel)) => {
                actions.push(Action::SelectKeyframe {
                    clip: clip.id,
                    property: property.clone(),
                    at: time,
                    additive,
                });
                if response.drag_started() {
                    state.animation.drag =
                        Some(KeyframeGesture::Value { property, time, channel });
                }
            }
            None => {
                if !additive {
                    actions.push(Action::ClearKeyframeSelection);
                }
                if response.clicked() {
                    let at = state.timeline.time_at(pointer.x - lanes_left);
                    actions.push(Action::ScrubTo(animation::snap(state, at)));
                }
            }
        }
    }

    if response.dragged() {
        match state.animation.drag.clone() {
            Some(KeyframeGesture::Value { property, time, channel }) => {
                let Some(curve) = curves.iter().find(|c| c.property == property) else {
                    return;
                };
                let Some(view) = property_ref(clip, &property) else { return };
                let Some(kf) = view.keyframe_at(time) else { return };
                let value = kf.value.with_channel(channel, curve.value_at(pointer.y));
                actions.push(Action::SetKeyframe {
                    clip: clip.id,
                    property,
                    at: time,
                    value,
                    coalesce: true,
                });
            }
            Some(KeyframeGesture::Handle { property, time, outgoing }) => {
                let Some(curve) = curves.iter().find(|c| c.property == property) else {
                    return;
                };
                let Some(view) = property_ref(clip, &property) else { return };
                let keyframes = view.keyframes();
                let Some(index) = keyframes.iter().position(|k| k.time == time) else {
                    return;
                };
                let (Some(kf), Some(next)) = (keyframes.get(index), keyframes.get(index + 1))
                else {
                    return;
                };

                // The handle is a fraction of its own segment in both axes, so
                // the pointer is converted into that segment's coordinates
                // before anything is stored.
                let span = (next.time - kf.time).raw() as f64;
                let at = state.timeline.time_at(pointer.x - lanes_left)
                    - clip.timeline_start
                    - kf.time;
                let fx = if span <= 0.0 { 0.0 } else { at.raw() as f64 / span };

                let channel = 0;
                let (v0, v1) = (kf.value.channel(channel), next.value.channel(channel));
                let value = curve.value_at(pointer.y);
                // A flat segment has no value span to express a handle in, so
                // the vertical part of the drag is simply ignored rather than
                // dividing by nothing.
                let fy = if (v1 - v0).abs() < 1e-12 {
                    kf.interpolation
                        .control_points()
                        .map(|c| if outgoing { c[1] } else { c[3] })
                        .unwrap_or(0.0)
                } else {
                    (value - v0) / (v1 - v0)
                };

                actions.push(Action::SetKeyframeEasing {
                    clip: clip.id,
                    property,
                    at: time,
                    interpolation: kf.interpolation.with_handle(outgoing, fx, fy),
                    coalesce: true,
                });
            }
            _ => {}
        }
    }

    if response.drag_stopped() {
        animation::finish(state, actions);
    }

    if !response.dragged()
        && (point_at(state, clip, curves, pointer, lanes_left).is_some()
            || handle_at(state, clip, curves, pointer, lanes_left).is_some())
    {
        ui.ctx().set_cursor_icon(egui::CursorIcon::Grab);
    }
}

/// The keyframe point under the pointer, with the channel it belongs to.
fn point_at(
    state: &EditorState,
    clip: &Clip,
    curves: &[Curve],
    pointer: Pos2,
    lanes_left: f32,
) -> Option<(ClipProperty, Ticks, usize)> {
    for curve in curves {
        let view = property_ref(clip, &curve.property)?;
        for kf in view.keyframes() {
            let x = lanes_left + state.timeline.x_of(clip.timeline_start + kf.time);
            if (pointer.x - x).abs() > HIT_PX {
                continue;
            }
            for channel in 0..kf.value.channel_count() {
                let y = curve.y_of(kf.value.channel(channel));
                if (pointer.y - y).abs() <= HIT_PX {
                    return Some((curve.property.clone(), kf.time, channel));
                }
            }
        }
    }
    None
}

/// The bezier handle under the pointer. Only the selected keyframes have any,
/// which is what keeps a busy curve from being a field of dots.
fn handle_at(
    state: &EditorState,
    clip: &Clip,
    curves: &[Curve],
    pointer: Pos2,
    lanes_left: f32,
) -> Option<KeyframeGesture> {
    if state.keyframes.clip != Some(clip.id) {
        return None;
    }
    for curve in curves {
        let view = property_ref(clip, &curve.property)?;
        let keyframes = view.keyframes();
        for (i, kf) in keyframes.iter().enumerate() {
            if !state.keyframes.contains(&curve.property, kf.time) {
                continue;
            }
            let Some(next) = keyframes.get(i + 1) else { continue };
            for channel in 0..kf.value.channel_count() {
                for (outgoing, position) in handles(
                    state,
                    clip,
                    curve,
                    channel,
                    kf.time,
                    kf.value,
                    next.time,
                    next.value,
                    kf.interpolation,
                    lanes_left,
                ) {
                    if position.distance(pointer) <= HIT_PX {
                        return Some(KeyframeGesture::Handle {
                            property: curve.property.clone(),
                            time: kf.time,
                            outgoing,
                        });
                    }
                }
            }
        }
    }
    None
}
