//! Finding how far apart two recordings of the same event are.
//!
//! # What is correlated
//!
//! Not the samples. Two cameras twenty feet apart record the same event through
//! different microphones, at different levels, with different room colouration
//! and a propagation delay between them; their *waveforms* do not match at all,
//! and correlating 48,000 samples a second to discover that is expensive as
//! well as wrong.
//!
//! What does match is the **envelope**: when the loud parts happen. So this
//! correlates the RMS series the waveform analysis already produced — 200
//! buckets a second, five milliseconds each — which is plenty to place a clap
//! and cheap enough to search minutes of material.
//!
//! Five milliseconds is not frame-accurate at 24 fps, and this does not pretend
//! otherwise: the result is a measurement with a confidence attached, offered as
//! a starting point that the user can nudge. Sample-accurate alignment needs the
//! samples, and it needs them only over the few hundred milliseconds this has
//! already located.
//!
//! # Coarse to fine
//!
//! A full search of ±10 minutes at 200 buckets a second is 240,000 lags against
//! a series of the same order — a hundred billion multiply-adds, which is not a
//! thing to do while someone waits. So the series are decimated by eight,
//! repeatedly, and the search runs at the coarsest level first: the same ±10
//! minutes is 59 lags there. Each finer level then searches a handful of buckets
//! either side of where the level above put it.
//!
//! The cost of that is linear in the material rather than quadratic, and the
//! risk is the usual one for a coarse-to-fine search: a coarse level that picks
//! the wrong peak cannot be talked out of it by the finer ones. The confidence
//! figure is what makes that visible rather than silent.
//!
//! # Correlation, not difference
//!
//! Scores are **Pearson** correlation over the overlapping region: both series
//! are mean-centred and divided by their own magnitude, so a camera that
//! recorded everything 10 dB quieter than another still matches it exactly. A
//! plain sum of differences would rank the quieter camera as a worse match than
//! silence.

use ve_core::MediaInfo;
use ve_time::Ticks;

use crate::waveform::{Waveform, BUCKET_TICKS};

/// How much of each series has to overlap before a score means anything.
///
/// Two buckets that happen to agree correlate perfectly, and without a floor
/// the best-scoring lag is always the one that slides the series almost
/// entirely apart. One second is short enough to align a clap at the very end
/// of one recording and long enough that agreement is not an accident.
pub const MIN_OVERLAP_SECONDS: f64 = 1.0;

/// Below this, a match is not worth offering.
///
/// Recordings of the same event from different microphones routinely score
/// above 0.8; unrelated material sits near zero. Half is well clear of noise
/// and well below anything real.
pub const MIN_CONFIDENCE: f32 = 0.5;

/// How far apart two angles are assumed to start, unless a caller says
/// otherwise. Ten minutes covers a camera left rolling through a reset.
pub const DEFAULT_MAX_OFFSET: Ticks = Ticks::new(ve_time::TICKS_PER_SECOND * 600);

/// How many buckets one coarse bucket summarises.
const DECIMATION: usize = 8;

/// A level coarser than this has too few buckets for a score to mean anything.
const MIN_LEVEL_BUCKETS: usize = 64;

/// How many scored points a level needs before its answer means anything.
///
/// The time floor alone is not enough on a coarse level: one second of overlap
/// is two hundred points at the base grid but only three where each bucket is a
/// third of a second, and three points correlate at very nearly ±1 whatever
/// they are. A coarse pass is a screening pass and needs statistical weight, so
/// it asks for points rather than for seconds — which on the base grid is no
/// extra demand at all, because the time floor is already far more than this.
const MIN_SCORE_POINTS: usize = 32;

/// How far either side of the coarse answer each finer level looks.
///
/// One coarse bucket is [`DECIMATION`] fine ones, so the true peak is within
/// that of where the level above put it; the extra is slack for a peak that
/// straddled a boundary.
const REFINE_RADIUS: isize = DECIMATION as isize + 4;

/// Where a candidate recording sits relative to a reference.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct SyncMatch {
    /// How much later the candidate started than the reference.
    ///
    /// Positive means the candidate's own zero falls *after* the reference's,
    /// so the candidate was late to the event. Negative means it was rolling
    /// first.
    pub offset: Ticks,
    /// Peak correlation, `0.0..=1.0`. See [`MIN_CONFIDENCE`].
    pub confidence: f32,
}

