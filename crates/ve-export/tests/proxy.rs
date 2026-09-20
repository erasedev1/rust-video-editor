//! Proxy building, against the committed fixtures in `testdata/`.
//!
//! Every fixture frame is a solid colour encoding its own index, so these tests
//! assert *which* frame a proxy hands back rather than merely that it hands one
//! back. That is the whole question a proxy has to answer correctly: a proxy
//! that showed a different frame from its original at the same instant would
//! have the cut made against one picture and delivered from another.

use std::path::PathBuf;
use std::time::Duration;

use ve_core::{AssetId, MediaAsset, MediaInfo, Size};
use ve_export::proxy::{self, ProxyScale, ProxySettings};
use ve_export::{Cancel, ExportError};
use ve_media::{VideoDecoder, VideoFrame};
use ve_metrics::{spans, Metrics};
use ve_time::{Rate, Ticks};

fn testdata(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../testdata").join(name)
}

/// The colour `testdata/generate.py` paints frame `i`.
fn expected_colour(i: i64) -> [u8; 3] {
    [((i % 10) * 25) as u8, ((i / 10) * 25) as u8, 200]
}

fn sample_pixel(frame: &VideoFrame) -> [u8; 3] {
    let row = frame.row(4);
    let x = 4 * 4;
    [row[x], row[x + 1], row[x + 2]]
}

/// Which frame index a decoded picture's colour identifies.
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

/// Settings to build a proxy of `fixture` at an explicit size.
///
/// The fixtures are 160×120, which is below the size at which scaling is worth
/// doing at all, so [`ProxyScale`] would correctly leave them alone. These
/// tests want the scaling path exercised, so they state a size.
fn settings(fixture: &str, out: &std::path::Path, size: Size, rate: Rate) -> ProxySettings {
    ProxySettings {
        source: testdata(fixture),
        path: out.to_path_buf(),
        size,
        rate,
        duration: Ticks::from_seconds(3),
        codec: ve_export::VideoCodec::H264,
        quality: ve_export::Quality::High,
    }
}

#[test]
fn a_proxy_shows_the_same_frame_as_its_original_at_every_instant() {
    let dir = tempfile::tempdir().unwrap();
    let out = dir.path().join("counter.proxy.mov");
    let settings = settings("counter_30fps.mp4", &out, Size::new(80, 60), Rate::FPS_30);

    let report =
        proxy::build(&settings, &Metrics::new(), &Cancel::new(), |_| {}).expect("build");

    assert_eq!(report.frames, 90, "the fixture is 90 frames and so is its proxy");
    assert_eq!(report.size, Size::new(80, 60));
    assert!(report.bytes > 0);
    assert!(out.exists());

    let mut original = VideoDecoder::open(testdata("counter_30fps.mp4")).unwrap();
    let mut built = VideoDecoder::open(&out).unwrap();
    assert_eq!(built.source_size(), Size::new(80, 60), "the picture really is smaller");
    assert_eq!(built.rate(), Rate::FPS_30, "and declares the source's own rate");

    // Every frame, in order, from both files. This is the assertion the whole
    // feature rests on.
    for i in 0..90i64 {
        let t = Ticks::from_rational(i, 30);
        let from_original = original.frame_at(t).unwrap().expect("original frame");
        let from_proxy = built.frame_at(t).unwrap().expect("proxy frame");
        assert_eq!(
            identify_frame(&from_proxy),
            Some(i),
            "the proxy returned the wrong frame at {i}"
        );
        assert_eq!(
            identify_frame(&from_proxy),
            identify_frame(&from_original),
            "proxy and original disagree at frame {i}"
        );
    }
}

