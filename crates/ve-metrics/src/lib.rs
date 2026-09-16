//! Performance instrumentation.
//!
//! The editor's central claim is that it is fast, so speed has to be a measured
//! quantity rather than an impression. Every expensive stage — decode, upload,
//! composite, present — opens a [`Span`], and the numbers surface in the
//! development overlay and in the benchmark suite.
//!
//! # Cost
//!
//! Recording a span is a lock on a small map plus a push into a fixed ring
//! buffer: on the order of a hundred nanoseconds, against frame budgets of
//! 16 ms. Instrumentation is therefore always on rather than behind a feature
//! flag, because a measurement you have to recompile to get is one you will not
//! take.

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use parking_lot::Mutex;

mod platform;
mod window;

pub use platform::{process_memory, ProcessMemory};
pub use window::{RollingWindow, SpanStats};

/// Number of samples kept per span: four seconds at 60 fps, which is long
/// enough to see a stutter and short enough to still reflect the present.
pub const WINDOW_SAMPLES: usize = 240;

/// A shared handle to the metric registry.
///
/// Cheap to clone and safe to hold from any thread, so decoder workers, the
/// render thread and the UI all report into the same place.
#[derive(Clone, Default)]
pub struct Metrics {
    inner: Arc<Inner>,
}

#[derive(Default)]
struct Inner {
    spans: Mutex<HashMap<&'static str, RollingWindow>>,
    counters: Mutex<HashMap<&'static str, u64>>,
    gauges: Mutex<HashMap<&'static str, f64>>,
    /// Frames the playback clock asked for but which were not ready in time.
    dropped_frames: AtomicU64,
    presented_frames: AtomicU64,
}

impl Metrics {
    pub fn new() -> Self {
        Metrics::default()
    }

    /// Opens a timing span that records itself when dropped.
    ///
    /// ```
    /// # let metrics = ve_metrics::Metrics::new();
    /// {
    ///     let _span = metrics.span("decode");
    ///     // ... work ...
    /// }
    /// assert_eq!(metrics.span_stats("decode").unwrap().count, 1);
    /// ```
    pub fn span(&self, name: &'static str) -> Span {
        Span { metrics: self.clone(), name, start: Instant::now() }
    }

    /// Records a duration directly, for work timed by other means (a GPU
    /// timestamp query, a duration returned by a worker).
    pub fn record(&self, name: &'static str, elapsed: Duration) {
        self.inner
            .spans
            .lock()
            .entry(name)
            .or_insert_with(|| RollingWindow::new(WINDOW_SAMPLES))
            .push(elapsed);
    }

    pub fn span_stats(&self, name: &str) -> Option<SpanStats> {
        self.inner.spans.lock().get(name).map(RollingWindow::stats)
    }

    pub fn incr(&self, name: &'static str, by: u64) {
        *self.inner.counters.lock().entry(name).or_insert(0) += by;
    }

    pub fn counter(&self, name: &str) -> u64 {
        self.inner.counters.lock().get(name).copied().unwrap_or(0)
    }

    /// Sets a point-in-time value such as a cache fill level or GPU load.
    pub fn set_gauge(&self, name: &'static str, value: f64) {
        self.inner.gauges.lock().insert(name, value);
    }

    pub fn gauge(&self, name: &str) -> Option<f64> {
        self.inner.gauges.lock().get(name).copied()
    }

    /// Records that a frame reached the screen.
    pub fn frame_presented(&self) {
        self.inner.presented_frames.fetch_add(1, Ordering::Relaxed);
    }

    /// Records that a frame the clock wanted was not ready in time.
    ///
    /// This is the single most important playback metric: a renderer that is
    /// fast on average but misses deadlines looks broken, and only a dropped
    /// count exposes that.
    pub fn frame_dropped(&self) {
        self.inner.dropped_frames.fetch_add(1, Ordering::Relaxed);
    }

    pub fn dropped_frames(&self) -> u64 {
        self.inner.dropped_frames.load(Ordering::Relaxed)
    }

    pub fn presented_frames(&self) -> u64 {
        self.inner.presented_frames.load(Ordering::Relaxed)
    }

    /// Resets playback counters at the start of a playback run, so the drop
    /// rate describes the current run rather than the whole session.
    pub fn reset_playback_counters(&self) {
        self.inner.dropped_frames.store(0, Ordering::Relaxed);
        self.inner.presented_frames.store(0, Ordering::Relaxed);
    }

    /// A consistent view of everything, for the overlay and for benchmarks.
    pub fn snapshot(&self) -> MetricsSnapshot {
        let spans = self
            .inner
            .spans
            .lock()
            .iter()
            .map(|(k, v)| (k.to_string(), v.stats()))
            .collect();
        let counters =
            self.inner.counters.lock().iter().map(|(k, v)| (k.to_string(), *v)).collect();
        let gauges = self.inner.gauges.lock().iter().map(|(k, v)| (k.to_string(), *v)).collect();
        MetricsSnapshot {
            spans,
            counters,
            gauges,
            dropped_frames: self.dropped_frames(),
            presented_frames: self.presented_frames(),
            memory: platform::process_memory(),
        }
    }

    pub fn clear(&self) {
        self.inner.spans.lock().clear();
        self.inner.counters.lock().clear();
        self.inner.gauges.lock().clear();
        self.reset_playback_counters();
    }
}

/// An open timing span. Records its elapsed time when dropped.
pub struct Span {
    metrics: Metrics,
    name: &'static str,
    start: Instant,
}

impl Span {
    /// Time elapsed so far, without ending the span.
    pub fn elapsed(&self) -> Duration {
        self.start.elapsed()
    }
}

impl Drop for Span {
    fn drop(&mut self) {
        let elapsed = self.start.elapsed();
        self.metrics.record(self.name, elapsed);
    }
}

