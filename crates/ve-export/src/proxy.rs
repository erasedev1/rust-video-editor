//! Building proxies: smaller stand-ins for the pictures being cut with.
//!
//! # What a proxy is for
//!
//! Not disk space — a proxy adds to it. What a proxy buys is **latency**. A
//! frame of 4K H.264 is sixteen times the pixels of the same frame at a
//! quarter, and reaching one frame in the middle of a long group of pictures
//! costs every frame before it. Editing is one frame in the middle, over and
//! over, as fast as a hand can drag.
//!
//! So a proxy is written **all-intra**: every frame a keyframe, so no frame is
//! ever decoded twice to reach another. That is why editing formats are
//! intra-frame, and it is the larger half of what makes scrubbing feel
//! attached to the mouse. The smaller picture is the other half.
//!
//! # A proxy is a picture, not a file
//!
//! It carries no sound. The mixer and the waveforms read the original whether
//! or not a proxy exists, which means building one never re-encodes audio and
//! there are never two sound tracks that could drift apart. There is no code
//! here that keeps them in step because there is nothing to keep in step.
//!
//! # The frame at an instant must be the same frame
//!
//! A proxy that showed a different frame from its original at the same instant
//! would be worse than no proxy at all: the cut would be made against one
//! picture and delivered from another. So source presentation times are
//! **preserved** rather than recounted from zero — see
//! [`MediaWriter::write_frame_at`]. For constant-rate footage the two are the
//! same number; for variable-rate footage they are not, and recounting would
//! silently re-time the file.

use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, Receiver, TryRecvError};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use ve_core::{AssetId, MediaAsset, ProxyMedia, Size};
use ve_media::VideoDecoder;
use ve_metrics::Metrics;
use ve_time::Rate;

use crate::job::Cancel;
use crate::settings::{ExportSettings, Keyframes, Quality, VideoCodec};
use crate::writer::MediaWriter;
use crate::ExportError;

/// How often a running build reports progress. The same interval an export
/// uses: fast enough to move a bar, slow enough not to cost more than the work.
const PROGRESS_INTERVAL: Duration = Duration::from_millis(100);

/// The smallest a proxy is allowed to get on its shorter axis.
///
/// An eighth of a phone video is a postage stamp that no one can cut against,
/// and the decode it saves is already down in the noise. The scale is a request
/// rather than an instruction for this reason.
const MIN_HEIGHT: u32 = 180;

/// How much smaller a proxy is than its source.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ProxyScale {
    Half,
    #[default]
    Quarter,
    Eighth,
}

impl ProxyScale {
    pub fn label(self) -> &'static str {
        match self {
            ProxyScale::Half => "Half",
            ProxyScale::Quarter => "Quarter",
            ProxyScale::Eighth => "Eighth",
        }
    }

    pub const ALL: [ProxyScale; 3] =
        [ProxyScale::Half, ProxyScale::Quarter, ProxyScale::Eighth];

    fn divisor(self) -> u32 {
        match self {
            ProxyScale::Half => 2,
            ProxyScale::Quarter => 4,
            ProxyScale::Eighth => 8,
        }
    }

    /// The proxy size this scale means for a source of `size`.
    ///
    /// Both axes come out **even**, because every codec here subsamples chroma
    /// and cannot represent an odd one — and rounded rather than truncated, so
    /// the aspect ratio stays as close to the source's as an even size allows.
    /// A source small enough that the scale would take it below [`MIN_HEIGHT`]
    /// is scaled less, rather than being shrunk to something unusable.
    pub fn apply(self, size: Size) -> Size {
        if size.width == 0 || size.height == 0 {
            return size;
        }
        for divisor in (1..=self.divisor()).rev() {
            let height = even(size.height / divisor);
            if height >= MIN_HEIGHT || divisor == 1 {
                let width = even(
                    ((size.width as u64 * height as u64) as f64 / size.height as f64).round()
                        as u32,
                );
                return Size::new(width.max(2), height.max(2));
            }
        }
        size
    }
}

/// Rounds to the nearest even number, never to zero.
fn even(n: u32) -> u32 {
    if n < 2 {
        return 2;
    }
    n + (n % 2)
}

/// What to build, and from what.
#[derive(Debug, Clone, PartialEq)]
pub struct ProxySettings {
    /// The file being stood in for. Never written to.
    pub source: PathBuf,
    /// Where the proxy is written.
    pub path: PathBuf,
    pub size: Size,
    /// The rate the proxy declares. The source's own, so an instant maps to the
    /// same frame in both files.
    pub rate: Rate,
    /// How long the source runs, for the progress bar and nothing else.
    pub duration: ve_time::Ticks,
    pub codec: VideoCodec,
    pub quality: Quality,
}

