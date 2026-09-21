//! Export tests, which render and encode real files and then read them back.
//!
//! The fixtures make this possible: every frame of `counter_30fps.mp4` is a
//! solid colour encoding its own frame index, so decoding an exported file
//! answers *which* frame of the source ended up where, rather than merely that
//! something came out. A test that only checked "a file was written" would pass
//! just as happily for a file of thirty black frames.

use std::path::PathBuf;
use std::sync::Arc;

use ve_core::{Clip, ClipId, Project, Property, SequenceId, Size, TrackId};
use ve_export::{run, Cancel, ExportRange, ExportSettings, VideoCodec};
use ve_media::VideoDecoder;
use ve_metrics::Metrics;
use ve_render::GpuContext;
use ve_time::{Rate, Ticks, TimeRange};

fn testdata(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../testdata").join(name)
}

/// One GPU device for the whole test binary.
///
/// Creating one per test would be both slow and, on some drivers, a way to run
/// out of device handles; the renderer is stateless between exports.
fn gpu() -> &'static GpuContext {
    use std::sync::OnceLock;
    static GPU: OnceLock<GpuContext> = OnceLock::new();
    GPU.get_or_init(|| GpuContext::headless().expect("a GPU device for the tests"))
}

struct Fixture {
    project: Project,
    sequence: SequenceId,
    v1: TrackId,
    a1: TrackId,
    video: ve_core::AssetId,
    audio: ve_core::AssetId,
    dir: tempfile::TempDir,
}

/// A 160×120 sequence at 30 fps, which is the fixtures' own format: exporting
/// at the source's size and rate keeps the tests about the export rather than
/// about scaling.
fn fixture() -> Fixture {
    let mut project = Project::with_default_sequence("Export");
    let video = project.add_asset(
        testdata("counter_30fps.mp4"),
        ve_media::probe(testdata("counter_30fps.mp4")).unwrap(),
    );
    let audio = project.add_asset(
        testdata("tone_48k.wav"),
        ve_media::probe(testdata("tone_48k.wav")).unwrap(),
    );
    let sequence = project.active_sequence.unwrap();
    {
        let s = project.sequence_mut(sequence).unwrap();
        s.settings.resolution = Size::new(160, 120);
        s.settings.rate = Rate::FPS_30;
    }
    let s = project.sequence(sequence).unwrap();
    let (v1, a1) = (s.tracks[0].id, s.tracks[2].id);
    Fixture {
        project,
        sequence,
        v1,
        a1,
        video,
        audio,
        dir: tempfile::tempdir().expect("a temporary directory"),
    }
}

impl Fixture {
    fn add_video(&mut self, start: Ticks, duration: Ticks) -> ClipId {
        self.add_clip(self.v1, self.video, Ticks::ZERO, start, duration)
    }

    fn add_audio(&mut self, start: Ticks, duration: Ticks) -> ClipId {
        self.add_clip(self.a1, self.audio, Ticks::ZERO, start, duration)
    }

    fn add_clip(
        &mut self,
        track: TrackId,
        asset: ve_core::AssetId,
        source_in: Ticks,
        start: Ticks,
        duration: Ticks,
    ) -> ClipId {
        let id = self.project.new_clip_id();
        let clip = Clip::new(id, asset, "clip", source_in, start, duration);
        self.project
            .sequence_mut(self.sequence)
            .unwrap()
            .track_mut(track)
            .unwrap()
            .insert_clip(clip)
            .unwrap();
        id
    }

    fn path(&self, name: &str) -> PathBuf {
        self.dir.path().join(name)
    }

    fn settings(&self, name: &str) -> ExportSettings {
        let sequence = self.project.sequence(self.sequence).unwrap();
        ExportSettings::for_sequence(sequence, self.path(name)).without_audio()
    }

    /// Runs an export to completion, failing the test with the reason if it
    /// does not.
    #[track_caller]
    fn export(&self, settings: &ExportSettings) -> ve_export::ExportReport {
        run(
            &self.project,
            self.sequence,
            settings,
            gpu(),
            &Metrics::new(),
            &Cancel::new(),
            |_| {},
        )
        .expect("the export to succeed")
    }
}

