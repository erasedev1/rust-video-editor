//! Reading SubRip and WebVTT.
//!
//! # One reader for both
//!
//! The two formats differ in a header line, a separator and a handful of block
//! keywords. Everything else — an optional identifier, a timing line with an
//! arrow in it, then lines of text until a blank one — is the same shape, and
//! files in the wild ignore even those differences: `.srt` files written with
//! full stops, `.vtt` files with numbered cues. A reader that insisted on the
//! specification its extension implied would refuse files that every player
//! opens.
//!
//! So the reader accepts the union and the writer emits exactly one of the two.
//! Liberal in, exact out: that is what stops a round trip through another tool
//! from degrading a file a little more each time.

use ve_time::{Ticks, TimeRange};

use crate::{CaptionFile, ParsedCue};

/// The arrow between two timestamps. Both formats spell it the same way.
const ARROW: &str = "-->";

/// Blocks that carry no cue, which WebVTT allows between them.
const BLOCK_KEYWORDS: [&str; 3] = ["NOTE", "STYLE", "REGION"];

pub fn parse(text: &str) -> CaptionFile {
    // A byte-order mark is invisible and would otherwise be read as part of the
    // first cue's identifier — or, worse, its first timestamp.
    let text = text.strip_prefix('\u{feff}').unwrap_or(text);

    let mut file = CaptionFile::default();
    let lines: Vec<&str> = text.lines().collect();
    let mut i = 0;
    let mut settings_dropped = 0usize;

    while i < lines.len() {
        let line = lines[i].trim();

        if line.is_empty() {
            i += 1;
            continue;
        }
        // The WebVTT header, with or without the text that may follow it.
        if i == 0 && line.starts_with("WEBVTT") {
            i += 1;
            continue;
        }
        if BLOCK_KEYWORDS.iter().any(|k| line == *k || line.starts_with(&format!("{k} "))) {
            i = skip_block(&lines, i);
            continue;
        }

        // A cue is found by its timing line, not by counting: the identifier
        // above it is optional in one format and routinely absent in files of
        // the other.
        let timing = if line.contains(ARROW) {
            i
        } else if lines.get(i + 1).is_some_and(|l| l.contains(ARROW)) {
            i + 1
        } else {
            file.warnings.push(format!("line {}: expected a timing line", i + 1));
            i = skip_block(&lines, i);
            continue;
        };

        let (start, end, had_settings) = match parse_timing(lines[timing]) {
            Some(parsed) => parsed,
            None => {
                file.warnings.push(format!(
                    "line {}: {} is not a timing line",
                    timing + 1,
                    lines[timing]
                ));
                i = skip_block(&lines, i);
                continue;
            }
        };
        if had_settings {
            settings_dropped += 1;
        }

        let text_start = timing + 1;
        let text_end = block_end(&lines, text_start);
        let body = lines[text_start..text_end].join("\n").trim().to_string();

        i = text_end;

        if end <= start {
            file.warnings.push(format!(
                "line {}: a caption that ends before it starts was dropped",
                timing + 1
            ));
            continue;
        }
        if body.is_empty() {
            file.warnings
                .push(format!("line {}: a caption with no text was dropped", timing + 1));
            continue;
        }
        file.cues.push(ParsedCue { span: TimeRange::from_bounds(start, end), text: body });
    }

    if settings_dropped > 0 {
        // Said once rather than per cue: a file styled by another tool would
        // otherwise bury every real problem under hundreds of copies of this.
        file.warnings.push(format!(
            "{settings_dropped} caption(s) carried position or alignment settings, which \
             Verge does not store"
        ));
    }
    file
}

/// Splits a timing line into its two times, and whether anything followed them.
fn parse_timing(line: &str) -> Option<(Ticks, Ticks, bool)> {
    let (left, right) = line.split_once(ARROW)?;
    let start = crate::timestamp::parse(left)?;
    // WebVTT cue settings sit after the end time, separated by spaces:
    // `00:00:01.000 --> 00:00:04.000 align:start position:10%`.
    let right = right.trim();
    let (end_text, rest) = match right.split_once(char::is_whitespace) {
        Some((end, rest)) => (end, rest.trim()),
        None => (right, ""),
    };
    let end = crate::timestamp::parse(end_text)?;
    Some((start, end, !rest.is_empty()))
}

/// The index of the first blank line at or after `from`.
fn block_end(lines: &[&str], from: usize) -> usize {
    let mut i = from;
    while i < lines.len() && !lines[i].trim().is_empty() {
        i += 1;
    }
    i
}