impl ProxySettings {
    /// Settings to build a proxy of `asset` into `dir`.
    ///
    /// Returns `None` for an asset with no video stream: there is no picture to
    /// stand in for, and the sound is read from the original anyway.
    pub fn for_asset(asset: &MediaAsset, dir: &Path, scale: ProxyScale) -> Option<Self> {
        let video = asset.info.video.as_ref()?;
        Some(ProxySettings {
            source: asset.path.clone(),
            path: dir.join(proxy_file_name(asset)),
            size: scale.apply(video.size),
            rate: video.rate,
            duration: video.duration.max(asset.info.duration),
            // H.264 all-intra rather than ProRes: a quarter-size intra H.264
            // is a fraction of the size for a picture that is being cut
            // against rather than graded, and every build of FFmpeg has it.
            codec: VideoCodec::H264,
            // Deliberately not Draft. An intra-only picture spends its bits on
            // every frame instead of on the differences between them, so the
            // bits-per-pixel that looks fine long-GOP looks like a mosaic here.
            // At a quarter of the pixels this is still a fraction of what the
            // original costs.
            quality: Quality::High,
        })
    }

    /// The file this proxy will be written into, as the writer needs it.
    fn export_settings(&self) -> ExportSettings {
        ExportSettings {
            path: self.path.clone(),
            range: crate::settings::ExportRange::Whole,
            resolution: self.size,
            rate: self.rate,
            video: self.codec,
            quality: self.quality,
            // A proxy is a picture, not a file.
            audio: None,
            // The whole point.
            keyframes: Keyframes::EveryFrame,
            // A proxy stands in for one file's picture. Captions belong to the
            // sequence, and a folder of them beside every proxy would be
            // meaningless as well as a surprise.
            captions: None,
        }
    }

    /// What the model records once this has been built.
    pub fn media(&self) -> ProxyMedia {
        ProxyMedia::new(self.path.clone(), self.size)
    }
}

/// The name a proxy is written under.
///
/// Derived from the asset's own file name and its id, so two files called
/// `A001.mp4` from different cards do not write over each other's proxy in a
/// shared directory. `.mov` because it holds all-intra H.264 without complaint.
fn proxy_file_name(asset: &MediaAsset) -> String {
    let stem = asset
        .path
        .file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_else(|| "media".into());
    format!("{stem}.{}.proxy.mov", asset.id.raw())
}

/// The conventional place to put a project's proxies: beside the project file.
pub fn proxy_dir_for(project_path: &Path) -> PathBuf {
    let parent = project_path.parent().unwrap_or(Path::new("."));
    parent.join("Proxies")
}

/// What building one proxy did.
#[derive(Debug, Clone, PartialEq)]
pub struct ProxyReport {
    pub path: PathBuf,
    pub size: Size,
    pub frames: u64,
    pub bytes: u64,
    pub elapsed: Duration,
    pub encoder: String,
}

/// How far a batch of proxies has got.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ProxyProgress {
    pub asset: AssetId,
    /// Which file of the batch, counting from one.
    pub item: usize,
    pub items: usize,
    pub frames_done: u64,
    /// An estimate: a container that does not state its frame count leaves this
    /// derived from the duration, which a variable-frame-rate file will beat.
    pub frames_total: u64,
    pub elapsed: Duration,
}

impl ProxyProgress {
    /// How much of the **batch** is done, from 0 to 1.
    ///
    /// Files are counted as equal shares rather than weighted by length: the
    /// lengths are known but the cost per frame is not, and a bar that jumped
    /// about as it crossed from a short 4K file to a long HD one would be less
    /// honest than one that simply counts files.
    pub fn fraction(&self) -> f32 {
        if self.items == 0 {
            return 0.0;
        }
        let within = if self.frames_total == 0 {
            0.0
        } else {
            (self.frames_done as f32 / self.frames_total as f32).clamp(0.0, 1.0)
        };
        ((self.item.saturating_sub(1) as f32 + within) / self.items as f32).clamp(0.0, 1.0)
    }
}

