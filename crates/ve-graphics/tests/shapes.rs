//! What a shape actually puts on the canvas.
//!
//! These read pixels rather than geometry: a path that is right and a picture
//! that is wrong is exactly the failure a test over the path alone would miss.

use ve_core::{GraphicState, Rgba, ShapeKind, ShapeState, Vec2};
use ve_graphics::{draw, RasterImage};

fn shape(kind: ShapeKind, w: f64, h: f64, fill: Rgba) -> ShapeState {
    ShapeState {
        kind,
        size: Vec2::new(w, h),
        corner_radius: 0.0,
        inner_radius: 0.382,
        fill,
        stroke: Rgba::TRANSPARENT,
        stroke_width: 0.0,
    }
}

fn drawn(state: ShapeState) -> RasterImage {
    draw(&GraphicState::Shape(state)).expect("something to draw")
}

fn centre(image: &RasterImage) -> [u8; 4] {
    image.pixel(image.width() / 2, image.height() / 2).expect("a middle")
}

#[test]
fn a_filled_rectangle_is_solid_in_the_middle_and_empty_outside_it() {
    let image = drawn(shape(ShapeKind::Rectangle, 60.0, 40.0, Rgba::new(1.0, 0.0, 0.0, 1.0)));
    assert_eq!(centre(&image), [255, 0, 0, 255]);
    // The corner of the picture is the anti-alias margin, which nothing
    // reaches.
    assert_eq!(image.pixel(0, 0).expect("a corner")[3], 0);
}

#[test]
fn alpha_comes_back_straight_rather_than_premultiplied() {
    // The whole reason this crate demultiplies: a half-transparent red is
    // "red, half covered", not "half-red, fully covered". A premultiplied
    // buffer would answer 128 in the red channel here, and the compositor,
    // which multiplies by alpha itself, would draw it at a quarter strength.
    let image = drawn(shape(ShapeKind::Rectangle, 40.0, 40.0, Rgba::new(1.0, 0.0, 0.0, 0.5)));
    let [r, g, b, a] = centre(&image);
    assert_eq!((g, b), (0, 0));
    assert!(a.abs_diff(128) <= 1, "alpha {a}");
    assert!(r >= 254, "colour is not scaled by its own alpha: {r}");
}

#[test]
fn an_ellipse_leaves_the_corners_of_its_box_empty() {
    let size = 60.0;
    let rect = drawn(shape(ShapeKind::Rectangle, size, size, Rgba::WHITE));
    let ellipse = drawn(shape(ShapeKind::Ellipse, size, size, Rgba::WHITE));
    assert_eq!(rect.size(), ellipse.size(), "the same box either way");

    // One pixel inside the box's top-left corner: filled by the rectangle,
    // outside the inscribed ellipse.
    let (x, y) = (2, 2);
    assert_eq!(rect.pixel(x, y).expect("a corner")[3], 255);
    assert_eq!(ellipse.pixel(x, y).expect("a corner")[3], 0);
    assert_eq!(centre(&ellipse)[3], 255);
}

#[test]
fn rounding_a_corner_takes_it_off() {
    let mut state = shape(ShapeKind::Rectangle, 60.0, 60.0, Rgba::WHITE);
    state.corner_radius = 20.0;
    let rounded = drawn(state);
    assert_eq!(rounded.pixel(2, 2).expect("a corner")[3], 0);
    assert_eq!(centre(&rounded)[3], 255);
}

#[test]
fn a_corner_radius_past_half_the_side_is_a_stadium_rather_than_a_fold() {
    let mut state = shape(ShapeKind::Rectangle, 60.0, 60.0, Rgba::WHITE);
    state.corner_radius = 10_000.0;
    let circle = drawn(state);
    // Clamped to half the shorter side, which on a square is a circle.
    let ellipse = drawn(shape(ShapeKind::Ellipse, 60.0, 60.0, Rgba::WHITE));
    assert_eq!(circle.size(), ellipse.size());
    for (x, y) in [(2, 2), (30, 1), (1, 30)] {
        let a = circle.pixel(x, y).expect("in range")[3];
        let b = ellipse.pixel(x, y).expect("in range")[3];
        assert!(a.abs_diff(b) <= 2, "at {x},{y}: rounded {a}, ellipse {b}");
    }
}

#[test]
fn a_stroke_straddles_the_outline() {
    let mut state = shape(ShapeKind::Rectangle, 60.0, 60.0, Rgba::new(0.0, 0.0, 1.0, 1.0));
    state.stroke = Rgba::new(1.0, 0.0, 0.0, 1.0);
    state.stroke_width = 8.0;
    let image = drawn(state);

    // The picture is the shape plus half the stroke plus the margin, so the
    // shape's own left edge is 5px in. Two pixels either side of it are stroke.
    let edge = 5;
    for x in [edge - 2, edge + 2] {
        let px = image.pixel(x, image.height() / 2).expect("in range");
        assert_eq!(px, [255, 0, 0, 255], "at x={x}");
    }
    // Well inside is the fill, not the stroke.
    assert_eq!(centre(&image), [0, 0, 255, 255]);
}

#[test]
fn a_stroke_alone_is_a_hollow_shape() {
    let mut state = shape(ShapeKind::Ellipse, 80.0, 80.0, Rgba::TRANSPARENT);
    state.stroke = Rgba::WHITE;
    state.stroke_width = 4.0;
    let image = drawn(state);
    assert_eq!(centre(&image)[3], 0, "no fill means nothing in the middle");
    // The left edge of the circle, at its vertical centre.
    let px = image.pixel(3, image.height() / 2).expect("in range");
    assert_eq!(px[3], 255);
}

#[test]
fn a_star_reaches_the_top_of_its_box_and_leaves_the_corners() {
    let image = drawn(shape(ShapeKind::Star { points: 5 }, 100.0, 100.0, Rgba::WHITE));
    let top_middle = image.pixel(image.width() / 2, 2).expect("in range");
    assert!(top_middle[3] > 0, "the first point is straight up");
    assert_eq!(image.pixel(2, 2).expect("in range")[3], 0, "a star has no corners");
}

#[test]
fn a_polygon_fills_more_of_its_box_the_more_sides_it_has() {
    let coverage = |sides: u32| {
        let image = drawn(shape(ShapeKind::Polygon { sides }, 100.0, 100.0, Rgba::WHITE));
        (0..image.height())
            .flat_map(|y| (0..image.width()).map(move |x| (x, y)))
            .filter(|(x, y)| image.pixel(*x, *y).expect("in range")[3] > 127)
            .count()
    };
    let triangle = coverage(3);
    let octagon = coverage(8);
    assert!(triangle < octagon, "triangle {triangle}, octagon {octagon}");
    // An octagon in a 100px box covers about 82% of it; a triangle about 32%.
    assert!(octagon < 100 * 100, "and neither fills the box");
}