/// Frames of `path`, as the colour at their centre.
fn decoded_colours(path: &PathBuf, count: usize) -> Vec<[u8; 3]> {
    let mut decoder = VideoDecoder::open(path).expect("the exported file to open");
    let rate = decoder.rate();
    (0..count)
        .map(|i| {
            let frame = decoder
                .frame_at(rate.frame_to_ticks(i as i64))
                .expect("decoding")
                .unwrap_or_else(|| panic!("frame {i} is missing from the exported file"));
            let x = frame.width() / 2;
            let y = frame.height() / 2;
            let row = frame.row(y);
            let i = (x * 4) as usize;
            [row[i], row[i + 1], row[i + 2]]
        })
        .collect()
}

/// The colour the fixture's frame `i` is painted, as `generate.py` writes it.
fn source_colour(i: usize) -> [u8; 3] {
    [((i % 10) * 25) as u8, ((i / 10) * 25) as u8, 200]
}

/// Whether two colours are the same picture once a lossy codec has been through
/// it twice.
///
/// Adjacent fixture frames differ by 25 levels in one channel, so a tolerance of
/// 12 still cannot confuse one frame with its neighbour — which is the whole
/// property these tests rely on.
#[track_caller]
fn assert_close(got: [u8; 3], want: [u8; 3], what: &str) {
    let close = got.iter().zip(&want).all(|(a, b)| (*a as i32 - *b as i32).abs() <= 12);
    assert!(close, "{what}: got {got:?}, expected about {want:?}");
}

#[test]
fn an_export_writes_a_file_with_the_shape_that_was_asked_for() {
    let mut f = fixture();
    f.add_video(Ticks::ZERO, Ticks::from_seconds(1));

    let settings = f.settings("shape.mp4");
    let report = f.export(&settings);

    assert_eq!(report.frames, 30, "one second at 30 fps");
    assert!(report.bytes > 0, "the file has contents");
    assert_eq!(report.missing_frames, 0, "every layer decoded");
    assert!(report.problems.is_empty(), "{:?}", report.problems);

    let info = ve_media::probe(&settings.path).expect("the exported file to probe");
    let video = info.video.expect("a video stream");
    assert_eq!(video.size, Size::new(160, 120));
    assert_eq!(video.rate, Rate::FPS_30);
    assert!(info.audio.is_none(), "no audio was asked for");
    // A second, to within the frame the container rounds to.
    let second = Ticks::from_seconds(1);
    assert!(
        (video.duration - second).abs() <= Rate::FPS_30.frame_duration(),
        "duration was {:?}",
        video.duration
    );
}

#[test]
fn every_exported_frame_is_the_frame_the_timeline_showed() {
    let mut f = fixture();
    f.add_video(Ticks::ZERO, Ticks::from_seconds(1));

    let settings = f.settings("frames.mp4");
    f.export(&settings);

    for (i, colour) in decoded_colours(&settings.path, 30).into_iter().enumerate() {
        assert_close(colour, source_colour(i), &format!("exported frame {i}"));
    }
}

#[test]
fn the_exported_range_is_the_one_that_was_asked_for() {
    let mut f = fixture();
    f.add_video(Ticks::ZERO, Ticks::from_seconds(3));

    // Frames 10 to 20 of the timeline, which is the same span of the source:
    // the clip starts at zero and reads from its own start.
    let frame = Rate::FPS_30.frame_duration();
    let mut settings = f.settings("range.mp4");
    settings.range = ExportRange::Span(TimeRange::from_bounds(frame * 10, frame * 20));
    let report = f.export(&settings);

    assert_eq!(report.frames, 10);
    let colours = decoded_colours(&settings.path, 10);
    assert_close(colours[0], source_colour(10), "the first exported frame");
    assert_close(colours[9], source_colour(19), "the last exported frame");
}

