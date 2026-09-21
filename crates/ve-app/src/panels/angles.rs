//! The angle viewer: every camera in a group, at the same instant, in a grid.
//!
//! # Why these frames do not go through the compositor
//!
//! The preview's picture is composited on the GPU and handed to egui as one
//! native texture, because it is one picture at sequence resolution with
//! transforms, effects and blending to apply. A viewer tile is none of those
//! things: it is a thumbnail of one decoded frame, with nothing done to it.
//!
//! So each tile is uploaded as an ordinary egui texture instead. That costs a
//! CPU-side copy per tile per *changed* frame — eight tiles at thumbnail size is
//! well under a megabyte — and in exchange the viewer needs no render target, no
//! pipeline and no second path through the compositor. The frames themselves are
//! already decoded: they come from the same cache and the same scheduler the
//! preview uses.
//!
//! # What a tile says when it has nothing
//!
//! Cameras start and stop at different moments, so an angle routinely has no
//! frame at a given instant. The tile stays where it is, keeps its number, and
//! says so. Removing it would renumber every tile after it under the user's
//! fingers, part way through a shot — which is the one thing a viewer must never
//! do, because the numbers are the controls.

use std::collections::HashMap;

use egui::{Color32, ColorImage, RichText, TextureHandle, TextureOptions, Ui};
use ve_core::AssetId;
use ve_engine::MulticamAt;
use ve_media::{DecodeService, FrameRequest, VideoFrame};
use ve_time::Ticks;

use crate::actions::Action;
use crate::state::EditorState;
use crate::theme;

/// The longest edge a tile's texture is uploaded at.
///
/// A viewer tile is looked at to judge framing and action, not focus. Uploading
/// full frames would cost a megabyte apiece and tell the user nothing more.
const TILE_PIXELS: usize = 320;

/// How many tiles fit across before the grid wraps. Four covers the common
/// shoot; past that the tiles shrink rather than the grid growing sideways.
const MAX_COLUMNS: usize = 4;

/// Textures for the tiles, kept between frames.
///
/// Keyed on the asset with the instant it holds, so a tile is only re-uploaded
/// when the frame behind it actually changed — which during playback is once a
/// frame, and while parked is never.
#[derive(Default)]
pub struct AngleTextures {
    tiles: HashMap<AssetId, (Ticks, TextureHandle)>,
}

impl AngleTextures {
    /// Drops textures for cameras that are no longer on screen.
    fn retain(&mut self, live: &[AssetId]) {
        self.tiles.retain(|asset, _| live.contains(asset));
    }

    fn upload(
        &mut self,
        ctx: &egui::Context,
        asset: AssetId,
        at: Ticks,
        frame: &VideoFrame,
    ) -> TextureHandle {
        if let Some((held, texture)) = self.tiles.get(&asset) {
            if *held == at {
                return texture.clone();
            }
        }
        let image = thumbnail(frame);
        let handle = ctx.load_texture(format!("angle-{asset}"), image, TextureOptions::LINEAR);
        self.tiles.insert(asset, (at, handle.clone()));
        handle
    }

    pub fn len(&self) -> usize {
        self.tiles.len()
    }

    pub fn is_empty(&self) -> bool {
        self.tiles.is_empty()
    }
}

/// Reduces a decoded frame to a tile-sized RGBA image.
///
/// Nearest-neighbour by a whole-number step, which is all a thumbnail needs and
/// which costs one read per output pixel rather than a filtered gather.
fn thumbnail(frame: &VideoFrame) -> ColorImage {
    let (width, height) = (frame.width() as usize, frame.height() as usize);
    if width == 0 || height == 0 {
        return ColorImage::new([1, 1], vec![Color32::BLACK]);
    }
    let step = (width.max(height) / TILE_PIXELS).max(1);
    let (out_w, out_h) = ((width / step).max(1), (height / step).max(1));

    let mut pixels = Vec::with_capacity(out_w * out_h);
    for y in 0..out_h {
        let row = frame.row((y * step) as u32);
        for x in 0..out_w {
            let i = x * step * 4;
            match row.get(i..i + 4) {
                Some(p) => {
                    pixels.push(Color32::from_rgba_premultiplied(p[0], p[1], p[2], p[3]))
                }
                None => pixels.push(Color32::BLACK),
            }
        }
    }
    ColorImage::new([out_w, out_h], pixels)
}

