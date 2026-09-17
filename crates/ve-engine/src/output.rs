//! Audio output: the mixing thread, the ring, and the device at the end of it.
//!
//! # Three threads, one direction
//!
//! The interface thread owns the device stream and publishes a snapshot of the
//! project. A mixing thread reads that snapshot, decodes and mixes ahead, and
//! pushes samples into the ring. The device callback, on a real-time thread,
//! does nothing but copy out. Nothing ever travels the other way, and nothing
//! the interface does can block the callback.
//!
//! # The project crosses as a snapshot
//!
//! The mixer cannot borrow the project the interface is editing, so it gets an
//! `Arc<Project>` published whenever the edit changes. That costs one clone of
//! the edit model per edit — no media, only the decisions — which is far cheaper
//! than it sounds and much cheaper than a lock held across a decode.
//!
//! # The meters describe what is being heard
//!
//! Mixing runs ahead of the device by up to the ring's depth, so the block being
//! mixed is not the block coming out of the speakers. Each block's reading is
//! therefore queued with the sample count it ends at, and the published reading
//! is the newest one the device has actually reached. Without that the meters
//! would lead the sound by a fifth of a second and never agree with it.

use std::collections::VecDeque;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;

use ve_core::{Project, SequenceId, TrackId};
use ve_metrics::Metrics;
use ve_time::{SampleRate, Ticks};

use crate::audio::{AudioError, AudioRenderer, AudioSink};
use crate::cpal_sink::CpalSink;
use crate::mixer::Meter;
use crate::ring::AudioRing;

/// How much mixed audio to keep ahead of the device, in seconds.
///
/// Long enough that a slow decode does not become an underrun, short enough
/// that starting, stopping and seeking feel immediate — the ring has to drain
/// before a stop is silent, so this is also the worst-case lag on the transport.
const BUFFER_SECONDS: f64 = 0.2;

/// How long the mixing thread sleeps when there is nothing to do.
///
/// Short against [`BUFFER_SECONDS`], so the ring is topped up long before it can
/// run dry, and long enough that an idle editor is not spinning a core.
const IDLE_SLEEP: std::time::Duration = std::time::Duration::from_millis(4);

/// What the meters read for one block.
#[derive(Debug, Clone, Default)]
pub struct AudioLevels {
    pub master: Meter,
    pub tracks: Vec<(TrackId, Meter)>,
    /// Samples limited in this block. Non-zero means the mix is over.
    pub clipped: usize,
}

impl AudioLevels {
    pub fn track(&self, track: TrackId) -> Meter {
        self.tracks
            .iter()
            .find(|(id, _)| *id == track)
            .map(|(_, m)| *m)
            .unwrap_or(Meter::SILENT)
    }
}

/// What the interface hands the mixing thread: the edit, and where in it to be.
struct Snapshot {
    project: Arc<Project>,
    sequence: SequenceId,
    /// Bumped on every publish, so the mixer knows to re-register assets rather
    /// than comparing paths every block.
    generation: u64,
}

struct Shared {
    snapshot: Mutex<Option<Snapshot>>,
    /// Set by the interface, honoured by the mixer at the start of a block.
    seek_to: Mutex<Option<Ticks>>,
    playing: AtomicBool,
    running: AtomicBool,
    levels: Mutex<AudioLevels>,
    /// Blocks pushed since the stream opened, for the overlay.
    blocks: AtomicUsize,
}

/// Plays a sequence's audio through the system's default output device.
///
/// Opening one starts the mixing thread; dropping it stops the thread and
/// closes the stream. Deliberately not `Send`: the device stream is
/// thread-affine on several backends, so this belongs to the thread that opened
/// it — in practice the interface thread.
pub struct AudioOutput {
    sink: CpalSink,
    shared: Arc<Shared>,
    worker: Option<JoinHandle<()>>,
    sample_rate: SampleRate,
    channels: u16,
    device: String,
    /// Whether the device stream has been started. The stream is left running
    /// once started and fed silence when stopped, because tearing a stream down
    /// and rebuilding it on every play is where clicks come from.
    started: bool,
}

