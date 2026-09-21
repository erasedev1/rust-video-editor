//! Turning a resolved shape into an outline, and that outline into pixels.
//!
//! Every kind is a path, and every path is filled and then stroked in that
//! order — the stroke sits *on* the outline, half inside and half outside, so
//! drawing it second is what makes an outline read as an outline rather than as
//! a line the fill has grown over.

use tiny_skia::{FillRule, Paint, PathBuilder, Pixmap, Stroke, Transform};
use ve_core::{ShapeKind, ShapeState};

use crate::{colour, image_from_pixmap, RasterImage, MAX_DIMENSION};

/// How far outside the shape the picture extends beyond its stroke, in pixels.
///
/// One pixel, for the coverage an anti-aliased edge writes past the geometry.
/// Without it a shape's outermost row of samples would be clipped by the edge
/// of its own image, which reads as a hard line down one side of a soft shape.
const ANTIALIAS_MARGIN: f32 = 1.0;

/// The circular-arc constant: the distance along each tangent that makes a
/// cubic Bezier match a quarter circle to within a ten-thousandth of its
/// radius. `4/3 · (√2 − 1)`.
const KAPPA: f32 = 0.552_284_8;

/// The outline of a shape, with its bounding box at `left`, `top`.
///
/// Public because it is what a test asserts against and what a benchmark times
/// without the fill: the geometry of a star is arithmetic worth checking on its
/// own, separately from what the rasteriser does with it.
pub fn shape_path(state: &ShapeState, left: f32, top: f32) -> Option<tiny_skia::Path> {
    let w = state.size.x as f32;
    let h = state.size.y as f32;
    if !(w.is_finite() && h.is_finite()) || w <= 0.0 || h <= 0.0 {
        return None;
    }
    let (right, bottom) = (left + w, top + h);
    let mut path = PathBuilder::new();

    match state.kind {
        ShapeKind::Rectangle => {
            // Clamped to half the shorter side, where the corners meet and the
            // rectangle becomes a stadium. Past that the arcs would cross and
            // the outline would fold through itself.
            let radius = (state.corner_radius as f32).clamp(0.0, w.min(h) / 2.0);
            if radius <= 0.0 {
                let rect = tiny_skia::Rect::from_ltrb(left, top, right, bottom)?;
                path.push_rect(rect);
            } else {
                rounded_rect(&mut path, left, top, right, bottom, radius);
            }
        }
        ShapeKind::Ellipse => {
            let rect = tiny_skia::Rect::from_ltrb(left, top, right, bottom)?;
            path.push_oval(rect);
        }
        ShapeKind::Polygon { sides } => {
            let n = sides.max(ShapeKind::MIN_POINTS);
            radial(&mut path, left, top, w, h, n, &[1.0]);
        }
        ShapeKind::Star { points } => {
            let n = points.max(ShapeKind::MIN_POINTS);
            // A notch at zero would put every inner vertex on the centre and
            // draw a sunburst of degenerate spikes; at one it is the polygon.
            let inner = (state.inner_radius as f32).clamp(0.02, 1.0);
            radial(&mut path, left, top, w, h, n, &[1.0, inner]);
        }
    }

    path.finish()
}

/// A rectangle with round corners, drawn as four sides and four quarter arcs.
fn rounded_rect(path: &mut PathBuilder, left: f32, top: f32, right: f32, bottom: f32, r: f32) {
    let c = r * KAPPA;
    path.move_to(left + r, top);
    path.line_to(right - r, top);
    path.cubic_to(right - r + c, top, right, top + r - c, right, top + r);
    path.line_to(right, bottom - r);
    path.cubic_to(right, bottom - r + c, right - r + c, bottom, right - r, bottom);
    path.line_to(left + r, bottom);
    path.cubic_to(left + r - c, bottom, left, bottom - r + c, left, bottom - r);
    path.line_to(left, top + r);
    path.cubic_to(left, top + r - c, left + r - c, top, left + r, top);
    path.close();
}

/// A closed outline of `count · radii.len()` vertices around the centre of the
/// box, the first one pointing straight up.
///
/// Both a polygon and a star are this: a polygon cycles through one radius, a
/// star alternates between two. Point-up because that is what every shape tool
/// draws and what a user expects a five-pointed star to look like.
///
/// The vertices sit on the *ellipse* inscribed in the box rather than on a
/// circle, so a polygon stretched into a wide box stretches with it instead of
/// staying round inside it.
fn radial(
    path: &mut PathBuilder,
    left: f32,
    top: f32,
    w: f32,
    h: f32,
    count: u32,
    radii: &[f32],
) {
    let (cx, cy) = (left + w / 2.0, top + h / 2.0);
    let (rx, ry) = (w / 2.0, h / 2.0);
    let total = count as usize * radii.len();
    let step = std::f32::consts::TAU / total as f32;
    for i in 0..total {
        let angle = -std::f32::consts::FRAC_PI_2 + step * i as f32;
        let scale = radii[i % radii.len()];
        let (x, y) = (cx + rx * scale * angle.cos(), cy + ry * scale * angle.sin());
        if i == 0 {
            path.move_to(x, y);
        } else {
            path.line_to(x, y);
        }
    }
    path.close();
}

