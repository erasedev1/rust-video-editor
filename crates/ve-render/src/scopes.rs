//! Video scopes: the picture counted rather than looked at.
//!
//! A grade cannot be judged by eye alone. A monitor's own contrast, the light
//! in the room and the last ten minutes of looking at the same shot all move
//! what "neutral" looks like, and none of them move where the numbers are. So
//! a colourist works against instruments, and these are those instruments:
//!
//! * a **waveform**, which plots code value against horizontal position, and
//!   answers where the blacks and the whites of the frame actually sit;
//! * a **vectorscope**, which plots the chroma plane, and answers which way the
//!   frame is pushed and by how much;
//! * a **histogram**, which counts codes without regard to where they are, and
//!   answers how the range is being used.
//!
//! # They read the picture, not the plan
//!
//! Every scope here reads the *composited* frame — after the grade, after every
//! effect, after the blend with whatever is underneath. That is the only
//! reading worth having: what is being judged is the picture that will be
//! delivered, not an estimate assembled from the parameters that made it.
//!
//! # Small on purpose
//!
//! The picture is read back at a few hundred pixels across rather than at the
//! sequence's own resolution. Two reasons, and the second is the one that
//! matters:
//!
//! * a scope is a *statistic*. Thirty thousand samples locate a black level to
//!   far better than the width of a line on screen, and a million do not locate
//!   it better enough to see;
//! * reading a 1920×1080 target back costs eight megabytes over the bus and a
//!   pipeline stall to wait for it, **every frame**. At 256 across it is under
//!   a tenth of that, and the cost of a scope is then bounded by the scope
//!   rather than by the resolution of the sequence being cut.
//!
//! The consequence is stated rather than hidden: a scope here will not show a
//! single stray hot pixel. It is an instrument for judging a grade, not for
//! auditing a delivery.
//!
//! # Straight colour, and coverage ignored
//!
//! A target holds premultiplied colour, so every reading divides the alpha back
//! out first: a half-covered white pixel is *white*, and a scope that plotted
//! it at half would report a grade that does not exist. A pixel with no
//! coverage at all has no colour to report and is left out of the count
//! entirely rather than being counted as black — otherwise a title over
//! nothing would bury its own waveform under a floor of zeros.

use ve_core::{ColorSpace, Size};

use crate::cache::CompositeKey;
use crate::renderer::{Layer, Renderer};
use crate::target::RenderTarget;
use crate::texture::GpuTexture;
use crate::transform::fit_scale;
use ve_core::TransformState;

/// Rec. 709 luma, the weights the rest of the pipeline uses.
const LUMA: [f64; 3] = [0.2126, 0.7152, 0.0722];

/// How wide the picture is read back for the scopes to count.
///
/// 256 columns is one per column of a waveform plot at its natural width, so
/// nothing is thrown away by the plotting that the sampling kept.
pub const SAMPLE_WIDTH: u32 = 256;

/// One frame, read back small, ready to be counted.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ScopeSample {
    /// Tightly packed premultiplied RGBA8, as a render target reads back.
    pixels: Vec<u8>,
    size: Size,
}

impl ScopeSample {
    pub fn new(pixels: Vec<u8>, size: Size) -> Self {
        ScopeSample { pixels, size }
    }

    pub fn size(&self) -> Size {
        self.size
    }

    pub fn is_empty(&self) -> bool {
        self.pixels.is_empty() || self.size.width == 0 || self.size.height == 0
    }

    /// The read-back bytes, for a caller that wants to notice the picture
    /// changed without counting the whole of it again.
    pub fn bytes(&self) -> &[u8] {
        &self.pixels
    }

