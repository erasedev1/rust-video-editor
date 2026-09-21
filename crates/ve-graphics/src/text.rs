//! Laying out a run of text, and drawing it as an outline.
//!
//! # What is borrowed and what is ours
//!
//! Shaping — turning characters into positioned glyphs, with the kerning,
//! ligatures and reordering a script asks for — is `rustybuzz`, a port of
//! HarfBuzz. Reading the outlines out of a font file is `ttf-parser`. Finding a
//! family on the machine is `fontdb`. What is written here is the part above
//! those: where the lines go, how far apart, where each one starts, and how the
//! result becomes a path.
//!
//! # Text is a path, not a bitmap
//!
//! Glyphs come out of the font as outlines and go into the same rasteriser a
//! rectangle does. That is what makes a stroke on a title the same stroke as on
//! a shape — one implementation, so the two cannot disagree — and it is what
//! will let a text animator move a glyph without redrawing anything else.
//!
//! Every glyph of every line goes into **one** path, filled once. Filling each
//! glyph separately would double-blend wherever two overlap, which in a script
//! face is most of them, and a half-transparent title would show a seam along
//! every join.
//!
//! # What is not here
//!
//! One face for the whole run: no per-character styling, and no fallback to a
//! second family for a glyph this one does not have — a missing glyph draws the
//! font's own missing-glyph box, which is what it is for. No bidirectional
//! reordering *between* runs: a single script reads correctly, including
//! right-to-left, because shaping handles it, but a paragraph mixing directions
//! needs a bidi pass this does not have. And no vertical writing modes.

use tiny_skia::{FillRule, Paint, PathBuilder, Pixmap, Stroke, Transform};
use ve_core::{Size, TextAlign, TextState};

use crate::fonts::FontLibrary;
use crate::{colour, image_from_pixmap, RasterImage, MAX_DIMENSION};

/// The margin an anti-aliased edge writes past the outline, in pixels.
const ANTIALIAS_MARGIN: f32 = 1.0;

/// A glyph, placed.
#[derive(Debug, Clone, Copy, PartialEq)]
struct Placed {
    id: u16,
    /// Pen position in pixels, relative to the start of the line.
    x: f32,
    /// Offset from the baseline, in pixels, positive downwards.
    y: f32,
}

/// One line of shaped glyphs and how wide it is.
#[derive(Debug, Clone, Default)]
struct Line {
    glyphs: Vec<Placed>,
    width: f32,
}

/// What a run of text occupies, before anything is drawn.
///
/// Returned by [`measure`] so that the interface can put a box around a title,
/// and a test can ask how wide something is without reading pixels.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct TextMetrics {
    /// The width of the longest line, or the wrapping width when one is set.
    pub width: f32,
    /// From the top of the first line to the bottom of the last.
    pub height: f32,
    /// How far the first baseline sits below the top.
    pub ascent: f32,
    /// Distance between baselines.
    pub line_height: f32,
    pub lines: usize,
}

/// Measures a run of text without drawing it.
pub fn measure(state: &TextState, fonts: &FontLibrary) -> Option<TextMetrics> {
    layout(state, fonts).map(|l| l.metrics)
}

struct Layout {
    lines: Vec<Line>,
    metrics: TextMetrics,
}

