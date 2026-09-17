//! Waveform peaks: what an audio clip looks like.
//!
//! # Why peaks rather than samples
//!
//! A minute of stereo 48 kHz audio is 5.8 million sample frames. Drawing it
//! means turning those into a few hundred pixel columns, and doing that work
//! again on every repaint — during a scroll, a zoom, a drag — would cost more
//! than compositing the picture does. So the samples are reduced once, to a
//! fixed grid of [`Peak`]s, and every later question is answered from that.
//!
//! The grid is [`PEAKS_PER_SECOND`] buckets a second, an exact divisor of the
//! timebase so a bucket boundary is a whole number of ticks and no bucket
//! drifts against the timeline. At twelve bytes a bucket that is 2.4 kB per
//! second of audio, about 8.6 MB an hour — small enough to keep for every asset
//! in a project under the same kind of byte budget the frame cache uses.
//!
//! # What a bucket holds
//!
//! The envelope (`min`, `max`) and the `rms` level, all reduced across
//! channels. The envelope is what makes a transient visible: averaging would
//! hide a single-sample click that the min/max pair shows plainly. RMS is drawn
//! inside the envelope because it is much closer to what the clip *sounds*
//! like — a square wave and a spiky one can share an envelope and be ten
//! decibels apart.
//!
//! # The resolution limit
//!
//! Five milliseconds is finer than the eye can use at ordinary zooms and
//! coarser than the editor's maximum zoom of 4000 px/s, where one bucket is
//! twenty pixels wide. Rather than store more, [`Waveform::envelope`]
//! interpolates between bucket centres once a column is narrower than a bucket,
//! so the drawn shape stays smooth; it is a smooth *summary* at that zoom, not
//! the sample data. Editing that needs individual samples — a click repair —
//! would read the file, not this.

use std::sync::Arc;

use ve_time::{SampleRate, Ticks, TimeRange, TICKS_PER_SECOND};

use crate::decoder::AudioDecoder;
use crate::frame::AudioBuffer;
use crate::MediaError;

/// Buckets per second of audio. Divides [`TICKS_PER_SECOND`] exactly.
pub const PEAKS_PER_SECOND: i64 = 200;

/// Length of one bucket, in ticks. Exact by construction.
pub const BUCKET_TICKS: i64 = TICKS_PER_SECOND / PEAKS_PER_SECOND;

/// How many buckets of one level a bucket of the level above summarises.
const MIP_FACTOR: usize = 8;

/// Coarse levels kept above the base grid.
///
/// Four takes the grid from 200 buckets a second to one per 20.5 seconds. At
/// the timeline's widest zoom — half a pixel a second — that is one bucket per
/// ten pixels, so the coarsest question the interface can ask reads tens of
/// buckets rather than hundreds of thousands. They cost about a seventh of the
/// base grid in memory, all the levels together.
const MIP_LEVELS: usize = 4;

/// Channels the analyser decodes to.
///
/// A 5.1 source is downmixed rather than analysed in full: the display is one
/// mono trace either way, and decoding six channels to draw one is work with no
/// visible result. Two rather than one because a downmix to mono cancels
/// out-of-phase content, which would draw a loud passage as a quiet one.
pub const ANALYSIS_CHANNELS: u16 = 2;

/// One bucket: the shape of a few milliseconds of audio.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Peak {
    pub min: f32,
    pub max: f32,
    pub rms: f32,
}

impl Peak {
    pub const SILENT: Peak = Peak { min: 0.0, max: 0.0, rms: 0.0 };

    /// The larger of the two excursions, for a symmetric display.
    pub fn amplitude(self) -> f32 {
        self.max.max(-self.min).max(0.0)
    }

    /// Reduces several buckets to one.
    ///
    /// The envelope is the union, so a transient survives being zoomed out —
    /// which is the whole point of keeping min and max rather than an average.
    /// RMS combines as a quadratic mean, which is what RMS *is*; taking the
    /// maximum instead would make every zoomed-out waveform look louder than
    /// it is.
    pub fn merge(peaks: &[Peak]) -> Peak {
        if peaks.is_empty() {
            return Peak::SILENT;
        }
        let mut min = f32::INFINITY;
        let mut max = f32::NEG_INFINITY;
        let mut sum_sq = 0.0f64;
        for p in peaks {
            min = min.min(p.min);
            max = max.max(p.max);
            sum_sq += (p.rms as f64) * (p.rms as f64);
        }
        Peak { min, max, rms: (sum_sq / peaks.len() as f64).sqrt() as f32 }
    }

