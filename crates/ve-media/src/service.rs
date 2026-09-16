//! Non-blocking frame decoding.
//!
//! # The scrubbing problem
//!
//! Dragging a playhead across a minute of 4K footage can ask for a hundred
//! frames a second. Decoding all of them is impossible and, more importantly,
//! pointless: by the time frame 40 is decoded the user is at frame 900 and only
//! wants to see frame 900.
//!
//! So each open file gets a worker thread with a **single-slot** interactive
//! queue. Posting a new interactive request overwrites whatever was waiting,
//! which cancels it. The worker therefore always decodes the most recent
//! request rather than working through a backlog of stale ones, and the
//! playhead stays attached to the mouse.
//!
//! Prefetch requests — reading ahead during playback — use a separate bounded
//! FIFO, and interactive work always wins.

use std::collections::{HashMap, VecDeque};
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::thread::JoinHandle;

use crossbeam_channel::{Receiver, Sender};
use parking_lot::{Condvar, Mutex};
use ve_core::{AssetId, Size};
use ve_metrics::{counters, Metrics};
use ve_time::{Rate, Ticks};

use crate::cache::{CacheKey, CacheStats, FrameCache};
use crate::decoder::VideoDecoder;
use crate::frame::VideoFrame;
use crate::MediaError;

/// How many prefetch requests may be outstanding per asset before the oldest
/// are dropped. Read-ahead that has fallen this far behind is no longer
/// read-ahead.
const PREFETCH_QUEUE_LIMIT: usize = 16;

/// Urgency of a decode request.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RequestPriority {
    /// The user is waiting: a scrub, a seek, a paused playhead move. Supersedes
    /// any interactive request not yet started.
    Interactive,
    /// Read-ahead. Queued, never cancels anything.
    Prefetch,
}

/// A request for the frame covering a source time.
#[derive(Debug, Clone, Copy)]
pub struct FrameRequest {
    pub asset: AssetId,
    /// Position in *source* media time.
    pub source_time: Ticks,
    pub priority: RequestPriority,
}

impl FrameRequest {
    pub fn interactive(asset: AssetId, source_time: Ticks) -> Self {
        FrameRequest { asset, source_time, priority: RequestPriority::Interactive }
    }

    pub fn prefetch(asset: AssetId, source_time: Ticks) -> Self {
        FrameRequest { asset, source_time, priority: RequestPriority::Prefetch }
    }
}

/// A completed decode.
#[derive(Debug, Clone)]
pub struct FrameReady {
    pub asset: AssetId,
    pub key: CacheKey,
    pub frame: VideoFrame,
}

/// Something a worker could not do.
#[derive(Debug, Clone)]
pub struct DecodeFailure {
    pub asset: AssetId,
    pub source_time: Ticks,
    pub message: String,
}

/// Events a worker sends back to the caller.
#[derive(Debug, Clone)]
pub enum DecodeEvent {
    Ready(FrameReady),
    Failed(DecodeFailure),
}

/// Owns the decoder threads and the shared frame cache.
///
/// Every method is non-blocking except [`DecodeService::open`], which has to
/// read the file's headers.
pub struct DecodeService {
    cache: Arc<Mutex<FrameCache>>,
    workers: Mutex<HashMap<AssetId, Worker>>,
    events_tx: Sender<DecodeEvent>,
    events_rx: Receiver<DecodeEvent>,
    metrics: Metrics,
}

impl DecodeService {
    pub fn new(cache_budget_mb: usize, metrics: Metrics) -> Self {
        let cache = Arc::new(Mutex::new(
            FrameCache::with_budget_mb(cache_budget_mb).with_metrics(metrics.clone()),
        ));
        // Unbounded so a worker can never block on delivery; a stalled consumer
        // costs memory rather than wedging decoding.
        let (events_tx, events_rx) = crossbeam_channel::unbounded();
        DecodeService {
            cache,
            workers: Mutex::new(HashMap::new()),
            events_tx,
            events_rx,
            metrics,
        }
    }

    /// Opens a file and starts its worker thread.
    ///
    /// `output_size` scales every frame on the way out, for proxies and
    /// thumbnails. Opening an asset that is already open is a no-op.
    pub fn open(
        &self,
        asset: AssetId,
        path: impl AsRef<Path>,
        output_size: Option<Size>,
    ) -> Result<(), MediaError> {
        let mut workers = self.workers.lock();
        if workers.contains_key(&asset) {
            return Ok(());
        }

        let decoder = VideoDecoder::open_scaled(path.as_ref(), output_size)?
            .with_metrics(self.metrics.clone());
        let worker = Worker::spawn(
            asset,
            decoder,
            Arc::clone(&self.cache),
            self.events_tx.clone(),
            self.metrics.clone(),
        );
        workers.insert(asset, worker);
        Ok(())
    }

