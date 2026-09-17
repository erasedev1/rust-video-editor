//! Track levels, fades and meters, from the edit model through to mixed samples.
//!
//! The shape of a fade curve and the arithmetic of a pan are unit-tested where
//! they live. What is asserted here is the wiring: that a track fader actually
//! reaches the mix, that a fade is audible in the samples rather than only in
//! the model, and that every sound the mixer hears can be attributed back to the
//! track it came through.

use std::path::PathBuf;

use ve_core::{
    AudioStreamInfo, Clip, ClipId, CompositionLayer, CompositionSettings, Fade, FadeCurve,
    MediaInfo, Project, Property, SequenceId, Track, TrackId, TrackKind,
};
use ve_engine::*;
use ve_metrics::Metrics;
use ve_time::{SampleRate, Ticks};

fn testdata(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../testdata").join(name)
}

fn tone_info(seconds: i64) -> MediaInfo {
    MediaInfo {
        duration: Ticks::from_seconds(seconds),
        video: None,
        audio: Some(AudioStreamInfo {
            sample_rate: SampleRate::HZ_48000,
            channels: 2,
            duration: Ticks::from_seconds(seconds),
            codec: "pcm_s16le".into(),
        }),
        container: "wav".into(),
    }
}

struct Fixture {
    project: Project,
    sequence: SequenceId,
    a1: TrackId,
    asset: ve_core::AssetId,
}

fn fixture() -> Fixture {
    let mut project = Project::with_default_sequence("Levels");
    let asset = project.add_asset(testdata("tone_48k.wav"), tone_info(1));
    let sequence = project.active_sequence.unwrap();
    let a1 = project
        .sequence(sequence)
        .unwrap()
        .tracks
        .iter()
        .find(|t| t.kind == TrackKind::Audio)
        .expect("the default sequence has an audio track")
        .id;
    Fixture { project, sequence, a1, asset }
}

impl Fixture {
    fn add_audio_track(&mut self, name: &str) -> TrackId {
        let id = self.project.new_track_id();
        self.project.sequence_mut(self.sequence).unwrap().tracks.push(Track::new(
            id,
            TrackKind::Audio,
            name,
        ));
        id
    }

    fn add_clip(&mut self, track: TrackId, start_ms: i64, dur_ms: i64) -> ClipId {
        let id = self.project.new_clip_id();
        let clip = Clip::new(
            id,
            self.asset,
            "tone",
            Ticks::ZERO,
            Ticks::from_millis(start_ms),
            Ticks::from_millis(dur_ms),
        );
        self.project
            .sequence_mut(self.sequence)
            .unwrap()
            .track_mut(track)
            .unwrap()
            .insert_clip(clip)
            .unwrap();
        id
    }

    fn track_mut(&mut self, track: TrackId) -> &mut Track {
        self.project.sequence_mut(self.sequence).unwrap().track_mut(track).unwrap()
    }

    fn clip_mut(&mut self, clip: ClipId) -> &mut Clip {
        self.project.sequence_mut(self.sequence).unwrap().find_clip_mut(clip).unwrap().1
    }

    fn plan(&self, at: Ticks) -> RenderPlan {
        let sequence = self.project.sequence(self.sequence).unwrap();
        evaluate_project(&self.project, sequence, at)
    }

    /// Mixes one block from `at` and reports the peak that came out, plus what
    /// each track was metered at.
    fn mix_peak(&self, at: Ticks, frames: usize) -> (f32, Vec<(TrackId, Meter)>) {
        let mut renderer = AudioRenderer::new(SampleRate::HZ_48000, 2, Metrics::new());
        renderer.register_asset(self.asset, testdata("tone_48k.wav"));
        renderer.seek(at);

        let (producer, consumer) = AudioRing::split(frames * 4);
        let sequence = self.project.sequence(self.sequence).unwrap();
        let written = renderer.render_into(&self.project, sequence, &producer, frames);
        assert!(written > 0, "the renderer produced nothing to measure");

        let mut out = vec![0.0f32; written * 2];
        consumer.fill(&mut out);
        let peak = out.iter().fold(0.0f32, |a, s| a.max(s.abs()));
        (peak, renderer.track_meters().to_vec())
    }
}

// ---- track level in the plan -------------------------------------------

#[test]
fn a_track_fader_scales_everything_the_track_contributes() {
    let mut f = fixture();
    let clip = f.add_clip(f.a1, 0, 4000);
    f.clip_mut(clip).audio.volume = Property::constant(0.5);
    f.track_mut(f.a1).volume = 0.25;

    let plan = f.plan(Ticks::from_seconds(1));
    assert_eq!(plan.audio.len(), 1);
    assert!(
        (plan.audio[0].gain - 0.125).abs() < 1e-12,
        "clip and track gain must multiply, got {}",
        plan.audio[0].gain
    );
}

