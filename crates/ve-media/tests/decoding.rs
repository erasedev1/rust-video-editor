//! Decoding tests against the committed fixtures in `testdata/`.
//!
//! Each fixture frame is a solid colour derived from its own index, so these
//! tests can assert *which* frame came back rather than merely that decoding
//! produced something. That is what makes them meaningful tests of seek
//! accuracy.

use std::path::PathBuf;
use std::time::Duration;

use ve_core::{AssetId, Size};
use ve_media::*;
use ve_metrics::Metrics;
use ve_time::{Rate, SampleRate, Ticks};

fn testdata(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../testdata").join(name)
}

/// The colour `testdata/generate.py` paints frame `i`: the index as two
/// base-10 digits, 25 levels apart, so no two frames are confusable.
fn expected_colour(i: i64) -> [u8; 3] {
    [((i % 10) * 25) as u8, ((i / 10) * 25) as u8, 200]
}

/// Reads the pixel at (4, 4), safely inside the solid-colour field.
fn sample_pixel(frame: &VideoFrame) -> [u8; 3] {
    let row = frame.row(4);
    let x = 4 * 4; // 4 px in, 4 bytes per pixel
    [row[x], row[x + 1], row[x + 2]]
}

/// Which frame index a decoded frame's colour identifies.
///
/// Picks the *nearest* encodable colour rather than the first within some
/// tolerance, then insists the match is unambiguous. Frames are 25 levels
/// apart and lossless 4:4:4 round-trips within a couple of levels, so a
/// distance above 12 means the decoder returned something that is not a
/// fixture frame at all.
fn identify_frame(frame: &VideoFrame) -> Option<i64> {
    let [r, g, b] = sample_pixel(frame);
    let distance = |i: i64| {
        let e = expected_colour(i);
        (r as i32 - e[0] as i32)
            .abs()
            .max((g as i32 - e[1] as i32).abs().max((b as i32 - e[2] as i32).abs()))
    };
    let best = (0..100).min_by_key(|&i| distance(i))?;
    (distance(best) <= 12).then_some(best)
}

#[test]
fn probing_reports_the_fixture_metadata() {
    let info = probe(testdata("counter_30fps.mp4")).unwrap();
    let video = info.video.as_ref().expect("fixture has video");

    assert_eq!(video.size, Size::new(160, 120));
    assert_eq!(video.rate, Rate::FPS_30);
    assert_eq!(video.sample_aspect_ratio, (1, 1));
    assert_eq!(video.codec, "h264");
    assert!(info.audio.is_none(), "this fixture is video only");

    // 90 frames at 30 fps is exactly three seconds.
    assert_eq!(info.duration, Ticks::from_seconds(3));
    assert!(info.container.contains("mp4") || info.container.contains("mov"));
}

#[test]
fn probing_preserves_ntsc_rates_as_exact_rationals() {
    let info = probe(testdata("counter_2997fps.mp4")).unwrap();
    let video = info.video.unwrap();
    assert_eq!(video.rate, Rate::FPS_29_97, "29.97 must not be rounded to 30");
    assert_eq!(video.rate.numerator(), 30000);
    assert_eq!(video.rate.denominator(), 1001);
}

#[test]
fn probing_reports_audio_streams() {
    let info = probe(testdata("tone_48k.wav")).unwrap();
    let audio = info.audio.expect("fixture has audio");
    assert_eq!(audio.sample_rate, SampleRate::HZ_48000);
    assert_eq!(audio.channels, 2);
    assert!(info.video.is_none());
    assert_eq!(info.duration, Ticks::from_seconds(1));
}

#[test]
fn probing_reports_both_streams_of_a_muxed_file() {
    let info = probe(testdata("av_30fps.mp4")).unwrap();
    assert!(info.has_video() && info.has_audio());
    assert_eq!(info.video.unwrap().rate, Rate::FPS_30);
    assert_eq!(info.audio.unwrap().channels, 2);
}

