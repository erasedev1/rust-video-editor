//! Stateful, single-threaded decoders for one open media file each.
//!
//! These types own a seek position and are deliberately *not* `Sync`: an
//! FFmpeg decode context cannot be driven from two threads at once. Concurrency
//! is provided a level up by [`crate::DecodeService`], which gives each open
//! file its own thread and its own decoder.

use std::path::{Path, PathBuf};

use ffmpeg_next as ffmpeg;
use ffmpeg_next::software::scaling;
use ve_core::Size;
use ve_metrics::{spans, Metrics};
use ve_time::{Rate, SampleRate, Ticks};

use crate::colour;
use crate::frame::{AudioBuffer, PixelFormat, VideoFrame};
use crate::probe::{ticks_to_ts, ts_to_ticks};
use crate::{ffmpeg_init, MediaError};

/// How far ahead of the current position a request can be before seeking beats
/// decoding forward.
///
/// Seeking lands on a keyframe and then has to decode forward to the target
/// anyway, so for small jumps — the common case during playback and fine
/// scrubbing — decoding straight through is both faster and avoids the
/// frame-accuracy pitfalls of seeking. Two seconds is comfortably longer than
/// any sane GOP.
const FORWARD_DECODE_LIMIT: Ticks = Ticks::new(ve_time::TICKS_PER_SECOND * 2);

/// An swscale context that may be moved between threads.
///
/// FFmpeg does not mark `SwsContext` as `Send` because it is not safe to *use*
/// one from two threads at once. Transferring ownership is a different matter:
/// the context is reached only through this field, [`VideoDecoder`] owns it
/// exclusively, and every method that touches it takes `&mut self`, so at most
/// one thread can be inside `sws_scale` at a time. `DecodeService` relies on
/// this to hand a freshly opened decoder to its worker thread.
struct SendScaler(scaling::Context);

// SAFETY: see the type's documentation. The wrapper is deliberately private and
// holds no `Sync` promise — only the ownership transfer is claimed sound.
unsafe impl Send for SendScaler {}

impl std::ops::Deref for SendScaler {
    type Target = scaling::Context;
    fn deref(&self) -> &scaling::Context {
        &self.0
    }
}

impl std::ops::DerefMut for SendScaler {
    fn deref_mut(&mut self) -> &mut scaling::Context {
        &mut self.0
    }
}

/// A resampling context that may be moved between threads.
///
/// Same reasoning as [`SendScaler`]: `SwrContext` is not safe to *use* from two
/// threads at once, but it is reached only through this privately owned field
/// and every method touching it takes `&mut self`.
struct SendResampler(ffmpeg::software::resampling::Context);

// SAFETY: see the type's documentation.
unsafe impl Send for SendResampler {}

impl std::ops::Deref for SendResampler {
    type Target = ffmpeg::software::resampling::Context;
    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

impl std::ops::DerefMut for SendResampler {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.0
    }
}

/// Decodes video frames from one file.
pub struct VideoDecoder {
    path: PathBuf,
    input: ffmpeg::format::context::Input,
    decoder: ffmpeg::decoder::Video,
    scaler: SendScaler,
    stream_index: usize,
    time_base: ffmpeg::Rational,
    /// Size frames are delivered at, which may be smaller than the source for
    /// proxy or thumbnail decoding.
    output_size: Size,
    source_size: Size,
    rate: Rate,
    /// Presentation time of the most recently delivered frame, used to decide
    /// between seeking and decoding forward.
    position: Option<Ticks>,
    /// The frame the decoder is currently "showing": the last one delivered.
    ///
    /// Kept so that a request landing inside the frame already on screen is
    /// answered without decoding anything, and so that the lookahead below has
    /// something to fall back to.
    current: Option<VideoFrame>,
    /// A frame decoded but not yet wanted.
    ///
    /// Deciding that a frame is the last one at or before `t` requires seeing
    /// the *next* frame, so overshooting by one is unavoidable. Holding that
    /// overshoot here means the following request — which during playback and
    /// single-frame stepping is very often for exactly this frame — does not
    /// have to decode it again, and cannot skip past it.
    pending: Option<VideoFrame>,
    /// Set once the demuxer has run out and the decoder has been flushed.
    drained: bool,
    metrics: Option<Metrics>,
}

