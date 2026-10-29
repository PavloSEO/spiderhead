//! `datetime.fromisoformat` parsing for `run.generated_at`: the full Creference
//! grammar (compact and week dates, `HH`-only times, colon-less offsets, hour 24).

use super::*;

pub(crate) fn generated_at(document: &Object) -> Option<i64> {
    let run = get_mapping(document, "run")?;
    let value = run.get("generated_at")?.as_str()?;
    parse_iso_micros(&value.replace('Z', "+00:00"))
}

/// Days from civil date (Howard Hinnant's algorithm).
fn days_from_civil(y: i64, m: i64, d: i64) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = if y >= 0 { y } else { y - 399 } / 400;
    let yoe = y - era * 400;
    let doy = (153 * (if m > 2 { m - 3 } else { m + 9 }) + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

/// Proleptic-Gregorian leap year.
fn is_leap(y: i64) -> bool {
    (y % 4 == 0 && y % 100 != 0) || y % 400 == 0
}

/// Days in a Gregorian month.
fn days_in_month(y: i64, m: i64) -> i64 {
    match m {
        1 | 3 | 5 | 7 | 8 | 10 | 12 => 31,
        4 | 6 | 9 | 11 => 30,
        2 if is_leap(y) => 29,
        2 => 28,
        _ => 0,
    }
}

/// Weekday of an epoch day, `0 = Monday .. 6 = Sunday` (1970-01-01 was a Thursday).
fn weekday(days: i64) -> i64 {
    (days + 3).rem_euclid(7)
}

/// `n` ASCII digits at `pos`, or `None`.
fn digits(b: &[u8], pos: usize, n: usize) -> Option<i64> {
    if pos + n > b.len() || !b[pos..pos + n].iter().all(|c| c.is_ascii_digit()) {
        return None;
    }
    b[pos..pos + n].iter().try_fold(0, |acc: i64, c| {
        acc.checked_mul(10).map(|a| a + i64::from(c - b'0'))
    })
}

/// ISO-8601 week date to epoch day; `day` is 1..=7 (Monday=1), `week` 1..=52/53.
fn iso_week_to_days(year: i64, week: i64, day: i64) -> Option<i64> {
    if !(1..=7).contains(&day) || !(1..=53).contains(&week) {
        return None;
    }
    let jan1_wd = weekday(days_from_civil(year, 1, 1));
    let has_53 = jan1_wd == 3 || (is_leap(year) && jan1_wd == 2);
    if week == 53 && !has_53 {
        return None;
    }
    let jan4 = days_from_civil(year, 1, 4);
    Some(jan4 - weekday(jan4) + (week - 1) * 7 + (day - 1))
}

/// Fraction digits after `.`/`,` at `i`: at least one, value truncated/padded to 6.
fn parse_frac(b: &[u8], i: &mut usize) -> Option<i64> {
    let start = *i;
    while *i < b.len() && b[*i].is_ascii_digit() {
        *i += 1;
    }
    let n = (*i - start).min(6);
    if n == 0 {
        return None;
    }
    let value = digits(b, start, n)?;
    Some(value * 10_i64.pow(6 - n as u32))
}

/// `datetime.fromisoformat` (Creference 3.11+ grammar): `YYYY-MM-DD`, `YYYYMMDD`,
/// `YYYY-Www[-D]` and `YYYYWww[D]` dates; any single-char separator; times `HH`,
/// `HHMM`, `HHMMSS`, `HH:MM[:SS]` with a trailing `:` allowed after `HH`/`MM`;
/// fractions `[.,]d+` only on seconds; offsets `±HH[MM[SS[.f]]]`/`±HH:MM[:SS[.f]]`
/// normalized to a total strictly below 24 h. `None` for naive or invalid input.
fn parse_iso_micros(text: &str) -> Option<i64> {
    let b = text.as_bytes();
    let year = digits(b, 0, 4)?;
    if !(1..=9999).contains(&year) {
        return None;
    }
    let (days, mut i) = match b.get(4) {
        Some(b'-') => {
            if b.get(5) == Some(&b'W') {
                let week = digits(b, 6, 2)?;
                let (day, i) = if b.get(8) == Some(&b'-') {
                    (digits(b, 9, 1)?, 10)
                } else {
                    (1, 8)
                };
                (iso_week_to_days(year, week, day)?, i)
            } else {
                let month = digits(b, 5, 2)?;
                if b.get(7) != Some(&b'-') {
                    return None;
                }
                let day = digits(b, 8, 2)?;
                if !(1..=12).contains(&month) || !(1..=days_in_month(year, month)).contains(&day) {
                    return None;
                }
                (days_from_civil(year, month, day), 10)
            }
        }
        Some(b'W') => {
            let week = digits(b, 5, 2)?;
            let (day, i) = if b.get(7).is_some_and(|c| c.is_ascii_digit()) {
                (digits(b, 7, 1)?, 8)
            } else {
                (1, 7)
            };
            (iso_week_to_days(year, week, day)?, i)
        }
        _ => {
            let month = digits(b, 4, 2)?;
            let day = digits(b, 6, 2)?;
            if !(1..=12).contains(&month) || !(1..=days_in_month(year, month)).contains(&day) {
                return None;
            }
            (days_from_civil(year, month, day), 8)
        }
    };
    if i == b.len() {
        return None; // date only: naive
    }
    i += 1; // any single separator
    let hour = digits(b, i, 2)?;
    i += 2;
    let (mut minute, mut second, mut micros) = (0, 0, 0);
    if b.get(i) == Some(&b':') {
        i += 1;
        if let Some(m) = digits(b, i, 2) {
            minute = m;
            i += 2;
            if b.get(i) == Some(&b':') {
                i += 1;
                if let Some(s) = digits(b, i, 2) {
                    second = s;
                    i += 2;
                    if matches!(b.get(i), Some(b'.') | Some(b',')) {
                        i += 1;
                        micros = parse_frac(b, &mut i)?;
                    }
                }
            }
        }
    } else if let Some(m) = digits(b, i, 2) {
        minute = m;
        i += 2;
        if let Some(s) = digits(b, i, 2) {
            second = s;
            i += 2;
            if matches!(b.get(i), Some(b'.') | Some(b',')) {
                i += 1;
                micros = parse_frac(b, &mut i)?;
            }
        }
    }
    if hour > 24 || minute > 59 || second > 59 || (hour == 24 && (minute | second) + micros > 0) {
        return None;
    }
    if i == b.len() {
        return None; // naive
    }
    let sign = match b[i] {
        b'+' => 1_i64,
        b'-' => -1_i64,
        _ => return None,
    };
    i += 1;
    let oh = digits(b, i, 2)?;
    i += 2;
    let (mut om, mut osec, mut omicros) = (0_i64, 0_i64, 0_i64);
    if b.get(i) == Some(&b':') {
        i += 1;
        if let Some(m) = digits(b, i, 2) {
            om = m;
            i += 2;
            if b.get(i) == Some(&b':') {
                i += 1;
                if let Some(s) = digits(b, i, 2) {
                    osec = s;
                    i += 2;
                    if matches!(b.get(i), Some(b'.') | Some(b',')) {
                        i += 1;
                        omicros = parse_frac(b, &mut i)?;
                    }
                }
            }
        }
    } else if let Some(m) = digits(b, i, 2) {
        om = m;
        i += 2;
        if let Some(s) = digits(b, i, 2) {
            osec = s;
            i += 2;
            if matches!(b.get(i), Some(b'.') | Some(b',')) {
                i += 1;
                omicros = parse_frac(b, &mut i)?;
            }
        }
    }
    if i != b.len() {
        return None;
    }
    let offset = sign * (oh * 3600 + om * 60 + osec) * 1_000_000 + sign * omicros;
    if offset.unsigned_abs() >= 86_400 * 1_000_000 {
        return None;
    }
    Some((days * 86_400 + hour * 3600 + minute * 60 + second) * 1_000_000 + micros - offset)
}