/// Something a proxy build said.
#[derive(Debug, Clone)]
pub enum ProxyEvent {
    Progress(ProxyProgress),
    /// One file is done and its proxy is ready to be attached to the asset.
    Built {
        asset: AssetId,
        media: ProxyMedia,
        report: Box<ProxyReport>,
    },
    /// One file could not be built. The batch carries on with the rest: a card
    /// with one unreadable clip on it should still get proxies for the others.
    Failed {
        asset: AssetId,
        message: String,
    },
    /// Every file in the batch has been dealt with, built or failed.
    Finished,
    Cancelled,
}

/// Builds one proxy, start to finish.
///
/// Blocking, and driven by the caller's thread — [`ProxyJob`] is what puts it
/// on one of its own. Returns [`ExportError::Cancelled`] if `cancel` is raised,
/// having removed the part-written file.
pub fn build(
    settings: &ProxySettings,
    metrics: &Metrics,
    cancel: &Cancel,
    mut progress: impl FnMut(u64),
) -> Result<ProxyReport, ExportError> {
    let started = Instant::now();

    // Decoded straight to the proxy's size. Scaling in the decoder rather than
    // after it is most of the saving: swscale converts and resizes in one pass,
    // and nothing full-size is ever allocated.
    let mut decoder = VideoDecoder::open_scaled(&settings.source, Some(settings.size))
        .map_err(|e| ExportError::Render(format!("{}: {e}", settings.source.display())))?;

    let export = settings.export_settings();
    let mut writer = MediaWriter::create(&export, settings.size)?;
    let encoder = writer.video_encoder_name().to_string();

    let mut frames = 0u64;
    let mut last_report = Instant::now();
    // swscale allocates aligned rows, so a decoded frame's stride can exceed
    // the width the writer expects tightly packed. Repacked into one buffer
    // reused for the whole file rather than allocated per frame.
    let mut packed: Vec<u8> = Vec::new();

    loop {
        if cancel.is_cancelled() {
            abandon(writer, &settings.path);
            return Err(ExportError::Cancelled);
        }

        let frame = match decoder.next_frame() {
            Ok(Some(frame)) => frame,
            Ok(None) => break,
            Err(e) => {
                abandon(writer, &settings.path);
                return Err(ExportError::Render(format!("{}: {e}", settings.source.display())));
            }
        };

        // The source's own time, in frames of the proxy's rate. Preserved
        // rather than counted, so the proxy shows the same frame at the same
        // instant as the file it stands in for.
        let pts = settings.rate.ticks_to_frame(frame.pts());
        let rgba = tightly_packed(&frame, &mut packed);
        {
            let _span = metrics.span(ve_metrics::spans::ENCODE);
            writer.write_frame_at(rgba, pts)?;
        }

        frames += 1;
        if last_report.elapsed() >= PROGRESS_INTERVAL {
            last_report = Instant::now();
            progress(frames);
        }
    }

    if frames == 0 {
        abandon(writer, &settings.path);
        return Err(ExportError::Render(format!(
            "{} yielded no frames to build a proxy from",
            settings.source.display()
        )));
    }

    writer.finish()?;
    progress(frames);

    let bytes = std::fs::metadata(&settings.path).map(|m| m.len()).unwrap_or(0);
    Ok(ProxyReport {
        path: settings.path.clone(),
        size: settings.size,
        frames,
        bytes,
        elapsed: started.elapsed(),
        encoder,
    })
}

/// The frame's pixels with no row padding, borrowed where they already are.
///
/// A copy only when swscale's alignment made the rows wider than the picture,
/// which is what the encoder's own row copy assumes they are not.
fn tightly_packed<'a>(frame: &'a ve_media::VideoFrame, scratch: &'a mut Vec<u8>) -> &'a [u8] {
    if frame.is_tightly_packed() {
        return frame.data();
    }
    let tight = frame.width() as usize * 4;
    scratch.clear();
    scratch.reserve(tight * frame.height() as usize);
    for y in 0..frame.height() {
        scratch.extend_from_slice(&frame.row(y)[..tight]);
    }
    scratch
}

/// Closes a writer without its trailer and removes the file.
///
/// The same rule a cancelled export follows: what is on disk is a container
/// with no index, and leaving it would mean the editor finding it later and
/// taking it for a finished proxy.
fn abandon(writer: MediaWriter, path: &Path) {
    drop(writer);
    if path.exists() {
        if let Err(e) = std::fs::remove_file(path) {
            log::warn!("could not remove the abandoned proxy {}: {e}", path.display());
        }
    }
}