    /// Merges groups of buckets whose sizes differ.
    ///
    /// Each part is weighted by how many base buckets it stands for, so a
    /// summary assembled from one coarse group and a few fine ones reports the
    /// level the audio actually has rather than the level of whichever part
    /// happened to be counted as one.
    fn merge_weighted(parts: &[(Peak, usize)]) -> Peak {
        let total: usize = parts.iter().map(|(_, n)| *n).sum();
        if total == 0 {
            return Peak::SILENT;
        }
        let mut min = f32::INFINITY;
        let mut max = f32::NEG_INFINITY;
        let mut sum_sq = 0.0f64;
        for (peak, weight) in parts {
            if *weight == 0 {
                continue;
            }
            min = min.min(peak.min);
            max = max.max(peak.max);
            sum_sq += (peak.rms as f64) * (peak.rms as f64) * *weight as f64;
        }
        Peak { min, max, rms: (sum_sq / total as f64).sqrt() as f32 }
    }

    /// Blends two buckets, for columns narrower than the grid.
    pub fn lerp(self, other: Peak, t: f32) -> Peak {
        let t = t.clamp(0.0, 1.0);
        let mix = |a: f32, b: f32| a + (b - a) * t;
        Peak {
            min: mix(self.min, other.min),
            max: mix(self.max, other.max),
            rms: mix(self.rms, other.rms),
        }
    }
}

/// The peaks for one asset, in order from the start of the file.
///
/// Grows as analysis proceeds: a `Waveform` that covers only the first ten
/// seconds of a long file is a valid one, and the interface draws what exists
/// rather than waiting. [`Waveform::envelope`] reports `None` for a column
/// beyond what has been analysed, which is how a partly-drawn clip tells the
/// difference between silence and not-yet-known.
///
/// # Why there is a pyramid
///
/// Drawing a clip zoomed *in* reads a handful of buckets a column, and reducing
/// them costs nothing. Drawing an hour-long clip zoomed all the way out asks a
/// thousand columns to summarise 720,000 buckets, and reading every one of them
/// measured at 3.3 ms — a fifth of a frame, for one clip, on every repaint of a
/// scroll. So each level of coarser buckets is kept alongside the base grid,
/// and a column reads from the coarsest level whose buckets still fit inside
/// it. Cost then follows the columns on screen at every zoom rather than only
/// at close ones, which is the same bargain the timeline's clip culling makes.
///
/// The coarse levels are summaries of the base grid rather than a second
/// analysis of the audio, so they cannot disagree with it: a transient in the
/// base grid is in the envelope of every level above it, which is what lets a
/// zoomed-out waveform be trusted.
#[derive(Debug, Clone)]
pub struct Waveform {
    /// `levels[0]` is the base grid; each level above summarises
    /// [`MIP_FACTOR`] buckets of the one below it.
    levels: Vec<Vec<Peak>>,
    sample_rate: SampleRate,
    channels: u16,
    /// Whether analysis reached the end of the file.
    complete: bool,
}

impl Waveform {
    pub fn new(peaks: Vec<Peak>, sample_rate: SampleRate, channels: u16) -> Self {
        let mut waveform = Waveform::empty(sample_rate, channels);
        waveform.extend(&peaks);
        waveform.complete = true;
        waveform
    }

    pub fn empty(sample_rate: SampleRate, channels: u16) -> Self {
        Waveform {
            levels: vec![Vec::new(); MIP_LEVELS + 1],
            sample_rate,
            channels,
            complete: false,
        }
    }

    /// Appends newly analysed buckets, extending the coarse levels to match.
    ///
    /// A coarse bucket is only produced once every bucket under it exists, so
    /// the top of the pyramid trails the base grid by at most one of its own
    /// buckets. [`Self::merge_range`] reads that tail from the base grid, so
    /// the lag is invisible rather than a hole at the end of a waveform still
    /// being analysed.
    pub fn extend(&mut self, peaks: &[Peak]) {
        self.levels[0].extend_from_slice(peaks);
        for level in 1..self.levels.len() {
            let (below, above) = self.levels.split_at_mut(level);
            let below = &below[level - 1];
            let here = &mut above[0];
            while (here.len() + 1) * MIP_FACTOR <= below.len() {
                let from = here.len() * MIP_FACTOR;
                here.push(Peak::merge(&below[from..from + MIP_FACTOR]));
            }
        }
    }

