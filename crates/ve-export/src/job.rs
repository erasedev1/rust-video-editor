//! Running an export: the loop, and the thread it runs on.
//!
//! # The loop
//!
//! Every frame of the range, in order: resolve the instant into a plan, decode
//! what it draws, composite it, read the picture back, and encode it — then mix
//! exactly that frame's worth of sound and encode that too.
//!
//! Each step is the editor's own. The plan comes from the same `evaluate` the
//! preview uses, the picture from the same compositor, the sound from the same
//! mixer. An export is therefore not a second implementation of the editor: it
//! is the editor, run as fast as it will go with nobody watching.
//!
//! # Sound is counted in samples, not in frames
//!
//! How much audio belongs to one video frame is not a constant — 48000 samples
//! do not divide 1001/30000 evenly — so each frame's block is the difference
//! between two **absolute** sample indices rather than a fixed count. Over an
//! hour that is the difference between sound that stays in sync and sound that
//! ends up a frame and a half late.
//!
//! # Cancelling
//!
//! A cancelled export deletes what it had written. A partial MP4 has no index —
//! the muxer writes that last — so what would be left behind is a file that
//! looks like a deliverable and plays as nothing at all.

use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver, TryRecvError};
use std::sync::Arc;
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use ve_core::{Project, SequenceId};
use ve_engine::{evaluate_project, AudioRenderer, EngineUpdate};
use ve_metrics::{spans, Metrics};
use ve_render::GpuContext;
use ve_time::Ticks;

use crate::compose::FrameComposer;
use crate::settings::ExportSettings;
use crate::sources::SourceFrames;
use crate::writer::MediaWriter;
use crate::ExportError;

/// Frames uploaded for export, in megabytes.
///
/// Smaller than the preview's budget: an export walks forward and almost never
/// asks for a frame twice, so this only has to hold the handful of layers one
/// instant draws rather than a scrubbing history.
const TEXTURE_BUDGET_MB: usize = 128;

/// Composited pictures held during an export, in megabytes.
///
/// Each frame is a fresh key, so this is a *pool* rather than a cache: what it
/// buys is that a ten-minute render allocates its render targets once instead
/// of eighteen thousand times.
const COMPOSITE_BUDGET_MB: usize = 128;

/// How often a running export reports progress.
///
/// Fast enough that a progress bar moves smoothly, slow enough that a cheap
/// frame does not spend more time waking the interface than rendering.
const PROGRESS_INTERVAL: Duration = Duration::from_millis(100);

/// How far an export has got.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Progress {
    pub frames_done: u64,
    pub frames_total: u64,
    pub elapsed: Duration,
}

impl Progress {
    /// How much of the export is done, from 0 to 1.
    pub fn fraction(&self) -> f32 {
        if self.frames_total == 0 {
            return 0.0;
        }
        (self.frames_done as f32 / self.frames_total as f32).clamp(0.0, 1.0)
    }

    pub fn frames_per_second(&self) -> f64 {
        let seconds = self.elapsed.as_secs_f64();
        if seconds <= 0.0 {
            return 0.0;
        }
        self.frames_done as f64 / seconds
    }

    /// An estimate of what is left, from the rate so far.
    ///
    /// From the whole run rather than from the last few frames: a timeline's
    /// cost varies clip by clip, and an estimate that chased the current frame
    /// would swing wildly every time the export crossed a cut.
    pub fn remaining(&self) -> Option<Duration> {
        let fps = self.frames_per_second();
        if fps <= 0.0 || self.frames_done == 0 {
            return None;
        }
        let left = self.frames_total.saturating_sub(self.frames_done) as f64;
        Some(Duration::from_secs_f64(left / fps))
    }
}

/// What an export did.
#[derive(Debug, Clone, PartialEq)]
pub struct ExportReport {
    pub path: PathBuf,
    pub frames: u64,
    /// The span of timeline that was written.
    pub duration: Ticks,
    pub elapsed: Duration,
    pub bytes: u64,
    pub video_encoder: String,
    pub audio_encoder: Option<String>,
    /// Caption files written beside the delivery, one per caption track that
    /// had anything to say inside the exported range.
    pub captions: Vec<PathBuf>,
    /// Layers that should have been drawn and were not, because their media
    /// would not decode. Reported rather than swallowed: a delivery missing a
    /// layer is worse than an export that says it went wrong.
    pub missing_frames: u64,
    /// Samples the mix limited on the way out, which is the export's own
    /// version of the meter's over-level warning.
    pub clipped_samples: u64,
    /// The loudest sample in the whole mix, before limiting, as full scale.
    pub peak: f32,
    pub problems: Vec<String>,
}