#[test]
fn probing_a_missing_file_reports_the_path() {
    let err = probe(testdata("does_not_exist.mp4")).unwrap_err();
    assert!(matches!(err, MediaError::Open { .. }));
    assert!(err.to_string().contains("does_not_exist.mp4"), "{err}");
}

#[test]
fn probing_a_non_media_file_fails_cleanly() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("notmedia.mp4");
    std::fs::write(&path, b"this is definitely not a video").unwrap();
    assert!(probe(&path).is_err());
}

#[test]
fn decoding_sequentially_yields_every_frame_in_order() {
    let mut decoder = VideoDecoder::open(testdata("counter_30fps.mp4")).unwrap();
    assert_eq!(decoder.source_size(), Size::new(160, 120));

    let mut seen = Vec::new();
    while let Some(frame) = decoder.next_frame().unwrap() {
        assert_eq!(frame.size(), Size::new(160, 120));
        assert_eq!(frame.format(), PixelFormat::Rgba8);
        seen.push(identify_frame(&frame).expect("frame colour should identify it"));
    }

    assert_eq!(seen.len(), 90, "the fixture has 90 frames");
    assert_eq!(seen, (0..90).collect::<Vec<_>>(), "frames must arrive in order");
}

#[test]
fn every_decoded_frame_is_fully_opaque_rgba() {
    let mut decoder = VideoDecoder::open(testdata("counter_30fps.mp4")).unwrap();
    let frame = decoder.next_frame().unwrap().unwrap();
    let row = frame.row(10);
    // Alpha is the fourth byte of each pixel and must be opaque for source
    // media that has no alpha channel, or compositing would make it vanish.
    for px in row.as_chunks::<4>().0.iter().take(40) {
        assert_eq!(px[3], 255, "opaque source decoded with alpha {}", px[3]);
    }
}

#[test]
fn seeking_lands_on_the_exact_requested_frame() {
    let mut decoder = VideoDecoder::open(testdata("counter_30fps.mp4")).unwrap();
    let rate = Rate::FPS_30;

    // Deliberately jump around, including backwards and across the short GOPs
    // the fixture was encoded with.
    for &target in &[0i64, 45, 12, 89, 3, 60, 1, 30, 88, 7] {
        let t = rate.frame_to_ticks(target);
        let frame = decoder.frame_at(t).unwrap().expect("frame should exist");
        let got = identify_frame(&frame).expect("frame colour should identify it");
        assert_eq!(got, target, "requested frame {target} but decoded frame {got}");
    }
}

#[test]
fn a_time_between_frame_boundaries_returns_the_frame_that_covers_it() {
    let mut decoder = VideoDecoder::open(testdata("counter_30fps.mp4")).unwrap();
    let rate = Rate::FPS_30;

    // Two thirds of the way into frame 10 is still frame 10.
    let inside = rate.frame_to_ticks(10) + rate.frame_duration() * 2 / 3;
    let frame = decoder.frame_at(inside).unwrap().unwrap();
    assert_eq!(identify_frame(&frame), Some(10));

    // One tick before frame 11 starts is also still frame 10.
    let just_before = rate.frame_to_ticks(11) - Ticks::new(1);
    let frame = decoder.frame_at(just_before).unwrap().unwrap();
    assert_eq!(identify_frame(&frame), Some(10));
}

#[test]
fn seeking_is_accurate_at_ntsc_rates() {
    let mut decoder = VideoDecoder::open(testdata("counter_2997fps.mp4")).unwrap();
    assert_eq!(decoder.rate(), Rate::FPS_29_97);
    let rate = Rate::FPS_29_97;

    // The 1001 denominator is exactly where naive float timing drifts; walking
    // deep into the file proves it does not.
    for &target in &[0i64, 1, 29, 30, 31, 45, 59] {
        let frame = decoder.frame_at(rate.frame_to_ticks(target)).unwrap().unwrap();
        assert_eq!(identify_frame(&frame), Some(target), "drift at frame {target}");
    }
}

