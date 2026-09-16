//! Crash-safe reading and writing of project files.
//!
//! # The durability rule
//!
//! The primary project file is never written in place. Every save goes to a
//! temporary file in the same directory, is flushed to disk, and is then moved
//! over the target with a single atomic rename. A crash at any point leaves the
//! target holding either the complete previous version or the complete new one,
//! never a half-written file.
//!
//! Before that rename the previous version is moved aside to a `.bak` sibling,
//! which is also atomic. The only window in which the primary path does not
//! exist is between those two renames, and [`load`] recovers from exactly that
//! case by falling back to the backup.

use std::fs::{self, File};
use std::io::{BufWriter, Read, Write};
use std::path::{Path, PathBuf};

use serde_json::Value;
use ve_core::Project;

use crate::format::{ProjectFile, FORMAT_MAGIC, FORMAT_VERSION};
use crate::migrate::migrate_to_current;
use crate::ProjectError;

/// Suffix for the previous version of a project file.
pub const BACKUP_SUFFIX: &str = "bak";

/// What [`load`] found.
#[derive(Debug)]
pub struct LoadOutcome {
    pub project: Project,
    /// Non-fatal problems: dangling asset references, overlapping clips, a
    /// format upgrade. The project is usable regardless.
    pub warnings: Vec<String>,
    /// Set when the file was written by an older format version.
    pub migrated_from: Option<u32>,
    /// Set when the primary file was missing or unreadable and the backup was
    /// used instead.
    pub recovered_from_backup: bool,
    /// Epoch seconds recorded in the file.
    pub saved_at_unix: u64,
}

/// The `.bak` sibling of a project path.
pub fn backup_path(path: &Path) -> PathBuf {
    let mut name = path.file_name().unwrap_or_default().to_os_string();
    name.push(".");
    name.push(BACKUP_SUFFIX);
    path.with_file_name(name)
}

/// Serialises a project to pretty-printed JSON.
///
/// Pretty-printed because the format is meant to be diffable and inspectable by
/// hand; project files are small next to the media they reference, so the extra
/// whitespace costs nothing that matters.
pub fn to_json(project: &Project) -> Result<String, ProjectError> {
    let file = ProjectFile::wrap(project.clone());
    Ok(serde_json::to_string_pretty(&file)?)
}

/// Parses a project from JSON text, running migrations and repairing
/// invariants.
pub fn from_json(text: &str) -> Result<LoadOutcome, ProjectError> {
    let mut doc: Value = serde_json::from_str(text)?;

    match doc.get("format").and_then(Value::as_str) {
        Some(FORMAT_MAGIC) => {}
        Some(other) => return Err(ProjectError::NotAProjectFile(other.to_string())),
        None => return Err(ProjectError::NotAProjectFile("<missing>".into())),
    }

    let original_version =
        doc.get("version").and_then(Value::as_u64).ok_or(ProjectError::MissingVersion)? as u32;

    let mut warnings = migrate_to_current(&mut doc)?;
    let migrated_from = (original_version < FORMAT_VERSION).then_some(original_version);

    let file: ProjectFile = serde_json::from_value(doc)?;
    let mut project = file.project;
    // The file is untrusted input: it may have been hand-edited, or written by
    // a build with a bug. Repair what can be repaired and report the rest.
    warnings.extend(project.normalise());

    Ok(LoadOutcome {
        project,
        warnings,
        migrated_from,
        recovered_from_backup: false,
        saved_at_unix: file.saved_at_unix,
    })
}

/// Writes `project` to `path` atomically, rotating the previous version to
/// `.bak`.
///
/// Asset paths are relinked relative to the project's directory first, so a
/// project folder that is moved or shared still finds its media.
pub fn save(project: &Project, path: &Path) -> Result<(), ProjectError> {
    let mut project = project.clone();
    if let Some(dir) = path.parent() {
        for asset in &mut project.assets {
            asset.relink_relative_to(dir);
        }
    }

    let json = to_json(&project)?;
    write_atomically(path, json.as_bytes(), true)
}

