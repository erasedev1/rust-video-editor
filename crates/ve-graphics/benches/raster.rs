//! Benchmarks for drawing shapes and text.
//!
//! Three claims are being kept honest here.
//!
//! **A graphic is rasterised on the CPU, so it has to be cheap enough to sit on
//! the frame path.** Everything else the editor draws per frame is on the GPU;
//! this is not, because outlining a glyph and filling a path are work that
//! belongs where the fonts and the path library live. What that costs is
//! therefore worth knowing exactly.
//!
//! **What it actually costs, on a moving timeline, is the cache.** A title
//! sitting still is the common case by a wide margin — a lower third is on
//! screen for five seconds and changes on none of those frames — so the figure
//! that decides whether this is affordable is the hit, not the draw.
//!
//! **Text is not shapes.** Laying out a run means shaping it with the font's
//! own tables, which is a different order of work from filling a polygon, and
//! quoting one number for "a graphic" would hide that.

use std::hint::black_box;

use criterion::{criterion_group, criterion_main, BenchmarkId, Criterion};
use ve_core::{Graphic, GraphicContent, GraphicId, Rgba, Shape, ShapeKind, Text, Vec2};
use ve_graphics::{draw, Rasteriser};
use ve_time::Ticks;

fn shape_graphic(kind: ShapeKind, size: f64) -> Graphic {
    Graphic {
        id: GraphicId::from_raw(1),
        name: "bench".into(),
        content: GraphicContent::Shape(Shape::new(
            kind,
            Vec2::new(size, size),
            Rgba::new(1.0, 0.8, 0.0, 1.0),
        )),
    }
}

fn text_graphic(words: &str, size: f64) -> Graphic {
    Graphic {
        id: GraphicId::from_raw(2),
        name: "bench".into(),
        content: GraphicContent::Text(Text::new(words, size, Rgba::WHITE)),
    }
}

/// Filling a path, by kind and by size.
///
/// Size is the interesting axis: a fill is bounded by the pixels it covers, so
/// the cost should follow the area rather than the outline's complexity.
fn bench_shapes(c: &mut Criterion) {
    let mut group = c.benchmark_group("raster_shape");
    for size in [200.0f64, 800.0] {
        for (label, kind) in [
            ("rectangle", ShapeKind::Rectangle),
            ("ellipse", ShapeKind::Ellipse),
            ("star", ShapeKind::Star { points: 5 }),
        ] {
            let graphic = shape_graphic(kind, size);
            let state = graphic.evaluate(Ticks::ZERO);
            group.bench_with_input(BenchmarkId::new(label, size as u32), &state, |b, state| {
                b.iter(|| black_box(draw(black_box(state))));
            });
        }
    }
    group.finish();
}

/// A corner radius turns a rectangle into four arcs and four lines, which is a
/// different path to fill. Worth knowing whether rounding one is free.
fn bench_corner_radius(c: &mut Criterion) {
    let mut group = c.benchmark_group("raster_corner");
    for radius in [0.0f64, 40.0] {
        let mut graphic = shape_graphic(ShapeKind::Rectangle, 800.0);
        if let GraphicContent::Shape(shape) = &mut graphic.content {
            shape.corner_radius.value = radius;
        }
        let state = graphic.evaluate(Ticks::ZERO);
        group.bench_with_input(
            BenchmarkId::from_parameter(radius as u32),
            &state,
            |b, state| {
                b.iter(|| black_box(draw(black_box(state))));
            },
        );
    }
    group.finish();
}

/// Laying out and filling a run of text, by how much of it there is.
fn bench_text(c: &mut Criterion) {
    let lines = [
        ("short", "Ada Lovelace"),
        ("line", "The Analytical Engine weaves algebraic patterns"),
        (
            "paragraph",
            "The Analytical Engine weaves algebraic patterns\njust as the Jacquard loom weaves\n\
             flowers and leaves, and we may say most aptly\nthat it does so with a generality\n\
             that the calculating engines of the past could not.",
        ),
    ];

    let mut group = c.benchmark_group("raster_text");
    for (label, words) in lines {
        let graphic = text_graphic(words, 72.0);
        let state = graphic.evaluate(Ticks::ZERO);
        group.bench_with_input(BenchmarkId::from_parameter(label), &state, |b, state| {
            b.iter(|| black_box(draw(black_box(state))));
        });
    }
    group.finish();
}

/// The figure that actually decides whether graphics are affordable on the
/// frame path: what a held title costs on the frames where nothing changed.
fn bench_cache(c: &mut Criterion) {
    let mut group = c.benchmark_group("raster_cache");

    let graphic = text_graphic("Ada Lovelace", 72.0);
    let state = graphic.evaluate(Ticks::ZERO);

    let mut warm = Rasteriser::with_budget_mb(64);
    // Drawn once so the measured calls are hits.
    warm.picture(&state);
    assert!(warm.contains(&state), "the fixture must be cached to measure a hit");
    group.bench_function("hit", |b| {
        b.iter(|| black_box(warm.picture(black_box(&state))));
    });

    group.bench_function("miss", |b| {
        b.iter(|| {
            let mut cold = Rasteriser::with_budget_mb(64);
            black_box(cold.picture(black_box(&state)))
        });
    });

    group.finish();
}

criterion_group!(benches, bench_shapes, bench_corner_radius, bench_text, bench_cache);
criterion_main!(benches);
