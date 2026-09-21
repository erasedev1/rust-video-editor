//! Benchmarks for multicam syncing.
//!
//! Two claims are being kept honest here.
//!
//! **Syncing is something a person waits for**, so what matters is the wall
//! clock: a producer drops four cards on the timeline and presses Sync. The
//! figures below are per *pair*, and a group of N cameras is N-1 pairs.
//!
//! **The coarse-to-fine search is what makes that possible.** A full search of
//! ±10 minutes at 200 buckets a second is a quarter of a million lags against a
//! series of the same order. The same search is measured both ways here, at a
//! length where the exhaustive version can still be run at all, and the ratio
//! between them is the whole argument for the pyramid.

use std::hint::black_box;

use criterion::{criterion_group, criterion_main, BenchmarkId, Criterion};
use ve_media::{align, Peak, Waveform, PEAKS_PER_SECOND};
use ve_time::{SampleRate, Ticks};

/// An envelope with the shape a real recording has: a quiet floor with
/// transients scattered irregularly over it.
fn envelope(seconds: i64, seed: u32) -> Vec<f32> {
    let buckets = (seconds * PEAKS_PER_SECOND) as usize;
    let mut state = seed.wrapping_mul(2654435761).wrapping_add(12345);
    let mut next = move || {
        state = state.wrapping_mul(1664525).wrapping_add(1013904223);
        (state >> 8) as f32 / (1 << 24) as f32
    };

    let mut series = vec![0.0f32; buckets];
    for (i, value) in series.iter_mut().enumerate() {
        *value = 0.05 + 0.03 * next() + 0.02 * (i as f32 * 0.0013).sin();
    }
    let mut at = 0usize;
    while at < buckets {
        at += 1 + (next() * 120.0) as usize;
        if at >= buckets {
            break;
        }
        let level = 0.3 + 0.6 * next();
        let decay = 3 + (next() * 20.0) as usize;
        for k in 0..decay {
            let Some(slot) = series.get_mut(at + k) else { break };
            let fade = 1.0 - k as f32 / decay as f32;
            *slot = (*slot + level * fade * fade).min(1.0);
        }
    }
    series
}

fn waveform(series: &[f32]) -> Waveform {
    let peaks: Vec<Peak> = series.iter().map(|v| Peak { min: -*v, max: *v, rms: *v }).collect();
    Waveform::new(peaks, SampleRate::HZ_48000, 2)
}

/// A pair of cameras at one event, the second `late` seconds behind.
fn pair(seconds: i64, late: i64, seed: u32) -> (Waveform, Waveform) {
    let full = envelope(seconds, seed);
    let skip = (late * PEAKS_PER_SECOND) as usize;
    (waveform(&full), waveform(&full[skip..]))
}

/// One pair, at the lengths a real shoot produces.
fn bench_align(c: &mut Criterion) {
    let mut group = c.benchmark_group("sync_align");
    // Criterion's default sample size takes minutes at the longer lengths, and
    // the measurement is stable well before that.
    group.sample_size(20);
    for minutes in [1i64, 10, 60] {
        let (a, b) = pair(minutes * 60, 5, minutes as u32);
        group.bench_with_input(
            BenchmarkId::from_parameter(format!("{minutes}min")),
            &(a, b),
            |bench, (a, b)| {
                bench.iter(|| black_box(align(a, b, black_box(ve_media::DEFAULT_MAX_OFFSET))));
            },
        );
    }
    group.finish();
}

/// The coarse-to-fine search against the exhaustive one it replaces.
///
/// The exhaustive version is written out here rather than kept in the crate: it
/// exists to be measured against, not to be called.
fn bench_search_strategy(c: &mut Criterion) {
    let mut group = c.benchmark_group("sync_strategy");
    group.sample_size(10);

    // Two minutes, searched ±60 seconds. Long enough to be realistic, short
    // enough that the exhaustive search finishes inside a benchmark run.
    let (a, b) = pair(120, 5, 7);
    let limit = Ticks::from_seconds(60);

    group.bench_function("coarse_to_fine", |bench| {
        bench.iter(|| black_box(align(&a, &b, black_box(limit))));
    });

    let series_a: Vec<f32> = a.peaks().iter().map(|p| p.rms).collect();
    let series_b: Vec<f32> = b.peaks().iter().map(|p| p.rms).collect();
    let max_lag = (limit.raw() / ve_media::BUCKET_TICKS) as isize;
    group.bench_function("exhaustive", |bench| {
        bench.iter(|| black_box(exhaustive(&series_a, &series_b, max_lag)));
    });

    group.finish();
}

/// Every lag in range, scored. What the pyramid exists to avoid.
fn exhaustive(a: &[f32], b: &[f32], max_lag: isize) -> Option<(isize, f64)> {
    let min_overlap = PEAKS_PER_SECOND as usize;
    let mut best: Option<(isize, f64)> = None;
    for lag in -max_lag..=max_lag {
        let start = lag.max(0);
        let end = (a.len() as isize).min(b.len() as isize + lag);
        if end <= start || ((end - start) as usize) < min_overlap {
            continue;
        }
        let count = (end - start) as usize;
        let (mut sum_a, mut sum_b) = (0.0f64, 0.0f64);
        for i in start..end {
            sum_a += a[i as usize] as f64;
            sum_b += b[(i - lag) as usize] as f64;
        }
        let (mean_a, mean_b) = (sum_a / count as f64, sum_b / count as f64);
        let (mut num, mut var_a, mut var_b) = (0.0f64, 0.0f64, 0.0f64);
        for i in start..end {
            let da = a[i as usize] as f64 - mean_a;
            let db = b[(i - lag) as usize] as f64 - mean_b;
            num += da * db;
            var_a += da * da;
            var_b += db * db;
        }
        let den = (var_a * var_b).sqrt();
        if den <= 0.0 {
            continue;
        }
        let score = num / den;
        if best.is_none_or(|(_, b)| score > b) {
            best = Some((lag, score));
        }
    }
    best
}

criterion_group!(benches, bench_align, bench_search_strategy);
criterion_main!(benches);
