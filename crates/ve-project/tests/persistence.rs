//! Save/load round trips, atomic-write durability, backup fallback, and the
//! handling of corrupt or hostile project files.

use std::fs;
use std::path::Path;
use std::time::Duration;

use ve_core::registry::kinds;
use ve_core::{
    builtin_registry, BlendMode, Clip, CompositionLayer, CompositionSettings, Effect,
    FadeCurve, Interpolation, MediaInfo, ParamValue, Project, Size, Source, VideoStreamInfo,
};
use ve_project::{autosave, store, Autosave, ProjectError, FORMAT_MAGIC, FORMAT_VERSION};
use ve_time::{Rate, Ticks};

fn sample_project(media: &Path) -> Project {
    let mut p = Project::with_default_sequence("Round Trip");
    let asset = p.add_asset(
        media,
        MediaInfo {
            duration: Ticks::from_seconds(30),
            video: Some(VideoStreamInfo {
                size: Size::new(1920, 1080),
                rate: Rate::FPS_23_976,
                duration: Ticks::from_seconds(30),
                frame_count: Some(719),
                codec: "h264".into(),
                pixel_format: "yuv420p".into(),
                sample_aspect_ratio: (1, 1),
            }),
            audio: None,
            container: "mp4".into(),
        },
    );
    let seq = p.active_sequence.unwrap();
    let track = p.sequence(seq).unwrap().tracks[0].id;

    let clip_id = p.new_clip_id();
    let mut clip = Clip::new(
        clip_id,
        asset,
        "shot_01",
        Ticks::from_seconds(5),
        Ticks::ZERO,
        Ticks::from_seconds(10),
    );
    clip.blend = BlendMode::Screen;
    clip.transform.opacity.set_keyframe(Ticks::ZERO, 0.0, Interpolation::EaseInOut);
    clip.transform.opacity.set_keyframe(Ticks::from_seconds(1), 1.0, Interpolation::Linear);
    p.sequence_mut(seq).unwrap().track_mut(track).unwrap().insert_clip(clip).unwrap();
    p.sequence_mut(seq).unwrap().set_playhead(Ticks::from_seconds(3));
    p
}

/// [`sample_project`] plus a composition holding media, nested inside another
/// composition, with the outer one cut into the sequence. Everything the format
/// has to carry for compositing, in one fixture.
fn project_with_compositions(media: &Path) -> Project {
    let mut p = sample_project(media);
    let asset = p.assets[0].id;
    let seq = p.active_sequence.unwrap();
    let track = p.sequence(seq).unwrap().tracks[0].id;

    let inner = p.add_composition("Lower Third", CompositionSettings::default());
    let layer_id = p.new_layer_id();
    p.composition_mut(inner).unwrap().push_layer(CompositionLayer::new(
        layer_id,
        "plate",
        asset,
        Ticks::ZERO,
        Ticks::from_seconds(4),
    ));

    let outer = p.add_composition("Titles", CompositionSettings::default());
    let nested_layer = p.new_layer_id();
    let mut nested = CompositionLayer::new(
        nested_layer,
        "lower third",
        inner,
        Ticks::from_seconds(1),
        Ticks::from_seconds(3),
    );
    nested.blend = BlendMode::Screen;
    p.composition_mut(outer).unwrap().push_layer(nested);

    let comp_clip_id = p.new_clip_id();
    let comp_clip = Clip::new(
        comp_clip_id,
        outer,
        "titles",
        Ticks::ZERO,
        Ticks::from_seconds(12),
        Ticks::from_seconds(3),
    );
    p.sequence_mut(seq).unwrap().track_mut(track).unwrap().insert_clip(comp_clip).unwrap();
    p
}

#[test]
fn a_project_survives_a_save_and_reopen_unchanged() {
    let dir = tempfile::tempdir().unwrap();
    let media = dir.path().join("movie.mp4");
    fs::write(&media, b"not really a movie").unwrap();
    let path = dir.path().join("test.verge");

    let original = sample_project(&media);
    store::save(&original, &path).unwrap();
    let loaded = store::load(&path).unwrap();

    assert!(loaded.warnings.is_empty(), "{:?}", loaded.warnings);
    assert_eq!(loaded.migrated_from, None);
    assert!(!loaded.recovered_from_backup);

    // The edit model is identical apart from the relative path the save
    // recorded, which is the point of relinking.
    assert_eq!(loaded.project.name, original.name);
    assert_eq!(loaded.project.clip_count(), original.clip_count());
    assert_eq!(loaded.project.ids.peek(), original.ids.peek());

    let seq = loaded.project.active().unwrap();
    assert_eq!(seq.playhead, Ticks::from_seconds(3));
    assert_eq!(seq.settings.rate, original.active().unwrap().settings.rate);

    let clip = &seq.tracks[0].clips()[0];
    assert_eq!(clip.source_in, Ticks::from_seconds(5));
    assert_eq!(clip.duration, Ticks::from_seconds(10));
    assert_eq!(clip.transform.opacity.keyframes().len(), 2);
    assert_eq!(clip.transform.opacity.keyframes()[0].interpolation, Interpolation::EaseInOut);
}

#[test]
fn the_file_is_human_readable_and_self_describing() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("readable.verge");
    store::save(&Project::with_default_sequence("Readable"), &path).unwrap();
    let text = fs::read_to_string(&path).unwrap();

    assert!(text.contains(FORMAT_MAGIC));
    assert!(text.contains(&format!("\"version\": {FORMAT_VERSION}")));
    assert!(text.contains("\"saved_at\""));
    assert!(text.contains('\n'), "the format is pretty-printed, not minified");

    // The envelope must be readable without the typed model.
    let doc: serde_json::Value = serde_json::from_str(&text).unwrap();
    assert_eq!(doc["format"], FORMAT_MAGIC);
    assert_eq!(doc["version"], FORMAT_VERSION);
}

