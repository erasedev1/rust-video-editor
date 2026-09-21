//! The scopes panel: the picture as numbers, beside the picture as a picture.
//!
//! One instrument at a time. Three scopes tiled into the width beside a preview
//! are three scopes too small to read, and the reading wanted is rarely all
//! three at the same instant: exposure is a waveform question, a cast is a
//! vectorscope question, and how much of the range is being used is a histogram
//! question.
//!
//! # The graticule is drawn, the trace is uploaded
//!
//! A trace is tens of thousands of lit cells, which is a picture and is drawn
//! as one — `ve_render::scopes` counts it into an image and this uploads that
//! image once per distinct frame. The graticule over it is a handful of lines
//! and is stroked by the painter, so it stays crisp when the panel is resized
//! and does not have to be re-counted to be redrawn.

use ve_render::scopes::{self, Histogram, ScopeSample, WaveformMode};
use ve_render::ScopeImage;

use crate::actions::Action;
use crate::state::{EditorState, ScopeKind};
use crate::theme;

/// How big a trace is counted into, before it is scaled to the panel.
///
/// Fixed rather than the panel's own size, so resizing the panel does not
/// change what the scope *reads* — only how large it is drawn. A scope whose
/// numbers moved when its window was dragged would be an instrument nobody
/// could trust.
const PLOT: ve_core::Size = ve_core::Size::new(256, 256);

/// The uploaded traces, held across frames so a held picture is uploaded once.
#[derive(Default)]
pub struct ScopeTextures {
    handle: Option<egui::TextureHandle>,
    /// What is in the handle, so an unchanged trace is not re-uploaded.
    shown: Option<(ScopeKind, WaveformMode, u64)>,
}

impl ScopeTextures {
    /// The texture for `image`, uploading it only if it is not the one already
    /// there.
    fn texture(
        &mut self,
        ctx: &egui::Context,
        id: (ScopeKind, WaveformMode, u64),
        image: impl FnOnce() -> ScopeImage,
    ) -> &egui::TextureHandle {
        if self.shown != Some(id) || self.handle.is_none() {
            let plot = image();
            let colour = egui::ColorImage::from_rgba_unmultiplied(
                [plot.size.width as usize, plot.size.height as usize],
                &plot.pixels,
            );
            match &mut self.handle {
                Some(handle) => handle.set(colour, egui::TextureOptions::LINEAR),
                None => {
                    self.handle = Some(ctx.load_texture(
                        "verge-scope",
                        colour,
                        egui::TextureOptions::LINEAR,
                    ))
                }
            }
            self.shown = Some(id);
        }
        self.handle.as_ref().expect("just filled")
    }
}

/// Draws the panel. `sample` is the last picture read back, if any.
pub fn show(
    ui: &mut egui::Ui,
    state: &EditorState,
    sample: Option<&ScopeSample>,
    textures: &mut ScopeTextures,
    actions: &mut Vec<Action>,
) {
    ui.horizontal(|ui| {
        for kind in ScopeKind::ALL {
            if ui.selectable_label(state.scopes.kind == kind, kind.label()).clicked() {
                actions.push(Action::ShowScope(kind));
            }
        }
    });
    if state.scopes.kind == ScopeKind::Waveform {
        ui.horizontal(|ui| {
            for mode in WaveformMode::ALL {
                if ui.selectable_label(state.scopes.waveform == mode, mode.label()).clicked() {
                    actions.push(Action::SetWaveformMode(mode));
                }
            }
        });
    }
    ui.add_space(4.0);

    let Some(sample) = sample.filter(|s| !s.is_empty()) else {
        ui.centered_and_justified(|ui| {
            ui.label(egui::RichText::new("no picture").color(theme::TEXT_FAINT));
        });
        return;
    };

    // Identity of what is being drawn, so an unchanged picture in an unchanged
    // scope is not counted or uploaded again.
    let id = (state.scopes.kind, state.scopes.waveform, fingerprint(sample));

    match state.scopes.kind {
        ScopeKind::Waveform => {
            let mode = state.scopes.waveform;
            let texture =
                textures.texture(ui.ctx(), id, || scopes::waveform(sample, mode, PLOT));
            let rect = draw_trace(ui, texture, false);
            waveform_graticule(ui, rect, mode);
        }
        ScopeKind::Vectorscope => {
            let texture = textures.texture(ui.ctx(), id, || scopes::vectorscope(sample, PLOT));
            let rect = draw_trace(ui, texture, true);
            vectorscope_graticule(ui, rect);
        }
        ScopeKind::Histogram => histogram(ui, &Histogram::of(sample)),
    }
}

/// A cheap identity for a sample, so a held frame is recognised as held.
///
/// The sampler already refuses to read the same composite twice, so this only
/// has to notice that the *contents* changed — not to be a hash anyone could
/// rely on. A few hundred bytes spread through the buffer is enough for that
/// and costs nothing beside counting the whole frame.
fn fingerprint(sample: &ScopeSample) -> u64 {
    use std::hash::{Hash, Hasher};
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    sample.size().hash(&mut hasher);
    sample.bytes().iter().step_by(257).for_each(|b| b.hash(&mut hasher));
    hasher.finish()
}

