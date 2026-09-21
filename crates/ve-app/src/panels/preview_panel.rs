//! The program monitor: the composited picture, plus transport controls.

use egui::{Align, Color32, FontId, Layout, Rect, RichText, Sense, Ui, Vec2};
use ve_core::Size;
use ve_time::Ticks;

use crate::actions::Action;
use crate::state::EditorState;
use crate::theme;

pub struct PreviewInfo {
    pub texture: Option<egui::TextureId>,
    pub size: Size,
    pub position: Ticks,
    pub playing: bool,
    pub pending_layers: usize,
    pub visible_layers: usize,
}

pub fn show(ui: &mut Ui, state: &EditorState, info: &PreviewInfo, actions: &mut Vec<Action>) {
    let Some(sequence) = state.active_sequence() else { return };

    ui.horizontal(|ui| {
        ui.label(RichText::new("PREVIEW").small().color(theme::TEXT_FAINT));
        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
            ui.label(
                RichText::new(format!("{}×{}", info.size.width, info.size.height))
                    .small()
                    .color(theme::TEXT_FAINT),
            );
            ui.label(
                RichText::new(format!("{}", sequence.rate())).small().color(theme::TEXT_FAINT),
            );
        });
    });
    ui.separator();

    // Reserve the transport row before the picture, so the picture takes
    // whatever is left rather than pushing the controls off the bottom.
    let transport_height = 34.0;
    let picture_height = (ui.available_height() - transport_height).max(40.0);
    let picture_rect = ui.allocate_space(Vec2::new(ui.available_width(), picture_height)).1;

    draw_picture(ui, picture_rect, info);
    draw_captions(ui, picture_rect, state, info);
    ui.add_space(4.0);
    transport(ui, state, info, actions);
}

/// Where the picture lands inside the panel, letterboxed.
///
/// Fit rather than fill: cropping the program monitor would misrepresent the
/// frame, which is the one thing a program monitor must not do.
fn picture_rect(area: Rect, size: Size) -> Rect {
    let source_aspect = size.aspect_ratio() as f32;
    let area_aspect = area.width() / area.height().max(1.0);
    if area_aspect > source_aspect {
        let w = area.height() * source_aspect;
        Rect::from_center_size(area.center(), Vec2::new(w, area.height()))
    } else {
        let h = area.width() / source_aspect.max(0.0001);
        Rect::from_center_size(area.center(), Vec2::new(area.width(), h))
    }
}

/// Draws the composited frame, letterboxed to preserve aspect ratio.
fn draw_picture(ui: &Ui, area: Rect, info: &PreviewInfo) {
    let painter = ui.painter_at(area);
    painter.rect_filled(area, 0.0, theme::SUNKEN);

    let Some(texture) = info.texture else {
        painter.text(
            area.center(),
            egui::Align2::CENTER_CENTER,
            "no preview",
            egui::FontId::proportional(12.0),
            theme::TEXT_FAINT,
        );
        return;
    };

    let display = picture_rect(area, info.size);

    painter.image(
        texture,
        display,
        Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(1.0, 1.0)),
        Color32::WHITE,
    );

    if info.visible_layers == 0 {
        painter.text(
            area.center(),
            egui::Align2::CENTER_CENTER,
            "no clip at the playhead",
            egui::FontId::proportional(12.0),
            theme::TEXT_FAINT,
        );
    } else if info.pending_layers > 0 {
        // Say so rather than showing a stale or blank frame with no explanation.
        painter.text(
            display.left_top() + Vec2::new(8.0, 8.0),
            egui::Align2::LEFT_TOP,
            "decoding…",
            egui::FontId::proportional(11.0),
            theme::TEXT_DIM,
        );
    }
}

