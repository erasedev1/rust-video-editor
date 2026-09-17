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

/// A peak level, per side of the stereo field.
///
/// Peak rather than RMS because a meter's job in an editor is to show how close
/// the mix is to the ceiling, and only the peak answers that. Loudness is a
/// different measurement and belongs with the loudness work, not here.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct Meter {
    pub left: f32,
    pub right: f32,
}

impl Meter {
    pub const SILENT: Meter = Meter { left: 0.0, right: 0.0 };

    pub fn peak(self) -> f32 {
        self.left.max(self.right)
    }

    pub fn is_silent(self) -> bool {
        self.left <= 0.0 && self.right <= 0.0
    }

    /// Whether this level reached or passed full scale.
    pub fn is_over(self) -> bool {
        self.peak() >= 1.0
    }

    /// Takes the louder of the two on each side. Used to fold several blocks,
    /// or several clips on one track, into a single reading.
    pub fn absorb(&mut self, other: Meter) {
        self.left = self.left.max(other.left);
        self.right = self.right.max(other.right);
    }

    /// The level scaled by `factor`, for a meter that falls back towards
    /// silence between blocks instead of freezing at the last peak.
    pub fn decayed(self, factor: f32) -> Meter {
        Meter { left: self.left * factor, right: self.right * factor }
    }

    /// Full scale in decibels, `0.0` being unity and `None` being silence.
    ///
    /// `None` rather than negative infinity so a caller has to say what it
    /// wants drawn for silence instead of formatting `-inf` into the interface.
    pub fn dbfs(self) -> Option<f32> {
        let peak = self.peak();
        (peak > 0.0).then(|| 20.0 * peak.log10())
    }
}