    pub fn mark_complete(&mut self) {
        self.complete = true;
    }

    pub fn is_complete(&self) -> bool {
        self.complete
    }

    /// The base grid, at [`PEAKS_PER_SECOND`] buckets a second.
    pub fn peaks(&self) -> &[Peak] {
        &self.levels[0]
    }

    pub fn len(&self) -> usize {
        self.levels[0].len()
    }

    pub fn is_empty(&self) -> bool {
        self.levels[0].is_empty()
    }

    pub fn sample_rate(&self) -> SampleRate {
        self.sample_rate
    }

    pub fn channels(&self) -> u16 {
        self.channels
    }

    /// How much of the file the peaks cover so far.
    pub fn analysed_duration(&self) -> Ticks {
        Ticks::new(self.len() as i64 * BUCKET_TICKS)
    }

    /// Everything held, the coarse levels included.
    pub fn byte_size(&self) -> usize {
        self.levels.iter().map(|l| l.len()).sum::<usize>() * std::mem::size_of::<Peak>()
    }

    /// The bucket covering `t`, or `None` if it has not been analysed.
    pub fn peak_at(&self, t: Ticks) -> Option<Peak> {
        if t.is_negative() {
            return None;
        }
        self.levels[0].get((t.raw() / BUCKET_TICKS) as usize).copied()
    }

    /// Reduces `range` to one [`Peak`] per pixel column.
    ///
    /// This is the call the timeline makes for every visible audio clip on
    /// every repaint, so its cost is deliberately bounded by the number of
    /// columns — which is the clip's width on screen — rather than by the
    /// length of the clip or of the file behind it.
    pub fn envelope(&self, range: TimeRange, columns: usize) -> Vec<Option<Peak>> {
        let mut out = Vec::with_capacity(columns);
        if columns == 0 {
            return out;
        }
        let start = range.start.raw() as f64;
        let span = range.duration.raw() as f64;
        let bucket = BUCKET_TICKS as f64;

        for column in 0..columns {
            let from = start + span * (column as f64) / columns as f64;
            let to = start + span * ((column + 1) as f64) / columns as f64;
            out.push(if to - from < bucket {
                // A column narrower than the grid: interpolate between the two
                // nearest bucket centres rather than drawing a step, so a
                // zoomed-in waveform is a smooth curve instead of a staircase.
                let centre = (from + to) * 0.5 / bucket - 0.5;
                let index = centre.floor();
                let t = (centre - index) as f32;
                let index = index as i64;
                match (self.get(index), self.get(index + 1)) {
                    (Some(a), Some(b)) => Some(a.lerp(b, t)),
                    (Some(a), None) => Some(a),
                    (None, Some(b)) => Some(b),
                    (None, None) => None,
                }
            } else {
                let first = (from / bucket).floor() as i64;
                let last = (to / bucket).ceil() as i64;
                self.merge_range(first, last)
            });
        }
        out
    }

    fn get(&self, index: i64) -> Option<Peak> {
        if index < 0 {
            return None;
        }
        self.levels[0].get(index as usize).copied()
    }

    /// The coarsest level whose buckets are comfortably smaller than a column
    /// covering `span` base buckets.
    ///
    /// "Comfortably" is four of them, so the edges of a column can never be off
    /// by more than a quarter of it — and in the case this exists for, a column
    /// covers hundreds, so the real error is a fraction of a pixel.
    fn level_for(&self, span: usize) -> usize {
        let mut level = 0;
        let mut step = MIP_FACTOR;
        while level + 1 < self.levels.len() && span >= step * 4 {
            level += 1;
            step *= MIP_FACTOR;
        }
        level
    }

