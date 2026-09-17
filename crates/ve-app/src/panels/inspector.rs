//! The inspector: properties of whatever is selected.

use egui::{DragValue, RichText, Ui};
use ve_command::{ClipProperty, PropertyValue};
use ve_core::{BlendMode, Vec2};

use crate::actions::Action;
use crate::state::EditorState;
use crate::theme;

pub fn show(ui: &mut Ui, state: &EditorState, actions: &mut Vec<Action>) {
    ui.label(RichText::new("INSPECTOR").small().color(theme::TEXT_FAINT));
    ui.separator();

    let Some(sequence) = state.active_sequence() else { return };
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

        section(ui, "Transform", |ui| {
            let t = &clip.transform;
            point_row(ui, "Position", t.position.value, t.position.is_animated(), |v| {
                actions.push(Action::SetClipProperty {
                    clip: clip_id,
                    property: ClipProperty::Position,
                    value: PropertyValue::Point(v),
                });
            });
            point_row(ui, "Scale", t.scale.value, t.scale.is_animated(), |v| {
                actions.push(Action::SetClipProperty {
                    clip: clip_id,
                    property: ClipProperty::Scale,
                    value: PropertyValue::Point(v),
                });
            });
            scalar_row(
                ui,
                "Rotation",
                t.rotation.value,
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
            point_row(ui, "Anchor", t.anchor.value, t.anchor.is_animated(), |v| {
                actions.push(Action::SetClipProperty {
                    clip: clip_id,
                    property: ClipProperty::Anchor,
                    value: PropertyValue::Point(v),
                });
            });
            scalar_row(
                ui,
                "Opacity",
                t.opacity.value,
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
        });

        section(ui, "Audio", |ui| {
            scalar_row(
                ui,
                "Volume",
                clip.audio.volume.value,
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
                clip.audio.pan.value,
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

        if !clip.effects.is_empty() {
            section(ui, "Effects", |ui| {
                for effect in &clip.effects {
                    ui.horizontal(|ui| {
                        ui.label(RichText::new(&effect.name).color(theme::TEXT));
                        ui.label(RichText::new(&effect.kind).small().color(theme::TEXT_FAINT));
                    });
                }
            });
        }
    });
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
    mut on_change: impl FnMut(f64),
) {
    ui.horizontal(|ui| {
        property_label(ui, label, animated);
        let mut v = value;
        let response = ui.add(
            DragValue::new(&mut v).speed(speed).range(range).suffix(suffix).max_decimals(3),
        );
        if response.changed() {
            on_change(v);
        }
    });
}

fn point_row(
    ui: &mut Ui,
    label: &str,
    value: Vec2,
    animated: bool,
    mut on_change: impl FnMut(Vec2),
) {
    ui.horizontal(|ui| {
        property_label(ui, label, animated);
        let mut x = value.x;
        let mut y = value.y;
        let rx = ui.add(DragValue::new(&mut x).speed(0.5).prefix("x ").max_decimals(3));
        let ry = ui.add(DragValue::new(&mut y).speed(0.5).prefix("y ").max_decimals(3));
        if rx.changed() || ry.changed() {
            on_change(Vec2::new(x, y));
        }
    });
}

/// A property's name, marked when it is animated so a value that will not stay
/// put is visibly different from one that will.
fn property_label(ui: &mut Ui, label: &str, animated: bool) {
    let text = if animated {
        RichText::new(format!("◆ {label}")).color(theme::ACCENT)
    } else {
        RichText::new(label).color(theme::TEXT_DIM)
    };
    ui.add_sized([72.0, 18.0], egui::Label::new(text).halign(egui::Align::LEFT))
        .on_hover_text(if animated { "animated" } else { "" });
}
