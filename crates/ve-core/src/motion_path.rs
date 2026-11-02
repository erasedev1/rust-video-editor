//! A curve for a clip to travel along.
//!
//! # Why a path, rather than more position keyframes
//!
//! Position keyframes interpolate *between* values: three keyframes make two
//! straight lines (or eased lines) with a corner at the middle one. To get a
//! swoop, an editor either places dozens of keyframes or is given a curve. A
//! [`MotionPath`] is that curve: bezier points with handles, drawn through
//! space, with a single animated number — **progress** — saying how far along it
//! the clip is.
//!
//! That split is the point. *Where* the clip goes (the shape) and *when* it gets
//! there (progress, an ordinary [`Property`] with every interpolation the editor
//! has) are separate, so easing the move is easing a number, and reshaping the
//! route never disturbs the timing.
//!
//! # Progress is by distance, not by segment
//!
//! Progress 0.5 is **half the length of the path**, not the join between the
//! first and second segments. Sampling by parameter would make the clip race
//! over a long segment and crawl over a short one at a steady progress rate,
//! and moving a handle would change its speed. Sampling by arc length makes
//! progress a speed the user can reason about, at the price of measuring the
//! curve — flattened to short lines, which is the same approximation any
//! rasteriser would draw it with.
//!
//! [`Property`]: crate::Property

use serde::{Deserialize, Serialize};

use crate::geometry::Vec2;

/// How many straight pieces each bezier segment is measured as.
///
/// Thirty-two keeps the error against the true length well under a pixel for
/// the sizes a canvas has, and keeps a sample cheap enough to take for every
/// motion-blur sub-frame.
const FLATTEN_STEPS: usize = 32;

/// One vertex of the path, with the handles that shape the curve either side.
///
/// Handles are **relative to the point**, so dragging a point carries its
/// handles along and the curve keeps its shape.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct PathPoint {
    /// Where the path passes through, as an offset from the clip's position
    /// property — in composition pixels.
    pub position: Vec2,
    /// The handle on the way in, relative to `position`.
    #[serde(default)]
    pub handle_in: Vec2,
    /// The handle on the way out, relative to `position`.
    #[serde(default)]
    pub handle_out: Vec2,
}

impl PathPoint {
    /// A corner: no handles, so the segments either side are straight there.
    pub fn corner(x: f64, y: f64) -> Self {
        PathPoint { position: Vec2::new(x, y), handle_in: Vec2::ZERO, handle_out: Vec2::ZERO }
    }

    /// A smooth point, with handles of equal length pointing opposite ways
    /// along `tangent`, so the curve passes through it without a kink.
    pub fn smooth(x: f64, y: f64, tangent: Vec2) -> Self {
        PathPoint {
            position: Vec2::new(x, y),
            handle_in: Vec2::new(-tangent.x, -tangent.y),
            handle_out: tangent,
        }
    }
}

/// An open curve through a list of points.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct MotionPath {
    pub points: Vec<PathPoint>,
}

fn cubic(p0: Vec2, p1: Vec2, p2: Vec2, p3: Vec2, t: f64) -> Vec2 {
    let u = 1.0 - t;
    let (a, b, c, d) = (u * u * u, 3.0 * u * u * t, 3.0 * u * t * t, t * t * t);
    Vec2::new(
        a * p0.x + b * p1.x + c * p2.x + d * p3.x,
        a * p0.y + b * p1.y + c * p2.y + d * p3.y,
    )
}

fn add(a: Vec2, b: Vec2) -> Vec2 {
    Vec2::new(a.x + b.x, a.y + b.y)
}

impl MotionPath {
    /// A straight path between two points: the least a path can be.
    pub fn line(from: Vec2, to: Vec2) -> Self {
        MotionPath {
            points: vec![PathPoint::corner(from.x, from.y), PathPoint::corner(to.x, to.y)],
        }
    }

    /// The number of bezier segments.
    pub fn segments(&self) -> usize {
        self.points.len().saturating_sub(1)
    }

