//! The export dialogue: what to write, and how far it has got.
//!
//! Not modal. The window sits over the editor and the editor keeps running
//! underneath it — playing, scrubbing, being edited — because an export of
//! anything long takes minutes, and an editor that cannot be used while one
//! runs is an editor that cannot be used.
//!
//! Every control writes into the draft settings held in
//! [`crate::state::ExportState`] and nothing else; pressing Export turns that
//! draft into one [`Action::StartExport`]. So the whole dialogue is a view of a
//! value, and exporting without it — from a test, or from a script later — is
//! the same call with the same value.

use egui::{Context, RichText, Ui};
use ve_export::{AudioCodec, AudioSettings, ExportRange, ExportSettings, Quality, VideoCodec};
use ve_time::Rate;

use crate::actions::Action;
use crate::state::EditorState;
use crate::theme;

/// Draws the dialogue if it is open, and the progress of a running export.
pub fn show(ctx: &Context, state: &EditorState, actions: &mut Vec<Action>) {
    if !state.export.open {
        return;
    }
    let Some(settings) = state.export.settings.clone() else { return };
    let Some(sequence) = state.active_sequence() else { return };

    let mut open = true;
    let mut draft = settings.clone();
    let running = state.export.is_running();

    egui::Window::new("Export")
        .open(&mut open)
        .collapsible(false)
        .resizable(false)
        .default_width(420.0)
        .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
        .show(ctx, |ui| {
            if running {
                progress_section(ui, state, actions);
                ui.separator();
                // The settings are still shown while a render runs, greyed out:
                // what is being written is worth being able to read.
                ui.add_enabled_ui(false, |ui| {
                    settings_body(ui, &mut draft, sequence, true);
                });
                return;
            }

            settings_body(ui, &mut draft, sequence, false);

            if let Some(last) = &state.export.last {
                ui.add_space(6.0);
                ui.label(RichText::new(last).small().color(theme::TEXT_DIM));
            }

            ui.add_space(10.0);
            ui.horizontal(|ui| {
                let problem = draft.validate().err();
                if ui
                    .add_enabled(
                        problem.is_none(),
                        egui::Button::new(RichText::new("Export").color(theme::TEXT)),
                    )
                    .clicked()
                {
                    actions.push(Action::StartExport(Box::new(draft.clone())));
                }
                if ui.button("Cancel").clicked() {
                    actions.push(Action::CloseExportDialog);
                }
                if let Some(problem) = problem {
                    ui.label(RichText::new(problem.to_string()).small().color(theme::ERROR));
                }
            });
        });

    if !open {
        actions.push(Action::CloseExportDialog);
    }

    // The dialogue edits a copy, so a change becomes state in one place rather
    // than at every control that could have made it.
    if draft != settings {
        actions.push(Action::SetExportSettings(Box::new(draft)));
    }
}

