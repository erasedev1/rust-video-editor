//! The editor's visual language.
//!
//! Original rather than an imitation of any existing product, but shaped by the
//! same constraints professional tools work under: a dark, low-chroma surface
//! so that footage is judged against neutral grey rather than against the
//! interface, dense controls so more timeline fits on screen, and colour
//! reserved for meaning — selection, the playhead, track kind — instead of
//! decoration.

use egui::{Color32, CornerRadius, Stroke};

/// Surfaces, from furthest back to nearest front.
pub const BACKGROUND: Color32 = Color32::from_rgb(0x14, 0x15, 0x18);
pub const PANEL: Color32 = Color32::from_rgb(0x1b, 0x1d, 0x21);
pub const PANEL_RAISED: Color32 = Color32::from_rgb(0x23, 0x26, 0x2b);
pub const SUNKEN: Color32 = Color32::from_rgb(0x0e, 0x0f, 0x11);
pub const SEPARATOR: Color32 = Color32::from_rgb(0x2e, 0x32, 0x38);

/// Text, from most to least prominent.
pub const TEXT: Color32 = Color32::from_rgb(0xd8, 0xdc, 0xe2);
pub const TEXT_DIM: Color32 = Color32::from_rgb(0x8b, 0x93, 0x9e);
pub const TEXT_FAINT: Color32 = Color32::from_rgb(0x5d, 0x65, 0x70);

/// Accents. One hue carries "the system is acting on this".
pub const ACCENT: Color32 = Color32::from_rgb(0x4d, 0x9d, 0xe0);
pub const ACCENT_DIM: Color32 = Color32::from_rgb(0x2f, 0x5f, 0x87);
pub const PLAYHEAD: Color32 = Color32::from_rgb(0xff, 0xc4, 0x4d);
pub const WARNING: Color32 = Color32::from_rgb(0xe0, 0x8c, 0x3a);
pub const ERROR: Color32 = Color32::from_rgb(0xe0, 0x5c, 0x5c);
pub const OFFLINE: Color32 = Color32::from_rgb(0x8a, 0x3d, 0x3d);

/// Clip fills. Video and audio are told apart by hue, not by label, so a glance
/// at a dense timeline reads correctly.
pub const CLIP_VIDEO: Color32 = Color32::from_rgb(0x2f, 0x4f, 0x6b);
pub const CLIP_VIDEO_TOP: Color32 = Color32::from_rgb(0x3c, 0x63, 0x85);
pub const CLIP_AUDIO: Color32 = Color32::from_rgb(0x2c, 0x53, 0x45);
pub const CLIP_AUDIO_TOP: Color32 = Color32::from_rgb(0x38, 0x69, 0x57);
pub const CLIP_SELECTED: Color32 = Color32::from_rgb(0xf0, 0xf4, 0xf8);
/// A caption on the timeline. Deliberately unlike either clip colour: a cue is
/// text rather than media, and a lane of them should not read as a third kind
/// of track.
pub const CAPTION_CUE: Color32 = Color32::from_rgb(0x46, 0x3b, 0x60);
pub const CLIP_DISABLED: Color32 = Color32::from_rgb(0x30, 0x33, 0x38);

/// The waveform inside an audio clip. Two tones: the peak envelope, and the
/// RMS body drawn inside it. The pair is the point — an envelope alone makes a
/// gently loud passage and a spiky quiet one look the same, and it is the
/// difference between them that tells you where the audio actually is.
pub const WAVEFORM_PEAK: Color32 = Color32::from_rgb(0x74, 0xc4, 0xa2);
pub const WAVEFORM_BODY: Color32 = Color32::from_rgb(0xa8, 0xe6, 0xc8);
/// The centre line, which is what makes silence legible as silence rather than
/// as a clip that failed to draw.
pub const WAVEFORM_AXIS: Color32 = Color32::from_rgb(0x4a, 0x7d, 0x68);

/// Timeline furniture.
pub const RULER_BACKGROUND: Color32 = Color32::from_rgb(0x18, 0x1a, 0x1e);
pub const TRACK_HEADER: Color32 = Color32::from_rgb(0x1f, 0x22, 0x26);
pub const TRACK_LANE: Color32 = Color32::from_rgb(0x17, 0x19, 0x1c);
pub const TRACK_LANE_ALT: Color32 = Color32::from_rgb(0x1a, 0x1c, 0x20);
pub const GRID: Color32 = Color32::from_rgb(0x26, 0x2a, 0x30);

