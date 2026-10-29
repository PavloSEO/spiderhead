//! Calendar arithmetic and ISO-8601 rendering for UTC instants held as Unix seconds.

/// Days since 1970-01-01 of a proleptic Gregorian date.
pub fn days_from_civil(year: i64, month: i64, day: i64) -> i64 {
    let y = if month <= 2 { year - 1 } else { year };
    let era = y.div_euclid(400);
    let yoe = y - era * 400;
    let mp = (month + 9) % 12;
    let doy = (153 * mp + 2) / 5 + day - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

/// Inverse of [`days_from_civil`]: `(year, month, day)`.
pub fn civil_from_days(days: i64) -> (i64, i64, i64) {
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    (if m <= 2 { y + 1 } else { y }, m, d)
}

/// Whether `year-month-day` exists on the calendar (years 1..=9999).
pub fn valid_date(year: i64, month: i64, day: i64) -> bool {
    if !(1..=9999).contains(&year) || !(1..=12).contains(&month) || day < 1 {
        return false;
    }
    let leap = year % 4 == 0 && (year % 100 != 0 || year % 400 == 0);
    let last = match month {
        2 if leap => 29,
        2 => 28,
        4 | 6 | 9 | 11 => 30,
        _ => 31,
    };
    day <= last
}

/// Unix seconds of a UTC civil time, or `None` when the date or clock is invalid.
pub fn epoch(year: i64, month: i64, day: i64, hour: i64, minute: i64, second: i64) -> Option<i64> {
    let clock_ok =
        (0..24).contains(&hour) && (0..60).contains(&minute) && (0..60).contains(&second);
    if !valid_date(year, month, day) || !clock_ok {
        return None;
    }
    Some(days_from_civil(year, month, day) * 86_400 + hour * 3600 + minute * 60 + second)
}

/// Earliest and latest representable instants (0001-01-01 and 9999-12-31 23:59:59).
const MIN_EPOCH: i64 = -62_135_596_800;
const MAX_EPOCH: i64 = 253_402_300_799;

/// Whether an instant lies inside the representable calendar range.
pub fn in_range(epoch: i64) -> bool {
    (MIN_EPOCH..=MAX_EPOCH).contains(&epoch)
}

/// `YYYY-MM-DD` of the instant's UTC date.
pub fn iso_date(epoch: i64) -> String {
    let (y, m, d) = civil_from_days(epoch.div_euclid(86_400));
    format!("{y:04}-{m:02}-{d:02}")
}

/// `YYYY-MM-DDTHH:MM:SS+00:00` for the instant.
pub fn iso_utc(epoch: i64) -> String {
    let secs = epoch.rem_euclid(86_400);
    format!(
        "{}T{:02}:{:02}:{:02}+00:00",
        iso_date(epoch),
        secs / 3600,
        secs % 3600 / 60,
        secs % 60
    )
}

/// ISO rendering of a wall-clock reading with microseconds, as stored in import headers.
pub fn iso_utc_micros(seconds: f64) -> String {
    let whole = seconds.floor();
    let micros = ((seconds - whole) * 1e6).round() as i64;
    let (whole, micros) = if micros >= 1_000_000 {
        (whole as i64 + 1, 0)
    } else {
        (whole as i64, micros)
    };
    let base = iso_utc(whole);
    let stem = base.trim_end_matches("+00:00");
    if micros == 0 {
        format!("{stem}+00:00")
    } else {
        format!("{stem}.{micros:06}+00:00")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trips_dates_and_renders_iso() {
        let t = epoch(2024, 3, 18, 0, 2, 9).unwrap();
        assert_eq!(iso_utc(t), "2024-03-18T00:02:09+00:00");
        assert_eq!(iso_date(t), "2024-03-18");
        assert_eq!(
            iso_utc(epoch(1, 1, 1, 0, 0, 0).unwrap()),
            "0001-01-01T00:00:00+00:00"
        );
        assert!(epoch(2023, 2, 29, 0, 0, 0).is_none());
        assert!(epoch(2024, 2, 29, 0, 0, 0).is_some());
        assert!(in_range(t) && !in_range(MIN_EPOCH - 1));
    }
}
