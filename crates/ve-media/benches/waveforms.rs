//! Benchmarks for waveform reduction and display.
//!
//! Two claims are being kept honest here.
//!
//! **Analysis is a one-off cost proportional to the audio**, so what matters is
//! how much faster than real time it runs: a figure below 1× would mean a
//! waveform that never catches up with a file being played.
//!
//! **Drawing is bounded by the number of pixel columns, not by the length of
//! the file behind them.** A clip on screen is a few hundred columns whether it
//! is drawn from a ten-second file or a ten-hour one, and the zoomed-in figures
//! below should therefore be flat across waveform lengths. Only zooming all the
//! way out — asking a thousand columns to summarise every bucket there is —
//! should cost more, and then in proportion to what it actually reads.

use std::hint::black_box;
use std::path::PathBuf;

use criterion::{criterion_group, criterion_main, BenchmarkId, Criterion};
use ve_media::{AudioBuffer, Peak, Waveform, WaveformBuilder, PEAKS_PER_SECOND};
use ve_time::{SampleRate, Ticks, TimeRange};

fn testdata(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../testdata").join(name)
}

/// One second of stereo 48 kHz audio, as the decoder would hand it over.
fn one_second(channels: u16) -> AudioBuffer {
    let frames = 48_000usize;
    let samples: Vec<f32> =
        (0..frames * channels as usize).map(|i| ((i as f32) * 0.001).sin() * 0.8).collect();
    AudioBuffer::new(samples.into(), SampleRate::HZ_48000, channels, Ticks::ZERO)
}

/// A waveform covering `seconds` of audio, without decoding anything.
fn waveform_of(seconds: i64) -> Waveform {
    let peaks: Vec<Peak> = (0..seconds * PEAKS_PER_SECOND)
        .map(|i| {
            let v = ((i as f32) * 0.01).sin().abs();
            Peak { min: -v, max: v, rms: v * 0.7 }
        })
        .collect();
    Waveform::new(peaks, SampleRate::HZ_48000, 2)
}

/// Turning samples into buckets, which is what analysis spends its time on
/// once the decoder has handed the samples over.
fn bench_reduce(c: &mut Criterion) {
    let mut group = c.benchmark_group("waveform_reduce");
    for channels in [1u16, 2, 6] {
        let buffer = one_second(channels);
        group.bench_with_input(
            BenchmarkId::from_parameter(format!("{channels}ch_1s")),
            &buffer,
            |b, buffer| {
                b.iter(|| {
                    let mut builder = WaveformBuilder::new(SampleRate::HZ_48000, channels);
                    builder.push(black_box(buffer));
                    black_box(builder.bucket_count())
                });
            },
        );
    }
    group.finish();
}

/// The per-repaint cost: reducing part of a waveform to pixel columns.
fn bench_envelope(c: &mut Criterion) {
    let mut group = c.benchmark_group("waveform_envelope");

    // A ten-second clip 600 pixels wide, drawn from files of wildly different
    // lengths. These must not diverge: the cost is the columns, not the file.
    for minutes in [1i64, 60] {
        let waveform = waveform_of(minutes * 60);
        let range = TimeRange::new(Ticks::from_seconds(5), Ticks::from_seconds(10));
        group.bench_with_input(
            BenchmarkId::from_parameter(format!("clip_600px_of_{minutes}min")),
            &waveform,
            |b, waveform| {
                b.iter(|| black_box(waveform.envelope(black_box(range), 600)));
            },
        );
    }

    // Zoomed in past the bucket grid, where every column interpolates.
    let waveform = waveform_of(3600);
    let tight = TimeRange::new(Ticks::from_seconds(5), Ticks::from_millis(40));
    group.bench_function("zoomed_past_the_grid_600px", |b| {
        b.iter(|| black_box(waveform.envelope(black_box(tight), 600)));
    });

    // And zoomed all the way out, which is the only case that reads the whole
    // thing: 720,000 buckets into 1,000 columns.
    let whole = TimeRange::new(Ticks::ZERO, Ticks::from_seconds(3600));
    group.bench_function("whole_hour_1000px", |b| {
        b.iter(|| black_box(waveform.envelope(black_box(whole), 1000)));
    });

    group.finish();
}

/// Decoding and reducing a real file, end to end.
fn bench_analyse(c: &mut Criterion) {
    let path = testdata("tone_48k.wav");
    if !path.exists() {
        return;
    }
    c.bench_function("waveform_analyse_1s_wav", |b| {
        b.iter(|| black_box(ve_media::analyse_file(&path).unwrap().len()));
    });
}

criterion_group!(benches, bench_reduce, bench_envelope, bench_analyse);
criterion_main!(benches);
