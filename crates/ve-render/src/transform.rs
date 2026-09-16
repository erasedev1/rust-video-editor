//! Building the per-layer matrix.

use ve_core::{Size, TransformState, Vec2};

/// A column-major 4x4 matrix, laid out the way WGSL expects it.
pub type Matrix4 = [[f32; 4]; 4];

pub const IDENTITY: Matrix4 =
    [[1.0, 0.0, 0.0, 0.0], [0.0, 1.0, 0.0, 0.0], [0.0, 0.0, 1.0, 0.0], [0.0, 0.0, 0.0, 1.0]];

/// Composes the matrix that maps the unit quad `[0,1]^2` onto clip space for
/// one layer.
///
/// The chain, in order:
///
/// 1. Unit quad to source pixels: `q * (sw, sh)`.
/// 2. Move the anchor point to the origin, so scale and rotation act about it
///    rather than about the corner.
/// 3. Scale, then rotate.
/// 4. Translate by the layer's position, in composition pixels measured from
///    the composition's centre.
/// 5. Composition pixels to clip space, which flips Y because clip space is
///    Y-up and every image coordinate system in the editor is Y-down.
///
/// Defaults — anchor `(0.5, 0.5)`, scale `1`, position `(0, 0)` — put the
/// source at its natural size, centred, which is what dropping a clip onto a
/// matching sequence should look like.
pub fn layer_matrix(
    source: Size,
    composition: Size,
    transform: &TransformState,
) -> Matrix4 {
    let (sw, sh) = (source.width as f32, source.height as f32);
    let (cw, ch) = (composition.width.max(1) as f32, composition.height.max(1) as f32);

    let anchor = Vec2::new(transform.anchor.x * sw as f64, transform.anchor.y * sh as f64);
    let (sx, sy) = (transform.scale.x as f32, transform.scale.y as f32);
    let theta = (transform.rotation as f32).to_radians();
    let (sin, cos) = theta.sin_cos();

    // Steps 1 and 2 fold into a single affine map of the unit quad:
    //   p(q) = q * (sw, sh) - anchor
    // Steps 3 and 4 then apply  R * S * p + position,
    // and step 5 divides by half the composition size.
    //
    // Written out, the matrix columns below are the images of the unit basis
    // vectors and of the origin under that whole chain.
    let ax = -anchor.x as f32;
    let ay = -anchor.y as f32;

    // Column for the quad's x axis: (sw, 0) scaled then rotated.
    let x_axis_x = cos * sx * sw;
    let x_axis_y = sin * sx * sw;
    // Column for the quad's y axis: (0, sh) scaled then rotated.
    let y_axis_x = -sin * sy * sh;
    let y_axis_y = cos * sy * sh;
    // Where the quad's origin lands: the anchor offset, scaled and rotated,
    // then displaced by the layer position.
    let origin_x = cos * sx * ax - sin * sy * ay + transform.position.x as f32;
    let origin_y = sin * sx * ax + cos * sy * ay + transform.position.y as f32;

    // Composition pixels to clip space: x / (cw/2), and y negated because clip
    // space puts +Y at the top.
    let kx = 2.0 / cw;
    let ky = -2.0 / ch;

    [
        [x_axis_x * kx, x_axis_y * ky, 0.0, 0.0],
        [y_axis_x * kx, y_axis_y * ky, 0.0, 0.0],
        [0.0, 0.0, 1.0, 0.0],
        [origin_x * kx, origin_y * ky, 0.0, 1.0],
    ]
}

/// Applies a matrix to a point on the unit quad, for tests and hit-testing.
pub fn apply(m: &Matrix4, x: f32, y: f32) -> (f32, f32) {
    (
        m[0][0] * x + m[1][0] * y + m[3][0],
        m[0][1] * x + m[1][1] * y + m[3][1],
    )
}

