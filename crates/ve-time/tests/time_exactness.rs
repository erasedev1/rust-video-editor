//! The time system's whole reason to exist is exactness. These tests pin that
//! down for every rate the editor claims to support.

use ve_time::{Rate, SampleRate, Ticks, TimeRange, Timecode, TICKS_PER_SECOND};

const VIDEO_RATES: &[(u32, u32)] = &[
    (24000, 1001),
    (24, 1),
    (25, 1),
    (30000, 1001),
    (30, 1),
    (48, 1),
    (50, 1),
    (60000, 1001),
    (60, 1),
    (120000, 1001),
    (120, 1),
    (240, 1),
];

const AUDIO_RATES: &[u32] =
    &[8000, 11025, 16000, 22050, 32000, 44100, 48000, 64000, 88200, 96000, 176400, 192000];

#[test]
fn timebase_factorisation_is_what_the_docs_claim() {
    assert_eq!(TICKS_PER_SECOND, 2i64.pow(10) * 3i64.pow(2) * 5i64.pow(4) * 7i64.pow(2));
    assert_eq!(TICKS_PER_SECOND, 282_240_000);
}

#[test]
fn every_supported_video_rate_has_an_exact_frame_duration() {
    for &(num, den) in VIDEO_RATES {
        let rate = Rate::new(num, den).unwrap();
        let exact = rate.frame_duration_exact();
        assert!(exact.is_some(), "{rate:?} has no exact frame duration");
        assert_eq!(
            exact.unwrap(),
            rate.frame_duration(),
            "{rate:?} rounded duration disagrees"
        );
    }
}

#[test]
fn every_supported_audio_rate_is_exact() {
    for &hz in AUDIO_RATES {
        let sr = SampleRate::new(hz).unwrap();
        assert!(sr.is_exact(), "{hz} Hz does not divide the timebase");
        // One second of audio is exactly `hz` samples, with nothing left over.
        assert_eq!(sr.ticks_to_sample(Ticks::SECOND), hz as i64);
        assert_eq!(sr.sample_to_ticks(hz as i64), Ticks::SECOND);
    }
}

#[test]
fn frame_conversion_round_trips_without_drift_over_a_long_timeline() {
    for &(num, den) in VIDEO_RATES {
        let rate = Rate::new(num, den).unwrap();
        // Ten hours at 60 fps is 2.16M frames; step through the whole span.
        let max_frame = rate.ticks_to_frame(Ticks::from_seconds(10 * 3600));
        let mut frame = 0i64;
        while frame <= max_frame {
            let t = rate.frame_to_ticks(frame);
            assert_eq!(rate.ticks_to_frame(t), frame, "{rate:?} drifted at frame {frame}");
            // A tick just before the next boundary must still be this frame.
            let next = rate.frame_to_ticks(frame + 1);
            assert_eq!(rate.ticks_to_frame(next - Ticks(1)), frame, "{rate:?} boundary off");
            frame += 997; // A prime stride, so we sample odd phases too.
        }
    }
}

#[test]
fn summing_frame_durations_matches_absolute_frame_positions() {
    // The classic accumulation bug: adding one frame at a time must agree with
    // computing the position directly, even for 1001-denominator rates.
    for &(num, den) in VIDEO_RATES {
        let rate = Rate::new(num, den).unwrap();
        let step = rate.frame_duration_exact().unwrap();
        let mut acc = Ticks::ZERO;
        for frame in 0..100_000i64 {
            assert_eq!(acc, rate.frame_to_ticks(frame), "{rate:?} diverged at frame {frame}");
            acc += step;
        }
    }
}

#[test]
fn ntsc_rates_land_on_whole_ticks() {
    // 29.97 fps is exactly 9_417_408 ticks per frame.
    assert_eq!(Rate::FPS_29_97.frame_duration_exact(), Some(Ticks(9_417_408)));
    // One hour of 29.97 is 107_892 frames, and that many frames is 3600.0 s
    // scaled by 1000/1001.
    let hour_frames = 107_892i64;
    let t = Rate::FPS_29_97.frame_to_ticks(hour_frames);
    assert_eq!(t.raw(), 9_417_408 * hour_frames);
    // That is the point of drop-frame: 107_892 frames is 3599.9964 s, i.e. one
    // hour of wall clock to within 3.6 ms, whereas a naive 108_000 frames would
    // have drifted 3.6 s ahead.
    assert!((t.as_secs_f64() - 3599.9964).abs() < 1e-6, "got {}", t.as_secs_f64());
    let naive = Rate::FPS_29_97.frame_to_ticks(108_000);
    assert!((naive.as_secs_f64() - 3603.6).abs() < 1e-9);
}

#[test]
fn ticks_to_frame_floors_toward_negative_infinity() {
    let r = Rate::FPS_25;
    let fd = r.frame_duration_exact().unwrap();
    assert_eq!(r.ticks_to_frame(Ticks::ZERO), 0);
    assert_eq!(r.ticks_to_frame(fd - Ticks(1)), 0);
    assert_eq!(r.ticks_to_frame(fd), 1);
    assert_eq!(r.ticks_to_frame(-Ticks(1)), -1);
    assert_eq!(r.ticks_to_frame(-fd), -1);
    assert_eq!(r.ticks_to_frame(-fd - Ticks(1)), -2);
}

