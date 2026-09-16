//! Native file dialogs.
//!
//! Wrapped so the rest of the editor never depends on a dialog being available:
//! every one of these returns `None` when there is no desktop portal to talk
//! to, which is exactly the situation in a headless test or on a build machine.
//! Nothing in [`crate::actions`] needs a dialog — actions carry paths — so the
//! editor stays fully drivable without one.

use std::path::PathBuf;

/// Extensions offered in the media import dialog.
const MEDIA_EXTENSIONS: &[&str] = &[
    "mp4", "mov", "mkv", "avi", "webm", "m4v", "mxf", "wav", "mp3", "aac", "flac", "m4a",
    "ogg", "opus",
];

pub fn pick_media_files() -> Option<Vec<PathBuf>> {
    rfd::FileDialog::new()
        .add_filter("Media", MEDIA_EXTENSIONS)
        .set_title("Import media")
        .pick_files()
}

pub fn pick_project_to_open() -> Option<PathBuf> {
    rfd::FileDialog::new()
        .add_filter("Verge project", &[ve_project::PROJECT_EXTENSION])
        .set_title("Open project")
        .pick_file()
}

pub fn pick_project_to_save(suggested: &str) -> Option<PathBuf> {
    let path = rfd::FileDialog::new()
        .add_filter("Verge project", &[ve_project::PROJECT_EXTENSION])
        .set_file_name(suggested)
        .set_title("Save project as")
        .save_file()?;
    // A project saved without the extension would not be found by the open
    // dialog's filter later, so add it if the user left it off.
    Some(match path.extension() {
        Some(_) => path,
        None => path.with_extension(ve_project::PROJECT_EXTENSION),
    })
}
