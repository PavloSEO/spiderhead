//! Civil-date helpers shared by recon modules, without a calendar crate dependency.
//! Covers day counts, ISO rendering and the date formats RDAP and WHOIS use.

use std::sync::LazyLock;

use regex::Regex;
use serde_json::Value as Json;

/// Days since the Unix epoch for a proleptic Gregorian date
/// (Howard Hinnant's civil algorithm; `None` when the date is invalid).
pub(crate) fn days_from_civil(y: i64, m: i64, d: i64) -> Option<i64> {
    if !(1..=12).contains(&m) {
        return None;
    }
    let month_days = [31, 28, 31, 30, 31, 30, 31, 31, 30, 31, 30, 31];
    let is_leap = y % 4 == 0 && (y % 100 != 0 || y % 400 == 0);
    let max_day = month_days[(m - 1) as usize] as i64 + i64::from(m == 2 && is_leap);
    if !(1..=max_day).contains(&d) {
        return None;
    }
    let y = if m <= 2 { y - 1 } else { y };
    let era = if y >= 0 { y } else { y - 399 } / 400;
    let yoe = y - era * 400;
    let mp = (m + 9) % 12;
    let doy = (153 * mp + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    Some(era * 146097 + doe - 719468)
}

/// ISO `YYYY-MM-DD` rendering of a day count (inverse of [`days_from_civil`]).
pub(crate) fn civil_from_days(days: i64) -> String {
    let z = days + 719468;
    let era = if z >= 0 { z } else { z - 146096 } / 146097;
    let doe = z - era * 146097;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    format!("{:04}-{:02}-{:02}", if m <= 2 { y + 1 } else { y }, m, d)
}

/// Days since the Unix epoch for today (UTC).
pub(crate) fn today_days() -> i64 {
    let secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    secs as i64 / 86_400
}

/// `None` when the day is unknown, else `today - day` in days.
pub(crate) fn days_from(day: Option<i64>) -> Option<i64> {
    day.map(|d| today_days() - d)
}

const MONTHS: &[(&str, i64)] = &[
    ("jan", 1),
    ("feb", 2),
    ("mar", 3),
    ("apr", 4),
    ("may", 5),
    ("jun", 6),
    ("jul", 7),
    ("aug", 8),
    ("sep", 9),
    ("oct", 10),
    ("nov", 11),
    ("dec", 12),
];

fn month_abbrev(value: &str) -> Option<i64> {
    let low = value.to_lowercase();
    MONTHS
        .iter()
        .find(|(name, _)| low.starts_with(name))
        .map(|(_, m)| *m)
}

/// Parse an RDAP or WHOIS date into days since epoch. ISO comes first, then
/// `%d.%m.%Y`, `%Y.%m.%d`, `%d-%b-%Y` and `%Y/%m/%d` on the trimmed 11-character head.
pub(crate) fn parse_date(value: &str) -> Option<i64> {
    let raw = value.trim();
    if raw.is_empty() {
        return None;
    }
    static ISO: LazyLock<Regex> = LazyLock::new(|| {
        Regex::new(r"^(\d{4})-(\d{2})-(\d{2})(?:[T ].*)?$").unwrap_or_else(|e| panic!("{e}"))
    });
    if let Some(m) = ISO.captures(raw) {
        if let Some(day) =
            days_from_civil(m[1].parse().ok()?, m[2].parse().ok()?, m[3].parse().ok()?)
        {
            return Some(day);
        }
    }
    let head = raw[..raw.len().min(11)].trim();
    static NUMERIC: LazyLock<Regex> = LazyLock::new(|| {
        Regex::new(r"^(\d{1,4})[./-](\d{1,2})[./-](\d{1,4})$").unwrap_or_else(|e| panic!("{e}"))
    });
    static TEXT: LazyLock<Regex> = LazyLock::new(|| {
        Regex::new(r"(?i)^(\d{1,2})-([a-z]{3})-(\d{4})$").unwrap_or_else(|e| panic!("{e}"))
    });
    if let Some(m) = NUMERIC.captures(head) {
        let (a, b, c): (i64, i64, i64) =
            (m[1].parse().ok()?, m[2].parse().ok()?, m[3].parse().ok()?);
        if m[1].len() == 4 {
            return days_from_civil(a, b, c);
        }
        return days_from_civil(c, b, a);
    }
    if let Some(m) = TEXT.captures(head) {
        return days_from_civil(m[3].parse().ok()?, month_abbrev(&m[2])?, m[1].parse().ok()?);
    }
    None
}

/// Re-render an offset-aware ISO datetime as UTC with microseconds.
/// `None` when the stamp has no offset.
pub(crate) fn normalize_observed_at(stamp: &str) -> Option<String> {
    static RE: LazyLock<Regex> = LazyLock::new(|| {
        Regex::new(
            r"(?i)^(\d{4})-(\d{2})-(\d{2})[t ](\d{2}):(\d{2})(?::(\d{2})(?:\.(\d{1,6}))?)?(z|[+-]\d{2}:?\d{2})$",
        )
        .unwrap_or_else(|e| panic!("{e}"))
    });
    let m = RE.captures(stamp.trim())?;
    let days = days_from_civil(m[1].parse().ok()?, m[2].parse().ok()?, m[3].parse().ok()?)?;
    let h: i64 = m[4].parse().ok()?;
    let mi: i64 = m[5].parse().ok()?;
    let s: i64 = m.get(6).map(|v| v.as_str()).unwrap_or("0").parse().ok()?;
    let mut micros = m.get(7).map(|v| v.as_str().to_string()).unwrap_or_default();
    while micros.len() < 6 {
        micros.push('0');
    }
    let frac: i64 = micros.parse().ok()?;
    let offset = m[8].to_lowercase();
    let offset_secs = if offset == "z" {
        0
    } else {
        let sign = if offset.starts_with('-') { -1 } else { 1 };
        let digits: String = offset
            .chars()
            .skip(1)
            .filter(|c| c.is_ascii_digit())
            .collect();
        let hh: i64 = digits[..digits.len() - 2].parse().ok()?;
        let mm: i64 = digits[digits.len() - 2..].parse().ok()?;
        sign * (hh * 3600 + mm * 60)
    };
    let micros_total = (days * 86_400 + h * 3600 + mi * 60 + s - offset_secs) * 1_000_000 + frac;
    let secs = micros_total.div_euclid(1_000_000);
    let frac = micros_total.rem_euclid(1_000_000);
    let days = secs.div_euclid(86_400);
    let tod = secs.rem_euclid(86_400);
    Some(format!(
        "{}T{:02}:{:02}:{:02}.{:06}+00:00",
        civil_from_days(days),
        tod / 3600,
        (tod % 3600) / 60,
        tod % 60,
        frac
    ))
}

/// Parse the fixed certificate date format, e.g. `Jun  1 12:00:00 2027 GMT`.
pub(crate) fn parse_cert_date(value: &str) -> Option<i64> {
    static RE: LazyLock<Regex> = LazyLock::new(|| {
        Regex::new(r"(?i)^([a-z]{3})\s+(\d{1,2})\s+\d{2}:\d{2}:\d{2}\s+(\d{4})\s+\w+$")
            .unwrap_or_else(|e| panic!("{e}"))
    });
    let m = RE.captures(value.trim())?;
    days_from_civil(m[3].parse().ok()?, month_abbrev(&m[1])?, m[2].parse().ok()?)
}

/// Normalize a date string to ISO, accepting ISO dates and datetimes and `DD/MM/YYYY`.
/// `Err` names the accepted formats.
pub(crate) fn iso_date(value: &Json) -> Result<Option<String>, String> {
    let raw = match value {
        Json::Null => return Ok(None),
        Json::String(s) => s.trim().to_string(),
        other => other.to_string().trim_matches('"').trim().to_string(),
    };
    if raw.is_empty() || matches!(raw.to_lowercase().as_str(), "-" | "n/a" | "null") {
        return Ok(None);
    }
    // ISO date or datetime, with an optional offset.
    static ISO: LazyLock<Regex> = LazyLock::new(|| {
        Regex::new(r"^(\d{4})-(\d{2})-(\d{2})(?:[T ].*)?$").unwrap_or_else(|e| panic!("{e}"))
    });
    if let Some(m) = ISO.captures(&raw) {
        if let Some(day) = days_from_civil(
            m[1].parse().ok().unwrap_or(0),
            m[2].parse().ok().unwrap_or(0),
            m[3].parse().ok().unwrap_or(0),
        ) {
            return Ok(Some(civil_from_days(day)));
        }
        return Err("link dates must be ISO dates or DD/MM/YYYY".to_string());
    }
    static EU: LazyLock<Regex> = LazyLock::new(|| {
        Regex::new(r"^(\d{1,2})/(\d{1,2})/(\d{4})$").unwrap_or_else(|e| panic!("{e}"))
    });
    if let Some(m) = EU.captures(&raw) {
        if let Some(day) = days_from_civil(
            m[3].parse().ok().unwrap_or(0),
            m[2].parse().ok().unwrap_or(0),
            m[1].parse().ok().unwrap_or(0),
        ) {
            return Ok(Some(civil_from_days(day)));
        }
    }
    Err("link dates must be ISO dates or DD/MM/YYYY".to_string())
}