    /// The control points of segment `i`.
    fn segment(&self, i: usize) -> [Vec2; 4] {
        let (a, b) = (&self.points[i], &self.points[i + 1]);
        [a.position, add(a.position, a.handle_out), add(b.position, b.handle_in), b.position]
    }

    /// The measured polyline: every flattened vertex, with the distance
    /// travelled to reach it.
    fn measure(&self) -> Vec<(Vec2, f64)> {
        let mut out = Vec::with_capacity(self.segments() * FLATTEN_STEPS + 1);
        let Some(first) = self.points.first() else { return out };
        out.push((first.position, 0.0));
        let mut total = 0.0;
        for i in 0..self.segments() {
            let [p0, p1, p2, p3] = self.segment(i);
            for step in 1..=FLATTEN_STEPS {
                let at = cubic(p0, p1, p2, p3, step as f64 / FLATTEN_STEPS as f64);
                let (prev, _) = *out.last().expect("seeded with the first point");
                total += ((at.x - prev.x).powi(2) + (at.y - prev.y).powi(2)).sqrt();
                out.push((at, total));
            }
        }
        out
    }

    /// The length of the path in pixels.
    pub fn length(&self) -> f64 {
        self.measure().last().map_or(0.0, |&(_, d)| d)
    }

    /// The point `progress` of the way along the path *by distance*, clamped to
    /// the ends. An empty path is the origin; a single point is that point.
    pub fn sample(&self, progress: f64) -> Vec2 {
        let measured = self.measure();
        let (Some(&(first, _)), Some(&(last, total))) = (measured.first(), measured.last())
        else {
            return Vec2::ZERO;
        };
        let progress = if progress.is_finite() { progress.clamp(0.0, 1.0) } else { 0.0 };
        if total <= 0.0 {
            return first;
        }
        if progress >= 1.0 {
            return last;
        }
        let target = progress * total;
        // First vertex at or past the target distance; the point lies on the
        // piece that ends there.
        let hi = measured.partition_point(|&(_, d)| d < target).clamp(1, measured.len() - 1);
        let ((a, da), (b, db)) = (measured[hi - 1], measured[hi]);
        let f = if db > da { (target - da) / (db - da) } else { 0.0 };
        Vec2::new(a.x + (b.x - a.x) * f, a.y + (b.y - a.y) * f)
    }

    /// Whether point `i` has any handle at all.
    pub fn is_smooth(&self, i: usize) -> bool {
        self.points
            .get(i)
            .is_some_and(|p| p.handle_in != Vec2::ZERO || p.handle_out != Vec2::ZERO)
    }

    /// Turns point `i` from a corner into a smooth point, or back.
    ///
    /// Smoothing points the handles along the line between the neighbours — the
    /// direction the path is passing through — each a third of the way to its
    /// neighbour, which is the classic rule that gives a round, unsurprising
    /// curve. An end point has only one neighbour and takes its tangent from
    /// that.
    pub fn toggle_smooth(&mut self, i: usize) {
        let n = self.points.len();
        if i >= n {
            return;
        }
        if self.is_smooth(i) {
            self.points[i].handle_in = Vec2::ZERO;
            self.points[i].handle_out = Vec2::ZERO;
            return;
        }
        let here = self.points[i].position;
        let prev = if i > 0 { self.points[i - 1].position } else { here };
        let next = if i + 1 < n { self.points[i + 1].position } else { here };
        let tangent = Vec2::new((next.x - prev.x) / 6.0, (next.y - prev.y) / 6.0);
        self.points[i].handle_in = Vec2::new(-tangent.x, -tangent.y);
        self.points[i].handle_out = tangent;
    }

    /// Adds a point on the end, continuing in a straight line.
    pub fn push_corner(&mut self, x: f64, y: f64) {
        self.points.push(PathPoint::corner(x, y));
    }
}

