//! Encoding composited pictures and mixed sound into a file.
//!
//! The one part of the export path that talks to FFmpeg. Everything above it
//! hands over finished RGBA frames and interleaved `f32` sample blocks and
//! knows nothing about codecs, packets or time bases.
//!
//! # Time is stated, never accumulated
//!
//! A frame's presentation time is its **index** in the encoder's own time base
//! — `1/rate` — and a block of sound is stamped with the running sample count.
//! Neither is a sum of durations, for the same reason the timeline is not: at
//! 29.97 the frame interval is not representable, so adding it up drifts, and a
//! long export would end with the sound visibly late against the picture.
//!
//! # What the muxer is told
//!
//! Packets are rescaled from the encoder's time base into the stream's and
//! written interleaved, which is what lets the muxer order audio and video by
//! time rather than by the order they happened to be encoded in.

use ffmpeg_next as ffmpeg;
use ffmpeg_next::software::scaling;
use ve_core::Size;
use ve_media::colour;

use crate::settings::{
    AudioCodec, AudioSettings, Container, ExportSettings, Quality, VideoCodec,
};
use crate::ExportError;

/// Writes one output file: a video stream, optionally an audio stream, and the
/// container around them.
pub struct MediaWriter {
    output: ffmpeg::format::context::Output,
    video: VideoStream,
    audio: Option<AudioStream>,
}

impl MediaWriter {
    /// Creates the file and opens its encoders.
    ///
    /// `source` is the size pictures will arrive at — the canvas the editor
    /// composited — which may differ from the size being written.
    pub fn create(settings: &ExportSettings, source: Size) -> Result<Self, ExportError> {
        let container = settings.validate()?;
        ve_media::init().map_err(|e| ExportError::Encoder(e.to_string()))?;

        if let Some(parent) = settings.path.parent() {
            if !parent.as_os_str().is_empty() && !parent.exists() {
                std::fs::create_dir_all(parent)
                    .map_err(|e| ExportError::Io { path: parent.into(), source: e })?;
            }
        }

        let mut output = ffmpeg::format::output(&settings.path)
            .map_err(|e| ExportError::Open { path: settings.path.clone(), source: e })?;

        // Some containers keep the codec's extradata in the file header rather
        // than in the first packet, and an encoder that was not told so writes
        // a file no demuxer can start.
        let global_header =
            output.format().flags().contains(ffmpeg::format::Flags::GLOBAL_HEADER);

        let video = VideoStream::add(&mut output, settings, source, global_header)?;
        let audio = match &settings.audio {
            Some(audio) => {
                Some(AudioStream::add(&mut output, audio, container, global_header)?)
            }
            None => None,
        };

        output
            .write_header()
            .map_err(|e| ExportError::Encoder(format!("writing the file header: {e}")))?;

        // The muxer is allowed to change a stream's time base while writing the
        // header, so what packets are rescaled into has to be read back after
        // it rather than assumed from what was asked for.
        let mut writer = MediaWriter { output, video, audio };
        writer.video.stream_time_base =
            writer.output.stream(writer.video.index).expect("video stream").time_base();
        if let Some(audio) = &mut writer.audio {
            audio.stream_time_base =
                writer.output.stream(audio.index).expect("audio stream").time_base();
        }
        Ok(writer)
    }

    /// Encodes one composited picture, tightly packed RGBA at the source size.
    ///
    /// The picture is stamped with the next frame index, which is what an
    /// export wants: it renders every frame of a range in order, so the index
    /// *is* the time.
    pub fn write_frame(&mut self, rgba: &[u8]) -> Result<(), ExportError> {
        self.video.send(rgba, None)?;
        self.video.drain(&mut self.output, false)
    }

    /// Encodes one picture at a stated time, in frames of the output rate.
    ///
    /// For copying a file rather than rendering one: a transcode has real
    /// source timestamps to preserve, and a variable-frame-rate source has
    /// frames its own average rate would not put where they actually are.
    /// Stamping by index would silently re-time such a file, and a proxy that
    /// is re-timed shows a different frame from the original at the same
    /// instant — which is the one thing a proxy may never do.
    ///
    /// Presentation times must increase. One that does not — two source frames
    /// landing in the same slot of the average rate — is nudged to the slot
    /// after the last, because a muxer will not accept the alternative and a
    /// proxy cannot hold more frames than its rate has room for.
    pub fn write_frame_at(&mut self, rgba: &[u8], pts: i64) -> Result<(), ExportError> {
        self.video.send(rgba, Some(pts))?;
        self.video.drain(&mut self.output, false)
    }

