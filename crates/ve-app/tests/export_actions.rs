//! Exporting through the action layer, with no window.
//!
//! The dialogue is a view of one value and the button is one action, so this
//! drives exactly what the interface drives: open, adjust, export, wait, and
//! read the status line that the user would have read.

use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

use ve_app::actions::{dispatch, Action};
use ve_app::state::EditorState;
use ve_core::{Project, TrackKind};
use ve_engine::{ManualTime, PlaybackClock, PlaybackEngine};
use ve_export::{ExportRange, ExportSettings, VideoCodec};
use ve_media::{DecodeService, WaveformService};
use ve_metrics::Metrics;
use ve_render::GpuContext;
use ve_time::Ticks;

fn testdata(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../testdata").join(name)
}

/// One device for the whole binary; creating one per test is slow and, on some
/// drivers, a way to run out of handles.
fn gpu() -> &'static GpuContext {
    use std::sync::OnceLock;
    static GPU: OnceLock<GpuContext> = OnceLock::new();
    GPU.get_or_init(|| GpuContext::headless().expect("a GPU device for the tests"))
}

struct Editor {
    state: EditorState,
    engine: PlaybackEngine,
    waveforms: WaveformService,
    out: tempfile::TempDir,
    _scratch: tempfile::TempDir,
}

impl Editor {
    fn new() -> Self {
        let scratch = tempfile::tempdir().unwrap();
        let metrics = Metrics::new();
        let decode = Arc::new(DecodeService::new(64, metrics.clone()));
        let engine =
            PlaybackEngine::new(PlaybackClock::new(ManualTime::new()), decode, metrics.clone());
        let waveforms = WaveformService::new(4, metrics.clone());
        let mut state = EditorState::new(
            Project::with_default_sequence("Untitled"),
            scratch.path().to_path_buf(),
        );
        // What the application shell does at startup: hand the export its
        // device. Everything else about exporting is the action layer.
        state.export.gpu = Some(gpu().clone());
        state.export.metrics = metrics;
        Editor {
            state,
            engine,
            waveforms,
            out: tempfile::tempdir().unwrap(),
            _scratch: scratch,
        }
    }

    fn act(&mut self, action: Action) {
        dispatch(&mut self.state, &mut self.engine, &self.waveforms, action);
    }

    fn status(&self) -> String {
        self.state.status.as_ref().map(|s| s.text.clone()).unwrap_or_default()
    }

    /// Imports the counter fixture and lays it on V1.
    fn with_video(&mut self) -> &mut Self {
        self.act(Action::ImportMedia(vec![testdata("counter_30fps.mp4")]));
        let asset = self.state.project.assets[0].id;
        let track = self
            .state
            .active_sequence()
            .unwrap()
            .tracks
            .iter()
            .find(|t| t.kind == TrackKind::Video)
            .unwrap()
            .id;
        self.act(Action::AddAssetToTimeline { asset, track, at: Ticks::ZERO });
        self
    }

    /// Settings for one second of picture: the fixture is three, and a test
    /// that encodes all of it is three times as slow for nothing.
    fn settings(&self, name: &str) -> ExportSettings {
        let sequence = self.state.active_sequence().unwrap();
        ExportSettings::for_sequence(sequence, self.out.path().join(name))
            .without_audio()
            .with_range(ExportRange::Span(ve_time::TimeRange::new(
                Ticks::ZERO,
                Ticks::from_seconds(1),
            )))
    }

    /// Runs the frame loop's export polling until the job is done.
    fn wait_for_export(&mut self) {
        let deadline = Instant::now() + Duration::from_secs(60);
        while self.state.export.job.is_some() {
            assert!(Instant::now() < deadline, "the export never finished");
            let events = self.state.export.job.as_mut().map(|j| j.poll()).unwrap_or_default();
            for event in events {
                match event {
                    ve_export::ExportEvent::Progress(_) => {}
                    ve_export::ExportEvent::Finished(report) => {
                        self.state.export.last = Some(report.summary());
                        self.state.export.job = None;
                    }
                    ve_export::ExportEvent::Failed(why) => {
                        panic!("the export failed: {why}")
                    }
                    ve_export::ExportEvent::Cancelled => {
                        self.state.export.last = Some("cancelled".into());
                        self.state.export.job = None;
                    }
                }
            }
            std::thread::yield_now();
        }
    }
}