impl ExportReport {
    pub fn frames_per_second(&self) -> f64 {
        let seconds = self.elapsed.as_secs_f64();
        if seconds <= 0.0 {
            return 0.0;
        }
        self.frames as f64 / seconds
    }

    /// How much faster than real time the export ran, if it ran at all.
    pub fn realtime_factor(&self, rate: ve_time::Rate) -> f64 {
        let fps = self.frames_per_second();
        if rate.as_f64() <= 0.0 {
            return 0.0;
        }
        fps / rate.as_f64()
    }

    /// One line for the status bar.
    pub fn summary(&self) -> String {
        let mut line = format!(
            "exported {} frames to {} ({}, {:.1} s, {:.1} fps)",
            self.frames,
            self.path.display(),
            file_size(self.bytes),
            self.elapsed.as_secs_f64(),
            self.frames_per_second(),
        );
        if self.missing_frames > 0 {
            line.push_str(&format!(" — {} layers could not be decoded", self.missing_frames));
        }
        if !self.captions.is_empty() {
            line.push_str(&format!(
                " — {} caption file{}",
                self.captions.len(),
                if self.captions.len() == 1 { "" } else { "s" }
            ));
        }
        if self.clipped_samples > 0 {
            line.push_str(&format!(" — {} samples clipped", self.clipped_samples));
        }
        line
    }
}

/// Something a running export has to say.
#[derive(Debug, Clone, PartialEq)]
pub enum ExportEvent {
    Progress(Progress),
    Finished(Box<ExportReport>),
    Failed(String),
    Cancelled,
}

/// A handle that asks a running export to stop.
///
/// Clonable and cheap: the export thread reads it once a frame, so cancelling
/// takes effect within one frame rather than at the end of the render.
#[derive(Debug, Clone, Default)]
pub struct Cancel(Arc<AtomicBool>);

impl Cancel {
    pub fn new() -> Self {
        Cancel(Arc::new(AtomicBool::new(false)))
    }

    pub fn cancel(&self) {
        self.0.store(true, Ordering::Relaxed);
    }

    pub fn is_cancelled(&self) -> bool {
        self.0.load(Ordering::Relaxed)
    }
}