impl std::hash::Hash for MotionPath {
    fn hash<H: std::hash::Hasher>(&self, state: &mut H) {
        self.points.len().hash(state);
        for p in &self.points {
            for v in [p.position, p.handle_in, p.handle_out] {
                v.x.to_bits().hash(state);
                v.y.to_bits().hash(state);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn close(a: Vec2, b: Vec2) -> bool {
        (a.x - b.x).abs() < 1e-6 && (a.y - b.y).abs() < 1e-6
    }

    #[test]
    fn a_straight_path_is_sampled_linearly() {
        let path = MotionPath::line(Vec2::new(0.0, 0.0), Vec2::new(100.0, 50.0));
        assert!(close(path.sample(0.0), Vec2::new(0.0, 0.0)));
        assert!(close(path.sample(0.5), Vec2::new(50.0, 25.0)));
        assert!(close(path.sample(1.0), Vec2::new(100.0, 50.0)));
        assert!((path.length() - (100.0f64.powi(2) + 50.0f64.powi(2)).sqrt()).abs() < 1e-9);
    }

    #[test]
    fn progress_is_by_distance_not_by_segment() {
        // A long segment then a short one. Half way along by *distance* is on the
        // long segment; by segment it would be the join at x = 300.
        let path = MotionPath {
            points: vec![
                PathPoint::corner(0.0, 0.0),
                PathPoint::corner(300.0, 0.0),
                PathPoint::corner(300.0, 100.0),
            ],
        };
        assert!(close(path.sample(0.5), Vec2::new(200.0, 0.0)));
        assert!(close(path.sample(0.75), Vec2::new(300.0, 0.0)));
    }

    #[test]
    fn handles_bend_the_path_and_lengthen_it() {
        let mut path = MotionPath::line(Vec2::new(0.0, 0.0), Vec2::new(100.0, 0.0));
        let straight = path.length();
        path.points[0].handle_out = Vec2::new(0.0, 80.0);
        path.points[1].handle_in = Vec2::new(0.0, 80.0);
        assert!(path.length() > straight, "a swoop is longer than the line");
        let mid = path.sample(0.5);
        assert!((mid.x - 50.0).abs() < 1.0 && mid.y > 30.0, "{mid:?} bulges up");
        assert!(close(path.sample(1.0), Vec2::new(100.0, 0.0)), "still ends at its last point");
    }

    #[test]
    fn progress_outside_the_path_holds_at_the_ends() {
        let path = MotionPath::line(Vec2::new(10.0, 0.0), Vec2::new(20.0, 0.0));
        assert!(close(path.sample(-3.0), Vec2::new(10.0, 0.0)));
        assert!(close(path.sample(7.0), Vec2::new(20.0, 0.0)));
        assert!(close(path.sample(f64::NAN), Vec2::new(10.0, 0.0)));
    }

    #[test]
    fn smoothing_a_corner_gives_it_opposed_handles_and_toggling_again_removes_them() {
        let mut path = MotionPath {
            points: vec![
                PathPoint::corner(0.0, 0.0),
                PathPoint::corner(100.0, 100.0),
                PathPoint::corner(200.0, 0.0),
            ],
        };
        assert!(!path.is_smooth(1));
        path.toggle_smooth(1);
        assert!(path.is_smooth(1));
        let p = path.points[1];
        assert_eq!(p.handle_in.x, -p.handle_out.x);
        assert_eq!(p.handle_in.y, -p.handle_out.y);
        assert!(p.handle_out.x > 0.0, "points the way the path is going");
        path.toggle_smooth(1);
        assert!(!path.is_smooth(1));
        path.toggle_smooth(99); // out of range: nothing to do, nothing to panic
    }

    #[test]
    fn degenerate_paths_do_not_panic() {
        assert!(close(MotionPath::default().sample(0.5), Vec2::ZERO));
        let one = MotionPath { points: vec![PathPoint::corner(5.0, 6.0)] };
        assert!(close(one.sample(0.5), Vec2::new(5.0, 6.0)));
        let stacked = MotionPath::line(Vec2::new(1.0, 1.0), Vec2::new(1.0, 1.0));
        assert!(close(stacked.sample(0.5), Vec2::new(1.0, 1.0)));
    }
}