/// A batch of proxies building on its own thread.
///
/// Held by the editor, drained once a repaint. Nothing about it blocks the
/// interface thread — building proxies for a card of footage is measured in
/// minutes, and the whole point is to keep cutting while it happens.
pub struct ProxyJob {
    events: Receiver<ProxyEvent>,
    cancel: Cancel,
    handle: Option<JoinHandle<()>>,
    progress: ProxyProgress,
    finished: bool,
}

impl ProxyJob {
    /// Starts building `items` in the order given.
    pub fn start(
        items: Vec<(AssetId, ProxySettings)>,
        metrics: Metrics,
        dir: PathBuf,
    ) -> ProxyJob {
        let (tx, events) = mpsc::channel();
        let cancel = Cancel::new();
        let count = items.len();
        let first = items.first().map(|(id, _)| *id).unwrap_or(AssetId::from_raw(0));

        let handle = {
            let cancel = cancel.clone();
            std::thread::Builder::new()
                .name("verge-proxy".into())
                .spawn(move || {
                    if let Err(e) = std::fs::create_dir_all(&dir) {
                        for (asset, _) in &items {
                            let _ = tx.send(ProxyEvent::Failed {
                                asset: *asset,
                                message: format!("{}: {e}", dir.display()),
                            });
                        }
                        let _ = tx.send(ProxyEvent::Finished);
                        return;
                    }

                    for (index, (asset, settings)) in items.iter().enumerate() {
                        if cancel.is_cancelled() {
                            let _ = tx.send(ProxyEvent::Cancelled);
                            return;
                        }

                        let started = Instant::now();
                        let total = estimated_frames(settings);
                        let report = {
                            let tx = tx.clone();
                            build(settings, &metrics, &cancel, |frames| {
                                let _ = tx.send(ProxyEvent::Progress(ProxyProgress {
                                    asset: *asset,
                                    item: index + 1,
                                    items: count,
                                    frames_done: frames,
                                    frames_total: total,
                                    elapsed: started.elapsed(),
                                }));
                            })
                        };

                        match report {
                            Ok(report) => {
                                let _ = tx.send(ProxyEvent::Built {
                                    asset: *asset,
                                    media: settings.media(),
                                    report: Box::new(report),
                                });
                            }
                            Err(ExportError::Cancelled) => {
                                let _ = tx.send(ProxyEvent::Cancelled);
                                return;
                            }
                            // One bad file does not stop the batch: a card with
                            // a single unreadable clip on it should still get
                            // proxies for everything else.
                            Err(e) => {
                                let _ = tx.send(ProxyEvent::Failed {
                                    asset: *asset,
                                    message: e.to_string(),
                                });
                            }
                        }
                    }
                    let _ = tx.send(ProxyEvent::Finished);
                })
                .expect("spawning the proxy thread")
        };

        ProxyJob {
            events,
            cancel,
            handle: Some(handle),
            progress: ProxyProgress {
                asset: first,
                item: 1,
                items: count,
                frames_done: 0,
                frames_total: 0,
                elapsed: Duration::ZERO,
            },
            finished: false,
        }
    }

    /// Everything the build has said since the last call. Never blocks.
    pub fn poll(&mut self) -> Vec<ProxyEvent> {
        let mut events = Vec::new();
        loop {
            match self.events.try_recv() {
                Ok(event) => {
                    match &event {
                        ProxyEvent::Progress(p) => self.progress = *p,
                        ProxyEvent::Finished | ProxyEvent::Cancelled => self.finished = true,
                        _ => {}
                    }
                    events.push(event);
                }
                Err(TryRecvError::Empty) => break,
                Err(TryRecvError::Disconnected) => {
                    self.finished = true;
                    break;
                }
            }
        }
        events
    }

    pub fn cancel(&self) {
        self.cancel.cancel();
    }

    pub fn is_cancelling(&self) -> bool {
        self.cancel.is_cancelled() && !self.finished
    }

    pub fn is_running(&self) -> bool {
        !self.finished
    }

    pub fn progress(&self) -> ProxyProgress {
        self.progress
    }

    pub fn join(&mut self) {
        if let Some(handle) = self.handle.take() {
            let _ = handle.join();
        }
        self.finished = true;
    }
}

impl Drop for ProxyJob {
    fn drop(&mut self) {
        // Asked to stop and then waited for, so the file it was writing is
        // removed rather than left half-muxed under a name the editor would
        // later take for a finished proxy.
        self.cancel.cancel();
        self.join();
    }
}

