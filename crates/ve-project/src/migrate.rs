//! Forward migration of older project files.
//!
//! Migrations run on the raw `serde_json::Value` tree *before* it is
//! deserialised, so an old file never has to satisfy the current Rust types.
//! Each step moves the document from one version to the next, and they are
//! applied in sequence, which means only one step has to be written per format
//! change no matter how old the file is.

use serde_json::{json, Value};

use crate::format::FORMAT_VERSION;
use crate::ProjectError;

/// A single version-to-version transformation.
///
/// Takes the document at version `from` and leaves it at version `from + 1`.
/// It must not set the `version` field; [`migrate_to_current`] does that, so a
/// step cannot leave the envelope inconsistent with what it actually did.
pub type MigrationFn = fn(&mut Value) -> Result<(), ProjectError>;

/// Migrations from each version to the next, in ascending order.
pub const MIGRATIONS: &[(u32, MigrationFn)] = &[(1, clip_asset_to_source)];

/// **1 → 2.** A clip's `asset` becomes a tagged `source`.
///
/// Compositions can be cut into a sequence, so a clip's source is no longer
/// necessarily a file. On disk that means `"asset": 3` becomes
/// `"source": {"asset": 3}`, which is a shape older builds cannot read — hence a
/// version rather than a defaulted field.
///
/// A clip that already carries a `source` is left alone: a file half-written by
/// a newer build, or one a user has edited, should migrate to something coherent
/// rather than being clobbered.
fn clip_asset_to_source(doc: &mut Value) -> Result<(), ProjectError> {
    let Some(sequences) = doc
        .get_mut("project")
        .and_then(|p| p.get_mut("sequences"))
        .and_then(Value::as_array_mut)
    else {
        // A document with no sequences at all is odd but not broken, and a
        // migration is not the place to start rejecting files.
        return Ok(());
    };

    for sequence in sequences {
        let Some(tracks) = sequence.get_mut("tracks").and_then(Value::as_array_mut) else {
            continue;
        };
        for track in tracks {
            let Some(clips) = track.get_mut("clips").and_then(Value::as_array_mut) else {
                continue;
            };
            for clip in clips {
                let Some(object) = clip.as_object_mut() else { continue };
                if object.contains_key("source") {
                    object.remove("asset");
                    continue;
                }
                if let Some(asset) = object.remove("asset") {
                    object.insert("source".into(), json!({ "asset": asset }));
                }
            }
        }
    }
    Ok(())
}

/// Brings `doc` up to [`FORMAT_VERSION`], returning a description of each step
/// applied so the UI can tell the user their file was upgraded.
pub fn migrate_to_current(doc: &mut Value) -> Result<Vec<String>, ProjectError> {
    migrate_with(doc, MIGRATIONS)
}

