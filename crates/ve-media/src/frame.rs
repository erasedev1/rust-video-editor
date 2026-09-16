use std::sync::Arc;

use ve_core::Size;
use ve_time::{SampleRate, Ticks};

/// Pixel layout of a decoded frame.
///
/// Only packed RGBA is produced today: the decoder converts through swscale on
/// the way out, which is correct for every input format and costs a single pass
/// over the frame. The enum exists because the next optimisation is uploading
/// planar YUV straight to the GPU and doing the colour conversion in the
/// shader, and that wants a frame type that can already describe it rather than
/// a new one.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum PixelFormat {
    /// 8 bits per channel, red first, alpha last.
    Rgba8,
}

impl PixelFormat {
    pub fn bytes_per_pixel(self) -> u32 {
        match self {
            PixelFormat::Rgba8 => 4,
        }
    }
}

/// A decoded video frame.
///
/// The pixel data sits behind an [`Arc`], so handing a frame to the cache, the
/// GPU uploader and the UI at once costs three pointer copies rather than three
/// buffer copies. Frames are immutable once decoded, which is what makes that
/// sharing safe without a lock.
#[derive(Clone)]
pub struct VideoFrame {
    data: Arc<[u8]>,
    size: Size,
    /// Bytes per row, which may exceed `width * bytes_per_pixel` because
    /// swscale aligns rows for SIMD.
    stride: u32,
    format: PixelFormat,
    /// Presentation time in *source* media time, not timeline time.
    pts: Ticks,
}

impl VideoFrame {
    pub fn new(
        data: Arc<[u8]>,
        size: Size,
        stride: u32,
        format: PixelFormat,
        pts: Ticks,
    ) -> Self {
        debug_assert!(
            stride >= size.width * format.bytes_per_pixel(),
            "stride {stride} is too small for {}px of {format:?}",
            size.width
        );
        debug_assert!(data.len() as u64 >= stride as u64 * size.height as u64);
        VideoFrame { data, size, stride, format, pts }
    }

    #[inline]
    pub fn data(&self) -> &[u8] {
        &self.data
    }

    /// The shared buffer handle, for passing on without copying.
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
    #[inline]
    pub fn stride(&self) -> u32 {
        self.stride
    }
    #[inline]
    pub fn format(&self) -> PixelFormat {
        self.format
    }
    #[inline]
    pub fn pts(&self) -> Ticks {
        self.pts
    }

    /// Bytes this frame occupies, for the cache's memory budget.
    pub fn byte_size(&self) -> usize {
        self.data.len()
    }

    /// One row of pixels.
    pub fn row(&self, y: u32) -> &[u8] {
        let start = (y as usize) * (self.stride as usize);
        let end = start + (self.size.width * self.format.bytes_per_pixel()) as usize;
        &self.data[start..end]
    }

    /// Whether rows are packed with no padding, in which case the whole buffer
    /// can be handed to the GPU in one go.
    pub fn is_tightly_packed(&self) -> bool {
        self.stride == self.size.width * self.format.bytes_per_pixel()
    }
}

impl std::fmt::Debug for VideoFrame {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("VideoFrame")
            .field("size", &self.size)
            .field("stride", &self.stride)
            .field("format", &self.format)
            .field("pts", &self.pts)
            .field("bytes", &self.data.len())
            .finish()
    }
}

/// A block of decoded audio, interleaved 32-bit float.
///
/// Float because every mixing, gain and fade operation downstream works in
/// float, so converting once at the decoder is cheaper than converting at each
/// stage, and it removes clipping from intermediate sums.
#[derive(Clone)]
pub struct AudioBuffer {
    samples: Arc<[f32]>,
    sample_rate: SampleRate,
    channels: u16,
    pts: Ticks,
}

impl AudioBuffer {
    pub fn new(
        samples: Arc<[f32]>,
        sample_rate: SampleRate,
        channels: u16,
        pts: Ticks,
    ) -> Self {
        AudioBuffer { samples, sample_rate, channels, pts }
    }

    pub fn silence(frames: usize, sample_rate: SampleRate, channels: u16, pts: Ticks) -> Self {
        AudioBuffer {
            samples: vec![0.0; frames * channels as usize].into(),
            sample_rate,
            channels,
            pts,
        }
    }

    #[inline]
    pub fn samples(&self) -> &[f32] {
        &self.samples
    }
    #[inline]
    pub fn channels(&self) -> u16 {
        self.channels
    }
    #[inline]
    pub fn sample_rate(&self) -> SampleRate {
        self.sample_rate
    }
    #[inline]
    pub fn pts(&self) -> Ticks {
        self.pts
    }

    /// Sample frames (one per channel-group), not individual samples.
    pub fn frame_count(&self) -> usize {
        if self.channels == 0 {
            0
        } else {
            self.samples.len() / self.channels as usize
        }
    }

    pub fn duration(&self) -> Ticks {
        self.sample_rate.sample_to_ticks(self.frame_count() as i64)
    }

    /// Exclusive end of this buffer in source time.
    pub fn end_pts(&self) -> Ticks {
        self.pts + self.duration()
    }

    pub fn byte_size(&self) -> usize {
        self.samples.len() * std::mem::size_of::<f32>()
    }

    /// Peak absolute amplitude, for meters and waveform overviews.
    pub fn peak(&self) -> f32 {
        self.samples.iter().fold(0.0f32, |acc, s| acc.max(s.abs()))
    }
}

impl std::fmt::Debug for AudioBuffer {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AudioBuffer")
            .field("frames", &self.frame_count())
            .field("channels", &self.channels)
            .field("sample_rate", &self.sample_rate)
            .field("pts", &self.pts)
            .finish()
    }
}
