//! The application shell: layout, the frame loop, and the GPU bridge.

use std::path::PathBuf;
use std::sync::Arc;

use eframe::CreationContext;
use ve_core::{Project, Size};
use ve_engine::{EngineUpdate, PlaybackClock, PlaybackEngine};
use ve_media::DecodeService;
use ve_metrics::{spans, Metrics};

use crate::actions::{self, Action};
use crate::panels;
use crate::preview::Preview;
use crate::state::{EditorState, Status, StatusLevel};
use crate::theme;

/// Frames cached on the CPU, in megabytes.
const FRAME_CACHE_MB: usize = 512;

pub struct VergeApp {
    state: EditorState,
    engine: PlaybackEngine,
    preview: Option<Preview>,
    metrics: Metrics,
    adapter_name: String,
    software_gpu: bool,
    last_update: Option<EngineUpdate>,
    show_shortcuts: bool,
    /// When the previous frame began, so the overlay can report the interval
    /// the user actually sees rather than the cost of any one stage.
    last_frame_at: Option<std::time::Instant>,
}

impl VergeApp {
    pub fn new(cc: &CreationContext<'_>, open: Option<PathBuf>) -> Self {
        theme::apply(&cc.egui_ctx);

        let metrics = Metrics::new();
        let decode = Arc::new(DecodeService::new(FRAME_CACHE_MB, metrics.clone()));
        let engine =
            PlaybackEngine::new(PlaybackClock::with_system_time(), decode, metrics.clone());

        let scratch = std::env::temp_dir().join("verge-autosave");
        let state = EditorState::new(Project::with_default_sequence("Untitled"), scratch);

        // Share the interface's device rather than creating a second one, so a
        // decoded frame is uploaded once and both the compositor and the UI
        // draw from the same texture.
        let (preview, adapter_name, software_gpu) = match cc.wgpu_render_state.as_ref() {
            Some(render_state) => {
                let size = state
                    .active_sequence()
                    .map(|s| s.settings.resolution)
                    .unwrap_or(Size::new(1920, 1080));
                let info = render_state.adapter.get_info();
                let software = matches!(info.device_type, wgpu::DeviceType::Cpu);
                let name = format!("{} ({:?})", info.name, info.backend);
                (Some(Preview::new(render_state, size, metrics.clone())), name, software)
            }
            None => {
                log::error!("no wgpu render state; the preview will not be available");
                (None, "<no GPU>".to_string(), true)
            }
        };

        let mut app = VergeApp {
            state,
            engine,
            preview,
            metrics,
            adapter_name,
            software_gpu,
            last_update: None,
            show_shortcuts: false,
            last_frame_at: None,
        };

        if let Some(path) = open {
            actions::dispatch(&mut app.state, &mut app.engine, Action::OpenProject(path));
        } else {
            app.state.set_status(Status::info("new project — import media to begin"));
        }
        app
    }

    /// Runs the engine and composites, before any interface is drawn.
    ///
    /// Doing this first means the panels draw a picture that is current for
    /// this frame rather than one frame behind.
    fn render_preview(&mut self, render_state: Option<&egui_wgpu::RenderState>) {
        let Some(sequence) = self.state.active_sequence().cloned() else { return };
        let update = self.engine.update(&sequence);

        if let (Some(preview), Some(render_state)) = (self.preview.as_mut(), render_state) {
            let decode = self.engine.decode_service().clone();
            preview.render(render_state, &update, |clip| {
                decode.key_for(clip.asset, clip.source_time)
            });
        }
        self.last_update = Some(update);
    }

