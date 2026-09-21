//! The inspector: properties of whatever is selected.

use egui::{DragValue, RichText, Ui};
use ve_command::{ClipProperty, PropertyValue, TrackLevel};
use ve_core::registry::{EffectCategory, EffectRegistry, ParamKind};
use ve_core::{
    builtin_registry, BlendMode, Clip, ColorSpace, Effect, EffectId, Fade, FadeCurve, FadeEdge,
    MotionBlur, ParamValue, Rgba, Vec2,
};
use ve_engine::AudioLevels;
use ve_time::Ticks;

use crate::actions::Action;
use crate::meter;
use crate::state::EditorState;
use crate::theme;

/// `playhead` is where the transport is, which is the instant an animated
/// property is read at: a field showing the static value underneath a curve
/// would disagree with the picture beside it.
pub fn show(
    ui: &mut Ui,
    state: &EditorState,
    playhead: Ticks,
    levels: &AudioLevels,
    actions: &mut Vec<Action>,
) {
    ui.label(RichText::new("INSPECTOR").small().color(theme::TEXT_FAINT));
    ui.separator();

    let Some(sequence) = state.active_sequence() else { return };

    // A caption in hand takes the panel: it is what the user is editing, and
    // the clip underneath it is not what they asked about.
    if state.selection.cue.is_some() {
        caption_section(ui, state, actions);
        return;
    }

    let Some(clip_id) = state.selection.only() else {
        ui.add_space(12.0);
        ui.vertical_centered(|ui| {
            let n = state.selection.clips.len();
            ui.label(
                RichText::new(if n == 0 {
                    "Nothing selected".to_string()
                } else {
                    format!("{n} clips selected")
                })
                .color(theme::TEXT_DIM),
            );
            if n == 0 {
                ui.label(
                    RichText::new("Select a clip to edit its properties")
                        .small()
                        .color(theme::TEXT_FAINT),
                );
            }
        });
        // Nothing selected is the natural place for the canvas's own settings:
        // the panel is otherwise empty, and they describe what the preview is
        // showing.
        track_section(ui, state, levels, actions);
        caption_track_section(ui, state, actions);
        canvas_section(ui, state, actions);
        return;
    };

    let Some((_, clip)) = sequence.find_clip(clip_id) else { return };

    egui::ScrollArea::vertical().auto_shrink([false, false]).show(ui, |ui| {
        ui.label(RichText::new(&clip.name).color(theme::TEXT));
        ui.label(
            RichText::new(format!(
                "{} → {}   ({})",
                sequence.timecode_at(clip.timeline_start),
                sequence.timecode_at(clip.timeline_end()),
                clip.duration
            ))
            .small()
            .monospace()
            .color(theme::TEXT_FAINT),
        );
        ui.label(
            RichText::new(format!("source in {}", clip.source_in))
                .small()
                .monospace()
                .color(theme::TEXT_FAINT),
        );

        ui.add_space(8.0);
        section(ui, "Speed", |ui| {
            speed_row(ui, clip.speed, |speed| {
                actions.push(Action::SetClipSpeed { clip: clip_id, speed });
            });
        });

        // Animated properties are shown as they read *now*, which is also what
        // an edit to one of them writes a keyframe for.
        let local = clip.local_time_at(playhead);

        section(ui, "Transform", |ui| {
            let t = &clip.transform;
            point_row(
                ui,
                "Position",
                t.position.evaluate(local),
                t.position.is_animated(),
                |v| {
                    actions.push(Action::SetClipProperty {
                        clip: clip_id,
                        property: ClipProperty::Position,
                        value: PropertyValue::Point(v),
                    });
                },
            );
            point_row(ui, "Scale", t.scale.evaluate(local), t.scale.is_animated(), |v| {
                actions.push(Action::SetClipProperty {
                    clip: clip_id,
                    property: ClipProperty::Scale,
                    value: PropertyValue::Point(v),
                });
            });
            scalar_row(
                ui,
                "Rotation",
                t.rotation.evaluate(local),
                t.rotation.is_animated(),
                -3600.0..=3600.0,
                0.5,
                "°",
                |v| {
                    actions.push(Action::SetClipProperty {
                        clip: clip_id,
                        property: ClipProperty::Rotation,
                        value: PropertyValue::Scalar(v),
                    });
                },
            );
            point_row(ui, "Anchor", t.anchor.evaluate(local), t.anchor.is_animated(), |v| {
                actions.push(Action::SetClipProperty {
                    clip: clip_id,
                    property: ClipProperty::Anchor,
                    value: PropertyValue::Point(v),
                });
            });
            scalar_row(
                ui,
                "Opacity",
                t.opacity.evaluate(local),
                t.opacity.is_animated(),
                0.0..=1.0,
                0.01,
                "",
                |v| {
                    actions.push(Action::SetClipProperty {
                        clip: clip_id,
                        property: ClipProperty::Opacity,
                        value: PropertyValue::Scalar(v),
                    });
                },
            );
            blend_row(ui, clip.blend, |blend| {
                actions.push(Action::SetClipBlendMode { clip: clip_id, blend });
            });
            motion_blur_row(ui, clip, |blurred| {
                actions.push(Action::SetClipMotionBlur { clip: clip_id, blurred });
            });
        });

        section(ui, "Audio", |ui| {
            fade_row(ui, "Fade In", FadeEdge::In, clip.audio.fade_in, clip.duration, |fade| {
                actions.push(Action::SetClipFade {
                    clip: clip_id,
                    edge: FadeEdge::In,
                    length: fade.length,
                    curve: fade.curve,
                    coalesce: true,
                });
            });
            fade_row(
                ui,
                "Fade Out",
                FadeEdge::Out,
                clip.audio.fade_out,
                clip.duration,
                |fade| {
                    actions.push(Action::SetClipFade {
                        clip: clip_id,
                        edge: FadeEdge::Out,
                        length: fade.length,
                        curve: fade.curve,
                        coalesce: true,
                    });
                },
            );
            scalar_row(
                ui,
                "Volume",
                clip.audio.volume.evaluate(local),
                clip.audio.volume.is_animated(),
                0.0..=4.0,
                0.01,
                "",
                |v| {
                    actions.push(Action::SetClipProperty {
                        clip: clip_id,
                        property: ClipProperty::Volume,
                        value: PropertyValue::Scalar(v),
                    });
                },
            );
            scalar_row(
                ui,
                "Pan",
                clip.audio.pan.evaluate(local),
                clip.audio.pan.is_animated(),
                -1.0..=1.0,
                0.01,
                "",
                |v| {
                    actions.push(Action::SetClipProperty {
                        clip: clip_id,
                        property: ClipProperty::Pan,
                        value: PropertyValue::Scalar(v),
                    });
                },
            );
        });

        track_section(ui, state, levels, actions);

        effects_section(ui, clip, local, actions);
    });
}