#[test]
fn a_track_pan_offsets_the_clips_own_and_stays_in_range() {
    let mut f = fixture();
    let clip = f.add_clip(f.a1, 0, 4000);
    f.clip_mut(clip).audio.pan = Property::constant(-0.25);
    f.track_mut(f.a1).pan = 0.5;

    let plan = f.plan(Ticks::from_seconds(1));
    assert!((plan.audio[0].pan - 0.25).abs() < 1e-12, "pan {}", plan.audio[0].pan);

    // And a pair that would run off the end of the field is clamped rather
    // than wrapping round or exceeding hard right.
    f.clip_mut(clip).audio.pan = Property::constant(0.9);
    let plan = f.plan(Ticks::from_seconds(1));
    assert_eq!(plan.audio[0].pan, 1.0);
}

#[test]
fn every_audible_item_names_the_track_it_reached_the_mix_through() {
    let mut f = fixture();
    let a2 = f.add_audio_track("A2");
    f.add_clip(f.a1, 0, 4000);
    f.add_clip(a2, 0, 4000);

    let plan = f.plan(Ticks::from_seconds(1));
    let mut tracks: Vec<_> = plan.audio.iter().map(|a| a.track).collect();
    tracks.sort_by_key(|t| t.map(|t| t.to_string()));
    assert_eq!(tracks.len(), 2);
    assert!(tracks.iter().all(|t| t.is_some()), "a meter has nothing to group by without this");
    assert_ne!(tracks[0], tracks[1]);
}

#[test]
fn a_track_at_unity_changes_nothing_about_what_it_carries() {
    let mut f = fixture();
    let clip = f.add_clip(f.a1, 0, 4000);
    f.clip_mut(clip).audio.volume = Property::constant(0.75);
    f.clip_mut(clip).audio.pan = Property::constant(-0.5);

    assert!(f.project.sequence(f.sequence).unwrap().track(f.a1).unwrap().is_unity());
    let plan = f.plan(Ticks::from_seconds(1));
    assert_eq!(plan.audio[0].gain, 0.75);
    assert_eq!(plan.audio[0].pan, -0.5);
}

/// A composition previewed on its own has no sequence under it, so its sound
/// cannot be attributed to a track. Saying `None` is the honest answer; making
/// one up would put a level on a meter that does not exist.
#[test]
fn a_previewed_composition_has_no_track_to_attribute_its_sound_to() {
    let mut f = fixture();
    let comp = f.project.add_composition("Nested", CompositionSettings::default());
    let layer_id = f.project.new_layer_id();
    let layer =
        CompositionLayer::new(layer_id, "tone", f.asset, Ticks::ZERO, Ticks::from_seconds(2));
    f.project.composition_mut(comp).unwrap().push_layer(layer);

    let plan = evaluate_composition(&f.project, comp, Ticks::from_millis(500));
    assert_eq!(plan.audio.len(), 1);
    assert_eq!(plan.audio[0].track, None);
}

// ---- fades in the plan --------------------------------------------------

#[test]
fn a_fade_in_rises_from_silence_across_the_head_of_the_clip() {
    let mut f = fixture();
    let clip = f.add_clip(f.a1, 0, 4000);
    f.clip_mut(clip).audio.fade_in = Fade::new(Ticks::from_seconds(2), FadeCurve::Linear);

    let at = |ms: i64| f.plan(Ticks::from_millis(ms)).audio[0].gain;
    assert_eq!(at(0), 0.0, "the first instant of a fade-in is silent");
    assert!((at(1000) - 0.5).abs() < 1e-6, "halfway should be half, got {}", at(1000));
    assert_eq!(at(2000), 1.0, "past the fade the clip plays at its own level");
    assert_eq!(at(3000), 1.0);
}

#[test]
fn a_fade_out_falls_to_silence_at_the_tail_of_the_clip() {
    let mut f = fixture();
    let clip = f.add_clip(f.a1, 0, 4000);
    f.clip_mut(clip).audio.fade_out = Fade::new(Ticks::from_seconds(2), FadeCurve::Linear);

    let at = |ms: i64| f.plan(Ticks::from_millis(ms)).audio[0].gain;
    assert_eq!(at(0), 1.0);
    assert_eq!(at(2000), 1.0, "the fade starts two seconds from the end");
    assert!((at(3000) - 0.5).abs() < 1e-6, "got {}", at(3000));
    assert!(at(3999) < 0.01, "the last instant is all but silent, got {}", at(3999));
}

/// Fades multiply the level rather than replacing it, which is the whole reason
/// they are not keyframes on `volume`.
#[test]
fn a_fade_rides_on_top_of_the_clips_level_and_the_tracks() {
    let mut f = fixture();
    let clip = f.add_clip(f.a1, 0, 4000);
    f.clip_mut(clip).audio.volume = Property::constant(0.5);
    f.clip_mut(clip).audio.fade_in = Fade::new(Ticks::from_seconds(2), FadeCurve::Linear);
    f.track_mut(f.a1).volume = 0.5;

    let gain = f.plan(Ticks::from_seconds(1)).audio[0].gain;
    assert!((gain - 0.125).abs() < 1e-9, "0.5 clip x 0.5 fade x 0.5 track, got {gain}");
}