    pub fn is_open(&self, asset: AssetId) -> bool {
        self.workers.lock().contains_key(&asset)
    }

    /// Stops an asset's worker and drops its cached frames.
    pub fn close(&self, asset: AssetId) {
        if let Some(worker) = self.workers.lock().remove(&asset) {
            worker.shutdown();
        }
        self.cache.lock().invalidate_asset(asset);
    }

    /// The frame rate a worker reports for its asset.
    pub fn asset_rate(&self, asset: AssetId) -> Option<Rate> {
        self.workers.lock().get(&asset).map(|w| w.rate)
    }

    /// Builds the cache key a request would produce.
    pub fn key_for(&self, asset: AssetId, source_time: Ticks) -> Option<CacheKey> {
        let workers = self.workers.lock();
        let worker = workers.get(&asset)?;
        Some(CacheKey::new(asset, worker.rate.ticks_to_frame(source_time), worker.width))
    }

    /// Returns a frame if it is already decoded. Never blocks on decoding.
    ///
    /// This is the call the UI makes on every repaint: a hit paints
    /// immediately, a miss posts a request and paints the previous frame.
    pub fn cached_frame(&self, asset: AssetId, source_time: Ticks) -> Option<VideoFrame> {
        let key = self.key_for(asset, source_time)?;
        self.cache.lock().get(&key)
    }

    /// Posts a decode request. Returns immediately.
    ///
    /// An interactive request replaces any interactive request not yet started,
    /// which is what makes scrubbing track the mouse instead of the decoder.
    pub fn request(&self, request: FrameRequest) {
        self.metrics.incr(counters::DECODE_REQUESTS, 1);
        let workers = self.workers.lock();
        if let Some(worker) = workers.get(&request.asset) {
            worker.post(request.source_time, request.priority, &self.metrics);
        } else {
            log::warn!("decode requested for asset {} which is not open", request.asset);
        }
    }

    /// Takes the next completed decode, or `None` if nothing is ready.
    ///
    /// Called once per UI frame to drain completions and schedule a repaint.
    pub fn try_recv(&self) -> Option<DecodeEvent> {
        self.events_rx.try_recv().ok()
    }

    /// Drains every completed decode.
    pub fn drain(&self) -> Vec<DecodeEvent> {
        self.events_rx.try_iter().collect()
    }

    /// Blocks until an event arrives or the timeout expires. For tests and for
    /// export, which has nothing else to do while it waits.
    pub fn recv_timeout(&self, timeout: std::time::Duration) -> Option<DecodeEvent> {
        self.events_rx.recv_timeout(timeout).ok()
    }

    pub fn cache_stats(&self) -> CacheStats {
        self.cache.lock().stats()
    }

    pub fn clear_cache(&self) {
        self.cache.lock().clear();
    }

    pub fn set_cache_budget_mb(&self, mb: usize) {
        self.cache.lock().set_capacity_bytes(mb * 1024 * 1024);
    }

    /// Whether any worker still has work outstanding.
    pub fn is_idle(&self) -> bool {
        self.workers.lock().values().all(Worker::is_idle)
    }
}

impl Drop for DecodeService {
    fn drop(&mut self) {
        for (_, worker) in self.workers.lock().drain() {
            worker.shutdown();
        }
    }
}

/// The queue shared between a worker thread and its posters.
#[derive(Default)]
struct Queue {
    /// Single slot: a new interactive request overwrites and thereby cancels
    /// the one waiting.
    interactive: Option<Ticks>,
    prefetch: VecDeque<Ticks>,
    /// True while the worker is decoding, so `is_idle` does not report done
    /// with a job in flight.
    busy: bool,
}

impl Queue {
    fn take_job(&mut self) -> Option<Ticks> {
        // Interactive always wins: the user is waiting on it.
        self.interactive.take().or_else(|| self.prefetch.pop_front())
    }

    fn has_work(&self) -> bool {
        self.interactive.is_some() || !self.prefetch.is_empty()
    }
}

struct WorkerShared {
    queue: Mutex<Queue>,
    wake: Condvar,
    shutdown: AtomicBool,
}

struct Worker {
    shared: Arc<WorkerShared>,
    handle: Option<JoinHandle<()>>,
    rate: Rate,
    width: u32,
}