    /// Merges base buckets `[first, last)`, clamped to what exists.
    ///
    /// Reads whole coarse buckets where it can and the base grid at the two
    /// ends, so the answer covers exactly the range asked for even though the
    /// coarse grid does not line up with it.
    fn merge_range(&self, first: i64, last: i64) -> Option<Peak> {
        let base = &self.levels[0];
        let lo = first.max(0) as usize;
        let hi = (last.max(0) as usize).min(base.len());
        if lo >= hi {
            return None;
        }

        let level = self.level_for(hi - lo);
        if level == 0 {
            return Some(Peak::merge(&base[lo..hi]));
        }

        let step = MIP_FACTOR.pow(level as u32);
        let coarse = &self.levels[level];
        // Whole coarse buckets entirely inside the range, and no further than
        // the pyramid has been built.
        let from = lo.div_ceil(step);
        let to = (hi / step).min(coarse.len());
        if from >= to {
            return Some(Peak::merge(&base[lo..hi]));
        }

        let head = &base[lo..(from * step).min(base.len())];
        let tail = &base[(to * step).min(base.len())..hi];
        Some(Peak::merge_weighted(&[
            (Peak::merge(head), head.len()),
            (Peak::merge(&coarse[from..to]), (to - from) * step),
            (Peak::merge(tail), tail.len()),
        ]))
    }
}

/// Reduces decoded audio to peaks, one bucket at a time.
///
/// Fed sequentially from the start of a file: the bucket a sample belongs to is
/// derived from how many sample frames have been pushed, not from any buffer's
/// timestamp. That is exact for the only way this is used — decoding straight
/// through — and it means a container with sloppy timestamps still produces a
/// waveform of the right length rather than one with holes in it.
pub struct WaveformBuilder {
    sample_rate: SampleRate,
    channels: u16,
    /// Buckets completed but not yet taken by the caller.
    completed: Vec<Peak>,
    /// Buckets taken, so bucket boundaries keep advancing across a drain.
    taken: i64,
    frames: i64,
    open: Accumulator,
}

impl WaveformBuilder {
    pub fn new(sample_rate: SampleRate, channels: u16) -> Self {
        WaveformBuilder {
            sample_rate,
            channels,
            completed: Vec::new(),
            taken: 0,
            frames: 0,
            open: Accumulator::default(),
        }
    }

    /// Reduces one decoded block.
    pub fn push(&mut self, buffer: &AudioBuffer) {
        let channels = buffer.channels().max(1) as usize;
        let samples = buffer.samples();
        let total = buffer.frame_count();
        let mut consumed = 0usize;

        while consumed < total {
            let boundary = self.next_boundary();
            // `max(1)` keeps this making progress even for a nonsensically low
            // sample rate, where a bucket could otherwise be zero frames long.
            let take = ((boundary - self.frames).max(1) as usize).min(total - consumed);
            let from = consumed * channels;
            let to = (consumed + take) * channels;
            self.open.absorb(&samples[from..to]);
            self.frames += take as i64;
            consumed += take;
            if self.frames >= boundary {
                self.completed.push(self.open.close());
            }
        }
    }

    /// The sample-frame index where the bucket under construction ends.
    ///
    /// Derived from the bucket index rather than accumulated, so a rate that
    /// does not divide the grid — 44.1 kHz gives 220.5 frames a bucket —
    /// alternates between 220 and 221 instead of drifting a frame per bucket
    /// until the waveform no longer lines up with the audio.
    fn next_boundary(&self) -> i64 {
        let bucket = self.bucket_count() + 1;
        (bucket * self.sample_rate.hz() as i64) / PEAKS_PER_SECOND
    }

    /// Closes a partially filled bucket, at end of file.
    pub fn flush(&mut self) {
        if !self.open.is_empty() {
            self.completed.push(self.open.close());
        }
    }

    /// How many buckets have been produced in total, taken or not.
    pub fn bucket_count(&self) -> i64 {
        self.taken + self.completed.len() as i64
    }

    /// How many buckets are waiting to be taken.
    pub fn completed_len(&self) -> usize {
        self.completed.len()
    }

    /// Removes and returns the buckets completed since the last call.
    ///
    /// Analysis publishes incrementally, so the peaks it has produced move out
    /// of the builder rather than being copied out of it: a long file is never
    /// held twice.
    pub fn take_completed(&mut self) -> Vec<Peak> {
        let out = std::mem::take(&mut self.completed);
        self.taken += out.len() as i64;
        out
    }