/// What a mix produced, beyond the samples themselves.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct MixStats {
    /// Samples that exceeded full scale and were limited.
    ///
    /// Surfaced rather than swallowed: silent clipping is the kind of fault a
    /// user discovers only after exporting.
    pub clipped: usize,
    pub sources: usize,
    /// The loudest sample in the block, per side, **before** limiting.
    ///
    /// Before rather than after, so a meter can show by how much a mix went
    /// over rather than pinning at exactly full scale and saying nothing. The
    /// samples themselves are still limited; only the reading is unclamped.
    pub master: Meter,
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
        self.mix_metered(out, sources, frames, &mut [])
    }

    /// Mixes, and additionally reports what each source contributed.
    ///
    /// `meters` is parallel to `sources` and may be shorter — a caller that
    /// wants no per-source reading passes an empty slice and pays nothing for
    /// it. Per source rather than per track because the mixer has no idea what
    /// a track is: grouping the readings is the caller's business, and keeping
    /// it that way is what lets the same mixer serve a live meter, an export
    /// report and a test.
    pub fn mix_metered(
        &self,
        out: &mut [f32],
        sources: &[MixSource<'_>],
        frames: usize,
        meters: &mut [Meter],
    ) -> MixStats {
        let channels = self.channels as usize;
        let wanted = frames * channels;
        let limit = wanted.min(out.len());
        out[..limit].fill(0.0);

        for meter in meters.iter_mut() {
            *meter = Meter::SILENT;
        }

        for (index, source) in sources.iter().enumerate() {
            let (left, right) = pan_gains(source.gain, source.pan);
            let src_channels = source.channels.max(1) as usize;
            let mut meter = Meter::SILENT;

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

                // What this source puts into the mix, which is what its own
                // meter reads — after its gain and pan, before everything else
                // sums on top of it.
                let (cl, cr) = (sl * left as f32, sr * right as f32);
                meter.absorb(Meter { left: cl.abs(), right: cr.abs() });

                if channels == 1 {
                    // Downmix: average rather than sum, so a centred stereo
                    // source does not double in level when folded to mono.
                    out[dst_base] += (cl + cr) * 0.5;
                } else {
                    out[dst_base] += cl;
                    out[dst_base + 1] += cr;
                    // Any channels past stereo get the source's first channel,
                    // which is a placeholder until real surround routing exists.
                    for c in 2..channels {
                        if dst_base + c < limit {
                            out[dst_base + c] += cl;
                        }
                    }
                }
            }

            if let Some(slot) = meters.get_mut(index) {
                *slot = meter;
            }
        }

        let mut clipped = 0usize;
        let mut master = Meter::SILENT;
        for (index, sample) in out[..limit].iter_mut().enumerate() {
            // Read before limiting, so the meter can show the overshoot that
            // `clipped` is counting rather than pinning silently at full scale.
            let level = sample.abs();
            if channels > 1 && index % channels == 1 {
                master.right = master.right.max(level);
            } else {
                master.left = master.left.max(level);
            }

            if *sample > 1.0 {
                *sample = 1.0;
                clipped += 1;
            } else if *sample < -1.0 {
                *sample = -1.0;
                clipped += 1;
            }
        }
        if channels == 1 {
            master.right = master.left;
        }

        MixStats { clipped, sources: sources.len(), master }
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
        assert_eq!(stats, MixStats { clipped: 0, sources: 0, master: Meter::SILENT });
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
    fn the_master_meter_reads_the_loudest_sample_on_each_side() {
        let samples = [0.5f32, -0.25];
        let (_, stats) = mixer()
            .mix(&[MixSource { samples: &samples, channels: 2, gain: 1.0, pan: 0.0 }], 1);
        assert_eq!(stats.master, Meter { left: 0.5, right: 0.25 });
        assert_eq!(stats.master.peak(), 0.5);
    }

    /// The reading is taken before limiting, which is the whole point of
    /// measuring it there: a mix that went 6 dB over says so.
    #[test]
    fn the_master_meter_shows_an_overshoot_rather_than_pinning_at_full_scale() {
        let samples = [2.0f32, 2.0];
        let (out, stats) = mixer()
            .mix(&[MixSource { samples: &samples, channels: 2, gain: 1.0, pan: 0.0 }], 1);
        assert_eq!(out, vec![1.0, 1.0], "the samples themselves are still limited");
        assert_eq!(stats.master, Meter { left: 2.0, right: 2.0 });
        assert!(stats.master.is_over());
        assert_eq!(stats.clipped, 2);
    }

    #[test]
    fn per_source_meters_read_each_contribution_after_its_own_gain_and_pan() {
        let loud = [1.0f32, 1.0];
        let quiet = [0.5f32, 0.5];
        let mut out = vec![0.0f32; 2];
        let mut meters = [Meter::SILENT; 2];
        let stats = mixer().mix_metered(
            &mut out,
            &[
                MixSource { samples: &loud, channels: 2, gain: 0.25, pan: 0.0 },
                MixSource { samples: &quiet, channels: 2, gain: 1.0, pan: 1.0 },
            ],
            1,
            &mut meters,
        );
        assert_eq!(meters[0], Meter { left: 0.25, right: 0.25 });
        assert_eq!(meters[1], Meter { left: 0.0, right: 0.5 }, "hard right silences the left");
        // And the master is the sum of the two, not either one.
        assert_eq!(stats.master, Meter { left: 0.25, right: 0.75 });
    }

    #[test]
    fn a_meter_slice_shorter_than_the_sources_is_filled_as_far_as_it_goes() {
        let samples = [1.0f32, 1.0];
        let mut out = vec![0.0f32; 2];
        let mut meters = [Meter::SILENT; 1];
        mixer().mix_metered(
            &mut out,
            &[
                MixSource { samples: &samples, channels: 2, gain: 0.5, pan: 0.0 },
                MixSource { samples: &samples, channels: 2, gain: 0.5, pan: 0.0 },
            ],
            1,
            &mut meters,
        );
        assert_eq!(meters[0], Meter { left: 0.5, right: 0.5 });
    }

    #[test]
    fn meters_are_reset_between_blocks_rather_than_holding_an_old_peak() {
        let loud = [1.0f32, 1.0];
        let quiet = [0.1f32, 0.1];
        let m = mixer();
        let mut out = vec![0.0f32; 2];
        let mut meters = [Meter::SILENT; 1];

        m.mix_metered(
            &mut out,
            &[MixSource { samples: &loud, channels: 2, gain: 1.0, pan: 0.0 }],
            1,
            &mut meters,
        );
        assert_eq!(meters[0].peak(), 1.0);

        m.mix_metered(
            &mut out,
            &[MixSource { samples: &quiet, channels: 2, gain: 1.0, pan: 0.0 }],
            1,
            &mut meters,
        );
        assert!((meters[0].peak() - 0.1).abs() < 1e-6, "a stale peak would hide a drop");
    }

    #[test]
    fn a_meter_reports_full_scale_as_zero_decibels_and_silence_as_nothing() {
        assert_eq!(Meter { left: 1.0, right: 1.0 }.dbfs(), Some(0.0));
        assert_eq!(Meter::SILENT.dbfs(), None);
        let half = Meter { left: 0.5, right: 0.0 }.dbfs().expect("not silent");
        assert!((half + 6.0206).abs() < 0.001, "half amplitude is about -6 dB, got {half}");
    }

    #[test]
    fn a_decayed_meter_falls_towards_silence() {
        let meter = Meter { left: 1.0, right: 0.5 }.decayed(0.5);
        assert_eq!(meter, Meter { left: 0.5, right: 0.25 });
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
