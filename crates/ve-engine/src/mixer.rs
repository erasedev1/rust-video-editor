//! Audio mixing.
//!
//! Pure and synchronous: given each contributing clip's samples plus its gain
//! and pan, produce the interleaved output block. Keeping the arithmetic here,
//! away from devices and threads, is what makes gain, pan, fades and clipping
//! testable without an audio device.

use ve_time::SampleRate;

/// One clip's contribution to a mix block.
pub struct MixSource<'a> {
    /// Interleaved samples already at the output rate, starting at the first
    /// sample frame of the block being mixed.
    pub samples: &'a [f32],
    pub channels: u16,
    /// Linear gain, from the clip's animated volume.
    pub gain: f64,
    /// -1 hard left to +1 hard right.
    pub pan: f64,
}

/// What a mix produced, beyond the samples themselves.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct MixStats {
    /// Samples that exceeded full scale and were limited.
    ///
    /// Surfaced rather than swallowed: silent clipping is the kind of fault a
    /// user discovers only after exporting.
    pub clipped: usize,
    pub sources: usize,
}

/// Mixes clips down to the sequence's output format.
#[derive(Debug, Clone, Copy)]
pub struct AudioMixer {
    sample_rate: SampleRate,
    channels: u16,
}

impl AudioMixer {
    pub fn new(sample_rate: SampleRate, channels: u16) -> Self {
        AudioMixer { sample_rate, channels: channels.max(1) }
    }

    pub fn sample_rate(&self) -> SampleRate {
        self.sample_rate
    }

    pub fn channels(&self) -> u16 {
        self.channels
    }