    /// Encodes a block of interleaved samples at the export's audio format.
    ///
    /// Blocks need not line up with what the encoder wants: whatever does not
    /// fill a codec frame is held over and joined to the next block, which is
    /// what lets the caller mix one video frame's worth of sound at a time.
    pub fn write_audio(&mut self, interleaved: &[f32]) -> Result<(), ExportError> {
        let Some(audio) = self.audio.as_mut() else { return Ok(()) };
        audio.push(interleaved);
        while audio.take_full_frame()? {
            audio.drain(&mut self.output, false)?;
        }
        Ok(())
    }

    /// Flushes both encoders, writes the trailer and closes the file.
    ///
    /// Consuming, because anything written after this would not be in the file:
    /// the container's index has already been written. A writer dropped without
    /// this — a cancelled export — leaves a container with no index, which is
    /// why the job removes the file rather than leaving it.
    pub fn finish(mut self) -> Result<(), ExportError> {
        self.video.send_eof()?;
        self.video.drain(&mut self.output, true)?;

        if let Some(audio) = self.audio.as_mut() {
            // Whatever is left over is a legal final frame even though it is
            // shorter than the rest; dropping it would clip the last few
            // milliseconds off every export.
            audio.flush_remainder()?;
            audio.drain(&mut self.output, false)?;
            audio.send_eof()?;
            audio.drain(&mut self.output, true)?;
        }

        self.output
            .write_trailer()
            .map_err(|e| ExportError::Encoder(format!("writing the file trailer: {e}")))?;
        Ok(())
    }

    /// The encoder actually in use, for the report and for the log.
    pub fn video_encoder_name(&self) -> &str {
        &self.video.encoder_name
    }

    pub fn audio_encoder_name(&self) -> Option<&str> {
        self.audio.as_ref().map(|a| a.encoder_name.as_str())
    }

    /// How many sample frames the audio encoder takes at a time.
    pub fn audio_frame_size(&self) -> Option<usize> {
        self.audio.as_ref().map(|a| a.frame_size)
    }
}

/// The picture half: one encoder, one scaler, one reusable pair of frames.
struct VideoStream {
    encoder: ffmpeg::encoder::video::Encoder,
    encoder_name: String,
    index: usize,
    time_base: ffmpeg::Rational,
    stream_time_base: ffmpeg::Rational,
    scaler: scaling::Context,
    /// The incoming picture, wrapped rather than copied where possible.
    source: ffmpeg::frame::Video,
    /// What the encoder is handed, in its own pixel format and size.
    target: ffmpeg::frame::Video,
    next_pts: i64,
}

