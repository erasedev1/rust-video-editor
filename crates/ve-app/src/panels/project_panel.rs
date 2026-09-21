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
    proxy_bar(ui, state, actions);
    multicam_bar(ui, state, actions);
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
    let using_proxies = state.project.settings.use_proxies;

    egui::ScrollArea::vertical().auto_shrink([false, false]).show(ui, |ui| {
        let assets: Vec<_> = state.project.assets.iter().map(|a| a.id).collect();
        for asset_id in assets {
            let Some(asset) = state.project.asset(asset_id) else { continue };
            let is_selected = selected_asset == Some(asset_id);

            let name = asset.name.clone();
            let offline = asset.offline;
            let duration = asset.duration();
            let summary = summarise(asset);
            let proxy_badge = proxy_badge(asset, using_proxies);
            let has_video = asset.info.has_video();
            let picked = state.multicam_picks.contains(&asset_id);
            let mut picked_now = false;

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
                                    // The tick that gathers cameras for a
                                    // multicam group. Only offered for media
                                    // that has a picture: an angle is a camera.
                                    if has_video {
                                        // A real checkbox rather than a glyph:
                                        // egui draws its own box, so it cannot
                                        // come out as a missing-character
                                        // rectangle on a system whose font has
                                        // no ballot symbols.
                                        let mut on = picked;
                                        if ui
                                            .add(egui::Checkbox::without_text(&mut on))
                                            .on_hover_text("include in the next multicam group")
                                            .clicked()
                                        {
                                            picked_now = true;
                                        }
                                    }
                                    if offline {
                                        ui.label(
                                            RichText::new("⚠").color(theme::OFFLINE).small(),
                                        )
                                        .on_hover_text("media is missing");
                                    }
                                    ui.label(RichText::new(&name).color(theme::TEXT));
                                    if let Some(badge) = &proxy_badge {
                                        ui.label(
                                            RichText::new(&badge.text)
                                                .small()
                                                .color(badge.colour),
                                        )
                                        .on_hover_text(&badge.hover);
                                    }
                                });
                                ui.label(
                                    RichText::new(summary).small().color(theme::TEXT_FAINT),
                                );
                            });
                        });
                })
                .response
                .interact(Sense::click());

            if picked_now {
                actions.push(Action::ToggleMulticamPick(asset_id));
            } else if response.clicked() {
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

/// What the proxy row above the media list says, and the two things it does.
///
/// In the panel rather than only in a menu because the state it reports —
/// whether the pictures on screen are the real ones — is the sort of thing a
/// user needs to be able to see rather than remember.
fn proxy_bar(ui: &mut Ui, state: &EditorState, actions: &mut Vec<Action>) {
    let running = state.proxies.is_running();
    let with_video = state.project.assets.iter().filter(|a| a.info.has_video()).count();
    if with_video == 0 && !running {
        return;
    }

    ui.horizontal(|ui| {
        if running {
            let progress = state.proxies.job.as_ref().map(|j| j.progress());
            let fraction = progress.map(|p| p.fraction()).unwrap_or(0.0);
            let (item, items) = progress.map(|p| (p.item, p.items)).unwrap_or((0, 0));
            ui.add(
                egui::ProgressBar::new(fraction)
                    .desired_width(110.0)
                    .text(RichText::new(format!("proxy {item}/{items}")).small()),
            );
            if ui.small_button("Stop").clicked() {
                actions.push(Action::CancelProxyBuild);
            }
            return;
        }

        // How many of the video assets can actually be decoded through right
        // now — an attached proxy whose file has gone does not count, which is
        // the number that matters and the one a plain `has_proxy` would get
        // wrong.
        let ready = state
            .project
            .assets
            .iter()
            .filter(|a| a.info.has_video() && a.picture_source(None, true).is_proxy)
            .count();

        let mut using = state.project.settings.use_proxies;
        let toggle = ui
            .add_enabled(
                ready > 0,
                egui::Checkbox::new(&mut using, RichText::new("Proxies").small()),
            )
            .on_hover_text(if ready > 0 {
                "Cut with the smaller stand-ins. Exports always render the originals."
            } else {
                "Build proxies first"
            })
            .on_disabled_hover_text("Build proxies first");
        if toggle.changed() {
            actions.push(Action::SetUseProxies(using));
        }

        ui.label(
            RichText::new(format!("{ready}/{with_video}"))
                .small()
                .color(if ready == with_video { theme::TEXT_FAINT } else { theme::TEXT_DIM }),
        )
        .on_hover_text("How much of the footage has a proxy on disk");

        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
            if ui
                .small_button("Build")
                .on_hover_text("Build proxies for anything that has not got one")
                .clicked()
            {
                actions.push(Action::BuildProxies);
            }
        });
    });
}

