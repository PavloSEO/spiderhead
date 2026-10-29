//! `_parse_date`: ISO-8601 calendar parsing with Creference 3.14 semantics —
//! `datetime.fromisoformat` first (a permissive grammar: week dates, any
//! single-character separator, basic or colon time forms, `24:00` rollover,
//! offsets that may overflow into the next field), then the oracle's three
//! `strptime` fallbacks for the non-zero-padded forms.
//!
//! Naive values are read as UTC, exactly like the oracle's
//! `dt.replace(tzinfo=timezone.utc)` fallback.

/// A parsed instant: whole-day civil fields plus the epoch timestamp.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct IsoDate {
    /// Seconds since the Unix epoch (fractional when the text carried a
    /// fraction).
    pub epoch: f64,
    /// Civil year of `dt.date()` — the local date in the written offset,
    /// after the `T24:00` rollover when it applies.
    pub year: i64,
    /// Civil month, 1–12.
    pub month: i64,
    /// Civil day of month.
    pub day: i64,
}

impl IsoDate {
    /// `d.date().isoformat()`.
    pub fn date_iso(&self) -> String {
        format!("{:04}-{:02}-{:02}", self.year, self.month, self.day)
    }
}

fn is_leap(y: i64) -> bool {
    (y % 4 == 0 && y % 100 != 0) || y % 400 == 0
}

fn days_in_month(y: i64, m: i64) -> i64 {
    match m {
        1 | 3 | 5 | 7 | 8 | 10 | 12 => 31,
        4 | 6 | 9 | 11 => 30,
        2 => {
            if is_leap(y) {
                29
            } else {
                28
            }
        }
        _ => 0,
    }
}

/// Days from civil epoch (1970-01-01) — Howard Hinnant's algorithm.
fn days_from_civil(y: i64, m: i64, d: i64) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = if y >= 0 { y } else { y - 399 } / 400;
    let yoe = y - era * 400;
    let mp = (m + 9) % 12;
    let doy = (153 * mp + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146097 + doe - 719468
}

/// Civil date of the day containing `epoch` seconds (UTC floor).
pub fn civil_from_epoch(epoch: f64) -> (i64, i64, i64) {
    let days = (epoch / 86400.0).floor() as i64;
    // Inverse of days_from_civil.
    let z = days + 719468;
    let era = if z >= 0 { z } else { z - 146096 } / 146097;
    let doe = z - era * 146097;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    (if m <= 2 { y + 1 } else { y }, m, d)
}

/// `datetime.fromtimestamp(ts, utc).date().isoformat()`.
pub fn epoch_date_iso(epoch: f64) -> String {
    let (y, m, d) = civil_from_epoch(epoch);
    format!("{y:04}-{m:02}-{d:02}")
}

fn take_digits(text: &str, n: usize) -> Option<(i64, &str)> {
    let (head, tail) = text.split_at(text.len().min(n));
    if head.len() == n && head.bytes().all(|b| b.is_ascii_digit()) {
        head.parse::<i64>().ok().map(|v| (v, tail))
    } else {
        None
    }
}

/// `strptime`-style field: 1–2 digits, returned with the remainder.
fn take_digits_12(text: &str) -> Option<(i64, &str)> {
    let n = text.bytes().take_while(|b| b.is_ascii_digit()).count();
    if (1..=2).contains(&n) {
        let (head, tail) = text.split_at(n);
        head.parse::<i64>().ok().map(|v| (v, tail))
    } else {
        None
    }
}

/// ISO weekday of an epoch-day count: Monday = 1 … Sunday = 7
/// (day 0, 1970-01-01, was a Thursday).
fn iso_weekday(days: i64) -> i64 {
    (days.rem_euclid(7) + 3) % 7 + 1
}

/// `date.fromisocalendar(y, w, d)` — ISO week date to Gregorian, `None` when
/// the ISO year does not actually contain that week.
fn from_isocalendar(year: i64, week: i64, weekday: i64) -> Option<(i64, i64, i64)> {
    if !(1..=53).contains(&week) || !(1..=7).contains(&weekday) {
        return None;
    }
    // Jan 4 is always inside ISO week 1; Monday of week 1 = Jan4 - weekday+1.
    let jan4 = days_from_civil(year, 1, 4);
    let week1_monday = jan4 - (iso_weekday(jan4) - 1);
    let day = week1_monday + (week - 1) * 7 + (weekday - 1);
    let (cy, cm, cd) = civil_from_epoch(day as f64 * 86400.0);
    // Reject weeks that belong to a neighbouring ISO year (e.g. 2024-W53):
    // recompute the week's ordinal inside the resolved year.
    let back_jan4 = days_from_civil(cy, 1, 4);
    let back_w1 = back_jan4 - (iso_weekday(back_jan4) - 1);
    if cy != year || (day - back_w1) / 7 + 1 != week {
        return None;
    }
    Some((cy, cm, cd))
}

