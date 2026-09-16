//! Orchestration: clock, composition, decode scheduling.

use std::sync::Arc;

use ve_core::{Project, Sequence, SequenceId};
use ve_media::{DecodeService, FrameRequest, MediaError, VideoFrame};
use ve_metrics::{spans, Metrics};
use ve_time::Ticks;

use crate::clock::{PlaybackClock, TransportState};
use crate::composition::{evaluate, Composition, VisibleClip};

/// How far ahead to decode while playing, in frames.
///
/// Enough to ride out a slow decode without committing so much of the cache to
/// the future that scrubbing backwards loses everything it just had.
const DEFAULT_PREFETCH_FRAMES: i64 = 12;

/// One clip's picture for the current instant.
pub struct ResolvedLayer {
    pub clip: VisibleClip,
    pub frame: VideoFrame,
}

/// What the engine resolved for one instant.
pub struct EngineUpdate {
    pub position: Ticks,
    pub composition: Composition,
    /// Layers whose frames are decoded and ready, bottom first.
    pub layers: Vec<ResolvedLayer>,
    /// Layers whose frames are still decoding. Non-zero means the preview is
    /// showing an incomplete picture this instant.
    pub pending: usize,
    pub playing: bool,
    /// Set when playback ran to the end of the sequence during this update.
    pub reached_end: bool,
}

impl EngineUpdate {
    /// Whether every visible layer had a frame ready.
    pub fn is_complete(&self) -> bool {
        self.pending == 0
    }
}

/// Drives playback: reads the clock, resolves the composition, and asks the
/// decode service for what it needs.
///
/// Holds no GPU state and does no drawing. The UI calls [`PlaybackEngine::update`]
/// once per repaint and composites whatever came back, which is what keeps the
/// playback logic testable without a window.
pub struct PlaybackEngine {
    clock: PlaybackClock,
    decode: Arc<DecodeService>,
    metrics: Metrics,
    prefetch_frames: i64,
}

impl PlaybackEngine {
    pub fn new(clock: PlaybackClock, decode: Arc<DecodeService>, metrics: Metrics) -> Self {
        PlaybackEngine { clock, decode, metrics, prefetch_frames: DEFAULT_PREFETCH_FRAMES }
    }

    pub fn clock(&self) -> &PlaybackClock {
        &self.clock
    }

    pub fn clock_mut(&mut self) -> &mut PlaybackClock {
        &mut self.clock
    }

    pub fn decode_service(&self) -> &Arc<DecodeService> {
        &self.decode
    }

    pub fn set_prefetch_frames(&mut self, frames: i64) {
        self.prefetch_frames = frames.max(0);
    }

    /// Opens a decoder for every asset the project references.
    ///
    /// Returns the assets that could not be opened, paired with why. A failure
    /// is not fatal: the rest of the timeline still plays, and the offending
    /// clip renders as offline.
    pub fn open_project_assets(
        &self,
        project: &Project,
    ) -> Vec<(ve_core::AssetId, MediaError)> {
        let mut failures = Vec::new();
        for asset in &project.assets {
            if asset.offline || !asset.info.has_video() || self.decode.is_open(asset.id) {
                continue;
            }
            if let Err(e) = self.decode.open(asset.id, &asset.path, None) {
                failures.push((asset.id, e));
            }
        }
        failures
    }

    /// Stops playback and moves the playhead.
    ///
    /// Scrubbing goes through here so that the requested frame is posted as
    /// interactive work, which supersedes any decode still in flight for a
    /// position the user has already left.
    pub fn scrub_to(&mut self, sequence: &Sequence, to: Ticks) {
        let snapped = sequence.snap_to_frame(to).clamp_non_negative();
        self.clock.seek(snapped);
        for clip in evaluate(sequence, snapped).video {
            self.decode.request(FrameRequest::interactive(clip.asset, clip.source_time));
        }
    }

