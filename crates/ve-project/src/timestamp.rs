//! A minimal UTC timestamp formatter.
//!
//! The project file wants a human-readable "last saved" field. That is the only
//! date handling in the whole editor, so it is written out here rather than
//! pulling in a calendar crate.

use std::time::{SystemTime, UNIX_EPOCH};

/// Seconds since the Unix epoch, or zero if the clock is before it.
pub fn now_unix() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0)
}

/// Formats epoch seconds as `YYYY-MM-DDTHH:MM:SSZ`.
pub fn format_iso8601(epoch_secs: u64) -> String {
    let days = (epoch_secs / 86_400) as i64;
    let secs_of_day = epoch_secs % 86_400;
    let (y, m, d) = civil_from_days(days);
    format!(
        "{:04}-{:02}-{:02}T{:02}:{:02}:{:02}Z",
        y,
        m,
        d,
        secs_of_day / 3600,
        (secs_of_day / 60) % 60,
        secs_of_day % 60
    )
}

/// Days since 1970-01-01 to a civil (year, month, day).
///
/// Howard Hinnant's `civil_from_days`, which shifts the era to start in March so
/// the leap day lands at the end of the cycle and the month-length table
/// collapses into a single expression.
fn civil_from_days(z: i64) -> (i64, u32, u32) {
    let z = z + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = (z - era * 146_097) as u64; // [0, 146096]
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146_096) / 365; // [0, 399]
    let y = yoe as i64 + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100); // [0, 365]
    let mp = (5 * doy + 2) / 153; // [0, 11], March-based
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32; // [1, 31]
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32; // [1, 12]
    (if m <= 2 { y + 1 } else { y }, m, d)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn known_epochs_format_correctly() {
        assert_eq!(format_iso8601(0), "1970-01-01T00:00:00Z");
        assert_eq!(format_iso8601(1_000_000_000), "2001-09-09T01:46:40Z");
        // A leap day, to exercise the era shift.
        assert_eq!(format_iso8601(1_709_164_800), "2024-02-29T00:00:00Z");
        assert_eq!(format_iso8601(1_735_689_599), "2024-12-31T23:59:59Z");
    }
}
