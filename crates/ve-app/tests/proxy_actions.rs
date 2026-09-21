//! Proxies through the action layer, with no window.
//!
//! Building proxies is a background job, so these drive the same two calls the
//! shell makes — `dispatch` for what the user asked for, `poll_proxies` once a
//! frame for what the job has to say — and then assert on the project and the
//! status line the user would have read.

use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

use ve_app::actions::{dispatch, poll_proxies, Action};
use ve_app::state::EditorState;
use ve_core::{Project, Size};
use ve_engine::{ManualTime, PlaybackClock, PlaybackEngine};
use ve_export::ProxyScale;
use ve_media::{DecodeService, WaveformService};
use ve_metrics::Metrics;
use ve_time::Rate;

fn testdata(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../testdata").join(name)
}

struct Editor {
    state: EditorState,
    engine: PlaybackEngine,
    waveforms: WaveformService,
    dir: tempfile::TempDir,
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
        let state = EditorState::new(
            Project::with_default_sequence("Untitled"),
            scratch.path().to_path_buf(),
        );
        Editor {
            state,
            engine,
            waveforms,
            dir: tempfile::tempdir().unwrap(),
            _scratch: scratch,
        }
    }

    fn act(&mut self, action: Action) {
        dispatch(&mut self.state, &mut self.engine, &self.waveforms, action);
    }

    fn status(&self) -> String {
        self.state.status.as_ref().map(|s| s.text.clone()).unwrap_or_default()
    }

    /// Imports the counter fixture and saves the project, which is what gives
    /// proxies somewhere to live.
    fn with_saved_project(&mut self) -> &mut Self {
        self.act(Action::ImportMedia(vec![testdata("counter_30fps.mp4")]));
        let path = self.dir.path().join("cut.verge");
        self.act(Action::SaveProjectAs(path));
        self
    }

    /// Pumps the frame loop until the build finishes, as the shell would.
    fn settle(&mut self) {
        let deadline = Instant::now() + Duration::from_secs(90);
        while self.state.proxies.job.is_some() && Instant::now() < deadline {
            poll_proxies(&mut self.state, &mut self.engine);
            std::thread::sleep(Duration::from_millis(10));
        }
        poll_proxies(&mut self.state, &mut self.engine);
        assert!(self.state.proxies.job.is_none(), "the build never finished");
    }

    fn proxy_dir(&self) -> PathBuf {
        self.dir.path().join("Proxies")
    }
}

#[test]
fn building_proxies_attaches_them_and_leaves_the_editor_at_full_resolution() {
    let mut e = Editor::new();
    e.with_saved_project();
    let asset = e.state.project.assets[0].id;

    e.act(Action::BuildProxies);
    assert!(e.state.proxies.is_running(), "the build should start in the background");
    e.settle();

    let built = e.state.project.asset(asset).unwrap();
    assert!(built.has_proxy(), "{}", e.status());
    assert!(built.picture_source(None, true).is_proxy, "the file should really be there");
    assert!(e.proxy_dir().is_dir(), "proxies go beside the project file");

    // Building is not switching: the user still sees the real picture until
    // they ask for the stand-ins.
    assert!(!e.state.project.settings.use_proxies);
    assert_eq!(e.engine.decode_service().asset_rate(asset), Some(Rate::FPS_30));

    // And the reference is an edit, so it is in the history and will be saved.
    assert!(e.state.history.is_dirty());
    assert_eq!(e.state.history.undo_name(), Some("Attach Proxy"));
}

#[test]
fn switching_proxies_on_reopens_the_decoders_on_them() {
    let mut e = Editor::new();
    e.act(Action::ImportMedia(vec![testdata("counter_30fps.mp4")]));
    let asset = e.state.project.assets[0].id;

    // A stand-in that is really the 25 fps fixture, so the rate a worker
    // reports says which file it opened. Attached directly because what is
    // under test is the switch, not the build — and because the fixtures are
    // too small for a real proxy to differ from its original in size.
    e.state.project.asset_mut(asset).unwrap().proxy =
        Some(ve_core::ProxyMedia::new(testdata("counter_25fps.mp4"), Size::new(160, 120)));
    e.engine.open_project_assets(&e.state.project);
    assert_eq!(e.engine.decode_service().asset_rate(asset), Some(Rate::FPS_30));

    e.act(Action::SetUseProxies(true));
    assert!(e.state.project.settings.use_proxies);
    assert_eq!(e.status(), "cutting on proxies");
    assert_eq!(
        e.engine.decode_service().asset_rate(asset),
        Some(Rate::FPS_25),
        "the switch should have reopened the worker on the proxy"
    );

    // And back, because seeing the real picture is the point of it being a
    // switch rather than a consequence of a proxy existing.
    e.act(Action::SetUseProxies(false));
    assert!(!e.state.project.settings.use_proxies);
    assert_eq!(e.status(), "cutting at full resolution");
    assert_eq!(e.engine.decode_service().asset_rate(asset), Some(Rate::FPS_30));

    // Undo reaches it too: it went through the history like every other edit.
    e.act(Action::Undo);
    assert!(e.state.project.settings.use_proxies);
}