/// Skips the whole block starting at `from`, blank line included, so a
/// malformed cue costs its own block and not the file.
fn skip_block(lines: &[&str], from: usize) -> usize {
    (block_end(lines, from) + 1).max(from + 1)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ms(t: Ticks) -> i64 {
        crate::timestamp::to_millis(t)
    }

    #[test]
    fn an_ordinary_subrip_file_reads() {
        let file = parse(
            "1\n00:00:01,000 --> 00:00:04,000\nHello there.\n\n\
             2\n00:00:05,500 --> 00:00:07,000\n- Who's there?\n- It's me.\n",
        );
        assert!(file.warnings.is_empty(), "{:?}", file.warnings);
        assert_eq!(file.cues.len(), 2);
        assert_eq!(ms(file.cues[0].span.start), 1000);
        assert_eq!(ms(file.cues[0].span.end()), 4000);
        assert_eq!(file.cues[0].text, "Hello there.");
        assert_eq!(file.cues[1].text, "- Who's there?\n- It's me.");
    }

    #[test]
    fn an_ordinary_webvtt_file_reads() {
        let file = parse(
            "WEBVTT - Some title\n\n\
             NOTE this is a comment\nspanning two lines\n\n\
             STYLE\n::cue { color: peachpuff }\n\n\
             intro\n00:00:01.000 --> 00:00:04.000\nHello there.\n",
        );
        assert!(file.warnings.is_empty(), "{:?}", file.warnings);
        assert_eq!(file.cues.len(), 1);
        assert_eq!(file.cues[0].text, "Hello there.");
    }

    #[test]
    fn a_file_spelled_the_other_way_round_still_reads() {
        // An .srt with full stops and no indices, and a .vtt with commas and
        // indices, are both things that exist and that players accept.
        let file = parse("00:00:01.000 --> 00:00:02.000\nno index here\n");
        assert_eq!(file.cues.len(), 1);
        let file = parse("WEBVTT\n\n1\n00:00:01,000 --> 00:00:02,000\ncommas\n");
        assert_eq!(file.cues.len(), 1);
    }

    #[test]
    fn a_byte_order_mark_is_not_read_as_part_of_the_first_cue() {
        let file = parse("\u{feff}1\n00:00:01,000 --> 00:00:02,000\nfirst\n");
        assert!(file.warnings.is_empty(), "{:?}", file.warnings);
        assert_eq!(file.cues.len(), 1);
    }

    #[test]
    fn cue_settings_are_dropped_once_rather_than_per_cue() {
        let file = parse(
            "WEBVTT\n\n00:00:01.000 --> 00:00:02.000 align:start position:10%\none\n\n\
             00:00:03.000 --> 00:00:04.000 align:end\ntwo\n",
        );
        assert_eq!(file.cues.len(), 2);
        assert_eq!(file.warnings.len(), 1);
        assert!(file.warnings[0].contains("2 caption(s)"), "{:?}", file.warnings);
    }

    #[test]
    fn a_broken_cue_costs_its_own_block_and_not_the_file() {
        let file = parse(
            "1\n00:00:01,000 --> 00:00:02,000\nfirst\n\n\
             2\nthis line is not a timing line\nsecond\n\n\
             3\n00:00:05,000 --> 00:00:06,000\nthird\n",
        );
        assert_eq!(file.cues.len(), 2);
        assert_eq!(file.cues[1].text, "third");
        assert_eq!(file.warnings.len(), 1);
        // The warning names where the block began, which is what the reader
        // was looking at when it gave up and what the user has to go and find.
        assert!(file.warnings[0].contains("line 5"), "{:?}", file.warnings);
    }

    #[test]
    fn a_cue_that_ends_before_it_starts_is_dropped_and_reported() {
        let file = parse("1\n00:00:04,000 --> 00:00:02,000\nbackwards\n");
        assert!(file.cues.is_empty());
        assert_eq!(file.warnings.len(), 1);
    }

    #[test]
    fn markup_is_kept_exactly_as_the_file_wrote_it() {
        // Verge cannot draw italics yet, but stripping the tags would lose
        // something the file said and could not be undone on the way out.
        let file = parse("1\n00:00:01,000 --> 00:00:02,000\n<i>whispering</i>\n");
        assert_eq!(file.cues[0].text, "<i>whispering</i>");
    }

    #[test]
    fn windows_line_endings_read_the_same_as_unix_ones() {
        let file = parse("1\r\n00:00:01,000 --> 00:00:02,000\r\nfirst\r\nsecond\r\n\r\n");
        assert!(file.warnings.is_empty(), "{:?}", file.warnings);
        assert_eq!(file.cues[0].text, "first\nsecond");
    }

    #[test]
    fn a_file_that_is_not_captions_at_all_yields_nothing_and_says_why() {
        let file = parse("{\n  \"this\": \"is json\"\n}\n");
        assert!(file.cues.is_empty());
        assert!(!file.warnings.is_empty());
    }
}
