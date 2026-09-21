//! Syncing against real decoded audio.
//!
//! The algorithm is unit-tested on envelopes built by hand, where the right
//! answer is known exactly. What is tested here is the whole path: open two
//! files, decode them, reduce them to peaks, and measure how far apart they
//! start — through the same analysis the timeline's waveforms come from.
//!
//! The fixtures are one event recorded twice. `sync_cam_b` joined 1.5 seconds
//! late and was set 14 dB quieter, which is what two cameras at one shoot
//! actually look like.

use std::path::PathBuf;

use ve_media::{align, analyse_file, DEFAULT_MAX_OFFSET};
use ve_time::Ticks;

fn testdata(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../testdata").join(name)
}

/// How far apart the fixtures were generated. See `testdata/generate.py`.
const TRUE_OFFSET_SECONDS: f64 = 1.5;

#[test]
fn two_recordings_of_one_event_are_measured_against_each_other() {
    let a = analyse_file(testdata("sync_cam_a.wav")).expect("camera A analyses");
    let b = analyse_file(testdata("sync_cam_b.wav")).expect("camera B analyses");

    let found = align(&a, &b, DEFAULT_MAX_OFFSET).expect("two real recordings must align");

    let error = (found.offset.as_secs_f64() - TRUE_OFFSET_SECONDS).abs();
    assert!(
        error <= 0.01,
        "expected about {TRUE_OFFSET_SECONDS}s, got {:.4}s ({error:.4}s out)",
        found.offset.as_secs_f64()
    );
    assert!(found.is_confident(), "a real match scored only {:.3}", found.confidence);
}

/// The measurement is a *relative* one, so swapping the arguments must swap the
/// sign and nothing else. A camera being "1.5 seconds late" and the other being
/// "1.5 seconds early" are the same fact.
#[test]
fn the_measurement_is_symmetric() {
    let a = analyse_file(testdata("sync_cam_a.wav")).unwrap();
    let b = analyse_file(testdata("sync_cam_b.wav")).unwrap();

    let forward = align(&a, &b, DEFAULT_MAX_OFFSET).unwrap();
    let backward = align(&b, &a, DEFAULT_MAX_OFFSET).unwrap();

    let sum = (forward.offset + backward.offset).as_secs_f64().abs();
    assert!(sum <= 0.01, "{forward:?} and {backward:?} disagree by {sum}s");
}

/// The offset stored on an angle is the other sign, and this is the assertion
/// that keeps the two conventions from drifting apart: an angle that joined
/// late has group zero *before* its own first frame.
#[test]
fn the_angle_offset_places_group_zero_inside_the_late_camera() {
    let a = analyse_file(testdata("sync_cam_a.wav")).unwrap();
    let b = analyse_file(testdata("sync_cam_b.wav")).unwrap();
    let found = align(&a, &b, DEFAULT_MAX_OFFSET).unwrap();

    let offset = found.angle_offset();
    assert!(offset.is_negative(), "a late camera's offset is negative: {offset}");

    // With that offset, the two angles read the same instant of the event.
    let angle_a = ve_core::MulticamAngle::new(
        ve_core::AngleId::from_raw(1),
        "A",
        ve_core::AssetId::from_raw(1),
    );
    let angle_b = ve_core::MulticamAngle::new(
        ve_core::AngleId::from_raw(2),
        "B",
        ve_core::AssetId::from_raw(2),
    )
    .with_offset(offset);

    // Three seconds into the group: A is three seconds into its own file, and
    // B is one and a half, because it started that much later.
    let at = Ticks::from_seconds(3);
    assert_eq!(angle_a.source_time_at(at), Ticks::from_seconds(3));
    let b_time = angle_b.source_time_at(at).as_secs_f64();
    assert!((b_time - 1.5).abs() < 0.01, "camera B read {b_time}s");
}

/// A recording correlated with itself is the degenerate case, and it has to
/// come back as "no offset at all" rather than as some near-zero drift.
#[test]
fn a_recording_lines_up_with_itself_exactly() {
    let a = analyse_file(testdata("sync_cam_a.wav")).unwrap();
    let found = align(&a, &a, DEFAULT_MAX_OFFSET).expect("a file matches itself");
    assert_eq!(found.offset, Ticks::ZERO);
    assert!(found.confidence > 0.99, "{found:?}");
}

/// A steady tone has no events to align to. The honest answer is a low score,
/// not a confident number — this is the case that would otherwise put a
/// plausible-looking offset on a group synced from music-free room tone.
#[test]
fn a_featureless_recording_does_not_produce_a_confident_match() {
    let tone = analyse_file(testdata("tone_48k.wav")).expect("the tone analyses");
    let event = analyse_file(testdata("sync_cam_a.wav")).unwrap();

    if let Some(found) = align(&event, &tone, DEFAULT_MAX_OFFSET) {
        assert!(
            !found.is_confident(),
            "a steady tone scored {:.3} against a real event",
            found.confidence
        );
    }
}
