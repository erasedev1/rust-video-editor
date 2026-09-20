//! Reading a media file's metadata without decoding it.

use std::path::Path;

use ffmpeg_next as ffmpeg;
use ve_core::{AudioStreamInfo, MediaInfo, Size, VideoStreamInfo};
use ve_time::{Rate, SampleRate, Ticks};

use crate::{ffmpeg_init, MediaError};

/// Converts an FFmpeg stream timestamp into the editor's timebase.
pub(crate) fn ts_to_ticks(ts: i64, time_base: ffmpeg::Rational) -> Ticks {
    Ticks::from_rational(ts * time_base.numerator() as i64, time_base.denominator() as i64)
}

/// Converts an editor timestamp into microseconds, which is FFmpeg's own
/// `AV_TIME_BASE`.
///
/// This is the unit a **seek** target must be in. `avformat_seek_file` is
/// called with a stream index of -1, and FFmpeg documents that as "a default
/// stream is selected, and the timestamp is automatically converted from
/// AV_TIME_BASE units to the stream specific time_base" — so handing it a
/// timestamp already in the stream's time base asks it to convert twice.
///
/// Getting this wrong does not produce wrong frames, which is why it can hide:
/// a seek that lands too early is still *before* the target, and the decode
/// that follows walks forward to the right frame regardless. It produces a
/// seek that does nothing, and a scrub that decodes the file from its
/// beginning every time it moves backwards.
pub(crate) fn ticks_to_micros(t: Ticks) -> i64 {
    let v = t.raw() as i128 * 1_000_000 / ve_time::TICKS_PER_SECOND as i128;
    v as i64
}

fn rational_to_rate(r: ffmpeg::Rational) -> Option<Rate> {
    let (num, den) = (r.numerator(), r.denominator());
    if num <= 0 || den <= 0 {
        return None;
    }
    Rate::new(num as u32, den as u32).ok()
}

/// Reads container and stream metadata.
///
/// Opens the file, inspects the headers and closes it again; no frames are
/// decoded, so this stays fast enough to run on the import path for a whole
/// folder of media.
pub fn probe(path: impl AsRef<Path>) -> Result<MediaInfo, MediaError> {
    let path = path.as_ref();
    ffmpeg_init()?;

    let input = ffmpeg::format::input(&path).map_err(|e| MediaError::open(path, e))?;

    // The container duration is in AV_TIME_BASE (microsecond) units, and is
    // negative or AV_NOPTS_VALUE for formats that do not know it.
    let container_duration = match input.duration() {
        d if d > 0 => Ticks::from_rational(d, 1_000_000),
        _ => Ticks::ZERO,
    };

    let video = input
        .streams()
        .best(ffmpeg::media::Type::Video)
        .and_then(|stream| probe_video(&stream).ok());

    let audio = input
        .streams()
        .best(ffmpeg::media::Type::Audio)
        .and_then(|stream| probe_audio(&stream).ok());

    // Some containers report no duration of their own; fall back to the longest
    // stream so clips still get a sensible length.
    let duration = if container_duration.is_zero() {
        video
            .as_ref()
            .map(|v: &VideoStreamInfo| v.duration)
            .into_iter()
            .chain(audio.as_ref().map(|a: &AudioStreamInfo| a.duration))
            .max()
            .unwrap_or(Ticks::ZERO)
    } else {
        container_duration
    };

    if video.is_none() && audio.is_none() {
        return Err(MediaError::NoDecodableStreams(path.to_path_buf()));
    }

    Ok(MediaInfo { duration, video, audio, container: input.format().name().to_string() })
}

fn probe_video(stream: &ffmpeg::format::stream::Stream) -> Result<VideoStreamInfo, MediaError> {
    let params = stream.parameters();
    let codec_id = params.id();
    let ctx =
        ffmpeg::codec::context::Context::from_parameters(params).map_err(MediaError::Ffmpeg)?;
    let decoder = ctx.decoder().video().map_err(MediaError::Ffmpeg)?;

    let time_base = stream.time_base();
    let duration = match stream.duration() {
        d if d > 0 => ts_to_ticks(d, time_base),
        _ => Ticks::ZERO,
    };

    // `avg_frame_rate` is the honest choice for variable-frame-rate sources:
    // `r_frame_rate` reports the smallest tick the timestamps use, which for
    // VFR content can be an absurd 1000 fps.
    let rate = rational_to_rate(stream.avg_frame_rate())
        .or_else(|| rational_to_rate(stream.rate()))
        .unwrap_or(Rate::FPS_30);

    let frame_count = match stream.frames() {
        n if n > 0 => Some(n),
        _ => None,
    };

    let sar = decoder.aspect_ratio();
    let sample_aspect_ratio = if sar.numerator() > 0 && sar.denominator() > 0 {
        (sar.numerator() as u32, sar.denominator() as u32)
    } else {
        (1, 1)
    };

    Ok(VideoStreamInfo {
        size: Size::new(decoder.width(), decoder.height()),
        rate,
        duration,
        frame_count,
        codec: codec_name(codec_id),
        pixel_format: format!("{:?}", decoder.format()).to_lowercase(),
        sample_aspect_ratio,
    })
}

fn probe_audio(stream: &ffmpeg::format::stream::Stream) -> Result<AudioStreamInfo, MediaError> {
    let params = stream.parameters();
    let codec_id = params.id();
    let ctx =
        ffmpeg::codec::context::Context::from_parameters(params).map_err(MediaError::Ffmpeg)?;
    let decoder = ctx.decoder().audio().map_err(MediaError::Ffmpeg)?;

    let duration = match stream.duration() {
        d if d > 0 => ts_to_ticks(d, stream.time_base()),
        _ => Ticks::ZERO,
    };

    Ok(AudioStreamInfo {
        sample_rate: SampleRate::new(decoder.rate()).unwrap_or(SampleRate::HZ_48000),
        channels: decoder.channels(),
        duration,
        codec: codec_name(codec_id),
    })
}

fn codec_name(id: ffmpeg::codec::Id) -> String {
    ffmpeg::codec::decoder::find(id)
        .map(|c| c.name().to_string())
        .unwrap_or_else(|| format!("{id:?}").to_lowercase())
}