#[test]
fn snapping_is_idempotent() {
    let r = Rate::FPS_23_976;
    let t = Ticks::from_secs_f64(12.3456789);
    let a = r.snap_floor(t);
    assert_eq!(a, r.snap_floor(a));
    let b = r.snap_round(t);
    assert_eq!(b, r.snap_round(b));
    assert!(a <= t);
}

#[test]
fn rates_are_stored_reduced() {
    assert_eq!(Rate::new(60000, 1000).unwrap(), Rate::FPS_60);
    assert_eq!(Rate::new(50, 2).unwrap(), Rate::FPS_25);
    assert!(Rate::new(0, 1).is_err());
    assert!(Rate::new(30, 0).is_err());
}

#[test]
fn nominal_fps_rounds_ntsc_rates_up() {
    assert_eq!(Rate::FPS_23_976.nominal_fps(), 24);
    assert_eq!(Rate::FPS_29_97.nominal_fps(), 30);
    assert_eq!(Rate::FPS_59_94.nominal_fps(), 60);
    assert_eq!(Rate::FPS_25.nominal_fps(), 25);
}

#[test]
fn only_ntsc_30_family_rates_are_drop_frame() {
    assert!(Rate::FPS_29_97.is_drop_frame());
    assert!(Rate::FPS_59_94.is_drop_frame());
    assert!(Rate::new(120000, 1001).unwrap().is_drop_frame());
    assert!(!Rate::FPS_23_976.is_drop_frame());
    assert!(!Rate::FPS_30.is_drop_frame());
    assert!(!Rate::FPS_25.is_drop_frame());
}

#[test]
fn non_drop_timecode_matches_hand_computed_values() {
    let r = Rate::FPS_25;
    assert_eq!(Timecode::from_frame(0, r).to_string(), "00:00:00:00");
    assert_eq!(Timecode::from_frame(24, r).to_string(), "00:00:00:24");
    assert_eq!(Timecode::from_frame(25, r).to_string(), "00:00:01:00");
    assert_eq!(Timecode::from_frame(25 * 60, r).to_string(), "00:01:00:00");
    assert_eq!(Timecode::from_frame(25 * 3600, r).to_string(), "01:00:00:00");
}

#[test]
fn drop_frame_timecode_skips_the_right_labels() {
    let r = Rate::FPS_29_97;
    // At the first minute boundary, labels :00 and :01 are skipped.
    assert_eq!(Timecode::from_frame(1799, r).to_string(), "00:00:59;29");
    assert_eq!(Timecode::from_frame(1800, r).to_string(), "00:01:00;02");
    // The tenth minute does not drop.
    assert_eq!(Timecode::from_frame(17982, r).to_string(), "00:10:00;00");
    // One hour of drop-frame is 107_892 frames and reads as exactly 01:00:00;00.
    assert_eq!(Timecode::from_frame(107_892, r).to_string(), "01:00:00;00");
}

#[test]
fn drop_frame_timecode_round_trips_across_an_hour() {
    let r = Rate::FPS_29_97;
    for frame in 0..107_892i64 {
        let tc = Timecode::from_frame(frame, r);
        assert_eq!(tc.to_frame(), frame, "round trip failed at frame {frame} ({tc})");
    }
}

#[test]
fn non_drop_timecode_round_trips() {
    for &(num, den) in VIDEO_RATES {
        let r = Rate::new(num, den).unwrap();
        if r.is_drop_frame() {
            continue;
        }
        for frame in (0..500_000i64).step_by(613) {
            let tc = Timecode::from_frame(frame, r);
            assert_eq!(tc.to_frame(), frame, "{r:?} failed at {frame}");
        }
    }
}

#[test]
fn timecode_parses_both_separators() {
    let tc = Timecode::parse("01:02:03;04", Rate::FPS_29_97).unwrap();
    assert!(tc.drop_frame);
    assert_eq!((tc.hours, tc.minutes, tc.seconds, tc.frames), (1, 2, 3, 4));

    let tc = Timecode::parse("01:02:03:04", Rate::FPS_29_97).unwrap();
    assert!(!tc.drop_frame);

    // A drop-frame marker on a rate that cannot drop is ignored, not an error.
    let tc = Timecode::parse("00:00:01;00", Rate::FPS_25).unwrap();
    assert!(!tc.drop_frame);

    assert!(Timecode::parse("1:2:3", Rate::FPS_25).is_err());
    assert!(
        Timecode::parse("00:00:00:25", Rate::FPS_25).is_err(),
        "frame 25 invalid at 25 fps"
    );
    assert!(Timecode::parse("00:60:00:00", Rate::FPS_25).is_err());
}

