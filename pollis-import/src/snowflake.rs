//! Discord snowflake ids, which encode their creation time.
//!
//! Ordering and date ranges come from the id rather than the export's
//! timestamp strings, so the planner needs no date parser and two exports of
//! the same message always sort identically.

/// Milliseconds from the Unix epoch to the Discord epoch (2015-01-01T00:00:00Z).
pub const DISCORD_EPOCH_MS: u64 = 1_420_070_400_000;

/// Unix milliseconds encoded in a snowflake, or `None` if it is not a number.
pub fn unix_ms(id: &str) -> Option<u64> {
    let raw: u64 = id.parse().ok()?;
    Some((raw >> 22) + DISCORD_EPOCH_MS)
}

/// `YYYY-MM-DD` (UTC) for a Unix millisecond timestamp.
///
/// Civil-from-days (Howard Hinnant's algorithm), so the report can print dates
/// without a calendar dependency.
pub fn utc_date(unix_ms: u64) -> String {
    let days = (unix_ms / 86_400_000) as i64;
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = yoe + era * 400 + i64::from(month <= 2);
    format!("{year:04}-{month:02}-{day:02}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decodes_a_known_snowflake() {
        // Discord's documented example: 175928847299117063 → 2016-04-30T11:18:25.796Z.
        let ms = unix_ms("175928847299117063").unwrap();
        assert_eq!(ms, 1_462_015_105_796);
        assert_eq!(utc_date(ms), "2016-04-30");
    }

    #[test]
    fn rejects_a_non_numeric_id() {
        assert_eq!(unix_ms("not-a-snowflake"), None);
    }

    #[test]
    fn formats_epoch_and_leap_day() {
        assert_eq!(utc_date(0), "1970-01-01");
        // 2024-02-29T12:00:00Z
        assert_eq!(utc_date(1_709_208_000_000), "2024-02-29");
    }
}
