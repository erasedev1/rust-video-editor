//! Non-blocking waveform analysis.
//!
//! # Why this is not the decode service
//!
//! Decoding a picture is a *random-access* problem: the user is at frame 900
//! and the answer to frame 40 is worthless, so the queue is one slot deep and a
//! new request cancels the old one. Analysing audio is the opposite. It is a
//! linear pass over a whole file, it is wanted exactly once, and its result is
//! useful the moment the first second of it exists. So the shape is different:
//! a small pool of workers, a FIFO of files to get through, and results
//! published *as they are produced* rather than at the end.
//!
//! That last part is what makes a long import bearable. Dropping a two-hour
//! podcast onto the timeline draws a waveform that fills in from the left while
//! you are already cutting with it, instead of an empty rectangle that becomes
//! a waveform some minutes later.
//!
//! # Bounded like the frame cache
//!
//! Peaks are held under a byte budget with LRU eviction, for the same reason
//! frames are: a project can reference more audio than fits in memory, and the
//! peaks for a file nobody is looking at are the cheapest thing to lose — they
//! cost a re-analysis, not a re-edit. An analysis in flight is never evicted,
//! because evicting it would throw away work that is still being done.

use std::collections::{HashMap, VecDeque};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::thread::JoinHandle;

use crossbeam_channel::{Receiver, Sender};
use parking_lot::{Condvar, Mutex};
use ve_core::AssetId;
use ve_metrics::{counters, spans, Metrics};
use ve_time::{SampleRate, Ticks, TimeRange};

use crate::decoder::AudioDecoder;
use crate::waveform::{Peak, Waveform, WaveformBuilder, ANALYSIS_CHANNELS};
use crate::MediaError;

/// Worker threads analysing files at once.
///
/// Two rather than one so a short clip dropped in during a long file's analysis
/// is not stuck behind it, and not more because analysis is decode-bound and
/// the decode service wants the rest of the machine.
const ANALYSIS_THREADS: usize = 2;

/// Buckets accumulated before publishing — two seconds of audio.
///
/// Small enough that a waveform visibly fills in, large enough that a long file
/// does not take the registry lock thousands of times a second.
const PUBLISH_EVERY: usize = 2 * crate::waveform::PEAKS_PER_SECOND as usize;

/// How far analysis has got with one asset.
#[derive(Debug, Clone, PartialEq)]
pub enum WaveformState {
    /// Never requested, or forgotten since.
    Missing,
    /// Requested, nothing analysed yet.
    Queued,
    /// Being analysed; `analysed` is how much of the file is already drawable.
    Analysing {
        analysed: Ticks,
    },
    Ready,
    /// The file could not be analysed. Sticky: retrying on every repaint would
    /// reopen a broken file sixty times a second.
    Failed(String),
}

impl WaveformState {
    pub fn is_missing(&self) -> bool {
        matches!(self, WaveformState::Missing)
    }

    /// Whether anything is drawable yet.
    pub fn has_peaks(&self) -> bool {
        matches!(self, WaveformState::Ready | WaveformState::Analysing { .. })
    }
}

/// Something a worker finished or made progress on.
#[derive(Debug, Clone, PartialEq)]
pub enum WaveformEvent {
    /// More of an asset's waveform is available.
    Progress(AssetId),
    Ready(AssetId),
    Failed {
        asset: AssetId,
        message: String,
    },
}

