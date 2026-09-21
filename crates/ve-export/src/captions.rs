//! Writing a delivery's captions beside it.
//!
//! # Why beside it rather than inside it
//!
//! A sidecar file is what a caption track becomes on the way out: `film.mp4`
//! and `film.en.srt`, in the same folder. That is what a web player's `<track>`
//! element points at, what a broadcaster's delivery specification asks for, and
//! what anyone can open and correct a typo in without a video editor.
//!
//! The alternative — a subtitle stream muxed into the container — is a real
//! thing and is not here. It would mean encoding the text through `mov_text`
//! for MP4 and something else for Matroska, interleaving subtitle packets with
//! the picture and the sound, and then having no way to show the result to
//! anyone who asked "is the caption right?" without another player. A sidecar
//! is a text file next to the video. Burning captions **into** the picture is
//! the other thing that is missing, and it waits on the text rendering the
//! motion-graphics phase brings: there is no glyph rasteriser in the compositor
//! to draw one with.
//!
//! # What the file says
//!
//! Timings count from the start of the exported range, not from the start of
//! the sequence. A delivery of ten minutes from the middle of a cut is its own
//! file that starts at zero, and a sidecar that still counted from the
//! sequence's origin would be out by however far in the range began — silently,
//! and only noticed by whoever played the delivery.

use std::collections::HashSet;
use std::path::{Path, PathBuf};

use ve_caption::CaptionFormat;
use ve_core::Sequence;
use ve_time::TimeRange;

/// Writes one file per caption track, and says what it wrote.
///
/// Problems are **returned rather than raised**: this runs after the video file
/// has been finished, and a caption file that could not be written is not a
/// reason to throw away a delivery that rendered correctly. The caller reports
/// them alongside the export's other problems.
pub fn write_sidecars(
    sequence: &Sequence,
    output: &Path,
    range: TimeRange,
    format: CaptionFormat,
) -> (Vec<PathBuf>, Vec<String>) {
    let mut written = Vec::new();
    let mut problems = Vec::new();
    let mut used: HashSet<PathBuf> = HashSet::new();

    for track in &sequence.captions {
        let cues = ve_caption::within(track.cues(), range);
        // A track with nothing in the exported range writes no file, rather
        // than an empty one nobody can tell from a mistake.
        if cues.iter().all(|c| c.is_blank()) {
            continue;
        }
        let path = sidecar_path(output, &track.language, &track.name, format, &used);
        used.insert(path.clone());
        match ve_caption::write(&path, &cues) {
            Ok(()) => written.push(path),
            Err(e) => problems.push(format!("captions: {e}")),
        }
    }
    (written, problems)
}

/// `film.mp4` and `en` become `film.en.srt`.
///
/// The language in the name is the convention every player and every delivery
/// folder relies on to tell two caption files apart. Two tracks tagged the same
/// language would collide, so the second one also carries its track name —
/// overwriting the first would lose a language's worth of work to a naming
/// rule.
fn sidecar_path(
    output: &Path,
    language: &str,
    name: &str,
    format: CaptionFormat,
    used: &HashSet<PathBuf>,
) -> PathBuf {
    let stem = output.file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default();
    let directory = output.parent().unwrap_or(Path::new(""));
    let language = sanitise(language);
    let extension = format.extension();

    let candidate = directory.join(format!("{stem}.{language}.{extension}"));
    if !used.contains(&candidate) {
        return candidate;
    }
    let suffixed = directory.join(format!("{stem}.{language}.{}.{extension}", sanitise(name)));
    if !used.contains(&suffixed) {
        return suffixed;
    }
    // Three tracks in one language is beyond what a name can disambiguate
    // politely; a number is still better than a file that overwrites another.
    for n in 2.. {
        let numbered =
            directory.join(format!("{stem}.{language}.{}-{n}.{extension}", sanitise(name)));
        if !used.contains(&numbered) {
            return numbered;
        }
    }
    unreachable!("the loop above returns")
}

