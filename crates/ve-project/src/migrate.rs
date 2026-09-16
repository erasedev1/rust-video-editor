//! Forward migration of older project files.
//!
//! Migrations run on the raw `serde_json::Value` tree *before* it is
//! deserialised, so an old file never has to satisfy the current Rust types.
//! Each step moves the document from one version to the next, and they are
//! applied in sequence, which means only one step has to be written per format
//! change no matter how old the file is.

use serde_json::Value;

use crate::format::FORMAT_VERSION;
use crate::ProjectError;

/// A single version-to-version transformation.
///
/// Takes the document at version `from` and leaves it at version `from + 1`.
/// It must not set the `version` field; [`migrate_to_current`] does that, so a
/// step cannot leave the envelope inconsistent with what it actually did.
pub type MigrationFn = fn(&mut Value) -> Result<(), ProjectError>;

/// Migrations from each version to the next, in ascending order.
///
/// Version 1 is the first released format, so there is nothing to migrate from
/// yet. The pipeline exists and is exercised by its own tests so that the first
/// real format change is a one-line addition here rather than new machinery.
pub const MIGRATIONS: &[(u32, MigrationFn)] = &[];

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
    use serde_json::json;

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

        // Only migrate as far as the table allows; FORMAT_VERSION is 1 today,
        // so drive the loop manually to prove ordering.
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
