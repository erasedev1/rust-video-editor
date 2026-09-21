//! What a run of text actually puts on the canvas.
//!
//! Drawn against a font committed to `testdata`, not against whatever the
//! machine has installed, so that a glyph measures the same here as on anyone
//! else's computer.

use ve_core::{FontSpec, GraphicState, Rgba, TextAlign, TextState};
use ve_graphics::{draw_with, measure, FontLibrary, RasterImage};

fn library() -> FontLibrary {
    let mut library = FontLibrary::empty();
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../testdata/LiberationSans-Regular.ttf");
    assert!(library.load_file(&path), "the test font is committed");
    library
}

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

fn drawn(state: &TextState, fonts: &FontLibrary) -> RasterImage {
    draw_with(&GraphicState::Text(state.clone()), fonts).expect("something to draw")
}

/// Columns and rows that have any ink in them at all.
fn ink_columns(image: &RasterImage) -> Vec<u32> {
    (0..image.width())
        .filter(|x| (0..image.height()).any(|y| image.pixel(*x, y).expect("in range")[3] > 0))
        .collect()
}

fn ink_rows(image: &RasterImage) -> Vec<u32> {
    (0..image.height())
        .filter(|y| (0..image.width()).any(|x| image.pixel(x, *y).expect("in range")[3] > 0))
        .collect()
}

#[test]
fn text_puts_ink_on_the_picture() {
    let fonts = library();
    let image = drawn(&text("Verge"), &fonts);
    let inked = ink_columns(&image);
    assert!(!inked.is_empty(), "a title that draws nothing is not a title");
    assert!(
        inked.len() < image.width() as usize,
        "and not a solid block either: there are gaps between letters"
    );
}

#[test]
fn a_glyph_sits_on_the_baseline_the_metrics_promise() {
    let fonts = library();
    let state = text("H");
    let metrics = measure(&state, &fonts).expect("measured");
    let image = drawn(&state, &fonts);

    // A capital H sits on the baseline and rises to the cap height, so its ink
    // ends within a pixel of the baseline and starts well above it.
    let rows = ink_rows(&image);
    let (top, bottom) = (rows[0] as f32, *rows.last().expect("ink") as f32);
    let baseline = metrics.ascent + 1.0; // plus the anti-alias margin
    assert!((bottom - baseline).abs() <= 2.0, "bottom {bottom}, baseline {baseline}");
    assert!(top > 1.0, "the ascender line is above the cap height: {top}");
}

#[test]
fn spaces_take_up_room_but_draw_nothing() {
    let fonts = library();
    // A space advances the pen and has no outline, so a string of them measures
    // a width and produces no picture at all — there is nothing to upload, and
    // an empty layer is better than a transparent one.
    let state = text("   ");
    assert!(measure(&state, &fonts).expect("measured").width > 0.0);
    assert!(draw_with(&GraphicState::Text(state), &fonts).is_none());

    // The pen really did advance: a space between two letters separates them.
    let joined = measure(&text("aa"), &fonts).expect("measured").width;
    let spaced = measure(&text("a a"), &fonts).expect("measured").width;
    assert!(spaced > joined);
}

#[test]
fn a_stroke_puts_ink_outside_the_letters() {
    let fonts = library();
    let plain = drawn(&text("o"), &fonts);

    let mut state = text("o");
    state.stroke = Rgba::new(1.0, 0.0, 0.0, 1.0);
    state.stroke_width = 6.0;
    let stroked = drawn(&state, &fonts);

    assert!(stroked.width() > plain.width(), "the picture grows by half the stroke");
    assert!(
        ink_columns(&stroked).len() > ink_columns(&plain).len(),
        "and the letter grows with it"
    );
}

#[test]
fn two_lines_leave_a_gap_between_them() {
    let fonts = library();
    let image = drawn(&text("no\nno"), &fonts);
    let rows = ink_rows(&image);
    // Lowercase letters with no descenders leave the space between the baseline
    // of one line and the x-height of the next empty.
    let gaps = rows.windows(2).filter(|w| w[1] - w[0] > 1).count();
    assert_eq!(gaps, 1, "one band of ink per line: {rows:?}");
}

#[test]
fn alignment_puts_a_short_line_where_it_says() {
    let fonts = library();
    let mut state = text("hi");
    state.size = 24.0;
    state.wrap_width = Some(400.0);

    let left = ink_columns(&drawn(&state, &fonts));
    state.align = TextAlign::Center;
    let centre = ink_columns(&drawn(&state, &fonts));
    state.align = TextAlign::Right;
    let right = ink_columns(&drawn(&state, &fonts));

    assert!(left[0] < centre[0], "centring moves it right of the left edge");
    assert!(centre[0] < right[0], "and aligning right moves it further still");
    let end = |columns: &[u32]| *columns.last().expect("ink");
    assert!(end(&right) >= 395, "a right-aligned line ends at the right edge of the box");
}

#[test]
fn a_size_of_zero_draws_nothing_rather_than_dividing_by_it() {
    let fonts = library();
    let mut state = text("Verge");
    state.size = 0.0;
    assert!(measure(&state, &fonts).is_none());
    assert!(draw_with(&GraphicState::Text(state), &fonts).is_none());
}

#[test]
fn a_missing_family_draws_in_something_else_rather_than_nothing() {
    let fonts = library();
    let mut state = text("Verge");
    state.font = FontSpec::new("Definitely Not Installed");
    let image = drawn(&state, &fonts);
    assert!(!ink_columns(&image).is_empty(), "substituted, not skipped");
}