#[test]
fn media_is_referenced_rather_than_embedded() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("refs.verge");

    // Save the same project against a small and then a large media file. If any
    // media were being embedded, the project file would grow with it.
    let small = dir.path().join("small.mp4");
    fs::write(&small, vec![0xABu8; 1024]).unwrap();
    store::save(&sample_project(&small), &path).unwrap();
    let small_len = fs::metadata(&path).unwrap().len();

    let large = dir.path().join("small.mp4");
    fs::write(&large, vec![0xABu8; 8 * 1024 * 1024]).unwrap();
    store::save(&sample_project(&large), &path).unwrap();
    let large_len = fs::metadata(&path).unwrap().len();

    assert_eq!(
        small_len, large_len,
        "project size tracked media size, so media is being embedded"
    );

    // The project records a path, and none of the media's bytes.
    let bytes = fs::read(&path).unwrap();
    assert!(!bytes.windows(64).any(|w| w.iter().all(|&b| b == 0xAB)));
    assert!(String::from_utf8_lossy(&bytes).contains("small.mp4"));
}

#[test]
fn a_moved_project_folder_relinks_its_media() {
    let dir = tempfile::tempdir().unwrap();
    let original_dir = dir.path().join("original");
    fs::create_dir_all(&original_dir).unwrap();
    let media = original_dir.join("movie.mp4");
    fs::write(&media, b"media").unwrap();
    let path = original_dir.join("p.verge");
    store::save(&sample_project(&media), &path).unwrap();

    // Move the whole folder, as a user copying a project to another machine
    // would. The absolute path recorded in the file is now wrong.
    let moved_dir = dir.path().join("moved");
    fs::rename(&original_dir, &moved_dir).unwrap();

    let loaded = store::load(&moved_dir.join("p.verge")).unwrap();
    let asset = &loaded.project.assets[0];
    assert!(!asset.offline, "media should have been found via its relative path");
    assert_eq!(asset.path, moved_dir.join("movie.mp4"));
    assert!(loaded.warnings.is_empty(), "{:?}", loaded.warnings);
}

#[test]
fn missing_media_opens_the_project_and_flags_the_asset_offline() {
    let dir = tempfile::tempdir().unwrap();
    let media = dir.path().join("gone.mp4");
    fs::write(&media, b"media").unwrap();
    let path = dir.path().join("p.verge");
    store::save(&sample_project(&media), &path).unwrap();
    fs::remove_file(&media).unwrap();

    let loaded = store::load(&path).expect("a missing media file must not fail the open");
    assert!(loaded.project.assets[0].offline);
    assert_eq!(loaded.project.clip_count(), 1, "clips referencing it are kept");
    assert!(loaded.warnings.iter().any(|w| w.contains("offline")), "{:?}", loaded.warnings);
}

#[test]
fn saving_rotates_the_previous_version_to_a_backup() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("rot.verge");
    let bak = store::backup_path(&path);

    store::save(&Project::with_default_sequence("First"), &path).unwrap();
    assert!(!bak.exists(), "the first save has nothing to back up");

    store::save(&Project::with_default_sequence("Second"), &path).unwrap();
    assert!(bak.exists());
    assert_eq!(store::load(&path).unwrap().project.name, "Second");
    assert_eq!(store::load(&bak).unwrap().project.name, "First");
}

#[test]
fn a_save_leaves_no_temporary_files_behind() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("clean.verge");
    for _ in 0..3 {
        store::save(&Project::with_default_sequence("Clean"), &path).unwrap();
    }
    let leftovers: Vec<_> = fs::read_dir(dir.path())
        .unwrap()
        .filter_map(Result::ok)
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .filter(|n| n.contains(".tmp"))
        .collect();
    assert!(leftovers.is_empty(), "stray temporaries: {leftovers:?}");
}

#[test]
fn a_truncated_project_file_falls_back_to_the_backup() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("crash.verge");

    store::save(&Project::with_default_sequence("Good"), &path).unwrap();
    store::save(&Project::with_default_sequence("AlsoGood"), &path).unwrap();

    // Simulate the file being damaged after the fact (a bad disk, a killed
    // third-party tool): the atomic write itself cannot produce this, but the
    // reader must survive it.
    let text = fs::read_to_string(&path).unwrap();
    fs::write(&path, &text[..text.len() / 2]).unwrap();

    let loaded = store::load(&path).expect("must recover rather than fail");
    assert!(loaded.recovered_from_backup);
    assert_eq!(loaded.project.name, "Good");
    assert!(loaded.warnings.iter().any(|w| w.contains("recovered from backup")));
}

#[test]
fn a_project_missing_entirely_falls_back_to_the_backup() {
    // This is the one window the atomic save leaves open: a crash between
    // rotating the old file aside and renaming the new one into place.
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("window.verge");
    store::save(&Project::with_default_sequence("Only"), &path).unwrap();
    fs::rename(&path, store::backup_path(&path)).unwrap();

    let loaded = store::load(&path).unwrap();
    assert!(loaded.recovered_from_backup);
    assert_eq!(loaded.project.name, "Only");
}

#[test]
fn with_no_backup_a_corrupt_file_reports_a_clear_error() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("bad.verge");
    fs::write(&path, b"{ this is not json").unwrap();
    assert!(matches!(store::load(&path), Err(ProjectError::Json(_))));

    let missing = dir.path().join("nope.verge");
    assert!(matches!(store::load(&missing), Err(ProjectError::Io { .. })));
}

#[test]
fn an_unrelated_json_file_is_rejected_by_name() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("other.verge");
    fs::write(&path, br#"{"format":"some-other-app","version":1}"#).unwrap();
    match store::load(&path) {
        Err(ProjectError::NotAProjectFile(what)) => assert_eq!(what, "some-other-app"),
        other => panic!("expected NotAProjectFile, got {other:?}"),
    }
}