#[test]
fn a_work_area_is_what_an_active_range_export_writes() {
    let mut f = fixture();
    f.add_video(Ticks::ZERO, Ticks::from_seconds(3));
    let frame = Rate::FPS_30.frame_duration();
    f.project.sequence_mut(f.sequence).unwrap().work_area =
        Some(TimeRange::from_bounds(frame * 5, frame * 8));

    let mut settings = f.settings("work-area.mp4");
    settings.range = ExportRange::Active;
    let report = f.export(&settings);

    assert_eq!(report.frames, 3);
    assert_close(decoded_colours(&settings.path, 3)[0], source_colour(5), "frame 5");
}

#[test]
fn an_export_at_another_size_scales_the_same_picture() {
    let mut f = fixture();
    f.add_video(Ticks::ZERO, Ticks::from_seconds(1));

    let mut settings = f.settings("half.mp4");
    settings.resolution = Size::new(80, 60);
    f.export(&settings);

    let info = ve_media::probe(&settings.path).unwrap();
    assert_eq!(info.video.unwrap().size, Size::new(80, 60));
    // Scaling a flat colour leaves it alone, so the frames still identify
    // themselves — which is what says the picture was scaled rather than
    // cropped or re-timed.
    let colours = decoded_colours(&settings.path, 10);
    for (i, colour) in colours.into_iter().enumerate() {
        assert_close(colour, source_colour(i), &format!("scaled frame {i}"));
    }
}

#[test]
fn what_the_compositor_did_is_what_gets_encoded() {
    let mut f = fixture();
    let clip = f.add_video(Ticks::ZERO, Ticks::from_seconds(1));
    // Half opacity over the sequence's black background halves the picture.
    f.project
        .sequence_mut(f.sequence)
        .unwrap()
        .find_clip_mut(clip)
        .unwrap()
        .1
        .transform
        .opacity = Property::constant(0.5);

    let settings = f.settings("opacity.mp4");
    f.export(&settings);

    let colours = decoded_colours(&settings.path, 5);
    for (i, colour) in colours.into_iter().enumerate() {
        let full = source_colour(i);
        let half = [full[0] / 2, full[1] / 2, full[2] / 2];
        assert_close(colour, half, &format!("half-opacity frame {i}"));
    }
}

#[test]
fn an_export_carries_the_sound_as_well_as_the_picture() {
    let mut f = fixture();
    f.add_video(Ticks::ZERO, Ticks::from_seconds(1));
    f.add_audio(Ticks::ZERO, Ticks::from_seconds(1));

    let sequence = f.project.sequence(f.sequence).unwrap();
    let settings = ExportSettings::for_sequence(sequence, f.path("sound.mp4"));
    let report = f.export(&settings);

    assert_eq!(report.audio_encoder.as_deref(), Some("aac"));
    assert!(report.peak > 0.0, "the mix was not silent");
    assert_eq!(report.clipped_samples, 0, "a tone at unity does not clip");

    let info = ve_media::probe(&settings.path).expect("the exported file to probe");
    let audio = info.audio.expect("an audio stream");
    assert_eq!(audio.sample_rate.hz(), 48_000);
    assert_eq!(audio.channels, 2);
    // AAC pads the start of a stream, so this is about length rather than
    // sample-exact equality.
    let second = Ticks::from_seconds(1);
    assert!(
        (audio.duration - second).abs() < Ticks::from_millis(100),
        "audio duration was {:?}",
        audio.duration
    );
}