/// Every setting, in the order someone fills them in.
fn settings_body(
    ui: &mut Ui,
    draft: &mut ExportSettings,
    sequence: &ve_core::Sequence,
    running: bool,
) {
    egui::Grid::new("export-settings")
        .num_columns(2)
        .spacing([12.0, 8.0])
        .min_col_width(84.0)
        .show(ui, |ui| {
            ui.label(RichText::new("File").color(theme::TEXT_DIM));
            ui.horizontal(|ui| {
                let name = draft
                    .path
                    .file_name()
                    .map(|n| n.to_string_lossy().into_owned())
                    .unwrap_or_default();
                ui.label(RichText::new(name).monospace())
                    .on_hover_text(draft.path.display().to_string());
                if !running && ui.small_button("Choose…").clicked() {
                    if let Some(path) = crate::dialogs::pick_export_file(&draft.path) {
                        draft.path = path;
                    }
                }
            });
            ui.end_row();

            ui.label(RichText::new("Format").color(theme::TEXT_DIM));
            ui.horizontal(|ui| {
                egui::ComboBox::from_id_salt("export-codec")
                    .selected_text(draft.video.label())
                    .width(120.0)
                    .show_ui(ui, |ui| {
                        for codec in [VideoCodec::H264, VideoCodec::H265, VideoCodec::ProRes] {
                            if ui
                                .selectable_label(draft.video == codec, codec.label())
                                .clicked()
                            {
                                draft.video = codec;
                                // ProRes does not go in an MP4 and the dialogue
                                // knows that, so the container follows the codec
                                // rather than waiting to be told it is wrong.
                                if !draft.container().is_some_and(|c| c.accepts(codec)) {
                                    draft.path.set_extension(match codec {
                                        VideoCodec::ProRes => "mov",
                                        _ => "mp4",
                                    });
                                }
                            }
                        }
                    });
                if let Some(container) = draft.container() {
                    ui.label(
                        RichText::new(format!(".{}", container.extension()))
                            .small()
                            .color(theme::TEXT_FAINT),
                    );
                }
            });
            ui.end_row();

            if draft.video.uses_bitrate() {
                ui.label(RichText::new("Quality").color(theme::TEXT_DIM));
                ui.horizontal(|ui| {
                    for quality in [Quality::Draft, Quality::Standard, Quality::High] {
                        if ui
                            .selectable_label(draft.quality == quality, quality.label())
                            .clicked()
                        {
                            draft.quality = quality;
                        }
                    }
                    let mut custom = matches!(draft.quality, Quality::Custom(_));
                    if ui.selectable_label(custom, "Custom").clicked() && !custom {
                        draft.quality = Quality::Custom(draft.bitrate() / 1000);
                        custom = true;
                    }
                    if custom {
                        let mut kbps = match draft.quality {
                            Quality::Custom(k) => k,
                            _ => draft.bitrate() / 1000,
                        };
                        if ui
                            .add(
                                egui::DragValue::new(&mut kbps)
                                    .range(64..=200_000)
                                    .suffix(" kbps"),
                            )
                            .changed()
                        {
                            draft.quality = Quality::Custom(kbps);
                        }
                    }
                });
                ui.end_row();
            }

            ui.label(RichText::new("Size").color(theme::TEXT_DIM));
            ui.horizontal(|ui| {
                let canvas = sequence.settings.resolution;
                for (label, size) in [
                    ("Full", canvas),
                    ("Half", scaled(canvas, 2)),
                    ("Quarter", scaled(canvas, 4)),
                ] {
                    if ui.selectable_label(draft.resolution == size, label).clicked() {
                        draft.resolution = size;
                    }
                }
                ui.label(
                    RichText::new(format!(
                        "{}×{}",
                        draft.resolution.width, draft.resolution.height
                    ))
                    .small()
                    .monospace()
                    .color(theme::TEXT_FAINT),
                );
            });
            ui.end_row();

            ui.label(RichText::new("Range").color(theme::TEXT_DIM));
            ui.horizontal(|ui| {
                // Without a work area, "the active range" *is* the whole
                // sequence — so that is what the dialogue says, rather than
                // leaving a disabled control looking as though it were chosen.
                let has_work_area = sequence.work_area.is_some();
                let work_area = draft.range == ExportRange::Active && has_work_area;
                if ui.selectable_label(!work_area, "Whole sequence").clicked() {
                    draft.range = ExportRange::Whole;
                }
                ui.add_enabled_ui(has_work_area, |ui| {
                    let response = ui.selectable_label(work_area, "Work area");
                    if response.clicked() {
                        draft.range = ExportRange::Active;
                    }
                    if !has_work_area {
                        response.on_disabled_hover_text("No work area is set");
                    }
                });
            });
            ui.end_row();

            ui.label(RichText::new("Audio").color(theme::TEXT_DIM));
            ui.horizontal(|ui| {
                let mut wanted = draft.audio.is_some();
                if ui.checkbox(&mut wanted, "Include").clicked() {
                    draft.audio = wanted.then(|| AudioSettings::for_sequence(sequence));
                }
                if let Some(audio) = &mut draft.audio {
                    egui::ComboBox::from_id_salt("export-audio-codec")
                        .selected_text(audio.codec.label())
                        .width(110.0)
                        .show_ui(ui, |ui| {
                            for codec in [AudioCodec::Aac, AudioCodec::Pcm16] {
                                if ui
                                    .selectable_label(audio.codec == codec, codec.label())
                                    .clicked()
                                {
                                    audio.codec = codec;
                                }
                            }
                        });
                    if audio.codec.uses_bitrate() {
                        ui.add(
                            egui::DragValue::new(&mut audio.bitrate_kbps)
                                .range(64..=512)
                                .suffix(" kbps"),
                        );
                    }
                }
            });
            ui.end_row();
        });

    ui.add_space(6.0);
    ui.label(RichText::new(estimate(draft, sequence)).small().color(theme::TEXT_FAINT));
    // Said before the button is pressed rather than asked afterwards: the file
    // picker warns about overwriting, and a path that was filled in for the
    // user has never been through one.
    if !running && draft.path.exists() {
        ui.label(
            RichText::new("This replaces the file already there.")
                .small()
                .color(theme::WARNING),
        );
    }
}

