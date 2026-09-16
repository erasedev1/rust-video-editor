//! Playback tests. Timing is driven by a fake clock rather than by sleeping, so
//! the transport's behaviour is asserted instead of hoped for.

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use ve_core::{
    Clip, ClipId, Interpolation, MediaInfo, Project, SequenceId, Size, TrackId, TrackKind,
    Vec2, VideoStreamInfo,
};
use ve_engine::*;
use ve_media::DecodeService;
use ve_metrics::Metrics;
use ve_time::{Rate, Ticks};

fn testdata(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../testdata").join(name)
}

fn video_info(seconds: i64) -> MediaInfo {
    MediaInfo {
        duration: Ticks::from_seconds(seconds),
        video: Some(VideoStreamInfo {
            size: Size::new(160, 120),
            rate: Rate::FPS_30,
            duration: Ticks::from_seconds(seconds),
            frame_count: Some(seconds * 30),
            codec: "h264".into(),
            pixel_format: "yuv444p".into(),
            sample_aspect_ratio: (1, 1),
        }),
        audio: None,
        container: "mp4".into(),
    }
}

struct Fixture {
    project: Project,
    sequence: SequenceId,
    v1: TrackId,
    v2: TrackId,
    a1: TrackId,
    asset: ve_core::AssetId,
}

fn fixture() -> Fixture {
    let mut project = Project::with_default_sequence("Playback");
    let asset = project.add_asset(testdata("counter_30fps.mp4"), video_info(3));
    let sequence = project.active_sequence.unwrap();
    let s = project.sequence(sequence).unwrap();
    let (v1, v2, a1) = (s.tracks[0].id, s.tracks[1].id, s.tracks[2].id);
    Fixture { project, sequence, v1, v2, a1, asset }
}