impl VideoDecoder {
    /// Opens a file for video decoding, delivering frames at their native size.
    pub fn open(path: impl AsRef<Path>) -> Result<Self, MediaError> {
        Self::open_scaled(path, None)
    }

    /// Opens a file, optionally scaling every frame on the way out.
    ///
    /// Scaling in the decoder rather than on the GPU is the right trade for
    /// thumbnails and proxies: a 1/8-size frame costs a fraction of the upload
    /// bandwidth and the cache memory of a full one.
    pub fn open_scaled(
        path: impl AsRef<Path>,
        output_size: Option<Size>,
    ) -> Result<Self, MediaError> {
        let path = path.as_ref().to_path_buf();
        ffmpeg_init()?;

        let input = ffmpeg::format::input(&path).map_err(|e| MediaError::open(&path, e))?;
        let stream = input
            .streams()
            .best(ffmpeg::media::Type::Video)
            .ok_or(MediaError::NoVideoStream)?;

        let stream_index = stream.index();
        let time_base = stream.time_base();
        let rate = {
            let r = stream.avg_frame_rate();
            if r.numerator() > 0 && r.denominator() > 0 {
                Rate::new(r.numerator() as u32, r.denominator() as u32).unwrap_or(Rate::FPS_30)
            } else {
                Rate::FPS_30
            }
        };

        let ctx = ffmpeg::codec::context::Context::from_parameters(stream.parameters())?;
        let decoder = ctx.decoder().video()?;

        let source_size = Size::new(decoder.width(), decoder.height());
        let output_size = output_size.unwrap_or(source_size);

        // Bilinear is the right default: bicubic costs noticeably more for a
        // difference invisible at preview sizes, and export can ask for better.
        let mut scaler = scaling::Context::get(
            decoder.format(),
            source_size.width,
            source_size.height,
            ffmpeg::format::Pixel::RGBA,
            output_size.width,
            output_size.height,
            scaling::Flags::BILINEAR,
        )?;

        // swscale would otherwise convert every file with the BT.601 matrix,
        // which is wrong for anything shot in HD and visibly shifts saturated
        // colour. The stream's own tag decides, falling back on its size the
        // way a player does; the same rule writes the tag on the way out, so
        // importing Verge's own export gets back what it put in.
        let matrix = colour::Matrix::from_space(decoder.color_space(), source_size.height);
        let range = match decoder.color_range() {
            ffmpeg::color::Range::JPEG => colour::Range::Full,
            _ => colour::Range::Limited,
        };
        if !colour::set_matrix(&mut scaler, matrix, range, colour::Range::Full) {
            log::debug!("swscale kept its default matrix for {}", path.display());
        }

        Ok(VideoDecoder {
            path,
            input,
            decoder,
            scaler: SendScaler(scaler),
            stream_index,
            time_base,
            output_size,
            source_size,
            rate,
            position: None,
            current: None,
            pending: None,
            drained: false,
            metrics: None,
        })
    }

    pub fn with_metrics(mut self, metrics: Metrics) -> Self {
        self.metrics = Some(metrics);
        self
    }

    pub fn path(&self) -> &Path {
        &self.path
    }
    pub fn output_size(&self) -> Size {
        self.output_size
    }
    pub fn source_size(&self) -> Size {
        self.source_size
    }
    pub fn rate(&self) -> Rate {
        self.rate
    }
    /// Source time of the last frame delivered.
    pub fn position(&self) -> Option<Ticks> {
        self.position
    }

    /// Decodes and returns the next frame in presentation order.
    ///
    /// Returns `Ok(None)` at end of stream.
    pub fn next_frame(&mut self) -> Result<Option<VideoFrame>, MediaError> {
        // A frame held back by an earlier lookahead is the next frame.
        let frame = match self.pending.take() {
            Some(frame) => Some(frame),
            None => self.decode_one()?,
        };
        if let Some(ref frame) = frame {
            self.position = Some(frame.pts());
            self.current = Some(frame.clone());
        }
        Ok(frame)
    }

