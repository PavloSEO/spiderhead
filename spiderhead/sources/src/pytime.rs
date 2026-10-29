//! Date and time helpers: ISO-8601 parsing and rendering, following the standard library's rules.
//!
//! Provider modules keep timestamps as epoch microseconds; `aware` marks whether the parsed text carried
//! a UTC offset, because naive and aware timestamps cannot be compared.

use std::time::{SystemTime, UNIX_EPOCH};

/// One parsed timestamp: an instant in epoch microseconds, or `naive` when no offset was present.
/// Naive and aware timestamps cannot be compared.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Parsed {
    /// Epoch microseconds for aware input; days-resolution-less civil microseconds for
    /// naive input (valid for validation only, never for instants).
    pub micros: i64,
    /// Whether the parsed text carried a UTC offset.
    pub aware: bool,
}

/// Current UTC time as epoch microseconds.
pub fn now_micros() -> i64 {
    let elapsed = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default();
    elapsed.as_secs() as i64 * 1_000_000 + i64::from(elapsed.subsec_micros())
}

/// Current UTC time truncated to whole seconds, as epoch microseconds.
pub fn now_seconds() -> i64 {
    now_micros().div_euclid(1_000_000) * 1_000_000
}

/// Parse an ISO-8601 timestamp:
/// `YYYY-MM-DD`/`YYYYMMDD`, any single separator, `HH[:MM[:SS]]` or `HHMM[SS]` with an
/// optional `.ffffff`/`,ffffff` fraction, and a `Z` or `±HH[:MM[:SS]]`/`±HHMM[SS]` offset.
/// `None` when the text is not valid.
pub fn fromisoformat(text: &str) -> Option<Parsed> {
    fromisoformat_offset(text).map(|(parsed, _)| parsed)
}

/// [`fromisoformat`] plus the parsed UTC offset in microseconds (`0` for naive input or a `Z`/`+00:00`
/// suffix). Callers that must reject a non-zero offset need it.
pub fn fromisoformat_offset(text: &str) -> Option<(Parsed, i64)> {
    let bytes = text.as_bytes();
    let (date_days, mut index) = date_part(bytes).ok()?;
    let mut hour = 0i64;
    let mut minute = 0i64;
    let mut second = 0i64;
    let mut micros = 0i64;
    let mut has_seconds = false;
    if index < bytes.len() {
        // Any single character is accepted as the date/time separator.
        index += 1;
        let (h, i) = digits(bytes, index, 2)?;
        hour = h;
        index = i;
        if bytes.get(index) == Some(&b':') {
            let (m, i) = digits(bytes, index + 1, 2)?;
            minute = m;
            index = i;
            if bytes.get(index) == Some(&b':') {
                let (s, i) = digits(bytes, index + 1, 2)?;
                second = s;
                index = i;
                has_seconds = true;
            }
        } else {
            // Compact time: the digit run after HH is MM or MMSS as a unit.
            let start = index;
            while bytes.get(index).is_some_and(u8::is_ascii_digit) {
                index += 1;
            }
            match index - start {
                0 => {}
                2 => {
                    minute = parse_num(&bytes[start..index])?;
                }
                4 => {
                    minute = parse_num(&bytes[start..start + 2])?;
                    second = parse_num(&bytes[start + 2..index])?;
                    has_seconds = true;
                }
                _ => return None,
            }
        }
        // A fraction is accepted only when seconds were parsed.
        if matches!(bytes.get(index), Some(b'.') | Some(b',')) {
            if !has_seconds {
                return None;
            }
            let (frac, i) = fraction(bytes, index + 1)?;
            micros = frac;
            index = i;
        }
    }
    if hour > 23 || minute > 59 || second > 59 {
        return None;
    }
    let civil =
        date_days * 86_400_000_000 + (hour * 3600 + minute * 60 + second) * 1_000_000 + micros;
    if index == bytes.len() {
        return Some((
            Parsed {
                micros: civil,
                aware: false,
            },
            0,
        ));
    }
    let offset = if bytes[index] == b'Z' {
        index += 1;
        Some(0i64)
    } else {
        let sign = match bytes[index] {
            b'+' => 1i64,
            b'-' => -1i64,
            _ => return None,
        };
        let (h, i) = digits(bytes, index + 1, 2)?;
        let mut index2 = i;
        let mut m = 0i64;
        let mut s = 0i64;
        let mut us = 0i64;
        let mut offset_seconds = false;
        if bytes.get(index2) == Some(&b':') {
            let (m2, i2) = digits(bytes, index2 + 1, 2)?;
            m = m2;
            index2 = i2;
            if bytes.get(index2) == Some(&b':') {
                let (s2, i3) = digits(bytes, index2 + 1, 2)?;
                s = s2;
                index2 = i3;
                offset_seconds = true;
            }
        } else {
            let start = index2;
            while bytes.get(index2).is_some_and(u8::is_ascii_digit) {
                index2 += 1;
            }
            match index2 - start {
                0 => {}
                2 => {
                    m = parse_num(&bytes[start..index2])?;
                }
                4 => {
                    m = parse_num(&bytes[start..start + 2])?;
                    s = parse_num(&bytes[start + 2..index2])?;
                    offset_seconds = true;
                }
                _ => return None,
            }
        }
        if matches!(bytes.get(index2), Some(b'.') | Some(b',')) {
            if !offset_seconds {
                return None;
            }
            let (frac, i4) = fraction(bytes, index2 + 1)?;
            us = frac;
            index2 = i4;
        }
        index = index2;
        // The offset must be strictly less than 24 hours.
        if h > 23 || m > 59 || s > 59 {
            return None;
        }
        Some(sign * ((h * 3600 + m * 60 + s) * 1_000_000 + us))
    };
    if index != bytes.len() {
        return None;
    }
    offset.map(|off| {
        (
            Parsed {
                micros: civil - off,
                aware: true,
            },
            off,
        )
    })
}