/// Renders and encodes one sequence, start to finish, on the calling thread.
///
/// Blocking on purpose: this is the whole job, it has nothing else to do, and
/// keeping it a plain function is what lets a test drive an export without a
/// thread, a channel or a window. [`ExportJob`] is the thin wrapper that puts
/// it on a thread for the editor.
pub fn run(
    project: &Project,
    sequence: SequenceId,
    settings: &ExportSettings,
    gpu: &GpuContext,
    metrics: &Metrics,
    cancel: &Cancel,
    mut on_progress: impl FnMut(Progress),
) -> Result<ExportReport, ExportError> {
    let started = Instant::now();
    settings.validate()?;

    let sequence = project.sequence(sequence).ok_or(ExportError::NoSequence)?;
    let range = settings.range.resolve(sequence);
    let rate = settings.rate;
    let frames_total = rate.duration_to_frames(range.duration).max(0) as u64;
    if frames_total == 0 {
        return Err(ExportError::Settings("there is nothing in the range to export".into()));
    }

    // Composited at the sequence's own canvas and scaled on the way into the
    // encoder, so a half-size review copy is the same picture the editor showed
    // rather than a different composite with smaller masks and softer blurs.
    let canvas = sequence.settings.resolution;
    let mut composer = FrameComposer::new(
        &gpu.device,
        TEXTURE_BUDGET_MB,
        COMPOSITE_BUDGET_MB,
        metrics.clone(),
    );
    let mut sources = SourceFrames::new(project);
    let mut audio = settings.audio.map(|settings| {
        let mut renderer =
            AudioRenderer::new(settings.sample_rate, settings.channels, metrics.clone());
        for asset in &project.assets {
            if !asset.offline && asset.info.has_audio() {
                renderer.register_asset(asset.id, &asset.path);
            }
        }
        renderer.seek(range.start);
        renderer
    });

    let mut writer = MediaWriter::create(settings, canvas)?;
    let video_encoder = writer.video_encoder_name().to_string();
    let audio_encoder = writer.audio_encoder_name().map(str::to_string);

    let mut clipped_samples = 0u64;
    let mut peak = 0.0f32;
    // Where the sound has got to, counted from the start of the range in whole
    // samples so that no rounding ever accumulates.
    let mut samples_written: i64 = 0;
    let mut last_report = Instant::now();

    // Everything from here to the trailer is wrapped so that *any* way of not
    // finishing — a cancellation, a decoder that died, a disk that filled —
    // takes the half-written file with it. A container without its index plays
    // as nothing, and one sitting there under the name the user asked for is
    // worse than none at all.
    let rendered = (|| -> Result<(), ExportError> {
        for index in 0..frames_total {
            if cancel.is_cancelled() {
                return Err(ExportError::Cancelled);
            }

            let _span = metrics.span(spans::EXPORT_FRAME);
            let at = range.start + rate.frame_to_ticks(index as i64);

            let plan = evaluate_project(project, sequence, at);
            let nodes = sources.resolve(&plan);
            let update = EngineUpdate {
                position: at,
                plan,
                nodes,
                pending: 0,
                playing: false,
                reached_end: false,
            };

            let key = composer
                .compose(&gpu.device, &gpu.queue, &update, |item| sources.key_for(item))
                .ok_or_else(|| {
                    ExportError::Render("the compositor produced no picture".into())
                })?;
            let pixels = {
                let _span = metrics.span(spans::READBACK);
                let picture = composer.picture(&key).ok_or_else(|| {
                    ExportError::Render(
                        "the composited picture was evicted before it \
                                     could be read back"
                            .into(),
                    )
                })?;
                picture.read_pixels(&gpu.device, &gpu.queue)
            };

            {
                let _span = metrics.span(spans::ENCODE);
                writer.write_frame(&pixels)?;
            }

            if let Some(renderer) = audio.as_mut() {
                // This frame's sound is everything between the sample the last
                // frame ended on and the one this frame ends on — a difference of
                // absolute indices, so a rate that does not divide evenly spreads
                // the remainder across frames instead of losing it.
                let through = rate.frame_to_ticks(index as i64 + 1);
                let end = renderer.mixer().sample_rate().ticks_to_sample(through);
                let wanted = (end - samples_written).max(0) as usize;
                if wanted > 0 {
                    let block = renderer.mix_block(project, sequence, wanted);
                    writer.write_audio(block)?;
                    samples_written = end;
                }
                let stats = renderer.last_stats();
                clipped_samples += stats.clipped as u64;
                peak = peak.max(stats.master.peak());
            }

            let done = index + 1;
            if last_report.elapsed() >= PROGRESS_INTERVAL || done == frames_total {
                last_report = Instant::now();
                on_progress(Progress {
                    frames_done: done,
                    frames_total,
                    elapsed: started.elapsed(),
                });
            }
        }
        Ok(())
    })();

    if let Err(e) = rendered {
        abandon(writer, &settings.path);
        return Err(e);
    }
    if let Err(e) = writer.finish() {
        discard(&settings.path);
        return Err(e);
    }

    // After the trailer, never before: a cancelled export deletes its file, and
    // a folder of caption files beside a delivery that no longer exists would
    // be worse than none. The picture is finished, so these cannot fail the
    // export either — what goes wrong here is reported as a problem.
    let mut problems = sources.problems().to_vec();
    let captions = match settings.captions {
        Some(format) => {
            let (written, trouble) =
                crate::captions::write_sidecars(sequence, &settings.path, range, format);
            problems.extend(trouble);
            written
        }
        None => Vec::new(),
    };

    let bytes = std::fs::metadata(&settings.path).map(|m| m.len()).unwrap_or(0);
    Ok(ExportReport {
        path: settings.path.clone(),
        frames: frames_total,
        duration: range.duration,
        elapsed: started.elapsed(),
        bytes,
        video_encoder,
        audio_encoder,
        captions,
        missing_frames: sources.missing_frames(),
        clipped_samples,
        peak,
        problems,
    })
}

/// A file size in the unit a person would use for it.
///
/// A short test render is tens of kilobytes and a feature is gigabytes, and
/// "0.0 MB" for the first is the kind of small dishonesty that makes people stop
/// reading the status bar.
fn file_size(bytes: u64) -> String {
    const KB: f64 = 1024.0;
    const MB: f64 = KB * 1024.0;
    const GB: f64 = MB * 1024.0;
    let bytes = bytes as f64;
    if bytes >= GB {
        format!("{:.2} GB", bytes / GB)
    } else if bytes >= MB {
        format!("{:.1} MB", bytes / MB)
    } else if bytes >= KB {
        format!("{:.0} kB", bytes / KB)
    } else {
        format!("{bytes:.0} bytes")
    }
}

/// Closes a writer without its trailer and throws the file away.
///
/// What is on disk at that point is a container with no index — playable by
/// nothing — so it is removed rather than left under the name the user asked
/// for.
fn abandon(writer: MediaWriter, path: &std::path::Path) {
    drop(writer);
    discard(path);
}

/// Removes a file an export will not be finishing.
fn discard(path: &std::path::Path) {
    if let Err(e) = std::fs::remove_file(path) {
        log::warn!("could not remove the abandoned export {}: {e}", path.display());
    }
}