    /// Pulls one frame straight from the decoder, bypassing the lookahead.
    fn decode_one(&mut self) -> Result<Option<VideoFrame>, MediaError> {
        let _span = self.metrics.as_ref().map(|m| m.span(spans::DECODE));

        let mut decoded = ffmpeg::frame::Video::empty();
        loop {
            // Drain whatever the decoder already holds before feeding it more:
            // one packet can yield several frames.
            match self.decoder.receive_frame(&mut decoded) {
                Ok(()) => return Ok(Some(self.convert(&decoded)?)),
                Err(ffmpeg::Error::Other { errno }) if errno == ffmpeg::util::error::EAGAIN => {
                }
                Err(ffmpeg::Error::Eof) => return Ok(None),
                Err(e) => return Err(MediaError::Ffmpeg(e)),
            }

            if self.drained {
                return Ok(None);
            }

            match self.read_packet() {
                Some(packet) => self.decoder.send_packet(&packet)?,
                None => {
                    // No more packets: flush the decoder so any frames it is
                    // still holding come out before we report end of stream.
                    self.decoder.send_eof()?;
                    self.drained = true;
                }
            }
        }
    }

    /// Returns the frame that covers source time `t`.
    ///
    /// "Covers" means the last frame whose presentation time is at or before
    /// `t`, which is what a playhead at `t` should be showing. Chooses between
    /// seeking and decoding forward based on how far away `t` is.
    pub fn frame_at(&mut self, t: Ticks) -> Result<Option<VideoFrame>, MediaError> {
        let t = t.clamp_non_negative();

        let must_seek = match self.position {
            // Never decoded anything yet.
            None => true,
            // The target is behind us; the decoder cannot run backwards.
            Some(pos) if t < pos => true,
            // Too far ahead to be worth decoding through.
            Some(pos) => t - pos > FORWARD_DECODE_LIMIT,
        };

        if must_seek {
            self.seek(t)?;
        }

        self.decode_until_covering(t)
    }

    /// Seeks to at or before `t`.
    ///
    /// FFmpeg's backward seek lands on the keyframe at or before the target, so
    /// the subsequent forward decode is guaranteed to be able to reconstruct
    /// the requested frame exactly.
    pub fn seek(&mut self, t: Ticks) -> Result<(), MediaError> {
        let _span = self.metrics.as_ref().map(|m| m.span(spans::SEEK));
        if let Some(m) = &self.metrics {
            m.incr(ve_metrics::counters::SEEKS, 1);
        }

        let target = ticks_to_ts(t.clamp_non_negative(), self.time_base);
        self.input.seek(target, ..target).map_err(MediaError::Ffmpeg)?;
        // The decoder holds frames from before the seek; they are now wrong.
        self.decoder.flush();
        self.pending = None;
        self.current = None;
        self.position = None;
        self.drained = false;
        Ok(())
    }

    /// Decodes forward, returning the last frame at or before `t`.
    ///
    /// Works from the frame already held plus a one-frame lookahead, so a
    /// request that lands inside the current frame decodes nothing at all, and
    /// a request for the very next frame decodes nothing either.
    fn decode_until_covering(&mut self, t: Ticks) -> Result<Option<VideoFrame>, MediaError> {
        loop {
            // Copied out so the lookahead below can borrow `self` mutably.
            let current_pts = self.current.as_ref().map(|f| f.pts());

            match current_pts {
                // The frame on hand starts at or before `t`. It is the answer
                // unless the frame after it also starts at or before `t`.
                Some(pts) if pts <= t => {
                    if self.pending.is_none() {
                        self.pending = self.decode_one()?;
                    }
                    let next_also_covers =
                        matches!(&self.pending, Some(next) if next.pts() <= t);
                    if !next_also_covers {
                        self.position = Some(pts);
                        return Ok(self.current.clone());
                    }
                }
                // The frame on hand starts *after* `t`. Only reachable right
                // after a seek or at the very start of the file, because any
                // request behind the current position seeks first. Either way
                // it is the earliest frame available, so it is the best answer.
                Some(_) => return Ok(self.current.clone()),
                // Nothing decoded yet.
                None => {}
            }

            if self.next_frame()?.is_none() {
                // End of stream: whatever we last held covers `t`.
                return Ok(self.current.clone());
            }
        }
    }

    /// Reads the next packet belonging to our stream, skipping the others.
    fn read_packet(&mut self) -> Option<ffmpeg::codec::packet::Packet> {
        let wanted = self.stream_index;
        for (stream, packet) in self.input.packets() {
            if stream.index() == wanted {
                return Some(packet);
            }
        }
        None
    }