/// Lays a run of text out into lines of placed glyphs.
fn layout(state: &TextState, fonts: &FontLibrary) -> Option<Layout> {
    let size = state.size as f32;
    if !size.is_finite() || size <= 0.0 || state.text.is_empty() {
        return None;
    }
    let resolved = fonts.resolve(&state.font)?;

    fonts.with_face_data(resolved.id, |data, index| {
        let face = rustybuzz::Face::from_slice(data, index)?;
        let upem = face.units_per_em();
        if upem <= 0 {
            return None;
        }
        let scale = size / upem as f32;
        // Tracking is in thousandths of an em, the unit every type tool uses,
        // so a value copied from one of them means the same thing here.
        let tracking = (state.tracking as f32 / 1000.0) * size;

        let mut lines = Vec::new();
        for paragraph in state.text.split('\n') {
            match state.wrap_width {
                Some(width) if width > 0.0 => {
                    wrap(&face, paragraph, scale, tracking, width as f32, &mut lines);
                }
                // Without a width there is nothing to wrap against: the text
                // breaks where it says to and nowhere else.
                _ => lines.push(shape_line(&face, paragraph, scale, tracking)),
            }
        }

        let ascent = face.ascender() as f32 * scale;
        let descent = -face.descender() as f32 * scale;
        let line_height = (state.line_height as f32).max(0.0) * size;
        let widest = lines.iter().fold(0.0f32, |acc, l| acc.max(l.width));
        let width = match state.wrap_width {
            Some(w) if w > 0.0 => (w as f32).max(widest),
            _ => widest,
        };
        let height = ascent + descent + line_height * (lines.len().saturating_sub(1)) as f32;

        Some(Layout {
            metrics: TextMetrics { width, height, ascent, line_height, lines: lines.len() },
            lines,
        })
    })?
}

/// Shapes one line and places its glyphs from a pen at zero.
fn shape_line(face: &rustybuzz::Face, text: &str, scale: f32, tracking: f32) -> Line {
    if text.is_empty() {
        return Line::default();
    }
    let mut buffer = rustybuzz::UnicodeBuffer::new();
    buffer.push_str(text);
    // Direction, script and language are guessed from the characters
    // themselves, which is what makes a right-to-left line come out in the
    // right order without the caller having to say so.
    let shaped = rustybuzz::shape(face, &[], buffer);

    let mut glyphs = Vec::with_capacity(shaped.len());
    let mut pen = 0.0f32;
    for (info, position) in shaped.glyph_infos().iter().zip(shaped.glyph_positions()) {
        glyphs.push(Placed {
            id: info.glyph_id as u16,
            x: pen + position.x_offset as f32 * scale,
            // Font coordinates are Y-up and a picture is Y-down, so an offset
            // that raises a glyph lowers its row.
            y: -(position.y_offset as f32 * scale),
        });
        pen += position.x_advance as f32 * scale + tracking;
    }
    // The trailing gap belongs between characters, not after the last one;
    // leaving it in would shift every centred line by half a letterspace.
    let width = if glyphs.is_empty() { 0.0 } else { pen - tracking };
    Line { glyphs, width }
}

/// Greedy word wrapping: the most words that fit, then a new line.
///
/// Measured by shaping the candidate rather than by summing the widths of the
/// words in it, because shaping a phrase is not the same as shaping its parts —
/// kerning and ligatures act across the space, and a line that fit as a sum can
/// overflow once it is actually set.
fn wrap(
    face: &rustybuzz::Face,
    paragraph: &str,
    scale: f32,
    tracking: f32,
    width: f32,
    lines: &mut Vec<Line>,
) {
    let words: Vec<&str> = paragraph.split(' ').collect();
    let mut current = String::new();
    for word in words {
        let candidate =
            if current.is_empty() { word.to_string() } else { format!("{current} {word}") };
        let line = shape_line(face, &candidate, scale, tracking);
        if line.width > width && !current.is_empty() {
            lines.push(shape_line(face, &current, scale, tracking));
            current = word.to_string();
        } else {
            current = candidate;
        }
    }
    // A word longer than the box stays on a line of its own and overhangs:
    // breaking inside a word needs hyphenation rules this does not have, and
    // chopping one at an arbitrary letter is worse than an overhang the user
    // can see and fix.
    lines.push(shape_line(face, &current, scale, tracking));
}

/// Where a line starts, given the box it is aligned in.
fn line_origin(align: TextAlign, box_width: f32, line_width: f32) -> f32 {
    match align {
        TextAlign::Left => 0.0,
        TextAlign::Center => (box_width - line_width) / 2.0,
        TextAlign::Right => box_width - line_width,
    }
}

