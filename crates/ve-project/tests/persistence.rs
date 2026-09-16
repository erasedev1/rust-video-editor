//! Save/load round trips, atomic-write durability, backup fallback, and the
//! handling of corrupt or hostile project files.

use std::fs;
use std::path::Path;
use std::time::Duration;

use ve_core::{Clip, Interpolation, MediaInfo, Project, Size, VideoStreamInfo};
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
    clip.transform.opacity.set_keyframe(Ticks::ZERO, 0.0, Interpolation::EaseInOut);
    clip.transform.opacity.set_keyframe(Ticks::from_seconds(1), 1.0, Interpolation::Linear);
    p.sequence_mut(seq).unwrap().track_mut(track).unwrap().insert_clip(clip).unwrap();
    p.sequence_mut(seq).unwrap().set_playhead(Ticks::from_seconds(3));
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
    assert_eq!(clip.asset, orig_asset, "the clip still points at the right media");
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