impl AudioOutput {
    /// Opens the default output device and starts mixing for it.
    ///
    /// Fails with [`AudioError::NoDevice`] where there is no sound card at all,
    /// which is the ordinary state of a headless machine. Callers are expected
    /// to carry on without audio rather than to treat it as fatal.
    pub fn open(metrics: Metrics) -> Result<AudioOutput, AudioError> {
        // The ring is sized from the device's own format, so it holds the same
        // span of *time* whatever rate the device turns out to want.
        let (rate, device_channels) = CpalSink::default_format()?;
        let samples =
            (rate.hz() as f64 * BUFFER_SECONDS) as usize * device_channels.max(1) as usize;
        let (producer, consumer) = AudioRing::split(samples.max(1024));

        let mut sink = CpalSink::new(consumer)?;
        let sample_rate = sink.sample_rate();
        let channels = sink.channels();
        let device = sink.device_name();

        let shared = Arc::new(Shared {
            snapshot: Mutex::new(None),
            seek_to: Mutex::new(None),
            playing: AtomicBool::new(false),
            running: AtomicBool::new(true),
            levels: Mutex::new(AudioLevels::default()),
            blocks: AtomicUsize::new(0),
        });

        let worker = {
            let shared = Arc::clone(&shared);
            std::thread::Builder::new()
                .name("verge-audio-mix".into())
                .spawn(move || mix_loop(shared, producer, sample_rate, channels, metrics))
                .map_err(|e| AudioError::Device(format!("could not start the mixer: {e}")))?
        };

        // The stream runs from the moment the device opens, playing silence
        // until there is something to play. Starting it here rather than on the
        // first play means the first note is not swallowed by the device
        // spinning up.
        sink.play()?;

        Ok(AudioOutput {
            sink,
            shared,
            worker: Some(worker),
            sample_rate,
            channels,
            device,
            started: true,
        })
    }

    pub fn device_name(&self) -> &str {
        &self.device
    }

    pub fn sample_rate(&self) -> SampleRate {
        self.sample_rate
    }

    pub fn channels(&self) -> u16 {
        self.channels
    }

    pub fn is_playing(&self) -> bool {
        self.shared.playing.load(Ordering::Relaxed)
    }

    /// Publishes the edit the mixer should be playing.
    ///
    /// Called whenever the project changes. The clone is of the edit model
    /// alone; media is opened by the mixer from the asset paths inside it.
    pub fn publish(&self, project: &Project, sequence: SequenceId) {
        let mut slot = self.shared.snapshot.lock().unwrap_or_else(|e| e.into_inner());
        let generation = slot.as_ref().map(|s| s.generation + 1).unwrap_or(0);
        *slot = Some(Snapshot { project: Arc::new(project.clone()), sequence, generation });
    }

    /// Starts playing from `from`.
    pub fn play(&mut self, from: Ticks) {
        self.seek(from);
        self.shared.playing.store(true, Ordering::Relaxed);
        if !self.started {
            if let Err(e) = self.sink.play() {
                log::warn!("could not start the audio stream: {e}");
                return;
            }
            self.started = true;
        }
    }

    /// Stops playing. The stream stays open and plays silence.
    pub fn stop(&mut self) {
        self.shared.playing.store(false, Ordering::Relaxed);
        self.clear_levels();
    }

    /// Moves where the mixer reads from, discarding what it has mixed ahead.
    pub fn seek(&self, to: Ticks) {
        *self.shared.seek_to.lock().unwrap_or_else(|e| e.into_inner()) = Some(to);
        self.clear_levels();
    }

    /// What the device is playing right now, per track and overall.
    pub fn levels(&self) -> AudioLevels {
        self.shared.levels.lock().unwrap_or_else(|e| e.into_inner()).clone()
    }

    /// Blocks mixed since the device opened, for the performance overlay.
    pub fn blocks_mixed(&self) -> usize {
        self.shared.blocks.load(Ordering::Relaxed)
    }

    fn clear_levels(&self) {
        *self.shared.levels.lock().unwrap_or_else(|e| e.into_inner()) = AudioLevels::default();
    }
}

impl Drop for AudioOutput {
    fn drop(&mut self) {
        self.shared.running.store(false, Ordering::Relaxed);
        self.shared.playing.store(false, Ordering::Relaxed);
        let _ = self.sink.pause();
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
    }
}