/// The clip's effect chain: what runs over its picture, in the order it runs.
///
/// The chain is drawn as a list rather than as a set of tabs because **order is
/// the chain** — blurring and then brightening is a different picture from
/// brightening and then blurring — so the arrangement on screen has to be the
/// arrangement in the pipeline, with the arrows that change it right there.
///
/// Every control is built from the registry's descriptor rather than from
/// anything this panel knows about blurs or masks, which is what lets an effect
/// added later, or by a plugin, arrive here with working controls and no change
/// to this file.
fn effects_section(ui: &mut Ui, clip: &Clip, local: Ticks, actions: &mut Vec<Action>) {
    let registry = builtin_registry();
    let clip_id = clip.id;
    section(ui, "Effects", |ui| {
        let count = clip.effects.len();
        for (index, effect) in clip.effects.iter().enumerate() {
            effect_header(ui, clip_id, effect, index, count, actions);
            match registry.describe(effect) {
                Some(descriptor) => {
                    for param in &descriptor.params {
                        let Some(value) = effect.param(&param.key) else { continue };
                        param_row(
                            ui,
                            clip_id,
                            effect.id,
                            &param.label,
                            &param.hint,
                            &param.key,
                            &param.kind,
                            value,
                            local,
                            actions,
                        );
                    }
                }
                // An effect from a plugin that is not installed. It keeps its
                // place in the chain and can be switched off or removed; what
                // it cannot do is offer controls nobody can describe.
                None => {
                    ui.label(
                        RichText::new(format!("{} is not installed", effect.kind))
                            .small()
                            .color(theme::WARNING),
                    )
                    .on_hover_text(
                        "This effect is kept and saved back, but nothing in this build \
                         can draw it.",
                    );
                }
            }
            if index + 1 < count {
                ui.separator();
            }
        }
        if count == 0 {
            ui.label(RichText::new("No effects").small().color(theme::TEXT_FAINT));
        }
        ui.add_space(2.0);
        add_effect_menu(ui, registry, clip_id, actions);
    });
}