    /// Mixes `frames` sample frames into a freshly allocated interleaved block.
    pub fn mix(&self, sources: &[MixSource<'_>], frames: usize) -> (Vec<f32>, MixStats) {
        let mut out = vec![0.0f32; frames * self.channels as usize];
        let stats = self.mix_into(&mut out, sources, frames);
        (out, stats)
    }

    /// Mixes into an existing buffer, which the ring-filling thread reuses so
    /// that steady-state playback allocates nothing.
    ///
    /// `out` is overwritten, not added to.
    pub fn mix_into(
        &self,
        out: &mut [f32],
        sources: &[MixSource<'_>],
        frames: usize,
    ) -> MixStats {
        let channels = self.channels as usize;
        let wanted = frames * channels;
        let limit = wanted.min(out.len());
        out[..limit].fill(0.0);

        for source in sources {
            let (left, right) = pan_gains(source.gain, source.pan);
            let src_channels = source.channels.max(1) as usize;

            for frame in 0..frames {
                let src_base = frame * src_channels;
                if src_base + src_channels > source.samples.len() {
                    // The source ran out mid-block, which happens at the end of
                    // a clip. The rest of the block stays as whatever the other
                    // sources contributed.
                    break;
                }
                let dst_base = frame * channels;
                if dst_base + channels > limit {
                    break;
                }

                // Mono sources feed both output channels; a stereo source keeps
                // its own imaging and is attenuated per side by the pan.
                let (sl, sr) = if src_channels == 1 {
                    let s = source.samples[src_base];
                    (s, s)
                } else {
                    (source.samples[src_base], source.samples[src_base + 1])
                };

                if channels == 1 {
                    // Downmix: average rather than sum, so a centred stereo
                    // source does not double in level when folded to mono.
                    out[dst_base] += (sl * left as f32 + sr * right as f32) * 0.5;
                } else {
                    out[dst_base] += sl * left as f32;
                    out[dst_base + 1] += sr * right as f32;
                    // Any channels past stereo get the source's first channel,
                    // which is a placeholder until real surround routing exists.
                    for c in 2..channels {
                        if dst_base + c < limit {
                            out[dst_base + c] += sl * left as f32;
                        }
                    }
                }
            }
        }

        let mut clipped = 0usize;
        for sample in out[..limit].iter_mut() {
            if *sample > 1.0 {
                *sample = 1.0;
                clipped += 1;
            } else if *sample < -1.0 {
                *sample = -1.0;
                clipped += 1;
            }
        }

        MixStats { clipped, sources: sources.len() }
    }
}

/// Per-channel gains for a pan position.
///
/// A linear "balance" law: centre is unity on both sides, and panning
/// attenuates the far side without boosting the near one. Constant-power
/// panning would keep perceived loudness steadier across the sweep but either
/// drops the centre by 3 dB or boosts the extremes by 3 dB, and a pan control
/// that quietly changes the level of centred audio, or that can push a mix into
/// clipping, is the worse surprise. A selectable pan law belongs with the rest
/// of the mixer work.
fn pan_gains(gain: f64, pan: f64) -> (f64, f64) {
    let pan = pan.clamp(-1.0, 1.0);
    let gain = gain.max(0.0);
    (gain * (1.0 - pan).min(1.0), gain * (1.0 + pan).min(1.0))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn mixer() -> AudioMixer {
        AudioMixer::new(SampleRate::HZ_48000, 2)
    }

    #[test]
    fn no_sources_mixes_to_silence() {
        let (out, stats) = mixer().mix(&[], 4);
        assert_eq!(out, vec![0.0; 8]);
        assert_eq!(stats, MixStats { clipped: 0, sources: 0 });
    }

    #[test]
    fn a_single_source_at_unity_passes_through_unchanged() {
        let samples = [0.5f32, -0.25, 0.75, -1.0];
        let (out, _) = mixer()
            .mix(&[MixSource { samples: &samples, channels: 2, gain: 1.0, pan: 0.0 }], 2);
        assert_eq!(out, samples.to_vec());
    }

    #[test]
    fn gain_scales_linearly() {
        let samples = [1.0f32, 1.0];
        let (out, _) = mixer()
            .mix(&[MixSource { samples: &samples, channels: 2, gain: 0.5, pan: 0.0 }], 1);
        assert_eq!(out, vec![0.5, 0.5]);

        // Zero gain silences without clipping or NaN.
        let (out, stats) = mixer()
            .mix(&[MixSource { samples: &samples, channels: 2, gain: 0.0, pan: 0.0 }], 1);
        assert_eq!(out, vec![0.0, 0.0]);
        assert_eq!(stats.clipped, 0);
    }

    #[test]
    fn a_negative_gain_is_treated_as_silence_not_an_inversion() {
        let samples = [1.0f32, 1.0];
        let (out, _) = mixer()
            .mix(&[MixSource { samples: &samples, channels: 2, gain: -2.0, pan: 0.0 }], 1);
        assert_eq!(out, vec![0.0, 0.0]);
    }

    #[test]
    fn panning_attenuates_the_far_side_and_leaves_the_centre_at_unity() {
        let samples = [1.0f32, 1.0];
        let m = mixer();

        let (centre, _) =
            m.mix(&[MixSource { samples: &samples, channels: 2, gain: 1.0, pan: 0.0 }], 1);
        assert_eq!(centre, vec![1.0, 1.0], "centre must not change the level");

        let (left, _) =
            m.mix(&[MixSource { samples: &samples, channels: 2, gain: 1.0, pan: -1.0 }], 1);
        assert_eq!(left, vec![1.0, 0.0], "hard left silences the right");

        let (right, _) =
            m.mix(&[MixSource { samples: &samples, channels: 2, gain: 1.0, pan: 1.0 }], 1);
        assert_eq!(right, vec![0.0, 1.0], "hard right silences the left");

        let (half, _) =
            m.mix(&[MixSource { samples: &samples, channels: 2, gain: 1.0, pan: 0.5 }], 1);
        assert_eq!(half, vec![0.5, 1.0], "the near side is never boosted");
    }

    #[test]
    fn an_out_of_range_pan_is_clamped() {
        let samples = [1.0f32, 1.0];
        let (out, _) = mixer()
            .mix(&[MixSource { samples: &samples, channels: 2, gain: 1.0, pan: -5.0 }], 1);
        assert_eq!(out, vec![1.0, 0.0]);
    }

    #[test]
    fn sources_sum_together() {
        let a = [0.25f32, 0.25];
        let b = [0.5f32, -0.5];
        let (out, stats) = mixer().mix(
            &[
                MixSource { samples: &a, channels: 2, gain: 1.0, pan: 0.0 },
                MixSource { samples: &b, channels: 2, gain: 1.0, pan: 0.0 },
            ],
            1,
        );
        assert_eq!(out, vec![0.75, -0.25]);
        assert_eq!(stats.sources, 2);
    }

    #[test]
    fn a_sum_past_full_scale_is_limited_and_counted() {
        let a = [0.8f32, 0.8];
        let (out, stats) = mixer().mix(
            &[
                MixSource { samples: &a, channels: 2, gain: 1.0, pan: 0.0 },
                MixSource { samples: &a, channels: 2, gain: 1.0, pan: 0.0 },
            ],
            1,
        );
        assert_eq!(out, vec![1.0, 1.0]);
        assert_eq!(stats.clipped, 2, "clipping must be reported, not swallowed");
    }

    #[test]
    fn negative_overshoot_is_limited_too() {
        let a = [-0.9f32, -0.9];
        let (out, stats) = mixer().mix(
            &[
                MixSource { samples: &a, channels: 2, gain: 1.0, pan: 0.0 },
                MixSource { samples: &a, channels: 2, gain: 1.0, pan: 0.0 },
            ],
            1,
        );
        assert_eq!(out, vec![-1.0, -1.0]);
        assert_eq!(stats.clipped, 2);
    }

    #[test]
    fn a_mono_source_feeds_both_output_channels() {
        let samples = [0.5f32, 0.25];
        let (out, _) = mixer()
            .mix(&[MixSource { samples: &samples, channels: 1, gain: 1.0, pan: 0.0 }], 2);
        assert_eq!(out, vec![0.5, 0.5, 0.25, 0.25]);
    }

    #[test]
    fn a_stereo_source_folded_to_mono_averages_rather_than_sums() {
        let m = AudioMixer::new(SampleRate::HZ_48000, 1);
        let samples = [1.0f32, 1.0];
        let (out, stats) =
            m.mix(&[MixSource { samples: &samples, channels: 2, gain: 1.0, pan: 0.0 }], 1);
        assert_eq!(out, vec![1.0], "a centred stereo source must not double in mono");
        assert_eq!(stats.clipped, 0);
    }

    #[test]
    fn a_source_that_runs_out_leaves_the_rest_of_the_block_to_the_others() {
        let short = [1.0f32, 1.0]; // one frame
        let long = [0.25f32, 0.25, 0.25, 0.25]; // two frames
        let (out, _) = mixer().mix(
            &[
                MixSource { samples: &short, channels: 2, gain: 1.0, pan: 0.0 },
                MixSource { samples: &long, channels: 2, gain: 1.0, pan: 0.0 },
            ],
            2,
        );
        // First frame has both, second has only the longer source.
        assert_eq!(out, vec![1.0, 1.0, 0.25, 0.25]);
    }

    #[test]
    fn an_empty_source_contributes_nothing_without_panicking() {
        let (out, _) =
            mixer().mix(&[MixSource { samples: &[], channels: 2, gain: 1.0, pan: 0.0 }], 4);
        assert_eq!(out, vec![0.0; 8]);
    }

    #[test]
    fn mixing_into_a_short_buffer_stays_in_bounds() {
        let samples = [1.0f32; 16];
        let mut out = [0.0f32; 4];
        let stats = mixer().mix_into(
            &mut out,
            &[MixSource { samples: &samples, channels: 2, gain: 1.0, pan: 0.0 }],
            8,
        );
        assert_eq!(out, [1.0; 4]);
        assert_eq!(stats.clipped, 0);
    }

    #[test]
    fn mix_into_overwrites_rather_than_accumulating_across_calls() {
        let samples = [0.5f32, 0.5];
        let mut out = vec![9.0f32; 2];
        let m = mixer();
        m.mix_into(
            &mut out,
            &[MixSource { samples: &samples, channels: 2, gain: 1.0, pan: 0.0 }],
            1,
        );
        assert_eq!(out, vec![0.5, 0.5]);
        m.mix_into(
            &mut out,
            &[MixSource { samples: &samples, channels: 2, gain: 1.0, pan: 0.0 }],
            1,
        );
        assert_eq!(out, vec![0.5, 0.5], "a reused buffer must not accumulate");
    }
}