impl VideoStream {
    fn add(
        output: &mut ffmpeg::format::context::Output,
        settings: &ExportSettings,
        source_size: Size,
        global_header: bool,
    ) -> Result<Self, ExportError> {
        let (codec, encoder_name) =
            find_encoder(settings.video.encoder_names()).ok_or_else(|| {
                ExportError::NoEncoder(format!(
                    "{} — this build of FFmpeg has no encoder for it",
                    settings.video.label()
                ))
            })?;

        let index = output.add_stream(codec)?.index();
        let mut encoder = context_for(codec)?
            .encoder()
            .video()
            .map_err(|e| ExportError::Encoder(format!("preparing the video encoder: {e}")))?;

        let size = settings.resolution;
        let rate = settings.rate;
        // The encoder counts in frames: its time base is one frame, so a
        // presentation time is a frame index and nothing has to be rounded.
        let time_base =
            ffmpeg::Rational::new(rate.denominator() as i32, rate.numerator() as i32);

        encoder.set_width(size.width);
        encoder.set_height(size.height);
        encoder.set_format(pixel_format(settings.video));
        encoder.set_time_base(time_base);
        encoder.set_frame_rate(Some(ffmpeg::Rational::new(
            rate.numerator() as i32,
            rate.denominator() as i32,
        )));
        if settings.video.uses_bitrate() {
            encoder.set_bit_rate(settings.bitrate() as usize);
        }
        // A delivery gets a keyframe a second — long enough not to cost much,
        // short enough that scrubbing the result in any player lands quickly.
        // A file meant to be scrubbed *by this editor* asks for every frame.
        encoder.set_gop(settings.keyframes.gop(rate));

        // Say which matrix the picture was converted with, because the file is
        // unreadable without it: a player guessing 709 for a 601 conversion
        // shifts every saturated colour.
        let matrix = colour::Matrix::for_height(size.height);
        encoder.set_colorspace(matrix.space());
        set_colour_description(&mut encoder, matrix);
        // Limited range, which is what every delivery codec means by video
        // levels and what players assume when the tag is missing.
        encoder.set_color_range(ffmpeg::color::Range::MPEG);

        if global_header {
            encoder.set_flags(ffmpeg::codec::Flags::GLOBAL_HEADER);
        }

        let options = encoder_options(settings, &encoder_name);
        let encoder = encoder.open_as_with(codec, options).map_err(|e| {
            ExportError::Encoder(format!("opening the {encoder_name} encoder: {e}"))
        })?;

        {
            let mut stream = output.stream_mut(index).expect("the stream just added");
            stream.set_parameters(&encoder);
            stream.set_time_base(time_base);
        }

        // Bicubic rather than the decoder's bilinear: an export is written once
        // and looked at many times, so the difference is worth a few
        // milliseconds a frame. It costs nothing at all when the export is the
        // canvas's own size, which is the common case — swscale takes the
        // straight conversion path when no scaling is asked for.
        let mut scaler = scaling::Context::get(
            ffmpeg::format::Pixel::RGBA,
            source_size.width,
            source_size.height,
            pixel_format(settings.video),
            size.width,
            size.height,
            scaling::Flags::BICUBIC,
        )?;
        // Compositing happens in full-range RGB; the file is limited-range YUV.
        colour::set_matrix(&mut scaler, matrix, colour::Range::Full, colour::Range::Limited);

        let mut target =
            ffmpeg::frame::Video::new(pixel_format(settings.video), size.width, size.height);
        target.set_color_space(matrix.space());
        target.set_color_range(ffmpeg::color::Range::MPEG);

        Ok(VideoStream {
            encoder,
            encoder_name,
            index,
            time_base,
            stream_time_base: time_base,
            scaler,
            source: ffmpeg::frame::Video::new(
                ffmpeg::format::Pixel::RGBA,
                source_size.width,
                source_size.height,
            ),
            target,
            next_pts: 0,
        })
    }

    /// Converts one RGBA picture and hands it to the encoder.
    ///
    /// `pts` states the time in frames; `None` takes the next index.
    fn send(&mut self, rgba: &[u8], pts: Option<i64>) -> Result<(), ExportError> {
        let width = self.source.width() as usize;
        let height = self.source.height() as usize;
        let expected = width * height * 4;
        if rgba.len() < expected {
            return Err(ExportError::Encoder(format!(
                "a composited frame was {} bytes, not the {expected} a {width}×{height} \
                 picture takes",
                rgba.len()
            )));
        }

        // FFmpeg aligns its rows for SIMD, so the readback's tight rows are
        // copied one at a time rather than in one block.
        let stride = self.source.stride(0);
        let tight = width * 4;
        let plane = self.source.data_mut(0);
        for y in 0..height {
            let from = y * tight;
            let to = y * stride;
            plane[to..to + tight].copy_from_slice(&rgba[from..from + tight]);
        }

        self.scaler.run(&self.source, &mut self.target)?;
        // A stated time that has not moved on is pulled up to the next free
        // slot rather than repeated: the muxer rejects a pts that does not
        // increase, and dropping the frame instead would shorten the file.
        let pts = pts.map(|p| p.max(self.next_pts)).unwrap_or(self.next_pts);
        self.target.set_pts(Some(pts));
        self.next_pts = pts + 1;
        self.encoder
            .send_frame(&self.target)
            .map_err(|e| ExportError::Encoder(format!("encoding a frame: {e}")))
    }

    fn send_eof(&mut self) -> Result<(), ExportError> {
        self.encoder
            .send_eof()
            .map_err(|e| ExportError::Encoder(format!("flushing the video encoder: {e}")))
    }

    fn drain(
        &mut self,
        output: &mut ffmpeg::format::context::Output,
        draining: bool,
    ) -> Result<(), ExportError> {
        drain_packets(
            &mut self.encoder,
            output,
            self.index,
            self.time_base,
            self.stream_time_base,
            // The encoder's time base is one frame, so that is the duration.
            1,
            draining,
        )
    }
}

/// The sound half: one encoder and the samples not yet made into a full frame.
struct AudioStream {
    encoder: ffmpeg::encoder::audio::Encoder,
    encoder_name: String,
    index: usize,
    time_base: ffmpeg::Rational,
    stream_time_base: ffmpeg::Rational,
    format: ffmpeg::format::Sample,
    layout: ffmpeg::ChannelLayout,
    channels: usize,
    /// Sample frames the encoder takes at a time. Codecs that accept any
    /// length report zero, and are given a block of their own choosing.
    frame_size: usize,
    /// Interleaved samples waiting for the rest of a codec frame.
    pending: Vec<f32>,
    next_pts: i64,
}