/// One effect's title row: the switch, the name, and where it sits.
fn effect_header(
    ui: &mut Ui,
    clip: ve_core::ClipId,
    effect: &Effect,
    index: usize,
    count: usize,
    actions: &mut Vec<Action>,
) {
    ui.horizontal(|ui| {
        let mut enabled = effect.enabled;
        if ui
            .checkbox(&mut enabled, "")
            .on_hover_text("Switch the effect off without losing its settings")
            .changed()
        {
            actions.push(Action::SetEffectEnabled { clip, effect: effect.id, enabled });
        }

        let mut name = effect.name.clone();
        let response = ui.add(
            egui::TextEdit::singleline(&mut name)
                .desired_width(110.0)
                .text_color(if effect.enabled { theme::TEXT } else { theme::TEXT_FAINT }),
        );
        if response.changed() && !name.trim().is_empty() {
            actions.push(Action::RenameEffect { clip, effect: effect.id, name });
        }

        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            if ui.small_button("✖").on_hover_text("Remove").clicked() {
                actions.push(Action::RemoveEffect { clip, effect: effect.id });
            }
            if ui
                .add_enabled(index + 1 < count, egui::Button::new("⏷").small())
                .on_hover_text("Run later in the chain")
                .clicked()
            {
                actions.push(Action::MoveEffect { clip, effect: effect.id, to: index + 1 });
            }
            if ui
                .add_enabled(index > 0, egui::Button::new("⏶").small())
                .on_hover_text("Run earlier in the chain")
                .clicked()
            {
                actions.push(Action::MoveEffect { clip, effect: effect.id, to: index - 1 });
            }
        });
    });
}

/// One parameter, as whatever control its declared kind calls for.
///
/// The animatable kinds go through [`Action::SetClipProperty`] like any other
/// property, so an edit at the playhead writes a keyframe when the parameter is
/// animated and the static value when it is not — exactly as it does for
/// position or opacity, and for the same reason.
#[allow(clippy::too_many_arguments)]
fn param_row(
    ui: &mut Ui,
    clip: ve_core::ClipId,
    effect: EffectId,
    label: &str,
    hint: &str,
    key: &str,
    kind: &ParamKind,
    value: &ParamValue,
    local: Ticks,
    actions: &mut Vec<Action>,
) {
    let property = ClipProperty::EffectParam { effect, key: key.to_string() };
    let animated = value.is_animated();
    let set = |value: PropertyValue, actions: &mut Vec<Action>| {
        actions.push(Action::SetClipProperty { clip, property: property.clone(), value });
    };

    let response = match (kind, value) {
        (ParamKind::Scalar { min, max, .. }, ParamValue::Scalar(p)) => {
            let span = (max - min).abs().max(1e-6);
            Some(scalar_row_response(
                ui,
                label,
                p.evaluate(local),
                animated,
                *min..=*max,
                // A slider that crosses its whole range in one screen of
                // dragging is uncontrollable at one end and coarse at the
                // other; a thousandth of the range per pixel is neither.
                span / 1000.0,
                "",
                |v| set(PropertyValue::Scalar(v), actions),
            ))
        }
        (ParamKind::Point { .. }, ParamValue::Point(p)) => {
            Some(point_row_response(ui, label, p.evaluate(local), animated, |v| {
                set(PropertyValue::Point(v), actions)
            }))
        }
        (ParamKind::Color { .. }, ParamValue::Color(p)) => {
            Some(color_row(ui, label, p.evaluate(local), animated, |v| {
                set(PropertyValue::Color(v), actions)
            }))
        }
        (ParamKind::Bool { .. }, ParamValue::Bool(on)) => {
            let mut on = *on;
            let response = ui
                .horizontal(|ui| {
                    property_label(ui, label, false);
                    ui.checkbox(&mut on, "")
                })
                .inner;
            if response.changed() {
                actions.push(Action::SetEffectOption {
                    clip,
                    effect,
                    key: key.to_string(),
                    value: ParamValue::Bool(on),
                });
            }
            Some(response)
        }
        (ParamKind::Choice { options, .. }, ParamValue::Choice(chosen)) => {
            let chosen = *chosen;
            let current = options.get(chosen as usize).cloned().unwrap_or_default();
            let response = ui
                .horizontal(|ui| {
                    property_label(ui, label, false);
                    egui::ComboBox::from_id_salt((effect.raw(), key))
                        .selected_text(current)
                        .show_ui(ui, |ui| {
                            for (i, option) in options.iter().enumerate() {
                                let i = i as u32;
                                if ui.selectable_label(i == chosen, option).clicked()
                                    && i != chosen
                                {
                                    actions.push(Action::SetEffectOption {
                                        clip,
                                        effect,
                                        key: key.to_string(),
                                        value: ParamValue::Choice(i),
                                    });
                                }
                            }
                        })
                        .response
                })
                .inner;
            Some(response)
        }
        // The value disagrees with what the registry declares, which the loader
        // conforms away — so this is only reachable for an effect registered
        // after a project was opened.
        _ => None,
    };
    if let Some(response) = response {
        response.on_hover_text(hint);
    }
}

