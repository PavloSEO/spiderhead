//! Wall-clock helpers: the named UTC timestamp formats used by run logs and job records.
//!
//! Local-time formats are not here: they need the operator's time zone, which the core does not model.

use std::time::{SystemTime, UNIX_EPOCH};

/// Timestamp formats of the shared helper.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Format {
    /// `2026-10-25T12:34:56.123456Z`, microseconds dropped when zero.
    Zulu,
    /// `2026-10-25T12:34:56Z`.
    ZuluSeconds,
    /// `2026-10-25T12:34:56.123456+00:00`.
    Iso,
    /// `2026-10-25T12:34:56+00:00` — whole seconds, no fraction.
    IsoSeconds,
}

impl Format {
    /// Parse a format name (`"zulu"`, `"zulu_seconds"`, `"iso"`).
    pub fn parse(name: &str) -> Option<Self> {
        match name {
            "zulu" => Some(Self::Zulu),
            "zulu_seconds" => Some(Self::ZuluSeconds),
            "iso" => Some(Self::Iso),
            _ => None,
        }
    }
}

/// Error for an unknown format name.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("unknown timestamp format: {0:?}")]
pub struct UnknownFormat(pub String);

/// Format a POSIX timestamp (seconds, may be fractional) in UTC.
pub fn stamp_epoch(seconds: f64, format: Format) -> String {
    let whole = seconds.floor();
    let mut micros = ((seconds - whole) * 1_000_000.0).round() as i64;
    let mut secs = whole as i64;
    if micros >= 1_000_000 {
        secs += 1;
        micros -= 1_000_000;
    }
    let days = secs.div_euclid(86_400);
    let rem = secs.rem_euclid(86_400);
    let (year, month, day) = civil_from_days(days);
    let (hour, minute, second) = (rem / 3600, (rem % 3600) / 60, rem % 60);
    let base = format!("{year:04}-{month:02}-{day:02}T{hour:02}:{minute:02}:{second:02}");
    match format {
        Format::Zulu if micros != 0 => format!("{base}.{micros:06}Z"),
        Format::Zulu | Format::ZuluSeconds => format!("{base}Z"),
        Format::Iso if micros != 0 => format!("{base}.{micros:06}+00:00"),
        Format::Iso | Format::IsoSeconds => format!("{base}+00:00"),
    }
}

/// The current UTC instant in `format`.
pub fn stamp(format: Format) -> String {
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs_f64())
        .unwrap_or(0.0);
    stamp_epoch(now, format)
}

/// Parse a format by name, rejecting unknown ones.
pub fn stamp_named(name: &str) -> Result<String, UnknownFormat> {
    Format::parse(name)
        .map(stamp)
        .ok_or_else(|| UnknownFormat(name.to_owned()))
}

/// Days since 1970-01-01 to (year, month, day), proleptic Gregorian.
fn civil_from_days(days: i64) -> (i64, i64, i64) {
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = yoe + era * 400 + i64::from(month <= 2);
    (year, month, day)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stamp_epoch_formats_a_posix_timestamp() {
        // 2026-10-25T12:34:56 UTC.
        let epoch = 1792931696.0;
        assert_eq!(stamp_epoch(epoch, Format::Iso), "2026-10-25T12:34:56+00:00");
        assert_eq!(
            stamp_epoch(epoch, Format::ZuluSeconds),
            "2026-10-25T12:34:56Z"
        );
    }

    #[test]
    fn epoch_zero_is_the_unix_origin() {
        assert_eq!(stamp_epoch(0.0, Format::Iso), "1970-01-01T00:00:00+00:00");
    }

    #[test]
    fn zulu_keeps_microseconds() {
        assert_eq!(
            stamp_epoch(1792931696.0 + 0.123456, Format::Zulu),
            "2026-10-25T12:34:56.123456Z"
        );
    }

    #[test]
    fn every_format_renders_one_instant() {
        let t = 1792931696.0 + 0.123456;
        assert_eq!(stamp_epoch(t, Format::Zulu), "2026-10-25T12:34:56.123456Z");
        assert_eq!(stamp_epoch(t, Format::ZuluSeconds), "2026-10-25T12:34:56Z");
        assert_eq!(
            stamp_epoch(t, Format::Iso),
            "2026-10-25T12:34:56.123456+00:00"
        );
        assert_eq!(
            stamp_epoch(t, Format::IsoSeconds),
            "2026-10-25T12:34:56+00:00"
        );
    }

    #[test]
    fn unknown_format_is_rejected_by_name() {
        assert_eq!(stamp_named("unix"), Err(UnknownFormat("unix".to_owned())));
    }

    #[test]
    fn zulu_seconds_has_no_fraction_and_is_twenty_chars() {
        let value = stamp(Format::ZuluSeconds);
        assert_eq!(value.len(), 20);
        assert!(value.ends_with('Z') && !value.contains('.'));
    }

    // Leap-day check on the civil conversion: 2028-02-29T00:00:00Z.
    #[test]
    fn civil_conversion_handles_leap_day() {
        assert_eq!(
            stamp_epoch(1835395200.0, Format::ZuluSeconds),
            "2028-02-29T00:00:00Z"
        );
    }
}