/// An export running on its own thread.
///
/// The interface holds one of these, drains its events once a repaint, and can
/// ask it to stop. Nothing about it blocks the interface thread — which is the
/// standing rule, and matters more here than anywhere: an export is the one
/// operation that can legitimately run for an hour.
pub struct ExportJob {
    events: Receiver<ExportEvent>,
    cancel: Cancel,
    handle: Option<JoinHandle<()>>,
    settings: ExportSettings,
    /// The last progress seen, so the interface can draw a bar without keeping
    /// its own copy of the last event.
    progress: Progress,
    finished: bool,
}

impl ExportJob {
    /// Starts an export in the background.
    ///
    /// The project is taken by `Arc` rather than borrowed: the editor goes on
    /// being edited while this runs, and an export has to write the project as
    /// it was when the button was pressed. Cloning the edit model costs one
    /// pass over the clips and no media at all.
    pub fn start(
        project: Arc<Project>,
        sequence: SequenceId,
        settings: ExportSettings,
        gpu: GpuContext,
        metrics: Metrics,
    ) -> ExportJob {
        let (tx, events) = mpsc::channel();
        let cancel = Cancel::new();
        let frames_total = project
            .sequence(sequence)
            .map(|s| settings.range.resolve(s))
            .map(|r| settings.rate.duration_to_frames(r.duration).max(0) as u64)
            .unwrap_or(0);

        let handle = {
            let settings = settings.clone();
            let cancel = cancel.clone();
            let tx = tx.clone();
            std::thread::Builder::new()
                .name("verge-export".into())
                .spawn(move || {
                    let progress = tx.clone();
                    let result =
                        run(&project, sequence, &settings, &gpu, &metrics, &cancel, |p| {
                            let _ = progress.send(ExportEvent::Progress(p));
                        });
                    let _ = tx.send(match result {
                        Ok(report) => ExportEvent::Finished(Box::new(report)),
                        Err(ExportError::Cancelled) => ExportEvent::Cancelled,
                        Err(e) => ExportEvent::Failed(e.to_string()),
                    });
                })
                .expect("spawning the export thread")
        };

        ExportJob {
            events,
            cancel,
            handle: Some(handle),
            settings,
            progress: Progress { frames_done: 0, frames_total, elapsed: Duration::ZERO },
            finished: false,
        }
    }

    /// Everything the export has said since the last call.
    ///
    /// Never blocks. A disconnected channel means the thread is gone, which is
    /// reported as finished rather than as an error: whatever it had to say
    /// arrived before it went.
    pub fn poll(&mut self) -> Vec<ExportEvent> {
        let mut events = Vec::new();
        loop {
            match self.events.try_recv() {
                Ok(event) => {
                    match &event {
                        ExportEvent::Progress(p) => self.progress = *p,
                        _ => self.finished = true,
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

    /// Whether the export is still going.
    pub fn is_running(&self) -> bool {
        !self.finished
    }

    pub fn progress(&self) -> Progress {
        self.progress
    }

    pub fn settings(&self) -> &ExportSettings {
        &self.settings
    }

    /// Waits for the thread to end, which it does shortly after being
    /// cancelled.
    pub fn join(&mut self) {
        if let Some(handle) = self.handle.take() {
            let _ = handle.join();
        }
        self.finished = true;
    }
}

impl Drop for ExportJob {
    fn drop(&mut self) {
        // A job dropped while running — the editor quitting mid-export — is
        // asked to stop and then waited for, so the file it was writing is
        // cleaned up rather than left half-muxed on disk.
        self.cancel.cancel();
        self.join();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn progress_reports_a_fraction_and_an_estimate() {
        let p =
            Progress { frames_done: 30, frames_total: 120, elapsed: Duration::from_secs(3) };
        assert_eq!(p.fraction(), 0.25);
        assert_eq!(p.frames_per_second(), 10.0);
        // 90 frames left at 10 a second.
        assert_eq!(p.remaining(), Some(Duration::from_secs(9)));
    }

    #[test]
    fn progress_has_no_estimate_before_the_first_frame() {
        let p = Progress { frames_done: 0, frames_total: 120, elapsed: Duration::ZERO };
        assert_eq!(p.fraction(), 0.0);
        assert_eq!(p.remaining(), None);
    }

    #[test]
    fn a_size_is_reported_in_the_unit_that_suits_it() {
        assert_eq!(file_size(512), "512 bytes");
        assert_eq!(file_size(43_798), "43 kB");
        assert_eq!(file_size(5 * 1024 * 1024 + 512 * 1024), "5.5 MB");
        assert_eq!(file_size(3 * 1024 * 1024 * 1024), "3.00 GB");
    }

    #[test]
    fn cancelling_is_visible_through_every_clone() {
        let cancel = Cancel::new();
        let other = cancel.clone();
        assert!(!other.is_cancelled());
        cancel.cancel();
        assert!(other.is_cancelled());
    }
}