/// The menu that adds an effect, grouped the way the registry groups them.
fn add_effect_menu(
    ui: &mut Ui,
    registry: &EffectRegistry,
    clip: ve_core::ClipId,
    actions: &mut Vec<Action>,
) {
    ui.menu_button("+ Add effect", |ui| {
        for category in EffectCategory::ALL {
            let mut any = false;
            for descriptor in registry.in_category(category) {
                if !any {
                    ui.label(RichText::new(category.label()).small().color(theme::TEXT_FAINT));
                    any = true;
                }
                if ui.button(&descriptor.name).on_hover_text(&descriptor.summary).clicked() {
                    actions.push(Action::AddEffect { clip, kind: descriptor.kind.clone() });
                    ui.close();
                }
            }
            if any {
                ui.separator();
            }
        }
    });
}

/// A colour swatch that opens a picker.
///
/// Alpha is editable because a tint's alpha multiplies coverage, which is a
/// real thing to want and not a mistake to guard against.
fn color_row(
    ui: &mut Ui,
    label: &str,
    value: Rgba,
    animated: bool,
    mut on_change: impl FnMut(Rgba),
) -> egui::Response {
    ui.horizontal(|ui| {
        property_label(ui, label, animated);
        let mut rgba = egui::Rgba::from_rgba_unmultiplied(
            value.r as f32,
            value.g as f32,
            value.b as f32,
            value.a as f32,
        );
        let response = egui::color_picker::color_edit_button_rgba(
            ui,
            &mut rgba,
            egui::color_picker::Alpha::OnlyBlend,
        );
        if response.changed() {
            let [r, g, b, a] = rgba.to_rgba_unmultiplied();
            on_change(Rgba::new(r as f64, g as f64, b as f64, a as f64));
        }
        response
    })
    .inner
}

/// Playback speed, as a multiplier and as a row of the usual presets.
///
/// Changing this keeps the frames the clip shows and changes how long it takes
/// to play them, so the clip's length on the timeline moves with it — which the
/// duration in the header above updates to show.
fn speed_row(ui: &mut Ui, speed: ve_core::Speed, mut on_change: impl FnMut(ve_core::Speed)) {
    ui.horizontal(|ui| {
        property_label(ui, "Speed", false);
        let mut v = speed.as_f64();
        let response = ui.add(
            DragValue::new(&mut v).speed(0.01).range(0.01..=100.0).suffix("×").max_decimals(3),
        );
        if response.changed() {
            // A speed the user cannot dial in is not worth refusing over: the
            // rational is built to the nearest thousandth and applied.
            if let Ok(speed) = ve_core::Speed::from_f64(v) {
                on_change(speed);
            }
        }
    });
    ui.horizontal(|ui| {
        property_label(ui, "", false);
        for (label, num, den) in
            [("¼×", 1, 4), ("½×", 1, 2), ("1×", 1, 1), ("2×", 2, 1), ("4×", 4, 1)]
        {
            let preset = ve_core::Speed::new(num, den).expect("a literal preset is valid");
            if ui.selectable_label(speed == preset, label).clicked() {
                on_change(preset);
            }
        }
    });
}

