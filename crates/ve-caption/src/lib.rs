//! Caption interchange: SubRip (`.srt`) and WebVTT (`.vtt`).
//!
//! Captions are the one part of an edit that people routinely author somewhere
//! else — a transcription service, a captioner working to a broadcaster's
//! style guide, a colleague with a text editor — so reading and writing these
//! two files is not a convenience on the side of the feature. It *is* how
//! captions get into an edit and how they leave it.
//!
//! This crate is the only place that knows what those files look like.
//! [`ve_core`] holds the cues, `ve-command` edits them, `ve-export` writes them
//! out beside a delivery, and none of them contains a timestamp parser.
//!
//! # What is not carried
//!
//! Position, alignment, colour and styling. Both formats can express some of
//! it — WebVTT considerably more than SubRip — and Verge has nowhere to put it:
//! a cue is text and a span. Reading a file that has them says so once rather
//! than pretending, and writing one never invents them. Inline markup
//! (`<i>…</i>`) is the exception: it is kept in the text verbatim, because it
//! is part of what the file said and throwing it away could not be undone on
//! the way back out.

use std::path::{Path, PathBuf};

use ve_core::{Cue, CueId};
use ve_time::{Ticks, TimeRange};

mod parse;
mod timestamp;
mod write;

pub use timestamp::to_millis;

/// Which of the two files this is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CaptionFormat {
    /// SubRip. Numbered cues, comma separator; what almost everything reads.
    SubRip,
    /// WebVTT. What the web plays, and what an HTML `<track>` points at.
    WebVtt,
}

impl CaptionFormat {
    pub fn extension(self) -> &'static str {
        match self {
            CaptionFormat::SubRip => "srt",
            CaptionFormat::WebVtt => "vtt",
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            CaptionFormat::SubRip => "SubRip (.srt)",
            CaptionFormat::WebVtt => "WebVTT (.vtt)",
        }
    }

    pub fn from_extension(ext: &str) -> Option<CaptionFormat> {
        match ext.to_ascii_lowercase().as_str() {
            "srt" => Some(CaptionFormat::SubRip),
            "vtt" | "webvtt" => Some(CaptionFormat::WebVtt),
            _ => None,
        }
    }

    pub fn from_path(path: &Path) -> Option<CaptionFormat> {
        CaptionFormat::from_extension(path.extension()?.to_str()?)
    }

    pub const ALL: [CaptionFormat; 2] = [CaptionFormat::SubRip, CaptionFormat::WebVtt];
}

/// A cue as a file states it: a span and text, with no ID yet.
///
/// IDs come from the project's allocator, which this crate has no business
/// touching — so a parse produces spans and text, and the command that puts
/// them on a track is what numbers them.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParsedCue {
    pub span: TimeRange,
    pub text: String,
}

/// What reading a caption file produced.
///
/// Warnings rather than failure for anything short of "this is not a caption
/// file": a malformed cue in the middle of three hundred good ones should cost
/// that cue and a line in the status bar, not the import.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct CaptionFile {
    pub cues: Vec<ParsedCue>,
    pub warnings: Vec<String>,
}

impl CaptionFile {
    /// Turns parsed cues into model cues, numbering them from `next_id`.
    ///
    /// The result is sorted. Overlaps are left for
    /// [`ve_core::CaptionTrack::normalise`] to repair, since it is the track
    /// that knows what "overlapping" means and a file is outside data either
    /// way.
    pub fn into_cues(self, mut next_id: impl FnMut() -> CueId) -> Vec<Cue> {
        let mut cues: Vec<Cue> = self
            .cues
            .into_iter()
            .map(|c| Cue::new(next_id(), c.span.start, c.span.duration, c.text))
            .collect();
        cues.sort_by_key(|c| c.start);
        cues
    }
}

#[derive(Debug, thiserror::Error)]
pub enum CaptionError {
    #[error("{path} is not a caption file Verge reads — use .srt or .vtt")]
    UnknownFormat { path: PathBuf },
    #[error("reading {path}: {source}")]
    Read { path: PathBuf, source: std::io::Error },
    #[error("writing {path}: {source}")]
    Write { path: PathBuf, source: std::io::Error },
    #[error("no captions found in {path}")]
    Empty { path: PathBuf },
}