/// The progress of a running export, with the one control that matters.
fn progress_section(ui: &mut Ui, state: &EditorState, actions: &mut Vec<Action>) {
    let Some(job) = state.export.job.as_ref() else { return };
    let progress = job.progress();

    ui.add(egui::ProgressBar::new(progress.fraction()).show_percentage().desired_height(14.0));
    ui.horizontal(|ui| {
        ui.label(
            RichText::new(format!(
                "{} of {} frames · {:.1} fps",
                progress.frames_done,
                progress.frames_total,
                progress.frames_per_second()
            ))
            .small()
            .monospace()
            .color(theme::TEXT_DIM),
        );
        if let Some(left) = progress.remaining() {
            ui.label(
                RichText::new(format!("· {} left", short_duration(left)))
                    .small()
                    .color(theme::TEXT_FAINT),
            );
        }
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            if job.is_cancelling() {
                ui.label(RichText::new("stopping…").small().color(theme::WARNING));
            } else if ui.button("Stop").clicked() {
                actions.push(Action::CancelExport);
            }
        });
    });
}

/// A size at `1/divisor`, rounded to even dimensions.
///
/// Even because every codec here subsamples chroma and would refuse an odd
/// one — and rounding it here means the dialogue never offers a size the
/// exporter would reject.
fn scaled(size: ve_core::Size, divisor: u32) -> ve_core::Size {
    let even = |v: u32| (v / divisor).max(2) & !1;
    ve_core::Size::new(even(size.width), even(size.height))
}

/// How long the export will be, and roughly how large.
///
/// From the bitrate, so it is an estimate and says so: what a variable-rate
/// encoder actually writes for a given picture is not knowable in advance.
fn estimate(settings: &ExportSettings, sequence: &ve_core::Sequence) -> String {
    let range = settings.range.resolve(sequence);
    let frames = settings.rate.duration_to_frames(range.duration).max(0);
    let seconds = range.duration.raw() as f64 / ve_time::TICKS_PER_SECOND as f64;
    let video_bits = settings.bitrate() as f64 * seconds;
    let audio_bits =
        settings.audio.map(|a| a.bitrate_kbps as f64 * 1000.0 * seconds).unwrap_or(0.0);
    let megabytes = (video_bits + audio_bits) / 8.0 / (1024.0 * 1024.0);
    format!("{frames} frames at {} — about {megabytes:.0} MB", rate_label(settings.rate))
}

fn rate_label(rate: Rate) -> String {
    rate.to_string()
}

/// A duration as a person would say it.
fn short_duration(d: std::time::Duration) -> String {
    let seconds = d.as_secs();
    if seconds >= 60 {
        format!("{}m {:02}s", seconds / 60, seconds % 60)
    } else {
        format!("{seconds}s")
    }
}