    /// Flushes and returns everything built, for callers that analyse in one go.
    ///
    /// Only meaningful when [`Self::take_completed`] was never called.
    pub fn finish(mut self) -> Waveform {
        self.flush();
        Waveform::new(self.completed, self.sample_rate, self.channels)
    }
}

/// The bucket under construction.
#[derive(Debug)]
struct Accumulator {
    min: f32,
    max: f32,
    sum_sq: f64,
    count: u64,
}

impl Default for Accumulator {
    fn default() -> Self {
        Accumulator { min: f32::INFINITY, max: f32::NEG_INFINITY, sum_sq: 0.0, count: 0 }
    }
}

impl Accumulator {
    fn is_empty(&self) -> bool {
        self.count == 0
    }

    fn absorb(&mut self, samples: &[f32]) {
        for &s in samples {
            // A decoder can emit a NaN from a corrupt packet; letting one in
            // would poison the min and max for the whole bucket and draw a
            // hole in the waveform.
            if s.is_nan() {
                continue;
            }
            self.min = self.min.min(s);
            self.max = self.max.max(s);
            self.sum_sq += (s as f64) * (s as f64);
        }
        self.count += samples.len() as u64;
    }

    fn close(&mut self) -> Peak {
        let peak = if self.count == 0 || !self.min.is_finite() || !self.max.is_finite() {
            Peak::SILENT
        } else {
            Peak {
                min: self.min,
                max: self.max,
                rms: (self.sum_sq / self.count as f64).sqrt() as f32,
            }
        };
        *self = Accumulator::default();
        peak
    }
}

/// Decodes a whole file and reduces it to peaks.
///
/// Blocking, and used directly only by tests and tools; the editor goes through
/// [`crate::WaveformService`], which does this on a worker thread and publishes
/// as it goes.
pub fn analyse(decoder: &mut AudioDecoder) -> Result<Waveform, MediaError> {
    let mut builder = WaveformBuilder::new(decoder.sample_rate(), decoder.channels());
    while let Some(buffer) = decoder.next_buffer()? {
        builder.push(&buffer);
    }
    Ok(builder.finish())
}