/// Draws the caption that is on screen at the playhead.
///
/// **This is an overlay, not a burn-in.** The interface draws it with the
/// interface's own text; the compositor never sees it, and neither does an
/// export, which writes the captions as a file beside the delivery. Burning
/// them into the picture needs a glyph rasteriser in the render graph — the
/// text work the motion-graphics phase brings — and pretending otherwise here
/// would mean a preview that showed something the delivery would not have.
///
/// One track's captions at a time: the one selected, or the first. With three
/// languages on the timeline, only one of them is being written at any moment,
/// and stacking all three over the picture would show a state that no viewer
/// will ever see.
fn draw_captions(ui: &Ui, area: Rect, state: &EditorState, info: &PreviewInfo) {
    if !state.show_captions {
        return;
    }
    let Some(sequence) = state.active_sequence() else { return };
    let captions = state
        .selection
        .captions
        .and_then(|id| sequence.caption_track(id))
        .or_else(|| sequence.captions.first());
    let Some(cue) = captions.and_then(|t| t.cue_at(info.position)) else { return };

    let text = ve_caption::strip_markup(&cue.text);
    let lines: Vec<&str> = text.lines().filter(|l| !l.trim().is_empty()).collect();
    if lines.is_empty() {
        return;
    }

    // Against the picture rather than the panel, so a caption sits inside the
    // frame it belongs to rather than floating in the letterbox.
    let picture = picture_rect(area, info.size);
    let painter = ui.painter_at(picture);
    // Proportional to the picture's height, as a real caption is: the same
    // overlay at a fixed size would be unreadable on a small preview and lost
    // on a large one.
    let font = FontId::proportional((picture.height() * 0.045).clamp(9.0, 28.0));
    let line_height = font.size * 1.25;
    let bottom = picture.bottom() - picture.height() * 0.06;

    for (i, line) in lines.iter().rev().enumerate() {
        let centre = egui::pos2(picture.center().x, bottom - line_height * (i as f32 + 0.5));
        let galley = painter.layout_no_wrap((*line).to_string(), font.clone(), Color32::WHITE);
        let rect = Rect::from_center_size(
            centre,
            galley.size() + Vec2::new(font.size * 0.6, font.size * 0.2),
        );
        // A box behind the text, because a caption over a white sky is not a
        // caption. The same thing every player does, for the same reason.
        painter.rect_filled(rect, egui::CornerRadius::same(2), Color32::from_black_alpha(168));
        painter.galley(rect.center() - galley.size() / 2.0, galley, Color32::WHITE);
    }
}

fn transport(ui: &mut Ui, state: &EditorState, info: &PreviewInfo, actions: &mut Vec<Action>) {
    let Some(sequence) = state.active_sequence() else { return };

    ui.horizontal(|ui| {
        if ui.button("⏮").on_hover_text("Go to start (Home)").clicked() {
            actions.push(Action::GoToStart);
        }
        if ui.button("◀").on_hover_text("Previous frame (←)").clicked() {
            actions.push(Action::StepFrames(-1));
        }

        let play_label = if info.playing { "⏸" } else { "▶" };
        let play = ui.add(
            egui::Button::new(RichText::new(play_label).size(14.0))
                .min_size(Vec2::new(40.0, 22.0)),
        );
        if play.on_hover_text("Play / pause (Space)").clicked() {
            actions.push(Action::TogglePlayback);
        }

        if ui.button("▶").on_hover_text("Next frame (→)").clicked() {
            actions.push(Action::StepFrames(1));
        }
        if ui.button("⏭").on_hover_text("Go to end (End)").clicked() {
            actions.push(Action::GoToEnd);
        }

        ui.add_space(8.0);
        // Monospaced so the digits do not jitter as they run.
        ui.label(
            RichText::new(sequence.timecode_at(info.position).to_string())
                .monospace()
                .color(theme::PLAYHEAD),
        );
        ui.label(
            RichText::new(format!("/ {}", sequence.timecode_at(sequence.duration())))
                .monospace()
                .small()
                .color(theme::TEXT_FAINT),
        );
    });
}

/// A scrub bar under the preview, for coarse navigation.
pub fn scrub_bar(ui: &mut Ui, state: &EditorState, position: Ticks, actions: &mut Vec<Action>) {
    let Some(sequence) = state.active_sequence() else { return };
    let duration = sequence.duration();
    if duration.raw() <= 0 {
        return;
    }

    let height = 10.0;
    let (rect, response) = ui
        .allocate_exact_size(Vec2::new(ui.available_width(), height), Sense::click_and_drag());
    let painter = ui.painter_at(rect);
    painter.rect_filled(rect, theme::RADIUS, theme::SUNKEN);

    let fraction = (position.as_secs_f32() / duration.as_secs_f32()).clamp(0.0, 1.0);
    let x = rect.left() + rect.width() * fraction;
    painter.rect_filled(
        Rect::from_min_max(rect.left_top(), egui::pos2(x, rect.bottom())),
        theme::RADIUS,
        theme::ACCENT_DIM,
    );
    painter.line_segment(
        [egui::pos2(x, rect.top()), egui::pos2(x, rect.bottom())],
        egui::Stroke::new(2.0, theme::PLAYHEAD),
    );

    if let Some(pointer) = response.interact_pointer_pos() {
        let f = ((pointer.x - rect.left()) / rect.width()).clamp(0.0, 1.0);
        actions
            .push(Action::ScrubTo(Ticks::from_secs_f64((duration.as_secs_f32() * f) as f64)));
    }
}