    /// Converts a decoded frame to packed RGBA and stamps it with source time.
    fn convert(&mut self, decoded: &ffmpeg::frame::Video) -> Result<VideoFrame, MediaError> {
        let _span = self.metrics.as_ref().map(|m| m.span(spans::SCALE));

        let mut rgba = ffmpeg::frame::Video::empty();
        self.scaler.run(decoded, &mut rgba)?;

        // `pts` is preferred; some codecs only set the older `timestamp`.
        let raw_pts = decoded.pts().or_else(|| decoded.timestamp()).unwrap_or(0);
        let pts = ts_to_ticks(raw_pts, self.time_base).clamp_non_negative();

        let stride = rgba.stride(0) as u32;
        let height = self.output_size.height;
        let needed = stride as usize * height as usize;
        let plane = rgba.data(0);
        // swscale allocates whole aligned planes, which can exceed the rows we
        // actually need; trim rather than carry the slack into the cache.
        let data: std::sync::Arc<[u8]> = plane[..needed.min(plane.len())].into();

        Ok(VideoFrame::new(data, self.output_size, stride, PixelFormat::Rgba8, pts))
    }
}

/// Decodes audio from one file, resampled to a fixed output format.
///
/// The output rate and channel count are fixed at open time so that the mixer
/// downstream never has to deal with heterogeneous buffers: every clip in a
/// sequence arrives in the sequence's own format.
pub struct AudioDecoder {
    path: PathBuf,
    input: ffmpeg::format::context::Input,
    decoder: ffmpeg::decoder::Audio,
    /// Built from the first decoded frame rather than from the decoder's
    /// declared format. For several containers — WAV among them — the decoder
    /// does not settle its sample format and channel layout until it has
    /// actually decoded something, and a resampler configured from the
    /// pre-decode values is rejected with "Input changed" on first use.
    resampler: Option<SendResampler>,
    stream_index: usize,
    time_base: ffmpeg::Rational,
    out_rate: SampleRate,
    out_channels: u16,
    position: Option<Ticks>,
    drained: bool,
    metrics: Option<Metrics>,
}

impl AudioDecoder {
    pub fn open(
        path: impl AsRef<Path>,
        out_rate: SampleRate,
        out_channels: u16,
    ) -> Result<Self, MediaError> {
        let path = path.as_ref().to_path_buf();
        ffmpeg_init()?;

        let input = ffmpeg::format::input(&path).map_err(|e| MediaError::open(&path, e))?;
        let stream = input
            .streams()
            .best(ffmpeg::media::Type::Audio)
            .ok_or(MediaError::NoAudioStream)?;
        let stream_index = stream.index();
        let time_base = stream.time_base();

        let ctx = ffmpeg::codec::context::Context::from_parameters(stream.parameters())?;
        let decoder = ctx.decoder().audio()?;

        Ok(AudioDecoder {
            path,
            input,
            decoder,
            resampler: None,
            stream_index,
            time_base,
            out_rate,
            out_channels,
            position: None,
            drained: false,
            metrics: None,
        })
    }

    pub fn with_metrics(mut self, metrics: Metrics) -> Self {
        self.metrics = Some(metrics);
        self
    }

    pub fn path(&self) -> &Path {
        &self.path
    }
    pub fn sample_rate(&self) -> SampleRate {
        self.out_rate
    }
    pub fn channels(&self) -> u16 {
        self.out_channels
    }
    pub fn position(&self) -> Option<Ticks> {
        self.position
    }

    /// Decodes the next block of audio, resampled to the output format.
    pub fn next_buffer(&mut self) -> Result<Option<AudioBuffer>, MediaError> {
        let _span = self.metrics.as_ref().map(|m| m.span(spans::DECODE));

        let mut decoded = ffmpeg::frame::Audio::empty();
        loop {
            match self.decoder.receive_frame(&mut decoded) {
                Ok(()) => {
                    // Several demuxers (WAV among them) hand back frames with
                    // no channel layout set at all, not even after decoding.
                    // swresample compares the incoming frame's layout against
                    // its configured input and rejects a mismatch with
                    // "Input changed", so the layout has to be filled in on the
                    // frame itself rather than on the decoder — setting it on
                    // the decoder does not propagate to the frames it emits.
                    if decoded.channel_layout().is_empty() {
                        let channels = decoded.channels() as i32;
                        decoded.set_channel_layout(ffmpeg::ChannelLayout::default(channels));
                    }
                    let buffer = self.resample(&decoded)?;
                    self.position = Some(buffer.end_pts());
                    return Ok(Some(buffer));
                }
                Err(ffmpeg::Error::Other { errno }) if errno == ffmpeg::util::error::EAGAIN => {
                }
                Err(ffmpeg::Error::Eof) => return Ok(None),
                Err(e) => return Err(MediaError::Ffmpeg(e)),
            }

            if self.drained {
                return Ok(None);
            }

            match self.read_packet() {
                Some(packet) => self.decoder.send_packet(&packet)?,
                None => {
                    self.decoder.send_eof()?;
                    self.drained = true;
                }
            }
        }
    }

