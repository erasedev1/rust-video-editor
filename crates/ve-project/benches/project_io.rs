//! Save and load benchmarks.
//!
//! Project load time is one of the numbers a professional editor is judged on:
//! it is the delay between deciding to work and being able to.

use criterion::{criterion_group, criterion_main, BenchmarkId, Criterion};
use std::hint::black_box;
use ve_core::{Clip, Project, TrackKind};
use ve_project::store;
use ve_time::Ticks;

fn project_with_clips(clips: usize) -> Project {
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
            .unwrap();
    }
    project
}

fn save(c: &mut Criterion) {
    let dir = tempfile::tempdir().unwrap();
    let mut group = c.benchmark_group("save_project");
    for &count in &[100usize, 1_000, 10_000] {
        let project = project_with_clips(count);
        let path = dir.path().join(format!("bench{count}.verge"));
        group.bench_with_input(BenchmarkId::from_parameter(count), &count, |b, _| {
            b.iter(|| store::save(black_box(&project), black_box(&path)).unwrap());
        });
    }
    group.finish();
}

fn load(c: &mut Criterion) {
    let dir = tempfile::tempdir().unwrap();
    let mut group = c.benchmark_group("load_project");
    for &count in &[100usize, 1_000, 10_000] {
        let path = dir.path().join(format!("bench{count}.verge"));
        store::save(&project_with_clips(count), &path).unwrap();
        group.bench_with_input(BenchmarkId::from_parameter(count), &count, |b, _| {
            b.iter(|| black_box(store::load(black_box(&path)).unwrap()));
        });
    }
    group.finish();
}

fn serialise_only(c: &mut Criterion) {
    let mut group = c.benchmark_group("serialise_to_json");
    {
        let &count = &1_000usize;
        let project = project_with_clips(count);
        group.bench_with_input(BenchmarkId::from_parameter(count), &count, |b, _| {
            b.iter(|| black_box(store::to_json(black_box(&project)).unwrap()));
        });
    }
    group.finish();
}

criterion_group!(benches, save, load, serialise_only);
criterion_main!(benches);
