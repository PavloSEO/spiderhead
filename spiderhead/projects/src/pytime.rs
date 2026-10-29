//! Datetime helpers matching the `datetime`/`zoneinfo` surface the project modules use.
//!
//! Formatting goes through `spiderhead_core::timeutil`; parsing accepts the
//! RFC3339-style inputs project documents carry (date, `T` or space separator, `Z` and
//! `±HH:MM[:SS]` offsets, fractional seconds).

use crate::error::{ProjectsError, ProjectsResult};
use jiff::civil::{Date, DateTime, Time};
use jiff::tz::TimeZone;
use jiff::{Span, Timestamp, Zoned};

/// Current UTC instant as `YYYY-MM-DDTHH:MM:SS[.ffffff]Z`.
pub fn zulu() -> String {
    spiderhead_core::timeutil::stamp(spiderhead_core::timeutil::Format::Zulu)
}

/// Current UTC instant, second precision, `Z` suffix.
pub fn zulu_seconds() -> String {
    spiderhead_core::timeutil::stamp(spiderhead_core::timeutil::Format::ZuluSeconds)
}

/// Current UTC instant as ISO 8601 text with a `+00:00` offset.
pub fn iso_now() -> String {
    spiderhead_core::timeutil::stamp(spiderhead_core::timeutil::Format::Iso)
}

/// Format a POSIX timestamp in UTC.
pub fn epoch_z(seconds: f64) -> String {
    spiderhead_core::timeutil::stamp_epoch(seconds, spiderhead_core::timeutil::Format::Zulu)
}

/// Wall-clock seconds since the Unix epoch.
pub fn time_seconds() -> f64 {
    Timestamp::now().as_second() as f64 + Timestamp::now().subsec_nanosecond() as f64 / 1e9
}

static MONOTONIC_EPOCH: std::sync::LazyLock<std::time::Instant> =
    std::sync::LazyLock::new(std::time::Instant::now);

/// Monotonic seconds (process-local epoch; only deltas are meaningful).
pub fn monotonic() -> f64 {
    MONOTONIC_EPOCH.elapsed().as_secs_f64()
}

/// One parsed `datetime` value: the naive fields plus an optional UTC offset.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct PyDateTime {
    /// Civil fields exactly as written.
    pub civil: DateTime,
    /// UTC offset in seconds; `None` for a naive value.
    pub offset_seconds: Option<f64>,
}

impl PyDateTime {
    /// Whether the value carries a UTC offset.
    pub fn is_aware(&self) -> bool {
        self.offset_seconds.is_some()
    }

    /// Whether the offset is exactly UTC.
    pub fn is_utc(&self) -> bool {
        self.offset_seconds == Some(0.0)
    }

    /// Epoch seconds; aware values convert, naive values are refused as a caller bug.
    pub fn timestamp(&self) -> ProjectsResult<f64> {
        let offset = self
            .offset_seconds
            .ok_or_else(|| crate::error::value("naive datetime has no timestamp"))?;
        let utc = self.civil.to_zoned(TimeZone::UTC).map_err(invalid)?;
        let seconds = utc.timestamp().as_second() as f64 - offset;
        Ok(seconds + self.civil.subsec_nanosecond() as f64 / 1e9)
    }
}

fn invalid<E: std::fmt::Display>(e: E) -> ProjectsError {
    crate::error::value(e.to_string())
}

fn digits(bytes: &[u8], at: usize, len: usize) -> Option<i64> {
    let slice = bytes.get(at..at + len)?;
    if !slice.iter().all(u8::is_ascii_digit) {
        return None;
    }
    let mut value = 0i64;
    for &b in slice {
        value = value * 10 + i64::from(b - b'0');
    }
    Some(value)
}

fn bad(text: &str) -> ProjectsError {
    crate::error::value(format!("Invalid isoformat string: '{text}'"))
}

fn range_err(field: &str, lo: i64, hi: i64, value: i64) -> ProjectsError {
    crate::error::value(format!("{field} must be in {lo}..{hi}, not {value}"))
}