impl Worker {
    fn spawn(
        asset: AssetId,
        mut decoder: VideoDecoder,
        cache: Arc<Mutex<FrameCache>>,
        events: Sender<DecodeEvent>,
        metrics: Metrics,
    ) -> Worker {
        let rate = decoder.rate();
        let width = decoder.output_size().width;
        let shared = Arc::new(WorkerShared {
            queue: Mutex::new(Queue::default()),
            wake: Condvar::new(),
            shutdown: AtomicBool::new(false),
        });

        let thread_shared = Arc::clone(&shared);
        let handle = std::thread::Builder::new()
            .name(format!("ve-decode-{}", asset.raw()))
            .spawn(move || {
                loop {
                    let job = {
                        let mut queue = thread_shared.queue.lock();
                        queue.busy = false;
                        while !queue.has_work()
                            && !thread_shared.shutdown.load(Ordering::Acquire)
                        {
                            thread_shared.wake.wait(&mut queue);
                        }
                        if thread_shared.shutdown.load(Ordering::Acquire) {
                            return;
                        }
                        let job = queue.take_job();
                        queue.busy = job.is_some();
                        job
                    };

                    let Some(source_time) = job else { continue };
                    let key = CacheKey::new(asset, rate.ticks_to_frame(source_time), width);

                    // Another request may have decoded this frame while this
                    // one waited.
                    if cache.lock().contains(&key) {
                        continue;
                    }

                    match decoder.frame_at(source_time) {
                        Ok(Some(frame)) => {
                            // Key on the frame's *actual* presentation time, not
                            // the requested one: a request between two frame
                            // boundaries must land on the frame that covers it,
                            // or the same frame would be cached under many keys.
                            let actual =
                                CacheKey::new(asset, rate.ticks_to_frame(frame.pts()), width);
                            {
                                let mut cache = cache.lock();
                                cache.insert(actual, frame.clone());
                                // Also satisfy the request as posed, so the
                                // caller's lookup hits.
                                if actual != key {
                                    cache.insert(key, frame.clone());
                                }
                            }
                            let _ = events.send(DecodeEvent::Ready(FrameReady {
                                asset,
                                key,
                                frame,
                            }));
                        }
                        Ok(None) => {
                            // Past the end of the media. Not an error: a clip
                            // can legitimately extend beyond its source.
                            log::trace!("no frame at {source_time} in asset {asset}");
                        }
                        Err(e) => {
                            metrics.incr("decode_errors", 1);
                            let _ = events.send(DecodeEvent::Failed(DecodeFailure {
                                asset,
                                source_time,
                                message: e.to_string(),
                            }));
                        }
                    }
                }
            })
            .expect("spawning a decoder thread");

        Worker { shared, handle: Some(handle), rate, width }
    }

    fn post(&self, source_time: Ticks, priority: RequestPriority, metrics: &Metrics) {
        {
            let mut queue = self.shared.queue.lock();
            match priority {
                RequestPriority::Interactive => {
                    // Replacing a waiting request *is* the cancellation.
                    if queue.interactive.replace(source_time).is_some() {
                        metrics.incr(counters::DECODE_CANCELLED, 1);
                    }
                }
                RequestPriority::Prefetch => {
                    if queue.prefetch.len() >= PREFETCH_QUEUE_LIMIT {
                        queue.prefetch.pop_front();
                        metrics.incr(counters::DECODE_CANCELLED, 1);
                    }
                    queue.prefetch.push_back(source_time);
                }
            }
        }
        self.shared.wake.notify_one();
    }

    fn is_idle(&self) -> bool {
        let queue = self.shared.queue.lock();
        !queue.has_work() && !queue.busy
    }

    fn shutdown(mut self) {
        self.shared.shutdown.store(true, Ordering::Release);
        self.shared.wake.notify_all();
        if let Some(handle) = self.handle.take() {
            let _ = handle.join();
        }
    }
}

impl Drop for Worker {
    fn drop(&mut self) {
        // Reached when the map is dropped without an explicit shutdown.
        self.shared.shutdown.store(true, Ordering::Release);
        self.shared.wake.notify_all();
        if let Some(handle) = self.handle.take() {
            let _ = handle.join();
        }
    }
}

/// Where the media crate keeps its default budgets.
pub mod budget {
    /// Frames cached by default. Roughly 40 frames of 4K RGBA, or 500 of 1080p.
    pub const DEFAULT_FRAME_CACHE_MB: usize = 512;
}