/// One block's reading, tagged with the sample frame the device reaches when it
/// has played the block.
struct PendingLevels {
    plays_until: u64,
    levels: AudioLevels,
}

fn mix_loop(
    shared: Arc<Shared>,
    producer: crate::ring::Producer,
    sample_rate: SampleRate,
    channels: u16,
    metrics: Metrics,
) {
    let mut renderer = AudioRenderer::new(sample_rate, channels, metrics);
    let mut known_generation: Option<u64> = None;
    let mut registered: Vec<(ve_core::AssetId, PathBuf)> = Vec::new();
    // Sample frames handed to the ring since the stream opened, which is what
    // the queued readings are measured against.
    let mut written_frames: u64 = 0;
    let mut pending: VecDeque<PendingLevels> = VecDeque::new();

    // A block is a fraction of the buffer, so the ring is topped up in several
    // small pushes rather than one big one that could arrive late.
    let block = (sample_rate.hz() as f64 * BUFFER_SECONDS / 4.0) as usize;
    let block = block.max(256);

    while shared.running.load(Ordering::Relaxed) {
        if let Some(to) = shared.seek_to.lock().unwrap_or_else(|e| e.into_inner()).take() {
            renderer.seek(to);
            // Everything queued describes the old position. The ring itself is
            // left alone: it drains in a fifth of a second, and clearing it
            // under the device is a click.
            pending.clear();
        }

        let snapshot = {
            let slot = shared.snapshot.lock().unwrap_or_else(|e| e.into_inner());
            slot.as_ref().map(|s| (Arc::clone(&s.project), s.sequence, s.generation))
        };

        let Some((project, sequence_id, generation)) = snapshot else {
            std::thread::sleep(IDLE_SLEEP);
            continue;
        };

        if known_generation != Some(generation) {
            known_generation = Some(generation);
            // Asset ids are stable within a project, so only the additions and
            // removals need acting on; re-opening every decoder on every edit
            // would make typing in the inspector stutter the sound.
            for asset in &project.assets {
                if !registered.iter().any(|(id, path)| *id == asset.id && path == &asset.path) {
                    renderer.register_asset(asset.id, &asset.path);
                    registered.retain(|(id, _)| *id != asset.id);
                    registered.push((asset.id, asset.path.clone()));
                }
            }
            registered.retain(|(id, _)| {
                let kept = project.assets.iter().any(|a| a.id == *id);
                if !kept {
                    renderer.forget_asset(*id);
                }
                kept
            });
        }

        if !shared.playing.load(Ordering::Relaxed) {
            std::thread::sleep(IDLE_SLEEP);
            continue;
        }

        let Some(sequence) = project.sequence(sequence_id) else {
            std::thread::sleep(IDLE_SLEEP);
            continue;
        };

        let frames = renderer.render_into(&project, sequence, &producer, block);
        if frames == 0 {
            // The ring is full, which is the steady state: the mixer is
            // comfortably ahead and there is nothing to do until the device has
            // taken some more.
            publish_levels(&shared, &producer, written_frames, channels, &mut pending);
            std::thread::sleep(IDLE_SLEEP);
            continue;
        }

        written_frames += frames as u64;
        shared.blocks.fetch_add(1, Ordering::Relaxed);
        let stats = renderer.last_stats();
        pending.push_back(PendingLevels {
            plays_until: written_frames,
            levels: AudioLevels {
                master: stats.master,
                tracks: renderer.track_meters().to_vec(),
                clipped: stats.clipped,
            },
        });
        publish_levels(&shared, &producer, written_frames, channels, &mut pending);
    }
}

