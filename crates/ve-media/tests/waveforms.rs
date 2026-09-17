//! Waveform analysis against the committed fixtures in `testdata/`.
//!
//! `tone_48k.wav` is one second of a 440 Hz sine at 0.8 full scale, generated
//! by `testdata/generate.py`. Knowing the signal exactly is what makes these
//! assertions about levels meaningful rather than "something came back": a sine
//! at amplitude A has peaks at ±A and an RMS of A/√2, and a summary that gets
//! either of those wrong would draw a waveform at the wrong height.

use std::path::PathBuf;
use std::time::Duration;

use ve_core::AssetId;
use ve_media::*;
use ve_metrics::Metrics;
use ve_time::{SampleRate, Ticks, TimeRange};

/// A distinct asset id per call. Ids are allocated by a project in the editor;
/// these tests only need them to differ.
fn next_asset() -> AssetId {
    use std::sync::atomic::{AtomicU64, Ordering};
    static NEXT: AtomicU64 = AtomicU64::new(1);
    AssetId::from_raw(NEXT.fetch_add(1, Ordering::Relaxed))
}

fn testdata(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../testdata").join(name)
}

/// The fixture's amplitude, from `testdata/generate.py`.
const TONE_AMPLITUDE: f32 = 0.8;

/// Waits for a service to go idle, failing rather than hanging forever.
fn wait_for_idle(service: &WaveformService) {
    let deadline = std::time::Instant::now() + Duration::from_secs(20);
    while !service.is_idle() {
        if std::time::Instant::now() > deadline {
            panic!("waveform analysis did not finish within 20 seconds");
        }
        std::thread::sleep(Duration::from_millis(5));
    }
}

#[test]
fn analysing_a_tone_reports_its_real_amplitude() {
    let mut decoder =
        AudioDecoder::open(testdata("tone_48k.wav"), SampleRate::HZ_48000, 2).unwrap();
    let waveform = analyse(&mut decoder).unwrap();

    // One second of audio at the fixed grid, within a bucket of exact.
    assert!(
        (waveform.len() as i64 - PEAKS_PER_SECOND).abs() <= 1,
        "produced {} buckets, expected about {PEAKS_PER_SECOND}",
        waveform.len()
    );

    let loudest = waveform.peaks().iter().fold(0.0f32, |a, p| a.max(p.amplitude()));
    assert!(
        (loudest - TONE_AMPLITUDE).abs() < 0.02,
        "peak was {loudest}, expected about {TONE_AMPLITUDE}"
    );

    // A sine's RMS is its amplitude over root two. Asserting this separately
    // from the peak is what proves the two are measuring different things:
    // a summary that stored the peak twice would pass the assertion above and
    // fail this one.
    let expected_rms = TONE_AMPLITUDE / 2.0f32.sqrt();
    let mean_rms = waveform.peaks().iter().map(|p| p.rms).sum::<f32>() / waveform.len() as f32;
    assert!(
        (mean_rms - expected_rms).abs() < 0.02,
        "rms was {mean_rms}, expected about {expected_rms}"
    );

    // A full-scale sine is symmetric, so the envelope must be too.
    let lowest = waveform.peaks().iter().fold(0.0f32, |a, p| a.min(p.min));
    assert!((lowest + TONE_AMPLITUDE).abs() < 0.02, "trough was {lowest}");
}

#[test]
fn the_waveform_of_a_file_with_video_comes_from_its_audio_stream() {
    // av_30fps.mp4 is two seconds of picture with the same tone under it.
    let waveform = analyse_file(testdata("av_30fps.mp4")).unwrap();
    assert!(waveform.analysed_duration() >= Ticks::from_millis(1900));
    let loudest = waveform.peaks().iter().fold(0.0f32, |a, p| a.max(p.amplitude()));
    // AAC rather than PCM here, so a wider tolerance: the point is that real
    // audio was analysed, not silence and not something an order out.
    assert!(loudest > 0.5 && loudest <= 1.0, "peak was {loudest}");
}

#[test]
fn a_file_with_no_audio_fails_rather_than_reporting_silence() {
    let result = analyse_file(testdata("counter_30fps.mp4"));
    assert!(
        matches!(result, Err(MediaError::NoAudioStream)),
        "expected NoAudioStream, got {result:?}"
    );
}

#[test]
fn the_service_analyses_in_the_background_and_reports_ready() {
    let service = WaveformService::new(16, Metrics::new());
    let asset = next_asset();

    assert_eq!(service.state(asset), WaveformState::Missing);
    service.request(asset, testdata("tone_48k.wav"), SampleRate::HZ_48000);
    assert!(!service.state(asset).is_missing(), "a request must register immediately");

    wait_for_idle(&service);
    assert_eq!(service.state(asset), WaveformState::Ready);

    let events = service.drain();
    assert!(
        events.contains(&WaveformEvent::Ready(asset)),
        "expected a Ready event, got {events:?}"
    );

    // And the peaks are the ones the blocking path produces.
    let columns = service
        .envelope(asset, TimeRange::new(Ticks::ZERO, Ticks::from_seconds(1)), 8)
        .expect("the waveform is present");
    assert_eq!(columns.len(), 8);
    for column in &columns {
        let peak = column.expect("a second of a one-second file is analysed");
        assert!((peak.amplitude() - TONE_AMPLITUDE).abs() < 0.05);
    }
}

