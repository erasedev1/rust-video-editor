//! Writing SubRip and WebVTT.
//!
//! The writer is strict where the reader is liberal: one separator per format,
//! hours always written, `\n` line endings, and a trailing blank line after
//! every cue. A file produced here is the shape both specifications describe,
//! so it can be handed to a player, a broadcaster or another editor without
//! anyone having to be generous about it.

use std::fmt::Write as _;

use ve_core::Cue;

use crate::timestamp::{self, Separator};
use crate::CaptionFormat;

/// Renders cues as a caption file.
///
/// Blank cues are skipped. One is blank for as long as it takes to type into
/// it, and neither format has a spelling for a cue with no text that a player
/// reads back as anything other than a broken file.
pub fn to_string(cues: &[Cue], format: CaptionFormat) -> String {
    let mut out = String::new();
    let separator = match format {
        CaptionFormat::SubRip => Separator::Comma,
        CaptionFormat::WebVtt => Separator::Period,
    };

    if format == CaptionFormat::WebVtt {
        out.push_str("WEBVTT\n\n");
    }

    let mut index = 0;
    for cue in cues.iter().filter(|c| !c.is_blank()) {
        index += 1;
        // SubRip counts its cues and the count is part of the format. WebVTT's
        // identifier is optional and means something else — a name a stylesheet
        // can select — so inventing numbers there would be inventing meaning.
        if format == CaptionFormat::SubRip {
            let _ = writeln!(out, "{index}");
        }
        let _ = writeln!(
            out,
            "{} --> {}",
            timestamp::format(cue.start, separator),
            timestamp::format(cue.end(), separator)
        );
        out.push_str(cue.text.trim_end());
        out.push_str("\n\n");
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use ve_core::CueId;
    use ve_time::Ticks;

    fn cue(raw: u64, start_ms: i64, end_ms: i64, text: &str) -> Cue {
        Cue::new(
            CueId::from_raw(raw),
            Ticks::from_millis(start_ms),
            Ticks::from_millis(end_ms - start_ms),
            text,
        )
    }

    #[test]
    fn subrip_is_written_with_indices_and_commas() {
        let out = to_string(
            &[
                cue(1, 1000, 4000, "Hello there."),
                cue(2, 5500, 7000, "- Who's there?\n- It's me."),
            ],
            CaptionFormat::SubRip,
        );
        assert_eq!(
            out,
            "1\n00:00:01,000 --> 00:00:04,000\nHello there.\n\n\
             2\n00:00:05,500 --> 00:00:07,000\n- Who's there?\n- It's me.\n\n"
        );
    }

    #[test]
    fn webvtt_is_written_with_a_header_full_stops_and_no_invented_identifiers() {
        let out = to_string(&[cue(1, 1000, 4000, "Hello there.")], CaptionFormat::WebVtt);
        assert_eq!(out, "WEBVTT\n\n00:00:01.000 --> 00:00:04.000\nHello there.\n\n");
    }

    #[test]
    fn a_blank_cue_is_left_out_and_does_not_consume_an_index() {
        let out = to_string(
            &[cue(1, 0, 1000, "first"), cue(2, 2000, 3000, "  "), cue(3, 4000, 5000, "second")],
            CaptionFormat::SubRip,
        );
        assert!(out.starts_with("1\n"), "{out}");
        assert!(out.contains("2\n00:00:04,000"), "{out}");
        assert!(!out.contains("00:00:02,000"), "{out}");
    }

    #[test]
    fn an_empty_track_still_writes_a_readable_webvtt_file() {
        // A header and nothing else is a valid WebVTT file; an empty SubRip
        // file is an empty file, which is also what SubRip's nothing looks like.
        assert_eq!(to_string(&[], CaptionFormat::WebVtt), "WEBVTT\n\n");
        assert_eq!(to_string(&[], CaptionFormat::SubRip), "");
    }
}
