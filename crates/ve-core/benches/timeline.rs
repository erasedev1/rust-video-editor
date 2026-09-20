//! Benchmarks for the edit model's hot paths.
//!
//! These exist to keep the claims in the architecture notes honest. The
//! interesting property is *scaling*: a timeline query should cost what is on
//! screen, not what is in the project, so the 100-clip and 10,000-clip figures
//! for a range query should be close to each other.

use criterion::{criterion_group, criterion_main, BenchmarkId, Criterion};
use std::hint::black_box;
use ve_core::{Clip, Project, TrackId, TrackKind};
use ve_time::{Rate, Ticks, TimeRange};

/// A project with `clips` clips spread across four video tracks.
fn project_with_clips(clips: usize) -> (Project, Vec<TrackId>) {
    let mut project = Project::new("Bench");
    let asset = project.add_asset("/media/bench.mp4", ve_core::MediaInfo::default());
    let settings = project.settings.default_sequence.clone();
    let sequence = project.add_sequence("Bench", settings);

    let mut tracks = Vec::new();
    for _ in 0..4 {
        let id = project.new_track_id();
        project.sequence_mut(sequence).unwrap().add_track(id, TrackKind::Video);
        tracks.push(id);
    }

    // Two seconds per clip, butt-joined, spread round-robin across tracks.
    for i in 0..clips {
        let track = tracks[i % tracks.len()];
        let start = Ticks::from_seconds((i / tracks.len()) as i64 * 2);
        let id = project.new_clip_id();
        let clip = Clip::new(
            id,
            asset,
            format!("clip{i}"),
            Ticks::ZERO,
            start,
            Ticks::from_seconds(2),
        );
        project
            .sequence_mut(sequence)
            .unwrap()
            .track_mut(track)
            .unwrap()
            .insert_clip(clip)
            .expect("bench clips must not overlap");
    }
    (project, tracks)
}

fn build(c: &mut Criterion) {
    let mut group = c.benchmark_group("build_timeline");
    for &count in &[100usize, 1_000, 10_000] {
        group.bench_with_input(BenchmarkId::from_parameter(count), &count, |b, &count| {
            b.iter(|| black_box(project_with_clips(count)));
        });
    }
    group.finish();
}

/// The property that matters: a visible-range query must cost what is on
/// screen, not what is in the project.
fn range_query(c: &mut Criterion) {
    let mut group = c.benchmark_group("clips_in_visible_range");
    for &count in &[100usize, 1_000, 10_000] {
        let (project, tracks) = project_with_clips(count);
        let sequence = project.sequences[0].id;
        // A window covering roughly twenty clips, whatever the project size.
        let window = TimeRange::from_bounds(Ticks::from_seconds(20), Ticks::from_seconds(30));
        group.bench_with_input(BenchmarkId::from_parameter(count), &count, |b, _| {
            b.iter(|| {
                let seq = project.sequence(sequence).unwrap();
                let mut n = 0usize;
                for track in &tracks {
                    n += seq.track(*track).unwrap().clips_in_range(window).count();
                }
                black_box(n)
            });
        });
    }
    group.finish();
}

fn clip_at(c: &mut Criterion) {
    let mut group = c.benchmark_group("clip_at_playhead");
    for &count in &[100usize, 10_000] {
        let (project, tracks) = project_with_clips(count);
        let sequence = project.sequences[0].id;
        let at = Ticks::from_seconds((count / 8) as i64);
        group.bench_with_input(BenchmarkId::from_parameter(count), &count, |b, _| {
            b.iter(|| {
                let seq = project.sequence(sequence).unwrap();
                black_box(seq.track(tracks[0]).unwrap().clip_at(at).is_some())
            });
        });
    }
    group.finish();
}

fn snapping(c: &mut Criterion) {
    let mut group = c.benchmark_group("snap_candidate");
    for &count in &[100usize, 10_000] {
        let (project, _) = project_with_clips(count);
        let sequence = project.sequences[0].id;
        let at = Ticks::from_seconds((count / 8) as i64) + Ticks::from_millis(120);
        let tolerance = Ticks::from_millis(200);
        group.bench_with_input(BenchmarkId::from_parameter(count), &count, |b, _| {
            b.iter(|| {
                let seq = project.sequence(sequence).unwrap();
                black_box(seq.snap_candidate(at, tolerance, None))
            });
        });
    }
    group.finish();
}