/// `.` or `,` fraction tail → fractional seconds, Creference pads/truncates to
/// microseconds. Returns `(frac, rest)`; `None` when there is no marker.
/// A marker with zero digits is allowed *only* before an offset (`12.+02:00`).
fn parse_frac(text: &str, offset_allowed: bool) -> Option<(f64, &str)> {
    let after = text.strip_prefix(['.', ','])?;
    let digits = after.bytes().take_while(|b| b.is_ascii_digit()).count();
    if digits == 0 {
        return if offset_allowed && after.starts_with(['+', '-']) {
            Some((0.0, after))
        } else {
            None
        };
    }
    let raw = &after[..digits.min(6)];
    let frac = raw.parse::<f64>().ok()? / 10f64.powi(raw.len() as i32);
    Some((frac, &after[digits..]))
}

/// Time-of-day after the separator. Returns `(hh, mm, ss+frac, rest)`.
/// Colon form needs 2-digit fields; basic form is `HH`, `HHMM` or `HHMMSS`.
/// The fraction is allowed after seconds only.
fn parse_time_iso(text: &str) -> Option<(i64, i64, f64, &str)> {
    let (hh, rest) = take_digits(text, 2)?;
    if let Some(after) = rest.strip_prefix(':') {
        let (mm, rest) = take_digits(after, 2)?;
        let mut ss = 0.0;
        let mut rest = rest;
        if let Some(after2) = rest.strip_prefix(':') {
            let (s, r) = take_digits(after2, 2)?;
            ss = s as f64;
            rest = r;
            if let Some((f, r2)) = parse_frac(rest, true) {
                ss += f;
                rest = r2;
            }
        }
        if hh > 24 || mm > 59 || ss >= 60.0 || (hh == 24 && (mm > 0 || ss > 0.0)) {
            return None;
        }
        Some((hh, mm, ss, rest))
    } else {
        // Basic form: HH alone, HHMM, or HHMMSS.
        let digits = rest.bytes().take_while(|b| b.is_ascii_digit()).count();
        let (mm, ss, mut rest) = match digits {
            0 => (0, 0.0, rest),
            2 => {
                let (v, r) = take_digits(rest, 2)?;
                (v, 0.0, r)
            }
            4 => {
                let (m, r) = take_digits(rest, 2)?;
                let (s, r2) = take_digits(r, 2)?;
                (m, s as f64, r2)
            }
            _ => return None,
        };
        let mut ss = ss;
        // A fraction is legal only after a seconds field.
        if digits == 4 {
            if let Some((f, r)) = parse_frac(rest, true) {
                ss += f;
                rest = r;
            }
        }
        if hh > 24 || mm > 59 || ss >= 60.0 || (hh == 24 && (mm > 0 || ss > 0.0)) {
            return None;
        }
        Some((hh, mm, ss, rest))
    }
}

/// Timezone tail `±HH[:MM[:SS[.frac]]]` / `±HH[MM[SS[.frac]]]` — offset
/// fields may overflow (mm/ss ≤ 99) but the total must stay under 24h.
fn parse_offset_iso(text: &str) -> Option<(f64, &str)> {
    let (sign, rest) = match text.strip_prefix('+') {
        Some(r) => (1.0, r),
        None => (-1.0, text.strip_prefix('-')?),
    };
    let (hh, rest) = take_digits(rest, 2)?;
    let mut mm = 0i64;
    let mut ss = 0.0;
    let mut rest = rest;
    if let Some(after) = rest.strip_prefix(':') {
        let (m, r) = take_digits(after, 2)?;
        mm = m;
        rest = r;
        if let Some(after2) = rest.strip_prefix(':') {
            let (s, r2) = take_digits(after2, 2)?;
            ss = s as f64;
            rest = r2;
            if let Some((f, r3)) = parse_frac(rest, false) {
                ss += f;
                rest = r3;
            }
        }
    } else {
        let digits = rest.bytes().take_while(|b| b.is_ascii_digit()).count();
        match digits {
            0 => {}
            2 => {
                let (m, r) = take_digits(rest, 2)?;
                mm = m;
                rest = r;
            }
            4 => {
                let (m, r) = take_digits(rest, 2)?;
                let (s, r2) = take_digits(r, 2)?;
                mm = m;
                ss = s as f64;
                rest = r2;
                if let Some((f, r3)) = parse_frac(rest, false) {
                    ss += f;
                    rest = r3;
                }
            }
            _ => return None,
        }
    }
    if mm > 99 || ss >= 100.0 {
        return None;
    }
    let total = hh as f64 * 3600.0 + mm as f64 * 60.0 + ss;
    // Creference requires the offset to be strictly less than 24h.
    if total >= 86400.0 {
        return None;
    }
    Some((sign * total, rest))
}