/// Duration text for the offset-overflow message.
fn timedelta_repr(seconds: i64) -> String {
    let days = seconds.div_euclid(86_400);
    let secs = seconds.rem_euclid(86_400);
    match (days, secs) {
        (0, s) => format!("datetime.timedelta(seconds={s})"),
        (d, 0) => format!("datetime.timedelta(days={d})"),
        (d, s) => format!("datetime.timedelta(days={d}, seconds={s})"),
    }
}

fn offset_err(seconds: i64) -> ProjectsError {
    crate::error::value(format!(
        "offset must be a timedelta strictly between -timedelta(hours=24) and \
         timedelta(hours=24), not {}",
        timedelta_repr(seconds)
    ))
}

/// Last day of a month, leap-year aware.
fn last_day(year: i64, month: i64) -> i64 {
    const DAYS: [i64; 12] = [31, 28, 31, 30, 31, 30, 31, 31, 30, 31, 30, 31];
    let mut last = DAYS[(month - 1) as usize];
    if month == 2 && (year % 4 == 0 && (year % 100 != 0 || year % 400 == 0)) {
        last = 29;
    }
    last
}

/// Parse a possibly-fractional field pair (digits, microsecond), truncating to 6 digits.
fn frac_field(body: &[u8]) -> Option<(i64, i64)> {
    let dot = body
        .iter()
        .position(|&b| b == b'.' || b == b',')
        .unwrap_or(body.len());
    let head = parse_int(&body[..dot])?;
    let mut micro = 0i64;
    if dot < body.len() {
        let frac = &body[dot + 1..];
        if frac.is_empty() || !frac.iter().all(u8::is_ascii_digit) {
            return None;
        }
        for (i, &b) in frac.iter().enumerate() {
            if i < 6 {
                micro = micro * 10 + i64::from(b - b'0');
            }
        }
        for _ in frac.len().min(6)..6 {
            micro *= 10;
        }
    }
    Some((head, micro))
}