#[test]
fn negative_positions_render_and_parse() {
    let r = Rate::FPS_24;
    let tc = Timecode::from_frame(-25, r);
    assert_eq!(tc.to_string(), "-00:00:01:01");
    assert_eq!(tc.to_frame(), -25);
    assert_eq!(Timecode::parse("-00:00:01:01", r).unwrap().to_frame(), -25);
}

#[test]
fn timecode_ticks_round_trip() {
    let r = Rate::FPS_59_94;
    let t = r.frame_to_ticks(123_456);
    let tc = Timecode::from_ticks(t, r);
    assert_eq!(tc.to_ticks(), t);
}

#[test]
fn rational_conversion_matches_ffmpeg_style_timestamps() {
    // A packet at pts 1500 on a 1/90000 timebase is 1/60 s.
    let t = Ticks::from_rational(1500, 90_000);
    assert_eq!(t, Ticks(TICKS_PER_SECOND / 60));
    // A pts of 1024 on a 1/48000 audio timebase.
    assert_eq!(Ticks::from_rational(1024, 48_000), SampleRate::HZ_48000.sample_to_ticks(1024));
    assert_eq!(Ticks::from_rational(1, 0), Ticks::ZERO, "zero denominator must not panic");
}

#[test]
fn speed_scaling_is_symmetric_for_simple_ratios() {
    let d = Ticks::from_seconds(10);
    assert_eq!(d.scale(1, 2), Ticks::from_seconds(5));
    assert_eq!(d.scale(2, 1), Ticks::from_seconds(20));
    assert_eq!(d.scale(1, 2).scale(2, 1), d);
    assert_eq!(d.scale(1, 0), Ticks::ZERO);
}

#[test]
fn ticks_display_is_readable() {
    assert_eq!(Ticks::from_seconds(3661).to_string(), "01:01:01.000");
    assert_eq!(Ticks::from_millis(1500).to_string(), "00:00:01.500");
    assert_eq!((-Ticks::from_millis(1500)).to_string(), "-00:00:01.500");
}

#[test]
fn milliseconds_are_exact_and_microseconds_round() {
    assert_eq!(Ticks::from_millis(1000), Ticks::SECOND);
    assert_eq!(Ticks::from_millis(1).raw() * 1000, Ticks::SECOND.raw());

    // 1_000_000 does not divide the timebase, so microseconds round to the
    // nearest tick instead of truncating.
    assert_eq!(Ticks::from_micros(1_000_000), Ticks::SECOND);
    assert_eq!(Ticks::from_micros(25), Ticks(7056), "multiples of 25 us are exact");
    assert_eq!(Ticks::from_micros(1), Ticks(282), "7056/25 = 282.24 rounds to 282");
}

#[test]
fn adjacent_ranges_touch_but_do_not_overlap() {
    let a = TimeRange::new(Ticks::ZERO, Ticks::from_seconds(5));
    let b = TimeRange::new(Ticks::from_seconds(5), Ticks::from_seconds(5));
    assert_eq!(a.end(), b.start);
    assert!(!a.intersects(b));
    assert!(a.intersection(b).is_none());
    assert!(!a.contains(a.end()));
    assert!(a.contains(a.end() - Ticks(1)));
}

#[test]
fn overlapping_ranges_intersect_correctly() {
    let a = TimeRange::new(Ticks::from_seconds(0), Ticks::from_seconds(10));
    let b = TimeRange::new(Ticks::from_seconds(4), Ticks::from_seconds(10));
    assert!(a.intersects(b));
    let i = a.intersection(b).unwrap();
    assert_eq!(i.start, Ticks::from_seconds(4));
    assert_eq!(i.duration, Ticks::from_seconds(6));
    let u = a.union(b);
    assert_eq!(u.start, Ticks::ZERO);
    assert_eq!(u.end(), Ticks::from_seconds(14));
}

#[test]
fn empty_ranges_never_intersect() {
    let empty = TimeRange::new(Ticks::from_seconds(2), Ticks::ZERO);
    let a = TimeRange::new(Ticks::ZERO, Ticks::from_seconds(10));
    assert!(!a.intersects(empty));
    assert!(!empty.intersects(a));
    assert_eq!(a.union(empty), a);
    assert_eq!(empty.union(a), a);
}

#[test]
fn from_bounds_clamps_reversed_input() {
    let r = TimeRange::from_bounds(Ticks::from_seconds(10), Ticks::from_seconds(2));
    assert!(r.is_empty());
    assert_eq!(r.start, Ticks::from_seconds(10));
}

#[test]
fn ticks_serialise_as_plain_integers() {
    let t = Ticks::from_seconds(3);
    let json = serde_json::to_string(&t).unwrap();
    assert_eq!(json, (TICKS_PER_SECOND * 3).to_string());
    assert_eq!(serde_json::from_str::<Ticks>(&json).unwrap(), t);
}

#[test]
fn rate_serialises_as_a_rational_pair() {
    let json = serde_json::to_string(&Rate::FPS_29_97).unwrap();
    assert_eq!(json, r#"{"num":30000,"den":1001}"#);
    assert_eq!(serde_json::from_str::<Rate>(&json).unwrap(), Rate::FPS_29_97);
}