#[test]
fn the_dialogue_opens_with_settings_derived_from_the_sequence() {
    let mut editor = Editor::new();
    editor.with_video();
    editor.act(Action::OpenExportDialog);

    assert!(editor.state.export.open);
    let settings = editor.state.export.settings.clone().expect("settings to edit");
    let sequence = editor.state.active_sequence().unwrap();
    assert_eq!(settings.resolution, sequence.settings.resolution);
    assert_eq!(settings.rate, sequence.rate());
    assert_eq!(settings.video, VideoCodec::H264);
    assert_eq!(settings.range, ExportRange::Active);
    // The sequence has sound, so the default carries it.
    assert!(settings.audio.is_some());
    assert_eq!(settings.path.extension().unwrap(), "mp4");
}

#[test]
fn a_dialogue_edit_is_remembered_for_the_next_export() {
    let mut editor = Editor::new();
    editor.with_video();
    editor.act(Action::OpenExportDialog);

    let mut settings = editor.state.export.settings.clone().unwrap();
    settings.quality = ve_export::Quality::Draft;
    editor.act(Action::SetExportSettings(Box::new(settings)));
    editor.act(Action::CloseExportDialog);

    assert!(!editor.state.export.open);
    editor.act(Action::OpenExportDialog);
    assert_eq!(editor.state.export.settings.unwrap().quality, ve_export::Quality::Draft);
}

#[test]
fn exporting_writes_a_file_and_says_what_it_wrote() {
    let mut editor = Editor::new();
    editor.with_video();

    let settings = editor.settings("from-actions.mp4");
    editor.act(Action::StartExport(Box::new(settings.clone())));
    assert!(editor.state.export.is_running(), "{}", editor.status());
    assert!(editor.status().contains("exporting"), "{}", editor.status());

    editor.wait_for_export();

    assert!(settings.path.exists(), "the file was not written");
    let summary = editor.state.export.last.clone().unwrap_or_default();
    assert!(summary.contains("30 frames"), "{summary}");

    let info = ve_media::probe(&settings.path).expect("the exported file to probe");
    assert_eq!(info.video.unwrap().size, editor.settings("x.mp4").resolution);
}

#[test]
fn the_editor_is_still_editable_while_an_export_runs() {
    let mut editor = Editor::new();
    editor.with_video();

    let settings = editor.settings("while-editing.mp4");
    editor.act(Action::StartExport(Box::new(settings.clone())));

    // The export holds a snapshot, so this edit belongs to the project and not
    // to the file being written.
    editor.act(Action::AddTrack(TrackKind::Video));
    let tracks = editor.state.active_sequence().unwrap().tracks.len();
    editor.act(Action::Undo);
    assert_eq!(editor.state.active_sequence().unwrap().tracks.len(), tracks - 1);

    editor.wait_for_export();
    assert!(settings.path.exists());
}

#[test]
fn a_second_export_is_refused_while_one_is_running() {
    let mut editor = Editor::new();
    editor.with_video();

    let settings = editor.settings("first.mp4");
    editor.act(Action::StartExport(Box::new(settings.clone())));
    editor.act(Action::StartExport(Box::new(editor.settings("second.mp4"))));
    assert!(editor.status().contains("already running"), "{}", editor.status());

    editor.wait_for_export();
    assert!(settings.path.exists());
    assert!(!editor.out.path().join("second.mp4").exists());
}

#[test]
fn a_cancelled_export_says_so_and_leaves_no_file() {
    let mut editor = Editor::new();
    editor.with_video();

    let settings = editor.settings("cancelled.mp4");
    editor.act(Action::StartExport(Box::new(settings.clone())));
    editor.act(Action::CancelExport);
    assert!(editor.status().contains("stopping"), "{}", editor.status());

    editor.wait_for_export();
    assert_eq!(editor.state.export.last.as_deref(), Some("cancelled"));
    assert!(!settings.path.exists(), "a half-written file was left behind");
}

#[test]
fn an_export_to_something_that_is_not_a_container_is_refused_before_it_starts() {
    let mut editor = Editor::new();
    editor.with_video();

    let mut settings = editor.settings("out.verge");
    settings.path.set_extension("verge");
    editor.act(Action::StartExport(Box::new(settings.clone())));

    assert!(!editor.state.export.is_running());
    assert!(editor.status().contains("not a container"), "{}", editor.status());
    assert!(!settings.path.exists());
}

#[test]
fn exporting_without_a_gpu_says_why_rather_than_failing_later() {
    let mut editor = Editor::new();
    editor.with_video();
    editor.state.export.gpu = None;

    let settings = editor.settings("no-gpu.mp4");
    editor.act(Action::StartExport(Box::new(settings.clone())));

    assert!(!editor.state.export.is_running());
    assert!(editor.status().contains("no GPU"), "{}", editor.status());
    assert!(!settings.path.exists());
}