#[test]
fn a_proxy_is_all_intra_so_a_jump_costs_one_frame_rather_than_a_group() {
    let dir = tempfile::tempdir().unwrap();
    let out = dir.path().join("intra.mov");
    let settings = settings("counter_30fps.mp4", &out, Size::new(80, 60), Rate::FPS_30);
    proxy::build(&settings, &Metrics::new(), &Cancel::new(), |_| {}).expect("build");

    // The fixture is written with a group of ten pictures, so landing on frame
    // 9 means decoding frames 0..=9 to reconstruct it. An all-intra file means
    // decoding frame 9. Counting decode spans is how that is observed, because
    // it is the cost itself rather than a proxy for it.
    let decodes = |path: &std::path::Path, frame: i64| -> u64 {
        let metrics = Metrics::new();
        let mut decoder = VideoDecoder::open(path).unwrap().with_metrics(metrics.clone());
        // A fresh seek to a frame at the far end of a group, which is the
        // worst case and exactly what scrubbing does over and over.
        let got = decoder.frame_at(Ticks::from_rational(frame, 30)).unwrap().unwrap();
        assert_eq!(identify_frame(&got), Some(frame), "wrong frame in {}", path.display());
        metrics.span_stats(spans::DECODE).map(|s| s.count).unwrap_or(0)
    };

    let original = decodes(&testdata("counter_30fps.mp4"), 9);
    let built = decodes(&out, 9);

    assert!(
        built < original,
        "a proxy that costs as much to jump about in as its original ({built} decodes \
         against {original}) is not doing the job it exists for"
    );
    // One frame, plus the one-frame lookahead the decoder needs to know it has
    // the right one.
    assert!(built <= 2, "an all-intra file should need one decode, not {built}");
}

#[test]
fn a_cancelled_build_leaves_nothing_behind() {
    let dir = tempfile::tempdir().unwrap();
    let out = dir.path().join("cancelled.mov");
    let settings = settings("counter_30fps.mp4", &out, Size::new(80, 60), Rate::FPS_30);

    let cancel = Cancel::new();
    cancel.cancel();
    let result = proxy::build(&settings, &Metrics::new(), &cancel, |_| {});

    assert!(matches!(result, Err(ExportError::Cancelled)), "{result:?}");
    assert!(
        !out.exists(),
        "a part-written proxy has no index, and would be taken for a finished one"
    );
}

#[test]
fn a_proxy_preserves_an_ntsc_rate_exactly() {
    let dir = tempfile::tempdir().unwrap();
    let out = dir.path().join("ntsc.mov");
    let settings = settings("counter_2997fps.mp4", &out, Size::new(80, 60), Rate::FPS_29_97);
    let report =
        proxy::build(&settings, &Metrics::new(), &Cancel::new(), |_| {}).expect("build");
    assert!(report.frames > 0);

    let built = VideoDecoder::open(&out).unwrap();
    assert_eq!(built.rate(), Rate::FPS_29_97, "29.97 must not become 30");
    assert_eq!(built.rate().numerator(), 30000);
    assert_eq!(built.rate().denominator(), 1001);

    // And the frames still line up, which is the reason the rate matters: a
    // proxy at 30 would drift a frame and a half over an hour.
    let mut original = VideoDecoder::open(testdata("counter_2997fps.mp4")).unwrap();
    let mut built = VideoDecoder::open(&out).unwrap();
    for i in [0i64, 7, 29, 30, 61, 88] {
        let t = Ticks::from_rational(i * 1001, 30000);
        let a = identify_frame(&original.frame_at(t).unwrap().unwrap());
        let b = identify_frame(&built.frame_at(t).unwrap().unwrap());
        assert_eq!(a, b, "proxy and original disagree at 29.97 frame {i}");
    }
}

#[test]
fn a_proxy_of_a_file_with_sound_carries_none() {
    let dir = tempfile::tempdir().unwrap();
    let out = dir.path().join("av.mov");
    let settings = settings("av_30fps.mp4", &out, Size::new(80, 60), Rate::FPS_30);
    proxy::build(&settings, &Metrics::new(), &Cancel::new(), |_| {}).expect("build");

    // The original has sound; its proxy does not, because the mixer and the
    // waveforms read the original either way. Nothing has to keep two sound
    // tracks in step because there is only ever one.
    assert!(ve_media::probe(testdata("av_30fps.mp4")).unwrap().has_audio());
    let info = ve_media::probe(&out).unwrap();
    assert!(info.has_video());
    assert!(!info.has_audio(), "a proxy is a picture, not a file");
}