#[test]
fn a_file_from_a_future_version_is_refused_with_a_useful_message() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("future.verge");
    let mut doc: serde_json::Value =
        serde_json::from_str(&store::to_json(&Project::with_default_sequence("F")).unwrap())
            .unwrap();
    doc["version"] = serde_json::Value::from(FORMAT_VERSION + 10);
    fs::write(&path, doc.to_string()).unwrap();

    let err = store::load(&path).unwrap_err();
    let msg = err.to_string();
    assert!(msg.contains("newer version"), "{msg}");
    assert!(msg.contains(&FORMAT_VERSION.to_string()), "{msg}");
}

#[test]
fn a_hand_edited_file_with_overlapping_clips_opens_with_warnings() {
    let dir = tempfile::tempdir().unwrap();
    let media = dir.path().join("m.mp4");
    fs::write(&media, b"m").unwrap();
    let path = dir.path().join("overlap.verge");
    store::save(&sample_project(&media), &path).unwrap();

    // Duplicate the clip at an overlapping position, as a careless hand-edit
    // or a buggy third-party tool might.
    let mut doc: serde_json::Value =
        serde_json::from_str(&fs::read_to_string(&path).unwrap()).unwrap();
    let clips = doc["project"]["sequences"][0]["tracks"][0]["clips"].as_array_mut().unwrap();
    let mut dup = clips[0].clone();
    dup["id"] = serde_json::Value::from(9_000);
    dup["timeline_start"] = clips[0]["timeline_start"].clone();
    clips.push(dup);
    fs::write(&path, doc.to_string()).unwrap();

    let loaded = store::load(&path).expect("must open despite the damage");
    assert!(loaded.warnings.iter().any(|w| w.contains("overlaps")), "{:?}", loaded.warnings);
    // Nothing is discarded: the user's data is still there to be fixed.
    assert_eq!(loaded.project.clip_count(), 2);
}

#[test]
fn an_effect_chain_survives_a_round_trip_with_its_keyframes() {
    let dir = tempfile::tempdir().unwrap();
    let media = dir.path().join("m.mp4");
    fs::write(&media, b"m").unwrap();
    let path = dir.path().join("effects.verge");

    let mut project = sample_project(&media);
    let seq = project.active_sequence.unwrap();
    let clip_id = project.sequence(seq).unwrap().tracks[0].clips()[0].id;
    let blur_id = project.new_effect_id();
    let colour_id = project.new_effect_id();
    let registry = builtin_registry();
    {
        let clip = project.sequence_mut(seq).unwrap().find_clip_mut(clip_id).unwrap().1;
        let mut blur = registry.instantiate(kinds::GAUSSIAN_BLUR, blur_id).unwrap();
        if let Some(ParamValue::Scalar(radius)) = blur.param_mut("radius") {
            radius.set_keyframe(Ticks::ZERO, 0.0, Interpolation::EaseInOut);
            radius.set_keyframe(Ticks::from_seconds(2), 60.0, Interpolation::Linear);
        }
        let mut colour = registry.instantiate(kinds::COLOR_ADJUST, colour_id).unwrap();
        colour.enabled = false;
        clip.effects = vec![blur, colour];
    }
    store::save(&project, &path).unwrap();

    let loaded = store::load(&path).unwrap().project;
    let clip = loaded.sequence(seq).unwrap().tracks[0].clips()[0].clone();
    assert_eq!(clip.effects.len(), 2);
    // Order is the chain, so it has to survive exactly.
    assert_eq!(clip.effects[0].kind, kinds::GAUSSIAN_BLUR);
    assert_eq!(clip.effects[0].id, blur_id);
    assert!(clip.effects[0].is_animated());
    assert_eq!(
        clip.effects[0].param("radius").unwrap().as_scalar_at(Ticks::from_seconds(2)),
        Some(60.0)
    );
    assert_eq!(clip.effects[1].kind, kinds::COLOR_ADJUST);
    assert!(!clip.effects[1].enabled, "a switched-off effect stays switched off");
}

#[test]
fn a_hand_edited_effect_parameter_is_conformed_on_load() {
    let dir = tempfile::tempdir().unwrap();
    let media = dir.path().join("m.mp4");
    fs::write(&media, b"m").unwrap();
    let path = dir.path().join("conform.verge");

    let mut project = sample_project(&media);
    let seq = project.active_sequence.unwrap();
    let clip_id = project.sequence(seq).unwrap().tracks[0].clips()[0].id;
    let id = project.new_effect_id();
    {
        let clip = project.sequence_mut(seq).unwrap().find_clip_mut(clip_id).unwrap().1;
        // A blur whose radius arrived as a switch, and which is missing the
        // direction entirely — neither is reachable through the editor, and
        // both are one text editor away.
        clip.effects = vec![Effect::new(id, kinds::GAUSSIAN_BLUR, "Gaussian Blur")
            .with_param("radius", ParamValue::Bool(true))];
    }
    store::save(&project, &path).unwrap();

    let loaded = store::load(&path).unwrap().project;
    let effect = loaded.sequence(seq).unwrap().tracks[0].clips()[0].effects[0].clone();
    assert_eq!(effect.param("radius").unwrap().as_scalar_at(Ticks::ZERO), Some(8.0));
    assert!(effect.param("direction").is_some(), "a missing parameter is filled in");
}

