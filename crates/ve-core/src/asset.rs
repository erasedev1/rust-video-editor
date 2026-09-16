use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use ve_time::{Rate, SampleRate, Ticks};

use crate::geometry::Size;
use crate::id::AssetId;

/// What a media file's video stream contains.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct VideoStreamInfo {
    pub size: Size,
    /// Average frame rate. Variable-frame-rate sources still report one here;
    /// their real frame boundaries come from packet timestamps at decode time.
    pub rate: Rate,
    pub duration: Ticks,
    /// Frame count when the container states it, `None` when it must be
    /// estimated or the source is variable-frame-rate.
    pub frame_count: Option<i64>,
    pub codec: String,
    pub pixel_format: String,
    /// Non-square pixel support: `(num, den)`, `(1, 1)` for square pixels.
    pub sample_aspect_ratio: (u32, u32),
}

impl VideoStreamInfo {
    /// The display size once the pixel aspect ratio is applied.
    pub fn display_size(&self) -> Size {
        let (num, den) = self.sample_aspect_ratio;
        if num == den || num == 0 || den == 0 {
            return self.size;
        }
        Size::new(((self.size.width as u64 * num as u64) / den as u64) as u32, self.size.height)
    }
}

/// What a media file's audio stream contains.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AudioStreamInfo {
    pub sample_rate: SampleRate,
    pub channels: u16,
    pub duration: Ticks,
    pub codec: String,
}

/// Everything probing a file told us about it.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct MediaInfo {
    /// Container duration, which may exceed either stream's own duration.
    pub duration: Ticks,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub video: Option<VideoStreamInfo>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub audio: Option<AudioStreamInfo>,
    #[serde(default)]
    pub container: String,
}

impl MediaInfo {
    pub fn has_video(&self) -> bool {
        self.video.is_some()
    }

    pub fn has_audio(&self) -> bool {
        self.audio.is_some()
    }
}

/// A reference to source media on disk.
///
/// The asset never owns pixel or sample data: it is a handle plus the metadata
/// needed to lay clips out on a timeline. This is the root of the editor's
/// non-destructive guarantee — nothing in the project model can modify the file
/// this points at.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct MediaAsset {
    pub id: AssetId,
    /// Display name, defaulting to the file stem but user-editable.
    pub name: String,
    /// Absolute path as last resolved. Written to the project file alongside
    /// [`Self::relative_path`] so a project can be relinked if either the
    /// project or the media moves.
    pub path: PathBuf,
    /// Path relative to the project file, when the media sits under the same
    /// root. Preferred on load, which is what makes projects portable.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub relative_path: Option<PathBuf>,
    pub info: MediaInfo,
    /// Set when the file could not be found on load. The project still opens;
    /// clips referencing it render as offline until it is relinked.
    #[serde(default, skip_serializing)]
    pub offline: bool,
}

impl MediaAsset {
    pub fn new(id: AssetId, path: impl Into<PathBuf>, info: MediaInfo) -> Self {
        let path = path.into();
        let name = path
            .file_name()
            .map(|s| s.to_string_lossy().into_owned())
            .unwrap_or_else(|| "Untitled".to_string());
        MediaAsset { id, name, path, relative_path: None, info, offline: false }
    }

    /// Records where this file sits relative to the project directory, if it
    /// sits under it at all.
    pub fn relink_relative_to(&mut self, project_dir: &Path) {
        self.relative_path = pathdiff(&self.path, project_dir);
    }

    /// Resolves the path to use when opening the file, preferring the relative
    /// form so that a moved project folder still finds its media.
    pub fn resolve_path(&self, project_dir: Option<&Path>) -> PathBuf {
        if let (Some(dir), Some(rel)) = (project_dir, &self.relative_path) {
            let candidate = dir.join(rel);
            if candidate.exists() {
                return candidate;
            }
        }
        self.path.clone()
    }

    pub fn duration(&self) -> Ticks {
        self.info.duration
    }
}

/// Returns `path` expressed relative to `base`, but only when `path` is beneath
/// `base`. Deliberately refuses to emit `../` chains: a project that reaches
/// outside its own directory is better off keeping the absolute path, which
/// stays correct if the project folder alone is moved.
fn pathdiff(path: &Path, base: &Path) -> Option<PathBuf> {
    path.strip_prefix(base).ok().map(|p| p.to_path_buf())
}