impl SyncMatch {
    /// The offset to store on a [`ve_core::MulticamAngle`] when the reference
    /// angle is the group's zero.
    ///
    /// The sign flips: an angle's offset says where group zero sits *inside*
    /// that angle's media, and a candidate that started late has group zero
    /// before its own first frame.
    pub fn angle_offset(self) -> Ticks {
        Ticks::ZERO - self.offset
    }

    pub fn is_confident(self) -> bool {
        self.confidence >= MIN_CONFIDENCE
    }
}

/// The instant a file's start timecode names, as a position on a common
/// clock.
///
/// `None` when the file records no timecode, when it has no video stream to
/// give the timecode a frame rate, or when what it recorded will not parse.
/// Each of those is a reason to fall back to another method rather than to
/// guess, and none of them is an error worth stopping an import over.
///
/// Drop-frame timecode is handled by [`ve_time::Timecode`] itself — the `;`
/// separator is what marks it — so a 29.97 shoot syncs against a 23.98 one
/// without either being converted first.
pub fn timecode_start(info: &MediaInfo) -> Option<Ticks> {
    let text = info.timecode.as_deref()?;
    let rate = info.video.as_ref()?.rate;
    ve_time::Timecode::parse(text, rate).ok().map(|tc| tc.to_ticks())
}

/// Measures how far `candidate` sits from `reference`.
///
/// `max_offset` bounds the search in both directions; pass
/// [`DEFAULT_MAX_OFFSET`] for the usual case. `None` when either recording is
/// too short to say anything, or when nothing correlated at all.
///
/// The result is returned whatever it scored — judging it is the caller's job,
/// and a weak match with its confidence attached is more use than a refusal
/// that says nothing about how close it came.
pub fn align(
    reference: &Waveform,
    candidate: &Waveform,
    max_offset: Ticks,
) -> Option<SyncMatch> {
    let a = rms_series(reference);
    let b = rms_series(candidate);
    align_series(&a, &b, max_lag(max_offset))
}

/// The RMS envelope of a waveform's base grid.
fn rms_series(waveform: &Waveform) -> Vec<f32> {
    waveform.peaks().iter().map(|p| p.rms).collect()
}

fn max_lag(max_offset: Ticks) -> usize {
    (max_offset.raw().unsigned_abs() / BUCKET_TICKS as u64) as usize
}

fn min_overlap() -> usize {
    (MIN_OVERLAP_SECONDS * crate::waveform::PEAKS_PER_SECOND as f64) as usize
}

/// The searchable part: two envelopes in, a lag in base buckets out.
///
/// Split from [`align`] so the algorithm can be tested on series built by hand,
/// where the true answer is known exactly rather than inferred from a file.
fn align_series(a: &[f32], b: &[f32], max_lag: usize) -> Option<SyncMatch> {
    let overlap = min_overlap();
    if a.len() < overlap || b.len() < overlap {
        return None;
    }

    // Coarsest first. Each level is half the search of the one below it, so the
    // whole descent costs little more than the finest pass alone.
    let pyramid_a = pyramid(a);
    let pyramid_b = pyramid(b);
    let levels = pyramid_a.len().min(pyramid_b.len());

    let mut best: Option<(isize, f32)> = None;
    for level in (0..levels).rev() {
        let step = DECIMATION.pow(level as u32) as isize;
        let (from, to) = match best {
            // Refine: the level below put it here, scaled up by one decimation.
            Some((lag, _)) => {
                let centre = lag * DECIMATION as isize;
                (centre - REFINE_RADIUS, centre + REFINE_RADIUS)
            }
            // The first, coarsest pass searches the whole permitted range.
            None => {
                let span = (max_lag as isize / step) + 1;
                (-span, span)
            }
        };

        let level_overlap = (overlap / step as usize).max(MIN_SCORE_POINTS);
        // A level that finds nothing leaves the coarser answer standing rather
        // than throwing the whole search away: the refinement had nothing to
        // add, not something to contradict.
        if let Some(found) =
            search(&pyramid_a[level], &pyramid_b[level], from, to, level_overlap)
        {
            best = Some(found);
        } else if let Some((lag, score)) = best {
            best = Some((lag * DECIMATION as isize, score));
        }
    }

    let (lag, confidence) = best?;
    Some(SyncMatch {
        offset: Ticks::new(lag as i64 * BUCKET_TICKS),
        confidence: confidence.clamp(0.0, 1.0),
    })
}

