//! Export benchmarks: what one written frame costs, and where the time goes.
//!
//! An export's cost is three things in a row — composite, read back, encode —
//! and only the first is shared with the preview, which is already measured
//! elsewhere. So these separate the other two, because they are the ones that
//! decide whether an export runs faster than real time.
//!
//! On a software rasteriser the readback numbers say nothing about real
//! hardware. They are here so that a regression in how many copies a frame
//! makes shows up as a change rather than being invisible.

use std::hint::black_box;
use std::path::PathBuf;

use criterion::{criterion_group, criterion_main, BenchmarkId, Criterion};
use ve_core::{Clip, MediaInfo, Project, Size, VideoStreamInfo};
use ve_export::{Cancel, ExportRange, ExportSettings, MediaWriter, Quality, VideoCodec};
use ve_metrics::Metrics;
use ve_render::{GpuContext, RenderTarget};
use ve_time::{Rate, Ticks, TimeRange};

fn testdata(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../testdata").join(name)
}

/// A picture with something in it: a flat colour compresses to nothing and
/// would flatter the encoder.
fn noisy_rgba(size: Size) -> Vec<u8> {
    let mut data = vec![0u8; (size.width * size.height * 4) as usize];
    let mut state = 0x1234_5678u32;
    for pixel in data.as_chunks_mut::<4>().0 {
        // xorshift: deterministic, and far cheaper than the encode it feeds.
        state ^= state << 13;
        state ^= state >> 17;
        state ^= state << 5;
        pixel[0] = state as u8;
        pixel[1] = (state >> 8) as u8;
        pixel[2] = (state >> 16) as u8;
        pixel[3] = 255;
    }
    data
}

/// Converting and encoding one frame, with no rendering in front of it.
fn encode_frame(c: &mut Criterion) {
    let dir = tempfile::tempdir().expect("a temporary directory");
    let mut group = c.benchmark_group("encode_frame");
    for &(w, h, label) in &[(640u32, 360u32, "360p"), (1920, 1080, "1080p")] {
        let size = Size::new(w, h);
        let pixels = noisy_rgba(size);
        group.bench_with_input(BenchmarkId::from_parameter(label), &label, |b, _| {
            let settings = ExportSettings {
                path: dir.path().join(format!("{label}.mp4")),
                range: ExportRange::Whole,
                resolution: size,
                rate: Rate::FPS_30,
                video: VideoCodec::H264,
                quality: Quality::Standard,
                audio: None,
                keyframes: ve_export::Keyframes::EverySecond,
            };
            let mut writer = MediaWriter::create(&settings, size).expect("an encoder to open");
            b.iter(|| writer.write_frame(black_box(&pixels)).expect("encoding"));
            // Finishing inside the timed loop would measure the muxer's
            // trailer once per sample; the file is thrown away anyway.
            drop(writer);
        });
    }
    group.finish();
}

/// Reading one composited picture back off the GPU.
fn read_back(c: &mut Criterion) {
    let Ok(gpu) = GpuContext::headless() else {
        eprintln!("skipping GPU benchmarks: no adapter");
        return;
    };
    let mut group = c.benchmark_group("read_back");
    for &(w, h, label) in &[(640u32, 360u32, "360p"), (1920, 1080, "1080p")] {
        let target = RenderTarget::new(&gpu.device, Size::new(w, h));
        group.bench_with_input(BenchmarkId::from_parameter(label), &label, |b, _| {
            b.iter(|| black_box(target.read_pixels(&gpu.device, &gpu.queue)));
        });
    }
    group.finish();
}

/// A whole second of export, end to end, from a real file on a real timeline.
///
/// The one number that answers "how long will this take": everything else is a
/// part of it.
fn export_a_second(c: &mut Criterion) {
    let Ok(gpu) = GpuContext::headless() else { return };
    let dir = tempfile::tempdir().expect("a temporary directory");
    let metrics = Metrics::new();

    let mut group = c.benchmark_group("export_second");
    // Sampled rather than measured many times over: a second of export is
    // hundreds of milliseconds, and criterion's default sample count would
    // take a quarter of an hour.
    group.sample_size(10);

    for &(w, h, label) in &[(640u32, 360u32, "360p"), (1920, 1080, "1080p")] {
        let (project, sequence) = one_clip_project(Size::new(w, h));
        let settings = ExportSettings::for_sequence(
            project.sequence(sequence).unwrap(),
            dir.path().join(format!("second-{label}.mp4")),
        )
        .with_range(ExportRange::Span(TimeRange::new(Ticks::ZERO, Ticks::from_seconds(1))));

        group.bench_with_input(BenchmarkId::from_parameter(label), &label, |b, _| {
            b.iter(|| {
                ve_export::run(
                    &project,
                    sequence,
                    &settings,
                    &gpu,
                    &metrics,
                    &Cancel::new(),
                    |_| {},
                )
                .expect("the export to succeed")
            });
        });
    }
    group.finish();
}

/// A project of one clip covering three seconds, on a canvas of `size`.
fn one_clip_project(size: Size) -> (Project, ve_core::SequenceId) {
    let mut project = Project::with_default_sequence("Bench");
    let info = MediaInfo {
        duration: Ticks::from_seconds(3),
        video: Some(VideoStreamInfo {
            size: Size::new(160, 120),
            rate: Rate::FPS_30,
            duration: Ticks::from_seconds(3),
            frame_count: Some(90),
            codec: "h264".into(),
            pixel_format: "yuv444p".into(),
            sample_aspect_ratio: (1, 1),
        }),
        audio: None,
        container: "mp4".into(),
        timecode: None,
    };
    let asset = project.add_asset(testdata("counter_30fps.mp4"), info);
    let sequence = project.active_sequence.unwrap();
    project.sequence_mut(sequence).unwrap().settings.resolution = size;
    let clip_id = project.new_clip_id();
    let track = project.sequence(sequence).unwrap().tracks[0].id;
    let clip =
        Clip::new(clip_id, asset, "clip", Ticks::ZERO, Ticks::ZERO, Ticks::from_seconds(3));
    project
        .sequence_mut(sequence)
        .unwrap()
        .track_mut(track)
        .unwrap()
        .insert_clip(clip)
        .unwrap();
    (project, sequence)
}

criterion_group!(benches, encode_frame, read_back, export_a_second);
criterion_main!(benches);