#[test]
fn seeking_is_accurate_at_25fps() {
    let mut decoder = VideoDecoder::open(testdata("counter_25fps.mp4")).unwrap();
    let rate = Rate::FPS_25;
    for &target in &[0i64, 24, 25, 49, 10] {
        let frame = decoder.frame_at(rate.frame_to_ticks(target)).unwrap().unwrap();
        assert_eq!(identify_frame(&frame), Some(target));
    }
}

#[test]
fn requesting_past_the_end_returns_the_last_frame_rather_than_failing() {
    let mut decoder = VideoDecoder::open(testdata("counter_30fps.mp4")).unwrap();
    // A clip may legitimately extend past its source; that is not an error.
    let frame = decoder.frame_at(Ticks::from_seconds(60)).unwrap();
    assert!(frame.is_some(), "past the end should clamp, not fail");
    assert_eq!(identify_frame(&frame.unwrap()), Some(89));
}

#[test]
fn requesting_a_negative_time_returns_the_first_frame() {
    let mut decoder = VideoDecoder::open(testdata("counter_30fps.mp4")).unwrap();
    let frame = decoder.frame_at(-Ticks::from_seconds(5)).unwrap().unwrap();
    assert_eq!(identify_frame(&frame), Some(0));
}

#[test]
fn decoding_scaled_produces_smaller_frames_with_the_same_content() {
    let mut decoder =
        VideoDecoder::open_scaled(testdata("counter_30fps.mp4"), Some(Size::new(40, 30)))
            .unwrap();
    assert_eq!(decoder.output_size(), Size::new(40, 30));
    assert_eq!(decoder.source_size(), Size::new(160, 120));

    let frame = decoder.frame_at(Rate::FPS_30.frame_to_ticks(20)).unwrap().unwrap();
    assert_eq!(frame.size(), Size::new(40, 30));
    assert_eq!(identify_frame(&frame), Some(20), "scaling must not change which frame");

    // A quarter-size frame is a sixteenth of the memory, which is the point.
    assert!(frame.byte_size() < 40 * 30 * 4 + 4096);
}

#[test]
fn opening_a_file_without_video_reports_the_right_error() {
    assert!(matches!(
        VideoDecoder::open(testdata("tone_48k.wav")),
        Err(MediaError::NoVideoStream)
    ));
}

#[test]
fn frames_share_their_buffer_rather_than_copying_it() {
    let mut decoder = VideoDecoder::open(testdata("counter_30fps.mp4")).unwrap();
    let frame = decoder.next_frame().unwrap().unwrap();
    let clone = frame.clone();
    // Cloning a frame must be a refcount bump, not a memcpy: the cache, the
    // uploader and the UI all hold one at once.
    assert!(std::ptr::eq(frame.data().as_ptr(), clone.data().as_ptr()));
}

#[test]
fn decoding_records_metrics() {
    let metrics = Metrics::new();
    let mut decoder = VideoDecoder::open(testdata("counter_30fps.mp4"))
        .unwrap()
        .with_metrics(metrics.clone());
    decoder.frame_at(Rate::FPS_30.frame_to_ticks(40)).unwrap();

    assert!(metrics.span_stats(ve_metrics::spans::DECODE).is_some());
    assert!(metrics.span_stats(ve_metrics::spans::SCALE).is_some());
    assert!(metrics.counter(ve_metrics::counters::SEEKS) >= 1);
}

// ---- audio -------------------------------------------------------------

#[test]
fn audio_decodes_to_the_requested_format() {
    let mut decoder =
        AudioDecoder::open(testdata("tone_48k.wav"), SampleRate::HZ_48000, 2).unwrap();
    assert_eq!(decoder.sample_rate(), SampleRate::HZ_48000);
    assert_eq!(decoder.channels(), 2);

    let mut total_frames = 0usize;
    let mut peak = 0.0f32;
    while let Some(buffer) = decoder.next_buffer().unwrap() {
        assert_eq!(buffer.channels(), 2);
        assert_eq!(buffer.samples().len(), buffer.frame_count() * 2);
        total_frames += buffer.frame_count();
        peak = peak.max(buffer.peak());
    }

    // One second at 48 kHz, within a block of the exact count.
    assert!(
        (total_frames as i64 - 48_000).abs() < 2048,
        "decoded {total_frames} frames, expected about 48000"
    );
    // The fixture is generated at 0.8 full scale (see testdata/generate.py),
    // so this proves real samples came through rather than silence, and that
    // the i16 -> f32 conversion is correctly scaled rather than off by orders
    // of magnitude.
    assert!((peak - 0.8).abs() < 0.02, "peak amplitude was {peak}, expected about 0.8");
}