/// The blend mode, as a dropdown.
///
/// Sits with the transform rather than in a section of its own: opacity and
/// blend mode are the two halves of one question — how this layer combines with
/// what is under it — and every editor puts them next to each other for that
/// reason.
fn blend_row(ui: &mut Ui, blend: BlendMode, mut on_change: impl FnMut(BlendMode)) {
    ui.horizontal(|ui| {
        property_label(ui, "Blend", false);
        egui::ComboBox::from_id_salt("clip-blend-mode").selected_text(blend.label()).show_ui(
            ui,
            |ui| {
                for mode in BlendMode::ALL {
                    if ui.selectable_label(mode == blend, mode.label()).clicked()
                        && mode != blend
                    {
                        on_change(mode);
                    }
                }
            },
        );
    });
}

fn section(ui: &mut Ui, title: &str, body: impl FnOnce(&mut Ui)) {
    ui.add_space(6.0);
    ui.label(RichText::new(title).small().color(theme::TEXT_DIM));
    egui::Frame::new()
        .fill(theme::PANEL_RAISED)
        .corner_radius(theme::RADIUS)
        .inner_margin(egui::Margin::symmetric(6, 5))
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            body(ui);
        });
}

/// One scalar property row. `on_change` fires only while the value is actually
/// being dragged, so the undo history gets one coalesced entry per gesture.
#[allow(clippy::too_many_arguments)]
fn scalar_row(
    ui: &mut Ui,
    label: &str,
    value: f64,
    animated: bool,
    range: std::ops::RangeInclusive<f64>,
    speed: f64,
    suffix: &str,
    on_change: impl FnMut(f64),
) {
    scalar_row_response(ui, label, value, animated, range, speed, suffix, on_change);
}

/// [`scalar_row`], handing back the control so a caller can hang a tooltip on
/// it. The effect rows need that; the hand-written ones do not.
#[allow(clippy::too_many_arguments)]
fn scalar_row_response(
    ui: &mut Ui,
    label: &str,
    value: f64,
    animated: bool,
    range: std::ops::RangeInclusive<f64>,
    speed: f64,
    suffix: &str,
    mut on_change: impl FnMut(f64),
) -> egui::Response {
    let response = ui
        .horizontal(|ui| {
            property_label(ui, label, animated);
            let mut v = value;
            let response = ui.add(
                DragValue::new(&mut v).speed(speed).range(range).suffix(suffix).max_decimals(3),
            );
            if response.changed() {
                on_change(v);
            }
            response
        })
        .inner;
    response
}

fn point_row(
    ui: &mut Ui,
    label: &str,
    value: Vec2,
    animated: bool,
    on_change: impl FnMut(Vec2),
) {
    point_row_response(ui, label, value, animated, on_change);
}

fn point_row_response(
    ui: &mut Ui,
    label: &str,
    value: Vec2,
    animated: bool,
    mut on_change: impl FnMut(Vec2),
) -> egui::Response {
    ui.horizontal(|ui| {
        property_label(ui, label, animated);
        let mut x = value.x;
        let mut y = value.y;
        let rx = ui.add(DragValue::new(&mut x).speed(0.5).prefix("x ").max_decimals(3));
        let ry = ui.add(DragValue::new(&mut y).speed(0.5).prefix("y ").max_decimals(3));
        if rx.changed() || ry.changed() {
            on_change(Vec2::new(x, y));
        }
        rx.union(ry)
    })
    .inner
}

/// A property's name, marked when it is animated so a value that will not stay
/// put is visibly different from one that will.
fn property_label(ui: &mut Ui, label: &str, animated: bool) {
    let text = if animated {
        RichText::new(format!("♦ {label}")).color(theme::ACCENT)
    } else {
        RichText::new(label).color(theme::TEXT_DIM)
    };
    ui.add_sized([72.0, 18.0], egui::Label::new(text).halign(egui::Align::LEFT))
        .on_hover_text(if animated { "animated" } else { "" });
}

