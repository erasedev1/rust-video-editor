use std::time::Duration;

/// A fixed-capacity ring of recent samples.
///
/// Fixed capacity so that instrumentation cannot grow without bound over a long
/// session, and so that the statistics describe recent behaviour rather than a
/// lifetime average that hides a regression.
#[derive(Debug, Clone)]
pub struct RollingWindow {
    samples: Vec<Duration>,
    capacity: usize,
    next: usize,
    /// Total ever pushed, which keeps counting after the ring wraps.
    total_count: u64,
}

impl RollingWindow {
    pub fn new(capacity: usize) -> Self {
        let capacity = capacity.max(1);
        RollingWindow {
            samples: Vec::with_capacity(capacity),
            capacity,
            next: 0,
            total_count: 0,
        }
    }

    pub fn push(&mut self, sample: Duration) {
        if self.samples.len() < self.capacity {
            self.samples.push(sample);
        } else {
            self.samples[self.next] = sample;
        }
        self.next = (self.next + 1) % self.capacity;
        self.total_count += 1;
    }

    pub fn is_empty(&self) -> bool {
        self.samples.is_empty()
    }

    /// The most recently pushed sample.
    pub fn last(&self) -> Option<Duration> {
        if self.samples.is_empty() {
            return None;
        }
        let idx = (self.next + self.capacity - 1) % self.capacity;
        self.samples.get(idx).copied()
    }

    pub fn stats(&self) -> SpanStats {
        if self.samples.is_empty() {
            return SpanStats::default();
        }
        let mut sorted = self.samples.clone();
        sorted.sort_unstable();

        let sum: Duration = self.samples.iter().sum();
        let mean = sum / self.samples.len() as u32;
        // Nearest-rank p95: the smallest sample at or above the 95th percentile.
        let p95_index = ((sorted.len() as f64 * 0.95).ceil() as usize).saturating_sub(1);

        SpanStats {
            count: self.total_count,
            last: self.last().unwrap_or_default(),
            mean,
            min: sorted[0],
            max: sorted[sorted.len() - 1],
            p95: sorted[p95_index.min(sorted.len() - 1)],
        }
    }
}

/// Summary statistics for one timing span.
///
/// `p95` matters more than `mean` for interactive work: a mean of 5 ms with a
/// p95 of 40 ms is a stutter the user will notice, and the mean alone hides it.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct SpanStats {
    /// Total samples ever recorded, not just those still in the window.
    pub count: u64,
    pub last: Duration,
    pub mean: Duration,
    pub min: Duration,
    pub max: Duration,
    pub p95: Duration,
}

impl SpanStats {
    pub fn mean_ms(&self) -> f64 {
        self.mean.as_secs_f64() * 1000.0
    }
    pub fn last_ms(&self) -> f64 {
        self.last.as_secs_f64() * 1000.0
    }
    pub fn p95_ms(&self) -> f64 {
        self.p95.as_secs_f64() * 1000.0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_empty_window_reports_zeroes() {
        let w = RollingWindow::new(4);
        assert!(w.is_empty());
        assert_eq!(w.last(), None);
        assert_eq!(w.stats(), SpanStats::default());
    }

    #[test]
    fn the_window_wraps_and_keeps_only_recent_samples() {
        let mut w = RollingWindow::new(3);
        for ms in 1..=10u64 {
            w.push(Duration::from_millis(ms));
        }
        let s = w.stats();
        assert_eq!(s.count, 10, "the total keeps counting past the window");
        // Only the last three samples remain: 8, 9, 10 ms.
        assert_eq!(s.min, Duration::from_millis(8));
        assert_eq!(s.max, Duration::from_millis(10));
        assert_eq!(s.last, Duration::from_millis(10));
        assert_eq!(s.mean, Duration::from_millis(9));
    }

    #[test]
    fn p95_picks_out_the_tail_that_the_mean_hides() {
        let mut w = RollingWindow::new(100);
        for _ in 0..90 {
            w.push(Duration::from_millis(1));
        }
        for _ in 0..10 {
            w.push(Duration::from_millis(100));
        }
        let s = w.stats();
        // The mean stays under 11 ms and looks healthy, while the p95 reports
        // the 100 ms stutter that a tenth of frames actually hit.
        assert!(s.mean < Duration::from_millis(11), "{:?}", s.mean);
        assert_eq!(s.p95, Duration::from_millis(100));
        assert_eq!(s.max, Duration::from_millis(100));

        // Nearest-rank is exact at the boundary: with exactly 95% of samples
        // at or below 1 ms, p95 is 1 ms and not the tail.
        let mut w = RollingWindow::new(100);
        for _ in 0..95 {
            w.push(Duration::from_millis(1));
        }
        for _ in 0..5 {
            w.push(Duration::from_millis(100));
        }
        assert_eq!(w.stats().p95, Duration::from_millis(1));
    }

    #[test]
    fn a_single_sample_is_its_own_everything() {
        let mut w = RollingWindow::new(10);
        w.push(Duration::from_millis(7));
        let s = w.stats();
        assert_eq!((s.min, s.max, s.mean, s.p95, s.last), {
            let d = Duration::from_millis(7);
            (d, d, d, d, d)
        });
    }

    #[test]
    fn capacity_is_never_zero() {
        let mut w = RollingWindow::new(0);
        w.push(Duration::from_millis(1));
        assert_eq!(w.stats().count, 1);
    }
}