#[test]
fn requesting_the_same_asset_twice_analyses_it_once() {
    let service = WaveformService::new(16, Metrics::new());
    let asset = next_asset();
    for _ in 0..10 {
        service.request(asset, testdata("tone_48k.wav"), SampleRate::HZ_48000);
    }
    wait_for_idle(&service);
    assert_eq!(service.stats().entries, 1);
    let ready = service.drain().iter().filter(|e| matches!(e, WaveformEvent::Ready(_))).count();
    assert_eq!(ready, 1, "the file was analysed more than once");
}

#[test]
fn an_unreadable_file_fails_once_and_stays_failed() {
    let service = WaveformService::new(16, Metrics::new());
    let asset = next_asset();
    service.request(asset, testdata("does-not-exist.wav"), SampleRate::HZ_48000);
    wait_for_idle(&service);

    match service.state(asset) {
        WaveformState::Failed(message) => assert!(!message.is_empty()),
        other => panic!("expected Failed, got {other:?}"),
    }

    // Asking again must not queue the broken file a second time: the timeline
    // asks on every repaint, and a failure that retries sixty times a second
    // is a file handle leak with a progress bar.
    service.request(asset, testdata("does-not-exist.wav"), SampleRate::HZ_48000);
    wait_for_idle(&service);
    let failures =
        service.drain().iter().filter(|e| matches!(e, WaveformEvent::Failed { .. })).count();
    assert_eq!(failures, 1);
}

#[test]
fn forgetting_an_asset_drops_its_peaks() {
    let service = WaveformService::new(16, Metrics::new());
    let asset = next_asset();
    service.request(asset, testdata("tone_48k.wav"), SampleRate::HZ_48000);
    wait_for_idle(&service);
    assert!(service.stats().bytes > 0);

    service.forget(asset);
    assert_eq!(service.state(asset), WaveformState::Missing);
    assert_eq!(service.stats().bytes, 0);
    assert_eq!(service.stats().entries, 0);

    // And it can be asked for again from scratch.
    service.request(asset, testdata("tone_48k.wav"), SampleRate::HZ_48000);
    wait_for_idle(&service);
    assert_eq!(service.state(asset), WaveformState::Ready);
}

#[test]
fn clearing_drops_everything() {
    let service = WaveformService::new(16, Metrics::new());
    let assets: Vec<AssetId> = (0..3).map(|_| next_asset()).collect();
    for asset in &assets {
        service.request(*asset, testdata("tone_48k.wav"), SampleRate::HZ_48000);
    }
    wait_for_idle(&service);
    assert_eq!(service.stats().entries, 3);

    service.clear();
    assert_eq!(service.stats().entries, 0);
    assert_eq!(service.stats().bytes, 0);
    for asset in &assets {
        assert_eq!(service.state(*asset), WaveformState::Missing);
    }
}

#[test]
fn the_budget_evicts_the_least_recently_used_waveform() {
    // Room for one second of audio and no more, so the second file has to
    // displace the first.
    let service = WaveformService::new(16, Metrics::new());
    let one_second = PEAKS_PER_SECOND as usize * std::mem::size_of::<Peak>();
    service.set_budget_bytes(one_second + one_second / 2);

    let first = next_asset();
    let second = next_asset();
    service.request(first, testdata("tone_48k.wav"), SampleRate::HZ_48000);
    wait_for_idle(&service);
    // Touch nothing, so `first` is the least recently used when `second` lands.
    service.request(second, testdata("tone_48k.wav"), SampleRate::HZ_48000);
    wait_for_idle(&service);

    assert!(service.stats().evictions > 0, "nothing was evicted under a zero budget");
    assert_eq!(service.state(first), WaveformState::Missing, "the older waveform survived");
    assert_eq!(service.state(second), WaveformState::Ready);
}

#[test]
fn an_asset_with_no_peaks_reports_no_columns_rather_than_silence() {
    let service = WaveformService::new(16, Metrics::new());
    let asset = next_asset();
    assert!(service
        .envelope(asset, TimeRange::new(Ticks::ZERO, Ticks::from_seconds(1)), 4)
        .is_none());

    service.request(asset, testdata("tone_48k.wav"), SampleRate::HZ_48000);
    wait_for_idle(&service);
    // Past the end of a one-second file: unknown, not silent.
    let columns = service
        .envelope(asset, TimeRange::new(Ticks::from_seconds(5), Ticks::from_seconds(1)), 4)
        .unwrap();
    assert!(columns.iter().all(|c| c.is_none()));
}

#[test]
fn dropping_the_service_stops_its_workers() {
    // Nothing to assert beyond termination: if shutdown did not reach the
    // workers, or the condvar wait missed it, this test hangs instead of
    // passing, which is the failure worth catching.
    let service = WaveformService::new(16, Metrics::new());
    service.request(next_asset(), testdata("tone_48k.wav"), SampleRate::HZ_48000);
    drop(service);
}
