//! Proxy benchmarks: what a proxy costs to build, and what it buys.
//!
//! The committed fixtures are 160×120, which is below the size at which a proxy
//! is worth having at all, so these generate a 1080p source of their own first.
//! It is encoded the way camera footage is — long-GOP, a keyframe a second —
//! because that is what makes the comparison mean anything: against an
//! all-intra source a proxy would buy only the smaller picture, and the larger
//! half of what it buys would be invisible.
//!
//! Two claims are being kept honest.
//!
//! **Scrubbing gets cheaper**, and by more than the pixel count alone explains.
//! A quarter-size picture is a sixteenth of the pixels, but a jump into the
//! middle of a long group of pictures also costs every frame back to the last
//! keyframe, and an all-intra proxy costs one. `scrub` measures exactly that:
//! seek somewhere new, decode the frame there, as a hand dragging a playhead
//! does over and over.
//!
//! **Playing forward gets cheaper too**, but for a different reason: sequential
//! decoding never pays the group-of-pictures cost in the first place, so what
//! it saves is the picture — fewer pixels to reconstruct, a smaller bitstream
//! to read, and a smaller conversion to RGBA on the way out. `play` measures
//! that, and the **gap between the two ratios** is the part of a proxy that has
//! nothing to do with resolution at all.

use std::hint::black_box;
use std::path::{Path, PathBuf};

use criterion::{criterion_group, criterion_main, BenchmarkId, Criterion};
use ve_core::Size;
use ve_export::proxy::{self, ProxyScale, ProxySettings};
use ve_export::{
    Cancel, ExportRange, ExportSettings, Keyframes, MediaWriter, Quality, VideoCodec,
};
use ve_media::VideoDecoder;
use ve_metrics::Metrics;
use ve_time::{Rate, Ticks};

/// How long the generated source runs. Long enough to hold several groups of
/// pictures, short enough that generating it is not the benchmark.
const FRAMES: i64 = 90;
const RATE: Rate = Rate::FPS_30;
const SOURCE: Size = Size::new(1920, 1080);