/// Keeps a language tag or a track name to what is safe in a file name.
///
/// A track's name is typed by the user and a language tag can be hand-edited,
/// so neither may put a path separator into a file name an export is about to
/// write.
fn sanitise(text: &str) -> String {
    let cleaned: String = text
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() || c == '-' || c == '_' { c } else { '-' })
        .collect();
    let trimmed = cleaned.trim_matches('-');
    if trimmed.is_empty() {
        "und".to_string() // What a container calls a language it does not know.
    } else {
        trimmed.to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ve_core::{CaptionTrack, CaptionTrackId, Cue, CueId, SequenceId};
    use ve_time::Ticks;

    fn sequence_with(tracks: Vec<CaptionTrack>) -> Sequence {
        let mut sequence = Sequence::new(SequenceId::from_raw(1), "seq", Default::default());
        sequence.captions = tracks;
        sequence
    }

    fn track(id: u64, language: &str, name: &str, cues: &[(i64, i64, &str)]) -> CaptionTrack {
        let mut track = CaptionTrack::new(CaptionTrackId::from_raw(id), name)
            .with_language(language.to_string());
        for (i, (start, length, text)) in cues.iter().enumerate() {
            track
                .insert_cue(Cue::new(
                    CueId::from_raw(id * 100 + i as u64),
                    Ticks::from_millis(*start),
                    Ticks::from_millis(*length),
                    *text,
                ))
                .unwrap();
        }
        track
    }

    #[test]
    fn one_file_per_track_named_for_its_language() {
        let dir = tempfile::tempdir().unwrap();
        let output = dir.path().join("film.mp4");
        let sequence = sequence_with(vec![
            track(1, "en", "C1", &[(0, 2_000, "Hello there.")]),
            track(2, "pt-BR", "C2", &[(0, 2_000, "Olá.")]),
        ]);

        let (written, problems) = write_sidecars(
            &sequence,
            &output,
            TimeRange::new(Ticks::ZERO, Ticks::from_seconds(10)),
            CaptionFormat::SubRip,
        );
        assert!(problems.is_empty(), "{problems:?}");
        assert_eq!(written.len(), 2);
        assert_eq!(written[0], dir.path().join("film.en.srt"));
        assert_eq!(written[1], dir.path().join("film.pt-BR.srt"));
        assert!(std::fs::read_to_string(&written[1]).unwrap().contains("Olá."));
    }

    #[test]
    fn timings_count_from_the_start_of_the_exported_range() {
        let dir = tempfile::tempdir().unwrap();
        let output = dir.path().join("excerpt.mp4");
        let sequence = sequence_with(vec![track(
            1,
            "en",
            "C1",
            &[(0, 1_000, "before"), (12_000, 2_000, "inside")],
        )]);

        let range = TimeRange::from_bounds(Ticks::from_seconds(10), Ticks::from_seconds(20));
        let (written, problems) =
            write_sidecars(&sequence, &output, range, CaptionFormat::WebVtt);
        assert!(problems.is_empty(), "{problems:?}");

        let text = std::fs::read_to_string(&written[0]).unwrap();
        assert!(text.contains("00:00:02.000 --> 00:00:04.000"), "{text}");
        assert!(!text.contains("before"), "{text}");
    }

    #[test]
    fn a_track_with_nothing_in_the_range_writes_no_file_at_all() {
        let dir = tempfile::tempdir().unwrap();
        let output = dir.path().join("film.mp4");
        let sequence = sequence_with(vec![track(1, "en", "C1", &[(60_000, 2_000, "later")])]);

        let (written, problems) = write_sidecars(
            &sequence,
            &output,
            TimeRange::new(Ticks::ZERO, Ticks::from_seconds(10)),
            CaptionFormat::SubRip,
        );
        assert!(written.is_empty());
        assert!(problems.is_empty());
        assert!(!dir.path().join("film.en.srt").exists());
    }

    #[test]
    fn two_tracks_in_one_language_do_not_overwrite_each_other() {
        let dir = tempfile::tempdir().unwrap();
        let output = dir.path().join("film.mp4");
        let sequence = sequence_with(vec![
            track(1, "en", "C1", &[(0, 2_000, "plain")]),
            track(2, "en", "SDH", &[(0, 2_000, "[door closes]")]),
        ]);

        let (written, _) = write_sidecars(
            &sequence,
            &output,
            TimeRange::new(Ticks::ZERO, Ticks::from_seconds(10)),
            CaptionFormat::SubRip,
        );
        assert_eq!(written.len(), 2);
        assert_ne!(written[0], written[1]);
        assert_eq!(written[1], dir.path().join("film.en.SDH.srt"));
    }

    #[test]
    fn a_language_tag_cannot_write_outside_the_export_folder() {
        let dir = tempfile::tempdir().unwrap();
        let output = dir.path().join("film.mp4");
        // A hand-edited project file is the only way to get here, and it must
        // not be a way to choose where an export writes.
        let sequence = sequence_with(vec![track(1, "../../etc/en", "C1", &[(0, 2_000, "hi")])]);

        let (written, problems) = write_sidecars(
            &sequence,
            &output,
            TimeRange::new(Ticks::ZERO, Ticks::from_seconds(10)),
            CaptionFormat::SubRip,
        );
        assert!(problems.is_empty(), "{problems:?}");
        assert_eq!(written[0].parent(), Some(dir.path()));
    }
}