/// Draws a shape into a picture of its own bounding box.
///
/// `None` when there is nothing to draw — no area, or neither a fill nor a
/// stroke that would put down a pixel — and when the picture would be larger
/// than [`MAX_DIMENSION`] on a side.
pub(crate) fn draw_shape(state: &ShapeState) -> Option<RasterImage> {
    if !state.has_fill() && !state.has_stroke() {
        return None;
    }

    let w = state.size.x as f32;
    let h = state.size.y as f32;
    if !(w.is_finite() && h.is_finite()) || w <= 0.0 || h <= 0.0 {
        return None;
    }

    // Half the stroke falls outside the outline, so the picture has to be that
    // much bigger than the shape — plus the margin an anti-aliased edge writes
    // into.
    let stroke_width = if state.has_stroke() { state.stroke_width as f32 } else { 0.0 };
    let margin = stroke_width / 2.0 + ANTIALIAS_MARGIN;
    let (width, height) = ((w + margin * 2.0).ceil() as u64, (h + margin * 2.0).ceil() as u64);
    if width > MAX_DIMENSION as u64 || height > MAX_DIMENSION as u64 {
        log::warn!("graphic is {width}×{height}, past the {MAX_DIMENSION}px limit; not drawn");
        return None;
    }

    let path = shape_path(state, margin, margin)?;
    let mut pixmap = Pixmap::new(width as u32, height as u32)?;

    if state.has_fill() {
        let mut paint = Paint { anti_alias: true, ..Paint::default() };
        paint.set_color(colour(state.fill));
        // Winding rather than even-odd: a star's outline does not cross itself,
        // and a hand-edited one that does should fill solid rather than punch a
        // hole through its own middle.
        pixmap.fill_path(&path, &paint, FillRule::Winding, Transform::identity(), None);
    }

    if state.has_stroke() {
        let mut paint = Paint { anti_alias: true, ..Paint::default() };
        paint.set_color(colour(state.stroke));
        let stroke = Stroke { width: stroke_width, ..Stroke::default() };
        pixmap.stroke_path(&path, &paint, &stroke, Transform::identity(), None);
    }

    Some(image_from_pixmap(pixmap))
}

#[cfg(test)]
mod tests {
    use super::*;
    use ve_core::{Rgba, Vec2};

    fn rect(w: f64, h: f64) -> ShapeState {
        ShapeState {
            kind: ShapeKind::Rectangle,
            size: Vec2::new(w, h),
            corner_radius: 0.0,
            inner_radius: 0.382,
            fill: Rgba::WHITE,
            stroke: Rgba::TRANSPARENT,
            stroke_width: 0.0,
        }
    }

    #[test]
    fn a_picture_is_the_shape_plus_its_antialias_margin() {
        let image = draw_shape(&rect(100.0, 40.0)).expect("a filled rectangle draws");
        assert_eq!(image.size(), ve_core::Size::new(102, 42));
    }

    #[test]
    fn a_stroke_widens_the_picture_by_half_its_width_on_each_side() {
        let mut state = rect(100.0, 40.0);
        state.stroke = Rgba::BLACK;
        state.stroke_width = 10.0;
        let image = draw_shape(&state).expect("a stroked rectangle draws");
        assert_eq!(image.size(), ve_core::Size::new(112, 52));
    }

    #[test]
    fn a_shape_with_no_area_draws_nothing() {
        assert!(draw_shape(&rect(0.0, 40.0)).is_none());
        assert!(draw_shape(&rect(100.0, -1.0)).is_none());
        assert!(draw_shape(&rect(f64::NAN, 40.0)).is_none());
    }

    #[test]
    fn a_shape_with_neither_fill_nor_stroke_draws_nothing() {
        let mut state = rect(100.0, 40.0);
        state.fill = Rgba::TRANSPARENT;
        assert!(draw_shape(&state).is_none());

        // A stroke colour with no width is still nothing.
        state.stroke = Rgba::BLACK;
        assert!(draw_shape(&state).is_none());
    }

    #[test]
    fn an_absurd_size_is_refused_rather_than_allocated() {
        assert!(draw_shape(&rect(1e9, 1e9)).is_none());
    }

    #[test]
    fn a_polygon_has_one_vertex_per_side() {
        let mut state = rect(100.0, 100.0);
        state.kind = ShapeKind::Polygon { sides: 6 };
        let path = shape_path(&state, 0.0, 0.0).expect("a hexagon");
        // Six moves and lines, then the close.
        assert_eq!(path.len(), 7, "{path:?}");
    }

    #[test]
    fn a_star_alternates_between_two_radii() {
        let mut state = rect(100.0, 100.0);
        state.kind = ShapeKind::Star { points: 5 };
        let path = shape_path(&state, 0.0, 0.0).expect("a star");
        assert_eq!(path.len(), 11);
    }

    #[test]
    fn a_polygon_points_up() {
        let mut state = rect(100.0, 100.0);
        state.kind = ShapeKind::Polygon { sides: 5 };
        let path = shape_path(&state, 0.0, 0.0).expect("a pentagon");
        let first = path.points().first().copied().expect("a first point");
        assert!((first.x - 50.0).abs() < 1e-3, "{first:?}");
        assert!(first.y.abs() < 1e-3, "the first vertex sits on the top edge: {first:?}");
    }
}
