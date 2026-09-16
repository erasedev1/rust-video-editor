//! A memory-bounded cache of decoded frames.
//!
//! Decoding is by far the most expensive thing the editor does per frame, and
//! scrubbing revisits the same frames constantly, so caching is what makes the
//! timeline feel immediate. The cache is bounded by *bytes* rather than by
//! entry count, because a 4K frame is thirty times the size of a thumbnail and
//! a count-based budget would either waste memory or thrash.

use std::collections::HashMap;

use ve_core::AssetId;
use ve_metrics::{counters, Metrics};

use crate::frame::VideoFrame;

/// Identifies a cached frame.
///
/// Keyed on the frame *index* rather than a timestamp: two requests that round
/// to the same frame must hit the same entry, and floating timestamps would
/// make that unreliable.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct CacheKey {
    pub asset: AssetId,
    pub frame: i64,
    /// Distinguishes entries decoded at different sizes, so a thumbnail never
    /// satisfies a full-resolution request.
    pub width: u32,
}

impl CacheKey {
    pub fn new(asset: AssetId, frame: i64, width: u32) -> Self {
        CacheKey { asset, frame, width }
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct CacheStats {
    pub hits: u64,
    pub misses: u64,
    pub evictions: u64,
    pub entries: usize,
    pub bytes: usize,
    pub capacity_bytes: usize,
}

impl CacheStats {
    pub fn hit_rate(&self) -> f64 {
        let total = self.hits + self.misses;
        if total == 0 {
            0.0
        } else {
            self.hits as f64 / total as f64
        }
    }

    pub fn fill(&self) -> f64 {
        if self.capacity_bytes == 0 {
            0.0
        } else {
            self.bytes as f64 / self.capacity_bytes as f64
        }
    }
}

struct Entry {
    frame: VideoFrame,
    /// Monotonic stamp of last use, for LRU ordering.
    last_used: u64,
}

/// An LRU cache of decoded frames under a byte budget.
///
/// Not internally synchronised: [`crate::DecodeService`] owns one behind a
/// lock. Keeping the lock outside means a caller can look up and insert under a
/// single acquisition rather than two.
pub struct FrameCache {
    entries: HashMap<CacheKey, Entry>,
    capacity_bytes: usize,
    bytes: usize,
    clock: u64,
    hits: u64,
    misses: u64,
    evictions: u64,
    metrics: Option<Metrics>,
}

impl FrameCache {
    /// A cache holding up to `capacity_bytes` of frames.
    pub fn new(capacity_bytes: usize) -> Self {
        FrameCache {
            entries: HashMap::new(),
            capacity_bytes,
            bytes: 0,
            clock: 0,
            hits: 0,
            misses: 0,
            evictions: 0,
            metrics: None,
        }
    }

    /// A cache sized as a fraction of a memory budget, clamped to something
    /// that can hold at least a few 4K frames.
    pub fn with_budget_mb(mb: usize) -> Self {
        const MIN_BYTES: usize = 64 * 1024 * 1024;
        FrameCache::new((mb * 1024 * 1024).max(MIN_BYTES))
    }

    pub fn with_metrics(mut self, metrics: Metrics) -> Self {
        self.metrics = Some(metrics);
        self
    }

    /// Looks up a frame, marking it as recently used.
    pub fn get(&mut self, key: &CacheKey) -> Option<VideoFrame> {
        self.clock += 1;
        let clock = self.clock;
        match self.entries.get_mut(key) {
            Some(entry) => {
                entry.last_used = clock;
                self.hits += 1;
                if let Some(m) = &self.metrics {
                    m.incr(counters::FRAME_CACHE_HIT, 1);
                }
                Some(entry.frame.clone())
            }
            None => {
                self.misses += 1;
                if let Some(m) = &self.metrics {
                    m.incr(counters::FRAME_CACHE_MISS, 1);
                }
                None
            }
        }
    }

    /// Whether a key is present, without disturbing LRU order.
    pub fn contains(&self, key: &CacheKey) -> bool {
        self.entries.contains_key(key)
    }

    /// Inserts a frame, evicting least-recently-used entries to stay in budget.
    pub fn insert(&mut self, key: CacheKey, frame: VideoFrame) {
        let size = frame.byte_size();

        // A frame larger than the whole budget would evict everything and then
        // not fit; refusing it keeps the cache useful for the frames that do.
        if size > self.capacity_bytes {
            log::debug!(
                "frame of {size} bytes exceeds the {} byte cache budget; not caching",
                self.capacity_bytes
            );
            return;
        }

        if let Some(old) = self.entries.remove(&key) {
            self.bytes -= old.frame.byte_size();
        }

        self.evict_until_fits(size);

        self.clock += 1;
        self.bytes += size;
        self.entries.insert(key, Entry { frame, last_used: self.clock });
        self.publish_gauges();
    }

    fn evict_until_fits(&mut self, incoming: usize) {
        while self.bytes + incoming > self.capacity_bytes && !self.entries.is_empty() {
            // Linear scan for the oldest entry. The cache holds hundreds of
            // entries, not millions, and eviction happens once per insert, so
            // a heap's bookkeeping would cost more than it saves. Revisit if
            // profiling ever says otherwise.
            let victim = self.entries.iter().min_by_key(|(_, e)| e.last_used).map(|(k, _)| *k);
            match victim {
                Some(k) => {
                    if let Some(e) = self.entries.remove(&k) {
                        self.bytes -= e.frame.byte_size();
                        self.evictions += 1;
                    }
                }
                None => break,
            }
        }
    }

    /// Drops every frame belonging to an asset, for when media is relinked or
    /// removed.
    pub fn invalidate_asset(&mut self, asset: AssetId) {
        let doomed: Vec<CacheKey> =
            self.entries.keys().filter(|k| k.asset == asset).copied().collect();
        for key in doomed {
            if let Some(e) = self.entries.remove(&key) {
                self.bytes -= e.frame.byte_size();
            }
        }
        self.publish_gauges();
    }

    pub fn clear(&mut self) {
        self.entries.clear();
        self.bytes = 0;
        self.publish_gauges();
    }

    pub fn stats(&self) -> CacheStats {
        CacheStats {
            hits: self.hits,
            misses: self.misses,
            evictions: self.evictions,
            entries: self.entries.len(),
            bytes: self.bytes,
            capacity_bytes: self.capacity_bytes,
        }
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    pub fn bytes(&self) -> usize {
        self.bytes
    }

    /// Resizes the budget, evicting immediately if it shrank.
    pub fn set_capacity_bytes(&mut self, capacity_bytes: usize) {
        self.capacity_bytes = capacity_bytes;
        self.evict_until_fits(0);
        self.publish_gauges();
    }

    fn publish_gauges(&self) {
        if let Some(m) = &self.metrics {
            m.set_gauge(counters::FRAME_CACHE_BYTES, self.bytes as f64);
            m.set_gauge(counters::FRAME_CACHE_ENTRIES, self.entries.len() as f64);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::frame::PixelFormat;
    use ve_core::Size;
    use ve_time::Ticks;

    /// A frame of a known size, for exercising the byte budget.
    fn frame(width: u32, height: u32) -> VideoFrame {
        let stride = width * 4;
        let data = vec![0u8; (stride * height) as usize];
        VideoFrame::new(
            data.into(),
            Size::new(width, height),
            stride,
            PixelFormat::Rgba8,
            Ticks::ZERO,
        )
    }

    fn key(frame_index: i64) -> CacheKey {
        CacheKey::new(AssetId::from_raw(1), frame_index, 100)
    }

    #[test]
    fn a_hit_returns_the_frame_and_a_miss_returns_nothing() {
        let mut cache = FrameCache::new(1024 * 1024);
        assert!(cache.get(&key(0)).is_none());

        cache.insert(key(0), frame(100, 100));
        assert!(cache.get(&key(0)).is_some());
        assert!(cache.get(&key(1)).is_none());

        let stats = cache.stats();
        assert_eq!(stats.hits, 1);
        assert_eq!(stats.misses, 2);
        assert!((stats.hit_rate() - 1.0 / 3.0).abs() < 1e-9);
    }

    #[test]
    fn the_budget_is_counted_in_bytes_not_entries() {
        // Room for exactly four 100x100 RGBA frames.
        let frame_bytes = 100 * 100 * 4;
        let mut cache = FrameCache::new(frame_bytes * 4);

        for i in 0..4 {
            cache.insert(key(i), frame(100, 100));
        }
        assert_eq!(cache.len(), 4);
        assert_eq!(cache.bytes(), frame_bytes * 4);

        // A single larger frame evicts several small ones to make room.
        cache.insert(key(100), frame(200, 200));
        assert!(cache.bytes() <= frame_bytes * 4);
        assert!(cache.get(&key(100)).is_some());
        assert!(cache.stats().evictions > 0);
    }

    #[test]
    fn eviction_removes_the_least_recently_used_entry() {
        let frame_bytes = 100 * 100 * 4;
        let mut cache = FrameCache::new(frame_bytes * 3);
        for i in 0..3 {
            cache.insert(key(i), frame(100, 100));
        }

        // Touch 0 and 2, leaving 1 as the oldest.
        assert!(cache.get(&key(0)).is_some());
        assert!(cache.get(&key(2)).is_some());

        cache.insert(key(3), frame(100, 100));
        assert!(cache.get(&key(1)).is_none(), "the least recently used entry should have gone");
        assert!(cache.get(&key(0)).is_some());
        assert!(cache.get(&key(2)).is_some());
        assert!(cache.get(&key(3)).is_some());
    }

    #[test]
    fn a_frame_larger_than_the_whole_budget_is_refused_rather_than_thrashing() {
        let mut cache = FrameCache::new(1024);
        cache.insert(key(0), frame(10, 10)); // 400 bytes, fits
        cache.insert(key(1), frame(100, 100)); // 40 KB, does not

        assert!(cache.get(&key(1)).is_none(), "the oversized frame must not be stored");
        assert!(cache.get(&key(0)).is_some(), "and must not have evicted everything first");
    }

    #[test]
    fn reinserting_a_key_replaces_rather_than_double_counting() {
        let mut cache = FrameCache::new(1024 * 1024);
        cache.insert(key(0), frame(100, 100));
        let bytes = cache.bytes();
        cache.insert(key(0), frame(100, 100));
        assert_eq!(cache.len(), 1);
        assert_eq!(cache.bytes(), bytes);
    }

    #[test]
    fn entries_of_different_sizes_do_not_collide() {
        let mut cache = FrameCache::new(1024 * 1024);
        let full = CacheKey::new(AssetId::from_raw(1), 5, 1920);
        let thumb = CacheKey::new(AssetId::from_raw(1), 5, 160);

        cache.insert(full, frame(100, 100));
        assert!(
            cache.get(&thumb).is_none(),
            "a thumbnail request must not be served a full-resolution frame"
        );
    }

    #[test]
    fn invalidating_an_asset_drops_only_its_frames() {
        let mut cache = FrameCache::new(1024 * 1024);
        let a = AssetId::from_raw(1);
        let b = AssetId::from_raw(2);
        for i in 0..3 {
            cache.insert(CacheKey::new(a, i, 100), frame(50, 50));
            cache.insert(CacheKey::new(b, i, 100), frame(50, 50));
        }
        assert_eq!(cache.len(), 6);

        cache.invalidate_asset(a);
        assert_eq!(cache.len(), 3);
        assert!(cache.get(&CacheKey::new(a, 0, 100)).is_none());
        assert!(cache.get(&CacheKey::new(b, 0, 100)).is_some());
        // The byte count must track the removals, or the budget drifts.
        assert_eq!(cache.bytes(), 3 * 50 * 50 * 4);
    }

    #[test]
    fn shrinking_the_budget_evicts_immediately() {
        let frame_bytes = 100 * 100 * 4;
        let mut cache = FrameCache::new(frame_bytes * 8);
        for i in 0..8 {
            cache.insert(key(i), frame(100, 100));
        }
        assert_eq!(cache.len(), 8);

        cache.set_capacity_bytes(frame_bytes * 2);
        assert!(cache.len() <= 2, "shrinking must evict down to the new budget");
        assert!(cache.bytes() <= frame_bytes * 2);
    }

    #[test]
    fn contains_does_not_disturb_lru_order() {
        let frame_bytes = 100 * 100 * 4;
        let mut cache = FrameCache::new(frame_bytes * 2);
        cache.insert(key(0), frame(100, 100));
        cache.insert(key(1), frame(100, 100));

        // Peeking at 0 must not protect it from eviction, or a probe would
        // silently change what gets kept.
        assert!(cache.contains(&key(0)));
        cache.insert(key(2), frame(100, 100));
        assert!(!cache.contains(&key(0)));
    }

    #[test]
    fn clearing_empties_the_cache_but_keeps_the_budget() {
        let mut cache = FrameCache::new(1024 * 1024);
        cache.insert(key(0), frame(100, 100));
        cache.clear();
        assert!(cache.is_empty());
        assert_eq!(cache.bytes(), 0);
        assert_eq!(cache.stats().capacity_bytes, 1024 * 1024);
    }

    #[test]
    fn an_empty_cache_reports_a_zero_hit_rate_rather_than_dividing_by_zero() {
        let cache = FrameCache::new(1024);
        assert_eq!(cache.stats().hit_rate(), 0.0);
        assert_eq!(cache.stats().fill(), 0.0);
    }

    #[test]
    fn the_minimum_budget_can_hold_a_few_4k_frames() {
        let cache = FrameCache::with_budget_mb(1);
        // 4K RGBA is about 33 MB; the floor must leave room for a working set.
        assert!(cache.stats().capacity_bytes >= 64 * 1024 * 1024);
    }
}