impl Fixture {
    fn add_clip(&mut self, track: TrackId, start_s: i64, dur_s: i64) -> ClipId {
        let id = self.project.new_clip_id();
        let clip = Clip::new(
            id,
            self.asset,
            "clip",
            Ticks::ZERO,
            Ticks::from_seconds(start_s),
            Ticks::from_seconds(dur_s),
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

    fn sequence(&self) -> &ve_core::Sequence {
        self.project.sequence(self.sequence).unwrap()
    }
}

// ---- composition evaluation -------------------------------------------

#[test]
fn an_empty_sequence_evaluates_to_nothing() {
    let f = fixture();
    let c = evaluate(f.sequence(), Ticks::ZERO);
    assert!(c.is_empty());
    assert_eq!(c.size, Size::new(1920, 1080));
    assert!(c.required_assets().is_empty());
}

#[test]
fn only_clips_covering_the_instant_are_included() {
    let mut f = fixture();
    f.add_clip(f.v1, 0, 2);
    f.add_clip(f.v1, 5, 2);

    assert_eq!(evaluate(f.sequence(), Ticks::from_seconds(1)).video.len(), 1);
    assert_eq!(evaluate(f.sequence(), Ticks::from_seconds(3)).video.len(), 0, "in the gap");
    assert_eq!(evaluate(f.sequence(), Ticks::from_seconds(6)).video.len(), 1);
    // Half-open: the instant a clip ends belongs to whatever comes next.
    assert_eq!(evaluate(f.sequence(), Ticks::from_seconds(2)).video.len(), 0);
}

#[test]
fn track_order_is_layer_order_with_v1_at_the_bottom() {
    let mut f = fixture();
    let bottom = f.add_clip(f.v1, 0, 5);
    let top = f.add_clip(f.v2, 0, 5);

    let c = evaluate(f.sequence(), Ticks::from_seconds(1));
    assert_eq!(c.video.len(), 2);
    assert_eq!(c.video[0].clip, bottom);
    assert_eq!(c.video[0].layer, 0);
    assert_eq!(c.video[1].clip, top);
    assert_eq!(c.video[1].layer, 1);
}

#[test]
fn source_time_accounts_for_the_clips_position_and_in_point() {
    let mut f = fixture();
    let id = f.project.new_clip_id();
    // Timeline 10s..20s showing source 5s..15s.
    let clip = Clip::new(
        id,
        f.asset,
        "c",
        Ticks::from_seconds(5),
        Ticks::from_seconds(10),
        Ticks::from_seconds(10),
    );
    f.project
        .sequence_mut(f.sequence)
        .unwrap()
        .track_mut(f.v1)
        .unwrap()
        .insert_clip(clip)
        .unwrap();

    let c = evaluate(f.sequence(), Ticks::from_seconds(13));
    assert_eq!(c.video[0].source_time, Ticks::from_seconds(8));
}

#[test]
fn a_muted_track_contributes_nothing() {
    let mut f = fixture();
    f.add_clip(f.v1, 0, 5);
    f.project.sequence_mut(f.sequence).unwrap().track_mut(f.v1).unwrap().muted = true;
    assert!(evaluate(f.sequence(), Ticks::from_seconds(1)).video.is_empty());
}

#[test]
fn a_disabled_clip_contributes_nothing() {
    let mut f = fixture();
    let id = f.add_clip(f.v1, 0, 5);
    f.project
        .sequence_mut(f.sequence)
        .unwrap()
        .find_clip_mut(id)
        .unwrap()
        .1
        .enabled = false;
    assert!(evaluate(f.sequence(), Ticks::from_seconds(1)).video.is_empty());
}

#[test]
fn soloing_a_video_track_excludes_the_others_but_not_the_audio() {
    let mut f = fixture();
    f.add_clip(f.v1, 0, 5);
    let soloed = f.add_clip(f.v2, 0, 5);
    f.add_clip(f.a1, 0, 5);

    f.project.sequence_mut(f.sequence).unwrap().track_mut(f.v2).unwrap().solo = true;
    let c = evaluate(f.sequence(), Ticks::from_seconds(1));

    assert_eq!(c.video.len(), 1);
    assert_eq!(c.video[0].clip, soloed);
    assert_eq!(c.audio.len(), 1, "video solo must not silence the mix");
}

#[test]
fn a_muted_solo_track_does_not_silence_everything() {
    let mut f = fixture();
    f.add_clip(f.v1, 0, 5);
    {
        let s = f.project.sequence_mut(f.sequence).unwrap();
        let t = s.track_mut(f.v2).unwrap();
        t.solo = true;
        t.muted = true;
    }
    // A track that is both soloed and muted must not win the solo and then
    // contribute nothing, leaving the composition empty.
    assert_eq!(evaluate(f.sequence(), Ticks::from_seconds(1)).video.len(), 1);
}

#[test]
fn animated_properties_resolve_at_clip_local_time() {
    let mut f = fixture();
    let id = f.project.new_clip_id();
    let mut clip = Clip::new(
        id,
        f.asset,
        "c",
        Ticks::ZERO,
        Ticks::from_seconds(10),
        Ticks::from_seconds(4),
    );
    // A fade over the clip's first two seconds, in clip-local time.
    clip.transform.opacity.set_keyframe(Ticks::ZERO, 0.0, Interpolation::Linear);
    clip.transform
        .opacity
        .set_keyframe(Ticks::from_seconds(2), 1.0, Interpolation::Linear);
    clip.transform.position.set_keyframe(Ticks::ZERO, Vec2::ZERO, Interpolation::Linear);
    clip.transform.position.set_keyframe(
        Ticks::from_seconds(2),
        Vec2::new(100.0, 0.0),
        Interpolation::Linear,
    );
    f.project
        .sequence_mut(f.sequence)
        .unwrap()
        .track_mut(f.v1)
        .unwrap()
        .insert_clip(clip)
        .unwrap();

    // Timeline 11s is clip-local 1s, which is halfway through the fade.
    let c = evaluate(f.sequence(), Ticks::from_seconds(11));
    let t = &c.video[0].transform;
    assert!((t.opacity - 0.5).abs() < 1e-9, "opacity {}", t.opacity);
    assert!((t.position.x - 50.0).abs() < 1e-9, "position {:?}", t.position);
}

#[test]
fn audio_gain_and_pan_resolve_from_the_clip() {
    let mut f = fixture();
    let id = f.project.new_clip_id();
    let mut clip =
        Clip::new(id, f.asset, "a", Ticks::ZERO, Ticks::ZERO, Ticks::from_seconds(4));
    clip.audio.volume = ve_core::Property::constant(0.5);
    clip.audio.pan = ve_core::Property::constant(-0.25);
    f.project
        .sequence_mut(f.sequence)
        .unwrap()
        .track_mut(f.a1)
        .unwrap()
        .insert_clip(clip)
        .unwrap();

    let c = evaluate(f.sequence(), Ticks::from_seconds(1));
    assert_eq!(c.audio.len(), 1);
    assert_eq!(c.audio[0].gain, 0.5);
    assert_eq!(c.audio[0].pan, -0.25);
    assert!(c.video.is_empty(), "an audio track must not produce picture");
}

#[test]
fn required_assets_are_deduplicated() {
    let mut f = fixture();
    f.add_clip(f.v1, 0, 5);
    f.add_clip(f.v2, 0, 5);
    let c = evaluate(f.sequence(), Ticks::from_seconds(1));
    assert_eq!(c.video.len(), 2);
    assert_eq!(c.required_assets(), vec![f.asset], "both clips share one asset");
}

#[test]
fn evaluating_a_frame_range_walks_the_timeline() {
    let mut f = fixture();
    f.add_clip(f.v1, 0, 1);
    let comps = evaluate_frames(f.sequence(), 0, 45);
    assert_eq!(comps.len(), 45);
    // The clip covers the first second, i.e. the first 30 frames at 30 fps.
    assert!(comps[0].video.len() == 1 && comps[29].video.len() == 1);
    assert!(comps[30].video.is_empty(), "past the end of the clip");
}

// ---- the playback clock ------------------------------------------------

#[test]
fn a_stopped_clock_does_not_advance() {
    let time = ManualTime::new();
    let clock = PlaybackClock::new(time.clone());
    assert!(!clock.is_playing());
    assert_eq!(clock.position(), Ticks::ZERO);

    time.advance(Duration::from_secs(5));
    assert_eq!(clock.position(), Ticks::ZERO, "a stopped clock must not drift");
}

#[test]
fn playing_advances_position_with_wall_clock_time() {
    let time = ManualTime::new();
    let mut clock = PlaybackClock::new(time.clone());
    clock.play(Ticks::ZERO);

    time.advance(Duration::from_secs(1));
    assert_eq!(clock.position(), Ticks::from_seconds(1));

    time.advance(Duration::from_millis(500));
    assert_eq!(clock.position(), Ticks::from_millis(1500));
}

#[test]
fn position_is_derived_rather_than_accumulated() {
    // Reading the position a hundred times must give the same answer as reading
    // it once: if position were accumulated per call it would race ahead.
    let time = ManualTime::new();
    let mut clock = PlaybackClock::new(time.clone());
    clock.play(Ticks::ZERO);
    time.advance(Duration::from_secs(1));

    let first = clock.position();
    for _ in 0..100 {
        assert_eq!(clock.position(), first);
    }
}

#[test]
fn a_long_playback_does_not_drift() {
    let time = ManualTime::new();
    let mut clock = PlaybackClock::new(time.clone());
    clock.play(Ticks::ZERO);

    // An hour, advanced in irregular steps, as a real frame loop would.
    let mut total = Duration::ZERO;
    for i in 0..10_000u64 {
        let step = Duration::from_micros(360_000 + (i % 7) * 1000);
        time.advance(step);
        total += step;
    }
    let expected = Ticks::from_rational(total.as_nanos() as i64, 1_000_000_000);
    let drift = (clock.position() - expected).abs();
    assert!(drift <= Ticks::new(1), "drifted by {drift} over {total:?}");
}

#[test]
fn stopping_freezes_the_position_and_resuming_continues_from_it() {
    let time = ManualTime::new();
    let mut clock = PlaybackClock::new(time.clone());
    clock.play(Ticks::ZERO);
    time.advance(Duration::from_secs(2));

    let stopped_at = clock.stop();
    assert_eq!(stopped_at, Ticks::from_seconds(2));

    // Time passing while stopped must not move the playhead.
    time.advance(Duration::from_secs(10));
    assert_eq!(clock.position(), Ticks::from_seconds(2));

    clock.resume();
    time.advance(Duration::from_secs(1));
    assert_eq!(clock.position(), Ticks::from_seconds(3));
}

#[test]
fn seeking_works_both_stopped_and_playing() {
    let time = ManualTime::new();
    let mut clock = PlaybackClock::new(time.clone());

    clock.seek(Ticks::from_seconds(30));
    assert_eq!(clock.position(), Ticks::from_seconds(30));

    clock.play(Ticks::from_seconds(30));
    time.advance(Duration::from_secs(1));
    clock.seek(Ticks::from_seconds(5));
    assert_eq!(clock.position(), Ticks::from_seconds(5), "a seek must land exactly");

    time.advance(Duration::from_secs(1));
    assert_eq!(clock.position(), Ticks::from_seconds(6), "and keep playing from there");
}

#[test]
fn a_negative_seek_clamps_to_zero() {
    let time = ManualTime::new();
    let mut clock = PlaybackClock::new(time.clone());
    clock.seek(-Ticks::from_seconds(10));
    assert_eq!(clock.position(), Ticks::ZERO);
}

#[test]
fn speed_scales_playback_without_moving_the_playhead() {
    let time = ManualTime::new();
    let mut clock = PlaybackClock::new(time.clone());
    clock.play(Ticks::ZERO);
    time.advance(Duration::from_secs(1));
    assert_eq!(clock.position(), Ticks::from_seconds(1));

    // Changing speed must not itself cause a jump.
    clock.set_speed(2.0);
    assert_eq!(clock.position(), Ticks::from_seconds(1));

    time.advance(Duration::from_secs(1));
    assert_eq!(clock.position(), Ticks::from_seconds(3), "one second at 2x is two");

    // And the first second must not be retroactively rescaled.
    clock.set_speed(0.5);
    time.advance(Duration::from_secs(2));
    assert_eq!(clock.position(), Ticks::from_seconds(4));
}

#[test]
fn playback_stops_at_the_limit() {
    let time = ManualTime::new();
    let mut clock = PlaybackClock::new(time.clone());
    clock.set_limit(Some(Ticks::from_seconds(5)));
    clock.play(Ticks::ZERO);

    time.advance(Duration::from_secs(3));
    assert!(!clock.has_reached_limit());
    assert_eq!(clock.position(), Ticks::from_seconds(3));

    time.advance(Duration::from_secs(10));
    assert!(clock.has_reached_limit());
    assert_eq!(clock.position(), Ticks::from_seconds(5), "must not run past the end");
}

#[test]
fn looping_wraps_instead_of_stopping() {
    let time = ManualTime::new();
    let mut clock = PlaybackClock::new(time.clone());
    clock.set_limit(Some(Ticks::from_seconds(4)));
    clock.set_looping(true, Ticks::from_seconds(1));
    clock.play(Ticks::from_seconds(1));

    time.advance(Duration::from_secs(2));
    assert_eq!(clock.position(), Ticks::from_seconds(3));

    // Past the limit: wraps back into the loop range rather than stopping.
    time.advance(Duration::from_secs(2));
    assert_eq!(clock.position(), Ticks::from_seconds(2));
    assert!(!clock.has_reached_limit(), "a looping clock never reaches its limit");
}

// ---- the engine --------------------------------------------------------

fn engine(metrics: &Metrics) -> (PlaybackEngine, Arc<ManualTime>) {
    let time = ManualTime::new();
    let decode = Arc::new(DecodeService::new(64, metrics.clone()));
    let clock = PlaybackClock::new(time.clone());
    (PlaybackEngine::new(clock, decode, metrics.clone()), time)
}

/// Pumps the engine until every layer has its frame, or gives up.
fn settle(engine: &mut PlaybackEngine, sequence: &ve_core::Sequence) -> EngineUpdate {
    let mut last = engine.update(sequence);
    for _ in 0..200 {
        if last.is_complete() {
            return last;
        }
        std::thread::sleep(Duration::from_millis(5));
        last = engine.update(sequence);
    }
    last
}

#[test]
fn the_engine_opens_every_asset_a_project_references() {
    let f = fixture();
    let metrics = Metrics::new();
    let (engine, _time) = engine(&metrics);

    let failures = engine.open_project_assets(&f.project);
    assert!(failures.is_empty(), "{failures:?}");
    assert!(engine.decode_service().is_open(f.asset));
}

#[test]
fn an_unopenable_asset_is_reported_without_stopping_the_rest() {
    let mut f = fixture();
    let missing = f.project.add_asset("/definitely/not/here.mp4", video_info(3));
    let metrics = Metrics::new();
    let (engine, _time) = engine(&metrics);

    let failures = engine.open_project_assets(&f.project);
    assert_eq!(failures.len(), 1);
    assert_eq!(failures[0].0, missing);
    assert!(engine.decode_service().is_open(f.asset), "the good asset still opened");
}

#[test]
fn updating_resolves_layers_once_their_frames_decode() {
    let mut f = fixture();
    f.add_clip(f.v1, 0, 3);
    let metrics = Metrics::new();
    let (mut engine, _time) = engine(&metrics);
    engine.open_project_assets(&f.project);

    let update = settle(&mut engine, f.sequence());
    assert_eq!(update.composition.video.len(), 1);
    assert_eq!(update.layers.len(), 1, "the frame should have decoded");
    assert_eq!(update.pending, 0);
    assert_eq!(update.layers[0].frame.size(), Size::new(160, 120));
}

#[test]
fn a_frame_that_is_not_ready_is_reported_pending_rather_than_waited_for() {
    let mut f = fixture();
    f.add_clip(f.v1, 0, 3);
    let metrics = Metrics::new();
    let (mut engine, _time) = engine(&metrics);
    engine.open_project_assets(&f.project);

    // The very first update cannot have a decoded frame yet, and must return
    // immediately rather than blocking on one.
    let start = std::time::Instant::now();
    let update = engine.update(f.sequence());
    assert!(start.elapsed() < Duration::from_millis(100), "update blocked on decoding");
    assert_eq!(update.pending, 1);
    assert!(update.layers.is_empty());
    assert!(!update.is_complete());
}

#[test]
fn scrubbing_moves_the_playhead_and_stops_at_a_frame_boundary() {
    let mut f = fixture();
    f.add_clip(f.v1, 0, 3);
    let metrics = Metrics::new();
    let (mut engine, _time) = engine(&metrics);
    engine.open_project_assets(&f.project);

    // A position between frame boundaries must snap onto one.
    engine.scrub_to(f.sequence(), Ticks::from_millis(1_234));
    let rate = f.sequence().rate();
    let position = engine.clock().position();
    assert_eq!(position, rate.snap_round(position));

    let update = settle(&mut engine, f.sequence());
    assert_eq!(update.position, position);
    assert_eq!(update.layers.len(), 1);
}

#[test]
fn playback_advances_the_composition_over_time() {
    let mut f = fixture();
    f.add_clip(f.v1, 0, 3);
    let metrics = Metrics::new();
    let (mut engine, time) = engine(&metrics);
    engine.open_project_assets(&f.project);

    engine.play(f.sequence());
    assert!(engine.is_playing());
    let first = engine.update(f.sequence()).position;

    time.advance(Duration::from_secs(1));
    let later = engine.update(f.sequence()).position;
    assert_eq!(later - first, Ticks::from_seconds(1));
}

#[test]
fn playback_stops_at_the_end_of_the_sequence() {
    let mut f = fixture();
    f.add_clip(f.v1, 0, 2);
    let metrics = Metrics::new();
    let (mut engine, time) = engine(&metrics);
    engine.open_project_assets(&f.project);

    engine.play(f.sequence());
    time.advance(Duration::from_secs(5));
    let update = engine.update(f.sequence());

    assert!(update.reached_end);
    assert!(!engine.is_playing(), "the transport must stop at the end");
    assert_eq!(engine.clock().position(), Ticks::from_seconds(2));
}

#[test]
fn pressing_play_at_the_end_rewinds_to_the_start() {
    let mut f = fixture();
    f.add_clip(f.v1, 0, 2);
    let metrics = Metrics::new();
    let (mut engine, _time) = engine(&metrics);

    engine.scrub_to(f.sequence(), Ticks::from_seconds(2));
    engine.play(f.sequence());
    assert_eq!(engine.clock().position(), Ticks::ZERO);
}

#[test]
fn stop_and_resume_keep_the_position() {
    let mut f = fixture();
    f.add_clip(f.v1, 0, 5);
    let metrics = Metrics::new();
    let (mut engine, time) = engine(&metrics);

    engine.play(f.sequence());
    time.advance(Duration::from_secs(1));
    let stopped = engine.stop();
    assert_eq!(stopped, Ticks::from_seconds(1));

    time.advance(Duration::from_secs(10));
    assert_eq!(engine.clock().position(), Ticks::from_seconds(1));

    engine.toggle_playback(f.sequence());
    assert!(engine.is_playing());
    time.advance(Duration::from_secs(1));
    assert_eq!(engine.clock().position(), Ticks::from_seconds(2));
}

#[test]
fn stepping_moves_exactly_one_frame_and_stops_playback() {
    let mut f = fixture();
    f.add_clip(f.v1, 0, 3);
    let metrics = Metrics::new();
    let (mut engine, _time) = engine(&metrics);
    engine.open_project_assets(&f.project);

    engine.play(f.sequence());
    engine.step_frames(f.sequence(), 1);
    assert!(!engine.is_playing(), "stepping must stop the transport");

    let rate = f.sequence().rate();
    let after_one = engine.clock().position();
    engine.step_frames(f.sequence(), 1);
    assert_eq!(engine.clock().position() - after_one, rate.frame_duration());

    // Stepping back past zero clamps rather than going negative.
    engine.step_frames(f.sequence(), -100);
    assert_eq!(engine.clock().position(), Ticks::ZERO);
}

#[test]
fn a_dropped_frame_during_playback_is_counted() {
    let mut f = fixture();
    f.add_clip(f.v1, 0, 3);
    let metrics = Metrics::new();
    let (mut engine, _time) = engine(&metrics);
    engine.open_project_assets(&f.project);

    engine.play(f.sequence());
    // The first update cannot have a frame ready, so it is a drop.
    let update = engine.update(f.sequence());
    assert_eq!(update.pending, 1);
    assert_eq!(metrics.dropped_frames(), 1);
    assert_eq!(metrics.presented_frames(), 0);

    // Once the frame lands, later updates present rather than drop.
    settle(&mut engine, f.sequence());
    assert!(metrics.presented_frames() > 0);
}

#[test]
fn multiple_tracks_resolve_to_multiple_layers_in_order() {
    let mut f = fixture();
    let bottom = f.add_clip(f.v1, 0, 3);
    let top = f.add_clip(f.v2, 0, 3);
    let metrics = Metrics::new();
    let (mut engine, _time) = engine(&metrics);
    engine.open_project_assets(&f.project);

    let update = settle(&mut engine, f.sequence());
    assert_eq!(update.layers.len(), 2);
    assert_eq!(update.layers[0].clip.clip, bottom);
    assert_eq!(update.layers[1].clip.clip, top);
}

#[test]
fn an_empty_instant_resolves_to_no_layers_without_requesting_anything() {
    let mut f = fixture();
    f.add_clip(f.v1, 10, 3);
    let metrics = Metrics::new();
    let (mut engine, _time) = engine(&metrics);
    engine.open_project_assets(&f.project);

    engine.scrub_to(f.sequence(), Ticks::from_seconds(1));
    let update = engine.update(f.sequence());
    assert!(update.layers.is_empty());
    assert_eq!(update.pending, 0);
    assert!(update.is_complete(), "nothing to show is still a complete frame");
}

#[test]
fn peek_resolves_a_sequence_without_any_decoding() {
    let mut f = fixture();
    f.add_clip(f.v1, 0, 3);
    let c = peek(&f.project, f.sequence, Ticks::from_seconds(1)).unwrap();
    assert_eq!(c.video.len(), 1);
    assert!(peek(&f.project, SequenceId::from_raw(999), Ticks::ZERO).is_none());
}

// ---- audio -------------------------------------------------------------

#[test]
fn the_audio_renderer_mixes_a_clip_into_the_ring() {
    let mut project = Project::with_default_sequence("Audio");
    let asset = project.add_asset(
        testdata("tone_48k.wav"),
        MediaInfo {
            duration: Ticks::from_seconds(1),
            video: None,
            audio: Some(ve_core::AudioStreamInfo {
                sample_rate: ve_time::SampleRate::HZ_48000,
                channels: 2,
                duration: Ticks::from_seconds(1),
                codec: "pcm_s16le".into(),
            }),
            container: "wav".into(),
        },
    );
    let sequence = project.active_sequence.unwrap();
    let a1 = project.sequence(sequence).unwrap().tracks[2].id;
    assert_eq!(project.sequence(sequence).unwrap().tracks[2].kind, TrackKind::Audio);

    let id = project.new_clip_id();
    let clip =
        Clip::new(id, asset, "tone", Ticks::ZERO, Ticks::ZERO, Ticks::from_millis(900));
    project
        .sequence_mut(sequence)
        .unwrap()
        .track_mut(a1)
        .unwrap()
        .insert_clip(clip)
        .unwrap();

    let metrics = Metrics::new();
    let mut renderer = AudioRenderer::new(ve_time::SampleRate::HZ_48000, 2, metrics);
    renderer.register_asset(asset, testdata("tone_48k.wav"));

    let (producer, consumer) = AudioRing::split(48_000 * 2);
    let written = renderer.render_into(project.sequence(sequence).unwrap(), &producer, 4800);
    assert!(written > 0, "the renderer produced no audio");

    let mut out = vec![0.0f32; written * 2];
    consumer.fill(&mut out);
    let peak = out.iter().fold(0.0f32, |a, s| a.max(s.abs()));
    // The fixture tone is at 0.8 full scale, so real audio came through.
    assert!(peak > 0.5, "mixed peak was {peak}");

    // And the renderer advanced by exactly what it wrote.
    assert_eq!(
        renderer.position(),
        ve_time::SampleRate::HZ_48000.sample_to_ticks(written as i64)
    );
}

#[test]
fn an_instant_with_no_audio_clips_mixes_silence() {
    let project = Project::with_default_sequence("Silent");
    let sequence = project.active_sequence.unwrap();
    let metrics = Metrics::new();
    let mut renderer = AudioRenderer::new(ve_time::SampleRate::HZ_48000, 2, metrics);

    let (producer, consumer) = AudioRing::split(4096);
    let written = renderer.render_into(project.sequence(sequence).unwrap(), &producer, 512);
    assert_eq!(written, 512, "silence is still output, not a gap");

    let mut out = vec![9.0f32; 1024];
    consumer.fill(&mut out);
    assert!(out.iter().all(|s| *s == 0.0));
}

#[test]
fn the_audio_renderer_writes_no_more_than_the_ring_has_room_for() {
    let project = Project::with_default_sequence("Small");
    let sequence = project.active_sequence.unwrap();
    let metrics = Metrics::new();
    let mut renderer = AudioRenderer::new(ve_time::SampleRate::HZ_48000, 2, metrics);

    // Room for 100 stereo frames.
    let (producer, _consumer) = AudioRing::split(200);
    let written = renderer.render_into(project.sequence(sequence).unwrap(), &producer, 10_000);
    assert_eq!(written, 100);

    // A full ring means there is nothing to do, not an error.
    assert_eq!(renderer.render_into(project.sequence(sequence).unwrap(), &producer, 10_000), 0);
}

#[test]
fn seeking_the_audio_renderer_moves_where_it_mixes_from() {
    let project = Project::with_default_sequence("Seek");
    let sequence = project.active_sequence.unwrap();
    let metrics = Metrics::new();
    let mut renderer = AudioRenderer::new(ve_time::SampleRate::HZ_48000, 2, metrics);

    renderer.seek(Ticks::from_seconds(5));
    assert_eq!(renderer.position(), Ticks::from_seconds(5));

    let (producer, _c) = AudioRing::split(4096);
    renderer.render_into(project.sequence(sequence).unwrap(), &producer, 480);
    assert_eq!(renderer.position(), Ticks::from_seconds(5) + Ticks::from_millis(10));
}