#[test]
fn an_effect_from_a_plugin_this_build_lacks_survives_a_round_trip() {
    let dir = tempfile::tempdir().unwrap();
    let media = dir.path().join("m.mp4");
    fs::write(&media, b"m").unwrap();
    let path = dir.path().join("plugin.verge");

    let mut project = sample_project(&media);
    let seq = project.active_sequence.unwrap();
    let clip_id = project.sequence(seq).unwrap().tracks[0].clips()[0].id;
    let id = project.new_effect_id();
    {
        let clip = project.sequence_mut(seq).unwrap().find_clip_mut(clip_id).unwrap().1;
        clip.effects = vec![Effect::new(id, "someone.elses.glow", "Glow")
            .with_param("intensity", ParamValue::scalar(3.0))];
    }
    store::save(&project, &path).unwrap();

    let loaded = store::load(&path).unwrap();
    let effect = loaded.project.sequence(seq).unwrap().tracks[0].clips()[0].effects[0].clone();
    assert_eq!(effect.kind, "someone.elses.glow");
    assert_eq!(effect.param("intensity").unwrap().as_scalar_at(Ticks::ZERO), Some(3.0));
    assert!(loaded.warnings.is_empty(), "{:?}", loaded.warnings);
}

#[test]
fn unknown_fields_from_a_newer_build_are_tolerated() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("extra.verge");
    let mut doc: serde_json::Value =
        serde_json::from_str(&store::to_json(&Project::with_default_sequence("E")).unwrap())
            .unwrap();
    doc["project"]["some_future_field"] = serde_json::json!({"nested": true});
    fs::write(&path, doc.to_string()).unwrap();

    let loaded = store::load(&path).unwrap();
    assert_eq!(loaded.project.name, "E");
}

#[test]
fn ids_survive_a_round_trip_so_references_stay_valid() {
    let dir = tempfile::tempdir().unwrap();
    let media = dir.path().join("m.mp4");
    fs::write(&media, b"m").unwrap();
    let path = dir.path().join("ids.verge");
    let original = sample_project(&media);

    let orig_asset = original.assets[0].id;
    let orig_clip = original.active().unwrap().tracks[0].clips()[0].id;

    store::save(&original, &path).unwrap();
    let loaded = store::load(&path).unwrap().project;

    assert_eq!(loaded.assets[0].id, orig_asset);
    let clip = &loaded.active().unwrap().tracks[0].clips()[0];
    assert_eq!(clip.id, orig_clip);
    assert_eq!(clip.asset(), Some(orig_asset), "the clip still points at the right media");
}

#[test]
fn saving_twice_is_byte_identical_apart_from_the_timestamp() {
    let dir = tempfile::tempdir().unwrap();
    let a = dir.path().join("a.verge");
    let b = dir.path().join("b.verge");
    let p = Project::with_default_sequence("Stable");
    store::save(&p, &a).unwrap();
    store::save(&p, &b).unwrap();

    let strip = |path: &Path| {
        let mut doc: serde_json::Value =
            serde_json::from_str(&fs::read_to_string(path).unwrap()).unwrap();
        let obj = doc.as_object_mut().unwrap();
        obj.remove("saved_at");
        obj.remove("saved_at_unix");
        doc
    };
    // Determinism matters for version control: an unchanged project must not
    // produce a churning diff.
    assert_eq!(strip(&a), strip(&b));
}

// ---- autosave ----------------------------------------------------------

#[test]
fn autosave_writes_beside_the_project_without_touching_it() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("auto.verge");
    store::save(&Project::with_default_sequence("Saved"), &path).unwrap();

    let mut auto = Autosave::new(dir.path(), Duration::from_secs(0));
    auto.set_project_path(Some(path.clone()));
    auto.mark_dirty();

    let target = auto.save(&Project::with_default_sequence("Unsaved work")).unwrap();
    assert_eq!(target, autosave::autosave_path(&path));
    assert!(target.exists());
    // The real project file is untouched by an autosave.
    assert_eq!(store::load(&path).unwrap().project.name, "Saved");
    assert_eq!(store::load(&target).unwrap().project.name, "Unsaved work");
}

#[test]
fn autosave_fires_only_when_dirty_and_due() {
    let mut auto = Autosave::new("/tmp", Duration::from_secs(3600));
    assert!(!auto.should_save(), "a clean project has nothing to save");
    auto.mark_dirty();
    assert!(auto.is_dirty());
    assert!(!auto.should_save(), "dirty but not yet due");

    auto.set_interval(Duration::ZERO);
    assert!(auto.should_save());

    auto.mark_saved();
    assert!(!auto.is_dirty());
    assert!(!auto.should_save());
}

#[test]
fn autosaving_does_not_clear_the_dirty_flag() {
    let dir = tempfile::tempdir().unwrap();
    let mut auto = Autosave::new(dir.path(), Duration::ZERO);
    auto.mark_dirty();
    auto.save(&Project::with_default_sequence("W")).unwrap();
    assert!(auto.is_dirty(), "an autosave is a safety net, not a save");
}

#[test]
fn a_project_that_has_never_been_saved_still_gets_autosaved() {
    let dir = tempfile::tempdir().unwrap();
    let mut auto = Autosave::new(dir.path(), Duration::ZERO);
    auto.mark_dirty();
    let target = auto.save(&Project::with_default_sequence("Untitled")).unwrap();
    assert!(target.starts_with(dir.path()));
    assert!(target.exists());
}

#[test]
fn a_newer_autosave_is_offered_for_recovery() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("recover.verge");
    store::save(&Project::with_default_sequence("Last explicit save"), &path).unwrap();

    // Write an autosave stamped later than the project, which is exactly the
    // state a crash between saves leaves behind.
    let auto_path = autosave::autosave_path(&path);
    let mut doc: serde_json::Value = serde_json::from_str(
        &store::to_json(&Project::with_default_sequence("Work since then")).unwrap(),
    )
    .unwrap();
    let project_stamp = {
        let d: serde_json::Value =
            serde_json::from_str(&fs::read_to_string(&path).unwrap()).unwrap();
        d["saved_at_unix"].as_u64().unwrap()
    };
    doc["saved_at_unix"] = serde_json::Value::from(project_stamp + 300);
    fs::write(&auto_path, doc.to_string()).unwrap();

    let candidate = autosave::recovery_candidate(&path).expect("recovery should be offered");
    assert_eq!(candidate.work_at_risk(), Duration::from_secs(300));
    let recovered = autosave::load_recovery(&candidate).unwrap();
    assert_eq!(recovered.project.name, "Work since then");
}