/// What a codec that accepts any block length is given.
///
/// Only PCM is in that position today. A block is otherwise arbitrary, and
/// 1024 matches what the compressed codecs ask for, so the muxer sees packets
/// of a similar size whichever codec is in use.
const DEFAULT_AUDIO_FRAME: usize = 1024;

impl AudioStream {
    fn add(
        output: &mut ffmpeg::format::context::Output,
        settings: &AudioSettings,
        container: Container,
        global_header: bool,
    ) -> Result<Self, ExportError> {
        if container == Container::Mp4 && settings.codec == AudioCodec::Pcm16 {
            return Err(ExportError::Settings(
                "MP4 does not carry uncompressed audio — use .mov, .mkv, or AAC".into(),
            ));
        }

        let (codec, encoder_name) =
            find_encoder(settings.codec.encoder_names()).ok_or_else(|| {
                ExportError::NoEncoder(format!(
                    "{} — this build of FFmpeg has no encoder for it",
                    settings.codec.label()
                ))
            })?;

        let index = output.add_stream(codec)?.index();
        let mut encoder = context_for(codec)?
            .encoder()
            .audio()
            .map_err(|e| ExportError::Encoder(format!("preparing the audio encoder: {e}")))?;

        let channels = settings.channels.max(1);
        let layout = ffmpeg::ChannelLayout::default(channels as i32);
        let format = sample_format(settings.codec);
        // Sample rate is the audio time base, so a packet's presentation time
        // is a sample index: exact, and impossible to drift.
        let time_base = ffmpeg::Rational::new(1, settings.sample_rate.hz() as i32);

        encoder.set_rate(settings.sample_rate.hz() as i32);
        encoder.set_channels(channels as i32);
        encoder.set_channel_layout(layout);
        encoder.set_format(format);
        encoder.set_time_base(time_base);
        if settings.codec.uses_bitrate() {
            encoder.set_bit_rate(settings.bitrate_kbps as usize * 1000);
        }
        if global_header {
            encoder.set_flags(ffmpeg::codec::Flags::GLOBAL_HEADER);
        }

        let encoder = encoder.open_as(codec).map_err(|e| {
            ExportError::Encoder(format!("opening the {encoder_name} encoder: {e}"))
        })?;

        {
            let mut stream = output.stream_mut(index).expect("the stream just added");
            stream.set_parameters(&encoder);
            stream.set_time_base(time_base);
        }

        let frame_size = match encoder.frame_size() as usize {
            0 => DEFAULT_AUDIO_FRAME,
            n => n,
        };

        Ok(AudioStream {
            encoder,
            encoder_name,
            index,
            time_base,
            stream_time_base: time_base,
            format,
            layout,
            channels: channels as usize,
            frame_size,
            pending: Vec::with_capacity(frame_size * channels as usize * 2),
            next_pts: 0,
        })
    }

    fn push(&mut self, interleaved: &[f32]) {
        self.pending.extend_from_slice(interleaved);
    }

    /// Encodes one full codec frame if there is enough waiting.
    ///
    /// Returns whether one was sent, so the caller can loop until the backlog
    /// is short again.
    fn take_full_frame(&mut self) -> Result<bool, ExportError> {
        let needed = self.frame_size * self.channels;
        if self.pending.len() < needed {
            return Ok(false);
        }
        self.encode(self.frame_size)?;
        Ok(true)
    }

    /// Encodes whatever is left, however short.
    fn flush_remainder(&mut self) -> Result<(), ExportError> {
        while !self.pending.is_empty() {
            let frames = (self.pending.len() / self.channels).min(self.frame_size);
            if frames == 0 {
                // A partial sample frame cannot be encoded and cannot be
                // completed: the mix is over. Nothing audible is in it.
                self.pending.clear();
                break;
            }
            self.encode(frames)?;
        }
        Ok(())
    }