#[test]
fn audio_resamples_to_a_different_rate() {
    let mut decoder =
        AudioDecoder::open(testdata("tone_48k.wav"), SampleRate::HZ_44100, 2).unwrap();
    let mut total = 0usize;
    while let Some(b) = decoder.next_buffer().unwrap() {
        assert_eq!(b.sample_rate(), SampleRate::HZ_44100);
        total += b.frame_count();
    }
    assert!(
        (total as i64 - 44_100).abs() < 4096,
        "resampled to {total} frames, expected about 44100"
    );
}

#[test]
fn audio_downmixes_to_mono_when_asked() {
    let mut decoder =
        AudioDecoder::open(testdata("tone_48k.wav"), SampleRate::HZ_48000, 1).unwrap();
    let buffer = decoder.next_buffer().unwrap().unwrap();
    assert_eq!(buffer.channels(), 1);
    assert_eq!(buffer.samples().len(), buffer.frame_count());
}

#[test]
fn audio_seeks_and_reports_buffers_covering_the_request() {
    let mut decoder =
        AudioDecoder::open(testdata("tone_48k.wav"), SampleRate::HZ_48000, 2).unwrap();
    let at = Ticks::from_millis(500);
    let buffers = decoder.buffers_from(at, 4800).unwrap();

    assert!(!buffers.is_empty());
    let frames: usize = buffers.iter().map(|b| b.frame_count()).sum();
    assert!(frames >= 4800, "asked for 4800 frames, got {frames}");
    // Every returned block must actually overlap the requested position or later.
    assert!(buffers.iter().all(|b| b.end_pts() > at));
}

#[test]
fn an_audio_buffers_duration_matches_its_sample_count() {
    let buffer = AudioBuffer::silence(48_000, SampleRate::HZ_48000, 2, Ticks::ZERO);
    assert_eq!(buffer.frame_count(), 48_000);
    assert_eq!(buffer.duration(), Ticks::from_seconds(1));
    assert_eq!(buffer.end_pts(), Ticks::from_seconds(1));
    assert_eq!(buffer.peak(), 0.0);
}

#[test]
fn opening_a_file_without_audio_reports_the_right_error() {
    assert!(matches!(
        AudioDecoder::open(testdata("counter_30fps.mp4"), SampleRate::HZ_48000, 2),
        Err(MediaError::NoAudioStream)
    ));
}

// ---- the decode service ------------------------------------------------

fn service() -> (DecodeService, AssetId) {
    let svc = DecodeService::new(64, Metrics::new());
    let asset = AssetId::from_raw(1);
    svc.open(asset, testdata("counter_30fps.mp4"), None).unwrap();
    (svc, asset)
}

/// Waits for a specific frame to reach the cache.
fn await_frame(svc: &DecodeService, asset: AssetId, t: Ticks) -> VideoFrame {
    for _ in 0..200 {
        if let Some(f) = svc.cached_frame(asset, t) {
            return f;
        }
        if let Some(DecodeEvent::Failed(f)) = svc.recv_timeout(Duration::from_millis(50)) {
            panic!("decode failed: {}", f.message);
        }
    }
    panic!("timed out waiting for the frame at {t}");
}