fn edits(c: &mut Criterion) {
    let (project, tracks) = project_with_clips(1_000);
    let sequence = project.sequences[0].id;

    c.bench_function("move_clip_1000", |b| {
        let mut project = project.clone();
        let clip = project.sequence(sequence).unwrap().track(tracks[0]).unwrap().clips()[0].id;
        let mut flip = false;
        b.iter(|| {
            flip = !flip;
            // Within the clip's own gap, so the edit always succeeds.
            let to = if flip { Ticks::from_millis(100) } else { Ticks::ZERO };
            let _ = project
                .sequence_mut(sequence)
                .unwrap()
                .track_mut(tracks[0])
                .unwrap()
                .move_clip(clip, to);
        });
    });

    c.bench_function("split_and_restore_1000", |b| {
        b.iter_batched(
            || project.clone(),
            |mut project| {
                let clip =
                    project.sequence(sequence).unwrap().track(tracks[0]).unwrap().clips()[0].id;
                let new_id = project.new_clip_id();
                let mut effect_id = || ve_core::EffectId::from_raw(1);
                let _ = project
                    .sequence_mut(sequence)
                    .unwrap()
                    .track_mut(tracks[0])
                    .unwrap()
                    .split_clip(clip, Ticks::from_seconds(1), new_id, &mut effect_id);
                black_box(project)
            },
            criterion::BatchSize::SmallInput,
        );
    });
}

fn time_conversion(c: &mut Criterion) {
    let rate = Rate::FPS_29_97;
    c.bench_function("frame_to_ticks_ntsc", |b| {
        let mut frame = 0i64;
        b.iter(|| {
            frame = frame.wrapping_add(1);
            black_box(rate.frame_to_ticks(black_box(frame)))
        });
    });
    c.bench_function("ticks_to_frame_ntsc", |b| {
        let t = rate.frame_to_ticks(123_456);
        b.iter(|| black_box(rate.ticks_to_frame(black_box(t))));
    });
    c.bench_function("timecode_from_frame_dropframe", |b| {
        let mut frame = 0i64;
        b.iter(|| {
            frame = (frame + 1) % 200_000;
            black_box(ve_time::Timecode::from_frame(black_box(frame), rate))
        });
    });
}

/// Evaluating animation, which happens once per animated property per frame —
/// and, with motion blur on, once per property per *sample*.
///
/// The interesting property is scaling: a property with a thousand keyframes
/// should cost barely more than one with two, because evaluation binary-searches
/// rather than walking. The un-animated case is the one that matters most, since
/// nearly every property in a project is a constant and pays only a length
/// check.
fn animation(c: &mut Criterion) {
    use ve_core::{Interpolation, Property, Transform, Vec2};

    let mut group = c.benchmark_group("evaluate_property");

    let constant = Property::constant(0.5f64);
    group.bench_function("constant", |b| {
        b.iter(|| black_box(constant.evaluate(black_box(Ticks::from_seconds(3)))));
    });

    for &count in &[2usize, 64, 1000] {
        let mut animated = Property::constant(0.0f64);
        for i in 0..count {
            animated.set_keyframe(
                Ticks::from_millis(i as i64 * 100),
                i as f64,
                Interpolation::Linear,
            );
        }
        group.bench_with_input(BenchmarkId::new("linear", count), &count, |b, &count| {
            let at = Ticks::from_millis(count as i64 * 50);
            b.iter(|| black_box(animated.evaluate(black_box(at))));
        });
    }

    // An eased segment pays for the bezier solver, which is the difference
    // between "the common case" and "the worst one".
    let mut eased = Property::constant(0.0f64);
    eased.set_keyframe(
        Ticks::ZERO,
        0.0,
        Interpolation::Bezier { x1: 0.9, y1: 0.0, x2: 0.1, y2: 1.0 },
    );
    eased.set_keyframe(Ticks::from_seconds(4), 1.0, Interpolation::Linear);
    group.bench_function("bezier", |b| {
        b.iter(|| black_box(eased.evaluate(black_box(Ticks::from_seconds(1)))));
    });
    group.finish();

    // A whole transform, which is what a plan resolves per item — and twelve of
    // them is what one motion-blurred item costs the engine before the GPU sees
    // anything.
    let mut transform = Transform::default();
    transform.position.set_keyframe(Ticks::ZERO, Vec2::ZERO, Interpolation::Linear);
    transform.position.set_keyframe(
        Ticks::from_seconds(4),
        Vec2::new(400.0, 0.0),
        Interpolation::Linear,
    );
    transform.rotation.set_keyframe(Ticks::ZERO, 0.0, Interpolation::EaseInOut);
    transform.rotation.set_keyframe(Ticks::from_seconds(4), 90.0, Interpolation::Linear);

    let mut group = c.benchmark_group("evaluate_transform");
    group.bench_function("once", |b| {
        b.iter(|| black_box(transform.evaluate(black_box(Ticks::from_seconds(2)))));
    });
    group.bench_function("twelve_samples", |b| {
        let blur = ve_core::MotionBlur::new(180.0, 12);
        let frame = Rate::FPS_30.frame_duration();
        let at = Ticks::from_seconds(2);
        b.iter(|| {
            let offsets = blur.offsets(black_box(frame));
            let states: Vec<_> = offsets.iter().map(|o| transform.evaluate(at + *o)).collect();
            black_box(states)
        });
    });
    group.finish();
}

criterion_group!(
    benches,
    build,
    range_query,
    clip_at,
    snapping,
    edits,
    time_conversion,
    animation
);
criterion_main!(benches);
