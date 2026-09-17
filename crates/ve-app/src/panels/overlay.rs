//! The development performance overlay.
//!
//! Speed is a product requirement here, which means it has to be a number on
//! screen rather than an impression. This reports what the frame actually cost,
//! broken down by stage, so a regression is visible while editing instead of
//! being discovered in a benchmark weeks later.

use egui::{Color32, Context, RichText};
use ve_media::CacheStats;
use ve_metrics::{counters, spans, Metrics};
use ve_render::CompositeCacheStats;

/// Everything the overlay reports that does not come from [`Metrics`].
pub struct OverlayInput<'a> {
    pub metrics: &'a Metrics,
    pub frame_cache: CacheStats,
    pub composite_cache: CompositeCacheStats,
    pub gpu_texture_bytes: usize,
    pub gpu_texture_count: usize,
    pub adapter: &'a str,
    pub software_gpu: bool,
    pub clip_count: usize,
    pub visible_layers: usize,
}

pub fn show(ctx: &Context, input: &OverlayInput<'_>) {
    let snapshot = input.metrics.snapshot();

    egui::Window::new("performance")
        .title_bar(false)
        .resizable(false)
        .movable(true)
        // Over the preview's own area rather than the right edge, where it
        // would cover the inspector. Draggable, so it can be moved aside.
        .default_pos(egui::pos2(260.0, 64.0))
        .frame(
            egui::Frame::new()
                .fill(Color32::from_black_alpha(215))
                .corner_radius(crate::theme::RADIUS)
                .inner_margin(egui::Margin::same(8)),
        )
        .show(ctx, |ui| {
            ui.set_min_width(214.0);

            let frame = snapshot.spans.get(spans::FRAME).copied().unwrap_or_default();
            let fps = if frame.mean.as_secs_f64() > 0.0 {
                1.0 / frame.mean.as_secs_f64()
            } else {
                0.0
            };

            ui.label(
                RichText::new(format!("FPS {fps:>8.1}"))
                    .monospace()
                    .color(fps_colour(fps))
                    .size(13.0),
            );
            ui.separator();

            // p95 alongside the mean: a good mean with a bad tail is a stutter
            // the user sees, and the mean alone hides it.
            row(ui, "frame", frame.mean_ms(), Some(frame.p95_ms()));
            row(ui, "engine", snapshot.mean_ms(spans::ENGINE), None);
            row(ui, "decode", snapshot.mean_ms(spans::DECODE), None);
            row(ui, "scale", snapshot.mean_ms(spans::SCALE), None);
            row(ui, "upload", snapshot.mean_ms(spans::UPLOAD), None);
            row(ui, "composite", snapshot.mean_ms(spans::COMPOSITE), None);
            row(ui, "ui", snapshot.mean_ms(spans::UI), None);

            ui.separator();

            let dropped = snapshot.dropped_frames;
            let drop_rate = snapshot.drop_rate() * 100.0;
            ui.label(
                RichText::new(format!("dropped  {dropped:>6}  ({drop_rate:.1}%)"))
                    .monospace()
                    .size(10.5)
                    .color(if drop_rate > 2.0 {
                        crate::theme::ERROR
                    } else {
                        crate::theme::TEXT_DIM
                    }),
            );

            let cache = input.frame_cache;
            ui.label(
                RichText::new(format!(
                    "frames   {:>6}  {:.0}% hit",
                    cache.entries,
                    cache.hit_rate() * 100.0
                ))
                .monospace()
                .size(10.5)
                .color(crate::theme::TEXT_DIM),
            );
            ui.label(
                RichText::new(format!(
                    "cache    {:>5.0} MB / {:.0} MB",
                    cache.bytes as f64 / 1048576.0,
                    cache.capacity_bytes as f64 / 1048576.0
                ))
                .monospace()
                .size(10.5)
                .color(crate::theme::TEXT_DIM),
            );
            // The render cache's hit rate answers a question the composite
            // timing cannot: whether the compositor is being asked to redraw
            // pictures it has already produced.
            let composites = input.composite_cache;
            ui.label(
                RichText::new(format!(
                    "renders  {:>6}  {:.0}% hit",
                    composites.entries,
                    composites.hit_rate() * 100.0
                ))
                .monospace()
                .size(10.5)
                .color(crate::theme::TEXT_DIM),
            );
            ui.label(
                RichText::new(format!(
                    "  cached {:>5.0} MB   unchanged {}",
                    composites.bytes as f64 / 1048576.0,
                    snapshot.counters.get(counters::COMPOSITE_UNCHANGED).copied().unwrap_or(0)
                ))
                .monospace()
                .size(10.5)
                .color(crate::theme::TEXT_DIM),
            );
            ui.label(
                RichText::new(format!(
                    "gpu tex  {:>5.0} MB  ({} textures)",
                    input.gpu_texture_bytes as f64 / 1048576.0,
                    input.gpu_texture_count
                ))
                .monospace()
                .size(10.5)
                .color(crate::theme::TEXT_DIM),
            );

            if let Some(memory) = snapshot.memory {
                ui.label(
                    RichText::new(format!("memory   {:>5.0} MB", memory.resident_mb()))
                        .monospace()
                        .size(10.5)
                        .color(crate::theme::TEXT_DIM),
                );
            }

            ui.separator();
            ui.label(
                RichText::new(format!(
                    "clips {}   layers {}   decodes {}",
                    input.clip_count,
                    input.visible_layers,
                    snapshot.counters.get(counters::DECODE_REQUESTS).copied().unwrap_or(0)
                ))
                .monospace()
                .size(10.0)
                .color(crate::theme::TEXT_FAINT),
            );
            ui.label(
                RichText::new(format!(
                    "cancelled {}   seeks {}",
                    snapshot.counters.get(counters::DECODE_CANCELLED).copied().unwrap_or(0),
                    snapshot.counters.get(counters::SEEKS).copied().unwrap_or(0)
                ))
                .monospace()
                .size(10.0)
                .color(crate::theme::TEXT_FAINT),
            );
            ui.label(
                RichText::new(input.adapter)
                    .monospace()
                    .size(9.5)
                    // A software rasteriser is flagged, so a slow machine is
                    // not mistaken for a slow renderer.
                    .color(if input.software_gpu {
                        crate::theme::WARNING
                    } else {
                        crate::theme::TEXT_FAINT
                    }),
            );
        });
}

fn row(ui: &mut egui::Ui, label: &str, mean_ms: f64, p95_ms: Option<f64>) {
    let text = match p95_ms {
        Some(p95) => format!("{label:<10}{mean_ms:>6.2} ms   p95 {p95:>6.2}"),
        None => format!("{label:<10}{mean_ms:>6.2} ms"),
    };
    ui.label(RichText::new(text).monospace().size(10.5).color(crate::theme::TEXT));
}

/// Green at or above 55 fps, amber down to 25, red below.
fn fps_colour(fps: f64) -> Color32 {
    if fps >= 55.0 {
        Color32::from_rgb(0x6f, 0xcf, 0x8f)
    } else if fps >= 25.0 {
        crate::theme::WARNING
    } else {
        crate::theme::ERROR
    }
}