#[test]
fn an_older_autosave_is_not_offered() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("stale.verge");
    let auto_path = autosave::autosave_path(&path);

    let mut doc: serde_json::Value =
        serde_json::from_str(&store::to_json(&Project::with_default_sequence("Old")).unwrap())
            .unwrap();
    doc["saved_at_unix"] = serde_json::Value::from(1_000u64);
    fs::write(&auto_path, doc.to_string()).unwrap();

    // The project is saved now, so its timestamp is far ahead of the autosave.
    store::save(&Project::with_default_sequence("Current"), &path).unwrap();
    assert!(autosave::recovery_candidate(&path).is_none());
}

#[test]
fn no_autosave_means_nothing_to_recover() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("plain.verge");
    store::save(&Project::with_default_sequence("P"), &path).unwrap();
    assert!(autosave::recovery_candidate(&path).is_none());
}

#[test]
fn discarding_removes_the_autosave_and_is_safe_to_repeat() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("d.verge");
    let mut auto = Autosave::new(dir.path(), Duration::ZERO);
    auto.set_project_path(Some(path.clone()));
    let target = auto.save(&Project::with_default_sequence("D")).unwrap();

    assert!(target.exists());
    autosave::discard(&path);
    assert!(!target.exists());
    autosave::discard(&path); // must not panic
}

#[test]
fn a_project_written_before_blend_modes_existed_loads_as_normal() {
    // The field was added without a format bump, which is only safe if an older
    // file still reads. This is that file: a valid version 1 document whose clip
    // has no `blend` key at all.
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("old.verge");
    let saved = sample_project(&dir.path().join("a.mp4"));
    store::save(&saved, &path).unwrap();

    let text = fs::read_to_string(&path).unwrap();
    let mut tree: serde_json::Value = serde_json::from_str(&text).unwrap();
    let clips = tree["project"]["sequences"][0]["tracks"][0]["clips"]
        .as_array_mut()
        .expect("the fixture has a clip");
    for clip in clips.iter_mut() {
        assert!(clip.get("blend").is_some(), "the current format writes the field");
        clip.as_object_mut().unwrap().remove("blend");
    }
    fs::write(&path, serde_json::to_string_pretty(&tree).unwrap()).unwrap();

    let loaded = store::load(&path).unwrap().project;
    let sequence = loaded.active().unwrap();
    let clip = &sequence.tracks[0].clips()[0];
    assert_eq!(clip.blend, BlendMode::Normal, "a missing mode is Normal, not a failure");
}

#[test]
fn a_version_1_file_is_migrated_when_it_is_opened() {
    // A real version 1 document, as the first released build wrote them: a clip
    // names an `asset`, and there is no composition list at all.
    let dir = tempfile::tempdir().unwrap();
    let media = dir.path().join("m.mp4");
    fs::write(&media, b"m").unwrap();
    let path = dir.path().join("v1.verge");

    let doc = serde_json::json!({
        "format": FORMAT_MAGIC,
        "version": 1,
        "app_version": "0.1.0",
        "saved_at": "2026-01-01T00:00:00Z",
        "saved_at_unix": 1767225600u64,
        "project": {
            "name": "Version One",
            "assets": [{
                "id": 1,
                "name": "m.mp4",
                "path": media.to_string_lossy(),
                "info": {
                    "duration": 300000000i64,
                    "video": {
                        "size": {"width": 1920, "height": 1080},
                        "rate": {"num": 30, "den": 1},
                        "duration": 300000000i64,
                        "frame_count": 900,
                        "codec": "h264",
                        "pixel_format": "yuv420p",
                        "sample_aspect_ratio": [1, 1],
                    },
                    "audio": null,
                    "container": "mp4",
                },
            }],
            "sequences": [{
                "id": 2,
                "name": "Sequence 1",
                "settings": {
                    "resolution": {"width": 1920, "height": 1080},
                    "rate": {"num": 30, "den": 1},
                    "sample_rate": 48000,
                    "channels": 2,
                },
                "tracks": [{
                    "id": 3,
                    "kind": "video",
                    "name": "V1",
                    "clips": [{
                        "id": 4,
                        "name": "shot_01",
                        "asset": 1,
                        "source_in": 0,
                        "timeline_start": 0,
                        "duration": 100000000i64,
                    }],
                }],
            }],
            "active_sequence": 2,
            "ids": 5,
        },
    });
    fs::write(&path, serde_json::to_string_pretty(&doc).unwrap()).unwrap();

    let outcome = store::load(&path).unwrap();
    assert_eq!(outcome.migrated_from, Some(1), "the upgrade has to be reported");
    assert!(
        outcome.warnings.iter().any(|w| w.contains("version 2")),
        "and named in the warnings: {:?}",
        outcome.warnings
    );

    let clip = &outcome.project.active().unwrap().tracks[0].clips()[0];
    assert_eq!(clip.source, Source::Asset(outcome.project.assets[0].id));
    assert!(outcome.project.compositions.is_empty(), "there were none to read");

    // And saving it again writes the current version, so the upgrade sticks.
    store::save(&outcome.project, &path).unwrap();
    assert_eq!(store::load(&path).unwrap().migrated_from, None);
}