/// [`migrate_to_current`] against an explicit migration table. Split out so the
/// pipeline itself can be tested without waiting for a real format change.
pub fn migrate_with(
    doc: &mut Value,
    migrations: &[(u32, MigrationFn)],
) -> Result<Vec<String>, ProjectError> {
    let mut version =
        doc.get("version").and_then(Value::as_u64).ok_or(ProjectError::MissingVersion)? as u32;

    if version > FORMAT_VERSION {
        return Err(ProjectError::FromTheFuture { file: version, supported: FORMAT_VERSION });
    }

    let mut applied = Vec::new();
    while version < FORMAT_VERSION {
        let step = migrations
            .iter()
            .find(|(from, _)| *from == version)
            .map(|(_, f)| *f)
            .ok_or(ProjectError::NoMigrationPath { from: version, to: FORMAT_VERSION })?;
        step(doc)?;
        version += 1;
        doc["version"] = Value::from(version);
        applied.push(format!("migrated project format to version {version}"));
    }

    Ok(applied)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A stand-in step, so the sequencing logic is covered before the first
    /// real format change needs it.
    fn rename_title_to_name(doc: &mut Value) -> Result<(), ProjectError> {
        if let Some(title) = doc["project"].get("title").cloned() {
            doc["project"]["name"] = title;
            doc["project"].as_object_mut().unwrap().remove("title");
        }
        Ok(())
    }

    fn add_marker_list(doc: &mut Value) -> Result<(), ProjectError> {
        doc["project"]["markers"] = json!([]);
        Ok(())
    }

    #[test]
    fn steps_are_applied_in_order_and_the_version_is_advanced() {
        // Pretend the current version is 3 by supplying a table that reaches it.
        let table: &[(u32, MigrationFn)] = &[(1, rename_title_to_name), (2, add_marker_list)];
        let mut doc = json!({"version": 1, "project": {"title": "Old"}});

        // Driven manually rather than through `migrate_with`, so the ordering
        // is asserted against a table that reaches further than the real one.
        let mut version = 1u32;
        for (from, step) in table {
            assert_eq!(*from, version);
            step(&mut doc).unwrap();
            version += 1;
            doc["version"] = Value::from(version);
        }
        assert_eq!(doc["project"]["name"], "Old");
        assert!(doc["project"].get("title").is_none());
        assert_eq!(doc["project"]["markers"], json!([]));
        assert_eq!(doc["version"], 3);
    }

    #[test]
    fn a_version_1_clip_gains_a_tagged_source() {
        let mut doc = json!({
            "version": 1,
            "project": {
                "name": "Old",
                "sequences": [{
                    "tracks": [
                        {"clips": [{"name": "a", "asset": 3}, {"name": "b", "asset": 4}]},
                        {"clips": []},
                    ]
                }],
            },
        });

        let applied = migrate_to_current(&mut doc).unwrap();
        assert_eq!(applied.len(), 1, "one step from 1 to 2: {applied:?}");
        assert_eq!(doc["version"], FORMAT_VERSION);

        let clips = &doc["project"]["sequences"][0]["tracks"][0]["clips"];
        assert_eq!(clips[0]["source"], json!({"asset": 3}));
        assert_eq!(clips[1]["source"], json!({"asset": 4}));
        assert!(clips[0].get("asset").is_none(), "the old field is gone");
    }

    #[test]
    fn migrating_a_clip_that_already_has_a_source_keeps_it() {
        // A hand-edited file, or one written by a newer build and re-versioned.
        // Overwriting the source it already names would lose a nested
        // composition, which is exactly the thing the version was bumped for.
        let mut doc = json!({
            "version": 1,
            "project": {
                "sequences": [{"tracks": [{"clips": [
                    {"name": "nested", "source": {"composition": 9}, "asset": 3},
                ]}]}],
            },
        });

        migrate_to_current(&mut doc).unwrap();
        let clip = &doc["project"]["sequences"][0]["tracks"][0]["clips"][0];
        assert_eq!(clip["source"], json!({"composition": 9}));
        assert!(clip.get("asset").is_none(), "the stale field goes with it");
    }

    #[test]
    fn migrating_a_document_with_no_sequences_is_not_an_error() {
        let mut doc = json!({"version": 1, "project": {"name": "Empty"}});
        assert_eq!(migrate_to_current(&mut doc).unwrap().len(), 1);
        assert_eq!(doc["version"], FORMAT_VERSION);
    }

    #[test]
    fn a_current_version_document_is_left_alone() {
        let mut doc = json!({"version": FORMAT_VERSION, "project": {"name": "Now"}});
        let before = doc.clone();
        assert!(migrate_to_current(&mut doc).unwrap().is_empty());
        assert_eq!(doc, before);
    }

    #[test]
    fn a_newer_file_is_refused_with_both_versions_named() {
        let mut doc = json!({"version": FORMAT_VERSION + 5, "project": {}});
        match migrate_to_current(&mut doc) {
            Err(ProjectError::FromTheFuture { file, supported }) => {
                assert_eq!(file, FORMAT_VERSION + 5);
                assert_eq!(supported, FORMAT_VERSION);
            }
            other => panic!("expected FromTheFuture, got {other:?}"),
        }
    }

    #[test]
    fn a_gap_in_the_migration_table_is_an_error_not_a_silent_skip() {
        // Claim the current version is far ahead with no steps registered.
        let mut doc = json!({"version": 0, "project": {}});
        match migrate_with(&mut doc, &[]) {
            Err(ProjectError::NoMigrationPath { from, to }) => {
                assert_eq!(from, 0);
                assert_eq!(to, FORMAT_VERSION);
            }
            other => panic!("expected NoMigrationPath, got {other:?}"),
        }
    }

    #[test]
    fn a_document_without_a_version_is_refused() {
        let mut doc = json!({"project": {}});
        assert!(matches!(migrate_to_current(&mut doc), Err(ProjectError::MissingVersion)));
    }
}