/// Settings belonging to the canvas being viewed, rather than to any clip.
fn canvas_section(ui: &mut Ui, state: &EditorState, actions: &mut Vec<Action>) {
    let (title, current, motion_blur) = match state.viewing() {
        Some(ve_engine::Viewing::Composition(id)) => match state.project.composition(id) {
            Some(composition) => (
                format!("COMPOSITION — {}", composition.name),
                composition.settings.color_space,
                composition.settings.motion_blur,
            ),
            None => return,
        },
        _ => match state.active_sequence() {
            Some(sequence) => (
                format!("SEQUENCE — {}", sequence.name),
                sequence.settings.color_space,
                sequence.settings.motion_blur,
            ),
            None => return,
        },
    };

    ui.add_space(14.0);
    ui.separator();
    ui.label(RichText::new(title).small().color(theme::TEXT_FAINT));
    ui.add_space(4.0);

    ui.label(RichText::new("Compositing").small().color(theme::TEXT_DIM));
    egui::Frame::new()
        .fill(theme::PANEL_RAISED)
        .corner_radius(theme::RADIUS)
        .inner_margin(egui::Margin::symmetric(6, 5))
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            for space in ColorSpace::ALL {
                let selected = space == current;
                if ui
                    .selectable_label(selected, space.label())
                    .on_hover_text(space.description())
                    .clicked()
                    && !selected
                {
                    actions.push(Action::SetColorSpace(space));
                }
            }
        });
    ui.label(
        RichText::new("Changes every dissolve and semi-transparent layer on this canvas.")
            .small()
            .color(theme::TEXT_FAINT),
    );

    shutter_section(ui, motion_blur, actions);
    ui.label(
        RichText::new("Only clips with motion blur switched on are exposed through it.")
            .small()
            .color(theme::TEXT_FAINT),
    );
}

/// The clip's motion blur switch, next to the transform it is a consequence of.
///
/// Disabled with a reason when the transform is not animated: a still layer has
/// nothing to smear, and a switch that can be turned on but does nothing is
/// worse than one that says why it cannot.
fn motion_blur_row(ui: &mut Ui, clip: &ve_core::Clip, mut on_change: impl FnMut(bool)) {
    let animated = clip.transform.is_animated();
    ui.horizontal(|ui| {
        property_label(ui, "Blur", clip.motion_blur);
        let mut blurred = clip.motion_blur;
        let response = ui
            .add_enabled(animated, egui::Checkbox::new(&mut blurred, "Motion blur"))
            .on_hover_text("Expose this clip across the shutter as it moves")
            .on_disabled_hover_text(
                "Keyframe the transform first: a still layer has nothing to smear",
            );
        if response.changed() {
            on_change(blurred);
        }
    });
}

/// The canvas's shutter: how long it is open, and how finely it is sampled.
fn shutter_section(ui: &mut Ui, current: MotionBlur, actions: &mut Vec<Action>) {
    section(ui, "Shutter", |ui| {
        ui.horizontal(|ui| {
            property_label(ui, "Motion blur", current.enabled);
            let mut enabled = current.enabled;
            if ui
                .checkbox(&mut enabled, "Enabled")
                .on_hover_text(
                    "Turns blur off for the whole canvas, whatever the clips ask for",
                )
                .changed()
            {
                actions.push(Action::SetMotionBlur(MotionBlur { enabled, ..current }));
            }
        });
        ui.horizontal(|ui| {
            property_label(ui, "Angle", false);
            let mut angle = current.shutter_angle;
            if ui
                .add(
                    DragValue::new(&mut angle)
                        .speed(1.0)
                        .range(0.0..=MotionBlur::MAX_ANGLE)
                        .suffix("°")
                        .max_decimals(1),
                )
                .on_hover_text(
                    "180° is the film convention: the shutter open for half the frame",
                )
                .changed()
            {
                actions.push(Action::SetMotionBlur(MotionBlur {
                    shutter_angle: angle,
                    ..current
                }));
            }
            ui.label(
                RichText::new(format!("{:.0}% of a frame", current.shutter_angle / 3.6))
                    .small()
                    .monospace()
                    .color(theme::TEXT_FAINT),
            );
        });
        ui.horizontal(|ui| {
            property_label(ui, "Samples", false);
            let mut samples = current.samples;
            if ui
                .add(
                    DragValue::new(&mut samples)
                        .speed(0.25)
                        .range(MotionBlur::MIN_SAMPLES..=MotionBlur::MAX_SAMPLES),
                )
                .on_hover_text("Each sample is a draw: more is smoother and slower")
                .changed()
            {
                actions.push(Action::SetMotionBlur(MotionBlur { samples, ..current }));
            }
        });
    });
}

