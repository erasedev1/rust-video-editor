//! Drawing a graphic at most once per distinct picture.
//!
//! The same content addressing the render cache uses, one level down: a
//! [`GraphicState`] is every parameter already resolved, so it hashes, and the
//! hash is the key. Two instants that would draw the same picture have the same
//! key by construction — which is what makes a static title free after its
//! first frame — and anything that would change the picture changes the key,
//! so a cached picture cannot go stale.
//!
//! Bounded by bytes rather than by entries, and evicted least-recently-used,
//! for the same reason the frame cache is: what matters is how much memory the
//! pictures occupy, and a thousand small shapes are cheaper to keep than two
//! full-canvas ones.

use std::collections::HashMap;

use ve_core::GraphicState;

use crate::RasterImage;

/// The default budget: enough for a handful of full-canvas graphics and a great
/// many ordinary ones.
const DEFAULT_BUDGET_BYTES: usize = 64 * 1024 * 1024;

struct Entry {
    image: RasterImage,
    last_used: u64,
}

/// What a rasteriser has been doing, for the performance overlay and for tests
/// that assert a picture was *not* drawn twice.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct RasterStats {
    pub hits: u64,
    pub misses: u64,
    pub evictions: u64,
    pub entries: usize,
    pub bytes: usize,
    pub capacity_bytes: usize,
}

impl RasterStats {
    pub fn hit_rate(&self) -> f64 {
        let total = self.hits + self.misses;
        if total == 0 {
            0.0
        } else {
            self.hits as f64 / total as f64
        }
    }
}

/// Draws graphics, keeping what it has drawn.
///
/// Not internally synchronised and not shared: the preview owns one and an
/// export owns another, exactly as they own separate decoders. An export visits
/// each instant once, so its hits come from a graphic that is on screen for
/// more than one frame — which is every title anyone has ever made.
pub struct Rasteriser {
    entries: HashMap<u64, Entry>,
    capacity_bytes: usize,
    bytes: usize,
    clock: u64,
    hits: u64,
    misses: u64,
    evictions: u64,
}

impl Default for Rasteriser {
    fn default() -> Self {
        Rasteriser::new(DEFAULT_BUDGET_BYTES)
    }
}

impl Rasteriser {
    pub fn new(capacity_bytes: usize) -> Self {
        Rasteriser {
            entries: HashMap::new(),
            capacity_bytes: capacity_bytes.max(1),
            bytes: 0,
            clock: 0,
            hits: 0,
            misses: 0,
            evictions: 0,
        }
    }

    pub fn with_budget_mb(mb: usize) -> Self {
        Rasteriser::new(mb * 1024 * 1024)
    }

    /// The picture this state draws, drawing it if it has not been drawn.
    ///
    /// `None` when there is nothing to draw — see [`crate::draw`] — and that
    /// answer is cached too, as an absent entry that costs a rasterisation the
    /// next time. Caching "nothing" is not worth an entry: the case it would
    /// save is a shape keyframed to fully transparent, where drawing nothing is
    /// already the cheap path.
    pub fn picture(&mut self, state: &GraphicState) -> Option<RasterImage> {
        let key = state.content_hash();
        self.clock += 1;
        if let Some(entry) = self.entries.get_mut(&key) {
            entry.last_used = self.clock;
            self.hits += 1;
            return Some(entry.image.clone());
        }
        self.misses += 1;

        let image = crate::draw(state)?;
        self.insert(key, image.clone());
        Some(image)
    }

    fn insert(&mut self, key: u64, image: RasterImage) {
        let size = image.bytes();
        // A picture larger than the whole budget would evict everything and
        // still not fit. It is handed back to the caller all the same — it is
        // drawn, and drawn is what was asked for — but it is not kept.
        if size > self.capacity_bytes {
            return;
        }
        if let Some(old) = self.entries.remove(&key) {
            self.bytes -= old.image.bytes();
        }
        while self.bytes + size > self.capacity_bytes && !self.entries.is_empty() {
            let victim = self.entries.iter().min_by_key(|(_, e)| e.last_used).map(|(k, _)| *k);
            match victim {
                Some(k) => {
                    if let Some(e) = self.entries.remove(&k) {
                        self.bytes -= e.image.bytes();
                        self.evictions += 1;
                    }
                }
                None => break,
            }
        }
        self.clock += 1;
        self.bytes += size;
        self.entries.insert(key, Entry { image, last_used: self.clock });
    }

    /// Whether a state's picture is already in hand, without disturbing LRU
    /// order.
    pub fn contains(&self, state: &GraphicState) -> bool {
        self.entries.contains_key(&state.content_hash())
    }

    pub fn clear(&mut self) {
        self.entries.clear();
        self.bytes = 0;
    }

    pub fn stats(&self) -> RasterStats {
        RasterStats {
            hits: self.hits,
            misses: self.misses,
            evictions: self.evictions,
            entries: self.entries.len(),
            bytes: self.bytes,
            capacity_bytes: self.capacity_bytes,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ve_core::{Rgba, ShapeKind, ShapeState, Vec2};

    fn shape(w: f64) -> GraphicState {
        GraphicState::Shape(ShapeState {
            kind: ShapeKind::Rectangle,
            size: Vec2::new(w, 100.0),
            corner_radius: 0.0,
            inner_radius: 0.382,
            fill: Rgba::WHITE,
            stroke: Rgba::TRANSPARENT,
            stroke_width: 0.0,
        })
    }

    /// The same picture in a different colour: same bytes, different key.
    fn recoloured(shade: f64) -> GraphicState {
        let mut state = shape(100.0);
        if let GraphicState::Shape(s) = &mut state {
            s.fill = Rgba::new(shade, shade, shade, 1.0);
        }
        state
    }

    #[test]
    fn the_same_picture_is_drawn_once() {
        let mut raster = Rasteriser::default();
        let state = shape(200.0);
        let first = raster.picture(&state).expect("drawn");
        let second = raster.picture(&state).expect("cached");
        assert_eq!(raster.stats().hits, 1);
        assert_eq!(raster.stats().misses, 1);
        assert_eq!(first.size(), second.size());
    }

    #[test]
    fn a_changed_parameter_is_a_different_picture() {
        let mut raster = Rasteriser::default();
        raster.picture(&shape(200.0));
        raster.picture(&shape(201.0));
        assert_eq!(raster.stats().misses, 2);
        assert_eq!(raster.stats().entries, 2);
    }

    #[test]
    fn a_budget_evicts_the_least_recently_used() {
        // Room for one 100×100 picture and no more.
        let one = recoloured(1.0);
        let bytes = crate::draw(&one).expect("drawn").bytes();
        let mut raster = Rasteriser::new(bytes);

        raster.picture(&one);
        raster.picture(&recoloured(0.5));
        assert_eq!(raster.stats().entries, 1);
        assert_eq!(raster.stats().evictions, 1);
        assert!(!raster.contains(&one), "the older picture is the one that goes");
    }

    #[test]
    fn a_picture_too_big_for_the_budget_is_still_drawn() {
        let mut raster = Rasteriser::new(1024);
        let image = raster.picture(&shape(400.0));
        assert!(image.is_some(), "drawing is what was asked for; keeping it is the extra");
        assert_eq!(raster.stats().entries, 0);
    }
}