/// Reads a caption file, choosing the format by extension.
///
/// **Encoding.** Caption files are older than the assumption that text is
/// UTF-8, and a `.srt` from a broadcast workflow is as likely to be Latin-1.
/// One that is not valid UTF-8 is therefore decoded as Latin-1 rather than
/// refused — every byte maps to a character, so the file opens and the accents
/// are whatever the file meant them to be, with a warning saying so.
pub fn read(path: &Path) -> Result<CaptionFile, CaptionError> {
    // The extension has to be one of the two — a .txt dropped on the import
    // button is a mistake worth catching — but nothing below branches on which,
    // because the reader accepts either spelling whatever the name claims.
    CaptionFormat::from_path(path)
        .ok_or_else(|| CaptionError::UnknownFormat { path: path.into() })?;

    let bytes = std::fs::read(path)
        .map_err(|source| CaptionError::Read { path: path.into(), source })?;

    let (text, encoding_warning) = match String::from_utf8(bytes) {
        Ok(text) => (text, None),
        Err(e) => {
            let latin1: String = e.into_bytes().into_iter().map(|b| b as char).collect();
            (latin1, Some("the file is not UTF-8; it was read as Latin-1".to_string()))
        }
    };

    let mut file = parse::parse(&text);
    if let Some(warning) = encoding_warning {
        file.warnings.insert(0, warning);
    }
    if file.cues.is_empty() {
        return Err(CaptionError::Empty { path: path.into() });
    }
    Ok(file)
}

/// Parses caption text that is already in hand.
pub fn parse(text: &str) -> CaptionFile {
    parse::parse(text)
}

/// Renders cues in `format`.
pub fn to_string(cues: &[Cue], format: CaptionFormat) -> String {
    write::to_string(cues, format)
}

/// Writes cues to a file, choosing the format by extension.
pub fn write(path: &Path, cues: &[Cue]) -> Result<(), CaptionError> {
    let format = CaptionFormat::from_path(path)
        .ok_or_else(|| CaptionError::UnknownFormat { path: path.into() })?;
    if let Some(parent) = path.parent() {
        if !parent.as_os_str().is_empty() && !parent.exists() {
            std::fs::create_dir_all(parent)
                .map_err(|source| CaptionError::Write { path: parent.into(), source })?;
        }
    }
    std::fs::write(path, write::to_string(cues, format))
        .map_err(|source| CaptionError::Write { path: path.into(), source })
}

/// The cues covering `range`, moved so that `range.start` becomes zero.
///
/// What an export needs: a delivery of the middle of a sequence is its own
/// file starting at zero, and a sidecar whose timings still counted from the
/// sequence's origin would be out by however far in the range began.
///
/// A cue straddling an edge is **clipped rather than dropped**. Half a line of
/// dialogue is spoken inside the range; showing it for as long as the range
/// lasts is what a player does with the same file, and losing it entirely is
/// the one outcome nobody would ask for.
pub fn within(cues: &[Cue], range: TimeRange) -> Vec<Cue> {
    cues.iter()
        .filter_map(|cue| {
            let overlap = cue.range().intersection(range)?;
            let mut shifted = cue.clone();
            shifted.start = overlap.start - range.start;
            shifted.duration = overlap.duration;
            Some(shifted)
        })
        .collect()
}

/// The language tag a caption file's name carries, as in `movie.pt-BR.srt`.
///
/// A convention rather than a standard, but a widely followed one: it is how a
/// web player finds the Portuguese subtitles next to the video, and how a
/// folder of deliverables says which file is which. Reading it on import means
/// a track arrives tagged instead of everything being `en` until someone
/// notices.
pub fn language_from_path(path: &Path) -> Option<String> {
    let stem = path.file_stem()?.to_str()?;
    let tag = stem.rsplit_once('.')?.1;
    is_language_tag(tag).then(|| tag.to_string())
}

/// Whether `tag` looks like BCP 47 — `en`, `pt-BR`, `zh-Hant`.
///
/// Shape only. Checking a tag against the registry would mean carrying the
/// registry, and the cost of being wrong here is a track labelled `v2` rather
/// than a corrupted file.
fn is_language_tag(tag: &str) -> bool {
    let mut parts = tag.split('-');
    let Some(primary) = parts.next() else { return false };
    if !(2..=3).contains(&primary.len()) || !primary.chars().all(|c| c.is_ascii_alphabetic()) {
        return false;
    }
    parts.all(|p| (1..=8).contains(&p.len()) && p.chars().all(|c| c.is_ascii_alphanumeric()))
}

