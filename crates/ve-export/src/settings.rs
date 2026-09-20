//! What an export is: where it goes, what it covers, and how it is encoded.
//!
//! Deliberately plain data with no FFmpeg types in it, so the interface can
//! build one, a test can assert on one, and a future command-line exporter can
//! parse one from arguments without any of them touching a codec.

use std::path::{Path, PathBuf};

use ve_core::{Sequence, Size};
use ve_time::{Rate, SampleRate, Ticks, TimeRange};

use crate::ExportError;

/// The container a file is written into, chosen by its extension.
///
/// A short list on purpose. Each entry is a container this crate knows which
/// codecs are legal in, and offering one it cannot check would mean discovering
/// the mismatch from a muxer error halfway through a long render.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Container {
    Mp4,
    Mov,
    Mkv,
}

impl Container {
    pub fn extension(self) -> &'static str {
        match self {
            Container::Mp4 => "mp4",
            Container::Mov => "mov",
            Container::Mkv => "mkv",
        }
    }

    /// The container an extension names, or `None` for anything else.
    pub fn from_extension(ext: &str) -> Option<Container> {
        match ext.to_ascii_lowercase().as_str() {
            "mp4" | "m4v" => Some(Container::Mp4),
            "mov" => Some(Container::Mov),
            "mkv" => Some(Container::Mkv),
            _ => None,
        }
    }

    pub fn from_path(path: &Path) -> Option<Container> {
        Container::from_extension(path.extension()?.to_str()?)
    }

    /// Whether this container can carry `codec`.
    ///
    /// ProRes in MP4 is the case this exists for: FFmpeg will write it, and
    /// almost nothing will play it. Refusing early is kinder than a file that
    /// only fails when it is handed on.
    pub fn accepts(self, codec: VideoCodec) -> bool {
        !matches!((self, codec), (Container::Mp4, VideoCodec::ProRes))
    }
}

/// How the picture is encoded.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VideoCodec {
    /// H.264, through libx264. The delivery default everything plays.
    H264,
    /// H.265, through libx265. Half the bitrate for the same picture, at
    /// several times the encoding cost.
    H265,
    /// Apple ProRes 422. Intra-frame and far larger, for handing work to
    /// another application rather than to an audience.
    ProRes,
}

impl VideoCodec {
    pub fn label(self) -> &'static str {
        match self {
            VideoCodec::H264 => "H.264",
            VideoCodec::H265 => "H.265",
            VideoCodec::ProRes => "ProRes 422",
        }
    }

    /// Whether the encoder is driven by a bitrate at all.
    ///
    /// ProRes is quantiser-driven per profile: handing it a bitrate would be
    /// ignored, and offering the control would be a lie.
    pub fn uses_bitrate(self) -> bool {
        !matches!(self, VideoCodec::ProRes)
    }

    /// The encoders to try, in order of preference.
    ///
    /// A list rather than one name because a build of FFmpeg without libx264
    /// still has its own H.264 encoder for some profiles, and falling back
    /// beats refusing to export at all.
    pub fn encoder_names(self) -> &'static [&'static str] {
        match self {
            VideoCodec::H264 => &["libx264", "h264"],
            VideoCodec::H265 => &["libx265", "hevc"],
            VideoCodec::ProRes => &["prores_ks", "prores_aw", "prores"],
        }
    }
}

/// How the sound is encoded.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AudioCodec {
    Aac,
    /// Uncompressed 16-bit PCM, for handing on rather than for delivery.
    Pcm16,
}

impl AudioCodec {
    pub fn label(self) -> &'static str {
        match self {
            AudioCodec::Aac => "AAC",
            AudioCodec::Pcm16 => "PCM 16-bit",
        }
    }

    pub fn encoder_names(self) -> &'static [&'static str] {
        match self {
            AudioCodec::Aac => &["aac"],
            AudioCodec::Pcm16 => &["pcm_s16le"],
        }
    }

    pub fn uses_bitrate(self) -> bool {
        matches!(self, AudioCodec::Aac)
    }
}

/// How much bitrate to spend.
///
/// Presets rather than a number in the interface, because the number that means
/// "good" depends on the resolution and the frame rate, and a user who has to
/// work that out is being asked to do the encoder's job. The number is still
/// available — [`Quality::Custom`] — for anyone delivering to a specification.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Quality {
    /// Small and quick, for a review copy.
    Draft,
    /// What most deliveries want.
    Standard,
    /// Visually transparent on ordinary material, at roughly twice the size.
    High,
    /// An exact bitrate in kilobits per second.
    Custom(u32),
}