#[test]
fn a_batch_reports_each_file_and_attaches_what_it_built() {
    let dir = tempfile::tempdir().unwrap();
    let proxies = dir.path().join("Proxies");

    let mut asset = MediaAsset::new(
        AssetId::from_raw(1),
        testdata("counter_30fps.mp4"),
        MediaInfo::default(),
    );
    asset.info = ve_media::probe(&asset.path).unwrap();
    let mut settings =
        ProxySettings::for_asset(&asset, &proxies, ProxyScale::Quarter).expect("has video");
    // The fixture is too small for a scale to apply, so state one.
    settings.size = Size::new(80, 60);

    let mut job = ve_export::ProxyJob::start(
        vec![(asset.id, settings.clone())],
        Metrics::new(),
        proxies.clone(),
    );

    let mut built = None;
    let mut finished = false;
    let deadline = std::time::Instant::now() + Duration::from_secs(60);
    while !finished && std::time::Instant::now() < deadline {
        for event in job.poll() {
            match event {
                ve_export::ProxyEvent::Built { asset, media, report } => {
                    assert_eq!(asset, AssetId::from_raw(1));
                    assert_eq!(media.size, Size::new(80, 60));
                    assert_eq!(report.frames, 90);
                    built = Some(media);
                }
                ve_export::ProxyEvent::Failed { message, .. } => panic!("{message}"),
                ve_export::ProxyEvent::Finished | ve_export::ProxyEvent::Cancelled => {
                    finished = true
                }
                ve_export::ProxyEvent::Progress(p) => {
                    assert!((0.0..=1.0).contains(&p.fraction()));
                }
            }
        }
        std::thread::sleep(Duration::from_millis(10));
    }

    assert!(finished, "the batch never finished");
    let media = built.expect("the batch built nothing");

    // The directory was created by the job, and the model now points at a file
    // that is really there — which is what makes `picture_source` choose it.
    assert!(proxies.is_dir());
    asset.proxy = Some(media);
    let source = asset.picture_source(None, true);
    assert!(source.is_proxy, "the built proxy should be the one the editor decodes");
    assert!(source.path.exists());
}

#[test]
fn a_batch_carries_on_past_a_file_it_cannot_read() {
    let dir = tempfile::tempdir().unwrap();
    let proxies = dir.path().join("Proxies");
    let broken = dir.path().join("broken.mp4");
    std::fs::write(&broken, b"not a video file at all").unwrap();

    let bad = ProxySettings {
        source: broken,
        path: proxies.join("broken.proxy.mov"),
        size: Size::new(80, 60),
        rate: Rate::FPS_30,
        duration: Ticks::from_seconds(1),
        codec: ve_export::VideoCodec::H264,
        quality: ve_export::Quality::High,
    };
    let good = settings(
        "counter_30fps.mp4",
        &proxies.join("good.proxy.mov"),
        Size::new(80, 60),
        Rate::FPS_30,
    );

    let mut job = ve_export::ProxyJob::start(
        vec![(AssetId::from_raw(1), bad), (AssetId::from_raw(2), good)],
        Metrics::new(),
        proxies.clone(),
    );

    let (mut failed, mut built, mut finished) = (vec![], vec![], false);
    let deadline = std::time::Instant::now() + Duration::from_secs(60);
    while !finished && std::time::Instant::now() < deadline {
        for event in job.poll() {
            match event {
                ve_export::ProxyEvent::Failed { asset, .. } => failed.push(asset),
                ve_export::ProxyEvent::Built { asset, .. } => built.push(asset),
                ve_export::ProxyEvent::Finished | ve_export::ProxyEvent::Cancelled => {
                    finished = true
                }
                ve_export::ProxyEvent::Progress(_) => {}
            }
        }
        std::thread::sleep(Duration::from_millis(10));
    }

    assert!(finished);
    // A card with one unreadable clip on it should still get proxies for the
    // rest, rather than the batch stopping at the first bad file.
    assert_eq!(failed, vec![AssetId::from_raw(1)]);
    assert_eq!(built, vec![AssetId::from_raw(2)]);
}
