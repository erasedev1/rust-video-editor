//! Drawing the pictures the editor synthesises: shapes and text.
//!
//! Given a [`GraphicState`] — a shape or a run of text with every animated
//! parameter already resolved — this produces an image, and that image is
//! indistinguishable from a decoded frame by the time anything downstream sees
//! it. No I/O beyond reading font files, no GPU, no threads: the same rule
//! `ve-core` follows, and what lets every pixel this crate produces be asserted
//! on in a unit test with no window open.
//!
//! # Why the CPU draws them
//!
//! A shape could be a signed-distance field in a fragment shader and would be
//! resolution independent if it were. Text could not: glyphs are outlines, and
//! turning outlines into coverage is what a rasteriser does. Writing the shapes
//! on the GPU and the text on the CPU would mean two ways to fill a path, two
//! ways to stroke one, and two answers to what a 40% opaque outline over a 60%
//! opaque fill looks like — and the two would eventually disagree.
//!
//! So both are drawn here, by one rasteriser, from paths. A stroke on a title
//! is the same stroke as on a rectangle because it is the same code.
//!
//! # What that costs, and what pays for it
//!
//! A picture is drawn once per *distinct* picture rather than once per frame.
//! Every parameter is resolved before it arrives, so a state hashes, and a
//! hash is the cache key — which means a title that is not animated is drawn
//! once however long it is on screen, and an animated one is drawn once per
//! instant it actually differs at. See [`Rasteriser`].
//!
//! That is also why this is not on a worker thread like decoding is. A decode
//! is unbounded — a disk, a long GOP, a file over a network — so a frame that
//! has not arrived is a dropped frame and playback carries on. A rasterisation
//! is bounded by the graphic's own area and deterministic, and deferring one
//! would not show a late picture but the *wrong* one: an animated shape differs
//! every frame, so "draw it next repaint" means never drawing what is asked
//! for. See `docs/BENCHMARKS.md` for what one costs.
//!
//! # The picture is the graphic's own bounding box
//!
//! Not the canvas. A lower third is drawn into an image the size of the lower
//! third, and the clip's transform places it — exactly as a 1920×1080 frame is
//! placed on a 1920×1080 canvas. So a small shape costs a small rasterisation
//! rather than a full-frame one, and nothing about placing a graphic is
//! different from placing footage.
//!
//! Sizes are in the pixels of the canvas the graphic is drawn on. **Scaling a
//! graphic up with the clip's transform resamples it**, as it would footage; it
//! does not redraw the outline at the larger size. Re-rasterising per transform
//! would make the picture depend on the thing that is meant to place it, and
//! then two clips showing one graphic at different scales would be two
//! rasterisations rather than one cached picture. The established compositors
//! make this a per-layer switch for exactly that reason, and the switch is not
//! here yet.
//!
//! # Straight alpha, like everything else
//!
//! `tiny-skia` composites premultiplied and this hands back straight
//! (non-premultiplied) RGBA8, which is what a decoded frame is. One conversion
//! here means the uploader, the texture cache, the compositor and every blend
//! mode treat a graphic exactly as they treat a frame, with no flag to thread
//! through and no path that can be got wrong. The round trip through 8-bit
//! straight alpha costs at most half a level in premultiplied space, which is
//! below what any blend can show.

mod cache;
mod shape;

pub use cache::{RasterStats, Rasteriser};
pub use shape::shape_path;

use std::sync::Arc;

use ve_core::{GraphicState, Size};

/// The largest picture a graphic is allowed to rasterise into, per axis.
///
/// A project file is untrusted input: a size of `1e9` is a hand edit or a
/// runaway drag, and allocating what it asks for would take the editor down
/// rather than draw something wrong. Two 8K frames on a side is past any canvas
/// the editor supports.
pub const MAX_DIMENSION: u32 = 16384;

/// A rasterised picture: straight RGBA8, rows packed tight.
///
/// The pixels sit behind an [`Arc`] for the same reason a decoded frame's do —
/// handing one to a cache, an uploader and a test costs three pointer copies —
/// and the image is immutable once drawn, so the sharing needs no lock.
#[derive(Clone)]
pub struct RasterImage {
    data: Arc<[u8]>,
    size: Size,
}

impl RasterImage {
    pub fn new(data: Arc<[u8]>, size: Size) -> Self {
        debug_assert_eq!(data.len() as u64, size.pixel_count() * 4);
        RasterImage { data, size }
    }

    #[inline]
    pub fn data(&self) -> &[u8] {
        &self.data
    }

    /// The shared buffer, for passing on without copying.
    #[inline]
    pub fn buffer(&self) -> Arc<[u8]> {
        Arc::clone(&self.data)
    }

    #[inline]
    pub fn size(&self) -> Size {
        self.size
    }

    #[inline]
    pub fn width(&self) -> u32 {
        self.size.width
    }

    #[inline]
    pub fn height(&self) -> u32 {
        self.size.height
    }

    /// Bytes per row. Always tight: nothing here has a reason to align rows,
    /// and a stride that is sometimes padded is a bug waiting for the one
    /// caller that forgets.
    #[inline]
    pub fn stride(&self) -> u32 {
        self.size.width * 4
    }

    pub fn bytes(&self) -> usize {
        self.data.len()
    }

    /// The straight RGBA of one pixel, for tests and for sampling.
    ///
    /// `None` outside the picture, rather than a panic: a caller asking about a
    /// pixel that is not there has a bug, and so does one that gets `[0,0,0,0]`
    /// back and believes it.
    pub fn pixel(&self, x: u32, y: u32) -> Option<[u8; 4]> {
        if x >= self.size.width || y >= self.size.height {
            return None;
        }
        let i = (y as usize * self.size.width as usize + x as usize) * 4;
        Some([self.data[i], self.data[i + 1], self.data[i + 2], self.data[i + 3]])
    }
}

impl std::fmt::Debug for RasterImage {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RasterImage")
            .field("size", &self.size)
            .field("bytes", &self.data.len())
            .finish()
    }
}

/// Draws a graphic, with no cache in front of it.
///
/// `None` when there is nothing to draw: a shape with no area, a colour that is
/// fully transparent with no stroke, a size past [`MAX_DIMENSION`]. Nothing to
/// draw is an ordinary state rather than an error — a title whose fill is
/// keyframed from transparent starts there — so the caller draws no layer and
/// carries on.
pub fn draw(state: &GraphicState) -> Option<RasterImage> {
    match state {
        GraphicState::Shape(shape) => shape::draw_shape(shape),
        // Text lands next; a state that cannot be drawn yet draws nothing
        // rather than drawing something wrong.
        GraphicState::Text(_) => None,
    }
}

/// Turns a `tiny-skia` pixmap into a [`RasterImage`].
///
/// The one place premultiplied becomes straight, so there is one answer to what
/// a graphic's alpha means.
fn image_from_pixmap(pixmap: tiny_skia::Pixmap) -> RasterImage {
    let size = Size::new(pixmap.width(), pixmap.height());
    let data: Arc<[u8]> = pixmap.take_demultiplied().into();
    RasterImage::new(data, size)
}

/// Converts a model colour to `tiny-skia`'s, clamping what a hand-edited file
/// might hold.
fn colour(c: ve_core::Rgba) -> tiny_skia::Color {
    tiny_skia::Color::from_rgba(
        c.r.clamp(0.0, 1.0) as f32,
        c.g.clamp(0.0, 1.0) as f32,
        c.b.clamp(0.0, 1.0) as f32,
        c.a.clamp(0.0, 1.0) as f32,
    )
    .unwrap_or(tiny_skia::Color::TRANSPARENT)
}
