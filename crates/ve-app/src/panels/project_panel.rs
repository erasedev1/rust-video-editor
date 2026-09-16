//! The media browser: what has been imported, and what it is.

use egui::{Align, Layout, RichText, Sense, Ui};
use ve_core::TrackKind;

use crate::actions::Action;
use crate::state::EditorState;
use crate::theme;

pub fn show(ui: &mut Ui, state: &mut EditorState, actions: &mut Vec<Action>) {
    ui.horizontal(|ui| {
        ui.label(RichText::new("PROJECT").small().color(theme::TEXT_FAINT));
        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
            if ui.small_button("Import…").on_hover_text("Import media (Ctrl+I)").clicked() {
                if let Some(paths) = crate::dialogs::pick_media_files() {
                    actions.push(Action::ImportMedia(paths));
                }
            }
        });
    });
    ui.separator();

    if state.project.assets.is_empty() {
        ui.add_space(12.0);
        ui.vertical_centered(|ui| {
            ui.label(RichText::new("No media yet").color(theme::TEXT_DIM));
            ui.label(
                RichText::new("Import a video or audio file to begin")
                    .small()
                    .color(theme::TEXT_FAINT),
            );
        });
        return;
    }

    let selected_asset = state.selection.asset;
    let target_track = default_track_for(state);

    egui::ScrollArea::vertical().auto_shrink([false, false]).show(ui, |ui| {
        let assets: Vec<_> = state.project.assets.iter().map(|a| a.id).collect();
        for asset_id in assets {
            let Some(asset) = state.project.asset(asset_id) else { continue };
            let is_selected = selected_asset == Some(asset_id);

            let name = asset.name.clone();
            let offline = asset.offline;
            let duration = asset.duration();
            let summary = summarise(asset);

            let response = ui
                .scope(|ui| {
                    let fill =
                        if is_selected { theme::ACCENT_DIM } else { theme::PANEL_RAISED };
                    egui::Frame::new()
                        .fill(fill)
                        .corner_radius(theme::RADIUS)
                        .inner_margin(egui::Margin::symmetric(6, 4))
                        .show(ui, |ui| {
                            ui.set_width(ui.available_width());
                            ui.vertical(|ui| {
                                ui.horizontal(|ui| {
                                    if offline {
                                        ui.label(
                                            RichText::new("⚠").color(theme::OFFLINE).small(),
                                        )
                                        .on_hover_text("media is missing");
                                    }
                                    ui.label(RichText::new(&name).color(theme::TEXT));
                                });
                                ui.label(
                                    RichText::new(summary).small().color(theme::TEXT_FAINT),
                                );
                            });
                        });
                })
                .response
                .interact(Sense::click());

            if response.clicked() {
                state.selection.asset = Some(asset_id);
            }
            if response.double_clicked() {
                if let Some(track) = target_track {
                    // Double-click appends at the playhead, the quickest path
                    // from "imported" to "on the timeline".
                    let at = state.active_sequence().map(|s| s.playhead).unwrap_or_default();
                    actions.push(Action::AddAssetToTimeline { asset: asset_id, track, at });
                }
            }
            response.on_hover_text(format!(
                "{name}\n{}\ndouble-click to add at the playhead",
                duration
            ));
            ui.add_space(2.0);
        }
    });
}

/// A one-line summary of what a file contains.
fn summarise(asset: &ve_core::MediaAsset) -> String {
    let mut parts = Vec::new();
    parts.push(format!("{}", asset.duration()));
    if let Some(v) = &asset.info.video {
        parts.push(format!("{}×{}", v.size.width, v.size.height));
        parts.push(format!("{}", v.rate));
    }
    if let Some(a) = &asset.info.audio {
        parts.push(format!("{} ch {} kHz", a.channels, a.sample_rate.hz() as f64 / 1000.0));
    }
    parts.join("  ·  ")
}

/// Where a double-clicked asset should land: the selected track if there is
/// one, otherwise the first video track.
fn default_track_for(state: &EditorState) -> Option<ve_core::TrackId> {
    let seq = state.active_sequence()?;
    if let Some(track) = state.selection.track {
        if seq.track(track).is_some() {
            return Some(track);
        }
    }
    seq.tracks.iter().find(|t| t.kind == TrackKind::Video).map(|t| t.id)
}
