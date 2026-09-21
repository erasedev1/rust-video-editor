//! Benchmarks for captions.
//!
//! Two things happen often enough to be worth a number.
//!
//! **Reading a file** is what an import costs, and a feature-length
//! transcription is a few thousand cues. Nobody waits for a parse on purpose,
//! but "a few thousand cues" is also what a caption house hands over, so the
//! figure is worth knowing rather than assuming.
//!
//! **Finding the cue at an instant** happens on every drawn frame, for the
//! preview overlay and for the caption lane. That is the one on the hot path,
//! and the reason cues are held sorted rather than in the order they were
//! typed: it is a binary search over the whole film, not a scan.

use std::hint::black_box;

use criterion::{criterion_group, criterion_main, BenchmarkId, Criterion};
use ve_caption::CaptionFormat;
use ve_core::{CaptionTrack, CaptionTrackId, Cue, CueId};
use ve_time::Ticks;

/// A caption track the length of a film: a cue every three seconds, two lines
/// each, which is roughly what continuous dialogue comes to.
fn track(cues: usize) -> CaptionTrack {
    let mut track = CaptionTrack::new(CaptionTrackId::from_raw(1), "C1");
    for i in 0..cues {
        let start = Ticks::from_millis(i as i64 * 3_000);
        track
            .insert_cue(Cue::new(
                CueId::from_raw(i as u64 + 2),
                start,
                Ticks::from_millis(2_400),
                "- Is that what you meant?\n- It is exactly what I meant.",
            ))
            .expect("fixture cues do not overlap");
    }
    track
}

fn parsing(c: &mut Criterion) {
    let mut group = c.benchmark_group("caption_parse");
    for cues in [100usize, 1_000, 4_000] {
        let track = track(cues);
        for format in CaptionFormat::ALL {
            let text = ve_caption::to_string(track.cues(), format);
            group.bench_with_input(
                BenchmarkId::new(format.extension(), cues),
                &text,
                |b, text| b.iter(|| black_box(ve_caption::parse(black_box(text)).cues.len())),
            );
        }
    }
    group.finish();
}

fn writing(c: &mut Criterion) {
    let mut group = c.benchmark_group("caption_write");
    for cues in [100usize, 1_000, 4_000] {
        let track = track(cues);
        group.bench_with_input(BenchmarkId::new("srt", cues), &track, |b, track| {
            b.iter(|| {
                black_box(ve_caption::to_string(track.cues(), CaptionFormat::SubRip).len())
            })
        });
    }
    group.finish();
}

/// What the preview overlay pays per frame. The whole point of the invariant.
fn lookup(c: &mut Criterion) {
    let mut group = c.benchmark_group("caption_lookup");
    for cues in [100usize, 1_000, 4_000] {
        let track = track(cues);
        // Two thirds of the way in, so the search does its full depth rather
        // than finding the answer next to where it started.
        let at = Ticks::from_millis(cues as i64 * 2_000);
        group.bench_with_input(BenchmarkId::new("cue_at", cues), &track, |b, track| {
            b.iter(|| black_box(track.cue_at(black_box(at)).is_some()))
        });
    }
    group.finish();
}

criterion_group!(benches, parsing, writing, lookup);
criterion_main!(benches);