    /// Builds one codec frame from the front of `pending` and sends it.
    fn encode(&mut self, frames: usize) -> Result<(), ExportError> {
        let mut frame = ffmpeg::frame::Audio::new(self.format, frames, self.layout);
        frame.set_rate(self.encoder.rate());
        frame.set_pts(Some(self.next_pts));

        let taken = frames * self.channels;
        match self.format {
            // Planar float: one plane per channel, which is what every
            // compressed codec here wants.
            ffmpeg::format::Sample::F32(ffmpeg::format::sample::Type::Planar) => {
                for channel in 0..self.channels {
                    let plane: &mut [f32] = frame.plane_mut(channel);
                    for (i, out) in plane.iter_mut().take(frames).enumerate() {
                        *out = self.pending[i * self.channels + channel];
                    }
                }
            }
            // Packed 16-bit, written through the byte plane: `plane_mut::<i16>`
            // hands back one sample frame per channel rather than the whole
            // interleaved block, which would silently drop half the samples.
            // Quantised with rounding and clamped, because a sample at exactly
            // full scale must not wrap round to silence.
            ffmpeg::format::Sample::I16(ffmpeg::format::sample::Type::Packed) => {
                let bytes = frame.data_mut(0);
                for (i, sample) in self.pending[..taken].iter().enumerate() {
                    let value = (sample.clamp(-1.0, 1.0) * i16::MAX as f32).round() as i16;
                    bytes[i * 2..i * 2 + 2].copy_from_slice(&value.to_le_bytes());
                }
            }
            other => {
                return Err(ExportError::Encoder(format!(
                    "no conversion to the {other:?} samples this encoder wants"
                )))
            }
        }

        self.pending.drain(..taken);
        self.next_pts += frames as i64;
        self.encoder
            .send_frame(&frame)
            .map_err(|e| ExportError::Encoder(format!("encoding audio: {e}")))
    }

    fn send_eof(&mut self) -> Result<(), ExportError> {
        self.encoder
            .send_eof()
            .map_err(|e| ExportError::Encoder(format!("flushing the audio encoder: {e}")))
    }

    fn drain(
        &mut self,
        output: &mut ffmpeg::format::context::Output,
        draining: bool,
    ) -> Result<(), ExportError> {
        drain_packets(
            &mut self.encoder,
            output,
            self.index,
            self.time_base,
            self.stream_time_base,
            0,
            draining,
        )
    }
}

/// Moves every packet an encoder has ready into the muxer.
///
/// `draining` says whether the encoder has been told the stream is over, which
/// is the only state in which end-of-file is an expected answer rather than a
/// fault.
fn drain_packets(
    encoder: &mut ffmpeg::encoder::Encoder,
    output: &mut ffmpeg::format::context::Output,
    stream: usize,
    time_base: ffmpeg::Rational,
    stream_time_base: ffmpeg::Rational,
    frame_duration: i64,
    draining: bool,
) -> Result<(), ExportError> {
    let mut packet = ffmpeg::Packet::empty();
    loop {
        // EAGAIN is not a failure: it is the encoder saying it wants more
        // frames before it has a packet to give, which is most of the time.
        match encoder.receive_packet(&mut packet) {
            Ok(()) => {
                packet.set_stream(stream);
                // A packet with no duration leaves the muxer to work the track
                // length out from the *last* timestamp, which is one frame
                // short — and a container that is one frame short reports a
                // frame rate that is slightly too high, so everything reading
                // it lands between frames. The video encoder does not set this;
                // the audio one does, from the samples in the frame.
                if packet.duration() == 0 && frame_duration > 0 {
                    packet.set_duration(frame_duration);
                }
                packet.rescale_ts(time_base, stream_time_base);
                packet
                    .write_interleaved(output)
                    .map_err(|e| ExportError::Encoder(format!("writing a packet: {e}")))?;
            }
            Err(ffmpeg::Error::Other { errno }) if errno == ffmpeg::util::error::EAGAIN => {
                return Ok(())
            }
            Err(ffmpeg::Error::Eof) if draining => return Ok(()),
            Err(e) => {
                return Err(ExportError::Encoder(format!("reading an encoded packet: {e}")))
            }
        }
    }
}

/// Tags the primaries and the transfer curve on an encoder.
///
/// Through the context directly because the binding has setters for the colour
/// space and the range but not for these two. Leaving them unset would leave a
/// player to guess at what the picture's primaries are, which is the same
/// mistake as leaving the matrix unsaid — just a less visible one.
fn set_colour_description(encoder: &mut ffmpeg::encoder::video::Video, matrix: colour::Matrix) {
    // SAFETY: the context is live and owned by the caller, and both fields are
    // plain enums on `AVCodecContext` that are read when the encoder is opened.
    unsafe {
        let ctx = encoder.as_mut_ptr();
        (*ctx).color_primaries = matrix.primaries().into();
        (*ctx).color_trc = matrix.transfer().into();
    }
}