/// The small mark beside an asset's name saying where its picture comes from.
struct ProxyBadge {
    text: String,
    colour: egui::Color32,
    hover: String,
}

fn proxy_badge(asset: &ve_core::MediaAsset, using_proxies: bool) -> Option<ProxyBadge> {
    if !asset.info.has_video() || !asset.has_proxy() {
        return None;
    }
    let proxy = asset.proxy.as_ref()?;
    let on_disk = asset.picture_source(None, true).is_proxy;

    // Three states worth telling apart, because the user's next move differs
    // for each: in use, built but switched off, and referenced but gone.
    Some(if !on_disk {
        ProxyBadge {
            text: "proxy?".into(),
            colour: theme::OFFLINE,
            hover: format!(
                "{} is missing, so this clip is decoding at full resolution.\nBuild \
                 proxies again to replace it.",
                proxy.path.display()
            ),
        }
    } else if using_proxies {
        ProxyBadge {
            text: format!("proxy {}p", proxy.size.height),
            colour: theme::ACCENT,
            hover: format!("cutting on {}", proxy.path.display()),
        }
    } else {
        ProxyBadge {
            text: "proxy".into(),
            colour: theme::TEXT_FAINT,
            hover: format!("{} is built but switched off", proxy.path.display()),
        }
    })
}

/// The multicam groups this project has, and the one being assembled.
///
/// Sits with the media rather than on the timeline because a group is a fact
/// about the footage — which cameras covered the same event — rather than about
/// any particular cut of it.
fn multicam_bar(ui: &mut Ui, state: &EditorState, actions: &mut Vec<Action>) {
    let picks = state.multicam_picks.len();
    if picks == 0 && state.project.multicams.is_empty() {
        return;
    }

    ui.add_space(2.0);
    if picks > 0 {
        ui.horizontal(|ui| {
            ui.label(RichText::new(format!("{picks} ticked")).small().color(theme::ACCENT));
            if ui
                .add_enabled(picks >= 2, egui::Button::new("Group"))
                .on_hover_text("Make a multicam group from the ticked media")
                .on_disabled_hover_text("Tick at least two cameras")
                .clicked()
            {
                actions.push(Action::CreateMulticamGroup);
            }
        });
    }

    let target_track = default_track_for(state);
    for group in &state.project.multicams {
        let id = group.id;
        egui::Frame::new()
            .fill(theme::PANEL_RAISED)
            .corner_radius(theme::RADIUS)
            .inner_margin(egui::Margin::symmetric(6, 4))
            .show(ui, |ui| {
                ui.set_width(ui.available_width());
                ui.horizontal(|ui| {
                    ui.label(RichText::new(&group.name).small().color(theme::TEXT));
                    ui.label(
                        RichText::new(format!(
                            "{} cams · {}",
                            group.len(),
                            group.sync.label().to_lowercase()
                        ))
                        .small()
                        .color(theme::TEXT_FAINT),
                    );
                });
                ui.horizontal(|ui| {
                    if ui
                        .small_button("Sync ♪")
                        .on_hover_text("Measure the offsets by correlating the cameras' sound")
                        .clicked()
                    {
                        actions.push(Action::SyncMulticam {
                            group: id,
                            method: ve_core::SyncMethod::Audio,
                        });
                    }
                    if ui
                        .small_button("Sync TC")
                        .on_hover_text("Use the start timecode each file records")
                        .clicked()
                    {
                        actions.push(Action::SyncMulticam {
                            group: id,
                            method: ve_core::SyncMethod::Timecode,
                        });
                    }
                    if ui
                        .add_enabled(target_track.is_some(), egui::Button::new("+"))
                        .on_hover_text("Add to the timeline at the playhead")
                        .clicked()
                    {
                        if let Some(track) = target_track {
                            let at =
                                state.active_sequence().map(|s| s.playhead).unwrap_or_default();
                            actions.push(Action::AddMulticamToTimeline {
                                group: id,
                                track,
                                at,
                            });
                        }
                    }
                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                        if ui.small_button("×").on_hover_text("Delete this group").clicked() {
                            actions.push(Action::RemoveMulticamGroup(id));
                        }
                    });
                });
            });
        ui.add_space(2.0);
    }
}