/// Draws the viewer, asking for whatever it needs decoded.
///
/// Requests are posted at interactive priority, the same as the frame under the
/// playhead: the viewer is what the user is looking at while cutting, and a tile
/// that arrives after the cut has been made is of no use.
pub fn show(
    ui: &mut Ui,
    state: &EditorState,
    view: &MulticamAt,
    decode: &DecodeService,
    textures: &mut AngleTextures,
    actions: &mut Vec<Action>,
) {
    let group_name = state
        .project
        .multicam(view.group)
        .map(|g| g.name.clone())
        .unwrap_or_else(|| "Multicam".into());

    ui.horizontal(|ui| {
        ui.label(RichText::new("ANGLES").small().color(theme::TEXT_FAINT));
        ui.label(RichText::new(&group_name).small().color(theme::TEXT_DIM));
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            ui.label(
                RichText::new("1-9 cuts · shift switches").small().color(theme::TEXT_FAINT),
            );
        });
    });

    let live: Vec<AssetId> = view.angles.iter().map(|a| a.asset).collect();
    textures.retain(&live);

    let count = view.angles.len().max(1);
    let columns = count.min(MAX_COLUMNS);
    let rows = count.div_ceil(columns);

    let available = ui.available_size();
    let spacing = 4.0;
    let tile_w = ((available.x - spacing * (columns as f32 - 1.0)) / columns as f32).max(40.0);
    let tile_h = ((available.y - 18.0 - spacing * (rows as f32 - 1.0)) / rows as f32).max(30.0);

    let shift = ui.input(|i| i.modifiers.shift);
    let ctx = ui.ctx().clone();

    egui::Grid::new("angle-grid").spacing([spacing, spacing]).show(ui, |ui| {
        for (index, angle) in view.angles.iter().enumerate() {
            let (rect, response) =
                ui.allocate_exact_size(egui::vec2(tile_w, tile_h), egui::Sense::click());
            let painter = ui.painter_at(rect);
            painter.rect_filled(rect, theme::RADIUS, theme::SUNKEN);

            // The frame, if it is decoded; a request for it if it is not.
            let picture = match angle.request() {
                Some((asset, at)) => match decode.cached_frame(asset, at) {
                    Some(frame) => Some(textures.upload(&ctx, asset, at, &frame)),
                    None => {
                        decode.request(FrameRequest::interactive(asset, at));
                        None
                    }
                },
                None => None,
            };

            match &picture {
                Some(texture) => {
                    let fitted = fit(rect.shrink(2.0), texture.size_vec2());
                    painter.image(
                        texture.id(),
                        fitted,
                        egui::Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(1.0, 1.0)),
                        if angle.enabled { Color32::WHITE } else { theme::TEXT_FAINT },
                    );
                }
                None => {
                    painter.text(
                        rect.center(),
                        egui::Align2::CENTER_CENTER,
                        if angle.source_time.is_none() { "not rolling" } else { "…" },
                        egui::FontId::proportional(10.0),
                        theme::TEXT_FAINT,
                    );
                }
            }

            // The number, which is the control: a tile is clicked or its number
            // is pressed, and they have to agree about which camera that is.
            let badge = egui::Rect::from_min_size(
                rect.left_top() + egui::vec2(4.0, 4.0),
                egui::vec2(16.0, 14.0),
            );
            painter.rect_filled(badge, 2, Color32::from_black_alpha(190));
            painter.text(
                badge.center(),
                egui::Align2::CENTER_CENTER,
                angle.number.to_string(),
                egui::FontId::monospace(10.0),
                if angle.enabled { theme::TEXT } else { theme::TEXT_FAINT },
            );
            if tile_w > 90.0 {
                painter.text(
                    rect.left_bottom() + egui::vec2(5.0, -4.0),
                    egui::Align2::LEFT_BOTTOM,
                    crate::panels::timeline::elide(&angle.name, tile_w - 10.0),
                    egui::FontId::proportional(9.5),
                    theme::TEXT_DIM,
                );
            }

            // The camera on screen is outlined, so the grid says what the
            // preview is showing without the user having to compare pictures.
            let (stroke, width) = if angle.active {
                (theme::ACCENT, 2.0)
            } else if response.hovered() {
                (theme::TEXT_DIM, 1.0)
            } else {
                (theme::SEPARATOR, 1.0)
            };
            painter.rect_stroke(
                rect,
                theme::RADIUS,
                egui::Stroke::new(width, stroke),
                egui::StrokeKind::Inside,
            );

            if response.clicked() && angle.enabled {
                actions.push(if shift {
                    Action::SwitchToAngle(angle.number)
                } else {
                    Action::CutToAngle(angle.number)
                });
            }
            response.on_hover_text(if angle.enabled {
                format!("{} — click to cut, shift-click to switch", angle.name)
            } else {
                format!("{} is switched off", angle.name)
            });

            if (index + 1) % columns == 0 {
                ui.end_row();
            }
        }
    });
}