    /// Every pixel that has any coverage, as straight colour in 0..1.
    ///
    /// The iterator yields `(column, [r, g, b])`. Column rather than `(x, y)`
    /// because a waveform is the only scope that cares where a pixel was, and
    /// it only cares about one axis.
    fn opaque_pixels(&self) -> impl Iterator<Item = (u32, [f64; 3])> + '_ {
        let width = self.size.width.max(1);
        // The column is counted rather than derived from the index. `i % width`
        // reads better and is an integer division per pixel of every sample of
        // every scope, which measured as most of what counting a waveform cost.
        let mut column = 0;
        self.pixels.as_chunks::<4>().0.iter().filter_map(move |px| {
            let x = column;
            column += 1;
            if column == width {
                column = 0;
            }
            if px[3] == 0 {
                return None;
            }
            // One reciprocal and three multiplies rather than three divisions,
            // for the same reason.
            let inv = 1.0 / px[3] as f64;
            Some((x, [px[0] as f64 * inv, px[1] as f64 * inv, px[2] as f64 * inv]))
        })
    }
}

/// A scope's plot, as an image the interface uploads and draws.
///
/// An image rather than a list of points because that is what a scope *is*: a
/// count per cell of a grid, which is a picture. Handing the interface tens of
/// thousands of points to stroke would cost more to draw than to compute.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ScopeImage {
    /// Tightly packed RGBA8.
    pub pixels: Vec<u8>,
    pub size: Size,
}

impl ScopeImage {
    fn blank(size: Size) -> Self {
        ScopeImage { pixels: vec![0; (size.width as usize) * (size.height as usize) * 4], size }
    }

    /// The pixel at `(x, y)`, for tests and for the panel's readouts.
    pub fn pixel_at(&self, x: u32, y: u32) -> [u8; 4] {
        let i = ((y * self.size.width + x) * 4) as usize;
        [self.pixels[i], self.pixels[i + 1], self.pixels[i + 2], self.pixels[i + 3]]
    }
}

/// Which reading a waveform shows.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum WaveformMode {
    /// Rec. 709 luma alone: the one that answers "is this shot exposed".
    #[default]
    Luma,
    /// Red, green and blue in one plot, each in its own colour. Where the three
    /// traces separate is where the picture has a cast.
    Overlay,
    /// The same three, side by side. Harder to compare, easier to read, and the
    /// one to reach for when a single channel is clipping.
    Parade,
}

impl WaveformMode {
    pub const ALL: [WaveformMode; 3] =
        [WaveformMode::Luma, WaveformMode::Overlay, WaveformMode::Parade];

    pub fn label(self) -> &'static str {
        match self {
            WaveformMode::Luma => "Luma",
            WaveformMode::Overlay => "RGB",
            WaveformMode::Parade => "Parade",
        }
    }
}

/// Rounds a non-negative number to the nearest whole one, as an index.
///
/// `f64::round` is not one instruction: it rounds half *away from zero*, which
/// the hardware's rounding mode does not do, so it compiles to a call. Adding a
/// half and truncating is the same answer for a number that cannot be negative,
/// and it is the single `cvttsd2si` the hardware does have. Measured as most of
/// what counting a scope cost — every plot rounds at least once per pixel of
/// every sample.
#[inline]
fn round_index(v: f64) -> u32 {
    (v + 0.5) as u32
}

/// How many pixels have to land in one cell for it to read full brightness.
///
/// A fraction of the sample's height rather than a constant, so the plot looks
/// the same whatever the frame's aspect ratio: an eighth of a column at one
/// code is a flat area, and a flat area is what a scope draws solid.
fn saturation_count(rows: u32) -> f64 {
    (rows as f64 / 8.0).max(1.0)
}

/// Adds one count to a cell of an accumulating plot, in the given channels.
fn plot(counts: &mut [u32], size: Size, x: u32, y: u32) {
    if x >= size.width || y >= size.height {
        return;
    }
    counts[(y * size.width + x) as usize] += 1;
}