/// Opens `path` and reduces it to peaks.
pub fn analyse_file(path: impl AsRef<std::path::Path>) -> Result<Arc<Waveform>, MediaError> {
    let mut decoder = AudioDecoder::open(path, SampleRate::HZ_48000, ANALYSIS_CHANNELS)?;
    Ok(Arc::new(analyse(&mut decoder)?))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn buffer(samples: &[f32], channels: u16) -> AudioBuffer {
        AudioBuffer::new(samples.to_vec().into(), SampleRate::HZ_48000, channels, Ticks::ZERO)
    }

    #[test]
    fn buckets_divide_the_timebase_exactly() {
        assert_eq!(BUCKET_TICKS * PEAKS_PER_SECOND, TICKS_PER_SECOND);
    }

    #[test]
    fn a_second_of_audio_makes_exactly_one_seconds_worth_of_buckets() {
        let mut builder = WaveformBuilder::new(SampleRate::HZ_48000, 1);
        builder.push(&buffer(&vec![0.5; 48_000], 1));
        let waveform = builder.finish();
        assert_eq!(waveform.len() as i64, PEAKS_PER_SECOND);
        assert_eq!(waveform.analysed_duration(), Ticks::from_seconds(1));
    }

    #[test]
    fn a_rate_that_does_not_divide_the_grid_still_lands_on_the_second() {
        // 44100 / 200 is 220.5 frames a bucket, so the boundaries alternate
        // between 220 and 221 frames rather than drifting.
        let mut builder = WaveformBuilder::new(SampleRate::HZ_44100, 1);
        builder.push(&buffer(&vec![0.25; 44_100], 1));
        let waveform = builder.finish();
        assert_eq!(waveform.len() as i64, PEAKS_PER_SECOND);
    }

    #[test]
    fn peaks_record_the_envelope_and_the_level() {
        let mut builder = WaveformBuilder::new(SampleRate::HZ_48000, 1);
        // One bucket of a square wave: every sample is ±0.5, so the envelope is
        // ±0.5 and the RMS is 0.5 too.
        let frames = 48_000 / PEAKS_PER_SECOND as usize;
        let square: Vec<f32> =
            (0..frames).map(|i| if i % 2 == 0 { 0.5 } else { -0.5 }).collect();
        builder.push(&buffer(&square, 1));
        let waveform = builder.finish();

        let p = waveform.peaks()[0];
        assert!((p.max - 0.5).abs() < 1e-6, "max was {}", p.max);
        assert!((p.min + 0.5).abs() < 1e-6, "min was {}", p.min);
        assert!((p.rms - 0.5).abs() < 1e-6, "rms was {}", p.rms);
        assert!((p.amplitude() - 0.5).abs() < 1e-6);
    }

    #[test]
    fn a_transient_survives_being_reduced() {
        // Silence with one loud sample in it. An averaging summary would lose
        // the click entirely; the envelope must keep it at every zoom.
        let frames = 48_000usize;
        let mut samples = vec![0.0f32; frames];
        samples[frames / 2] = 1.0;
        let mut builder = WaveformBuilder::new(SampleRate::HZ_48000, 1);
        builder.push(&buffer(&samples, 1));
        let waveform = builder.finish();

        let whole = waveform.envelope(TimeRange::new(Ticks::ZERO, Ticks::from_seconds(1)), 1);
        assert_eq!(whole.len(), 1);
        assert!((whole[0].unwrap().max - 1.0).abs() < 1e-6);
        // ... and the RMS of that one column is nearly nothing, which is the
        // distinction the two together are for.
        assert!(whole[0].unwrap().rms < 0.01);
    }

    #[test]
    fn channels_are_reduced_together() {
        // Left silent, right loud: the envelope must show the loud channel
        // rather than the average of the two.
        let mut builder = WaveformBuilder::new(SampleRate::HZ_48000, 2);
        let frames = 48_000 / PEAKS_PER_SECOND as usize;
        let mut samples = Vec::new();
        for _ in 0..frames {
            samples.push(0.0);
            samples.push(0.9);
        }
        builder.push(&buffer(&samples, 2));
        let waveform = builder.finish();
        assert!((waveform.peaks()[0].max - 0.9).abs() < 1e-6);
    }

    #[test]
    fn nan_samples_do_not_poison_a_bucket() {
        let mut builder = WaveformBuilder::new(SampleRate::HZ_48000, 1);
        let frames = 48_000 / PEAKS_PER_SECOND as usize;
        let mut samples = vec![0.3f32; frames];
        samples[7] = f32::NAN;
        builder.push(&buffer(&samples, 1));
        let waveform = builder.finish();
        let p = waveform.peaks()[0];
        assert!((p.max - 0.3).abs() < 1e-6, "max was {}", p.max);
        assert!(p.rms.is_finite());
    }

    #[test]
    fn an_unanalysed_column_is_none_rather_than_silence() {
        let mut builder = WaveformBuilder::new(SampleRate::HZ_48000, 1);
        builder.push(&buffer(&vec![0.5; 48_000], 1));
        let waveform = builder.finish();

        // Two seconds asked of a one-second waveform: the second half is not
        // silent, it is unknown, and the display has to be able to tell.
        let columns = waveform.envelope(TimeRange::new(Ticks::ZERO, Ticks::from_seconds(2)), 4);
        assert!(columns[0].is_some());
        assert!(columns[1].is_some());
        assert!(columns[2].is_none());
        assert!(columns[3].is_none());
    }

    #[test]
    fn zooming_in_past_the_grid_interpolates_instead_of_stepping() {
        // A ramp across buckets, sampled far finer than the grid. If columns
        // snapped to buckets, neighbouring columns would repeat; interpolating
        // means they differ.
        let mut peaks = Vec::new();
        for i in 0..10 {
            let v = i as f32 / 10.0;
            peaks.push(Peak { min: -v, max: v, rms: v });
        }
        let waveform = Waveform::new(peaks, SampleRate::HZ_48000, 1);

        let range = TimeRange::new(Ticks::new(BUCKET_TICKS * 2), Ticks::new(BUCKET_TICKS));
        let columns = waveform.envelope(range, 20);
        assert_eq!(columns.len(), 20);
        let first = columns[0].unwrap().max;
        let last = columns[19].unwrap().max;
        assert!(last > first, "expected a rising ramp, got {first} then {last}");
        // Monotonic, and strictly so: no two adjacent columns share a value.
        for pair in columns.windows(2) {
            let (a, b) = (pair[0].unwrap().max, pair[1].unwrap().max);
            assert!(b > a, "columns repeated: {a} then {b}");
        }
    }

    #[test]
    fn zooming_out_keeps_the_loudest_moment() {
        let mut peaks = vec![Peak { min: -0.1, max: 0.1, rms: 0.1 }; 1000];
        peaks[500] = Peak { min: -1.0, max: 1.0, rms: 1.0 };
        let waveform = Waveform::new(peaks, SampleRate::HZ_48000, 1);

        let range = TimeRange::new(Ticks::ZERO, Ticks::new(BUCKET_TICKS * 1000));
        let columns = waveform.envelope(range, 10);
        assert_eq!(columns.len(), 10);
        let loudest = columns.iter().filter_map(|c| *c).fold(0.0f32, |a, p| a.max(p.max));
        assert!((loudest - 1.0).abs() < 1e-6);
    }

    #[test]
    fn taking_completed_peaks_does_not_shift_the_grid() {
        // Analysis drains the builder as it goes. Draining must not reset where
        // the next bucket boundary falls, or a published waveform would be
        // shorter than the audio behind it.
        let mut builder = WaveformBuilder::new(SampleRate::HZ_44100, 1);
        let mut total = 0usize;
        for _ in 0..10 {
            builder.push(&buffer(&vec![0.5; 4_410], 1));
            total += builder.take_completed().len();
        }
        builder.flush();
        total += builder.take_completed().len();
        assert_eq!(total as i64, PEAKS_PER_SECOND);
    }

    /// A waveform whose buckets are a recognisable, uneven signal.
    fn ramped(buckets: usize) -> Waveform {
        let peaks: Vec<Peak> = (0..buckets)
            .map(|i| {
                let v = ((i as f32) * 0.037).sin().abs() * 0.9 + 0.05;
                Peak { min: -v * 0.8, max: v, rms: v * 0.6 }
            })
            .collect();
        Waveform::new(peaks, SampleRate::HZ_48000, 2)
    }

    /// The same reduction, forced to read only the base grid.
    fn base_envelope(waveform: &Waveform, range: TimeRange, columns: usize) -> Vec<Peak> {
        let base = waveform.peaks();
        let start = range.start.raw() as f64;
        let span = range.duration.raw() as f64;
        (0..columns)
            .map(|column| {
                let from = start + span * (column as f64) / columns as f64;
                let to = start + span * ((column + 1) as f64) / columns as f64;
                let lo = ((from / BUCKET_TICKS as f64).floor() as usize).min(base.len());
                let hi = ((to / BUCKET_TICKS as f64).ceil() as usize).min(base.len());
                Peak::merge(&base[lo..hi])
            })
            .collect()
    }

    #[test]
    fn the_coarse_levels_summarise_the_base_grid() {
        let waveform = ramped(8_000);
        // Level 1 buckets are eight base buckets each, level 2 sixty-four, and
        // so on, with the last partial group not yet summarised.
        assert_eq!(waveform.levels[0].len(), 8_000);
        assert_eq!(waveform.levels[1].len(), 1_000);
        assert_eq!(waveform.levels[2].len(), 125);
        assert_eq!(waveform.levels[3].len(), 15);
        assert_eq!(waveform.levels[4].len(), 1);

        // A coarse bucket's envelope is exactly the union of the eight below it.
        for (i, coarse) in waveform.levels[1].iter().enumerate() {
            let group = &waveform.levels[0][i * 8..(i + 1) * 8];
            let expected = Peak::merge(group);
            assert!((coarse.max - expected.max).abs() < 1e-6);
            assert!((coarse.min - expected.min).abs() < 1e-6);
        }
    }

    #[test]
    fn the_pyramid_answers_what_the_base_grid_would() {
        // The whole point: reading a coarse level is an optimisation, not a
        // different answer. Zoomed right out, where the pyramid is actually
        // used, every column must still report the level the base grid says.
        let waveform = ramped(200_000);
        let range = TimeRange::new(Ticks::ZERO, Ticks::new(200_000 * BUCKET_TICKS));
        let fast = waveform.envelope(range, 300);
        let slow = base_envelope(&waveform, range, 300);

        for (column, (fast, slow)) in fast.iter().zip(slow.iter()).enumerate() {
            let fast = fast.expect("every column is within the analysed range");
            assert!(
                (fast.max - slow.max).abs() < 0.02,
                "column {column}: max {} against {}",
                fast.max,
                slow.max
            );
            assert!(
                (fast.rms - slow.rms).abs() < 0.02,
                "column {column}: rms {} against {}",
                fast.rms,
                slow.rms
            );
        }
    }

    #[test]
    fn a_transient_survives_every_level_of_the_pyramid() {
        // One loud bucket in ten minutes of quiet. Whichever level a zoom reads
        // from, the column covering it has to be loud, or a click would be
        // invisible until zoomed in far enough to find it by accident.
        let mut peaks = vec![Peak { min: -0.02, max: 0.02, rms: 0.02 }; 120_000];
        peaks[61_234] = Peak { min: -1.0, max: 1.0, rms: 1.0 };
        let waveform = Waveform::new(peaks, SampleRate::HZ_48000, 2);
        let whole = TimeRange::new(Ticks::ZERO, Ticks::new(120_000 * BUCKET_TICKS));

        for columns in [4usize, 40, 400, 4_000] {
            let envelope = waveform.envelope(whole, columns);
            let loudest = envelope.iter().flatten().fold(0.0f32, |a, p| a.max(p.max));
            assert!(
                (loudest - 1.0).abs() < 1e-6,
                "at {columns} columns the loudest moment read {loudest}"
            );
        }
    }

    #[test]
    fn the_tail_of_a_growing_waveform_is_never_a_hole() {
        // Coarse buckets only appear once every bucket under them exists, so a
        // waveform mid-analysis always has a tail the pyramid has not reached.
        // Reading it must fall back to the base grid rather than report the end
        // of the file as unanalysed.
        let mut waveform = Waveform::empty(SampleRate::HZ_48000, 2);
        waveform.extend(&vec![Peak { min: -0.5, max: 0.5, rms: 0.5 }; 4_100]);
        assert_eq!(waveform.levels[4].len(), 1, "the top level trails, as designed");

        let whole = TimeRange::new(Ticks::ZERO, Ticks::new(4_100 * BUCKET_TICKS));
        let envelope = waveform.envelope(whole, 64);
        assert!(envelope.iter().all(|c| c.is_some()), "a column read as unanalysed");
        for column in envelope.iter().flatten() {
            assert!((column.max - 0.5).abs() < 1e-6, "column read {}", column.max);
        }
    }

    #[test]
    fn extending_gives_the_same_pyramid_as_building_in_one_go() {
        // Analysis publishes in chunks, so the incremental path is the one the
        // editor actually takes; it must not produce a different waveform from
        // the blocking one used by tools and tests.
        let peaks: Vec<Peak> = (0..5_000)
            .map(|i| {
                let v = ((i as f32) * 0.011).cos().abs();
                Peak { min: -v, max: v, rms: v * 0.5 }
            })
            .collect();
        let whole = Waveform::new(peaks.clone(), SampleRate::HZ_48000, 2);

        let mut incremental = Waveform::empty(SampleRate::HZ_48000, 2);
        for chunk in peaks.chunks(37) {
            incremental.extend(chunk);
        }

        assert_eq!(whole.levels.len(), incremental.levels.len());
        for (level, (a, b)) in whole.levels.iter().zip(incremental.levels.iter()).enumerate() {
            assert_eq!(a.len(), b.len(), "level {level} lengths differ");
            assert_eq!(a, b, "level {level} contents differ");
        }
    }

    #[test]
    fn the_pyramid_costs_a_seventh_of_the_base_grid() {
        let waveform = ramped(100_000);
        let base = std::mem::size_of_val(waveform.peaks());
        let total = waveform.byte_size();
        let overhead = (total - base) as f64 / base as f64;
        assert!(overhead < 0.15, "the pyramid added {:.1}%", overhead * 100.0);
    }

    #[test]
    fn merging_rms_is_a_quadratic_mean_not_a_maximum() {
        let peaks =
            [Peak { min: -1.0, max: 1.0, rms: 1.0 }, Peak { min: 0.0, max: 0.0, rms: 0.0 }];
        let merged = Peak::merge(&peaks);
        assert_eq!(merged.max, 1.0);
        assert_eq!(merged.min, -1.0);
        assert!((merged.rms - 0.5f32.sqrt()).abs() < 1e-6, "rms was {}", merged.rms);
    }
}