/// Writes bytes to `path` via a temporary file and an atomic rename.
///
/// `rotate_backup` moves any existing file to `.bak` first. Autosaves pass
/// `false`: they are themselves the backup, and a `.bak.autosave` chain would
/// just be noise.
pub fn write_atomically(
    path: &Path,
    bytes: &[u8],
    rotate_backup: bool,
) -> Result<(), ProjectError> {
    let dir = path.parent().unwrap_or(Path::new("."));
    fs::create_dir_all(dir).map_err(|e| ProjectError::io(dir, e))?;

    // The temporary lives in the destination directory so that the rename stays
    // within one filesystem, which is what makes it atomic.
    let tmp = temp_sibling(path);

    {
        let file = File::create(&tmp).map_err(|e| ProjectError::io(&tmp, e))?;
        let mut writer = BufWriter::new(file);
        writer.write_all(bytes).map_err(|e| ProjectError::io(&tmp, e))?;
        writer.flush().map_err(|e| ProjectError::io(&tmp, e))?;
        // Durability: the rename below is only meaningful once the bytes
        // themselves have reached the disk.
        writer.get_ref().sync_all().map_err(|e| ProjectError::io(&tmp, e))?;
    }

    if rotate_backup && path.exists() {
        let bak = backup_path(path);
        // A rename, not a copy: instant, and it cannot half-succeed.
        if let Err(e) = fs::rename(path, &bak) {
            let _ = fs::remove_file(&tmp);
            return Err(ProjectError::io(&bak, e));
        }
    }

    if let Err(e) = fs::rename(&tmp, path) {
        let _ = fs::remove_file(&tmp);
        return Err(ProjectError::io(path, e));
    }

    // Flushing the directory makes the rename itself durable. Without this the
    // entry can still be lost to a power cut on some filesystems. Best-effort:
    // on platforms where opening a directory is not permitted this is a no-op,
    // and the file contents are already safe.
    if let Ok(dir_handle) = File::open(dir) {
        let _ = dir_handle.sync_all();
    }

    Ok(())
}

/// A unique temporary path next to `path`.
///
/// The process ID and a counter keep two editor instances saving the same
/// project from clobbering each other's temporaries.
fn temp_sibling(path: &Path) -> PathBuf {
    use std::sync::atomic::{AtomicU64, Ordering};
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let n = COUNTER.fetch_add(1, Ordering::Relaxed);
    let mut name = path.file_name().unwrap_or_default().to_os_string();
    name.push(format!(".{}.{}.tmp", std::process::id(), n));
    path.with_file_name(name)
}

/// Reads a project, falling back to the `.bak` sibling if the primary file is
/// missing or unreadable.
pub fn load(path: &Path) -> Result<LoadOutcome, ProjectError> {
    let primary = read_project(path);

    match primary {
        Ok(mut outcome) => {
            resolve_asset_paths(&mut outcome, path);
            Ok(outcome)
        }
        Err(primary_err) => {
            let bak = backup_path(path);
            if !bak.exists() {
                return Err(primary_err);
            }
            log::warn!(
                "{} could not be read ({primary_err}); falling back to {}",
                path.display(),
                bak.display()
            );
            let mut outcome = read_project(&bak)?;
            outcome.recovered_from_backup = true;
            outcome.warnings.push(format!(
                "recovered from backup after failing to read the project: {primary_err}"
            ));
            resolve_asset_paths(&mut outcome, path);
            Ok(outcome)
        }
    }
}

fn read_project(path: &Path) -> Result<LoadOutcome, ProjectError> {
    // Read once, parse once. An earlier version parsed to a `Value`, serialised
    // that back to a string and parsed it again to keep a single code path;
    // the `load_project` benchmark showed that costing more than everything
    // else in the load put together.
    let mut file = File::open(path).map_err(|e| ProjectError::io(path, e))?;
    let mut text = String::new();
    file.read_to_string(&mut text).map_err(|e| ProjectError::io(path, e))?;
    from_json(&text)
}

/// Rewrites each asset's absolute path from its project-relative form, and
/// flags any that are missing.
///
/// A missing file is never fatal: the project opens, the asset is marked
/// offline, and clips referencing it render as offline until the user relinks.
fn resolve_asset_paths(outcome: &mut LoadOutcome, project_path: &Path) {
    let dir = project_path.parent().map(Path::to_path_buf);
    for asset in &mut outcome.project.assets {
        let resolved = asset.resolve_path(dir.as_deref());
        asset.path = resolved;
        asset.offline = !asset.path.exists();
        if asset.offline {
            outcome.warnings.push(format!("media offline: {}", asset.path.display()));
        }
    }
}