pub const RADIUS: CornerRadius = CornerRadius::same(3);

/// Applies the theme to an egui context.
///
/// The editor is dark only. A light theme for a colour-critical tool would mean
/// judging footage against a bright surround, which changes how the picture
/// reads; both of egui's theme slots are therefore set to the same dark style
/// so the interface does not flip with the desktop's preference.
pub fn apply(ctx: &egui::Context) {
    ctx.set_theme(egui::Theme::Dark);
    let mut style = (*ctx.style_of(egui::Theme::Dark)).clone();
    let v = &mut style.visuals;

    v.dark_mode = true;
    v.override_text_color = Some(TEXT);
    v.panel_fill = PANEL;
    v.window_fill = PANEL;
    v.extreme_bg_color = SUNKEN;
    v.faint_bg_color = PANEL_RAISED;
    v.window_stroke = Stroke::new(1.0, SEPARATOR);

    v.widgets.noninteractive.bg_fill = PANEL;
    v.widgets.noninteractive.weak_bg_fill = PANEL;
    v.widgets.noninteractive.bg_stroke = Stroke::new(1.0, SEPARATOR);
    v.widgets.noninteractive.fg_stroke = Stroke::new(1.0, TEXT_DIM);
    v.widgets.noninteractive.corner_radius = RADIUS;

    v.widgets.inactive.bg_fill = PANEL_RAISED;
    v.widgets.inactive.weak_bg_fill = PANEL_RAISED;
    v.widgets.inactive.bg_stroke = Stroke::new(1.0, SEPARATOR);
    v.widgets.inactive.fg_stroke = Stroke::new(1.0, TEXT);
    v.widgets.inactive.corner_radius = RADIUS;

    v.widgets.hovered.bg_fill = Color32::from_rgb(0x2c, 0x31, 0x38);
    v.widgets.hovered.weak_bg_fill = Color32::from_rgb(0x2c, 0x31, 0x38);
    v.widgets.hovered.bg_stroke = Stroke::new(1.0, ACCENT_DIM);
    v.widgets.hovered.fg_stroke = Stroke::new(1.0, TEXT);
    v.widgets.hovered.corner_radius = RADIUS;

    v.widgets.active.bg_fill = ACCENT_DIM;
    v.widgets.active.weak_bg_fill = ACCENT_DIM;
    v.widgets.active.bg_stroke = Stroke::new(1.0, ACCENT);
    v.widgets.active.fg_stroke = Stroke::new(1.0, Color32::WHITE);
    v.widgets.active.corner_radius = RADIUS;

    v.selection.bg_fill = ACCENT_DIM;
    v.selection.stroke = Stroke::new(1.0, ACCENT);

    // Tight spacing: a professional timeline lives or dies by how much fits.
    style.spacing.item_spacing = egui::vec2(6.0, 4.0);
    style.spacing.button_padding = egui::vec2(8.0, 3.0);
    style.spacing.menu_margin = egui::Margin::same(6);
    style.spacing.indent = 14.0;
    style.spacing.interact_size.y = 20.0;

    use egui::{FontFamily, FontId, TextStyle};
    style.text_styles = [
        (TextStyle::Heading, FontId::new(14.0, FontFamily::Proportional)),
        (TextStyle::Body, FontId::new(12.5, FontFamily::Proportional)),
        (TextStyle::Button, FontId::new(12.5, FontFamily::Proportional)),
        (TextStyle::Small, FontId::new(10.5, FontFamily::Proportional)),
        // Timecode and metrics are monospaced so digits do not jitter as they
        // change, which they do constantly during playback.
        (TextStyle::Monospace, FontId::new(12.0, FontFamily::Monospace)),
    ]
    .into();

    let style = std::sync::Arc::new(style);
    ctx.set_style_of(egui::Theme::Dark, style.clone());
    ctx.set_style_of(egui::Theme::Light, style);
}

/// Colour for a clip body.
pub fn clip_colour(
    kind: ve_core::TrackKind,
    selected: bool,
    enabled: bool,
) -> (Color32, Color32) {
    if !enabled {
        return (CLIP_DISABLED, SEPARATOR);
    }
    let (body, top) = match kind {
        ve_core::TrackKind::Video => (CLIP_VIDEO, CLIP_VIDEO_TOP),
        ve_core::TrackKind::Audio => (CLIP_AUDIO, CLIP_AUDIO_TOP),
    };
    if selected {
        (top, CLIP_SELECTED)
    } else {
        (body, top)
    }
}