/// Draws a run of text into a picture of its own bounding box.
pub(crate) fn draw_text(state: &TextState, fonts: &FontLibrary) -> Option<RasterImage> {
    if !state.has_fill() && !state.has_stroke() {
        return None;
    }
    let Layout { lines, metrics } = layout(state, fonts)?;
    let resolved = fonts.resolve(&state.font)?;

    let stroke_width = if state.has_stroke() { state.stroke_width as f32 } else { 0.0 };
    let margin = stroke_width / 2.0 + ANTIALIAS_MARGIN;
    let width = (metrics.width + margin * 2.0).ceil() as u64;
    let height = (metrics.height + margin * 2.0).ceil() as u64;
    if width == 0 || height == 0 {
        return None;
    }
    if width > MAX_DIMENSION as u64 || height > MAX_DIMENSION as u64 {
        log::warn!("text is {width}×{height}, past the {MAX_DIMENSION}px limit; not drawn");
        return None;
    }

    let path = fonts.with_face_data(resolved.id, |data, index| {
        let face = ttf_parser::Face::parse(data, index).ok()?;
        let scale = state.size as f32 / face.units_per_em() as f32;
        let mut builder = GlyphPath { path: PathBuilder::new(), x: 0.0, y: 0.0, scale };

        for (row, line) in lines.iter().enumerate() {
            let left = margin + line_origin(state.align, metrics.width, line.width);
            let baseline = margin + metrics.ascent + metrics.line_height * row as f32;
            for glyph in &line.glyphs {
                builder.x = left + glyph.x;
                builder.y = baseline + glyph.y;
                // A glyph with no outline — a space, and anything the font has
                // no drawing for — advances the pen and draws nothing, which is
                // exactly what the missing return value means here.
                face.outline_glyph(ttf_parser::GlyphId(glyph.id), &mut builder);
            }
        }
        builder.path.finish()
    })??;

    let mut pixmap = Pixmap::new(width as u32, height as u32)?;
    if state.has_fill() {
        let mut paint = Paint { anti_alias: true, ..Paint::default() };
        paint.set_color(colour(state.fill));
        // Non-zero winding, which is how a glyph says which of its contours are
        // counters: an even-odd fill would punch the bowl out of a 'b' and then
        // fill the hole in an 'o' that a stroke overlaps.
        pixmap.fill_path(&path, &paint, FillRule::Winding, Transform::identity(), None);
    }
    if state.has_stroke() {
        let mut paint = Paint { anti_alias: true, ..Paint::default() };
        paint.set_color(colour(state.stroke));
        let stroke = Stroke { width: stroke_width, ..Stroke::default() };
        pixmap.stroke_path(&path, &paint, &stroke, Transform::identity(), None);
    }

    let image = image_from_pixmap(pixmap);
    debug_assert_eq!(image.size(), Size::new(width as u32, height as u32));
    Some(image)
}

/// Collects one glyph's outline into the path being built, in picture
/// coordinates.
///
/// `ttf-parser` reports a glyph in font units with Y up and the origin on the
/// baseline; a picture counts pixels from its top-left corner with Y down. This
/// is the one place that conversion happens.
struct GlyphPath {
    path: PathBuilder,
    x: f32,
    y: f32,
    scale: f32,
}

impl GlyphPath {
    #[inline]
    fn at(&self, x: f32, y: f32) -> (f32, f32) {
        (self.x + x * self.scale, self.y - y * self.scale)
    }
}

impl ttf_parser::OutlineBuilder for GlyphPath {
    fn move_to(&mut self, x: f32, y: f32) {
        let (x, y) = self.at(x, y);
        self.path.move_to(x, y);
    }

    fn line_to(&mut self, x: f32, y: f32) {
        let (x, y) = self.at(x, y);
        self.path.line_to(x, y);
    }

    fn quad_to(&mut self, x1: f32, y1: f32, x: f32, y: f32) {
        let (x1, y1) = self.at(x1, y1);
        let (x, y) = self.at(x, y);
        self.path.quad_to(x1, y1, x, y);
    }

    fn curve_to(&mut self, x1: f32, y1: f32, x2: f32, y2: f32, x: f32, y: f32) {
        let (x1, y1) = self.at(x1, y1);
        let (x2, y2) = self.at(x2, y2);
        let (x, y) = self.at(x, y);
        self.path.cubic_to(x1, y1, x2, y2, x, y);
    }