#[test]
fn compositions_and_their_nesting_survive_a_round_trip() {
    let dir = tempfile::tempdir().unwrap();
    let media = dir.path().join("m.mp4");
    fs::write(&media, b"m").unwrap();
    let path = dir.path().join("comps.verge");
    let original = project_with_compositions(&media);

    store::save(&original, &path).unwrap();
    let outcome = store::load(&path).unwrap();
    assert!(
        outcome.warnings.is_empty(),
        "a clean project should load clean: {:?}",
        outcome.warnings
    );

    // Equality across the whole composition list: every layer, its source, its
    // blend mode, its transform and every ID.
    assert_eq!(outcome.project.compositions, original.compositions);
    // And the clip that holds the outer composition came back as a clip holding
    // a composition, not as a clip pointing at asset number seven.
    assert_eq!(
        outcome.project.active().unwrap().tracks[0].clips(),
        original.active().unwrap().tracks[0].clips()
    );

    let outer = outcome
        .project
        .compositions
        .iter()
        .find(|c| c.name == "Titles")
        .expect("the outer composition");
    assert_eq!(outer.layers.len(), 1);
    assert_eq!(outer.layers[0].blend, BlendMode::Screen);
    let inner = outer.layers[0].source.composition().expect("a nested composition");
    assert_eq!(outcome.project.composition(inner).unwrap().name, "Lower Third");
    assert_eq!(outcome.project.nesting_depth(outer.id), 2);
}

#[test]
fn a_file_whose_compositions_form_a_cycle_opens_with_the_cycle_broken() {
    let dir = tempfile::tempdir().unwrap();
    let media = dir.path().join("m.mp4");
    fs::write(&media, b"m").unwrap();
    let path = dir.path().join("cycle.verge");
    store::save(&project_with_compositions(&media), &path).unwrap();

    // Hand-edit the inner composition to contain the outer one. Nothing in the
    // editor can produce this; a text editor can, and the loader has to survive
    // it rather than recursing forever at the first repaint.
    let mut doc: serde_json::Value =
        serde_json::from_str(&fs::read_to_string(&path).unwrap()).unwrap();
    let comps = doc["project"]["compositions"].as_array_mut().unwrap();
    let outer_id =
        comps.iter().find(|c| c["name"] == "Titles").and_then(|c| c["id"].as_u64()).unwrap();
    let inner = comps.iter_mut().find(|c| c["name"] == "Lower Third").unwrap();
    inner["layers"].as_array_mut().unwrap().push(serde_json::json!({
        "id": 9001,
        "name": "loop",
        "source": {"composition": outer_id},
        "start": 0,
        "duration": 10000000i64,
    }));
    fs::write(&path, serde_json::to_string_pretty(&doc).unwrap()).unwrap();

    let outcome = store::load(&path).unwrap();
    assert!(
        outcome.warnings.iter().any(|w| w.contains("nesting cycle")),
        "the repair has to be reported: {:?}",
        outcome.warnings
    );
    let inner = outcome.project.compositions.iter().find(|c| c.name == "Lower Third").unwrap();
    assert!(inner.nested().is_empty(), "the offending layer is gone");
    assert!(
        outcome.project.ids.peek() > 9001,
        "and the hand-written id was counted, so a new layer cannot collide"
    );
}

#[test]
fn a_project_without_a_colour_space_loads_as_perceptual() {
    // The field was added after version 2 shipped, additively. A project
    // authored before it existed has to keep compositing the way it was
    // authored against — defaulting to linear would silently change every
    // dissolve in every old project.
    use ve_core::ColorSpace;

    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("old.verge");

    let mut doc: serde_json::Value =
        serde_json::from_str(&store::to_json(&Project::with_default_sequence("Old")).unwrap())
            .unwrap();
    let settings = doc["project"]["sequences"][0]["settings"].as_object_mut().unwrap();
    assert!(settings.remove("color_space").is_some(), "the field should be written");
    fs::write(&path, doc.to_string()).unwrap();

    let loaded = store::load(&path).unwrap();
    assert_eq!(loaded.project.active().unwrap().settings.color_space, ColorSpace::Perceptual);
}

#[test]
fn a_colour_space_survives_a_round_trip() {
    use ve_core::ColorSpace;

    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("linear.verge");

    let mut project = Project::with_default_sequence("Linear");
    project.active_mut().unwrap().settings.color_space = ColorSpace::Linear;
    store::save(&project, &path).unwrap();

    let loaded = store::load(&path).unwrap();
    assert_eq!(loaded.project.active().unwrap().settings.color_space, ColorSpace::Linear);
}

// ---- audio: fades and track levels --------------------------------------

/// A project carrying everything Phase 4 added, for the round trips below.
fn project_with_audio(media: &Path) -> Project {
    let mut p = Project::with_default_sequence("Mix");
    let asset = p.add_asset(
        media,
        MediaInfo {
            duration: Ticks::from_seconds(30),
            video: None,
            audio: Some(ve_core::AudioStreamInfo {
                sample_rate: ve_time::SampleRate::HZ_48000,
                channels: 2,
                duration: Ticks::from_seconds(30),
                codec: "pcm_s16le".into(),
            }),
            container: "wav".into(),
        },
    );
    let seq = p.active_sequence.unwrap();
    let track = p
        .sequence(seq)
        .unwrap()
        .tracks
        .iter()
        .find(|t| t.kind == ve_core::TrackKind::Audio)
        .unwrap()
        .id;

    let clip_id = p.new_clip_id();
    let mut clip =
        Clip::new(clip_id, asset, "vo_01", Ticks::ZERO, Ticks::ZERO, Ticks::from_seconds(10));
    clip.audio.fade_in = ve_core::Fade::new(Ticks::from_millis(750), FadeCurve::EqualPower);
    clip.audio.fade_out = ve_core::Fade::new(Ticks::from_seconds(2), FadeCurve::Smooth);
    clip.audio.volume = ve_core::Property::constant(0.8);
    p.sequence_mut(seq).unwrap().track_mut(track).unwrap().insert_clip(clip).unwrap();

    let t = p.sequence_mut(seq).unwrap().track_mut(track).unwrap();
    t.volume = 0.6;
    t.pan = -0.35;
    p
}