/// A consistent readout of every metric.
#[derive(Debug, Clone, Default)]
pub struct MetricsSnapshot {
    pub spans: HashMap<String, SpanStats>,
    pub counters: HashMap<String, u64>,
    pub gauges: HashMap<String, f64>,
    pub dropped_frames: u64,
    pub presented_frames: u64,
    pub memory: Option<ProcessMemory>,
}

impl MetricsSnapshot {
    /// Mean duration of a span in milliseconds, or zero if it never ran.
    pub fn mean_ms(&self, name: &str) -> f64 {
        self.spans.get(name).map(|s| s.mean.as_secs_f64() * 1000.0).unwrap_or(0.0)
    }

    /// Proportion of requested frames that missed their deadline.
    pub fn drop_rate(&self) -> f64 {
        let total = self.dropped_frames + self.presented_frames;
        if total == 0 {
            0.0
        } else {
            self.dropped_frames as f64 / total as f64
        }
    }
}

/// Standard span names, so producers and the overlay cannot drift apart.
pub mod spans {
    pub const FRAME: &str = "frame";
    pub const DECODE: &str = "decode";
    pub const SCALE: &str = "scale";
    pub const UPLOAD: &str = "upload";
    pub const COMPOSITE: &str = "composite";
    pub const PRESENT: &str = "present";
    pub const UI: &str = "ui";
    pub const PROJECT_LOAD: &str = "project_load";
    pub const PROJECT_SAVE: &str = "project_save";
    pub const PROBE: &str = "probe";
    pub const SEEK: &str = "seek";
}

/// Standard counter and gauge names.
pub mod counters {
    pub const FRAME_CACHE_HIT: &str = "frame_cache_hit";
    pub const FRAME_CACHE_MISS: &str = "frame_cache_miss";
    pub const DECODE_REQUESTS: &str = "decode_requests";
    pub const DECODE_CANCELLED: &str = "decode_cancelled";
    pub const SEEKS: &str = "seeks";

    pub const FRAME_CACHE_BYTES: &str = "frame_cache_bytes";
    pub const FRAME_CACHE_ENTRIES: &str = "frame_cache_entries";
    pub const GPU_TEXTURE_BYTES: &str = "gpu_texture_bytes";
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn spans_record_on_drop_and_accumulate() {
        let m = Metrics::new();
        for _ in 0..3 {
            let _s = m.span("work");
            std::thread::yield_now();
        }
        let stats = m.span_stats("work").unwrap();
        assert_eq!(stats.count, 3);
        assert!(stats.max >= stats.min);
        assert!(stats.mean <= stats.max && stats.mean >= stats.min);
        assert!(m.span_stats("never_run").is_none());
    }

    #[test]
    fn directly_recorded_durations_are_summarised() {
        let m = Metrics::new();
        for ms in [10u64, 20, 30, 40] {
            m.record("d", Duration::from_millis(ms));
        }
        let s = m.span_stats("d").unwrap();
        assert_eq!(s.count, 4);
        assert_eq!(s.min, Duration::from_millis(10));
        assert_eq!(s.max, Duration::from_millis(40));
        assert_eq!(s.mean, Duration::from_millis(25));
        assert_eq!(s.last, Duration::from_millis(40));
    }

    #[test]
    fn counters_and_gauges_round_trip() {
        let m = Metrics::new();
        m.incr("hits", 2);
        m.incr("hits", 3);
        assert_eq!(m.counter("hits"), 5);
        assert_eq!(m.counter("absent"), 0);

        m.set_gauge("fill", 0.75);
        assert_eq!(m.gauge("fill"), Some(0.75));
        assert_eq!(m.gauge("absent"), None);
    }

    #[test]
    fn the_drop_rate_reflects_missed_deadlines() {
        let m = Metrics::new();
        assert_eq!(m.snapshot().drop_rate(), 0.0, "no frames yet means no drops");

        for _ in 0..9 {
            m.frame_presented();
        }
        m.frame_dropped();
        let snap = m.snapshot();
        assert_eq!(snap.presented_frames, 9);
        assert_eq!(snap.dropped_frames, 1);
        assert!((snap.drop_rate() - 0.1).abs() < 1e-9);

        m.reset_playback_counters();
        assert_eq!(m.snapshot().drop_rate(), 0.0);
    }

    #[test]
    fn a_snapshot_is_a_consistent_detached_view() {
        let m = Metrics::new();
        m.record("a", Duration::from_millis(5));
        m.incr("c", 1);
        let snap = m.snapshot();

        // Later activity must not mutate an already-taken snapshot.
        m.record("a", Duration::from_millis(500));
        m.incr("c", 100);
        assert_eq!(snap.counters["c"], 1);
        assert!((snap.mean_ms("a") - 5.0).abs() < 0.001);
        assert_eq!(snap.mean_ms("nonexistent"), 0.0);
    }

    #[test]
    fn metrics_are_shared_across_threads() {
        let m = Metrics::new();
        std::thread::scope(|s| {
            for _ in 0..4 {
                let m = m.clone();
                s.spawn(move || {
                    for _ in 0..50 {
                        m.incr("n", 1);
                        m.record("t", Duration::from_micros(10));
                    }
                });
            }
        });
        assert_eq!(m.counter("n"), 200);
        assert_eq!(m.span_stats("t").unwrap().count, 200);
    }

    #[test]
    fn clearing_resets_everything() {
        let m = Metrics::new();
        m.incr("c", 1);
        m.record("s", Duration::from_millis(1));
        m.frame_dropped();
        m.clear();
        assert_eq!(m.counter("c"), 0);
        assert!(m.span_stats("s").is_none());
        assert_eq!(m.dropped_frames(), 0);
    }
}