/// Allocates an encoder context **with its codec**, so the codec's own defaults
/// are applied.
///
/// The binding's `Context::new` allocates with a null codec, which leaves
/// libavcodec's generic defaults in place. libx264 recognises exactly that set
/// of values — a quantiser step of 3, a keyframe interval of 12, no motion
/// estimation range — calls them "broken ffmpeg default settings", and refuses
/// to open at all. There is no constructor in the binding that takes a codec,
/// so this is the one place the allocation is done by hand.
fn context_for(codec: ffmpeg::Codec) -> Result<ffmpeg::codec::context::Context, ExportError> {
    // SAFETY: `avcodec_alloc_context3` returns either null or a context owned
    // by the caller, which is what `Context::wrap(ptr, None)` takes on — it
    // frees the context on drop and nothing else holds the pointer.
    unsafe {
        let ptr = ffmpeg::ffi::avcodec_alloc_context3(codec.as_ptr());
        if ptr.is_null() {
            return Err(ExportError::Encoder("out of memory allocating an encoder".into()));
        }
        Ok(ffmpeg::codec::context::Context::wrap(ptr, None))
    }
}

/// The first encoder in `names` this build of FFmpeg actually has.
fn find_encoder(names: &[&str]) -> Option<(ffmpeg::Codec, String)> {
    names
        .iter()
        .find_map(|name| ffmpeg::encoder::find_by_name(name).map(|c| (c, name.to_string())))
}

/// What an encoder is handed, given what it can take.
///
/// 4:2:0 for the delivery codecs — what every player and every device
/// decodes — and 4:2:2 10-bit for ProRes, which exists precisely to keep more
/// than that.
fn pixel_format(codec: VideoCodec) -> ffmpeg::format::Pixel {
    match codec {
        VideoCodec::H264 | VideoCodec::H265 => ffmpeg::format::Pixel::YUV420P,
        VideoCodec::ProRes => ffmpeg::format::Pixel::YUV422P10LE,
    }
}

fn sample_format(codec: AudioCodec) -> ffmpeg::format::Sample {
    match codec {
        AudioCodec::Aac => ffmpeg::format::Sample::F32(ffmpeg::format::sample::Type::Planar),
        AudioCodec::Pcm16 => ffmpeg::format::Sample::I16(ffmpeg::format::sample::Type::Packed),
    }
}

/// Private options for the encoder actually chosen.
///
/// Keyed on the encoder's *name* rather than on the codec, because these are
/// libx264's and libx265's own options and mean nothing to FFmpeg's built-in
/// encoders — passing them to one would fail the open.
fn encoder_options(settings: &ExportSettings, encoder: &str) -> ffmpeg::Dictionary<'static> {
    let mut options = ffmpeg::Dictionary::new();
    match encoder {
        "libx264" | "libx265" => {
            // The preset is how long the encoder is allowed to think, which is
            // the other half of what "quality" means: a draft copy is wanted
            // now, and a delivery is worth waiting for.
            options.set(
                "preset",
                match settings.quality {
                    Quality::Draft => "veryfast",
                    Quality::High => "slow",
                    _ => "medium",
                },
            );
            if encoder == "libx265" {
                // x265 writes a banner and a per-frame summary to stderr on
                // its own account, which FFmpeg's log level does not govern.
                options.set("x265-params", "log-level=error");
            }
        }
        "prores_ks" => {
            // Profile 2 is "standard" 422, the ProRes flavour intended for
            // handing work on rather than for archiving or for alpha.
            options.set("profile", "2");
        }
        _ => {}
    }
    options
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_encoder_is_found_for_every_codec_or_named_honestly() {
        ve_media::init().unwrap();
        // Which encoders a build has is not this crate's business, but the
        // lookup itself must work: a name that exists resolves, one that does
        // not is `None` rather than a panic.
        assert!(find_encoder(&["definitely-not-a-codec"]).is_none());
        assert!(find_encoder(&["definitely-not-a-codec", "mpeg4"]).is_some());
    }

    #[test]
    fn x264_options_are_not_handed_to_encoders_that_would_reject_them() {
        let settings = ExportSettings::for_sequence(
            &ve_core::Sequence::new(ve_core::SequenceId::from_raw(1), "s", Default::default()),
            "out.mp4",
        );
        assert!(encoder_options(&settings, "libx264").get("preset").is_some());
        assert!(encoder_options(&settings, "h264").get("preset").is_none());
    }
}