#[test]
fn fades_and_track_levels_survive_a_save_and_reopen() {
    let dir = tempfile::tempdir().unwrap();
    let media = dir.path().join("vo.wav");
    fs::write(&media, b"not really audio").unwrap();
    let path = dir.path().join("mix.verge");

    store::save(&project_with_audio(&media), &path).unwrap();
    let loaded = store::load(&path).unwrap();
    assert!(loaded.warnings.is_empty(), "{:?}", loaded.warnings);

    let seq = loaded.project.active().unwrap();
    let track = seq.tracks.iter().find(|t| !t.is_empty()).expect("the audio track");
    assert_eq!(track.audio_level(), (0.6, -0.35));

    let clip = &track.clips()[0];
    assert_eq!(clip.audio.fade_in.length, Ticks::from_millis(750));
    assert_eq!(clip.audio.fade_in.curve, FadeCurve::EqualPower);
    assert_eq!(clip.audio.fade_out.length, Ticks::from_seconds(2));
    assert_eq!(clip.audio.fade_out.curve, FadeCurve::Smooth);
    assert_eq!(clip.audio.volume.value, 0.8);
}

/// Fades and track levels are additive, so they need no format bump: a file
/// written before they existed still opens, with no fade and unity gain.
#[test]
fn a_project_written_before_fades_existed_opens_silent_of_them() {
    let dir = tempfile::tempdir().unwrap();
    let media = dir.path().join("vo.wav");
    fs::write(&media, b"not really audio").unwrap();
    let path = dir.path().join("old.verge");

    store::save(&project_with_audio(&media), &path).unwrap();

    // Strip every field this phase introduced, as an older build's file would
    // simply not have had them.
    let mut doc: serde_json::Value =
        serde_json::from_str(&fs::read_to_string(&path).unwrap()).unwrap();
    for track in doc["project"]["sequences"][0]["tracks"].as_array_mut().unwrap() {
        track.as_object_mut().unwrap().remove("volume");
        track.as_object_mut().unwrap().remove("pan");
        for clip in track["clips"].as_array_mut().unwrap() {
            let audio = clip["audio"].as_object_mut().unwrap();
            audio.remove("fade_in");
            audio.remove("fade_out");
        }
    }
    fs::write(&path, serde_json::to_string_pretty(&doc).unwrap()).unwrap();

    let loaded = store::load(&path).unwrap();
    let seq = loaded.project.active().unwrap();
    let track = seq.tracks.iter().find(|t| !t.is_empty()).expect("the audio track");
    assert!(track.is_unity(), "a track with no level recorded plays at unity");

    let clip = &track.clips()[0];
    assert!(!clip.audio.fade_in.is_active());
    assert!(!clip.audio.fade_out.is_active());
    assert_eq!(clip.audio.volume.value, 0.8, "the level that was recorded is kept");
}

/// The common case — no fades at all — must not bloat every clip in the file.
#[test]
fn a_clip_with_no_fades_writes_none() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("plain.verge");
    store::save(&Project::with_default_sequence("Plain"), &path).unwrap();
    let text = fs::read_to_string(&path).unwrap();
    assert!(!text.contains("fade_in"), "an absent fade should not be written out");
}

/// A hand-edited file cannot push the mixer somewhere it will not go.
#[test]
fn a_hand_edited_track_level_is_clamped_when_it_is_read() {
    let dir = tempfile::tempdir().unwrap();
    let media = dir.path().join("vo.wav");
    fs::write(&media, b"not really audio").unwrap();
    let path = dir.path().join("hostile.verge");

    store::save(&project_with_audio(&media), &path).unwrap();
    let mut doc: serde_json::Value =
        serde_json::from_str(&fs::read_to_string(&path).unwrap()).unwrap();
    for track in doc["project"]["sequences"][0]["tracks"].as_array_mut().unwrap() {
        track["volume"] = serde_json::json!(-5.0);
        track["pan"] = serde_json::json!(12.0);
    }
    fs::write(&path, serde_json::to_string_pretty(&doc).unwrap()).unwrap();

    let loaded = store::load(&path).unwrap();
    for track in &loaded.project.active().unwrap().tracks {
        let (gain, pan) = track.audio_level();
        assert_eq!(gain, 0.0, "a negative gain must not invert the phase");
        assert_eq!(pan, 1.0);
    }
}

// ---- animation and motion blur ------------------------------------------