/// A picture with something in it: a flat colour compresses to nothing and
/// would flatter both the encoder and the decoder under test.
fn noisy_rgba(size: Size, seed: u32) -> Vec<u8> {
    let mut data = vec![0u8; (size.width * size.height * 4) as usize];
    let mut state = seed | 1;
    for pixel in data.as_chunks_mut::<4>().0 {
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

/// Writes a 1080p long-GOP file that stands in for camera footage.
///
/// Every frame is different noise, so no frame is a cheap copy of the one
/// before it and the decoder has real work to do on each.
fn make_source(path: &Path) {
    let settings = ExportSettings {
        path: path.to_path_buf(),
        range: ExportRange::Whole,
        resolution: SOURCE,
        rate: RATE,
        video: VideoCodec::H264,
        quality: Quality::Standard,
        audio: None,
        // What a camera writes, and what a proxy is compared against.
        keyframes: Keyframes::EverySecond,
    };
    let mut writer = MediaWriter::create(&settings, SOURCE).expect("creating the source");
    for i in 0..FRAMES {
        writer.write_frame(&noisy_rgba(SOURCE, i as u32 * 2_654_435_761)).expect("encoding");
    }
    writer.finish().expect("finishing the source");
}

/// The source and its quarter-size proxy, built once for the whole run.
struct Material {
    original: PathBuf,
    proxy: PathBuf,
    proxy_size: Size,
    _dir: tempfile::TempDir,
}

fn material() -> &'static Material {
    use std::sync::OnceLock;
    static MATERIAL: OnceLock<Material> = OnceLock::new();
    MATERIAL.get_or_init(|| {
        let dir = tempfile::tempdir().expect("a temporary directory");
        let original = dir.path().join("source.mp4");
        make_source(&original);

        let proxy_size = ProxyScale::Quarter.apply(SOURCE);
        let settings = ProxySettings {
            source: original.clone(),
            path: dir.path().join("source.proxy.mov"),
            size: proxy_size,
            rate: RATE,
            duration: RATE.frame_duration() * FRAMES,
            codec: VideoCodec::H264,
            quality: Quality::High,
        };
        proxy::build(&settings, &Metrics::new(), &Cancel::new(), |_| {})
            .expect("building the proxy");

        Material { original, proxy: settings.path.clone(), proxy_size, _dir: dir }
    })
}

/// Frames chosen to land at different distances into a group of pictures, and
/// to jump backwards as well as forwards — which is what a scrub does and what
/// forces a real seek rather than a walk forward.
const SCRUB_TARGETS: [i64; 8] = [37, 4, 71, 22, 88, 15, 59, 2];

/// One frame from somewhere new: seek, decode, show. The cost of dragging a
/// playhead, which is where an editor either feels attached to the mouse or
/// does not.
fn scrub(c: &mut Criterion) {
    let m = material();
    let mut group = c.benchmark_group("scrub");
    for (label, path) in [("original_1080p", &m.original), ("proxy_270p", &m.proxy)] {
        group.bench_with_input(BenchmarkId::from_parameter(label), path, |b, path| {
            // A decoder per iteration would measure opening files. One decoder
            // across the jumps is what the editor actually keeps.
            let mut decoder = VideoDecoder::open(path).expect("opening");
            b.iter(|| {
                for target in SCRUB_TARGETS {
                    let t = Ticks::from_rational(target, 30);
                    black_box(decoder.frame_at(t).expect("decoding"));
                }
            });
        });
    }
    group.finish();
}

/// Decoding forward, as playback does. No seeking, so no group-of-pictures
/// cost — the difference here should be the pixel count and nothing else.
fn play(c: &mut Criterion) {
    let m = material();
    let mut group = c.benchmark_group("play");
    for (label, path) in [("original_1080p", &m.original), ("proxy_270p", &m.proxy)] {
        group.bench_with_input(BenchmarkId::from_parameter(label), path, |b, path| {
            b.iter(|| {
                let mut decoder = VideoDecoder::open(path).expect("opening");
                let mut seen = 0u32;
                for _ in 0..30 {
                    match decoder.next_frame().expect("decoding") {
                        Some(frame) => {
                            seen += 1;
                            black_box(frame.width());
                        }
                        None => break,
                    }
                }
                black_box(seen)
            });
        });
    }
    group.finish();
}

/// What building one costs, which is the price of the two above.
///
/// A one-off per file, paid while the editor carries on, but it is what decides
/// whether proxying a card of footage is a coffee break or an afternoon.
fn build(c: &mut Criterion) {
    let m = material();
    let dir = tempfile::tempdir().expect("a temporary directory");
    let mut group = c.benchmark_group("build_proxy");
    // Criterion's default of 100 samples would transcode 90 frames of 1080p a
    // hundred times over; this is a throughput figure, not a microbenchmark.
    group.sample_size(10);
    for scale in [ProxyScale::Half, ProxyScale::Quarter] {
        let size = scale.apply(SOURCE);
        let settings = ProxySettings {
            source: m.original.clone(),
            path: dir.path().join(format!("{}.mov", scale.label())),
            size,
            rate: RATE,
            duration: RATE.frame_duration() * FRAMES,
            codec: VideoCodec::H264,
            quality: Quality::High,
        };
        group.bench_with_input(
            BenchmarkId::from_parameter(format!("{}_{}p", scale.label(), size.height)),
            &settings,
            |b, settings| {
                b.iter(|| {
                    let report =
                        proxy::build(settings, &Metrics::new(), &Cancel::new(), |_| {})
                            .expect("building");
                    black_box(report.frames)
                });
            },
        );
    }
    group.finish();
    // Quoted in the write-up, and a cheap guard against the material silently
    // becoming something other than a quarter on each axis — a sixteenth of
    // the pixels, which is what the ratios below are read against.
    assert_eq!(m.proxy_size, Size::new(480, 270));
}

criterion_group!(benches, scrub, play, build);
criterion_main!(benches);