/// The `datetime.fromisoformat` layer (Creference 3.14). Returns
/// `(epoch_seconds, year, month, day)` of `dt.date()`.
fn fromisoformat(text: &str) -> Option<IsoDate> {
    let (year, rest) = take_digits(text, 4)?;
    // Date: `YYYY-Www[-D]` | `YYYYWww[D]` | `YYYY-MM-DD` | `YYYYMMDD`.
    let (days, mut rest) = if let Some(w) = rest.strip_prefix('-') {
        if let Some(w) = w.strip_prefix(['W', 'w']) {
            let (week, r) = take_digits(w, 2)?;
            let (wd, r2) = match r.strip_prefix('-') {
                Some(d) => take_digits(d, 1)?,
                None => (1, r),
            };
            (iso_days(year, week, wd)?, r2)
        } else {
            let (month, r) = take_digits(w, 2)?;
            let r = r.strip_prefix('-')?;
            let (day, r2) = take_digits(r, 2)?;
            if days_in_month(year, month) < day || day < 1 {
                return None;
            }
            (days_from_civil(year, month, day), r2)
        }
    } else if rest.starts_with(['W', 'w']) {
        let (week, r) = take_digits(&rest[1..], 2)?;
        let (wd, r2) = if !r.is_empty() && r.as_bytes()[0].is_ascii_digit() {
            take_digits(r, 1)?
        } else {
            (1, r)
        };
        (iso_days(year, week, wd)?, r2)
    } else {
        let (month, r) = take_digits(rest, 2)?;
        let (day, r2) = take_digits(r, 2)?;
        if days_in_month(year, month) < day || day < 1 {
            return None;
        }
        (days_from_civil(year, month, day), r2)
    };
    let mut days = days;
    let mut seconds = 0.0;
    let mut offset = 0.0;
    if !rest.is_empty() {
        // Any single character separates the date from the time.
        let sep = rest.chars().next()?;
        rest = &rest[sep.len_utf8()..];
        let (hh, mm, ss, r) = parse_time_iso(rest)?;
        rest = r;
        if hh == 24 {
            days += 1; // `24:00` normalises to the next day's midnight
        }
        seconds = hh as f64 * 3600.0 + mm as f64 * 60.0 + ss;
        if seconds >= 86400.0 {
            // Only an all-zero tail survives the rollover check above.
            seconds -= 86400.0;
        }
        if !rest.is_empty() {
            let (off, r) = parse_offset_iso(rest)?;
            offset = off;
            rest = r;
            if !rest.is_empty() {
                return None;
            }
        }
    }
    let (y, m, d) = civil_from_epoch(days as f64 * 86400.0);
    Some(IsoDate {
        epoch: days as f64 * 86400.0 + seconds - offset,
        year: y,
        month: m,
        day: d,
    })
}

fn iso_days(year: i64, week: i64, weekday: i64) -> Option<i64> {
    let (y, m, d) = from_isocalendar(year, week, weekday)?;
    Some(days_from_civil(y, m, d))
}

/// The `strptime` `%z` tail: `±HHMM`, `±HH:MM`, `±HHMMSS`, `±HH:MM:SS`
/// (optionally with a `.`/`,` fraction of 1–6 digits after seconds).
fn parse_offset_z(text: &str) -> Option<(f64, &str)> {
    let (sign, rest) = match text.strip_prefix('+') {
        Some(r) => (1.0, r),
        None => (-1.0, text.strip_prefix('-')?),
    };
    let (hh, rest) = take_digits(rest, 2)?;
    let (mm, ss, rest) = if let Some(after) = rest.strip_prefix(':') {
        let (m, mut rest) = take_digits(after, 2)?;
        let mut ss = 0.0;
        if let Some(after2) = rest.strip_prefix(':') {
            let (s, r2) = take_digits(after2, 2)?;
            ss = s as f64;
            rest = r2;
            rest = frac_z(rest, &mut ss)?;
        }
        (m, ss, rest)
    } else {
        let digits = rest.bytes().take_while(|b| b.is_ascii_digit()).count();
        match digits {
            2 => {
                let (m, r) = take_digits(rest, 2)?;
                (m, 0.0, r)
            }
            4 => {
                let (m, r) = take_digits(rest, 2)?;
                let (s, mut r2) = take_digits(r, 2)?;
                let mut ss = s as f64;
                r2 = frac_z(r2, &mut ss)?;
                (m, ss, r2)
            }
            _ => return None, // `%z` needs at least HHMM
        }
    };
    if mm > 59 || ss >= 60.0 {
        return None;
    }
    let total = hh as f64 * 3600.0 + mm as f64 * 60.0 + ss;
    if total >= 86400.0 {
        return None;
    }
    Some((sign * total, rest))
}

