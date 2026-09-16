//! The playback clock.
//!
//! Playback position is derived from **elapsed wall-clock time**, never
//! accumulated by adding a frame duration per rendered frame. Accumulating
//! would make playback speed depend on how fast the renderer happens to be, so
//! a slow frame would slow the audio down instead of dropping a picture.
//! Deriving from the wall clock means a slow frame drops a frame, which is what
//! every professional editor does and what keeps audio and video together.

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use ve_time::Ticks;

/// A monotonic time source.
///
/// Abstracted so that playback can be driven by a fake clock in tests: timing
/// behaviour that can only be checked by sleeping is behaviour that is not
/// really being checked.
pub trait TimeSource: Send + Sync + std::fmt::Debug {
    /// Monotonic time since an arbitrary fixed epoch.
    fn now(&self) -> Duration;
}

/// The real monotonic clock.
#[derive(Debug)]
pub struct SystemTime {
    epoch: Instant,
}

impl SystemTime {
    pub fn new() -> Self {
        SystemTime { epoch: Instant::now() }
    }
}

impl Default for SystemTime {
    fn default() -> Self {
        SystemTime::new()
    }
}

impl TimeSource for SystemTime {
    fn now(&self) -> Duration {
        self.epoch.elapsed()
    }
}

/// A clock the test drives by hand.
#[derive(Debug, Default)]
pub struct ManualTime {
    nanos: AtomicU64,
}

impl ManualTime {
    pub fn new() -> Arc<Self> {
        Arc::new(ManualTime::default())
    }

    pub fn advance(&self, by: Duration) {
        self.nanos.fetch_add(by.as_nanos() as u64, Ordering::Relaxed);
    }

    pub fn set(&self, to: Duration) {
        self.nanos.store(to.as_nanos() as u64, Ordering::Relaxed);
    }
}

impl TimeSource for ManualTime {
    fn now(&self) -> Duration {
        Duration::from_nanos(self.nanos.load(Ordering::Relaxed))
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TransportState {
    Stopped,
    Playing,
}

/// Maps wall-clock time onto a timeline position.
#[derive(Debug)]
pub struct PlaybackClock {
    source: Arc<dyn TimeSource>,
    state: TransportState,
    /// Timeline position playback started from.
    anchor_position: Ticks,
    /// Wall-clock reading when playback started.
    anchor_wall: Duration,
    /// Playback rate multiplier. Negative plays backwards.
    speed: f64,
    /// Playback stops here, so it cannot run off the end of the sequence.
    limit: Option<Ticks>,
    /// Restart at `loop_start` on reaching the limit instead of stopping.
    looping: bool,
    loop_start: Ticks,
}

impl PlaybackClock {
    pub fn new(source: Arc<dyn TimeSource>) -> Self {
        let now = source.now();
        PlaybackClock {
            source,
            state: TransportState::Stopped,
            anchor_position: Ticks::ZERO,
            anchor_wall: now,
            speed: 1.0,
            limit: None,
            looping: false,
            loop_start: Ticks::ZERO,
        }
    }

    pub fn with_system_time() -> Self {
        PlaybackClock::new(Arc::new(SystemTime::new()))
    }

    pub fn state(&self) -> TransportState {
        self.state
    }

    pub fn is_playing(&self) -> bool {
        self.state == TransportState::Playing
    }

    pub fn speed(&self) -> f64 {
        self.speed
    }

    /// Changes playback rate without jumping position.
    ///
    /// Re-anchors first, so the time already played at the old speed keeps the
    /// position it earned instead of being retroactively rescaled.
    pub fn set_speed(&mut self, speed: f64) {
        let position = self.position();
        self.speed = if speed.is_finite() { speed } else { 1.0 };
        self.reanchor(position);
    }

    /// Stops playback at or before this position.
    pub fn set_limit(&mut self, limit: Option<Ticks>) {
        self.limit = limit;
    }

    pub fn set_looping(&mut self, looping: bool, loop_start: Ticks) {
        self.looping = looping;
        self.loop_start = loop_start.clamp_non_negative();
    }

    /// Starts playing from `from`.
    pub fn play(&mut self, from: Ticks) {
        self.state = TransportState::Playing;
        self.reanchor(from.clamp_non_negative());
    }

    /// Resumes from the current position.
    pub fn resume(&mut self) {
        let position = self.position();
        self.state = TransportState::Playing;
        self.reanchor(position);
    }

    /// Stops, returning where it stopped.
    pub fn stop(&mut self) -> Ticks {
        let position = self.position();
        self.state = TransportState::Stopped;
        self.reanchor(position);
        position
    }

    /// Jumps to a position, playing or not.
    pub fn seek(&mut self, to: Ticks) {
        self.reanchor(to.clamp_non_negative());
    }

    /// The current timeline position.
    ///
    /// Recomputed from the wall clock on every call rather than stored, so the
    /// answer cannot drift no matter how irregularly it is asked.
    pub fn position(&self) -> Ticks {
        match self.state {
            TransportState::Stopped => self.anchor_position,
            TransportState::Playing => {
                let elapsed = self.source.now().saturating_sub(self.anchor_wall);
                // Wall-clock nanoseconds to ticks, in i128 so a long session
                // cannot overflow on the way through.
                let elapsed_ticks = duration_to_ticks(elapsed, self.speed);
                let raw = self.anchor_position + elapsed_ticks;
                self.constrain(raw)
            }
        }
    }

    /// Whether playback has reached its limit, so the caller can stop the
    /// transport and release the audio device.
    pub fn has_reached_limit(&self) -> bool {
        if self.looping {
            return false;
        }
        match (self.state, self.limit) {
            (TransportState::Playing, Some(limit)) => {
                let elapsed = self.source.now().saturating_sub(self.anchor_wall);
                self.anchor_position + duration_to_ticks(elapsed, self.speed) >= limit
            }
            _ => false,
        }
    }

    fn constrain(&self, raw: Ticks) -> Ticks {
        let raw = raw.clamp_non_negative();
        match self.limit {
            Some(limit) if limit > self.loop_start && self.looping => {
                // Wrap into the loop range rather than clamping.
                let span = (limit - self.loop_start).raw();
                if span <= 0 {
                    return self.loop_start;
                }
                let past = (raw - self.loop_start).raw();
                if past < 0 {
                    return self.loop_start;
                }
                Ticks::new(self.loop_start.raw() + past % span)
            }
            Some(limit) => raw.min(limit),
            None => raw,
        }
    }

    fn reanchor(&mut self, position: Ticks) {
        self.anchor_position = position;
        self.anchor_wall = self.source.now();
    }
}

/// Converts a wall-clock duration into timeline ticks at a playback speed.
fn duration_to_ticks(elapsed: Duration, speed: f64) -> Ticks {
    let nanos = elapsed.as_nanos() as i128;
    // TICKS_PER_SECOND / 1e9 is not an integer, so the multiply comes first and
    // the divide second, keeping full precision through the conversion.
    let ticks = nanos * ve_time::TICKS_PER_SECOND as i128 / 1_000_000_000i128;
    if speed == 1.0 {
        return Ticks::new(ticks as i64);
    }
    Ticks::new((ticks as f64 * speed) as i64)
}