#[test]
fn a_build_that_finishes_while_proxies_are_on_is_picked_up_at_once() {
    let mut e = Editor::new();
    e.with_saved_project();
    let asset = e.state.project.assets[0].id;

    // Switched on before anything has been built, using a stand-in so the
    // switch is legal and observable, then built for real.
    e.state.project.asset_mut(asset).unwrap().proxy =
        Some(ve_core::ProxyMedia::new(testdata("counter_25fps.mp4"), Size::new(160, 120)));
    e.act(Action::SetUseProxies(true));
    assert_eq!(e.engine.decode_service().asset_rate(asset), Some(Rate::FPS_25));

    e.act(Action::RebuildProxies);
    e.settle();

    // The real proxy is a 30 fps copy of the 30 fps original, so the worker
    // reporting 30 again means it was reopened on what was just built rather
    // than still reading the stand-in.
    assert_eq!(
        e.engine.decode_service().asset_rate(asset),
        Some(Rate::FPS_30),
        "a proxy built while proxies are on should be picked up without a toggle"
    );
    let proxy = e.state.project.asset(asset).unwrap().proxy.as_ref().unwrap();
    assert!(proxy.path.starts_with(e.proxy_dir()));
}

#[test]
fn a_second_build_skips_what_already_has_a_proxy() {
    let mut e = Editor::new();
    e.with_saved_project();
    e.act(Action::BuildProxies);
    e.settle();

    e.act(Action::BuildProxies);
    assert!(!e.state.proxies.is_running(), "there was nothing left to build");
    assert_eq!(e.status(), "every clip already has a proxy");

    // Asking for a rebuild does the work again, which is what footage replaced
    // on disk needs.
    e.act(Action::RebuildProxies);
    assert!(e.state.proxies.is_running());
    e.settle();
}

#[test]
fn a_build_needs_somewhere_to_put_them() {
    let mut e = Editor::new();
    e.act(Action::ImportMedia(vec![testdata("counter_30fps.mp4")]));
    // Deliberately not saved.
    e.act(Action::BuildProxies);

    assert!(!e.state.proxies.is_running());
    assert!(
        e.status().contains("save the project first"),
        "a build with nowhere to live should say so rather than writing orphans: {}",
        e.status()
    );
}

#[test]
fn forgetting_proxies_leaves_the_files_alone() {
    let mut e = Editor::new();
    e.with_saved_project();
    let asset = e.state.project.assets[0].id;
    e.act(Action::BuildProxies);
    e.settle();
    let path = e.state.project.asset(asset).unwrap().proxy.as_ref().unwrap().path.clone();

    e.act(Action::ForgetProxies);
    assert!(!e.state.project.asset(asset).unwrap().has_proxy());
    assert!(path.exists(), "forgetting a proxy must not delete it");

    // One undo step for the lot, and it brings them back.
    e.act(Action::Undo);
    assert!(e.state.project.asset(asset).unwrap().has_proxy());
}

#[test]
fn the_size_can_be_chosen_before_a_build_but_not_during_one() {
    let mut e = Editor::new();
    assert_eq!(e.state.proxies.scale, ProxyScale::Quarter, "a sensible default");

    e.act(Action::SetProxyScale(ProxyScale::Half));
    assert_eq!(e.state.proxies.scale, ProxyScale::Half);

    e.with_saved_project();
    e.act(Action::BuildProxies);
    e.act(Action::SetProxyScale(ProxyScale::Eighth));
    assert_eq!(
        e.state.proxies.scale,
        ProxyScale::Half,
        "changing the size under a running build would apply to only some files"
    );
    assert!(e.status().contains("already running"));
    e.settle();
}

#[test]
fn a_proxy_that_has_gone_is_built_again_by_an_ordinary_build() {
    let mut e = Editor::new();
    e.with_saved_project();
    let asset = e.state.project.assets[0].id;
    e.act(Action::BuildProxies);
    e.settle();

    // The user cleared their proxy folder.
    let path = e.state.project.asset(asset).unwrap().proxy.as_ref().unwrap().path.clone();
    std::fs::remove_file(&path).unwrap();

    e.act(Action::BuildProxies);
    assert!(
        e.state.proxies.is_running(),
        "a reference to a file that is gone is exactly what a build is for: {}",
        e.status()
    );
    e.settle();
    assert!(e.state.project.asset(asset).unwrap().picture_source(None, true).is_proxy);
}

#[test]
fn an_audio_only_file_is_not_given_a_proxy() {
    let mut e = Editor::new();
    e.act(Action::ImportMedia(vec![testdata("tone_48k.wav")]));
    e.act(Action::SaveProjectAs(e.dir.path().join("sound.verge")));

    e.act(Action::BuildProxies);
    assert!(!e.state.proxies.is_running());
    assert!(
        e.status().contains("no media to build proxies for")
            || e.status().contains("already has a proxy"),
        "{}",
        e.status()
    );
    assert!(!e.state.project.assets[0].has_proxy());
}

#[test]
fn a_built_proxy_survives_saving_and_reopening_the_project() {
    let mut e = Editor::new();
    e.with_saved_project();
    let path = e.state.path.clone().unwrap();
    e.act(Action::BuildProxies);
    e.settle();
    e.act(Action::SetUseProxies(true));
    e.act(Action::SaveProject);

    let mut reopened = Editor::new();
    reopened.act(Action::OpenProject(path));

    let asset = &reopened.state.project.assets[0];
    assert!(reopened.state.project.settings.use_proxies, "the switch is part of the project");
    assert!(asset.has_proxy());
    assert!(
        asset.picture_source(None, true).is_proxy,
        "a reopened project should find the proxies it built"
    );
    assert_eq!(asset.proxy.as_ref().unwrap().size, Size::new(160, 120));
}
