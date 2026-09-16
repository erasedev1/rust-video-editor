use serde::{Deserialize, Serialize};
use ve_core::Project;

use crate::timestamp;

/// Magic string identifying a Verge project file.
pub const FORMAT_MAGIC: &str = "verge-project";

/// The format version this build writes.
///
/// Bump this whenever the on-disk shape changes in a way older builds cannot
/// read, and add a migration in [`crate::migrate`] from the previous version.
pub const FORMAT_VERSION: u32 = 1;

/// Conventional file extension.
pub const PROJECT_EXTENSION: &str = "verge";

/// The root object of a `.verge` file.
///
/// The envelope fields come first and are all plain scalars, so a reader can
/// determine the version without understanding the payload. That is what makes
/// migration possible: [`crate::migrate`] works on the raw JSON tree, before
/// anything is deserialised into the typed model.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProjectFile {
    /// Always [`FORMAT_MAGIC`]. Guards against opening an unrelated JSON file.
    pub format: String,
    pub version: u32,
    /// The build that wrote this file, for bug reports. Never used for logic.
    #[serde(default)]
    pub app_version: String,
    #[serde(default)]
    pub saved_at: String,
    #[serde(default)]
    pub saved_at_unix: u64,
    pub project: Project,
}

impl ProjectFile {
    pub fn wrap(project: Project) -> Self {
        let now = timestamp::now_unix();
        ProjectFile {
            format: FORMAT_MAGIC.to_string(),
            version: FORMAT_VERSION,
            app_version: env!("CARGO_PKG_VERSION").to_string(),
            saved_at: timestamp::format_iso8601(now),
            saved_at_unix: now,
            project,
        }
    }
}