    fn menu_bar(&mut self, root: &mut egui::Ui, actions_out: &mut Vec<Action>) {
        let ctx = root.ctx().clone();
        egui::Panel::top("menu").show(root, |ui| {
            egui::MenuBar::new().ui(ui, |ui| {
                ui.menu_button("File", |ui| {
                    if ui.button("New Project    Ctrl+N").clicked() {
                        actions_out.push(Action::NewProject);
                        ui.close();
                    }
                    if ui.button("Open Project…").clicked() {
                        if let Some(path) = crate::dialogs::pick_project_to_open() {
                            actions_out.push(Action::OpenProject(path));
                        }
                        ui.close();
                    }
                    ui.separator();
                    if ui.button("Save    Ctrl+S").clicked() {
                        match self.state.path.clone() {
                            Some(_) => actions_out.push(Action::SaveProject),
                            None => {
                                if let Some(path) = crate::dialogs::pick_project_to_save(
                                    &self.state.project.name,
                                ) {
                                    actions_out.push(Action::SaveProjectAs(path));
                                }
                            }
                        }
                        ui.close();
                    }
                    if ui.button("Save As…").clicked() {
                        if let Some(path) =
                            crate::dialogs::pick_project_to_save(&self.state.project.name)
                        {
                            actions_out.push(Action::SaveProjectAs(path));
                        }
                        ui.close();
                    }
                    ui.separator();
                    if ui.button("Import Media…    Ctrl+I").clicked() {
                        if let Some(paths) = crate::dialogs::pick_media_files() {
                            actions_out.push(Action::ImportMedia(paths));
                        }
                        ui.close();
                    }
                    ui.separator();
                    if ui.button("Quit").clicked() {
                        ctx.send_viewport_cmd(egui::ViewportCommand::Close);
                    }
                });

                ui.menu_button("Edit", |ui| {
                    let undo = self.state.history.undo_name().map(str::to_string);
                    let redo = self.state.history.redo_name().map(str::to_string);
                    if ui
                        .add_enabled(
                            undo.is_some(),
                            egui::Button::new(match &undo {
                                Some(n) => format!("Undo {n}    Ctrl+Z"),
                                None => "Undo    Ctrl+Z".to_string(),
                            }),
                        )
                        .clicked()
                    {
                        actions_out.push(Action::Undo);
                        ui.close();
                    }
                    if ui
                        .add_enabled(
                            redo.is_some(),
                            egui::Button::new(match &redo {
                                Some(n) => format!("Redo {n}    Ctrl+Shift+Z"),
                                None => "Redo    Ctrl+Shift+Z".to_string(),
                            }),
                        )
                        .clicked()
                    {
                        actions_out.push(Action::Redo);
                        ui.close();
                    }
                    ui.separator();

                    let has_selection = !self.state.selection.clips.is_empty();
                    let has_clipboard = !self.state.clipboard.is_empty();
                    for (label, action, enabled) in [
                        ("Cut    Ctrl+X", Action::Cut, has_selection),
                        ("Copy    Ctrl+C", Action::Copy, has_selection),
                        ("Paste    Ctrl+V", Action::Paste, has_clipboard),
                    ] {
                        if ui.add_enabled(enabled, egui::Button::new(label)).clicked() {
                            actions_out.push(action);
                            ui.close();
                        }
                    }
                    ui.separator();

                    if ui.button("Select All    Ctrl+A").clicked() {
                        actions_out.push(Action::SelectAll);
                        ui.close();
                    }
                    if ui.button("Split at Playhead    Ctrl+K").clicked() {
                        actions_out.push(Action::SplitAtPlayhead);
                        ui.close();
                    }
                    if ui
                        .add_enabled(has_selection, egui::Button::new("Delete    Del"))
                        .clicked()
                    {
                        actions_out.push(Action::DeleteSelected);
                        ui.close();
                    }
                    if ui
                        .add_enabled(
                            has_selection,
                            egui::Button::new("Ripple Delete    Shift+Del"),
                        )
                        .clicked()
                    {
                        actions_out.push(Action::RippleDeleteSelected);
                        ui.close();
                    }
                    if ui.button("Close Gap    Ctrl+Backspace").clicked() {
                        actions_out.push(Action::CloseGapAtPlayhead);
                        ui.close();
                    }
                });

                ui.menu_button("Track", |ui| {
                    if ui.button("Add Video Track").clicked() {
                        actions_out.push(Action::AddTrack(ve_core::TrackKind::Video));
                        ui.close();
                    }
                    if ui.button("Add Audio Track").clicked() {
                        actions_out.push(Action::AddTrack(ve_core::TrackKind::Audio));
                        ui.close();
                    }
                    ui.separator();

                    // Everything below acts on the track last touched in the
                    // timeline, which is also where a paste lands.
                    let target = self.state.selection.track;
                    let name = target
                        .and_then(|t| self.state.active_sequence()?.track(t))
                        .map(|t| t.name.clone());
                    match &name {
                        Some(name) => ui.label(
                            egui::RichText::new(format!("Selected: {name}"))
                                .small()
                                .color(theme::TEXT_DIM),
                        ),
                        None => ui.label(
                            egui::RichText::new("No track selected")
                                .small()
                                .color(theme::TEXT_FAINT),
                        ),
                    };

                    for (label, action) in [
                        (
                            "Move Up",
                            target.map(|t| Action::MoveTrack { track: t, toward_top: true }),
                        ),
                        (
                            "Move Down",
                            target.map(|t| Action::MoveTrack { track: t, toward_top: false }),
                        ),
                        ("Delete Track", target.map(Action::RemoveTrack)),
                    ] {
                        if ui.add_enabled(action.is_some(), egui::Button::new(label)).clicked()
                        {
                            actions_out.extend(action);
                            ui.close();
                        }
                    }
                });

                ui.menu_button("Marker", |ui| {
                    if ui.button("Add Marker    M").clicked() {
                        actions_out.push(Action::AddMarkerAtPlayhead);
                        ui.close();
                    }
                    if ui.button("Previous Marker    Ctrl+←").clicked() {
                        actions_out.push(Action::GoToMarker(-1));
                        ui.close();
                    }
                    if ui.button("Next Marker    Ctrl+→").clicked() {
                        actions_out.push(Action::GoToMarker(1));
                        ui.close();
                    }
                    ui.separator();

                    let markers: Vec<(ve_core::MarkerId, String, String)> = self
                        .state
                        .active_sequence()
                        .map(|seq| {
                            seq.markers
                                .iter()
                                .map(|m| {
                                    (m.id, m.name.clone(), seq.timecode_at(m.time).to_string())
                                })
                                .collect()
                        })
                        .unwrap_or_default();
                    if markers.is_empty() {
                        ui.label(
                            egui::RichText::new("No markers").small().color(theme::TEXT_FAINT),
                        );
                    }
                    for (id, name, timecode) in markers {
                        ui.horizontal(|ui| {
                            ui.label(
                                egui::RichText::new(timecode)
                                    .monospace()
                                    .color(theme::TEXT_DIM),
                            );
                            ui.label(name);
                            if ui.small_button("✕").on_hover_text("Delete marker").clicked() {
                                actions_out.push(Action::RemoveMarker(id));
                            }
                        });
                    }
                });

                ui.menu_button("View", |ui| {
                    if ui.button("Zoom In    +").clicked() {
                        actions_out.push(Action::ZoomIn);
                        ui.close();
                    }
                    if ui.button("Zoom Out    −").clicked() {
                        actions_out.push(Action::ZoomOut);
                        ui.close();
                    }
                    ui.separator();
                    let mut overlay = self.state.show_performance_overlay;
                    if ui.checkbox(&mut overlay, "Performance Overlay").clicked() {
                        actions_out.push(Action::TogglePerformanceOverlay);
                        ui.close();
                    }
                    let mut snapping = self.state.timeline.snapping;
                    if ui.checkbox(&mut snapping, "Snapping").clicked() {
                        actions_out.push(Action::ToggleSnapping);
                        ui.close();
                    }
                });

                ui.menu_button("Help", |ui| {
                    if ui.button("Keyboard Shortcuts").clicked() {
                        self.show_shortcuts = true;
                        ui.close();
                    }
                });

                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if self.state.history.is_dirty() {
                        ui.label(
                            egui::RichText::new("● unsaved").small().color(theme::WARNING),
                        );
                    }
                });
            });
        });
    }

    fn status_bar(&mut self, root: &mut egui::Ui) {
        egui::Panel::bottom("status").show(root, |ui| {
            ui.horizontal(|ui| {
                match &self.state.status {
                    Some(status) => {
                        let colour = match status.level {
                            StatusLevel::Info => theme::TEXT_DIM,
                            StatusLevel::Warning => theme::WARNING,
                            StatusLevel::Error => theme::ERROR,
                        };
                        ui.label(egui::RichText::new(&status.text).small().color(colour));
                    }
                    None => {
                        ui.label(egui::RichText::new("Ready").small().color(theme::TEXT_FAINT));
                    }
                }

                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    let clips = self.state.project.clip_count();
                    let assets = self.state.project.assets.len();
                    ui.label(
                        egui::RichText::new(format!("{assets} media · {clips} clips"))
                            .small()
                            .color(theme::TEXT_FAINT),
                    );
                    if !self.state.warnings.is_empty() {
                        let n = self.state.warnings.len();
                        ui.label(
                            egui::RichText::new(format!("⚠ {n}")).small().color(theme::WARNING),
                        )
                        .on_hover_text(self.state.warnings.join("\n"));
                    }
                });
            });
        });
    }

    fn shortcuts_window(&mut self, ctx: &egui::Context) {
        if !self.show_shortcuts {
            return;
        }
        let mut open = true;
        egui::Window::new("Keyboard Shortcuts")
            .open(&mut open)
            .collapsible(false)
            .resizable(false)
            .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
            .show(ctx, |ui| {
                egui::Grid::new("shortcuts").num_columns(2).spacing([24.0, 4.0]).show(
                    ui,
                    |ui| {
                        for binding in crate::shortcuts::BINDINGS {
                            ui.label(binding.label);
                            ui.label(
                                egui::RichText::new(binding.keys)
                                    .monospace()
                                    .color(theme::TEXT_DIM),
                            );
                            ui.end_row();
                        }
                    },
                );
            });
        self.show_shortcuts = open;
    }

    /// Writes an autosave when one is due.
    fn maybe_autosave(&mut self) {
        if !self.state.autosave.should_save() {
            return;
        }
        match self.state.autosave.save(&self.state.project) {
            Ok(path) => log::debug!("autosaved to {}", path.display()),
            Err(e) => self.state.set_status(Status::warning(format!("autosave failed: {e}"))),
        }
    }
}

