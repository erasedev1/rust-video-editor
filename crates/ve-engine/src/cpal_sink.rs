//! A [`crate::AudioSink`] backed by the system's default output device.
//!
//! The callback does nothing but copy out of the ring: no locks, no
//! allocation, no decoding. Everything expensive happens on the mixing thread.

use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use ve_time::SampleRate;

use crate::audio::{AudioError, AudioSink};
use crate::ring::Consumer;

/// Plays mixed audio through the default output device.
pub struct CpalSink {
    stream: Option<cpal::Stream>,
    consumer: Option<Consumer>,
    device: cpal::Device,
    config: cpal::StreamConfig,
    sample_rate: SampleRate,
    channels: u16,
}

impl CpalSink {
    /// Opens the default output device.
    ///
    /// The device's own preferred rate and channel count are adopted rather
    /// than forced, because resampling once in the decoder is cheaper and
    /// cleaner than fighting the device. The caller reads them back from
    /// [`AudioSink::sample_rate`] and configures the mixer to match.
    pub fn new(consumer: Consumer) -> Result<Self, AudioError> {
        let host = cpal::default_host();
        let device = host.default_output_device().ok_or(AudioError::NoDevice)?;
        let supported = device
            .default_output_config()
            .map_err(|e| AudioError::UnsupportedFormat(e.to_string()))?;

        let channels = supported.channels();
        let sample_rate = SampleRate::new(supported.sample_rate().0)
            .map_err(|e| AudioError::UnsupportedFormat(e.to_string()))?;
        let config: cpal::StreamConfig = supported.into();

        Ok(CpalSink {
            stream: None,
            consumer: Some(consumer),
            device,
            config,
            sample_rate,
            channels,
        })
    }

    /// The default output device's format, without opening a stream for it.
    ///
    /// Used to size the ring before the sink is built, so the buffer is the
    /// same span of *time* whatever rate the device turns out to want.
    pub fn default_format() -> Result<(SampleRate, u16), AudioError> {
        let host = cpal::default_host();
        let device = host.default_output_device().ok_or(AudioError::NoDevice)?;
        let supported = device
            .default_output_config()
            .map_err(|e| AudioError::UnsupportedFormat(e.to_string()))?;
        let channels = supported.channels();
        let rate = SampleRate::new(supported.sample_rate().0)
            .map_err(|e| AudioError::UnsupportedFormat(e.to_string()))?;
        Ok((rate, channels))
    }

    pub fn device_name(&self) -> String {
        self.device.name().unwrap_or_else(|_| "<unknown>".into())
    }
}

impl AudioSink for CpalSink {
    fn sample_rate(&self) -> SampleRate {
        self.sample_rate
    }

    fn channels(&self) -> u16 {
        self.channels
    }

    fn play(&mut self) -> Result<(), AudioError> {
        if let Some(stream) = &self.stream {
            return stream.play().map_err(|e| AudioError::Device(e.to_string()));
        }

        let consumer = self
            .consumer
            .take()
            .ok_or_else(|| AudioError::Device("sink already consumed its ring".into()))?;

        let stream = self
            .device
            .build_output_stream(
                &self.config,
                move |out: &mut [f32], _: &cpal::OutputCallbackInfo| {
                    // The whole real-time budget: one copy, and silence for any
                    // shortfall. `fill` pads and counts underruns itself.
                    consumer.fill(out);
                },
                |err| log::error!("audio output stream error: {err}"),
                None,
            )
            .map_err(|e| AudioError::Device(e.to_string()))?;

        stream.play().map_err(|e| AudioError::Device(e.to_string()))?;
        self.stream = Some(stream);
        Ok(())
    }

    fn pause(&mut self) -> Result<(), AudioError> {
        match &self.stream {
            Some(stream) => stream.pause().map_err(|e| AudioError::Device(e.to_string())),
            None => Ok(()),
        }
    }
}
