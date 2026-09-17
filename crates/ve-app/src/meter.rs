//! Drawing a level meter, and the labels that go beside one.
//!
//! # Decibels, not amplitude
//!
//! A meter drawn on a linear amplitude scale spends most of its length on the
//! top 6 dB and squashes everything quiet into the first few pixels, which is
//! precisely backwards: the useful part of a meter is the last 20 dB below full
//! scale. So the bar is drawn on a decibel scale, from [`FLOOR_DB`] to full.
//!
//! # What the colours mean
//!
//! Green up to [`WARN_DB`], amber from there to full scale, and the whole bar
//! turns red once the reading reaches or passes it. The reading itself is taken
//! before limiting — see [`ve_engine::MixStats::master`] — so "red" means the
//! mix really is over, not that it happened to touch the ceiling.

use egui::{Color32, CornerRadius, Pos2, Rect};
use ve_engine::Meter;

use crate::theme;

/// The quietest level the bar shows. Below this it reads as empty.
pub const FLOOR_DB: f32 = -48.0;

/// Where the bar stops being green.
pub const WARN_DB: f32 = -6.0;

const GREEN: Color32 = Color32::from_rgb(0x5c, 0xc0, 0x8a);
const AMBER: Color32 = theme::WARNING;

/// Where a level sits along the bar, in `0..=1`.
///
/// Silence and anything under the floor are `0`; full scale is `1`. An
/// overshoot is clamped, because there is no room past the end of the bar —
/// the colour is what says a mix went over.
pub fn position(level: f32) -> f32 {
    if level <= 0.0 {
        return 0.0;
    }
    let db = 20.0 * level.log10();
    ((db - FLOOR_DB) / -FLOOR_DB).clamp(0.0, 1.0)
}

fn colour(level: f32) -> Color32 {
    if level >= 1.0 {
        theme::ERROR
    } else if 20.0 * level.log10() >= WARN_DB {
        AMBER
    } else {
        GREEN
    }
}

/// Draws a two-channel horizontal meter filling `rect`.
///
/// Two bars rather than one: a mix pushed hard to one side is a thing to notice,
/// and a single averaged bar hides it.
pub fn draw(painter: &egui::Painter, rect: Rect, meter: Meter) {
    painter.rect_filled(rect, CornerRadius::same(2), theme::SUNKEN);
    if rect.width() < 4.0 || rect.height() < 3.0 {
        return;
    }

    let gap = if rect.height() >= 8.0 { 1.0 } else { 0.0 };
    let half = (rect.height() - gap) / 2.0;
    for (index, level) in [meter.left, meter.right].into_iter().enumerate() {
        let top = rect.top() + index as f32 * (half + gap);
        let filled = rect.width() * position(level);
        if filled <= 0.0 {
            continue;
        }
        painter.rect_filled(
            Rect::from_min_max(
                Pos2::new(rect.left(), top),
                Pos2::new(rect.left() + filled, top + half),
            ),
            CornerRadius::same(1),
            colour(level),
        );
    }

    // The mark the eye actually looks for, at the point the mix stops being
    // comfortably clear of the ceiling.
    let warn_x = rect.left() + rect.width() * position(db_to_level(WARN_DB));
    painter.line_segment(
        [Pos2::new(warn_x, rect.top()), Pos2::new(warn_x, rect.bottom())],
        egui::Stroke::new(1.0, theme::SEPARATOR),
    );
}

fn db_to_level(db: f32) -> f32 {
    10f32.powf(db / 20.0)
}

/// A linear gain as decibels, for a label beside a fader.
pub fn gain_label(gain: f64) -> String {
    if gain <= 0.0 {
        return "−∞ dB".to_string();
    }
    let db = 20.0 * gain.log10();
    if db.abs() < 0.05 {
        "0.0 dB".to_string()
    } else {
        format!("{db:+.1} dB")
    }
}

/// A pan position as the labels a mixing desk uses.
pub fn pan_label(pan: f64) -> String {
    let percent = (pan.abs() * 100.0).round() as i64;
    match pan {
        p if p.abs() < 0.005 => "C".to_string(),
        p if p < 0.0 => format!("L{percent}"),
        _ => format!("R{percent}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn full_scale_fills_the_bar_and_silence_leaves_it_empty() {
        assert_eq!(position(1.0), 1.0);
        assert_eq!(position(0.0), 0.0);
        assert_eq!(position(-1.0), 0.0, "a nonsense level must not draw off the end");
    }

    #[test]
    fn an_overshoot_is_clamped_to_the_end_of_the_bar_and_coloured_red() {
        assert_eq!(position(4.0), 1.0);
        assert_eq!(colour(1.0), theme::ERROR);
        assert_eq!(colour(2.0), theme::ERROR);
    }

    #[test]
    fn the_scale_is_decibels_so_half_amplitude_is_most_of_the_way_along() {
        // Half amplitude is -6 dB, which on a 48 dB scale is seven eighths.
        let half = position(0.5);
        assert!((half - 0.875).abs() < 0.01, "got {half}");
        // Whereas a linear scale would have put it at exactly the middle.
        assert!(half > 0.6, "the quiet end must not be squashed into nothing");
    }

    #[test]
    fn anything_under_the_floor_reads_as_empty() {
        assert_eq!(position(db_to_level(FLOOR_DB - 1.0)), 0.0);
        assert!(position(db_to_level(FLOOR_DB + 1.0)) > 0.0);
    }

    #[test]
    fn the_colour_changes_at_the_warning_mark_and_not_before() {
        assert_eq!(colour(db_to_level(WARN_DB - 0.1)), GREEN);
        assert_eq!(colour(db_to_level(WARN_DB + 0.1)), AMBER);
    }

    #[test]
    fn gain_reads_as_decibels_with_silence_spelled_out() {
        assert_eq!(gain_label(1.0), "0.0 dB");
        assert_eq!(gain_label(0.0), "−∞ dB");
        assert_eq!(gain_label(0.5), "-6.0 dB");
        assert_eq!(gain_label(2.0), "+6.0 dB");
    }

    #[test]
    fn pan_reads_the_way_a_desk_labels_it() {
        assert_eq!(pan_label(0.0), "C");
        assert_eq!(pan_label(-1.0), "L100");
        assert_eq!(pan_label(0.5), "R50");
    }
}
