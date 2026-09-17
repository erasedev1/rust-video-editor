//! Turning a sequence's audio clips into a stream of samples for a device.

use std::collections::HashMap;
use std::path::PathBuf;

use ve_core::{AssetId, Sequence};
use ve_media::{AudioDecoder, MediaError};
use ve_metrics::Metrics;
use ve_time::{SampleRate, Ticks};

use crate::mixer::{AudioMixer, MixSource, MixStats};
use crate::plan::evaluate;
use crate::ring::Producer;

/// One open audio file plus a window of decoded samples around the playhead.
///
/// Audio decoders emit fixed-size blocks that rarely line up with what the
/// device asks for, so each source keeps a small buffer and serves arbitrary
/// slices out of it. Without that, every device callback would force a decode.
struct AudioSource {
    decoder: AudioDecoder,
    /// Interleaved samples at the output format.
    buffer: Vec<f32>,
    /// Source time of the first sample frame in `buffer`.
    buffer_start: Ticks,
    channels: u16,
}

impl AudioSource {
    fn buffered_frames(&self) -> usize {
        if self.channels == 0 {
            0
        } else {
            self.buffer.len() / self.channels as usize
        }
    }

    fn buffer_end(&self, rate: SampleRate) -> Ticks {
        self.buffer_start + rate.sample_to_ticks(self.buffered_frames() as i64)
    }

    /// Makes sure `frames` sample frames from `at` are in the buffer.
    fn ensure(&mut self, at: Ticks, frames: usize, rate: SampleRate) -> Result<(), MediaError> {
        let needed_end = at + rate.sample_to_ticks(frames as i64);
        if !self.buffer.is_empty()
            && at >= self.buffer_start
            && needed_end <= self.buffer_end(rate)
        {
            return Ok(());
        }

        // Decode a little past what was asked for, so the next block is
        // usually already in hand.
        let blocks = self.decoder.buffers_from(at, frames * 2)?;
        self.buffer.clear();
        self.buffer_start = blocks.first().map(|b| b.pts()).unwrap_or(at);
        for block in &blocks {
            self.buffer.extend_from_slice(block.samples());
        }
        Ok(())
    }

    /// The slice covering `frames` sample frames from `at`, or empty if the
    /// buffer does not reach it.
    fn slice(&self, at: Ticks, frames: usize, rate: SampleRate) -> &[f32] {
        let channels = self.channels.max(1) as usize;
        let offset = rate.ticks_to_sample(at - self.buffer_start);
        if offset < 0 {
            return &[];
        }
        let start = offset as usize * channels;
        if start >= self.buffer.len() {
            return &[];
        }
        let end = (start + frames * channels).min(self.buffer.len());
        &self.buffer[start..end]
    }
}

/// Mixes a sequence's audio ahead of the device.
///
/// Runs on an ordinary thread, never in the device callback: it decodes, mixes
/// and pushes into the ring, and the callback only copies out. That separation
/// is what keeps a slow decode from producing an audible click.
pub struct AudioRenderer {
    mixer: AudioMixer,
    sources: HashMap<AssetId, AudioSource>,
    paths: HashMap<AssetId, PathBuf>,
    scratch: Vec<f32>,
    /// Next timeline position to mix from.
    position: Ticks,
    metrics: Metrics,
    last_stats: MixStats,
}

impl AudioRenderer {
    pub fn new(sample_rate: SampleRate, channels: u16, metrics: Metrics) -> Self {
        AudioRenderer {
            mixer: AudioMixer::new(sample_rate, channels),
            sources: HashMap::new(),
            paths: HashMap::new(),
            scratch: Vec::new(),
            position: Ticks::ZERO,
            metrics,
            last_stats: MixStats::default(),
        }
    }

    pub fn mixer(&self) -> &AudioMixer {
        &self.mixer
    }

    pub fn position(&self) -> Ticks {
        self.position
    }

    pub fn last_stats(&self) -> MixStats {
        self.last_stats
    }

    /// Registers where an asset's media lives. Decoders open lazily, on the
    /// first block that actually needs them.
    pub fn register_asset(&mut self, asset: AssetId, path: impl Into<PathBuf>) {
        self.paths.insert(asset, path.into());
    }

    pub fn forget_asset(&mut self, asset: AssetId) {
        self.paths.remove(&asset);
        self.sources.remove(&asset);
    }

    /// Restarts mixing at a new timeline position, discarding what was buffered.
    pub fn seek(&mut self, to: Ticks) {
        self.position = to.clamp_non_negative();
    }