/// Turns accumulated counts into an image, tinted `colour` and added into
/// whatever is already there.
///
/// Added rather than written because the overlay mode draws three traces into
/// one plot, and where they coincide the result should be the white a
/// neutral frame gives — which is what addition says and what replacement
/// would not.
fn compose_counts(image: &mut ScopeImage, counts: &[u32], colour: [f64; 3], rows: u32) {
    let full = saturation_count(rows);
    for (i, count) in counts.iter().enumerate() {
        if *count == 0 {
            continue;
        }
        let intensity = (*count as f64 / full).min(1.0);
        for (c, weight) in colour.iter().enumerate() {
            let added = intensity * weight * 255.0;
            let existing = image.pixels[i * 4 + c] as f64;
            image.pixels[i * 4 + c] = (existing + added).min(255.0) as u8;
        }
        image.pixels[i * 4 + 3] = 255;
    }
}

/// The waveform of `sample`, plotted into an image of `size`.
///
/// Horizontal position is the frame's own horizontal position, so a feature on
/// the left of the picture is on the left of the scope. Vertical is code value
/// with black at the **bottom**, which is the convention every waveform monitor
/// has used since they were built out of a cathode ray tube pointed at a
/// voltage.
pub fn waveform(sample: &ScopeSample, mode: WaveformMode, size: Size) -> ScopeImage {
    let mut image = ScopeImage::blank(size);
    if sample.is_empty() || size.width == 0 || size.height == 0 {
        return image;
    }
    let rows = sample.size.height;
    let columns = sample.size.width.max(1);
    let cells = (size.width as usize) * (size.height as usize);

    // Where a pixel's column lands, and which of the plot's bands it lands in.
    let bands: &[([f64; 3], usize)] = match mode {
        WaveformMode::Luma => &[([1.0, 1.0, 1.0], 0)],
        WaveformMode::Overlay => {
            &[([1.0, 0.25, 0.25], 0), ([0.25, 1.0, 0.25], 0), ([0.25, 0.25, 1.0], 0)]
        }
        WaveformMode::Parade => {
            &[([1.0, 0.3, 0.3], 0), ([0.3, 1.0, 0.3], 1), ([0.3, 0.3, 1.0], 2)]
        }
    };
    let band_count = if mode == WaveformMode::Parade { 3 } else { 1 };
    let band_width = size.width / band_count as u32;

    // One buffer for every band, cleared between them: a parade is three
    // passes over the same grid, and allocating a megabyte per pass to zero it
    // again is most of what counting a waveform would otherwise cost.
    let mut counts = vec![0u32; cells];
    for (index, (colour, band)) in bands.iter().enumerate() {
        counts.fill(0);
        for (column, rgb) in sample.opaque_pixels() {
            let value = match mode {
                WaveformMode::Luma => LUMA[0] * rgb[0] + LUMA[1] * rgb[1] + LUMA[2] * rgb[2],
                _ => rgb[index],
            };
            // Black at the bottom: the row index runs the other way from the
            // code value, which is the one inversion in the whole module and
            // therefore worth saying out loud.
            let level = value.clamp(0.0, 1.0);
            let y = size.height
                - 1
                - round_index(level * (size.height - 1) as f64).min(size.height - 1);

            // A source column covers a *span* of the plot's columns, not one of
            // them. When the plot is wider than the sample — which it is
            // whenever a scope is drawn larger than it is sampled — plotting a
            // single column would leave the ones between empty, and a flat grey
            // field would draw as a comb rather than as a line.
            let base = *band as u32 * band_width;
            let x0 = base + (column * band_width) / columns;
            let x1 = (base + ((column + 1) * band_width) / columns).max(x0 + 1);
            for x in x0..x1 {
                plot(&mut counts, size, x, y);
            }
        }
        compose_counts(&mut image, &counts, *colour, rows);
    }
    image
}