    pub fn seek(&mut self, t: Ticks) -> Result<(), MediaError> {
        let _span = self.metrics.as_ref().map(|m| m.span(spans::SEEK));
        let target = ticks_to_ts(t.clamp_non_negative(), self.time_base);
        self.input.seek(target, ..target)?;
        self.decoder.flush();
        self.position = None;
        self.drained = false;
        Ok(())
    }

    /// Decodes at least `min_frames` sample frames starting at `t`.
    ///
    /// Audio decoders emit fixed-size blocks that rarely align with what the
    /// audio device asks for, so this accumulates whole blocks and lets the
    /// caller trim. Returns fewer frames only at end of stream.
    pub fn buffers_from(
        &mut self,
        t: Ticks,
        min_frames: usize,
    ) -> Result<Vec<AudioBuffer>, MediaError> {
        let needs_seek = match self.position {
            None => true,
            Some(pos) => t < pos || t - pos > FORWARD_DECODE_LIMIT,
        };
        if needs_seek {
            self.seek(t)?;
        }

        let mut out = Vec::new();
        let mut frames = 0usize;
        while frames < min_frames {
            match self.next_buffer()? {
                Some(buffer) => {
                    // Skip blocks that end before the requested start.
                    if buffer.end_pts() <= t {
                        continue;
                    }
                    frames += buffer.frame_count();
                    out.push(buffer);
                }
                None => break,
            }
        }
        Ok(out)
    }

    fn read_packet(&mut self) -> Option<ffmpeg::codec::packet::Packet> {
        let wanted = self.stream_index;
        for (stream, packet) in self.input.packets() {
            if stream.index() == wanted {
                return Some(packet);
            }
        }
        None
    }

    /// Builds the resampler from the format of an actually-decoded frame.
    fn ensure_resampler(&mut self, decoded: &ffmpeg::frame::Audio) -> Result<(), MediaError> {
        if self.resampler.is_some() {
            return Ok(());
        }
        let mut in_layout = decoded.channel_layout();
        if in_layout.is_empty() {
            in_layout = ffmpeg::ChannelLayout::default(decoded.channels() as i32);
        }
        let ctx = ffmpeg::software::resampling::Context::get(
            decoded.format(),
            in_layout,
            decoded.rate(),
            // Packed f32 because everything downstream mixes in interleaved
            // float, and converting once here avoids a second pass later.
            ffmpeg::format::Sample::F32(ffmpeg::format::sample::Type::Packed),
            ffmpeg::ChannelLayout::default(self.out_channels as i32),
            self.out_rate.hz(),
        )?;
        self.resampler = Some(SendResampler(ctx));
        Ok(())
    }

    fn resample(&mut self, decoded: &ffmpeg::frame::Audio) -> Result<AudioBuffer, MediaError> {
        self.ensure_resampler(decoded)?;
        let mut out = ffmpeg::frame::Audio::empty();
        self.resampler.as_mut().expect("just ensured").run(decoded, &mut out)?;

        let raw_pts = decoded.pts().or_else(|| decoded.timestamp()).unwrap_or(0);
        let pts = ts_to_ticks(raw_pts, self.time_base).clamp_non_negative();

        // Packed f32: one plane holding every channel interleaved.
        let channels = self.out_channels as usize;
        let sample_count = out.samples() * channels;
        let bytes = out.data(0);
        let usable = sample_count.min(bytes.len() / std::mem::size_of::<f32>());

        let mut samples = Vec::with_capacity(usable);
        for chunk in bytes.as_chunks::<4>().0.iter().take(usable) {
            samples.push(f32::from_ne_bytes([chunk[0], chunk[1], chunk[2], chunk[3]]));
        }

        Ok(AudioBuffer::new(samples.into(), self.out_rate, self.out_channels, pts))
    }
}