/// The date portion shared by timestamp and date parsing: `(epoch_days, consumed)` for `YYYY-MM-DD`,
/// `YYYYMMDD`, and ISO week dates (`YYYY-Www`, `YYYYWww`, `YYYY-Www-D`, `YYYYWwwD`). `Err(None)` is a bare
/// invalid-format error; `Err(Some(msg))` carries the field-range message (`month must be in 1..12, not 13`, …).
fn date_part(bytes: &[u8]) -> Result<(i64, usize), Option<String>> {
    let invalid = Err(None);
    if bytes.len() >= 10 && bytes[4] == b'-' && bytes[7] == b'-' {
        let year = parse_num(&bytes[0..4]).ok_or(None)?;
        let month = parse_num(&bytes[5..7]).ok_or(None)?;
        let day = parse_num(&bytes[8..10]).ok_or(None)?;
        check_ymd(year, month, day).map_err(Some)?;
        Ok((days_from_civil(year, month, day), 10))
    } else if bytes.len() >= 10 && bytes[4] == b'-' && bytes[5] == b'W' && bytes[8] == b'-' {
        // YYYY-Www-D
        let year = parse_num(&bytes[0..4]).ok_or(None)?;
        let week = parse_num(&bytes[6..8]).ok_or(None)?;
        let weekday = parse_num(&bytes[9..10]).ok_or(None)?;
        Ok((iso_week_to_days(year, week, weekday).ok_or(None)?, 10))
    } else if bytes.len() >= 8 && bytes[4] == b'-' && bytes[5] == b'W' {
        // YYYY-Www (weekday 1)
        let year = parse_num(&bytes[0..4]).ok_or(None)?;
        let week = parse_num(&bytes[6..8]).ok_or(None)?;
        Ok((iso_week_to_days(year, week, 1).ok_or(None)?, 8))
    } else if bytes.len() >= 8 && bytes[4] == b'W' {
        // YYYYWwwD
        let year = parse_num(&bytes[0..4]).ok_or(None)?;
        let week = parse_num(&bytes[5..7]).ok_or(None)?;
        let weekday = parse_num(&bytes[7..8]).ok_or(None)?;
        Ok((iso_week_to_days(year, week, weekday).ok_or(None)?, 8))
    } else if bytes.len() >= 7 && bytes[4] == b'W' {
        // YYYYWww (weekday 1)
        let year = parse_num(&bytes[0..4]).ok_or(None)?;
        let week = parse_num(&bytes[5..7]).ok_or(None)?;
        Ok((iso_week_to_days(year, week, 1).ok_or(None)?, 7))
    } else if bytes.len() >= 8 {
        let year = parse_num(&bytes[0..4]).ok_or(None)?;
        let month = parse_num(&bytes[4..6]).ok_or(None)?;
        let day = parse_num(&bytes[6..8]).ok_or(None)?;
        check_ymd(year, month, day).map_err(Some)?;
        Ok((days_from_civil(year, month, day), 8))
    } else {
        invalid
    }
}