    /// Mixes up to `max_frames` sample frames and pushes them into `producer`.
    ///
    /// Returns how many sample frames were actually written, which is limited
    /// by the ring's free space: a full ring means the mixer is comfortably
    /// ahead of the device and there is nothing to do.
    pub fn render_into(
        &mut self,
        sequence: &Sequence,
        producer: &Producer,
        max_frames: usize,
    ) -> usize {
        let channels = self.mixer.channels() as usize;
        let free_frames = producer.free() / channels.max(1);
        let frames = max_frames.min(free_frames);
        if frames == 0 {
            return 0;
        }

        let rate = self.mixer.sample_rate();
        let plan = evaluate(sequence, self.position);

        // Gather each clip's samples first, then mix: the borrow checker will
        // not allow decoding into `self.sources` while the mixer holds slices
        // out of it, and collecting the owned blocks is the honest fix.
        let mut blocks: Vec<(Vec<f32>, u16, f64, f64)> = Vec::new();
        for clip in &plan.audio {
            if clip.gain <= 0.0 {
                continue;
            }
            // Only media makes sound here. A nested composition's audio is part
            // of that composition's own plan, which the engine mixes separately.
            let Some(asset) = clip.source.asset() else { continue };
            let Some(source) = self.source_for(asset) else { continue };
            if let Err(e) = source.ensure(clip.source_time, frames, rate) {
                log::warn!("audio decode failed for asset {asset}: {e}");
                continue;
            }
            let slice = source.slice(clip.source_time, frames, rate);
            if slice.is_empty() {
                continue;
            }
            blocks.push((slice.to_vec(), source.channels, clip.gain, clip.pan));
        }

        let sources: Vec<MixSource<'_>> = blocks
            .iter()
            .map(|(samples, channels, gain, pan)| MixSource {
                samples,
                channels: *channels,
                gain: *gain,
                pan: *pan,
            })
            .collect();

        self.scratch.resize(frames * channels, 0.0);
        self.last_stats = self.mixer.mix_into(&mut self.scratch, &sources, frames);
        if self.last_stats.clipped > 0 {
            self.metrics.incr("audio_clipped_samples", self.last_stats.clipped as u64);
        }

        let written = producer.push(&self.scratch[..frames * channels]);
        let written_frames = written / channels.max(1);
        self.position += rate.sample_to_ticks(written_frames as i64);
        written_frames
    }

    /// Opens an asset's decoder if it is not open yet.
    fn source_for(&mut self, asset: AssetId) -> Option<&mut AudioSource> {
        if !self.sources.contains_key(&asset) {
            let path = self.paths.get(&asset)?.clone();
            let rate = self.mixer.sample_rate();
            let channels = self.mixer.channels();
            match AudioDecoder::open(&path, rate, channels) {
                Ok(decoder) => {
                    self.sources.insert(
                        asset,
                        AudioSource {
                            decoder,
                            buffer: Vec::new(),
                            buffer_start: Ticks::ZERO,
                            channels,
                        },
                    );
                }
                Err(e) => {
                    log::debug!("no audio in asset {asset} ({}): {e}", path.display());
                    // Remember the failure so it is not retried on every block.
                    self.paths.remove(&asset);
                    return None;
                }
            }
        }
        self.sources.get_mut(&asset)
    }
}

/// Where mixed audio goes.
///
/// Abstracted so the whole pipeline — scheduling, mixing, ring, timing — can be
/// driven and asserted on without a sound card, and so that an offline export
/// can consume exactly the same stream a live device would.
///
/// Deliberately not `Send`. A platform audio stream is thread-affine on several
/// backends, so a sink belongs to the thread that opened it — in practice the
/// UI thread. Nothing is lost by that: the only thing that has to cross a
/// thread boundary is the ring's [`Producer`], which the mixing thread owns,
/// and it is `Send` on its own.
///
/// [`Producer`]: crate::ring::Producer
pub trait AudioSink {
    /// Output sample rate the sink wants.
    fn sample_rate(&self) -> SampleRate;
    /// Output channel count the sink wants.
    fn channels(&self) -> u16;
    /// Begins pulling from the consumer end supplied at construction.
    fn play(&mut self) -> Result<(), AudioError>;
    fn pause(&mut self) -> Result<(), AudioError>;
}

#[derive(Debug, thiserror::Error)]
pub enum AudioError {
    #[error("no audio output device is available")]
    NoDevice,
    #[error("the audio device rejected the requested format: {0}")]
    UnsupportedFormat(String),
    #[error("audio device error: {0}")]
    Device(String),
}
