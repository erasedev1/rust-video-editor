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

/// A smaller stand-in for an asset's picture.
///
/// A proxy is a **picture, not a file**: the original's sound is what the mixer
/// and the waveforms read whether or not one of these exists. That is why there
/// is no sample rate or channel count here, and why generating one never has to
/// re-encode audio or keep two sound tracks in step.
///
/// Stored with the same absolute-and-relative pair as the asset itself, so
/// moving a project folder that carries its proxies alongside its media finds
/// both again.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ProxyMedia {
    pub path: PathBuf,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub relative_path: Option<PathBuf>,
    /// The size frames come out of the proxy at, which is what makes a proxy
    /// worth having and what keeps its frames out of the full-resolution
    /// cache entries.
    pub size: Size,
}

impl ProxyMedia {
    pub fn new(path: impl Into<PathBuf>, size: Size) -> Self {
        ProxyMedia { path: path.into(), relative_path: None, size }
    }

    fn resolve_path(&self, project_dir: Option<&Path>) -> PathBuf {
        if let (Some(dir), Some(rel)) = (project_dir, &self.relative_path) {
            let candidate = dir.join(rel);
            if candidate.exists() {
                return candidate;
            }
        }
        self.path.clone()
    }
}

/// Which file the editor decodes an asset's picture from, and whether that file
/// is a proxy.
///
/// Returned as one value rather than resolved at each call site because the
/// rule has exactly one subtle case — a proxy whose file has gone — and two
/// copies of it would eventually disagree about whether the editor and the
/// cache were looking at the same picture.
#[derive(Debug, Clone, PartialEq)]
pub struct PictureSource {
    pub path: PathBuf,
    pub is_proxy: bool,
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
    /// A smaller stand-in for this asset's picture, built by the editor and
    /// used in place of the original while proxies are switched on.
    ///
    /// Defaulted on read, so a project written before proxies existed opens
    /// with none rather than needing a format version of its own.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub proxy: Option<ProxyMedia>,
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
        MediaAsset { id, name, path, relative_path: None, info, proxy: None, offline: false }
    }

    /// Records where this file sits relative to the project directory, if it
    /// sits under it at all.
    ///
    /// A proxy is relinked alongside its asset rather than separately: the two
    /// travel together, and a proxy that kept an absolute path while its
    /// original went relative would be the one file a moved project could not
    /// find.
    pub fn relink_relative_to(&mut self, project_dir: &Path) {
        self.relative_path = pathdiff(&self.path, project_dir);
        if let Some(proxy) = &mut self.proxy {
            proxy.relative_path = pathdiff(&proxy.path, project_dir);
        }
    }

    pub fn has_proxy(&self) -> bool {
        self.proxy.is_some()
    }

    /// Which file to decode this asset's picture from.
    ///
    /// **A missing proxy is not a missing asset.** A proxy is a convenience the
    /// editor built for itself, so one that has been deleted — a cleared cache
    /// directory, a project moved without it — falls back to the original and
    /// carries on at full resolution. Going offline over it would lose the
    /// user's footage because the editor lost its own scratch file.
    pub fn picture_source(
        &self,
        project_dir: Option<&Path>,
        proxies_enabled: bool,
    ) -> PictureSource {
        if proxies_enabled {
            if let Some(proxy) = &self.proxy {
                let path = proxy.resolve_path(project_dir);
                if path.exists() {
                    return PictureSource { path, is_proxy: true };
                }
            }
        }
        PictureSource { path: self.resolve_path(project_dir), is_proxy: false }
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