impl Quality {
    pub fn label(self) -> &'static str {
        match self {
            Quality::Draft => "Draft",
            Quality::Standard => "Standard",
            Quality::High => "High",
            Quality::Custom(_) => "Custom",
        }
    }

    /// Bits per second for a picture of `size` at `rate`.
    ///
    /// Derived from pixels per second rather than from a table of resolutions,
    /// so an unusual canvas or a high frame rate gets a bitrate in proportion
    /// instead of whatever the nearest named preset happened to be. The
    /// bits-per-pixel figures are the conventional H.264 ones; H.265 is given
    /// less for the same picture because that is the whole point of it.
    pub fn bits_per_second(self, size: Size, rate: Rate, codec: VideoCodec) -> u32 {
        if let Quality::Custom(kbps) = self {
            return kbps.saturating_mul(1000);
        }
        let pixels_per_second = size.width as f64 * size.height as f64 * rate.as_f64();
        let bits_per_pixel = match self {
            Quality::Draft => 0.04,
            Quality::High => 0.20,
            _ => 0.10,
        };
        let efficiency = match codec {
            VideoCodec::H265 => 0.6,
            _ => 1.0,
        };
        let bps = pixels_per_second * bits_per_pixel * efficiency;
        // A floor so that a tiny canvas — a test fixture, a thumbnail render —
        // is not handed a bitrate the encoder cannot make a picture out of.
        bps.max(64_000.0).min(u32::MAX as f64) as u32
    }
}

/// How often the encoder writes a keyframe.
///
/// This is the difference between a file that is *small* and a file that is
/// *quick to jump about in*, and the right answer is not the same for both jobs.
/// A delivery is played from the beginning, so a group of pictures a second long
/// costs nothing anyone notices. A file being scrubbed is asked for one frame
/// from the middle over and over, and every one of those costs the whole group
/// up to it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Keyframes {
    /// One a second. What a delivery wants.
    #[default]
    EverySecond,
    /// Every frame, so no frame is decoded twice to reach another. Larger, and
    /// the reason editing formats are intra-frame.
    EveryFrame,
}

impl Keyframes {
    /// The group size to give the encoder at `rate`.
    pub fn gop(self, rate: Rate) -> u32 {
        match self {
            Keyframes::EverySecond => rate.as_f64().round().max(1.0) as u32,
            Keyframes::EveryFrame => 1,
        }
    }
}

/// The sound half of an export.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AudioSettings {
    pub codec: AudioCodec,
    pub sample_rate: SampleRate,
    pub channels: u16,
    /// Kilobits per second, ignored by codecs that are not compressed.
    pub bitrate_kbps: u32,
}

impl AudioSettings {
    /// The sequence's own format, which is what the mixer is already producing.
    pub fn for_sequence(sequence: &Sequence) -> Self {
        AudioSettings {
            codec: AudioCodec::Aac,
            sample_rate: sequence.settings.sample_rate,
            channels: sequence.settings.channels.max(1),
            bitrate_kbps: 192,
        }
    }
}

/// Which part of the sequence to write.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExportRange {
    /// From zero to the end of the last clip.
    Whole,
    /// The work area if one is set, and the whole sequence otherwise. The same
    /// rule [`Sequence::active_range`] follows, so what is exported is what the
    /// editor says is being worked on.
    Active,
    /// An explicit span, for an in and out point set for this export alone.
    Span(TimeRange),
}

impl ExportRange {
    /// The span this means for `sequence`, snapped to whole frames.
    ///
    /// Snapped because an export is a whole number of frames by definition: a
    /// range ending halfway through a frame would either write a frame that is
    /// mostly outside the range or drop one that is mostly inside it, and
    /// neither is what an in and out point mean.
    pub fn resolve(self, sequence: &Sequence) -> TimeRange {
        let rate = sequence.rate();
        let raw = match self {
            ExportRange::Whole => TimeRange::new(Ticks::ZERO, sequence.duration()),
            ExportRange::Active => sequence.active_range(),
            ExportRange::Span(range) => range,
        };
        let start = rate.snap_round(raw.start.clamp_non_negative());
        let end = rate.snap_round(raw.end().clamp_non_negative()).max(start);
        TimeRange::from_bounds(start, end)
    }
}

/// Everything one export needs to know.
#[derive(Debug, Clone, PartialEq)]
pub struct ExportSettings {
    pub path: PathBuf,
    pub range: ExportRange,
    /// The canvas written out, which need not be the sequence's own: a 1080p
    /// review copy of a 4K sequence is one setting rather than a second
    /// sequence. Compositing still happens at the **sequence's** resolution and
    /// the result is scaled on the way to the encoder, so a mask or a blur is
    /// the size the editor showed it.
    pub resolution: Size,
    pub rate: Rate,
    pub video: VideoCodec,
    pub quality: Quality,
    /// `None` writes a file with no audio stream at all, rather than one with a
    /// silent one.
    pub audio: Option<AudioSettings>,
    /// How often a keyframe is written. A delivery wants one a second; a file
    /// that will be scrubbed wants every frame.
    pub keyframes: Keyframes,
}

impl ExportSettings {
    /// Sensible settings for delivering `sequence` to `path`: the sequence's own
    /// canvas, rate and audio format, at standard quality.
    pub fn for_sequence(sequence: &Sequence, path: impl Into<PathBuf>) -> Self {
        ExportSettings {
            path: path.into(),
            range: ExportRange::Active,
            resolution: sequence.settings.resolution,
            rate: sequence.rate(),
            video: VideoCodec::H264,
            quality: Quality::Standard,
            audio: Some(AudioSettings::for_sequence(sequence)),
            keyframes: Keyframes::EverySecond,
        }
    }