#[test]
fn sound_stays_with_the_picture_at_a_rate_that_does_not_divide_evenly() {
    let mut f = fixture();
    {
        let s = f.project.sequence_mut(f.sequence).unwrap();
        s.settings.rate = Rate::FPS_29_97;
    }
    f.add_video(Ticks::ZERO, Ticks::from_seconds(2));
    f.add_audio(Ticks::ZERO, Ticks::from_seconds(2));

    let sequence = f.project.sequence(f.sequence).unwrap();
    let settings = ExportSettings::for_sequence(sequence, f.path("ntsc.mp4"));
    let report = f.export(&settings);

    // Two seconds is 59.94 frames at this rate, and a range is snapped to whole
    // frames: the last one is 94% inside the range, so it is written.
    assert_eq!(report.frames, 60);

    let info = ve_media::probe(&settings.path).unwrap();
    let video = info.video.expect("a video stream");
    let audio = info.audio.expect("an audio stream");
    assert_eq!(video.rate, Rate::FPS_29_97);
    // The two streams cover the same span: if the audio block per frame were a
    // constant 1600 samples instead of the exact difference of sample indices,
    // the sound would run short by about two milliseconds a second.
    let drift = (audio.duration - video.duration).abs();
    assert!(drift < Ticks::from_millis(50), "streams drifted apart by {drift:?}");
}

#[test]
fn every_codec_the_dialogue_offers_writes_a_file_that_says_what_it_is() {
    // One frame each: this is about the encoder opening and the container
    // taking what it was handed, not about how long anything takes.
    let frame = Rate::FPS_30.frame_duration();
    for (codec, container, expected) in [
        (VideoCodec::H264, "mp4", "h264"),
        (VideoCodec::H265, "mkv", "hevc"),
        (VideoCodec::ProRes, "mov", "prores"),
    ] {
        let mut f = fixture();
        f.add_video(Ticks::ZERO, Ticks::from_seconds(1));
        let mut settings = f
            .settings(&format!("codec.{container}"))
            .with_video(codec)
            .with_range(ExportRange::Span(TimeRange::new(Ticks::ZERO, frame * 2)));
        settings.audio = None;

        let report = f.export(&settings);
        assert_eq!(report.frames, 2, "{}", codec.label());

        let info = ve_media::probe(&settings.path).expect("the exported file to probe");
        assert_eq!(info.video.expect("a video stream").codec, expected);
    }
}

#[test]
fn uncompressed_audio_is_refused_where_the_container_cannot_carry_it() {
    let mut f = fixture();
    f.add_video(Ticks::ZERO, Ticks::from_seconds(1));

    let sequence = f.project.sequence(f.sequence).unwrap();
    let mut settings = ExportSettings::for_sequence(sequence, f.path("pcm.mp4"));
    settings.audio = Some(ve_export::AudioSettings {
        codec: ve_export::AudioCodec::Pcm16,
        ..ve_export::AudioSettings::for_sequence(sequence)
    });

    let error =
        run(&f.project, f.sequence, &settings, gpu(), &Metrics::new(), &Cancel::new(), |_| {})
            .expect_err("MP4 does not carry PCM");
    assert!(error.to_string().contains("uncompressed"), "{error}");

    // ...and the same thing in a container that can carry it works.
    let settings = ExportSettings {
        path: f.path("pcm.mov"),
        range: ExportRange::Span(TimeRange::new(
            Ticks::ZERO,
            Rate::FPS_30.frame_duration() * 2,
        )),
        ..settings
    };
    let report = f.export(&settings);
    assert_eq!(report.audio_encoder.as_deref(), Some("pcm_s16le"));
    assert!(ve_media::probe(&settings.path).unwrap().audio.is_some());
}

#[test]
fn a_cancelled_export_leaves_nothing_behind() {
    let mut f = fixture();
    f.add_video(Ticks::ZERO, Ticks::from_seconds(3));

    let settings = f.settings("cancelled.mp4");
    let cancel = Cancel::new();
    cancel.cancel();

    let result =
        run(&f.project, f.sequence, &settings, gpu(), &Metrics::new(), &cancel, |_| {});
    let error = result.expect_err("a cancelled export does not report success");
    assert!(error.is_cancellation(), "{error}");
    assert!(!settings.path.exists(), "the half-written file was left behind");
}