impl eframe::App for VergeApp {
    fn ui(&mut self, root: &mut egui::Ui, frame: &mut eframe::Frame) {
        let ui_span = self.metrics.span(spans::UI);
        let ctx = root.ctx().clone();

        // The interval between repaints is the honest frame time: it includes
        // every stage plus whatever the windowing system made us wait for.
        let now = std::time::Instant::now();
        if let Some(previous) = self.last_frame_at.replace(now) {
            self.metrics.record(spans::FRAME, now.duration_since(previous));
        }

        // 1. Engine and GPU, before any interface is drawn.
        let render_state = frame.wgpu_render_state().cloned();
        self.render_preview(render_state.as_ref());

        let update = self.last_update.as_ref();
        let position = update.map(|u| u.position).unwrap_or_default();
        let playing = update.map(|u| u.playing).unwrap_or(false);
        let pending = update.map(|u| u.pending).unwrap_or(0);
        let visible = update.map(|u| u.layers.len()).unwrap_or(0);

        let mut pending_actions = Vec::new();

        // 2. Keyboard, unless a text field has the keyboard.
        if !ctx.egui_wants_keyboard_input() {
            let width = root.max_rect().width();
            pending_actions.extend(crate::shortcuts::collect(&ctx, width));
        }

        // 3. Interface.
        self.menu_bar(root, &mut pending_actions);
        self.status_bar(root);

        egui::Panel::bottom("timeline")
            .resizable(true)
            .default_size(260.0)
            .min_size(120.0)
            .show(root, |ui| {
                panels::timeline::show(ui, &mut self.state, position, &mut pending_actions);
            });

        egui::Panel::left("project").resizable(true).default_size(236.0).min_size(160.0).show(
            root,
            |ui| {
                panels::project_panel::show(ui, &mut self.state, &mut pending_actions);
            },
        );

        egui::Panel::right("inspector")
            .resizable(true)
            .default_size(248.0)
            .min_size(180.0)
            .show(root, |ui| {
                panels::inspector::show(ui, &self.state, &mut pending_actions);
            });

        // The darkest surface surrounds the picture, so the preview is judged
        // against the most neutral thing on screen.
        egui::CentralPanel::default()
            .frame(
                egui::Frame::new().fill(theme::BACKGROUND).inner_margin(egui::Margin::same(6)),
            )
            .show(root, |ui| {
                let info = panels::preview_panel::PreviewInfo {
                    texture: self.preview.as_ref().and_then(|p| p.texture_id()),
                    size: self
                        .preview
                        .as_ref()
                        .map(|p| p.size())
                        .unwrap_or(Size::new(1920, 1080)),
                    position,
                    playing,
                    pending_layers: pending,
                    visible_layers: visible,
                };
                panels::preview_panel::show(ui, &self.state, &info, &mut pending_actions);
                panels::preview_panel::scrub_bar(
                    ui,
                    &self.state,
                    position,
                    &mut pending_actions,
                );
            });

        self.shortcuts_window(&ctx);

        if self.state.show_performance_overlay {
            let input = panels::overlay::OverlayInput {
                metrics: &self.metrics,
                frame_cache: self.engine.decode_service().cache_stats(),
                gpu_texture_bytes: self
                    .preview
                    .as_ref()
                    .map(|p| p.texture_bytes())
                    .unwrap_or(0),
                gpu_texture_count: self
                    .preview
                    .as_ref()
                    .map(|p| p.texture_count())
                    .unwrap_or(0),
                adapter: &self.adapter_name,
                software_gpu: self.software_gpu,
                clip_count: self.state.project.clip_count(),
                visible_layers: visible,
            };
            panels::overlay::show(&ctx, &input);
        }

        // 4. Apply everything the frame asked for, in one place.
        for action in pending_actions {
            actions::dispatch(&mut self.state, &mut self.engine, action);
        }

        // 5. Housekeeping.
        self.maybe_autosave();
        ctx.send_viewport_cmd(egui::ViewportCommand::Title(self.state.window_title()));

        // Follow the playhead during playback, so it does not run off screen.
        if playing {
            let width = root.max_rect().width() - self.state.timeline.header_width;
            self.state.timeline.scroll_to_show(position, width.max(100.0));
        }

        drop(ui_span);

        // Repaint continuously while playing or while frames are still
        // decoding; otherwise egui sleeps until the next input, which is what
        // keeps an idle editor off the CPU entirely.
        if playing || pending > 0 {
            ctx.request_repaint();
        }
    }

    fn save(&mut self, _storage: &mut dyn eframe::Storage) {
        // An unsaved project is protected by the autosave rather than by
        // silently writing over the user's file on exit.
        if self.state.history.is_dirty() {
            let _ = self.state.autosave.save(&self.state.project);
        }
    }
}