/// The vectorscope of `sample`: the chroma plane, plotted into a square.
///
/// The centre is neutral, distance from it is saturation and the angle is hue.
/// A frame with a cast sits off-centre in the direction of the cast, which is
/// the reading a waveform cannot give and a histogram cannot give either.
///
/// Plotted monochrome. The colour of a vectorscope's trace carries no
/// information — position already says which hue it is — and colouring it would
/// make a trace over a magenta graticule line hard to see.
pub fn vectorscope(sample: &ScopeSample, size: Size) -> ScopeImage {
    let mut image = ScopeImage::blank(size);
    if sample.is_empty() || size.width == 0 || size.height == 0 {
        return image;
    }
    let cells = (size.width as usize) * (size.height as usize);
    let mut counts = vec![0u32; cells];

    for (_, rgb) in sample.opaque_pixels() {
        let (cb, cr) = chroma(rgb);
        // Cb and Cr each run −0.5..0.5, and the plot is square, so both map the
        // same way. Cb is right and Cr is up, which is what puts the six
        // primaries where a vectorscope's graticule has always had them: red at
        // about 104°, magenta at 61°, blue at 347°.
        // Measured out from the middle rather than in from the left, so that
        // neutral lands exactly on a pixel. Mapping the −0.5..0.5 range onto
        // 0..width−1 would put it on the boundary between four of them, and the
        // last bit of rounding noise in a grey would then scatter what should
        // be one dot across a smudge.
        let centre = (size.width as f64 / 2.0, size.height as f64 / 2.0);
        // Clamped before rounding, because the rounding here only holds for a
        // number that is already inside the plot.
        let x = (centre.0 + cb * (size.width - 1) as f64).clamp(0.0, (size.width - 1) as f64);
        let y = (centre.1 - cr * (size.height - 1) as f64).clamp(0.0, (size.height - 1) as f64);
        plot(
            &mut counts,
            size,
            round_index(x).min(size.width - 1),
            round_index(y).min(size.height - 1),
        );
    }
    compose_counts(&mut image, &counts, [0.4, 1.0, 0.5], sample.size.height);
    image
}

/// Rec. 709 colour difference, each in −0.5..0.5 with neutral at zero.
pub fn chroma(rgb: [f64; 3]) -> (f64, f64) {
    let luma = LUMA[0] * rgb[0] + LUMA[1] * rgb[1] + LUMA[2] * rgb[2];
    // The standard normalisations, which are what put the six primary targets
    // on a circle rather than on an ellipse.
    let cb = (rgb[2] - luma) * (1.0 / 1.8556);
    let cr = (rgb[0] - luma) * (1.0 / 1.5748);
    (cb, cr)
}

/// How many pixels carry each code, per channel.
///
/// The one scope that throws away position entirely, which is exactly what
/// makes it the one that answers "how much of the range is this shot using".
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Histogram {
    pub red: [u32; 256],
    pub green: [u32; 256],
    pub blue: [u32; 256],
    pub luma: [u32; 256],
}

impl Default for Histogram {
    fn default() -> Self {
        Histogram { red: [0; 256], green: [0; 256], blue: [0; 256], luma: [0; 256] }
    }
}

impl Histogram {
    pub fn of(sample: &ScopeSample) -> Self {
        let mut histogram = Histogram::default();
        for (_, rgb) in sample.opaque_pixels() {
            let bin = |v: f64| round_index(v.clamp(0.0, 1.0) * 255.0) as usize;
            histogram.red[bin(rgb[0])] += 1;
            histogram.green[bin(rgb[1])] += 1;
            histogram.blue[bin(rgb[2])] += 1;
            histogram.luma[bin(LUMA[0] * rgb[0] + LUMA[1] * rgb[1] + LUMA[2] * rgb[2])] += 1;
        }
        histogram
    }

    /// The tallest bin in any channel, which is what the plot is scaled to.
    ///
    /// One scale across all four, so the channels can be compared against each
    /// other. Scaling each to its own peak would make a channel that is barely
    /// present look as tall as one that fills the frame.
    pub fn peak(&self) -> u32 {
        [&self.red, &self.green, &self.blue, &self.luma]
            .iter()
            .filter_map(|channel| channel.iter().copied().max())
            .max()
            .unwrap_or(0)
    }