#[test]
fn the_service_decodes_off_the_calling_thread() {
    let (svc, asset) = service();
    let t = Rate::FPS_30.frame_to_ticks(30);

    // Nothing is cached yet, and the lookup must not block to find that out.
    assert!(svc.cached_frame(asset, t).is_none());

    svc.request(FrameRequest::interactive(asset, t));
    let frame = await_frame(&svc, asset, t);
    assert_eq!(identify_frame(&frame), Some(30));

    // A second lookup is served straight from the cache.
    assert!(svc.cached_frame(asset, t).is_some());
    assert!(svc.cache_stats().hits > 0);
}

#[test]
fn scrubbing_cancels_obsolete_work_instead_of_queueing_it() {
    let (svc, asset) = service();
    let rate = Rate::FPS_30;

    // Fire a burst of interactive requests, exactly as dragging a playhead
    // across the timeline does.
    for frame in 0..80i64 {
        svc.request(FrameRequest::interactive(asset, rate.frame_to_ticks(frame)));
    }
    let final_time = rate.frame_to_ticks(79);
    let frame = await_frame(&svc, asset, final_time);

    // The last request is what the user is waiting for, and it must arrive.
    assert_eq!(identify_frame(&frame), Some(79));

    // The point of the single-slot queue: most of those 80 requests were
    // superseded rather than decoded.
    let decoded = svc.cache_stats().entries;
    assert!(
        decoded < 80,
        "decoded {decoded} of 80 superseded requests; obsolete work is not being cancelled"
    );
}

#[test]
fn prefetch_requests_are_queued_rather_than_replacing_each_other() {
    let (svc, asset) = service();
    let rate = Rate::FPS_30;

    for frame in 0..8i64 {
        svc.request(FrameRequest::prefetch(asset, rate.frame_to_ticks(frame)));
    }
    for frame in 0..8i64 {
        let t = rate.frame_to_ticks(frame);
        let got = await_frame(&svc, asset, t);
        assert_eq!(identify_frame(&got), Some(frame), "prefetch dropped frame {frame}");
    }
}

#[test]
fn completed_decodes_are_announced_as_events() {
    let (svc, asset) = service();
    let t = Rate::FPS_30.frame_to_ticks(15);
    svc.request(FrameRequest::interactive(asset, t));

    let mut got = None;
    for _ in 0..100 {
        if let Some(DecodeEvent::Ready(ready)) = svc.recv_timeout(Duration::from_millis(50)) {
            got = Some(ready);
            break;
        }
    }
    let ready = got.expect("a Ready event should have arrived");
    assert_eq!(ready.asset, asset);
    assert_eq!(identify_frame(&ready.frame), Some(15));
}

#[test]
fn closing_an_asset_stops_its_worker_and_frees_its_frames() {
    let (svc, asset) = service();
    let t = Rate::FPS_30.frame_to_ticks(5);
    svc.request(FrameRequest::interactive(asset, t));
    await_frame(&svc, asset, t);
    assert!(svc.cache_stats().entries > 0);

    svc.close(asset);
    assert!(!svc.is_open(asset));
    assert_eq!(svc.cache_stats().entries, 0);
    // Requesting against a closed asset is ignored, not a panic.
    svc.request(FrameRequest::interactive(asset, t));
    assert!(svc.cached_frame(asset, t).is_none());
}

#[test]
fn opening_the_same_asset_twice_is_a_no_op() {
    let (svc, asset) = service();
    svc.open(asset, testdata("counter_30fps.mp4"), None).unwrap();
    assert!(svc.is_open(asset));
}

#[test]
fn the_service_reports_the_assets_frame_rate() {
    let (svc, asset) = service();
    assert_eq!(svc.asset_rate(asset), Some(Rate::FPS_30));
    assert_eq!(svc.asset_rate(AssetId::from_raw(99)), None);
}

#[test]
fn several_assets_decode_concurrently() {
    let svc = DecodeService::new(64, Metrics::new());
    let a = AssetId::from_raw(1);
    let b = AssetId::from_raw(2);
    svc.open(a, testdata("counter_30fps.mp4"), None).unwrap();
    svc.open(b, testdata("counter_25fps.mp4"), None).unwrap();

    svc.request(FrameRequest::interactive(a, Rate::FPS_30.frame_to_ticks(40)));
    svc.request(FrameRequest::interactive(b, Rate::FPS_25.frame_to_ticks(20)));

    assert_eq!(
        identify_frame(&await_frame(&svc, a, Rate::FPS_30.frame_to_ticks(40))),
        Some(40)
    );
    assert_eq!(
        identify_frame(&await_frame(&svc, b, Rate::FPS_25.frame_to_ticks(20))),
        Some(20)
    );
}

