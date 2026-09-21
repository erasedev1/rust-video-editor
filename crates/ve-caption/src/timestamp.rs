//! Caption timestamps, in the two spellings the formats use.
//!
//! Both formats count **milliseconds from the start of the media**, not frames.
//! That is a choice their authors made and it is the right one for text: a
//! caption is not a picture, so it has no reason to land on a frame boundary,
//! and a file written in frames would be wrong the moment it was played at
//! another rate.
//!
//! Milliseconds divide the tick base exactly (`TICKS_PER_SECOND / 1000` is a
//! whole number), so reading a file is lossless. Writing one is not, in one
//! direction only: a cue dragged onto a 29.97 frame boundary sits at a tick
//! that is not a whole millisecond, and rounding moves it by at most half a
//! millisecond — a sixtieth of a frame at that rate.

use ve_time::Ticks;

/// Which character separates the seconds from the milliseconds when writing.
///
/// SubRip uses a comma and WebVTT a full stop. This is the one place the two
/// formats genuinely disagree about time, and a player that is handed the other
/// one usually drops the cue.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Separator {
    Comma,
    Period,
}

impl Separator {
    fn as_char(self) -> char {
        match self {
            Separator::Comma => ',',
            Separator::Period => '.',
        }
    }
}

/// Renders `t` as `HH:MM:SS,mmm` or `HH:MM:SS.mmm`.
///
/// Hours are always written, even when zero. WebVTT allows them to be left out
/// and every player accepts them, so writing them costs three characters and
/// removes a way to be misread.
pub fn format(t: Ticks, separator: Separator) -> String {
    let total_ms = to_millis(t).max(0);
    let ms = total_ms % 1000;
    let total_seconds = total_ms / 1000;
    let seconds = total_seconds % 60;
    let minutes = (total_seconds / 60) % 60;
    let hours = total_seconds / 3600;
    format!("{hours:02}:{minutes:02}:{seconds:02}{}{ms:03}", separator.as_char())
}

/// Ticks to whole milliseconds, rounded half away from zero.
pub fn to_millis(t: Ticks) -> i64 {
    let per_ms = ve_time::TICKS_PER_SECOND / 1000;
    let raw = t.raw();
    if raw >= 0 {
        (raw + per_ms / 2) / per_ms
    } else {
        -((-raw + per_ms / 2) / per_ms)
    }
}

/// Reads a timestamp, accepting anything either format permits.
///
/// Deliberately liberal, because the reader's job is to open the file the user
/// has rather than the file the specification describes:
///
/// * either separator, whichever the extension implied
/// * hours left out — `01:30.500` is a minute and a half
/// * one or two digits in any field, and one to three in the milliseconds,
///   which is how a hand-written file ends up spelled
///
/// The writer is strict. Being generous in what is read and exact in what is
/// written is what keeps a round trip through another tool from degrading.
pub fn parse(text: &str) -> Option<Ticks> {
    let text = text.trim();
    if text.is_empty() {
        return None;
    }
    let (clock, fraction) = match text.split_once([',', '.']) {
        Some((clock, fraction)) => (clock, Some(fraction)),
        None => (text, None),
    };

    let mut parts = [0i64; 3];
    let mut count = 0;
    for field in clock.split(':') {
        if count == 3 {
            return None;
        }
        let field = field.trim();
        // Digits only. Parsing the field would accept a sign, and `-00` parses
        // as zero — so a negative timestamp would come back as a positive one
        // with its sign quietly dropped.
        if field.is_empty() || !field.chars().all(|c| c.is_ascii_digit()) {
            return None;
        }
        parts[count] = field.parse::<i64>().ok()?;
        count += 1;
    }
    if count == 0 {
        return None;
    }
    // Fields are counted from the right: the last one is always seconds, so a
    // file that leaves the hours out still lands where it meant to.
    let (hours, minutes, seconds) = match count {
        3 => (parts[0], parts[1], parts[2]),
        2 => (0, parts[0], parts[1]),
        _ => (0, 0, parts[0]),
    };
    let millis = match fraction {
        Some(f) => {
            let digits: String = f.trim().chars().take_while(|c| c.is_ascii_digit()).collect();
            if digits.is_empty() {
                return None;
            }
            // `.5` is half a second, not five milliseconds.
            let scaled: String = digits.chars().chain(std::iter::repeat('0')).take(3).collect();
            scaled.parse::<i64>().ok()?
        }
        None => 0,
    };

    Some(Ticks::from_millis(((hours * 60 + minutes) * 60 + seconds) * 1000 + millis))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_timestamp_is_written_in_the_separator_its_format_uses() {
        let t = Ticks::from_millis(3_661_042);
        assert_eq!(format(t, Separator::Comma), "01:01:01,042");
        assert_eq!(format(t, Separator::Period), "01:01:01.042");
    }

    #[test]
    fn an_hour_is_written_even_when_it_is_zero() {
        assert_eq!(format(Ticks::from_millis(500), Separator::Period), "00:00:00.500");
    }

    #[test]
    fn both_separators_are_read_whichever_format_the_file_claims_to_be() {
        assert_eq!(parse("00:00:01,500"), Some(Ticks::from_millis(1500)));
        assert_eq!(parse("00:00:01.500"), Some(Ticks::from_millis(1500)));
    }

    #[test]
    fn a_timestamp_with_no_hours_is_minutes_and_seconds() {
        assert_eq!(parse("01:30.500"), Some(Ticks::from_millis(90_500)));
        assert_eq!(parse("9.25"), Some(Ticks::from_millis(9_250)));
    }

    #[test]
    fn a_short_fraction_is_a_fraction_rather_than_a_count_of_milliseconds() {
        assert_eq!(parse("00:00:00.5"), Some(Ticks::from_millis(500)));
        assert_eq!(parse("00:00:00.05"), Some(Ticks::from_millis(50)));
        assert_eq!(parse("00:00:00.005"), Some(Ticks::from_millis(5)));
    }

    #[test]
    fn nonsense_is_refused_rather_than_guessed_at() {
        assert_eq!(parse(""), None);
        assert_eq!(parse("--> 00:00:01,000"), None);
        assert_eq!(parse("01:02:03:04"), None);
        assert_eq!(parse("a caption line"), None);
        assert_eq!(parse("00:00:01,"), None);
        assert_eq!(parse("-00:00:01,000"), None);
    }

    #[test]
    fn a_whole_millisecond_survives_a_round_trip_exactly() {
        for ms in [0i64, 1, 999, 1_000, 59_999, 3_599_999, 36_000_000] {
            let t = Ticks::from_millis(ms);
            assert_eq!(parse(&format(t, Separator::Comma)), Some(t), "{ms} ms");
        }
    }

    #[test]
    fn a_tick_between_two_milliseconds_is_rounded_rather_than_truncated() {
        let per_ms = ve_time::TICKS_PER_SECOND / 1000;
        // 1.6 ms, which is where a 29.97 frame boundary tends to land.
        assert_eq!(to_millis(Ticks::new(per_ms * 3 / 2 + 1)), 2);
        assert_eq!(to_millis(Ticks::new(per_ms / 2 - 1)), 0);
    }
}