    pub fn is_empty(&self) -> bool {
        self.peak() == 0
    }
}

/// Reads the composited picture back, small, for the scopes to count.
///
/// Owns the small target the picture is scaled into and remembers which picture
/// is in it, so a held frame — the usual case while a grade is being dialled
/// rather than played — is read back once and counted once.
pub struct ScopeSampler {
    target: RenderTarget,
    /// The composite currently in the target, so an unchanged frame is not
    /// read back again.
    sampled: Option<CompositeKey>,
    sample: ScopeSample,
}

impl ScopeSampler {
    /// A sampler whose readback is `SAMPLE_WIDTH` across, at `aspect`.
    pub fn new(device: &wgpu::Device, aspect: Size) -> Self {
        let size = Self::sample_size(aspect);
        ScopeSampler {
            target: RenderTarget::new(device, size),
            sampled: None,
            sample: ScopeSample::default(),
        }
    }

    /// The readback size for a composition of `aspect`.
    ///
    /// Fixed width, height from the aspect ratio, so the sample is the same
    /// shape as the picture and a waveform's columns line up with the frame's.
    pub fn sample_size(aspect: Size) -> Size {
        if aspect.width == 0 || aspect.height == 0 {
            return Size::new(SAMPLE_WIDTH, 1);
        }
        let height = (SAMPLE_WIDTH as u64 * aspect.height as u64) / aspect.width as u64;
        Size::new(SAMPLE_WIDTH, (height as u32).max(1))
    }

    pub fn sample(&self) -> &ScopeSample {
        &self.sample
    }

    /// Scales `source` into the small target and reads it back.
    ///
    /// Returns `false` and does nothing at all when `key` is the picture
    /// already sampled — which is the common case, because a grade is dialled
    /// in on a held frame and the same composite is presented on every repaint.
    pub fn read(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        renderer: &mut Renderer,
        space: ColorSpace,
        source: &GpuTexture,
        key: CompositeKey,
    ) -> bool {
        if self.sampled == Some(key) {
            return false;
        }
        let wanted = Self::sample_size(source.size());
        if self.target.resize(device, wanted) {
            self.sampled = None;
        }

        // Scaled to fit rather than cropped: a scope reads the whole frame, and
        // one that quietly left the edges out would under-report exactly the
        // vignette and the lens fall-off a colourist is looking for.
        let transform = TransformState {
            scale: fit_scale(source.size(), self.target.size()),
            ..TransformState::default()
        };
        let layer = Layer::new(source).with_transform(transform);
        renderer.render(
            device,
            queue,
            &self.target,
            ve_core::Rgba::TRANSPARENT,
            space,
            &[layer],
        );

        self.sample =
            ScopeSample::new(self.target.read_pixels(device, queue), self.target.size());
        self.sampled = Some(key);
        true
    }