    pub fn with_range(mut self, range: ExportRange) -> Self {
        self.range = range;
        self
    }

    pub fn with_video(mut self, codec: VideoCodec) -> Self {
        self.video = codec;
        self
    }

    pub fn without_audio(mut self) -> Self {
        self.audio = None;
        self
    }

    pub fn with_keyframes(mut self, keyframes: Keyframes) -> Self {
        self.keyframes = keyframes;
        self
    }

    pub fn container(&self) -> Option<Container> {
        Container::from_path(&self.path)
    }

    pub fn bitrate(&self) -> u32 {
        self.quality.bits_per_second(self.resolution, self.rate, self.video)
    }

    /// Everything that can be rejected before a single frame is rendered.
    ///
    /// Called by [`crate::run`] before the file is created, so a mistake costs
    /// a message rather than a minute of encoding followed by a muxer error.
    pub fn validate(&self) -> Result<Container, ExportError> {
        let container = self.container().ok_or_else(|| {
            ExportError::Settings(format!(
                "{} is not a container Verge writes — use .mp4, .mov or .mkv",
                self.path.display()
            ))
        })?;
        if !container.accepts(self.video) {
            return Err(ExportError::Settings(format!(
                "{} cannot be written into a .{} file",
                self.video.label(),
                container.extension()
            )));
        }
        if self.resolution.width == 0 || self.resolution.height == 0 {
            return Err(ExportError::Settings("the export has no picture size".into()));
        }
        // Every codec here subsamples chroma, which needs an even number of
        // pixels on each axis. Rounding silently would write a file a pixel
        // different from the one that was asked for.
        if !self.resolution.width.is_multiple_of(2) || !self.resolution.height.is_multiple_of(2)
        {
            return Err(ExportError::Settings(format!(
                "{}×{} has an odd dimension; video codecs need both to be even",
                self.resolution.width, self.resolution.height
            )));
        }
        if let Some(audio) = &self.audio {
            if audio.channels == 0 {
                return Err(ExportError::Settings("audio with no channels".into()));
            }
        }
        Ok(container)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ve_core::SequenceId;

    fn sequence() -> Sequence {
        Sequence::new(SequenceId::from_raw(1), "seq", Default::default())
    }

    #[test]
    fn a_container_is_chosen_by_extension_whatever_its_case() {
        assert_eq!(Container::from_path(Path::new("out.MP4")), Some(Container::Mp4));
        assert_eq!(Container::from_path(Path::new("out.mov")), Some(Container::Mov));
        assert_eq!(Container::from_path(Path::new("out.verge")), None);
        assert_eq!(Container::from_path(Path::new("out")), None);
    }

    #[test]
    fn prores_in_mp4_is_refused_before_anything_is_rendered() {
        let settings =
            ExportSettings::for_sequence(&sequence(), "out.mp4").with_video(VideoCodec::ProRes);
        assert!(settings.validate().is_err());
        let settings =
            ExportSettings::for_sequence(&sequence(), "out.mov").with_video(VideoCodec::ProRes);
        assert_eq!(settings.validate().unwrap(), Container::Mov);
    }

    #[test]
    fn an_odd_dimension_is_refused_rather_than_rounded() {
        let mut settings = ExportSettings::for_sequence(&sequence(), "out.mp4");
        settings.resolution = Size::new(1921, 1080);
        assert!(settings.validate().is_err());
    }

    #[test]
    fn bitrate_follows_pixels_per_second_rather_than_a_table() {
        let hd = Size::new(1920, 1080);
        let uhd = Size::new(3840, 2160);
        let standard = Quality::Standard;
        let hd_bps = standard.bits_per_second(hd, Rate::FPS_30, VideoCodec::H264);
        let uhd_bps = standard.bits_per_second(uhd, Rate::FPS_30, VideoCodec::H264);
        assert_eq!(uhd_bps, hd_bps * 4);

        // Twice the frames, twice the bits.
        let hd60 = standard.bits_per_second(hd, Rate::FPS_60, VideoCodec::H264);
        assert_eq!(hd60, hd_bps * 2);

        // H.265 is asked for less to produce the same picture.
        assert!(standard.bits_per_second(hd, Rate::FPS_30, VideoCodec::H265) < hd_bps);

        // A custom rate is taken literally.
        assert_eq!(
            Quality::Custom(5_000).bits_per_second(hd, Rate::FPS_30, VideoCodec::H264),
            5_000_000
        );
    }

    #[test]
    fn a_range_is_snapped_to_whole_frames() {
        let mut seq = sequence();
        seq.settings.rate = Rate::FPS_25;
        let frame = Rate::FPS_25.frame_duration();
        // A span that starts and ends a third of a frame late.
        let third = Ticks::new(frame.raw() / 3);
        let range = ExportRange::Span(TimeRange::from_bounds(third, frame * 4 + third));
        let resolved = range.resolve(&seq);
        assert_eq!(resolved.start, Ticks::ZERO);
        assert_eq!(resolved.end(), frame * 4);
    }
}