/// `%z` fraction: 1–6 digits exactly (strptime does not truncate).
fn frac_z<'a>(rest: &'a str, ss: &mut f64) -> Option<&'a str> {
    let Some(after) = rest.strip_prefix(['.', ',']) else {
        return Some(rest);
    };
    let digits = after.bytes().take_while(|b| b.is_ascii_digit()).count();
    if !(1..=6).contains(&digits)
        || after.as_bytes()[digits..]
            .first()
            .is_some_and(|b| b.is_ascii_digit())
    {
        return None;
    }
    let raw = &after[..digits];
    *ss += raw.parse::<f64>().ok()? / 10f64.powi(raw.len() as i32);
    Some(&after[digits..])
}

/// The `strptime` fallbacks: `%Y-%m-%d`, `%Y-%m-%dT%H:%M:%S`,
/// `%Y-%m-%dT%H:%M:%S%z` — 4-digit year, 1–2-digit everything else.
fn from_strptime(text: &str) -> Option<IsoDate> {
    let (year, rest) = take_digits(text, 4)?;
    let rest = rest.strip_prefix('-')?;
    let (month, rest) = take_digits_12(rest)?;
    let rest = rest.strip_prefix('-')?;
    let (day, rest) = take_digits_12(rest)?;
    if days_in_month(year, month) < day || day < 1 {
        return None;
    }
    let days = days_from_civil(year, month, day);
    if rest.is_empty() {
        let (y, m, d) = civil_from_epoch(days as f64 * 86400.0);
        return Some(IsoDate {
            epoch: days as f64 * 86400.0,
            year: y,
            month: m,
            day: d,
        });
    }
    let rest = rest.strip_prefix('T')?;
    let (hh, rest) = take_digits_12(rest)?;
    let rest = rest.strip_prefix(':')?;
    let (mm, rest) = take_digits_12(rest)?;
    let rest = rest.strip_prefix(':')?;
    let (ss, mut rest) = take_digits_12(rest)?;
    if hh > 23 || mm > 59 || ss > 59 {
        return None;
    }
    let mut offset = 0.0;
    if !rest.is_empty() {
        let (off, r) = parse_offset_z(rest)?;
        offset = off;
        rest = r;
        if !rest.is_empty() {
            return None;
        }
    }
    let (y, m, d) = civil_from_epoch(days as f64 * 86400.0);
    Some(IsoDate {
        epoch: days as f64 * 86400.0 + hh as f64 * 3600.0 + mm as f64 * 60.0 + ss as f64 - offset,
        year: y,
        month: m,
        day: d,
    })
}

/// `_parse_date`: `fromisoformat` for the accepted subset, else the
/// oracle's three `strptime` formats, else `None`.
pub fn parse_date(value: Option<&str>) -> Option<IsoDate> {
    let text = value?.trim();
    if text.is_empty() {
        return None;
    }
    // The oracle replaces every 'Z' before parsing, mid-string included.
    let replaced = text.replace('Z', "+00:00");
    fromisoformat(&replaced).or_else(|| from_strptime(&replaced))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn date_parser_accepts_every_w3c_lastmod_precision() {
        for value in [
            "2024-01-01",
            "2024-01-01T10:11:12",
            "2024-01-01T10:11:12Z",
            "2024-01-01T10:11:12+02:00",
        ] {
            assert!(parse_date(Some(value)).is_some(), "{value}");
        }
        assert!(parse_date(Some("not a date at all")).is_none());
        assert!(parse_date(None).is_none());
        assert!(parse_date(Some("")).is_none());
    }

    #[test]
    fn epoch_of_plain_date_is_midnight_utc() {
        let d = parse_date(Some("2024-01-01")).expect("parses");
        assert_eq!(d.epoch, 1704067200.0);
        assert_eq!(d.date_iso(), "2024-01-01");
    }

    #[test]
    fn offset_shifts_the_epoch() {
        let d = parse_date(Some("2024-01-01T10:11:12+02:00")).expect("parses");
        let z = parse_date(Some("2024-01-01T08:11:12Z")).expect("parses");
        assert_eq!(d.epoch, z.epoch);
    }

    #[test]
    fn space_separator_and_comma_fraction() {
        assert!(parse_date(Some("2024-01-01 10:11:12")).is_some());
        assert!(parse_date(Some("2024-01-01T10:11:12,5")).is_some());
    }

    #[test]
    fn invalid_dates_are_refused() {
        assert!(parse_date(Some("2024-02-30")).is_none());
        assert!(parse_date(Some("2024-13-01")).is_none());
        assert!(parse_date(Some("2024-01-01T25:00:00")).is_none());
    }
}