#[test]
fn keyframes_and_their_easing_survive_a_round_trip() {
    use ve_core::{Interpolation, Vec2};

    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("animated.verge");

    let mut project = Project::with_default_sequence("Animated");
    let asset = project.add_asset(
        "/media/a.mp4",
        MediaInfo {
            duration: Ticks::from_seconds(30),
            video: None,
            audio: None,
            container: "mp4".into(),
        },
    );
    let sequence = project.active_sequence.unwrap();
    let track = project.sequence(sequence).unwrap().tracks[0].id;
    let clip_id = project.new_clip_id();
    let mut clip = ve_core::Clip::new(
        clip_id,
        asset,
        "moving",
        Ticks::ZERO,
        Ticks::ZERO,
        Ticks::from_seconds(5),
    );
    clip.transform.position.set_keyframe(Ticks::ZERO, Vec2::ZERO, Interpolation::EaseInOut);
    clip.transform.position.set_keyframe(
        Ticks::from_seconds(5),
        Vec2::new(120.0, -40.0),
        Interpolation::Bezier { x1: 0.1, y1: 0.9, x2: 0.4, y2: 1.2 },
    );
    clip.motion_blur = true;
    project
        .sequence_mut(sequence)
        .unwrap()
        .track_mut(track)
        .unwrap()
        .insert_clip(clip)
        .unwrap();
    project.active_mut().unwrap().settings.motion_blur = ve_core::MotionBlur::new(144.0, 6);

    store::save(&project, &path).unwrap();
    let loaded = store::load(&path).unwrap();

    let (_, clip) = loaded.project.active().unwrap().find_clip(clip_id).unwrap();
    let kfs = clip.transform.position.keyframes();
    assert_eq!(kfs.len(), 2);
    assert_eq!(kfs[0].interpolation, Interpolation::EaseInOut);
    assert_eq!(
        kfs[1].interpolation,
        Interpolation::Bezier { x1: 0.1, y1: 0.9, x2: 0.4, y2: 1.2 },
        "a hand-shaped curve has to come back the shape it was"
    );
    assert_eq!(kfs[1].value, Vec2::new(120.0, -40.0));
    assert!(clip.motion_blur);
    assert_eq!(
        loaded.project.active().unwrap().settings.motion_blur,
        ve_core::MotionBlur::new(144.0, 6)
    );
}

#[test]
fn a_project_written_before_motion_blur_loads_with_it_off() {
    // Additive, like the colour space before it: an old project has to draw
    // exactly what it drew, so the clip switch defaults to off even though the
    // canvas shutter defaults to enabled.
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("old.verge");

    let mut project = Project::with_default_sequence("Old");
    let asset = project.add_asset(
        "/media/a.mp4",
        MediaInfo {
            duration: Ticks::from_seconds(30),
            video: None,
            audio: None,
            container: "mp4".into(),
        },
    );
    let sequence = project.active_sequence.unwrap();
    let track = project.sequence(sequence).unwrap().tracks[0].id;
    let clip_id = project.new_clip_id();
    project
        .sequence_mut(sequence)
        .unwrap()
        .track_mut(track)
        .unwrap()
        .insert_clip(ve_core::Clip::new(
            clip_id,
            asset,
            "clip",
            Ticks::ZERO,
            Ticks::ZERO,
            Ticks::from_seconds(5),
        ))
        .unwrap();

    let mut doc: serde_json::Value =
        serde_json::from_str(&store::to_json(&project).unwrap()).unwrap();
    let settings = doc["project"]["sequences"][0]["settings"].as_object_mut().unwrap();
    assert!(settings.remove("motion_blur").is_some(), "the field should be written");
    let clip = doc["project"]["sequences"][0]["tracks"][0]["clips"][0].as_object_mut().unwrap();
    assert!(clip.remove("motion_blur").is_some(), "the field should be written");
    fs::write(&path, doc.to_string()).unwrap();

    let loaded = store::load(&path).unwrap();
    let sequence = loaded.project.active().unwrap();
    assert!(!sequence.find_clip(clip_id).unwrap().1.motion_blur, "no clip blurs by default");
    assert_eq!(
        sequence.settings.motion_blur,
        ve_core::MotionBlur::default(),
        "the shutter comes back at the film convention"
    );
}

#[test]
fn a_moved_project_folder_relinks_its_proxies_too() {
    let dir = tempfile::tempdir().unwrap();
    let original_dir = dir.path().join("original");
    fs::create_dir_all(original_dir.join("proxies")).unwrap();
    let media = original_dir.join("movie.mp4");
    fs::write(&media, b"media").unwrap();
    let proxy = original_dir.join("proxies").join("movie.mov");
    fs::write(&proxy, b"proxy").unwrap();

    let mut project = sample_project(&media);
    project.settings.use_proxies = true;
    project.assets[0].proxy =
        Some(ve_core::ProxyMedia::new(&proxy, ve_core::Size::new(640, 360)));
    let path = original_dir.join("p.verge");
    store::save(&project, &path).unwrap();

    let moved_dir = dir.path().join("moved");
    fs::rename(&original_dir, &moved_dir).unwrap();

    let loaded = store::load(&moved_dir.join("p.verge")).unwrap();
    let asset = &loaded.project.assets[0];
    assert!(loaded.project.settings.use_proxies, "the switch is part of the project");

    let source = asset.picture_source(Some(&moved_dir), true);
    assert!(source.is_proxy, "the proxy moved with the project and should be found");
    assert_eq!(source.path, moved_dir.join("proxies").join("movie.mov"));
    assert_eq!(asset.proxy.as_ref().unwrap().size, ve_core::Size::new(640, 360));
}

#[test]
fn a_project_whose_proxies_were_left_behind_opens_at_full_resolution() {
    let dir = tempfile::tempdir().unwrap();
    let media = dir.path().join("movie.mp4");
    fs::write(&media, b"media").unwrap();
    let proxy = dir.path().join("movie.proxy.mov");
    fs::write(&proxy, b"proxy").unwrap();

    let mut project = sample_project(&media);
    project.settings.use_proxies = true;
    project.assets[0].proxy =
        Some(ve_core::ProxyMedia::new(&proxy, ve_core::Size::new(640, 360)));
    let path = dir.path().join("p.verge");
    store::save(&project, &path).unwrap();

    // The user cleared their proxy directory between sessions.
    fs::remove_file(&proxy).unwrap();

    let loaded = store::load(&path).unwrap();
    let asset = &loaded.project.assets[0];
    assert!(!asset.offline, "losing a proxy must not take the footage offline");
    assert!(loaded.warnings.is_empty(), "{:?}", loaded.warnings);
    assert!(asset.has_proxy(), "it is remembered, so rebuilding or restoring it works");
    assert!(!asset.picture_source(Some(dir.path()), true).is_proxy);
}