impl WaveformEvent {
    pub fn asset(&self) -> AssetId {
        match self {
            WaveformEvent::Progress(a) | WaveformEvent::Ready(a) => *a,
            WaveformEvent::Failed { asset, .. } => *asset,
        }
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct WaveformStats {
    pub entries: usize,
    pub bytes: usize,
    pub capacity_bytes: usize,
    pub queued: usize,
    pub running: usize,
    pub evictions: u64,
}

/// Analyses audio files in the background and holds the results.
pub struct WaveformService {
    shared: Arc<Shared>,
    events_rx: Receiver<WaveformEvent>,
    workers: Vec<JoinHandle<()>>,
}

impl WaveformService {
    pub fn new(budget_mb: usize, metrics: Metrics) -> Self {
        const MIN_BYTES: usize = 4 * 1024 * 1024;
        let (events_tx, events_rx) = crossbeam_channel::unbounded();
        let shared = Arc::new(Shared {
            registry: Mutex::new(Registry {
                entries: HashMap::new(),
                queue: VecDeque::new(),
                running: 0,
                capacity_bytes: (budget_mb * 1024 * 1024).max(MIN_BYTES),
                bytes: 0,
                clock: 0,
                generation: 0,
                evictions: 0,
                shutdown: false,
            }),
            wake: Condvar::new(),
            events: events_tx,
            metrics,
        });

        let workers = (0..ANALYSIS_THREADS)
            .map(|i| {
                let shared = Arc::clone(&shared);
                std::thread::Builder::new()
                    .name(format!("ve-waveform-{i}"))
                    .spawn(move || work(shared))
                    .expect("spawning a waveform worker")
            })
            .collect();

        WaveformService { shared, events_rx, workers }
    }

    /// Asks for an asset's waveform, if it is not already known or in flight.
    ///
    /// Cheap enough to call from the paint loop, which is where it is called
    /// from: a clip that is on screen and has no waveform asks for one, exactly
    /// as a clip with no decoded frame asks for that.
    pub fn request(&self, asset: AssetId, path: impl AsRef<Path>, sample_rate: SampleRate) {
        let mut registry = self.shared.registry.lock();
        if registry.entries.contains_key(&asset) {
            return;
        }
        registry.generation += 1;
        let generation = registry.generation;
        let clock = registry.tick();
        registry.entries.insert(
            asset,
            Entry {
                waveform: Waveform::empty(sample_rate, ANALYSIS_CHANNELS),
                status: WaveformState::Queued,
                last_used: clock,
                generation,
            },
        );
        registry.queue.push_back(Job {
            asset,
            path: path.as_ref().to_path_buf(),
            sample_rate,
            generation,
        });
        drop(registry);
        self.shared.wake.notify_one();
    }

    /// How far along an asset is, marking it recently used.
    pub fn state(&self, asset: AssetId) -> WaveformState {
        let mut registry = self.shared.registry.lock();
        let clock = registry.tick();
        match registry.entries.get_mut(&asset) {
            Some(entry) => {
                entry.last_used = clock;
                entry.status.clone()
            }
            None => WaveformState::Missing,
        }
    }

    /// Reduces part of an asset's waveform to `columns` pixel columns.
    ///
    /// Computed under the registry lock rather than by handing out a copy of
    /// the peaks: a two-hour file's peaks are megabytes, and the caller only
    /// ever wants a few hundred numbers from them.
    pub fn envelope(
        &self,
        asset: AssetId,
        range: TimeRange,
        columns: usize,
    ) -> Option<Vec<Option<Peak>>> {
        let mut registry = self.shared.registry.lock();
        let clock = registry.tick();
        let entry = registry.entries.get_mut(&asset)?;
        entry.last_used = clock;
        Some(entry.waveform.envelope(range, columns))
    }

    /// Runs `f` against an asset's peaks, if it has any.
    pub fn with_waveform<R>(
        &self,
        asset: AssetId,
        f: impl FnOnce(&Waveform) -> R,
    ) -> Option<R> {
        let mut registry = self.shared.registry.lock();
        let clock = registry.tick();
        let entry = registry.entries.get_mut(&asset)?;
        entry.last_used = clock;
        Some(f(&entry.waveform))
    }

    /// Drops an asset's peaks, cancelling its analysis if one is running.
    pub fn forget(&self, asset: AssetId) {
        let mut registry = self.shared.registry.lock();
        registry.remove(asset);
        registry.queue.retain(|job| job.asset != asset);
        registry.publish_gauges(&self.shared.metrics);
    }

    /// Drops everything, cancelling analyses in flight. For closing a project.
    pub fn clear(&self) {
        let mut registry = self.shared.registry.lock();
        registry.entries.clear();
        registry.queue.clear();
        registry.bytes = 0;
        // Every job still running now carries a stale generation, so its next
        // publish finds no entry to write into and stops.
        registry.generation += 1;
        registry.publish_gauges(&self.shared.metrics);
    }

    pub fn stats(&self) -> WaveformStats {
        let registry = self.shared.registry.lock();
        WaveformStats {
            entries: registry.entries.len(),
            bytes: registry.bytes,
            capacity_bytes: registry.capacity_bytes,
            queued: registry.queue.len(),
            running: registry.running,
            evictions: registry.evictions,
        }
    }

    pub fn set_budget_mb(&self, mb: usize) {
        self.set_budget_bytes(mb * 1024 * 1024);
    }

    pub fn set_budget_bytes(&self, bytes: usize) {
        let mut registry = self.shared.registry.lock();
        registry.capacity_bytes = bytes;
        registry.evict_to_budget();
        registry.publish_gauges(&self.shared.metrics);
    }

    /// Takes the next event, or `None` if nothing is waiting.
    pub fn try_recv(&self) -> Option<WaveformEvent> {
        self.events_rx.try_recv().ok()
    }

    pub fn drain(&self) -> Vec<WaveformEvent> {
        self.events_rx.try_iter().collect()
    }

    /// Blocks until an event arrives or the timeout expires. For tests.
    pub fn recv_timeout(&self, timeout: std::time::Duration) -> Option<WaveformEvent> {
        self.events_rx.recv_timeout(timeout).ok()
    }

    /// Whether every requested analysis has finished.
    pub fn is_idle(&self) -> bool {
        let registry = self.shared.registry.lock();
        registry.queue.is_empty() && registry.running == 0
    }
}

impl Drop for WaveformService {
    fn drop(&mut self) {
        {
            let mut registry = self.shared.registry.lock();
            registry.shutdown = true;
            registry.queue.clear();
            // Stale-generation every job in flight, so a worker part way
            // through a long file gives up at its next publish rather than
            // decoding to the end of it while the editor is closing.
            registry.generation += 1;
        }
        self.shared.wake.notify_all();
        for worker in self.workers.drain(..) {
            let _ = worker.join();
        }
    }
}

struct Shared {
    registry: Mutex<Registry>,
    wake: Condvar,
    events: Sender<WaveformEvent>,
    metrics: Metrics,
}

struct Job {
    asset: AssetId,
    path: PathBuf,
    sample_rate: SampleRate,
    /// Identifies this request, so a worker whose asset was forgotten and
    /// requested again does not publish into the new entry.
    generation: u64,
}

struct Entry {
    waveform: Waveform,
    status: WaveformState,
    last_used: u64,
    generation: u64,
}

struct Registry {
    entries: HashMap<AssetId, Entry>,
    queue: VecDeque<Job>,
    running: usize,
    capacity_bytes: usize,
    bytes: usize,
    clock: u64,
    generation: u64,
    evictions: u64,
    shutdown: bool,
}

impl Registry {
    fn tick(&mut self) -> u64 {
        self.clock += 1;
        self.clock
    }

    fn remove(&mut self, asset: AssetId) {
        if let Some(entry) = self.entries.remove(&asset) {
            self.bytes = self.bytes.saturating_sub(entry.waveform.byte_size());
        }
    }

    /// Evicts least-recently-used finished waveforms until the budget is met.
    ///
    /// Only finished ones: an analysis in flight has a worker decoding into it,
    /// and dropping it would throw away work already done and leave that worker
    /// filling a hole.
    fn evict_to_budget(&mut self) {
        while self.bytes > self.capacity_bytes {
            let victim = self
                .entries
                .iter()
                .filter(|(_, e)| !matches!(e.status, WaveformState::Analysing { .. }))
                .filter(|(_, e)| e.waveform.byte_size() > 0)
                .min_by_key(|(_, e)| e.last_used)
                .map(|(id, _)| *id);
            match victim {
                Some(id) => {
                    self.remove(id);
                    self.evictions += 1;
                }
                // Nothing evictable: one file's peaks exceed the whole budget,
                // or everything present is still being analysed. Going over is
                // the lesser evil against refusing to draw anything.
                None => break,
            }
        }
    }

    fn publish_gauges(&self, metrics: &Metrics) {
        metrics.set_gauge(counters::WAVEFORM_BYTES, self.bytes as f64);
        metrics.set_gauge(counters::WAVEFORM_ENTRIES, self.entries.len() as f64);
    }
}

/// A worker thread: take a job, analyse it, publish as it goes.
fn work(shared: Arc<Shared>) {
    loop {
        let job = {
            let mut registry = shared.registry.lock();
            loop {
                if registry.shutdown {
                    return;
                }
                match registry.queue.pop_front() {
                    Some(job) => {
                        // A job whose entry has gone, or has been re-requested
                        // since, is stale: drop it and look for the next.
                        let live = registry
                            .entries
                            .get_mut(&job.asset)
                            .is_some_and(|e| e.generation == job.generation);
                        if !live {
                            continue;
                        }
                        if let Some(entry) = registry.entries.get_mut(&job.asset) {
                            entry.status = WaveformState::Analysing { analysed: Ticks::ZERO };
                        }
                        registry.running += 1;
                        break job;
                    }
                    None => shared.wake.wait(&mut registry),
                }
            }
        };

        let asset = job.asset;
        let result = analyse_job(&shared, &job);

        let mut registry = shared.registry.lock();
        registry.running -= 1;
        // A cancelled job reports `Ok(false)`: it reached neither the end of
        // the file nor a failure, so it must not mark anything ready. Checking
        // the generation as well is not enough on its own — shutdown cancels
        // without touching it.
        let still_ours = !matches!(result, Ok(false))
            && registry.entries.get(&asset).is_some_and(|e| e.generation == job.generation);
        if still_ours {
            match &result {
                Ok(_) => {
                    if let Some(entry) = registry.entries.get_mut(&asset) {
                        entry.waveform.mark_complete();
                        entry.status = WaveformState::Ready;
                    }
                    registry.evict_to_budget();
                    registry.publish_gauges(&shared.metrics);
                    let _ = shared.events.send(WaveformEvent::Ready(asset));
                }
                Err(e) => {
                    let message = e.to_string();
                    log::warn!("could not analyse audio for asset {asset}: {message}");
                    // The peaks produced before the failure are dropped: half a
                    // waveform marked ready would be indistinguishable from a
                    // file that really does end there.
                    registry.remove(asset);
                    let clock = registry.tick();
                    registry.entries.insert(
                        asset,
                        Entry {
                            waveform: Waveform::empty(job.sample_rate, ANALYSIS_CHANNELS),
                            status: WaveformState::Failed(message.clone()),
                            last_used: clock,
                            generation: job.generation,
                        },
                    );
                    registry.publish_gauges(&shared.metrics);
                    let _ = shared.events.send(WaveformEvent::Failed { asset, message });
                }
            }
        }
        drop(registry);
        // A cancelled job is not an event: whoever cancelled it knows.
    }
}

/// Decodes one file straight through, publishing peaks as they accumulate.
///
/// Returns whether it reached the end of the file; `false` means the analysis
/// was cancelled part way, which is not a failure and leaves nothing to report.
fn analyse_job(shared: &Arc<Shared>, job: &Job) -> Result<bool, MediaError> {
    let _span = shared.metrics.span(spans::WAVEFORM);

    let mut decoder = AudioDecoder::open(&job.path, job.sample_rate, ANALYSIS_CHANNELS)?
        .with_metrics(shared.metrics.clone());
    let mut builder = WaveformBuilder::new(job.sample_rate, ANALYSIS_CHANNELS);

    while let Some(buffer) = decoder.next_buffer()? {
        builder.push(&buffer);
        if builder.completed_len() >= PUBLISH_EVERY
            && !publish(shared, job, builder.take_completed())
        {
            return Ok(false);
        }
    }
    builder.flush();
    Ok(publish(shared, job, builder.take_completed()))
}

/// Appends peaks to the shared entry. Returns whether the job is still wanted.
fn publish(shared: &Arc<Shared>, job: &Job, peaks: Vec<Peak>) -> bool {
    if peaks.is_empty() {
        // Still a cancellation check: a job producing nothing must notice that
        // its asset has gone rather than decoding a whole file to find out.
        let registry = shared.registry.lock();
        return registry
            .entries
            .get(&job.asset)
            .is_some_and(|e| e.generation == job.generation)
            && !registry.shutdown;
    }

    let added = peaks.len() * std::mem::size_of::<Peak>();
    let mut registry = shared.registry.lock();
    if registry.shutdown {
        return false;
    }
    let clock = registry.tick();
    match registry.entries.get_mut(&job.asset) {
        Some(entry) if entry.generation == job.generation => {
            entry.waveform.extend(&peaks);
            entry.status =
                WaveformState::Analysing { analysed: entry.waveform.analysed_duration() };
            // Being analysed counts as being used: a waveform that has just
            // been produced is the last one that should be evicted to make room
            // for the next, and without this the budget would throw away the
            // work it just paid for.
            entry.last_used = clock;
        }
        // Forgotten, or superseded by a newer request for the same asset.
        _ => return false,
    }
    registry.bytes += added;
    registry.evict_to_budget();
    registry.publish_gauges(&shared.metrics);
    shared.metrics.incr(counters::WAVEFORM_BUCKETS, peaks.len() as u64);
    drop(registry);

    let _ = shared.events.send(WaveformEvent::Progress(job.asset));
    true
}