/// The largest rectangle of `size`'s aspect that fits inside `within`.
fn fit(within: egui::Rect, size: egui::Vec2) -> egui::Rect {
    if size.x <= 0.0 || size.y <= 0.0 {
        return within;
    }
    let scale = (within.width() / size.x).min(within.height() / size.y);
    egui::Rect::from_center_size(within.center(), size * scale)
}

#[cfg(test)]
mod tests {
    use super::*;
    use ve_core::Size;

    fn frame(width: u32, height: u32) -> VideoFrame {
        let data: Vec<u8> = (0..(width * height * 4)).map(|i| (i % 255) as u8).collect();
        VideoFrame::new(
            data.into(),
            Size::new(width, height),
            width * 4,
            ve_media::PixelFormat::Rgba8,
            Ticks::ZERO,
        )
    }

    #[test]
    fn a_thumbnail_is_reduced_to_around_the_tile_size() {
        let image = thumbnail(&frame(1920, 1080));
        assert!(image.size[0] <= TILE_PIXELS, "{:?}", image.size);
        assert!(image.size[0] > TILE_PIXELS / 2, "and not reduced further than it has to be");
        // The aspect survives: 1920/1080 is 16:9, and so is the thumbnail.
        let aspect = image.size[0] as f32 / image.size[1] as f32;
        assert!((aspect - 16.0 / 9.0).abs() < 0.1, "aspect {aspect}");
    }

    #[test]
    fn a_frame_already_smaller_than_a_tile_is_not_enlarged() {
        let image = thumbnail(&frame(160, 90));
        assert_eq!(image.size, [160, 90]);
    }

    #[test]
    fn a_degenerate_frame_does_not_panic() {
        let image = thumbnail(&frame(0, 0));
        assert_eq!(image.size, [1, 1]);
    }

    #[test]
    fn fitting_keeps_the_aspect_and_stays_inside() {
        let within = egui::Rect::from_min_size(egui::pos2(0.0, 0.0), egui::vec2(100.0, 100.0));
        let fitted = fit(within, egui::vec2(160.0, 90.0));
        assert!(fitted.width() <= within.width() + 0.01);
        assert!(fitted.height() <= within.height() + 0.01);
        let aspect = fitted.width() / fitted.height();
        assert!((aspect - 160.0 / 90.0).abs() < 0.01, "aspect {aspect}");
        assert!((fitted.center() - within.center()).length() < 0.01);
    }

    #[test]
    fn fitting_a_nothing_sized_picture_falls_back_to_the_whole_tile() {
        let within = egui::Rect::from_min_size(egui::pos2(0.0, 0.0), egui::vec2(100.0, 50.0));
        assert_eq!(fit(within, egui::vec2(0.0, 0.0)), within);
    }
}