#[test]
fn an_export_reports_its_progress_as_it_goes() {
    let mut f = fixture();
    f.add_video(Ticks::ZERO, Ticks::from_seconds(2));

    let settings = f.settings("progress.mp4");
    let mut seen: Vec<u64> = Vec::new();
    run(&f.project, f.sequence, &settings, gpu(), &Metrics::new(), &Cancel::new(), |p| {
        assert_eq!(p.frames_total, 60);
        seen.push(p.frames_done);
    })
    .unwrap();

    // However fast the render went, the last report is the finished one: a
    // progress bar must not stop at 59 of 60.
    assert_eq!(seen.last().copied(), Some(60));
    assert!(seen.windows(2).all(|w| w[0] <= w[1]), "progress went backwards: {seen:?}");
}

#[test]
fn an_empty_range_is_refused_rather_than_written() {
    let f = fixture();
    let settings = f.settings("empty.mp4");
    let error =
        run(&f.project, f.sequence, &settings, gpu(), &Metrics::new(), &Cancel::new(), |_| {})
            .expect_err("an empty sequence has nothing to export");
    assert!(!error.is_cancellation());
    assert!(!settings.path.exists(), "nothing was created for an export that cannot run");
}

#[test]
fn a_gap_in_the_timeline_is_exported_as_the_background() {
    let mut f = fixture();
    // Nothing until half a second in, so the first frames are the canvas.
    f.add_video(Ticks::from_millis(500), Ticks::from_millis(500));
    {
        let s = f.project.sequence_mut(f.sequence).unwrap();
        s.settings.background = ve_core::Rgba::new(0.0, 0.0, 0.0, 1.0);
    }

    let settings = f.settings("gap.mp4");
    let report = f.export(&settings);
    assert_eq!(report.frames, 30, "the gap is part of the sequence's length");

    let colours = decoded_colours(&settings.path, 30);
    assert_close(colours[0], [0, 0, 0], "the gap before the clip");
    assert_close(colours[20], source_colour(5), "the clip, six frames in");
}

#[test]
fn an_export_runs_on_its_own_thread_and_reports_when_it_is_done() {
    let mut f = fixture();
    f.add_video(Ticks::ZERO, Ticks::from_seconds(1));
    let settings = f.settings("threaded.mp4");

    let mut job = ve_export::ExportJob::start(
        Arc::new(f.project.clone()),
        f.sequence,
        settings.clone(),
        gpu().clone(),
        Metrics::new(),
    );

    let mut report = None;
    while job.is_running() {
        for event in job.poll() {
            match event {
                ve_export::ExportEvent::Finished(done) => report = Some(done),
                ve_export::ExportEvent::Failed(why) => panic!("the export failed: {why}"),
                ve_export::ExportEvent::Cancelled => panic!("nothing asked it to stop"),
                ve_export::ExportEvent::Progress(_) => {}
            }
        }
        std::thread::yield_now();
    }
    job.join();
    for event in job.poll() {
        if let ve_export::ExportEvent::Finished(done) = event {
            report = Some(done);
        }
    }

    let report = report.expect("the job to report what it did");
    assert_eq!(report.frames, 30);
    assert!(settings.path.exists());
}

#[test]
fn an_export_renders_the_original_even_when_the_editor_is_cutting_on_proxies() {
    let mut f = fixture();
    f.add_video(Ticks::ZERO, Ticks::from_seconds(3));

    // A stand-in that is really the 25 fps fixture. Both files paint frame *i*
    // the same colour, but they run at different rates, so the same instant is
    // a different frame index in each: a third of a second in is frame 10 of
    // the 30 fps original and frame 8 of this. The colours therefore say which
    // file was decoded, which a proxy that merely looked softer would not.
    f.project.asset_mut(f.video).unwrap().proxy =
        Some(ve_core::ProxyMedia::new(testdata("counter_25fps.mp4"), Size::new(160, 120)));
    // The editor is cutting on proxies, and says so in the project it hands
    // the exporter.
    f.project.settings.use_proxies = true;

    let frame = Rate::FPS_30.frame_duration();
    let mut settings = f.settings("not-the-proxy.mp4");
    settings.range = ExportRange::Span(TimeRange::from_bounds(frame * 10, frame * 20));
    f.export(&settings);

    let colours = decoded_colours(&settings.path, 10);
    for (i, colour) in colours.into_iter().enumerate() {
        // Frame 10 + i of the original. Had the proxy been used, this would be
        // frame 8 + something of the 25 fps file instead.
        assert_close(
            colour,
            source_colour(10 + i),
            &format!(
                "exported frame {i} came from the proxy — a delivery rendered from a \
                 stand-in is a soft file nobody asked for"
            ),
        );
    }
}