/// Parse the RFC3339 shapes project documents carry.
///
/// Accepts `YYYY-MM-DD` or `YYYYMMDD`, an optional `HH:MM[:SS[.fraction]]` time after `T` or a
/// space, and an optional `Z`/`±HH[:MM[:SS[.fraction]]]` offset. Returns `Err` for invalid input.
/// Week and ordinal dates are not supported.
pub fn fromisoformat(text: &str) -> Result<PyDateTime, ProjectsError> {
    let bytes = text.as_bytes();
    let date_part = if bytes.len() >= 10 && bytes[4] == b'-' && bytes[7] == b'-' {
        &bytes[..10]
    } else if bytes.len() >= 8 && bytes[..8].iter().all(u8::is_ascii_digit) {
        &bytes[..8]
    } else {
        return Err(bad(text));
    };
    let (year, month, day) = if date_part.len() == 10 {
        match (
            digits(date_part, 0, 4),
            digits(date_part, 5, 2),
            digits(date_part, 8, 2),
        ) {
            (Some(y), Some(m), Some(d)) => (y, m, d),
            _ => return Err(bad(text)),
        }
    } else {
        match (
            digits(date_part, 0, 4),
            digits(date_part, 4, 2),
            digits(date_part, 6, 2),
        ) {
            (Some(y), Some(m), Some(d)) => (y, m, d),
            _ => return Err(bad(text)),
        }
    };
    if !(1..=9999).contains(&year) {
        return Err(range_err("year", 1, 9999, year));
    }
    if !(1..=12).contains(&month) {
        return Err(range_err("month", 1, 12, month));
    }
    let last = last_day(year, month);
    if !(1..=last).contains(&day) {
        return Err(crate::error::value(format!(
            "day {day} must be in range 1..{last} for month {month} in year {year}"
        )));
    }
    let date = Date::new(year as i16, month as i8, day as i8).map_err(invalid)?;
    let mut civil = date.to_datetime(Time::midnight());
    let mut offset_seconds = None;
    if bytes.len() > date_part.len() {
        // One arbitrary separator character, then the time fields.
        let tb = &bytes[date_part.len() + 1..];
        let off_at = tb
            .iter()
            .position(|&b| matches!(b, b'+' | b'-' | b'Z' | b'z'))
            .unwrap_or(tb.len());
        let time_body = &tb[..off_at];
        let (hour, minute, second, micro) = if time_body.contains(&b':') {
            let fields: Vec<&[u8]> = time_body.split(|&b| b == b':').collect();
            match fields.len() {
                1..=3 => {
                    if fields[0].len() != 2 {
                        return Err(bad(text));
                    }
                    let h = parse_int(fields[0]).ok_or_else(|| bad(text))?;
                    let (m, s, us) = match fields.len() {
                        1 => (0, 0, 0),
                        2 => (
                            parse_int(fields[1])
                                .filter(|_v| fields[1].len() == 2)
                                .ok_or_else(|| bad(text))?,
                            0,
                            0,
                        ),
                        _ => {
                            let m = parse_int(fields[1])
                                .filter(|_v| fields[1].len() == 2)
                                .ok_or_else(|| bad(text))?;
                            let (s, us) = frac_field(fields[2]).ok_or_else(|| bad(text))?;
                            if fields[2].iter().take_while(|b| b.is_ascii_digit()).count() != 2 {
                                return Err(bad(text));
                            }
                            (m, s, us)
                        }
                    };
                    (h, m, s, us)
                }
                _ => return Err(bad(text)),
            }
        } else {
            // Compact HH, HHMM, HHMMSS[.frac]; a fraction is seconds-only here.
            let dot_at = time_body.iter().position(|&b| b == b'.' || b == b',');
            let head = match dot_at {
                Some(p) => &time_body[..p],
                None => time_body,
            };
            let (digits_body, us) = frac_field(time_body).ok_or_else(|| bad(text))?;
            match (head.len(), digits_body) {
                (2, h) if dot_at.is_none() => (h, 0, 0, us),
                (4, hhmm) if dot_at.is_none() => (hhmm / 100, hhmm % 100, 0, us),
                (6, hhmmss) => (hhmmss / 10_000, hhmmss / 100 % 100, hhmmss % 100, us),
                _ => return Err(bad(text)),
            }
        };
        if !(0..=23).contains(&hour) {
            return Err(range_err("hour", 0, 23, hour));
        }
        if !(0..=59).contains(&minute) {
            return Err(range_err("minute", 0, 59, minute));
        }
        if !(0..=59).contains(&second) {
            return Err(range_err("second", 0, 59, second));
        }
        civil = date.to_datetime(
            Time::new(
                hour as i8,
                minute as i8,
                second as i8,
                (micro * 1000) as i32,
            )
            .map_err(invalid)?,
        );
        if off_at < tb.len() {
            offset_seconds = Some(parse_offset(&tb[off_at..], text)?);
        }
    }
    Ok(PyDateTime {
        civil,
        offset_seconds,
    })
}

fn parse_int(text: &[u8]) -> Option<i64> {
    if text.is_empty() || !text.iter().all(u8::is_ascii_digit) {
        return None;
    }
    let mut value = 0i64;
    for &b in text {
        value = value * 10 + i64::from(b - b'0');
    }
    Some(value)
}

