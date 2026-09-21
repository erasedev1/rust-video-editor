//! Benchmarks for the audio path.
//!
//! Three claims are being kept honest here.
//!
//! **Mixing has to run far faster than real time.** A device asks for a block
//! every few milliseconds and will not wait; the figure that matters is not the
//! time per block but the multiple of real time the mixer achieves, because
//! that is the headroom a session has before it starts to stutter.
//!
//! **Metering has to be nearly free.** A meter that costs a measurable fraction
//! of the mix is a meter that would be turned off, and a meter that is turned
//! off is not a meter. The metered and unmetered mixes are therefore measured
//! side by side.
//!
//! **A fade has to be cheap enough to ignore.** Fades are on the per-block path
//! by way of the plan, so the question is what they add there. The plan and the
//! envelope are both measured with and without them, and the honest answer is
//! that a fade roughly doubles the cost of evaluating one clip's envelope — see
//! `docs/BENCHMARKS.md` for why that is still nothing against a block of audio.

use std::hint::black_box;

use criterion::{criterion_group, criterion_main, BenchmarkId, Criterion};
use ve_core::{
    AudioProperties, AudioStreamInfo, Clip, Fade, FadeCurve, MediaInfo, Project, Property,
    Sequence, SequenceId, Track, TrackKind,
};
use ve_engine::{evaluate_project, AudioMixer, Meter, MixSource};
use ve_time::{SampleRate, Ticks};

/// A block of interleaved stereo audio, as a decoder would hand it over.
fn block(frames: usize) -> Vec<f32> {
    (0..frames * 2).map(|i| ((i as f32) * 0.01).sin() * 0.7).collect()
}

/// Mixing, as a multiple of real time.
///
/// Throughput is in sample frames, so criterion reports elements per second —
/// divide by the sample rate for the multiple of real time.
fn bench_mix(c: &mut Criterion) {
    let mixer = AudioMixer::new(SampleRate::HZ_48000, 2);
    // A tenth of a second, which is the order of block a device asks for.
    let frames = 4800usize;
    let samples = block(frames);
    let mut out = vec![0.0f32; frames * 2];

    let mut group = c.benchmark_group("audio_mix");
    group.throughput(criterion::Throughput::Elements(frames as u64));
    for sources in [1usize, 4, 16, 64] {
        let inputs: Vec<MixSource<'_>> = (0..sources)
            .map(|i| MixSource {
                samples: &samples,
                channels: 2,
                gain: 1.0 / sources as f64,
                pan: (i as f64 / sources as f64) * 2.0 - 1.0,
            })
            .collect();

        group.bench_with_input(BenchmarkId::new("plain", sources), &inputs, |b, inputs| {
            b.iter(|| black_box(mixer.mix_into(&mut out, inputs, frames)));
        });

        let mut meters = vec![Meter::SILENT; sources];
        group.bench_with_input(BenchmarkId::new("metered", sources), &inputs, |b, inputs| {
            b.iter(|| black_box(mixer.mix_metered(&mut out, inputs, frames, &mut meters)));
        });
    }
    group.finish();
}

/// A sequence of `tracks` audio tracks, each with a clip under the playhead.
fn audio_sequence(tracks: usize, faded: bool) -> (Project, SequenceId) {
    let mut project = Project::with_default_sequence("Mix");
    let asset = project.add_asset(
        "/media/tone.wav",
        MediaInfo {
            duration: Ticks::from_seconds(600),
            video: None,
            audio: Some(AudioStreamInfo {
                sample_rate: SampleRate::HZ_48000,
                channels: 2,
                duration: Ticks::from_seconds(600),
                codec: "pcm_s16le".into(),
            }),
            container: "wav".into(),
            timecode: None,
        },
    );
    let sequence_id = project.active_sequence.unwrap();
    for index in 0..tracks {
        let track_id = project.new_track_id();
        let clip_id = project.new_clip_id();
        let mut clip = Clip::new(
            clip_id,
            asset,
            "tone",
            Ticks::ZERO,
            Ticks::ZERO,
            Ticks::from_seconds(60),
        );
        if faded {
            clip.audio.fade_in = Fade::new(Ticks::from_seconds(2), FadeCurve::EqualPower);
            clip.audio.fade_out = Fade::new(Ticks::from_seconds(2), FadeCurve::EqualPower);
        }
        let mut track = Track::new(track_id, TrackKind::Audio, format!("A{index}"));
        track.volume = 0.8;
        track.pan = -0.2;
        track.insert_clip(clip).expect("an empty track always takes a clip");
        project.sequence_mut(sequence_id).unwrap().tracks.push(track);
    }
    (project, sequence_id)
}

/// Resolving an instant into what is audible, which every mixed block does
/// once. Fades and track levels are both on this path.
fn bench_plan(c: &mut Criterion) {
    let mut group = c.benchmark_group("audio_plan");
    for tracks in [4usize, 16, 64] {
        for (label, faded) in [("plain", false), ("faded", true)] {
            let (project, sequence_id) = audio_sequence(tracks, faded);
            let sequence: &Sequence = project.sequence(sequence_id).unwrap();
            group.bench_with_input(BenchmarkId::new(label, tracks), &tracks, |b, _| {
                b.iter(|| {
                    black_box(evaluate_project(&project, sequence, Ticks::from_millis(1500)))
                });
            });
        }
    }
    group.finish();
}

/// Evaluating one clip's audio envelope, which happens once per clip per block.
fn bench_envelope(c: &mut Criterion) {
    let plain =
        AudioProperties { volume: Property::constant(0.8), ..AudioProperties::default() };

    let mut faded = plain.clone();
    faded.fade_in = Fade::new(Ticks::from_seconds(2), FadeCurve::EqualPower);
    faded.fade_out = Fade::new(Ticks::from_seconds(2), FadeCurve::Smooth);

    let duration = Ticks::from_seconds(60);
    let at = Ticks::from_millis(1500);

    let mut group = c.benchmark_group("audio_envelope");
    group.bench_function("plain", |b| {
        b.iter(|| black_box(plain.evaluate(black_box(at), duration)));
    });
    group.bench_function("faded", |b| {
        b.iter(|| black_box(faded.evaluate(black_box(at), duration)));
    });
    group.finish();
}

criterion_group!(benches, bench_mix, bench_plan, bench_envelope);
criterion_main!(benches);