    /// Forgets what was sampled, so the next read happens even if the same
    /// picture is still on screen. Called when the scopes are switched on.
    pub fn invalidate(&mut self) {
        self.sampled = None;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const PLOT: Size = Size::new(256, 256);

    /// A sample whose pixel at `(x, y)` is whatever `paint` says, premultiplied
    /// as a render target holds it.
    fn sample(size: Size, paint: impl Fn(u32, u32) -> [u8; 4]) -> ScopeSample {
        let mut pixels = Vec::with_capacity((size.width * size.height * 4) as usize);
        for y in 0..size.height {
            for x in 0..size.width {
                pixels.extend_from_slice(&paint(x, y));
            }
        }
        ScopeSample::new(pixels, size)
    }

    fn solid(size: Size, colour: [u8; 4]) -> ScopeSample {
        sample(size, |_, _| colour)
    }

    /// The rows of column `x` that have anything plotted in them.
    fn lit_rows(image: &ScopeImage, x: u32) -> Vec<u32> {
        (0..image.size.height).filter(|y| image.pixel_at(x, *y)[3] > 0).collect()
    }

    #[test]
    fn a_flat_field_plots_one_flat_line() {
        let grey = solid(Size::new(64, 64), [128, 128, 128, 255]);
        let image = waveform(&grey, WaveformMode::Luma, PLOT);
        for x in [0, 128, 255] {
            assert_eq!(lit_rows(&image, x).len(), 1, "one level at column {x}");
        }
    }

    #[test]
    fn black_is_at_the_bottom_and_white_at_the_top() {
        // The one inversion in the module, and the one every waveform monitor
        // has had since they were voltmeters.
        let black =
            waveform(&solid(Size::new(64, 64), [0, 0, 0, 255]), WaveformMode::Luma, PLOT);
        let white =
            waveform(&solid(Size::new(64, 64), [255, 255, 255, 255]), WaveformMode::Luma, PLOT);
        assert_eq!(lit_rows(&black, 128), vec![PLOT.height - 1], "black on the floor");
        assert_eq!(lit_rows(&white, 128), vec![0], "white on the ceiling");
    }

    #[test]
    fn a_flat_field_reads_full_brightness() {
        // A whole column at one code is the densest thing a frame can produce,
        // and a scope draws it solid rather than dim.
        let grey = solid(Size::new(64, 64), [128, 128, 128, 255]);
        let image = waveform(&grey, WaveformMode::Luma, PLOT);
        let row = lit_rows(&image, 10)[0];
        assert_eq!(image.pixel_at(10, row), [255, 255, 255, 255]);
    }

    #[test]
    fn a_horizontal_ramp_plots_a_diagonal() {
        // Position on the scope is position in the frame, so a picture that
        // gets brighter to the right draws a line that climbs to the right.
        let ramp = sample(Size::new(256, 16), |x, _| {
            let v = x as u8;
            [v, v, v, 255]
        });
        let image = waveform(&ramp, WaveformMode::Luma, PLOT);
        let left = lit_rows(&image, 4)[0];
        let middle = lit_rows(&image, 128)[0];
        let right = lit_rows(&image, 250)[0];
        assert!(left > middle && middle > right, "climbing: {left} {middle} {right}");
    }

    #[test]
    fn a_parade_puts_each_channel_in_its_own_third() {
        // Pure red: the red trace is at the top of the first band and the other
        // two are on the floor of theirs.
        let red = solid(Size::new(64, 64), [255, 0, 0, 255]);
        let image = waveform(&red, WaveformMode::Parade, PLOT);
        let third = PLOT.width / 3;
        assert_eq!(lit_rows(&image, third / 2), vec![0], "red band full");
        assert_eq!(lit_rows(&image, third + third / 2), vec![PLOT.height - 1], "green empty");
        assert_eq!(
            lit_rows(&image, 2 * third + third / 2),
            vec![PLOT.height - 1],
            "blue empty"
        );
    }

    #[test]
    fn an_overlay_draws_a_neutral_frame_white_and_a_cast_apart() {
        let grey = solid(Size::new(64, 64), [128, 128, 128, 255]);
        let image = waveform(&grey, WaveformMode::Overlay, PLOT);
        let row = lit_rows(&image, 10)[0];
        // Three traces coincide, so the three colours add back to white. That
        // is the whole reading: where they separate is where the cast is.
        assert_eq!(lit_rows(&image, 10).len(), 1, "one trace, not three");
        let px = image.pixel_at(10, row);
        assert!(px[0] > 200 && px[1] > 200 && px[2] > 200, "neutral reads white: {px:?}");

        let warm = solid(Size::new(64, 64), [200, 128, 64, 255]);
        let cast = waveform(&warm, WaveformMode::Overlay, PLOT);
        assert_eq!(lit_rows(&cast, 10).len(), 3, "three separated traces");
    }

    #[test]
    fn a_neutral_frame_sits_at_the_centre_of_the_vectorscope() {
        // Every grey has zero chroma, so a whole greyscale ramp is still one
        // dot — which is exactly the reading that makes the instrument useful.
        let ramp = sample(Size::new(256, 16), |x, _| {
            let v = x as u8;
            [v, v, v, 255]
        });
        let image = vectorscope(&ramp, PLOT);
        let lit: Vec<(u32, u32)> = (0..PLOT.height)
            .flat_map(|y| (0..PLOT.width).map(move |x| (x, y)))
            .filter(|(x, y)| image.pixel_at(*x, *y)[3] > 0)
            .collect();
        assert_eq!(lit.len(), 1, "a neutral frame is one dot");
        assert_eq!(
            lit[0],
            (PLOT.width / 2, PLOT.height / 2),
            "and it is exactly in the middle"
        );
    }

    #[test]
    fn complementary_colours_sit_exactly_opposite_each_other() {
        // The property a graticule is built on, and the one worth asserting:
        // chroma is linear in RGB and neutral is the origin, so `white − c` is
        // exactly `−chroma(c)`. Red faces cyan, green faces magenta, blue faces
        // yellow, through the middle, every time.
        for rgb in [[1.0, 0.0, 0.0], [0.0, 1.0, 0.0], [0.0, 0.0, 1.0]] {
            let (cb, cr) = chroma(rgb);
            let (ocb, ocr) = chroma([1.0 - rgb[0], 1.0 - rgb[1], 1.0 - rgb[2]]);
            assert!((cb + ocb).abs() < 1e-12 && (cr + ocr).abs() < 1e-12, "{rgb:?}");
        }
    }

    #[test]
    fn the_primaries_spread_round_the_circle_in_the_usual_order() {
        // Not a table of SMPTE angles — this plots Rec. 709 colour difference
        // and says so — but the layout a colourist reads: red up and to the
        // left, blue to the right, green down and to the left, and the six
        // spread rather than bunched.
        let angle = |rgb: [f64; 3]| {
            let (cb, cr) = chroma(rgb);
            cr.atan2(cb).to_degrees().rem_euclid(360.0)
        };
        let red = angle([1.0, 0.0, 0.0]);
        let green = angle([0.0, 1.0, 0.0]);
        let blue = angle([0.0, 0.0, 1.0]);
        assert!((90.0..180.0).contains(&red), "red is up and left: {red}");
        assert!((180.0..270.0).contains(&green), "green is down and left: {green}");
        assert!(!(90.0..270.0).contains(&blue), "blue is to the right: {blue}");

        let mut angles: Vec<f64> = [
            [1.0, 0.0, 0.0],
            [0.0, 1.0, 0.0],
            [0.0, 0.0, 1.0],
            [1.0, 1.0, 0.0],
            [0.0, 1.0, 1.0],
            [1.0, 0.0, 1.0],
        ]
        .into_iter()
        .map(angle)
        .collect();
        angles.sort_by(|a, b| a.partial_cmp(b).unwrap());
        for pair in angles.windows(2) {
            assert!(pair[1] - pair[0] > 20.0, "the six are spread: {angles:?}");
        }
    }

    #[test]
    fn a_saturated_frame_sits_further_out_than_a_washed_one() {
        let saturated = solid(Size::new(32, 32), [255, 0, 0, 255]);
        let washed = solid(Size::new(32, 32), [200, 128, 128, 255]);
        let radius = |s: &ScopeSample| {
            let image = vectorscope(s, PLOT);
            let centre = ((PLOT.width - 1) as f64 / 2.0, (PLOT.height - 1) as f64 / 2.0);
            (0..PLOT.height)
                .flat_map(|y| (0..PLOT.width).map(move |x| (x, y)))
                .filter(|(x, y)| image.pixel_at(*x, *y)[3] > 0)
                .map(|(x, y)| {
                    ((x as f64 - centre.0).powi(2) + (y as f64 - centre.1).powi(2)).sqrt()
                })
                .fold(0.0f64, f64::max)
        };
        assert!(radius(&saturated) > radius(&washed) * 2.0, "saturation is distance");
    }

    #[test]
    fn a_histogram_counts_every_pixel_once_per_channel() {
        let size = Size::new(32, 16);
        let half =
            sample(size, |x, _| if x < 16 { [0, 0, 0, 255] } else { [255, 255, 255, 255] });
        let histogram = Histogram::of(&half);
        let pixels = size.width * size.height;
        assert_eq!(histogram.luma[0], pixels / 2);
        assert_eq!(histogram.luma[255], pixels / 2);
        assert_eq!(histogram.red.iter().sum::<u32>(), pixels);
        assert_eq!(histogram.peak(), pixels / 2);
    }

    #[test]
    fn one_scale_covers_every_channel_so_they_can_be_compared() {
        // A channel that is barely present must plot short. Scaling each to its
        // own peak would draw it as tall as one filling the frame.
        let mostly_red = sample(Size::new(16, 16), |x, _| {
            if x == 0 {
                [255, 255, 255, 255]
            } else {
                [255, 0, 0, 255]
            }
        });
        let histogram = Histogram::of(&mostly_red);
        assert_eq!(histogram.peak(), histogram.red[255], "red fills the frame");
        assert!(histogram.green[255] < histogram.peak() / 8, "green barely appears");
    }

    #[test]
    fn coverage_is_divided_out_rather_than_plotted() {
        // A half-covered white pixel is white. A scope that plotted it at half
        // would report a grade nobody made.
        let opaque = solid(Size::new(16, 16), [255, 255, 255, 255]);
        let half = solid(Size::new(16, 16), [128, 128, 128, 128]);
        assert_eq!(
            waveform(&opaque, WaveformMode::Luma, PLOT)
                .pixels
                .iter()
                .filter(|b| **b > 0)
                .count(),
            waveform(&half, WaveformMode::Luma, PLOT).pixels.iter().filter(|b| **b > 0).count(),
        );
        assert_eq!(Histogram::of(&half).luma[255], 256, "still white");
    }

    #[test]
    fn a_pixel_with_no_coverage_is_left_out_rather_than_counted_as_black() {
        // Otherwise a title over nothing would bury its own waveform under a
        // floor of zeros that is not in the picture.
        let mostly_empty =
            sample(
                Size::new(16, 16),
                |x, _| if x == 0 { [255, 255, 255, 255] } else { [0; 4] },
            );
        let histogram = Histogram::of(&mostly_empty);
        assert_eq!(histogram.luma[0], 0, "nothing is plotted at black");
        assert_eq!(histogram.luma[255], 16);
        assert_eq!(histogram.red.iter().sum::<u32>(), 16, "only the covered pixels");
    }

    #[test]
    fn an_empty_sample_produces_an_empty_plot() {
        let nothing = ScopeSample::default();
        assert!(Histogram::of(&nothing).is_empty());
        assert!(waveform(&nothing, WaveformMode::Luma, PLOT).pixels.iter().all(|b| *b == 0));
        assert!(vectorscope(&nothing, PLOT).pixels.iter().all(|b| *b == 0));
    }

    #[test]
    fn the_sample_is_the_shape_of_the_picture() {
        // A waveform's columns line up with the frame's only if the sample has
        // the frame's aspect ratio.
        assert_eq!(ScopeSampler::sample_size(Size::new(1920, 1080)), Size::new(256, 144));
        assert_eq!(ScopeSampler::sample_size(Size::new(1080, 1920)), Size::new(256, 455));
        // And a degenerate size produces something a plot can still be made of.
        assert_eq!(ScopeSampler::sample_size(Size::new(0, 0)), Size::new(256, 1));
    }
}