/// One fade: how long it lasts, and what shape it follows.
///
/// Length in seconds rather than in frames, because a fade is heard rather than
/// counted and nothing about it has to land on the frame grid — the mixer works
/// in samples, which are finer than frames by three orders of magnitude.
fn fade_row(
    ui: &mut Ui,
    label: &str,
    edge: FadeEdge,
    fade: Fade,
    clip_duration: Ticks,
    mut on_change: impl FnMut(Fade),
) {
    ui.horizontal(|ui| {
        property_label(ui, label, fade.is_active());

        let mut seconds = fade.length.as_secs_f64();
        let longest = clip_duration.as_secs_f64().max(0.0);
        let response = ui.add(
            DragValue::new(&mut seconds)
                .speed(0.01)
                .range(0.0..=longest)
                .suffix(" s")
                .max_decimals(3),
        );
        if response.changed() {
            on_change(Fade::new(Ticks::from_secs_f64(seconds), fade.curve));
        }

        egui::ComboBox::from_id_salt(("fade-curve", edge))
            .width(96.0)
            .selected_text(fade.curve.label())
            .show_ui(ui, |ui| {
                for curve in FadeCurve::ALL {
                    if ui
                        .selectable_label(curve == fade.curve, curve.label())
                        .on_hover_text(curve.description())
                        .clicked()
                        && curve != fade.curve
                    {
                        on_change(Fade::new(fade.length, curve));
                    }
                }
            });
    });
}

/// The level, position and live reading of the track in focus.
///
/// Shown whether or not a clip is selected, because a mix is something you set
/// while listening rather than while pointing at a clip. The meter reads what is
/// actually coming out of the device — see [`ve_engine::AudioOutput`] for how it
/// is kept in step with the sound rather than with the mixer.
fn track_section(
    ui: &mut Ui,
    state: &EditorState,
    levels: &AudioLevels,
    actions: &mut Vec<Action>,
) {
    let Some(sequence) = state.active_sequence() else { return };
    let Some(track_id) = state.selection.track else { return };
    let Some(track) = sequence.track(track_id) else { return };

    let title = format!("Track — {}", track.name);
    section(ui, &title, |ui| {
        ui.horizontal(|ui| {
            property_label(ui, "Level", !track.is_unity());
            let mut volume = track.volume;
            let response = ui
                .add(DragValue::new(&mut volume).speed(0.01).range(0.0..=4.0).max_decimals(3));
            if response.changed() {
                actions.push(Action::SetTrackLevel {
                    track: track_id,
                    which: TrackLevel::Volume,
                    value: volume,
                });
            }
            ui.label(
                RichText::new(meter::gain_label(track.volume))
                    .small()
                    .monospace()
                    .color(theme::TEXT_FAINT),
            );
        });

        ui.horizontal(|ui| {
            property_label(ui, "Pan", false);
            let mut pan = track.pan;
            let response =
                ui.add(DragValue::new(&mut pan).speed(0.01).range(-1.0..=1.0).max_decimals(3));
            if response.changed() {
                actions.push(Action::SetTrackLevel {
                    track: track_id,
                    which: TrackLevel::Pan,
                    value: pan,
                });
            }
            ui.label(
                RichText::new(meter::pan_label(track.pan))
                    .small()
                    .monospace()
                    .color(theme::TEXT_FAINT),
            );
        });

        ui.horizontal(|ui| {
            property_label(ui, "Meter", false);
            let width = ui.available_width().max(40.0);
            let (rect, _) =
                ui.allocate_exact_size(egui::vec2(width, 12.0), egui::Sense::hover());
            meter::draw(ui.painter(), rect, levels.track(track_id));
        });
    });
}