/// The text of a cue with inline markup taken out, for drawing.
///
/// The model keeps `<i>whispering</i>` exactly as the file wrote it, because
/// that is what a round trip has to give back. Nothing in Verge can *draw*
/// italics yet, and drawing the angle brackets literally over the picture would
/// be worse than either — so the preview overlay strips them on the way to the
/// screen and nowhere else.
pub fn strip_markup(text: &str) -> String {
    if !text.contains('<') {
        return text.to_string();
    }
    let mut out = String::with_capacity(text.len());
    // What has been read since an unmatched `<`. A tag that is never closed is
    // not a tag — it is someone typing "a < b" — so it is put back rather than
    // swallowing the rest of the line.
    let mut pending: Option<String> = None;
    for c in text.chars() {
        match (&mut pending, c) {
            (None, '<') => pending = Some(String::from('<')),
            (Some(_), '>') => pending = None,
            (Some(buffer), _) => buffer.push(c),
            (None, _) => out.push(c),
        }
    }
    if let Some(buffer) = pending {
        out.push_str(&buffer);
    }
    out
}

/// A caption's default length when one is made from nothing.
///
/// Two seconds is what a reader needs for a line of dialogue, and it is what
/// every captioning guide starts from. Anything shorter has to be read faster
/// than it can be.
pub const DEFAULT_CUE_DURATION: Ticks = Ticks::new(ve_time::TICKS_PER_SECOND * 2);

#[cfg(test)]
mod tests {
    use super::*;
    use ve_core::CueId;

    fn cue(raw: u64, start_ms: i64, end_ms: i64, text: &str) -> Cue {
        Cue::new(
            CueId::from_raw(raw),
            Ticks::from_millis(start_ms),
            Ticks::from_millis(end_ms - start_ms),
            text,
        )
    }

    #[test]
    fn a_format_is_chosen_by_extension_whatever_its_case() {
        assert_eq!(CaptionFormat::from_path(Path::new("a.SRT")), Some(CaptionFormat::SubRip));
        assert_eq!(CaptionFormat::from_path(Path::new("a.vtt")), Some(CaptionFormat::WebVtt));
        assert_eq!(CaptionFormat::from_path(Path::new("a.txt")), None);
    }

    #[test]
    fn a_language_tag_is_read_off_the_file_name_when_there_is_one() {
        assert_eq!(language_from_path(Path::new("movie.pt-BR.srt")).as_deref(), Some("pt-BR"));
        assert_eq!(language_from_path(Path::new("movie.en.vtt")).as_deref(), Some("en"));
        assert_eq!(language_from_path(Path::new("movie.srt")), None);
        // A version suffix is not a language, and looks nothing like one.
        assert_eq!(language_from_path(Path::new("movie.v2.srt")), None);
        assert_eq!(language_from_path(Path::new("movie.final-cut.srt")), None);
    }

    #[test]
    fn an_export_range_moves_the_cues_to_start_at_zero() {
        let cues = [
            cue(1, 0, 1000, "before"),
            cue(2, 5000, 6000, "inside"),
            cue(3, 20_000, 21_000, "after"),
        ];
        let range = TimeRange::from_bounds(Ticks::from_seconds(4), Ticks::from_seconds(10));
        let out = within(&cues, range);
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].text, "inside");
        assert_eq!(out[0].start, Ticks::from_seconds(1));
    }

    #[test]
    fn a_cue_straddling_the_edge_is_clipped_rather_than_lost() {
        let cues = [cue(1, 3000, 6000, "straddling the in point")];
        let range = TimeRange::from_bounds(Ticks::from_seconds(4), Ticks::from_seconds(10));
        let out = within(&cues, range);
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].start, Ticks::ZERO);
        assert_eq!(out[0].duration, Ticks::from_seconds(2));
    }

    #[test]
    fn markup_is_stripped_for_drawing_and_left_alone_everywhere_else() {
        assert_eq!(strip_markup("<i>whispering</i>"), "whispering");
        assert_eq!(strip_markup("<v Roger>Hello</v>"), "Hello");
        assert_eq!(strip_markup("plain"), "plain");
        // An unclosed tag is not a tag: someone typed a less-than sign.
        assert_eq!(strip_markup("a < b"), "a < b");
    }

    #[test]
    fn a_file_read_back_says_what_it_said() {
        let cues = [
            cue(1, 1000, 4000, "Hello there."),
            cue(2, 5500, 7000, "- Who's there?\n- It's me."),
        ];
        for format in CaptionFormat::ALL {
            let text = to_string(&cues, format);
            let back = parse(&text);
            assert!(back.warnings.is_empty(), "{format:?}: {:?}", back.warnings);
            assert_eq!(back.cues.len(), 2, "{format:?}");
            for (original, parsed) in cues.iter().zip(&back.cues) {
                assert_eq!(parsed.span, original.range(), "{format:?}");
                assert_eq!(parsed.text, original.text, "{format:?}");
            }
        }
    }
}