/// Publishes the reading for the block the device has actually reached.
fn publish_levels(
    shared: &Shared,
    producer: &crate::ring::Producer,
    written_frames: u64,
    channels: u16,
    pending: &mut VecDeque<PendingLevels>,
) {
    let queued_frames = (producer.queued() / channels.max(1) as usize) as u64;
    let played = written_frames.saturating_sub(queued_frames);

    // Drop every block the device has finished with, keeping the last one:
    // that is the block being heard.
    let mut current = None;
    while pending.front().is_some_and(|p| p.plays_until <= played) {
        current = pending.pop_front();
    }
    if let Some(current) = current {
        *shared.levels.lock().unwrap_or_else(|e| e.into_inner()) = current.levels;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ring::AudioRing;

    fn levels(peak: f32) -> AudioLevels {
        AudioLevels {
            master: Meter { left: peak, right: peak },
            tracks: Vec::new(),
            clipped: 0,
        }
    }

    fn shared() -> Shared {
        Shared {
            snapshot: Mutex::new(None),
            seek_to: Mutex::new(None),
            playing: AtomicBool::new(false),
            running: AtomicBool::new(true),
            levels: Mutex::new(AudioLevels::default()),
            blocks: AtomicUsize::new(0),
        }
    }

    fn published(shared: &Shared) -> f32 {
        shared.levels.lock().unwrap().master.peak()
    }

    /// The property the queue exists for: with three blocks mixed and none of
    /// them played yet, the meter must still read the *first*, because that is
    /// what is about to come out of the speakers.
    #[test]
    fn the_meter_reads_the_block_the_device_has_reached_not_the_newest() {
        let shared = shared();
        let (producer, consumer) = AudioRing::split(4096);
        let mut pending = VecDeque::new();

        // Three blocks of 100 frames each, mixed and queued but unplayed.
        for (index, peak) in [0.1f32, 0.5, 0.9].into_iter().enumerate() {
            producer.push(&vec![0.0f32; 200]);
            pending.push_back(PendingLevels {
                plays_until: (index as u64 + 1) * 100,
                levels: levels(peak),
            });
        }
        publish_levels(&shared, &producer, 300, 2, &mut pending);
        assert_eq!(published(&shared), 0.0, "nothing has played yet");

        // The device takes the first block.
        let mut out = vec![0.0f32; 200];
        consumer.fill(&mut out);
        publish_levels(&shared, &producer, 300, 2, &mut pending);
        assert_eq!(published(&shared), 0.1, "the first block is what is being heard");

        // And the second.
        consumer.fill(&mut out);
        publish_levels(&shared, &producer, 300, 2, &mut pending);
        assert_eq!(published(&shared), 0.5);

        consumer.fill(&mut out);
        publish_levels(&shared, &producer, 300, 2, &mut pending);
        assert_eq!(published(&shared), 0.9);
        assert!(pending.is_empty(), "played blocks must not accumulate");
    }

    /// Feeding the meter straight from the mixer is the bug this replaces: it
    /// would have read 0.9 from the first call above. Assert the difference.
    #[test]
    fn the_meter_does_not_simply_follow_the_mixer() {
        let shared = shared();
        let (producer, _consumer) = AudioRing::split(4096);
        let mut pending = VecDeque::new();

        producer.push(&vec![0.0f32; 200]);
        pending.push_back(PendingLevels { plays_until: 100, levels: levels(0.9) });
        publish_levels(&shared, &producer, 100, 2, &mut pending);

        assert_eq!(published(&shared), 0.0);
        assert_eq!(pending.len(), 1, "the reading is held until it is heard");
    }

    /// Several blocks played between two calls: only the last one is the
    /// current reading, and the ones before it are discarded rather than shown.
    #[test]
    fn blocks_played_between_calls_are_skipped_past() {
        let shared = shared();
        let (producer, consumer) = AudioRing::split(4096);
        let mut pending = VecDeque::new();

        for (index, peak) in [0.2f32, 0.4, 0.6, 0.8].into_iter().enumerate() {
            producer.push(&vec![0.0f32; 200]);
            pending.push_back(PendingLevels {
                plays_until: (index as u64 + 1) * 100,
                levels: levels(peak),
            });
        }
        // The device plays three blocks' worth in one go.
        let mut out = vec![0.0f32; 600];
        consumer.fill(&mut out);
        publish_levels(&shared, &producer, 400, 2, &mut pending);

        assert_eq!(published(&shared), 0.6, "the newest block actually reached");
        assert_eq!(pending.len(), 1, "only the unplayed block is still waiting");
    }

    #[test]
    fn an_empty_queue_leaves_the_last_reading_alone() {
        let shared = shared();
        let (producer, _consumer) = AudioRing::split(1024);
        let mut pending = VecDeque::new();
        *shared.levels.lock().unwrap() = levels(0.3);

        publish_levels(&shared, &producer, 0, 2, &mut pending);
        assert_eq!(published(&shared), 0.3);
    }
}
