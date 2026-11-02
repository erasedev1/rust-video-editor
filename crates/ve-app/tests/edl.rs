//! EDL import and export through the action layer, headlessly.

use std::sync::Arc;

use ve_app::actions::{dispatch, Action};
use ve_app::state::EditorState;
use ve_core::{Clip, MediaInfo, Project};
use ve_engine::{ManualTime, PlaybackClock, PlaybackEngine};
use ve_media::{DecodeService, WaveformService};
use ve_metrics::Metrics;
use ve_time::Ticks;

struct Editor {
    state: EditorState,
    engine: PlaybackEngine,
    waveforms: WaveformService,
    scratch: tempfile::TempDir,
}

impl Editor {
    fn new() -> Self {
        let scratch = tempfile::tempdir().unwrap();
        let metrics = Metrics::new();
        let decode = Arc::new(DecodeService::new(64, metrics.clone()));
        let engine =
            PlaybackEngine::new(PlaybackClock::new(ManualTime::new()), decode, metrics.clone());
        let waveforms = WaveformService::new(4, metrics);
        let mut project = Project::with_default_sequence("EDL");
        project.add_asset(
            "/media/beach.mov",
            MediaInfo { duration: Ticks::from_secs_f64(60.0), ..Default::default() },
        );
        let state = EditorState::new(project, scratch.path().to_path_buf());
        Editor { state, engine, waveforms, scratch }
    }

    fn act(&mut self, action: Action) {
        dispatch(&mut self.state, &mut self.engine, &self.waveforms, action);
    }

    fn status(&self) -> String {
        self.state.status.as_ref().map(|s| s.text.clone()).unwrap_or_default()
    }
}

const LIST: &str = "TITLE: t\nFCM: NON-DROP FRAME\n\n\
    001  BEACH    V     C        00:00:00:00 00:00:02:00 00:00:00:00 00:00:02:00\n\
    * FROM CLIP NAME: beach.mov\n\
    002  BEACH    V     C        00:00:05:00 00:00:07:00 00:00:01:00 00:00:03:00\n\
    * FROM CLIP NAME: beach.mov\n\
    003  NOPE     V     C        00:00:00:00 00:00:01:00 00:00:03:00 00:00:04:00\n";

#[test]
fn import_lays_overlaps_on_extra_tracks_and_undoes_in_one_step() {
    let mut editor = Editor::new();
    let before = editor.state.active_sequence().unwrap().tracks.len();
    let path = editor.scratch.path().join("cut.edl");
    std::fs::write(&path, LIST).unwrap();

    editor.act(Action::ImportEdl(path));
    assert!(editor.status().contains("imported 2 clips"), "{}", editor.status());
    assert!(editor.status().contains("no matching media"), "{}", editor.status());

    let seq = editor.state.active_sequence().unwrap();
    // Events 1 and 2 overlap in record time, so they need two new lanes.
    assert_eq!(seq.tracks.len(), before + 2);
    let clips: usize = seq.tracks.iter().map(|t| t.len()).sum();
    assert_eq!(clips, 2);

    editor.act(Action::Undo);
    let seq = editor.state.active_sequence().unwrap();
    assert_eq!(seq.tracks.len(), before);
    assert_eq!(seq.tracks.iter().map(|t| t.len()).sum::<usize>(), 0);
}

#[test]
fn export_writes_a_list_that_imports_back() {
    let mut editor = Editor::new();
    let asset = editor.state.project.assets[0].id;
    let id = editor.state.project.new_clip_id();
    let rate = editor.state.active_sequence().unwrap().rate();
    let clip = Clip::new(
        id,
        asset,
        "beach",
        rate.frame_to_ticks(30),
        rate.frame_to_ticks(60),
        rate.frame_to_ticks(90),
    );
    editor.state.project.active_mut().unwrap().tracks[0].insert_clip(clip).unwrap();

    let path = editor.scratch.path().join("out.edl");
    editor.act(Action::ExportEdl(path.clone()));
    assert!(editor.status().contains("wrote 1 event"), "{}", editor.status());
    let text = std::fs::read_to_string(&path).unwrap();
    assert!(text.contains("00:00:01:00 00:00:04:00 00:00:02:00 00:00:05:00"), "{text}");

    editor.act(Action::ImportEdl(path));
    let seq = editor.state.active_sequence().unwrap();
    let imported = seq.tracks.last().unwrap().clips()[0].clone();
    assert_eq!(imported.timeline_start, rate.frame_to_ticks(60));
    assert_eq!(imported.source_in, rate.frame_to_ticks(30));
    assert_eq!(imported.duration, rate.frame_to_ticks(90));
}