/// The selected caption: its text, and where it sits.
///
/// The text box is the point of the panel. It is multi-line because a caption
/// is one or two lines and the line break is the only formatting either file
/// format carries — so where the line breaks is an editorial decision, and it
/// has to be made here rather than by whatever wraps the text later.
fn caption_section(ui: &mut Ui, state: &EditorState, actions: &mut Vec<Action>) {
    let Some(sequence) = state.active_sequence() else { return };
    let Some(track_id) = state.selection.captions else { return };
    let Some(cue_id) = state.selection.cue else { return };
    let Some(captions) = sequence.caption_track(track_id) else { return };
    let Some(cue) = captions.cue(cue_id) else { return };

    section(ui, &format!("CAPTION — {}", captions.name), |ui| {
        let mut text = cue.text.clone();
        let response = ui.add(
            egui::TextEdit::multiline(&mut text)
                .desired_width(f32::INFINITY)
                .desired_rows(2)
                .hint_text("the line as it will be read"),
        );
        if response.changed() {
            actions.push(Action::SetCueText { track: track_id, cue: cue_id, text });
        }
        // The gesture ends when the box loses focus, so the next edit to the
        // same caption is its own undo step rather than being absorbed.
        if response.lost_focus() {
            actions.push(Action::EndGesture);
        }

        ui.horizontal(|ui| {
            property_label(ui, "Start", false);
            let mut start = cue.start.as_secs_f64();
            if ui
                .add(DragValue::new(&mut start).speed(0.01).suffix(" s").max_decimals(3))
                .changed()
            {
                actions.push(Action::MoveCueTo {
                    track: track_id,
                    cue: cue_id,
                    to: Ticks::from_secs_f64(start),
                    coalesce: true,
                });
            }
            ui.label(
                RichText::new(sequence.timecode_at(cue.start).to_string())
                    .small()
                    .monospace()
                    .color(theme::TEXT_FAINT),
            );
        });

        ui.horizontal(|ui| {
            property_label(ui, "End", false);
            let mut end = cue.end().as_secs_f64();
            if ui
                .add(DragValue::new(&mut end).speed(0.01).suffix(" s").max_decimals(3))
                .changed()
            {
                actions.push(Action::TrimCueTo {
                    track: track_id,
                    cue: cue_id,
                    edge: ve_command::TrimEdge::End,
                    to: Ticks::from_secs_f64(end),
                    coalesce: true,
                });
            }
            // Reading speed is the number a captioner actually works to, and
            // the one thing about a cue that cannot be seen by looking at it.
            let characters = cue.text.chars().filter(|c| !c.is_control()).count();
            let seconds = cue.duration.as_secs_f64();
            if seconds > 0.0 && characters > 0 {
                let rate = characters as f64 / seconds;
                ui.label(
                    RichText::new(format!("{rate:.0} cps"))
                        .small()
                        .monospace()
                        .color(if rate > 20.0 { theme::WARNING } else { theme::TEXT_FAINT }),
                )
                .on_hover_text(
                    "characters per second — above about 20 is faster than most \
                     people read",
                );
            }
        });
    });

    caption_track_section(ui, state, actions);
}

/// The selected caption track: what language it is, and what becomes of it.
fn caption_track_section(ui: &mut Ui, state: &EditorState, actions: &mut Vec<Action>) {
    let Some(sequence) = state.active_sequence() else { return };
    let Some(track_id) = state.selection.captions else { return };
    let Some(captions) = sequence.caption_track(track_id) else { return };

    section(ui, &format!("CAPTION TRACK — {}", captions.name), |ui| {
        ui.horizontal(|ui| {
            property_label(ui, "Language", false);
            let mut language = captions.language.clone();
            let response = ui.add(
                egui::TextEdit::singleline(&mut language).desired_width(70.0).hint_text("en"),
            );
            if response.changed() {
                actions.push(Action::SetCaptionLanguage { track: track_id, language });
            }
            if response.lost_focus() {
                actions.push(Action::EndGesture);
            }
            ui.label(
                RichText::new(format!("{} caption(s)", captions.len()))
                    .small()
                    .color(theme::TEXT_FAINT),
            )
            .on_hover_text("the language tag names the file this track exports to");
        });
    });
}