#[test]
fn dropping_the_service_shuts_its_workers_down() {
    // The real assertion is that this returns rather than hanging on a worker
    // that never notices the shutdown.
    //
    // What actually guarantees that is where the shutdown flag lives: inside
    // the queue, behind the mutex the worker evaluates its whole park condition
    // under. This test cannot prove that, and it would be dishonest to pretend
    // otherwise — the window a flag outside the lock opens is a few
    // instructions wide, and churn alone hits it only every few runs. It is
    // here as a smoke test over the paths a single tidy drop misses: several
    // threads dropping services at every point across a decode, with a watchdog
    // so that a worker which does deadlock fails the run instead of hanging it.
    let (done, finished) = std::sync::mpsc::channel();
    let churning: Vec<_> = (0..4)
        .map(|t| {
            let done = done.clone();
            std::thread::spawn(move || {
                for i in 0..60u64 {
                    let (svc, asset) = service();
                    svc.request(FrameRequest::interactive(
                        asset,
                        Rate::FPS_30.frame_to_ticks(((t * 60 + i) % 80) as i64),
                    ));
                    // Walks the drop across the decode rather than always
                    // landing in the same place relative to it.
                    std::thread::sleep(Duration::from_micros((i % 20) * 50));
                    drop(svc);
                }
                let _ = done.send(());
            })
        })
        .collect();
    drop(done);

    for _ in 0..churning.len() {
        assert!(
            finished.recv_timeout(Duration::from_secs(120)).is_ok(),
            "a decode worker never noticed the shutdown"
        );
    }
    for thread in churning {
        thread.join().expect("a shutdown loop panicked");
    }
}

#[test]
fn a_seek_lands_at_the_keyframe_before_the_target_not_at_the_start_of_the_file() {
    // The existing seek tests all assert on the frame that comes *back*, which
    // a broken seek still gets right: landing too early is still landing
    // before the target, and the decode that follows walks forward to the
    // right frame. So this asserts on where the seek itself put the reader.
    //
    // The fixture is written with a group of ten pictures, so frames 0, 10, 20
    // and so on are keyframes and a seek to frame 50 can land exactly on one.
    let mut decoder = VideoDecoder::open(testdata("counter_30fps.mp4")).unwrap();
    decoder.seek(Ticks::from_rational(50, 30)).unwrap();
    let landed = decoder.next_frame().unwrap().expect("a frame after seeking");

    assert_eq!(
        identify_frame(&landed),
        Some(50),
        "a seek to frame 50 in a file with keyframes every ten frames should land \
         on frame 50, not walk there from the beginning"
    );
}

#[test]
fn seeking_backwards_does_not_rewind_to_the_beginning() {
    // What scrubbing backwards does, over and over. A seek in the wrong unit
    // sends every one of these to frame 0 and decodes forward from there,
    // which is correct and unusably slow.
    let mut decoder = VideoDecoder::open(testdata("counter_30fps.mp4")).unwrap();
    let metrics = Metrics::new();
    let mut decoder = {
        decoder.seek(Ticks::ZERO).unwrap();
        decoder
    }
    .with_metrics(metrics.clone());

    // Walk backwards through the file, a group at a time.
    for frame in [80i64, 60, 40, 20] {
        decoder.frame_at(Ticks::from_rational(frame, 30)).unwrap().unwrap();
    }
    let decodes = metrics.span_stats(ve_metrics::spans::DECODE).map(|s| s.count).unwrap_or(0);

    // Four backward jumps, each landing on the keyframe at or before the
    // target and decoding at most a group from there. Rewinding to the start
    // each time would cost 80 + 60 + 40 + 20 frames instead.
    assert!(
        decodes < 60,
        "four backward seeks cost {decodes} decodes, which means they are \
         rewinding to the start of the file rather than seeking"
    );
}