/// Progressively decimated copies of a series, finest first.
fn pyramid(series: &[f32]) -> Vec<Vec<f32>> {
    let mut levels = vec![series.to_vec()];
    while levels.last().is_some_and(|l| l.len() / DECIMATION >= MIN_LEVEL_BUCKETS) {
        let below = levels.last().expect("just checked");
        let coarse: Vec<f32> = below
            .as_chunks::<DECIMATION>()
            .0
            .iter()
            .map(|chunk| chunk.iter().sum::<f32>() / DECIMATION as f32)
            .collect();
        levels.push(coarse);
    }
    levels
}

/// The best-scoring lag in `from..=to`, and what it scored.
fn search(
    a: &[f32],
    b: &[f32],
    from: isize,
    to: isize,
    min_overlap: usize,
) -> Option<(isize, f32)> {
    let mut best: Option<(isize, f32)> = None;
    for lag in from..=to {
        let Some(score) = correlate(a, b, lag, min_overlap) else { continue };
        if best.is_none_or(|(_, b)| score > b) {
            best = Some((lag, score));
        }
    }
    best
}

/// Pearson correlation of `a` against `b` shifted by `lag`, over their overlap.
///
/// `None` when they overlap by less than `min_overlap`, or when either side is
/// flat there — a constant series correlates with nothing, and dividing by its
/// zero magnitude would produce a NaN that then wins every comparison it is in.
fn correlate(a: &[f32], b: &[f32], lag: isize, min_overlap: usize) -> Option<f32> {
    // Pairs are a[i] against b[i - lag].
    let start = lag.max(0);
    let end = (a.len() as isize).min(b.len() as isize + lag);
    if end <= start {
        return None;
    }
    let count = (end - start) as usize;
    if count < min_overlap {
        return None;
    }

    let (mut sum_a, mut sum_b) = (0.0f64, 0.0f64);
    for i in start..end {
        sum_a += a[i as usize] as f64;
        sum_b += b[(i - lag) as usize] as f64;
    }
    let (mean_a, mean_b) = (sum_a / count as f64, sum_b / count as f64);

    let (mut num, mut var_a, mut var_b) = (0.0f64, 0.0f64, 0.0f64);
    for i in start..end {
        let da = a[i as usize] as f64 - mean_a;
        let db = b[(i - lag) as usize] as f64 - mean_b;
        num += da * db;
        var_a += da * da;
        var_b += db * db;
    }

    let den = (var_a * var_b).sqrt();
    (den > 0.0).then(|| (num / den) as f32)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::waveform::{Peak, PEAKS_PER_SECOND};

    /// An envelope with the shape a real recording has at this resolution: a
    /// quiet floor with transients scattered irregularly over it.
    ///
    /// Irregularly is the important word. An envelope with a *periodic*
    /// structure correlates almost as well at every multiple of its period, so
    /// a fixture built that way would test how the search behaves on material
    /// that does not occur rather than on material that does — and would report
    /// a confident wrong answer for the fixture's reasons, not the code's.
    fn envelope(buckets: usize, seed: u32) -> Vec<f32> {
        let mut state = seed.wrapping_mul(2654435761).wrapping_add(12345);
        let mut next = move || {
            state = state.wrapping_mul(1664525).wrapping_add(1013904223);
            (state >> 8) as f32 / (1 << 24) as f32
        };

        let mut series = vec![0.0f32; buckets];
        for (i, value) in series.iter_mut().enumerate() {
            // A floor that drifts, so the series is not flat between events.
            *value = 0.05 + 0.03 * next() + 0.02 * (i as f32 * 0.0013).sin();
        }
        // Transients at random positions, each decaying over a few buckets —
        // a hit, a word, a door. These are what actually carries the alignment.
        let mut at = 0usize;
        while at < buckets {
            at += 1 + (next() * 120.0) as usize;
            if at >= buckets {
                break;
            }
            let level = 0.3 + 0.6 * next();
            let decay = 3 + (next() * 20.0) as usize;
            for k in 0..decay {
                let Some(slot) = series.get_mut(at + k) else { break };
                let fade = 1.0 - k as f32 / decay as f32;
                *slot = (*slot + level * fade * fade).min(1.0);
            }
        }
        series
    }

    fn waveform(series: &[f32]) -> Waveform {
        let peaks: Vec<Peak> =
            series.iter().map(|v| Peak { min: -*v, max: *v, rms: *v }).collect();
        Waveform::new(peaks, ve_time::SampleRate::HZ_48000, 2)
    }

    /// Shifts a series, as a second camera starting `lag` buckets later would
    /// have recorded it: the candidate's own zero is that much further into the
    /// event, so the beginning of the reference is simply not in it.
    fn started_later(series: &[f32], lag: usize) -> Vec<f32> {
        series[lag..].to_vec()
    }

    fn secs(buckets: usize) -> f64 {
        buckets as f64 / PEAKS_PER_SECOND as f64
    }

    #[test]
    fn a_camera_that_started_late_is_measured_as_late() {
        let reference = envelope(60 * PEAKS_PER_SECOND as usize, 1);
        let lag = 7 * PEAKS_PER_SECOND as usize; // seven seconds
        let candidate = started_later(&reference, lag);

        let found = align_series(&reference, &candidate, max_lag(DEFAULT_MAX_OFFSET))
            .expect("a shifted copy must correlate");
        assert!((found.offset.as_secs_f64() - secs(lag)).abs() < 0.01, "{found:?}");
        assert!(found.confidence > 0.99, "an exact copy should score near 1: {found:?}");
        assert!(found.is_confident());
    }

    #[test]
    fn a_camera_that_was_rolling_first_is_measured_as_early() {
        let full = envelope(60 * PEAKS_PER_SECOND as usize, 2);
        let lag = 4 * PEAKS_PER_SECOND as usize;
        // The reference is the *late* one this time.
        let reference = started_later(&full, lag);

        let found = align_series(&reference, &full, max_lag(DEFAULT_MAX_OFFSET))
            .expect("a shifted copy must correlate");
        assert!(
            (found.offset.as_secs_f64() + secs(lag)).abs() < 0.01,
            "expected about -{}s, got {found:?}",
            secs(lag)
        );
    }

    /// The property the Pearson normalisation exists for.
    #[test]
    fn a_quieter_camera_matches_just_as_well() {
        let reference = envelope(60 * PEAKS_PER_SECOND as usize, 3);
        let lag = 3 * PEAKS_PER_SECOND as usize;
        let quiet: Vec<f32> = started_later(&reference, lag).iter().map(|v| v * 0.05).collect();

        let found =
            align_series(&reference, &quiet, max_lag(DEFAULT_MAX_OFFSET)).expect("a match");
        assert!((found.offset.as_secs_f64() - secs(lag)).abs() < 0.01, "{found:?}");
        assert!(found.confidence > 0.99, "level must not affect the score: {found:?}");
    }

    #[test]
    fn two_unrelated_recordings_do_not_pretend_to_match() {
        let a = envelope(60 * PEAKS_PER_SECOND as usize, 10);
        let b = envelope(60 * PEAKS_PER_SECOND as usize, 99);
        let found = align_series(&a, &b, max_lag(DEFAULT_MAX_OFFSET));
        // Unrelated material still has a best lag — there is always one — but
        // it must not clear the bar.
        if let Some(found) = found {
            assert!(
                !found.is_confident(),
                "unrelated envelopes scored {}: {found:?}",
                found.confidence
            );
        }
    }

    #[test]
    fn a_recording_too_short_to_say_anything_says_nothing() {
        let long = envelope(60 * PEAKS_PER_SECOND as usize, 4);
        let short = vec![0.5f32; 10];
        assert_eq!(align_series(&long, &short, 1000), None);
        assert_eq!(align_series(&short, &long, 1000), None);
        assert_eq!(align_series(&[], &[], 1000), None);
    }

    /// Silence has no features, so there is nothing to align to. The answer has
    /// to be "I cannot tell", not a lag picked out of a division by zero.
    #[test]
    fn silence_correlates_with_nothing() {
        let quiet = vec![0.0f32; 60 * PEAKS_PER_SECOND as usize];
        let real = envelope(60 * PEAKS_PER_SECOND as usize, 5);
        assert_eq!(align_series(&quiet, &real, 1000), None);
        assert_eq!(align_series(&real, &quiet, 1000), None);
    }

    #[test]
    fn a_match_beyond_the_search_range_is_not_found() {
        let reference = envelope(120 * PEAKS_PER_SECOND as usize, 6);
        let lag = 30 * PEAKS_PER_SECOND as usize;
        let candidate = started_later(&reference, lag);

        // Told to look no further than five seconds, it must not report thirty.
        let found = align_series(&reference, &candidate, max_lag(Ticks::from_seconds(5)));
        if let Some(found) = found {
            assert!(found.offset.as_secs_f64() < 29.0, "{found:?}");
            assert!(!found.is_confident(), "a wrong answer must not look confident");
        }
    }

    fn info_with(timecode: Option<&str>, rate: ve_time::Rate) -> MediaInfo {
        MediaInfo {
            duration: Ticks::from_seconds(60),
            video: Some(ve_core::VideoStreamInfo {
                size: ve_core::Size::new(1920, 1080),
                rate,
                duration: Ticks::from_seconds(60),
                frame_count: None,
                codec: "h264".into(),
                pixel_format: "yuv420p".into(),
                sample_aspect_ratio: (1, 1),
            }),
            audio: None,
            container: "mov".into(),
            timecode: timecode.map(str::to_string),
        }
    }

    #[test]
    fn a_start_timecode_reads_as_a_position_on_a_common_clock() {
        let info = info_with(Some("01:00:30:00"), ve_time::Rate::FPS_25);
        assert_eq!(timecode_start(&info), Some(Ticks::from_seconds(3630)));
    }

    /// Two cameras on the same timecode clock at different frame rates still
    /// name the same instant, which is the whole reason to sync on it.
    #[test]
    fn two_rates_on_one_clock_name_the_same_instant() {
        let pal = timecode_start(&info_with(Some("10:00:00:00"), ve_time::Rate::FPS_25));
        let film = timecode_start(&info_with(Some("10:00:00:00"), ve_time::Rate::FPS_24));
        assert_eq!(pal, film);
        assert_eq!(pal, Some(Ticks::from_seconds(36_000)));
    }

    #[test]
    fn a_file_with_nothing_to_sync_on_says_so() {
        assert_eq!(timecode_start(&info_with(None, ve_time::Rate::FPS_25)), None);
        assert_eq!(
            timecode_start(&info_with(Some("not a timecode"), ve_time::Rate::FPS_25)),
            None
        );

        // Audio only: a timecode without a frame rate cannot be placed.
        let mut audio_only = info_with(Some("01:00:00:00"), ve_time::Rate::FPS_25);
        audio_only.video = None;
        assert_eq!(timecode_start(&audio_only), None);
    }

    #[test]
    fn an_angle_offset_is_the_other_sign() {
        let late = SyncMatch { offset: Ticks::from_seconds(8), confidence: 1.0 };
        assert_eq!(late.angle_offset(), Ticks::from_seconds(-8));
        let early = SyncMatch { offset: Ticks::from_seconds(-3), confidence: 1.0 };
        assert_eq!(early.angle_offset(), Ticks::from_seconds(3));
    }

    #[test]
    fn aligning_goes_through_the_waveforms_the_editor_already_has() {
        let reference = envelope(60 * PEAKS_PER_SECOND as usize, 7);
        let lag = 2 * PEAKS_PER_SECOND as usize;
        let candidate = started_later(&reference, lag);

        let found = align(&waveform(&reference), &waveform(&candidate), DEFAULT_MAX_OFFSET)
            .expect("a match");
        assert!((found.offset.as_secs_f64() - secs(lag)).abs() < 0.01, "{found:?}");
    }

    #[test]
    fn the_pyramid_stops_before_a_level_is_too_small_to_score() {
        let levels = pyramid(&vec![0.5f32; 10_000]);
        assert!(levels.len() > 1, "a long series must be decimated at all");
        assert!(
            levels.last().is_some_and(|l| l.len() >= MIN_LEVEL_BUCKETS),
            "the coarsest level must still be scorable"
        );
        // Each level is a factor of eight smaller than the one below it.
        for pair in levels.windows(2) {
            assert_eq!(pair[1].len(), pair[0].len() / DECIMATION);
        }
    }

    #[test]
    fn a_short_series_is_not_decimated_at_all() {
        let levels = pyramid(&vec![0.5f32; 100]);
        assert_eq!(levels.len(), 1);
    }

    #[test]
    fn an_overlap_too_small_to_mean_anything_is_refused() {
        let a = vec![1.0f32, 2.0, 3.0, 4.0];
        let b = vec![1.0f32, 2.0, 3.0, 4.0];
        // Two overlapping buckets, asked for three.
        assert_eq!(correlate(&a, &b, 2, 3), None);
        assert!(correlate(&a, &b, 0, 3).is_some());
        // And a lag that slides them completely apart has no overlap at all.
        assert_eq!(correlate(&a, &b, 99, 1), None);
    }

    #[test]
    fn correlation_is_one_for_a_copy_and_minus_one_for_an_inversion() {
        let a = vec![0.0f32, 1.0, 0.0, 1.0, 0.0, 1.0, 0.0, 1.0];
        let inverted: Vec<f32> = a.iter().map(|v| 1.0 - v).collect();
        assert!((correlate(&a, &a, 0, 1).unwrap() - 1.0).abs() < 1e-6);
        assert!((correlate(&a, &inverted, 0, 1).unwrap() + 1.0).abs() < 1e-6);
    }
}