/// `±HH[:MM[:SS[.frac]]]` or `Z` to seconds east of UTC. Fields are not
/// range-checked; the total must be strictly below 24 hours.
fn parse_offset(text: &[u8], whole: &str) -> Result<f64, ProjectsError> {
    let bad = || bad(whole);
    if text.len() == 1 && matches!(text[0], b'Z' | b'z') {
        return Ok(0.0);
    }
    let sign = match text.first() {
        Some(b'+') => 1.0f64,
        Some(b'-') => -1.0f64,
        _ => return Err(bad()),
    };
    let body = &text[1..];
    if body.is_empty() {
        return Err(bad());
    }
    let (hour, minute, second, micro) = if body.contains(&b':') {
        let fields: Vec<&[u8]> = body.split(|&b| b == b':').collect();
        match fields.len() {
            1 => (parse_int(fields[0]).ok_or_else(bad)?, 0, 0, 0),
            2 => (
                parse_int(fields[0]).ok_or_else(bad)?,
                parse_int(fields[1]).ok_or_else(bad)?,
                0,
                0,
            ),
            3 => {
                let (s, us) = frac_field(fields[2]).ok_or_else(bad)?;
                (
                    parse_int(fields[0]).ok_or_else(bad)?,
                    parse_int(fields[1]).ok_or_else(bad)?,
                    s,
                    us,
                )
            }
            _ => return Err(bad()),
        }
    } else {
        let (digits_body, us) = frac_field(body).ok_or_else(bad)?;
        let head_len = body
            .iter()
            .position(|&b| b == b'.' || b == b',')
            .unwrap_or(body.len());
        match head_len {
            2 => (digits_body, 0, 0, us),
            4 => (digits_body / 100, digits_body % 100, 0, us),
            6 => (
                digits_body / 10_000,
                digits_body / 100 % 100,
                digits_body % 100,
                us,
            ),
            _ => return Err(bad()),
        }
    };
    let total = (hour * 3600 + minute * 60 + second) as f64 + micro as f64 / 1e6;
    if total >= 86_400.0 {
        return Err(offset_err((sign * total) as i64));
    }
    Ok(sign * total)
}

/// A named IANA zone, or an error when the name is unknown.
pub fn zone(name: &str) -> ProjectsResult<TimeZone> {
    TimeZone::get(name)
        .map_err(|_| crate::error::value(format!("No time zone found with key {name:?}")))
}

/// Combine a date and time in a zone.
pub fn combine_zoned(date: Date, time: Time, zone: &TimeZone) -> ProjectsResult<Zoned> {
    date.to_datetime(time)
        .to_zoned(zone.clone())
        .map_err(invalid)
}

/// Add a span within `zone`; DST gaps resolve by the IANA zone rules.
pub fn zoned_add(zoned: &Zoned, span: Span) -> ProjectsResult<Zoned> {
    zoned.checked_add(span).map_err(invalid)
}

/// Render `zoned` as `YYYY-MM-DDTHH:MM:SS[.ffffff]±HH:MM`.
pub fn isoformat_zoned(value: &Zoned) -> String {
    let dt = value.datetime();
    let micros = dt.subsec_nanosecond() / 1000;
    let base = format!(
        "{:04}-{:02}-{:02}T{:02}:{:02}:{:02}",
        dt.year(),
        dt.month(),
        dt.day(),
        dt.hour(),
        dt.minute(),
        dt.second()
    );
    let frac = if micros != 0 {
        format!(".{micros:06}")
    } else {
        String::new()
    };
    let off = value.offset().seconds();
    let sign = if off < 0 { '-' } else { '+' };
    let abs = i64::from(off).abs();
    format!(
        "{base}{frac}{sign}{:02}:{:02}",
        abs / 3600,
        (abs % 3600) / 60
    )
}

/// Civil date as `YYYY-MM-DD`.
pub fn date_iso(date: Date) -> String {
    format!("{:04}-{:02}-{:02}", date.year(), date.month(), date.day())
}

/// Whole days as a [`Span`].
pub fn days(d: i64) -> Span {
    Span::new().days(d)
}

/// Whole seconds as a [`Span`].
pub fn seconds(s: i64) -> Span {
    Span::new().seconds(s)
}

/// The host's system time zone.
pub fn system_zone() -> TimeZone {
    TimeZone::system()
}

/// Current UTC instant as `YYYY-MM-DDTHH:MM:SS.ffffffZ` (microsecond precision).
pub fn now_iso_micros() -> String {
    let dt = Zoned::now().with_time_zone(TimeZone::UTC).datetime();
    format!(
        "{:04}-{:02}-{:02}T{:02}:{:02}:{:02}.{:06}Z",
        dt.year(),
        dt.month(),
        dt.day(),
        dt.hour(),
        dt.minute(),
        dt.second(),
        dt.subsec_nanosecond() / 1000
    )
}