/// How many frames a source is expected to yield, for the progress bar.
///
/// Derived from the duration, which a variable-frame-rate file will not match
/// exactly. Only ever an estimate — which is why it moves a bar and decides
/// nothing: the loop runs until the decoder says there are no more frames, not
/// until this many have gone by.
fn estimated_frames(settings: &ProxySettings) -> u64 {
    settings.rate.duration_to_frames(settings.duration).max(0) as u64
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_scale_keeps_the_aspect_ratio_and_both_axes_even() {
        let uhd = Size::new(3840, 2160);
        assert_eq!(ProxyScale::Half.apply(uhd), Size::new(1920, 1080));
        assert_eq!(ProxyScale::Quarter.apply(uhd), Size::new(960, 540));
        assert_eq!(ProxyScale::Eighth.apply(uhd), Size::new(480, 270));

        // 1080 / 8 is 135, which is odd, so the even rule has to bite.
        let hd = Size::new(1920, 1080);
        let eighth = ProxyScale::Eighth.apply(hd);
        assert!(
            eighth.width.is_multiple_of(2) && eighth.height.is_multiple_of(2),
            "{eighth:?}"
        );
    }

    /// Both axes even is a hard requirement of every codec here, so it is
    /// asserted for a spread of real and awkward sizes rather than one.
    #[test]
    fn every_scale_of_every_size_is_even_on_both_axes() {
        let sizes = [
            Size::new(3840, 2160),
            Size::new(1920, 1080),
            Size::new(1280, 720),
            Size::new(1440, 1080), // anamorphic
            Size::new(1079, 607),  // odd on both axes
            Size::new(640, 360),
            Size::new(320, 240),
            Size::new(64, 64),
        ];
        for size in sizes {
            for scale in ProxyScale::ALL {
                let out = scale.apply(size);
                assert!(out.width.is_multiple_of(2), "{size:?} at {}: {out:?}", scale.label());
                assert!(out.height.is_multiple_of(2), "{size:?} at {}: {out:?}", scale.label());
                assert!(out.width >= 2 && out.height >= 2, "{size:?} -> {out:?}");
                assert!(
                    out.width <= size.width.max(2) && out.height <= size.height.max(2),
                    "a proxy must not be larger than its source: {size:?} -> {out:?}"
                );
            }
        }
    }

    #[test]
    fn a_small_source_is_scaled_less_rather_than_into_a_postage_stamp() {
        // An eighth of this would be 40 lines, which nobody can cut against.
        let small = Size::new(640, 360);
        let out = ProxyScale::Eighth.apply(small);
        assert!(out.height >= MIN_HEIGHT || out.height == small.height, "{out:?}");
    }

    #[test]
    fn keyframes_say_how_often_rather_than_how_many() {
        assert_eq!(Keyframes::EveryFrame.gop(Rate::FPS_30), 1);
        assert_eq!(Keyframes::EverySecond.gop(Rate::FPS_30), 30);
        assert_eq!(Keyframes::EverySecond.gop(Rate::FPS_25), 25);
        // 29.97 rounds to 30 rather than to 29, which would put a keyframe a
        // frame early every second for the life of the file.
        assert_eq!(Keyframes::EverySecond.gop(Rate::FPS_29_97), 30);
    }

    #[test]
    fn a_batch_counts_files_as_equal_shares() {
        let p = |item, done, total| ProxyProgress {
            asset: AssetId::from_raw(1),
            item,
            items: 4,
            frames_done: done,
            frames_total: total,
            elapsed: Duration::ZERO,
        };
        assert_eq!(p(1, 0, 100).fraction(), 0.0);
        assert_eq!(p(1, 50, 100).fraction(), 0.125);
        assert_eq!(p(3, 0, 100).fraction(), 0.5);
        assert_eq!(p(4, 100, 100).fraction(), 1.0);
        // A file whose length is unknown contributes nothing until it is done,
        // rather than dividing by zero.
        assert_eq!(p(2, 10, 0).fraction(), 0.25);
    }

    #[test]
    fn a_proxy_is_named_so_two_cards_cannot_collide() {
        use ve_core::MediaInfo;
        let a = MediaAsset::new(AssetId::from_raw(1), "/card1/A001.mp4", MediaInfo::default());
        let b = MediaAsset::new(AssetId::from_raw(2), "/card2/A001.mp4", MediaInfo::default());
        assert_ne!(proxy_file_name(&a), proxy_file_name(&b));
        assert!(proxy_file_name(&a).starts_with("A001."));
        assert!(proxy_file_name(&a).ends_with(".proxy.mov"));
    }
}
