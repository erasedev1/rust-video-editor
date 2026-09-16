//! The Verge project file: a versioned, human-readable, crash-safe format.
//!
//! # Design
//!
//! * **JSON, pretty-printed.** Projects are small next to the media they point
//!   at, and being diffable and hand-inspectable is worth far more than the
//!   bytes. Version control works on them as-is.
//! * **Versioned envelope.** The `format`, `version` and timestamp fields sit
//!   at the top level as plain scalars, so any reader can determine what it is
//!   holding before it understands the payload. Older files are upgraded by the
//!   [`migrate`] pipeline, which operates on raw JSON.
//! * **Media is referenced, never embedded.** Assets store both an absolute and
//!   a project-relative path, so a moved project folder relinks itself and a
//!   missing file degrades to an offline asset rather than a failed open.
//! * **Atomic writes.** See [`store`] for the durability rule.

pub mod autosave;
pub mod format;
pub mod migrate;
pub mod store;
mod timestamp;

pub use autosave::{autosave_path, recovery_candidate, Autosave, RecoveryCandidate};
pub use format::{ProjectFile, FORMAT_MAGIC, FORMAT_VERSION, PROJECT_EXTENSION};
pub use store::{backup_path, from_json, load, save, to_json, LoadOutcome};

use std::path::{Path, PathBuf};

#[derive(Debug, thiserror::Error)]
pub enum ProjectError {
    #[error("{path}: {source}")]
    Io {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("malformed project JSON: {0}")]
    Json(#[from] serde_json::Error),
    #[error("not a Verge project file (format field was '{0}')")]
    NotAProjectFile(String),
    #[error("project file has no version field")]
    MissingVersion,
    #[error(
        "this project was written by a newer version of Verge \
         (file format {file}, this build supports up to {supported})"
    )]
    FromTheFuture { file: u32, supported: u32 },
    #[error("no migration path from project format {from} to {to}")]
    NoMigrationPath { from: u32, to: u32 },
}

impl ProjectError {
    pub(crate) fn io(path: impl AsRef<Path>, source: std::io::Error) -> Self {
        ProjectError::Io { path: path.as_ref().to_path_buf(), source }
    }
}
