//! Autosave and crash recovery.
//!
//! An autosave is written beside the project as `<name>.autosave`, using the
//! same atomic write as a real save. It is never written over the project file
//! itself, so an autosave can never destroy a deliberate save.
//!
//! On open, [`recovery_candidate`] compares timestamps: an autosave newer than
//! the project means the editor stopped between a save and the next one, and
//! the user is offered the newer work.

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use ve_core::Project;

use crate::store::{self, LoadOutcome};
use crate::ProjectError;

/// Suffix appended to a project path for its autosave.
pub const AUTOSAVE_SUFFIX: &str = "autosave";

/// The autosave sibling of a project path.
pub fn autosave_path(project_path: &Path) -> PathBuf {
    let mut name = project_path.file_name().unwrap_or_default().to_os_string();
    name.push(".");
    name.push(AUTOSAVE_SUFFIX);
    project_path.with_file_name(name)
}

/// An autosave that is newer than the project file it belongs to.
#[derive(Debug, Clone)]
pub struct RecoveryCandidate {
    pub autosave: PathBuf,
    pub project: PathBuf,
    /// Epoch seconds recorded inside each file.
    pub autosave_saved_at: u64,
    pub project_saved_at: u64,
}

impl RecoveryCandidate {
    /// How much work the autosave holds that the project file does not.
    pub fn work_at_risk(&self) -> Duration {
        Duration::from_secs(self.autosave_saved_at.saturating_sub(self.project_saved_at))
    }
}

/// Returns an autosave worth offering to the user, if one exists.
///
/// Reads the recorded timestamp from inside each file rather than the
/// filesystem mtime, because a file copy or a checkout can rewrite mtimes and
/// would otherwise silently offer stale work as newer.
pub fn recovery_candidate(project_path: &Path) -> Option<RecoveryCandidate> {
    let auto = autosave_path(project_path);
    if !auto.exists() {
        return None;
    }
    let autosave_saved_at = saved_at(&auto)?;
    // A missing or unreadable project file means everything in the autosave is
    // at risk, so treat it as infinitely old.
    let project_saved_at = saved_at(project_path).unwrap_or(0);

    (autosave_saved_at > project_saved_at).then_some(RecoveryCandidate {
        autosave: auto,
        project: project_path.to_path_buf(),
        autosave_saved_at,
        project_saved_at,
    })
}

/// Reads just the `saved_at_unix` field, without deserialising the project.
fn saved_at(path: &Path) -> Option<u64> {
    let text = std::fs::read_to_string(path).ok()?;
    let doc: serde_json::Value = serde_json::from_str(&text).ok()?;
    doc.get("saved_at_unix").and_then(serde_json::Value::as_u64)
}

/// Loads a recovery candidate's contents.
pub fn load_recovery(candidate: &RecoveryCandidate) -> Result<LoadOutcome, ProjectError> {
    store::load(&candidate.autosave)
}

/// Removes the autosave for a project. Called after a successful explicit save,
/// at which point the autosave holds nothing the project file does not.
pub fn discard(project_path: &Path) {
    let auto = autosave_path(project_path);
    if auto.exists() {
        if let Err(e) = std::fs::remove_file(&auto) {
            log::warn!("could not remove autosave {}: {e}", auto.display());
        }
    }
}

/// Decides *when* to autosave. Does not own the project or perform I/O on a
/// timer of its own.
///
/// The editor drives this from its frame loop by calling [`Autosave::should_save`]
/// and, when it returns true, handing the project to [`Autosave::save`]. Keeping
/// the policy separate from the writing means the scheduling is testable without
/// touching a disk or waiting on a clock.
pub struct Autosave {
    /// Where the project itself lives. `None` for a project that has never been
    /// saved, in which case autosaves go to `scratch_dir`.
    project_path: Option<PathBuf>,
    scratch_dir: PathBuf,
    interval: Duration,
    last_save: Instant,
    dirty: bool,
}

impl Autosave {
    pub fn new(scratch_dir: impl Into<PathBuf>, interval: Duration) -> Self {
        Autosave {
            project_path: None,
            scratch_dir: scratch_dir.into(),
            interval,
            last_save: Instant::now(),
            dirty: false,
        }
    }

    /// Points autosaves at a project's location. Called on save-as and on open.
    pub fn set_project_path(&mut self, path: Option<PathBuf>) {
        self.project_path = path;
    }

    pub fn set_interval(&mut self, interval: Duration) {
        self.interval = interval;
    }

    /// Records that the project has unsaved changes. Cheap enough to call from
    /// every command.
    pub fn mark_dirty(&mut self) {
        self.dirty = true;
    }

    pub fn is_dirty(&self) -> bool {
        self.dirty
    }

    /// Records that the project was saved explicitly, which clears the dirty
    /// flag and restarts the interval.
    pub fn mark_saved(&mut self) {
        self.dirty = false;
        self.last_save = Instant::now();
    }

    /// Where the next autosave would be written.
    pub fn target(&self) -> PathBuf {
        match &self.project_path {
            Some(p) => autosave_path(p),
            // A project with no home yet still gets protected.
            None => self.scratch_dir.join("untitled.verge.autosave"),
        }
    }

    /// Whether enough time has passed and there is anything worth writing.
    pub fn should_save(&self) -> bool {
        self.dirty && self.last_save.elapsed() >= self.interval
    }

    /// Writes the autosave and restarts the interval.
    ///
    /// Deliberately leaves the dirty flag set: an autosave is a safety net, not
    /// a save, and the project still has changes the user has not committed.
    pub fn save(&mut self, project: &Project) -> Result<PathBuf, ProjectError> {
        let target = self.target();
        let json = store::to_json(project)?;
        store::write_atomically(&target, json.as_bytes(), false)?;
        self.last_save = Instant::now();
        log::debug!("autosaved to {}", target.display());
        Ok(target)
    }

    /// Time until the next autosave is due, for a status readout.
    pub fn time_until_due(&self) -> Option<Duration> {
        self.dirty.then(|| self.interval.saturating_sub(self.last_save.elapsed()))
    }
}