    fn close(&mut self) {
        self.path.close();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ve_core::{FontSpec, Rgba};

    fn text(s: &str) -> TextState {
        TextState {
            text: s.into(),
            font: FontSpec::default(),
            size: 48.0,
            fill: Rgba::WHITE,
            stroke: Rgba::TRANSPARENT,
            stroke_width: 0.0,
            tracking: 0.0,
            line_height: 1.2,
            align: TextAlign::Left,
            wrap_width: None,
        }
    }

    #[test]
    fn an_empty_string_measures_nothing() {
        let library = crate::test_library();
        assert!(measure(&text(""), &library).is_none());
    }

    #[test]
    fn a_longer_string_is_wider() {
        let library = crate::test_library();
        let short = measure(&text("i"), &library).expect("measured");
        let long = measure(&text("in the beginning"), &library).expect("measured");
        assert!(long.width > short.width);
        assert_eq!(long.lines, 1);
        assert!((long.height - short.height).abs() < 1e-3, "one line either way");
    }

    #[test]
    fn a_bigger_size_is_bigger_in_both_directions() {
        let library = crate::test_library();
        let small = measure(&text("Verge"), &library).expect("measured");
        let mut state = text("Verge");
        state.size = 96.0;
        let big = measure(&state, &library).expect("measured");
        assert!((big.width / small.width - 2.0).abs() < 0.01, "{small:?} {big:?}");
        assert!((big.height / small.height - 2.0).abs() < 0.01);
    }

    #[test]
    fn a_newline_starts_another_line() {
        let library = crate::test_library();
        let one = measure(&text("one"), &library).expect("measured");
        let two = measure(&text("one\ntwo"), &library).expect("measured");
        assert_eq!(two.lines, 2);
        assert!((two.height - (one.height + one.line_height)).abs() < 1e-3);
    }

    #[test]
    fn line_height_spaces_the_baselines() {
        let library = crate::test_library();
        let mut state = text("one\ntwo");
        state.line_height = 2.0;
        let metrics = measure(&state, &library).expect("measured");
        assert!((metrics.line_height - 96.0).abs() < 1e-3, "two ems at 48px");
    }

    #[test]
    fn tracking_widens_a_line_by_a_gap_per_character() {
        let library = crate::test_library();
        let plain = measure(&text("abcdef"), &library).expect("measured");
        let mut state = text("abcdef");
        // A tenth of an em, which at 48px is 4.8px between each of six glyphs.
        state.tracking = 100.0;
        let tracked = measure(&state, &library).expect("measured");
        let expected = plain.width + 4.8 * 5.0;
        assert!((tracked.width - expected).abs() < 0.01, "{} vs {expected}", tracked.width);
    }

    #[test]
    fn wrapping_breaks_a_paragraph_into_lines_that_fit() {
        let library = crate::test_library();
        let mut state = text("the quick brown fox jumps over the lazy dog");
        state.size = 24.0;
        state.wrap_width = Some(200.0);
        let metrics = measure(&state, &library).expect("measured");
        assert!(metrics.lines > 1, "a long line in a narrow box wraps");
        assert_eq!(metrics.width, 200.0, "the box is the width, not the longest line");
    }

    #[test]
    fn a_word_longer_than_the_box_overhangs_rather_than_being_chopped() {
        let library = crate::test_library();
        let mut state = text("antidisestablishmentarianism");
        state.wrap_width = Some(20.0);
        let metrics = measure(&state, &library).expect("measured");
        assert_eq!(metrics.lines, 1);
        assert!(metrics.width > 20.0, "the box gives way to the word");
    }

    #[test]
    fn alignment_moves_a_line_within_the_box() {
        assert_eq!(line_origin(TextAlign::Left, 100.0, 40.0), 0.0);
        assert_eq!(line_origin(TextAlign::Center, 100.0, 40.0), 30.0);
        assert_eq!(line_origin(TextAlign::Right, 100.0, 40.0), 60.0);
    }

    #[test]
    fn text_with_no_font_to_draw_it_draws_nothing() {
        let empty = FontLibrary::empty();
        assert!(measure(&text("Verge"), &empty).is_none());
        assert!(draw_text(&text("Verge"), &empty).is_none());
    }
}