#[test]
fn a_clip_trimmed_shorter_than_its_fades_still_reaches_full_level() {
    let mut f = fixture();
    let clip = f.add_clip(f.a1, 0, 4000);
    f.clip_mut(clip).audio.fade_in = Fade::new(Ticks::from_seconds(3), FadeCurve::Linear);
    f.clip_mut(clip).audio.fade_out = Fade::new(Ticks::from_seconds(3), FadeCurve::Linear);

    // Six seconds of fade in a four second clip: they meet in the middle rather
    // than overlapping into a clip that never gets there.
    let middle = f.plan(Ticks::from_seconds(2)).audio[0].gain;
    assert!((middle - 1.0).abs() < 1e-6, "the fades should meet at unity, got {middle}");
}

// ---- levels and meters in the mix ---------------------------------------

#[test]
fn pulling_a_track_fader_down_is_audible_in_the_mixed_samples() {
    let mut f = fixture();
    f.add_clip(f.a1, 0, 900);

    let (full, _) = f.mix_peak(Ticks::ZERO, 2400);
    assert!(full > 0.5, "the fixture tone should mix near full scale, got {full}");

    f.track_mut(f.a1).volume = 0.25;
    let (quiet, _) = f.mix_peak(Ticks::ZERO, 2400);
    let ratio = quiet / full;
    assert!((ratio - 0.25).abs() < 0.01, "expected a quarter of the level, got {ratio}");
}

#[test]
fn the_meters_report_what_each_track_contributed_separately() {
    let mut f = fixture();
    let a2 = f.add_audio_track("A2");
    f.add_clip(f.a1, 0, 900);
    f.add_clip(a2, 0, 900);
    f.track_mut(a2).volume = 0.5;

    let (_, meters) = f.mix_peak(Ticks::ZERO, 2400);
    assert_eq!(meters.len(), 2, "each track gets its own reading");

    let peak_of = |track: TrackId| {
        meters.iter().find(|(id, _)| *id == track).expect("a reading for the track").1.peak()
    };
    let loud = peak_of(f.a1);
    let quiet = peak_of(a2);
    assert!(loud > 0.5, "the unity track read {loud}");
    let ratio = quiet / loud;
    assert!((ratio - 0.5).abs() < 0.02, "the halved track should read half, got {ratio}");
}

#[test]
fn a_silent_track_is_absent_from_the_meters_rather_than_reading_zero() {
    let mut f = fixture();
    let a2 = f.add_audio_track("A2");
    f.add_clip(f.a1, 0, 900);
    // Nothing on A2 at all.

    let (_, meters) = f.mix_peak(Ticks::ZERO, 2400);
    assert_eq!(meters.len(), 1);
    assert_eq!(meters[0].0, f.a1);
    assert!(!meters.iter().any(|(id, _)| *id == a2));
}

#[test]
fn a_fade_in_is_audible_in_the_mixed_samples_not_only_in_the_model() {
    let mut f = fixture();
    let clip = f.add_clip(f.a1, 0, 900);
    f.clip_mut(clip).audio.fade_in = Fade::new(Ticks::from_millis(800), FadeCurve::Linear);

    // The first 50 ms of an 800 ms fade: well under a tenth of full level.
    let (head, _) = f.mix_peak(Ticks::ZERO, 2400);
    // And a block starting past the fade, at its own level.
    let (body, _) = f.mix_peak(Ticks::from_millis(800), 2400);

    assert!(body > 0.5, "past the fade the tone should be near full, got {body}");
    assert!(head < body * 0.2, "the head of a fade should be much quieter: {head} vs {body}");
}

#[test]
fn seeking_clears_the_meters_so_they_do_not_describe_where_the_playhead_was() {
    let mut f = fixture();
    f.add_clip(f.a1, 0, 900);

    let mut renderer = AudioRenderer::new(SampleRate::HZ_48000, 2, Metrics::new());
    renderer.register_asset(f.asset, testdata("tone_48k.wav"));
    let (producer, _consumer) = AudioRing::split(48_000 * 2);
    let sequence = f.project.sequence(f.sequence).unwrap();
    renderer.render_into(&f.project, sequence, &producer, 2400);
    assert!(!renderer.track_meters().is_empty());

    renderer.seek(Ticks::from_seconds(30));
    assert!(renderer.track_meters().is_empty(), "a stale level would be a lie");
    assert_eq!(renderer.last_stats().master, Meter::SILENT);
}

#[test]
fn a_muted_track_contributes_neither_sound_nor_a_meter() {
    let mut f = fixture();
    f.add_clip(f.a1, 0, 900);
    f.track_mut(f.a1).muted = true;

    let plan = f.plan(Ticks::from_millis(100));
    assert!(plan.audio.is_empty());

    let (_, meters) = f.mix_peak(Ticks::ZERO, 2400);
    assert!(meters.is_empty());
}