// ---- resampling ---------------------------------------------------------

/// The regression this file exists to keep: a decoder asked for 48 kHz has to
/// *deliver* 48 kHz, not relabel whatever rate the file happened to hold.
///
/// Getting this wrong is silent. The samples are real, the buffers say what
/// rate they are, and every consumer believes them — so a 44.1 kHz file plays
/// 8.8% fast and draws its waveform 8.8% short, with nothing to show for it but
/// audio that is subtly wrong. It survived until a 16 kHz fixture was added,
/// where the error is a factor of three and impossible to miss.
#[test]
fn a_file_below_the_output_rate_is_resampled_up_to_it() {
    let path = testdata("sync_cam_a.wav");
    let info = probe(&path).expect("the fixture probes");
    let audio = info.audio.as_ref().expect("the fixture has audio");
    assert_eq!(audio.sample_rate, SampleRate::new(16_000).unwrap(), "fixture changed");
    let seconds = info.duration.as_secs_f64();

    let mut decoder = AudioDecoder::open(&path, SampleRate::HZ_48000, 2).unwrap();
    assert_eq!(decoder.sample_rate(), SampleRate::HZ_48000);

    let mut frames = 0usize;
    while let Some(buffer) = decoder.next_buffer().unwrap() {
        assert_eq!(buffer.sample_rate(), SampleRate::HZ_48000);
        assert_eq!(buffer.channels(), 2);
        frames += buffer.frame_count();
    }

    let decoded_seconds = frames as f64 / 48_000.0;
    assert!(
        (decoded_seconds - seconds).abs() < 0.01,
        "a {seconds}s file decoded to {decoded_seconds}s of 48 kHz audio \
         ({frames} frames); before this was fixed it came back as {}s",
        frames as f64 / 16_000.0
    );
}

/// The other direction has to be right too, and it is the one every editor
/// meets: 44.1 kHz music laid against 48 kHz camera sound.
#[test]
fn a_file_above_the_output_rate_is_resampled_down_to_it() {
    let path = testdata("tone_48k.wav");
    let seconds = probe(&path).unwrap().duration.as_secs_f64();

    let mut decoder = AudioDecoder::open(&path, SampleRate::HZ_44100, 2).unwrap();
    let mut frames = 0usize;
    while let Some(buffer) = decoder.next_buffer().unwrap() {
        assert_eq!(buffer.sample_rate(), SampleRate::HZ_44100);
        frames += buffer.frame_count();
    }

    let decoded_seconds = frames as f64 / 44_100.0;
    assert!(
        (decoded_seconds - seconds).abs() < 0.01,
        "a {seconds}s file decoded to {decoded_seconds}s at 44.1 kHz"
    );
}

/// A file already at the output rate must pass through untouched — the same
/// count, not a count that happens to be close.
#[test]
fn a_file_already_at_the_output_rate_is_not_disturbed() {
    let path = testdata("tone_48k.wav");
    let mut decoder = AudioDecoder::open(&path, SampleRate::HZ_48000, 2).unwrap();
    let mut frames = 0usize;
    while let Some(buffer) = decoder.next_buffer().unwrap() {
        frames += buffer.frame_count();
    }
    assert_eq!(frames, 48_000, "one second at 48 kHz, exactly");
}

/// And the waveform built from a resampled file has to cover the file, which is
/// what a timeline draws against.
#[test]
fn a_waveform_of_a_resampled_file_covers_the_whole_file() {
    let path = testdata("sync_cam_a.wav");
    let seconds = probe(&path).unwrap().duration.as_secs_f64();
    let waveform = analyse_file(&path).expect("the fixture analyses");

    let analysed = waveform.analysed_duration().as_secs_f64();
    assert!(
        (analysed - seconds).abs() < 0.02,
        "a {seconds}s file produced {analysed}s of peaks"
    );
}