/// Parse a date to epoch days; `None` when the text is not a valid date.
pub fn date_fromisoformat(text: &str) -> Option<i64> {
    date_fromisoformat_err(text).ok()
}

/// Parse a date to epoch days, or return the error message
/// (`Invalid isoformat string: '…'` / `month must be in 1..12, not 13` / …).
pub fn date_fromisoformat_err(text: &str) -> Result<i64, String> {
    let invalid = || format!("Invalid isoformat string: {}", crate::pystr::repr_str(text));
    let (days, index) =
        date_part(text.as_bytes()).map_err(|field| field.unwrap_or_else(invalid))?;
    if index != text.len() {
        return Err(invalid());
    }
    Ok(days)
}

/// 1=Monday..7=Sunday for an epoch day (1970-01-01 is Thursday = 4).
fn iso_weekday(days: i64) -> i64 {
    (days + 3).rem_euclid(7) + 1
}

/// ISO weeks in a Gregorian year: 53 when Jan 1 is Thursday, or Wednesday in a leap year.
fn iso_weeks_in_year(year: i64) -> i64 {
    let jan1_weekday = iso_weekday(days_from_civil(year, 1, 1));
    let leap = (year % 4 == 0 && year % 100 != 0) || year % 400 == 0;
    if jan1_weekday == 4 || (jan1_weekday == 3 && leap) {
        53
    } else {
        52
    }
}

/// ISO week date `(year, week 1..=53, weekday 1..=7)` to epoch days; `None` when the combination is
/// invalid (week 53 of a 52-week year, out-of-range fields).
fn iso_week_to_days(year: i64, week: i64, weekday: i64) -> Option<i64> {
    if !(1..=9999).contains(&year)
        || !(1..=7).contains(&weekday)
        || !(1..=iso_weeks_in_year(year)).contains(&week)
    {
        return None;
    }
    // Week 1's Monday is the Monday of the week containing January 4.
    let jan4 = days_from_civil(year, 1, 4);
    let week1_monday = jan4 - (iso_weekday(jan4) - 1);
    Some(week1_monday + (week - 1) * 7 + (weekday - 1))
}

/// Field-range checks for `YYYY-MM-DD`/`YYYYMMDD`, reported in year, month, day order; the first violation wins.
fn check_ymd(year: i64, month: i64, day: i64) -> Result<(), String> {
    if !(1..=9999).contains(&year) {
        return Err(format!("year must be in 1..9999, not {year}"));
    }
    let leap = (year % 4 == 0 && year % 100 != 0) || year % 400 == 0;
    let days = match month {
        1 | 3 | 5 | 7 | 8 | 10 | 12 => 31,
        4 | 6 | 9 | 11 => 30,
        2 => 28 + i64::from(leap),
        _ => return Err(format!("month must be in 1..12, not {month}")),
    };
    if !(1..=days).contains(&day) {
        return Err(format!(
            "day {day} must be in range 1..{days} for month {month} in year {year}"
        ));
    }
    Ok(())
}

fn parse_num(bytes: &[u8]) -> Option<i64> {
    if bytes.iter().all(u8::is_ascii_digit) {
        std::str::from_utf8(bytes).ok()?.parse().ok()
    } else {
        None
    }
}

fn digits(bytes: &[u8], start: usize, count: usize) -> Option<(i64, usize)> {
    let end = start.checked_add(count)?;
    let slice = bytes.get(start..end)?;
    parse_num(slice).map(|n| (n, end))
}

fn fraction(bytes: &[u8], start: usize) -> Option<(i64, usize)> {
    let mut index = start;
    while bytes.get(index).is_some_and(u8::is_ascii_digit) {
        index += 1;
    }
    let width = index - start;
    if width == 0 {
        return None;
    }
    // Fractions beyond microsecond resolution are truncated, not rounded.
    let kept = &bytes[start..start + width.min(6)];
    let value = parse_num(kept)?;
    Some((value * 10i64.pow((6 - kept.len()) as u32), index))
}