/// The scale that fits `source` inside `target` without cropping or distorting.
///
/// Used when a clip's resolution does not match the sequence's, which is the
/// normal case for mixed-format timelines.
pub fn fit_scale(source: Size, target: Size) -> Vec2 {
    if source.width == 0 || source.height == 0 {
        return Vec2::ONE;
    }
    let sx = target.width as f64 / source.width as f64;
    let sy = target.height as f64 / source.height as f64;
    let s = sx.min(sy);
    Vec2::splat(s)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn state() -> TransformState {
        TransformState::default()
    }

    /// A source exactly filling the composition, with default transform, must
    /// map the unit quad onto the full clip-space square.
    #[test]
    fn a_matching_source_fills_the_frame() {
        let size = Size::new(1920, 1080);
        let m = layer_matrix(size, size, &state());

        let (x0, y0) = apply(&m, 0.0, 0.0);
        let (x1, y1) = apply(&m, 1.0, 1.0);
        assert!((x0 + 1.0).abs() < 1e-5, "left edge at {x0}");
        assert!((y0 - 1.0).abs() < 1e-5, "top edge at {y0}");
        assert!((x1 - 1.0).abs() < 1e-5, "right edge at {x1}");
        assert!((y1 + 1.0).abs() < 1e-5, "bottom edge at {y1}");
    }

    #[test]
    fn a_half_size_source_occupies_the_middle_quarter() {
        let m = layer_matrix(Size::new(960, 540), Size::new(1920, 1080), &state());
        let (x0, y0) = apply(&m, 0.0, 0.0);
        let (x1, y1) = apply(&m, 1.0, 1.0);
        assert!((x0 + 0.5).abs() < 1e-5);
        assert!((y0 - 0.5).abs() < 1e-5);
        assert!((x1 - 0.5).abs() < 1e-5);
        assert!((y1 + 0.5).abs() < 1e-5);
    }

    #[test]
    fn position_moves_the_layer_in_composition_pixels() {
        let size = Size::new(200, 100);
        let mut t = state();
        t.position = Vec2::new(100.0, 50.0);
        let m = layer_matrix(size, size, &t);

        // Moving right by half the width puts the centre at clip x = +0.5, and
        // down by half the height puts it at clip y = -0.5.
        let (cx, cy) = apply(&m, 0.5, 0.5);
        assert!((cx - 1.0).abs() < 1e-5, "centre x {cx}");
        assert!((cy + 1.0).abs() < 1e-5, "centre y {cy}");
    }

    #[test]
    fn scale_grows_about_the_anchor() {
        let size = Size::new(100, 100);
        let mut t = state();
        t.scale = Vec2::splat(2.0);
        let m = layer_matrix(size, size, &t);

        // With the default centre anchor, doubling keeps the centre put and
        // pushes the corners out to twice clip space.
        let (cx, cy) = apply(&m, 0.5, 0.5);
        assert!(cx.abs() < 1e-5 && cy.abs() < 1e-5, "centre drifted to ({cx}, {cy})");
        let (x1, _) = apply(&m, 1.0, 1.0);
        assert!((x1 - 2.0).abs() < 1e-5, "right edge at {x1}");
    }

    #[test]
    fn scale_about_a_corner_anchor_keeps_that_corner_fixed() {
        let size = Size::new(100, 100);
        let mut t = state();
        t.anchor = Vec2::new(0.0, 0.0);
        t.scale = Vec2::splat(2.0);
        let m = layer_matrix(size, size, &t);

        // The anchored corner lands where the layer's origin was placed, which
        // with zero position is the composition centre.
        let (x0, y0) = apply(&m, 0.0, 0.0);
        assert!(x0.abs() < 1e-5 && y0.abs() < 1e-5, "anchor moved to ({x0}, {y0})");
    }

    #[test]
    fn rotation_is_clockwise_in_image_space() {
        let size = Size::new(100, 100);
        let mut t = state();
        t.rotation = 90.0;
        let m = layer_matrix(size, size, &t);

        // The top-left corner of a square rotated 90 degrees clockwise about
        // its centre becomes the top-right corner.
        let (x, y) = apply(&m, 0.0, 0.0);
        assert!((x - 1.0).abs() < 1e-5, "x {x}");
        assert!((y - 1.0).abs() < 1e-5, "y {y}");
    }

    #[test]
    fn a_full_turn_returns_to_the_identity() {
        let size = Size::new(640, 480);
        let mut t = state();
        t.rotation = 360.0;
        let m = layer_matrix(size, size, &t);
        let identity = layer_matrix(size, size, &state());
        for (a, b) in m.iter().flatten().zip(identity.iter().flatten()) {
            assert!((a - b).abs() < 1e-4, "{a} vs {b}");
        }
    }

    #[test]
    fn rotation_preserves_area() {
        let size = Size::new(200, 100);
        let mut t = state();
        t.rotation = 37.0;
        let m = layer_matrix(size, size, &t);

        // The determinant of the 2x2 upper-left block is the area scale factor,
        // which rotation must leave alone.
        let det = m[0][0] * m[1][1] - m[1][0] * m[0][1];
        let identity = layer_matrix(size, size, &state());
        let det0 = identity[0][0] * identity[1][1] - identity[1][0] * identity[0][1];
        assert!((det - det0).abs() < 1e-5, "{det} vs {det0}");
    }

    #[test]
    fn non_uniform_scale_stretches_each_axis_independently() {
        let size = Size::new(100, 100);
        let mut t = state();
        t.scale = Vec2::new(2.0, 0.5);
        let m = layer_matrix(size, size, &t);
        let (x1, y1) = apply(&m, 1.0, 1.0);
        assert!((x1 - 2.0).abs() < 1e-5, "x {x1}");
        assert!((y1 + 0.5).abs() < 1e-5, "y {y1}");
    }

    #[test]
    fn fit_scale_letterboxes_rather_than_cropping() {
        // 4:3 into 16:9 must fit by height, leaving pillarbox bars.
        let s = fit_scale(Size::new(640, 480), Size::new(1920, 1080));
        assert!((s.x - 2.25).abs() < 1e-9, "{s:?}");
        assert_eq!(s.x, s.y, "aspect ratio must be preserved");

        // 16:9 into 16:9 is exactly 1:1 at matching resolution.
        let s = fit_scale(Size::new(1920, 1080), Size::new(1920, 1080));
        assert!((s.x - 1.0).abs() < 1e-9);

        // Degenerate input must not divide by zero.
        assert_eq!(fit_scale(Size::new(0, 0), Size::new(100, 100)), Vec2::ONE);
    }

    #[test]
    fn a_composition_of_zero_size_does_not_produce_nans() {
        let m = layer_matrix(Size::new(100, 100), Size::new(0, 0), &state());
        assert!(m.iter().flatten().all(|v| v.is_finite()), "{m:?}");
    }
}
