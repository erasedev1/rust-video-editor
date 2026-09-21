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

/// Extensions offered in the export dialogue's file picker.
const EXPORT_EXTENSIONS: &[&str] = &["mp4", "mov", "mkv"];

/// Where an export should be written, starting from where it would go anyway.
///
/// The suggested name and directory come from the settings the dialogue
/// already holds, so choosing a file is a correction rather than a form to
/// fill in.
pub fn pick_export_file(suggested: &std::path::Path) -> Option<PathBuf> {
    let mut dialog =
        rfd::FileDialog::new().add_filter("Video", EXPORT_EXTENSIONS).set_title("Export");
    if let Some(name) = suggested.file_name().and_then(|n| n.to_str()) {
        dialog = dialog.set_file_name(name);
    }
    if let Some(parent) = suggested.parent() {
        if parent.is_dir() {
            dialog = dialog.set_directory(parent);
        }
    }
    let path = dialog.save_file()?;
    // A file with no extension would have no container, and the export would
    // refuse it; carrying the one already chosen over is what the user meant.
    Some(match path.extension() {
        Some(_) => path,
        None => {
            path.with_extension(suggested.extension().and_then(|e| e.to_str()).unwrap_or("mp4"))
        }
    })
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

/// Extensions offered in the caption dialogs.
const CAPTION_EXTENSIONS: &[&str] = &["srt", "vtt"];

pub fn pick_caption_file_to_open() -> Option<PathBuf> {
    rfd::FileDialog::new()
        .add_filter("Captions", CAPTION_EXTENSIONS)
        .set_title("Import captions")
        .pick_file()
}

/// Where a caption track should be written.
///
/// The suggested name carries the track's language, because that is the
/// convention a player reads to find the right file — `film.pt-BR.srt`.
pub fn pick_caption_file_to_save(suggested: &str) -> Option<PathBuf> {
    let path = rfd::FileDialog::new()
        .add_filter("Captions", CAPTION_EXTENSIONS)
        .set_file_name(format!("{suggested}.srt"))
        .set_title("Export captions")
        .save_file()?;
    // Without an extension there would be no format to write, so the default
    // is the one almost everything reads.
    Some(match path.extension() {
        Some(_) => path,
        None => path.with_extension("srt"),
    })
}