/// Days since 1970-01-01 for a proleptic Gregorian date (Howard Hinnant's algorithm).
pub fn days_from_civil(year: i64, month: i64, day: i64) -> i64 {
    let y = if month <= 2 { year - 1 } else { year };
    let era = y.div_euclid(400);
    let yoe = y.rem_euclid(400);
    let mp = if month > 2 { month - 3 } else { month + 9 };
    let doy = (153 * mp + 2) / 5 + day - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

/// (year, month, day) for days since 1970-01-01 (inverse of [`days_from_civil`]).
pub fn civil_from_days(days: i64) -> (i64, i64, i64) {
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    (yoe + era * 400 + i64::from(month <= 2), month, day)
}

/// Pacific calendar date under the post-2007 US DST rules (DST from the second Sunday of March
/// 07:00 UTC to the first Sunday of November 06:00 UTC). The Search Console default window is anchored here.
pub fn pacific_date(micros: i64) -> (i64, i64, i64) {
    let (year, ..) = civil_from_days(micros.div_euclid(86_400_000_000));
    let dst_start = nth_sunday(year, 3, 2) * 86_400_000_000 + 7 * 3_600_000_000;
    let dst_end = nth_sunday(year, 11, 1) * 86_400_000_000 + 6 * 3_600_000_000;
    let offset_hours = if micros >= dst_start && micros < dst_end {
        -7
    } else {
        -8
    };
    let local = micros + offset_hours * 3_600_000_000;
    civil_from_days(local.div_euclid(86_400_000_000))
}

/// Epoch days of the `n`-th Sunday of `month` in `year` (proleptic Gregorian).
fn nth_sunday(year: i64, month: i64, n: i64) -> i64 {
    let first = days_from_civil(year, month, 1);
    // 1970-01-01 was a Thursday: 0=Sunday … 4=Thursday.
    let dow = (first + 4).rem_euclid(7);
    first + (7 - dow).rem_euclid(7) + 7 * (n - 1)
}

/// UTC timestamp text with a `+00:00` suffix, and microseconds only when non-zero.
pub fn isoformat(micros: i64) -> String {
    iso_at(micros, false)
}

/// Timestamp text with whole seconds, microseconds truncated.
pub fn isoformat_seconds(micros: i64) -> String {
    iso_at(micros.div_euclid(1_000_000) * 1_000_000, true)
}

fn iso_at(micros: i64, whole_seconds: bool) -> String {
    let days = micros.div_euclid(86_400_000_000);
    let rem = micros.rem_euclid(86_400_000_000);
    let (year, month, day) = civil_from_days(days);
    let mut out = format!(
        "{year:04}-{month:02}-{day:02}T{:02}:{:02}:{:02}",
        rem / 3_600_000_000,
        (rem / 60_000_000) % 60,
        (rem / 1_000_000) % 60
    );
    let fraction = rem % 1_000_000;
    if fraction != 0 && !whole_seconds {
        out.push_str(&format!(".{fraction:06}"));
    }
    out.push_str("+00:00");
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    // Accepts the same forms as the parser above.
    #[test]
    fn parses_the_forms_records_carry() {
        let parsed = fromisoformat("2026-10-10T12:00:00+00:00").unwrap();
        assert!(parsed.aware);
        assert_eq!(isoformat(parsed.micros), "2026-10-10T12:00:00+00:00");
        let zulu = fromisoformat("2026-10-10T12:00:00Z").unwrap();
        assert_eq!(zulu.micros, parsed.micros);
        let offset = fromisoformat("2026-10-10T14:00:00+02:00").unwrap();
        assert_eq!(offset.micros, parsed.micros);
        let naive = fromisoformat("2026-10-10").unwrap();
        assert!(!naive.aware);
        let frac = fromisoformat("2026-10-10T12:00:00.5+00:00").unwrap();
        assert_eq!(isoformat(frac.micros), "2026-10-10T12:00:00.500000+00:00");
        assert!(fromisoformat("not a date").is_none());
        assert!(fromisoformat("2026-13-01T00:00:00+00:00").is_none());
        assert!(fromisoformat("2026-10-10T25:00:00+00:00").is_none());
    }

    #[test]
    fn renders_python_isoformat_shapes() {
        let epoch = 1_760_097_600_000_000i64; // 2025-10-10T12:00:00Z
        assert_eq!(isoformat(epoch), "2025-10-10T12:00:00+00:00");
        assert_eq!(isoformat(epoch + 42), "2025-10-10T12:00:00.000042+00:00");
        assert_eq!(
            isoformat_seconds(epoch + 999_999),
            "2025-10-10T12:00:00+00:00"
        );
    }
}