/// Puts one caption track on the fixture's sequence and returns its cues' text.
fn add_captions(f: &mut Fixture, language: &str, cues: &[(i64, i64, &str)]) {
    let track = f.project.new_caption_track_id();
    let sequence = f.project.sequence_mut(f.sequence).unwrap();
    sequence.add_caption_track(track);
    let captions = sequence.caption_track_mut(track).unwrap();
    captions.language = language.to_string();
    for (start_ms, length_ms, text) in cues {
        let id = ve_core::CueId::from_raw(1_000 + captions.len() as u64);
        captions
            .insert_cue(ve_core::Cue::new(
                id,
                Ticks::from_millis(*start_ms),
                Ticks::from_millis(*length_ms),
                *text,
            ))
            .unwrap();
    }
}

#[test]
fn an_export_writes_its_captions_beside_the_file() {
    let mut f = fixture();
    f.add_video(Ticks::ZERO, Ticks::from_seconds(2));
    add_captions(&mut f, "en", &[(0, 1_000, "Hello there."), (1_200, 700, "- Who's there?")]);

    let settings = f.settings("captioned.mp4");
    let report = f.export(&settings);

    assert_eq!(report.captions.len(), 1);
    let sidecar = &report.captions[0];
    assert_eq!(sidecar, &f.path("captioned.en.srt"));
    let text = std::fs::read_to_string(sidecar).unwrap();
    assert!(text.contains("00:00:00,000 --> 00:00:01,000"), "{text}");
    assert!(text.contains("Hello there."), "{text}");
    assert!(report.summary().contains("1 caption file"), "{}", report.summary());
}

#[test]
fn captions_can_be_left_out_of_a_delivery() {
    let mut f = fixture();
    f.add_video(Ticks::ZERO, Ticks::from_seconds(1));
    add_captions(&mut f, "en", &[(0, 500, "not wanted here")]);

    let settings = f.settings("plain.mp4").with_captions(None);
    let report = f.export(&settings);

    assert!(report.captions.is_empty());
    assert!(!f.path("plain.en.srt").exists());
}

#[test]
fn a_cancelled_export_leaves_no_caption_files_either() {
    let mut f = fixture();
    f.add_video(Ticks::ZERO, Ticks::from_seconds(3));
    add_captions(&mut f, "en", &[(0, 1_000, "orphan")]);

    let settings = f.settings("cancelled-captions.mp4");
    let cancel = Cancel::new();
    cancel.cancel();
    let result =
        run(&f.project, f.sequence, &settings, gpu(), &Metrics::new(), &cancel, |_| {});
    assert!(result.expect_err("cancelled").is_cancellation());

    // The sidecars are written after the trailer, so there is no window in
    // which they outlive the delivery they belong to.
    assert!(!f.path("cancelled-captions.en.srt").exists());
}

#[test]
fn exporting_part_of_a_sequence_re_times_its_captions_to_the_new_start() {
    let mut f = fixture();
    f.add_video(Ticks::ZERO, Ticks::from_seconds(4));
    add_captions(&mut f, "en", &[(0, 900, "before the in point"), (2_000, 1_000, "inside")]);

    let range = TimeRange::from_bounds(Ticks::from_seconds(1), Ticks::from_seconds(3));
    let settings = f.settings("excerpt.mp4").with_range(ExportRange::Span(range));
    let report = f.export(&settings);

    let text = std::fs::read_to_string(&report.captions[0]).unwrap();
    assert!(!text.contains("before the in point"), "{text}");
    // Two seconds into the sequence is one second into this delivery.
    assert!(text.contains("00:00:01,000 --> 00:00:02,000"), "{text}");
}