    pub fn play(&mut self, sequence: &Sequence) {
        // Starting from the very end would stop immediately; rewind instead,
        // which is what pressing play at the end of a sequence should do.
        let duration = sequence.duration();
        let from = if self.clock.position() >= duration && duration > Ticks::ZERO {
            Ticks::ZERO
        } else {
            self.clock.position()
        };
        self.clock.set_limit(Some(duration));
        self.clock.play(from);
        self.metrics.reset_playback_counters();
    }

    pub fn stop(&mut self) -> Ticks {
        self.clock.stop()
    }

    pub fn is_playing(&self) -> bool {
        self.clock.is_playing()
    }

    /// Resolves the current instant and schedules the decoding it needs.
    ///
    /// Non-blocking: layers whose frames are not decoded yet are reported as
    /// pending rather than waited for, so a slow decode drops a frame instead
    /// of stalling the interface.
    pub fn update(&mut self, sequence: &Sequence) -> EngineUpdate {
        let _span = self.metrics.span(spans::ENGINE);

        let mut reached_end = false;
        if self.clock.has_reached_limit() {
            self.clock.stop();
            reached_end = true;
        }

        let playing = self.clock.is_playing();
        // Snapping means every request in a frame's worth of wall-clock time
        // resolves to the same cache key, so playback does not decode a frame
        // twice just because two repaints landed inside it.
        let position = sequence.snap_to_frame(self.clock.position());
        let composition = evaluate(sequence, position);

        let mut layers = Vec::with_capacity(composition.video.len());
        let mut pending = 0usize;

        for clip in &composition.video {
            match self.decode.cached_frame(clip.asset, clip.source_time) {
                Some(frame) => layers.push(ResolvedLayer { clip: clip.clone(), frame }),
                None => {
                    pending += 1;
                    // Even while playing this is interactive work: it is the
                    // frame being shown right now, and it must beat any
                    // read-ahead already queued.
                    self.decode
                        .request(FrameRequest::interactive(clip.asset, clip.source_time));
                }
            }
        }

        if pending > 0 && playing {
            // A frame the clock wanted that was not ready is a dropped frame,
            // and saying so is the only way the overlay can be honest about
            // playback quality.
            self.metrics.frame_dropped();
        } else if playing {
            self.metrics.frame_presented();
        }

        if playing {
            self.prefetch(sequence, position);
        }

        EngineUpdate { position, composition, layers, pending, playing, reached_end }
    }

    /// Queues decoding for the frames just after `position`.
    fn prefetch(&self, sequence: &Sequence, position: Ticks) {
        if self.prefetch_frames == 0 {
            return;
        }
        let rate = sequence.rate();
        let current_frame = rate.ticks_to_frame(position);
        let duration = sequence.duration();

        for ahead in 1..=self.prefetch_frames {
            let at = rate.frame_to_ticks(current_frame + ahead);
            if at >= duration {
                break;
            }
            for clip in evaluate(sequence, at).video {
                // Skip what is already decoded, so read-ahead does not fill the
                // queue with work the cache has already done.
                if self.decode.cached_frame(clip.asset, clip.source_time).is_none() {
                    self.decode.request(FrameRequest::prefetch(clip.asset, clip.source_time));
                }
            }
        }
    }

    /// The transport state, for the UI.
    pub fn transport(&self) -> TransportState {
        self.clock.state()
    }

    /// Convenience for the common "play/pause" key.
    pub fn toggle_playback(&mut self, sequence: &Sequence) {
        if self.is_playing() {
            self.stop();
        } else {
            self.play(sequence);
        }
    }

    /// Steps the playhead by whole frames, stopping playback first.
    pub fn step_frames(&mut self, sequence: &Sequence, delta: i64) {
        self.clock.stop();
        let rate = sequence.rate();
        let frame = rate.ticks_to_frame_round(self.clock.position()) + delta;
        let to = rate.frame_to_ticks(frame.max(0));
        self.scrub_to(sequence, to);
    }
}

/// Resolves a sequence at an instant without any decoding, for tests and for
/// tooling that only needs to know *what* is there.
pub fn peek(project: &Project, sequence: SequenceId, at: Ticks) -> Option<Composition> {
    project.sequence(sequence).map(|s| evaluate(s, at))
}
