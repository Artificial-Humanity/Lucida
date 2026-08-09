//! The program's only date arithmetic.
//!
//! Both directions of the same conversion, hand-rolled rather than reached for.
//! A date crate would be a runtime dependency bought for thirty lines, and the
//! algorithm — Howard Hinnant's `days_from_civil` and its inverse — is exact for
//! every proleptic Gregorian date and is what every date library uses
//! underneath.
//!
//! Everything here is UTC and whole days or seconds. Nothing in Lucida needs a
//! local timezone: the two callers are a shutdown date, which is a date and not a
//! moment, and a ledger timestamp, which is a record rather than a schedule and
//! is better off unambiguous than familiar.

/// Seconds since the epoch, now. Zero if the clock is somehow before 1970, which
/// is not a case worth a `Result` — it makes a ledger entry look old and a
/// retirement look distant, and both are harmless.
pub fn now() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|since| since.as_secs() as i64)
        .unwrap_or(0)
}

/// `YYYY-MM-DD` to seconds since the epoch, at midnight UTC.
///
/// `None` for anything that is not three plausible numbers joined by dashes.
/// Callers treat that as "no date", which is deliberately the harmless answer:
/// a typo can only ever understate.
pub fn unix_time(date: &str) -> Option<i64> {
    let mut parts = date.split('-');
    let year: i64 = parts.next()?.parse().ok()?;
    let month: i64 = parts.next()?.parse().ok()?;
    let day: i64 = parts.next()?.parse().ok()?;
    if parts.next().is_some() || !(1..=12).contains(&month) || !(1..=31).contains(&day) {
        return None;
    }

    // Shift the year so it starts in March, which puts the leap day last and
    // makes the month-length pattern regular.
    let shifted = year - i64::from(month <= 2);
    let era = if shifted >= 0 { shifted } else { shifted - 399 } / 400;
    let year_of_era = shifted - era * 400;
    let day_of_year = (153 * (month + if month > 2 { -3 } else { 9 }) + 2) / 5 + day - 1;
    let day_of_era = year_of_era * 365 + year_of_era / 4 - year_of_era / 100 + day_of_year;
    let days = era * 146_097 + day_of_era - 719_468;

    Some(days * 86_400)
}

/// Seconds since the epoch to `YYYY-MM-DD HH:MM` UTC.
///
/// For a human reading `lucida history`. The ledger itself stores the number, so
/// this formatting is presentation and never the record — a stored string in
/// somebody's idea of a format is how a log becomes unparseable a year later.
pub fn stamp(seconds: i64) -> String {
    let (days, rest) = (seconds.div_euclid(86_400), seconds.rem_euclid(86_400));
    let (year, month, day) = civil_from_days(days);
    format!(
        "{year:04}-{month:02}-{day:02} {:02}:{:02}",
        rest / 3600,
        (rest % 3600) / 60
    )
}

/// The inverse of the shift in [`unix_time`].
fn civil_from_days(days: i64) -> (i64, u32, u32) {
    let shifted = days + 719_468;
    let era = if shifted >= 0 { shifted } else { shifted - 146_096 } / 146_097;
    let day_of_era = shifted - era * 146_097;
    let year_of_era =
        (day_of_era - day_of_era / 1460 + day_of_era / 36_524 - day_of_era / 146_096) / 365;
    let year = year_of_era + era * 400;
    let day_of_year = day_of_era - (365 * year_of_era + year_of_era / 4 - year_of_era / 100);
    let month_shifted = (5 * day_of_year + 2) / 153;
    let day = (day_of_year - (153 * month_shifted + 2) / 5 + 1) as u32;
    let month = (month_shifted + if month_shifted < 10 { 3 } else { -9 }) as u32;

    (year + i64::from(month <= 2), month, day)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Pinned against instants computed independently. The first literal written
    /// for 2026-08-17 was twelve days out, which is the whole argument for
    /// checking a hand-rolled conversion against something that is not itself.
    #[test]
    fn a_civil_date_converts_to_the_right_instant() {
        assert_eq!(unix_time("1970-01-01"), Some(0));
        assert_eq!(unix_time("2000-02-29"), Some(951_782_400));
        assert_eq!(unix_time("1900-03-01"), Some(-2_203_891_200));
        assert_eq!(unix_time("2026-08-17"), Some(1_786_924_800));
        assert_eq!(unix_time("2026-12-01"), Some(1_796_083_200));
    }

    #[test]
    fn an_unparseable_date_has_no_instant() {
        for bad in ["", "soon", "2026-13-01", "2026-08-32", "2026-08", "2026-08-17-1"] {
            assert_eq!(unix_time(bad), None, "{bad} parsed");
        }
    }

    /// The two directions must agree, which is the property that actually
    /// matters and the one a transcription error in either would break.
    #[test]
    fn the_conversion_round_trips() {
        for date in [
            "1970-01-01",
            "1999-12-31",
            "2000-01-01",
            "2000-02-29",
            "2024-02-29",
            "2026-08-09",
            "2100-03-01",
        ] {
            let seconds = unix_time(date).unwrap();
            assert_eq!(stamp(seconds), format!("{date} 00:00"), "{date}");
        }
    }

    #[test]
    fn the_time_of_day_survives() {
        let noon = unix_time("2026-08-09").unwrap() + 12 * 3600 + 34 * 60 + 56;
        assert_eq!(stamp(noon), "2026-08-09 12:34");
    }

    /// Dates before the epoch are negative seconds, and integer division
    /// truncates towards zero rather than down — which is why this uses
    /// `div_euclid`. Written with `/` first, and 1969 came out as 1970.
    #[test]
    fn a_date_before_the_epoch_still_reads_correctly() {
        assert_eq!(stamp(unix_time("1969-07-20").unwrap()), "1969-07-20 00:00");
        assert_eq!(stamp(-1), "1969-12-31 23:59");
    }
}