/// Draws the trace into the space available, on the sunken surface every
/// instrument in the editor sits on.
fn draw_trace(ui: &mut egui::Ui, texture: &egui::TextureHandle, square: bool) -> egui::Rect {
    let available = ui.available_size();
    let size = if square {
        let side = available.x.min(available.y).max(32.0);
        egui::vec2(side, side)
    } else {
        egui::vec2(available.x.max(32.0), available.y.max(32.0))
    };
    let (rect, _) = ui.allocate_exact_size(size, egui::Sense::hover());
    ui.painter().rect_filled(rect, theme::RADIUS, theme::SUNKEN);
    ui.painter().image(
        texture.id(),
        rect,
        egui::Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(1.0, 1.0)),
        egui::Color32::WHITE,
    );
    rect
}

/// The lines a waveform is read against: 0 and 100 IRE, and the quarters
/// between them.
///
/// Labelled in code value rather than in IRE, because that is the number every
/// other control in this editor is in — the grading sliders, the readouts, the
/// project format. Two units for one quantity is how a black level ends up set
/// to the wrong one.
fn waveform_graticule(ui: &mut egui::Ui, rect: egui::Rect, mode: WaveformMode) {
    let painter = ui.painter_at(rect);
    for step in 0..=4 {
        let fraction = step as f32 / 4.0;
        let y = rect.bottom() - fraction * rect.height();
        let edge = step == 0 || step == 4;
        painter.line_segment(
            [egui::pos2(rect.left(), y), egui::pos2(rect.right(), y)],
            egui::Stroke::new(1.0, if edge { theme::SEPARATOR } else { theme::GRID }),
        );
        painter.text(
            egui::pos2(rect.left() + 3.0, y),
            egui::Align2::LEFT_CENTER,
            format!("{}", (fraction * 255.0).round() as u32),
            egui::FontId::proportional(9.0),
            theme::TEXT_FAINT,
        );
    }
    if mode == WaveformMode::Parade {
        // The two divisions between the three bands, so it is obvious which
        // trace is which channel without reading the colours.
        for band in 1..3 {
            let x = rect.left() + rect.width() * band as f32 / 3.0;
            painter.line_segment(
                [egui::pos2(x, rect.top()), egui::pos2(x, rect.bottom())],
                egui::Stroke::new(1.0, theme::SEPARATOR),
            );
        }
    }
}

/// The rings and the neutral cross a vectorscope is read against.
///
/// No primary targets drawn: the boxes on a broadcast vectorscope are the
/// positions of 75% colour bars under one standard, and drawing them over a
/// Rec. 709 plot would invite a reading they do not support. The rings say how
/// saturated, which is what the instrument here is for.
fn vectorscope_graticule(ui: &mut egui::Ui, rect: egui::Rect) {
    let painter = ui.painter_at(rect);
    let centre = rect.center();
    let radius = rect.width() * 0.5;
    for ring in 1..=3 {
        painter.circle_stroke(
            centre,
            radius * ring as f32 / 3.0,
            egui::Stroke::new(1.0, theme::GRID),
        );
    }
    let cross = radius * 0.06;
    let stroke = egui::Stroke::new(1.0, theme::SEPARATOR);
    painter.line_segment(
        [centre - egui::vec2(cross, 0.0), centre + egui::vec2(cross, 0.0)],
        stroke,
    );
    painter.line_segment(
        [centre - egui::vec2(0.0, cross), centre + egui::vec2(0.0, cross)],
        stroke,
    );
}

/// The histogram, as three filled curves on one scale.
///
/// Drawn rather than uploaded: 256 bins is a polyline, not a picture, and a
/// polyline stays crisp at any panel width.
fn histogram(ui: &mut egui::Ui, histogram: &Histogram) {
    let available = ui.available_size();
    let (rect, _) = ui.allocate_exact_size(
        egui::vec2(available.x.max(32.0), available.y.max(32.0)),
        egui::Sense::hover(),
    );
    let painter = ui.painter_at(rect);
    painter.rect_filled(rect, theme::RADIUS, theme::SUNKEN);

    let peak = histogram.peak().max(1) as f32;
    let channels: [(&[u32; 256], egui::Color32); 3] = [
        (&histogram.red, egui::Color32::from_rgb(0xe0, 0x5c, 0x5c)),
        (&histogram.green, egui::Color32::from_rgb(0x5c, 0xe0, 0x7c)),
        (&histogram.blue, egui::Color32::from_rgb(0x5c, 0x8c, 0xe0)),
    ];
    for (bins, colour) in channels {
        let points: Vec<egui::Pos2> = bins
            .iter()
            .enumerate()
            .map(|(bin, count)| {
                let x = rect.left() + rect.width() * bin as f32 / 255.0;
                let y = rect.bottom() - rect.height() * (*count as f32 / peak);
                egui::pos2(x, y)
            })
            .collect();
        // Stroked rather than filled, and one curve per channel: three filled
        // areas would hide whichever was drawn first, which is exactly the
        // channel someone reading a histogram is looking for.
        painter.add(egui::Shape::line(points, egui::Stroke::new(1.0, colour)));
    }

    for step in 0..=4 {
        let x = rect.left() + rect.width() * step as f32 / 4.0;
        painter.line_segment(
            [egui::pos2(x, rect.top()), egui::pos2(x, rect.bottom())],
            egui::Stroke::new(1.0, theme::GRID),
        );
    }
}
