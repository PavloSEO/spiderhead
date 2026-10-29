//! `date`, `datetime` and `time` ISO 8601 validity; basic and week formats are accepted.
//! Only the yes/no answer is needed.

fn digits(b: &[u8], at: usize, n: usize) -> Option<u32> {
    let part = b.get(at..at + n)?;
    if part.iter().all(u8::is_ascii_digit) {
        Some(part.iter().fold(0, |acc, d| acc * 10 + u32::from(d - b'0')))
    } else {
        None
    }
}

fn is_leap(year: u32) -> bool {
    (year.is_multiple_of(4) && !year.is_multiple_of(100)) || year.is_multiple_of(400)
}

fn days_in_month(year: u32, month: u32) -> u32 {
    match month {
        2 => 28 + u32::from(is_leap(year)),
        4 | 6 | 9 | 11 => 30,
        _ => 31,
    }
}

/// Weekday of Jan 1st (Monday = 0), proleptic Gregorian.
fn first_weekday(year: u32) -> u32 {
    let y = year - 1;
    (y * 365 + y / 4 - y / 100 + y / 400) % 7
}

/// True when `text` is a valid ISO 8601 date.
pub(crate) fn date_ok(text: &str) -> bool {
    let b = text.as_bytes();
    matches!(b.len(), 7 | 8 | 10) && date_part(b).is_some()
}

fn date_part(b: &[u8]) -> Option<()> {
    let year = digits(b, 0, 4)?;
    let mut p = 4;
    let sep = b.get(p) == Some(&b'-');
    if sep {
        p += 1;
    }
    if year == 0 {
        return None;
    }
    if b.get(p) == Some(&b'W') {
        p += 1;
        let week = digits(b, p, 2)?;
        p += 2;
        let day = if p < b.len() {
            if sep {
                if b.get(p) != Some(&b'-') {
                    return None;
                }
                p += 1;
            }
            digits(b, p, 1)?
        } else {
            1
        };
        if week == 0 || week >= 53 {
            let long_year = first_weekday(year) == 3 || (first_weekday(year) == 2 && is_leap(year));
            if week != 53 || !long_year {
                return None;
            }
        }
        if day == 0 || day >= 8 {
            return None;
        }
        return Some(());
    }
    let month = digits(b, p, 2)?;
    p += 2;
    if sep {
        if b.get(p) != Some(&b'-') {
            return None;
        }
        p += 1;
    }
    let day = digits(b, p, 2)?;
    ((1..=12).contains(&month) && day >= 1 && day <= days_in_month(year, month)).then_some(())
}

/// Parse `HH[:MM[:SS[.f]]]` (or the basic form); returns the offset reached and whether the
/// clock fields are in range.
fn clock(b: &[u8], end: usize) -> Option<()> {
    let mut p = 0;
    let hour = digits(b, p, 2)?;
    p += 2;
    let separated = b.get(p) == Some(&b':');
    let mut minute = 0;
    let mut second = 0;
    let mut fraction_nonzero = false;
    if p < end {
        if separated {
            p += 1;
        }
        if p < end && b[p] != b'.' && b[p] != b',' {
            minute = digits(b, p, 2)?;
            p += 2;
            if p < end && b[p] != b'.' && b[p] != b',' {
                if separated {
                    if b.get(p) != Some(&b':') {
                        return None;
                    }
                    p += 1;
                }
                second = digits(b, p, 2)?;
                p += 2;
            }
        }
        if p < end && (b[p] == b'.' || b[p] == b',') {
            p += 1;
            let frac = p;
            while p < end && b[p].is_ascii_digit() {
                fraction_nonzero |= b[p] != b'0';
                p += 1;
            }
            if p == frac {
                return None;
            }
        }
    }
    // `24:00:00` is accepted and means midnight at the end of the day.
    let hour_ok = hour <= 23 || (hour == 24 && minute == 0 && second == 0 && !fraction_nonzero);
    if p != end || !hour_ok || minute > 59 || second > 59 {
        return None;
    }
    Some(())
}

/// Time of day with an optional `Z` / `±HH[:MM[:SS[.f]]]` suffix.
fn time_with_zone(b: &[u8]) -> bool {
    let zone = b.iter().position(|c| matches!(c, b'Z' | b'+' | b'-'));
    let clock_end = zone.unwrap_or(b.len());
    if clock(b, clock_end).is_none() {
        return false;
    }
    let Some(at) = zone else {
        return true;
    };
    if b[at] == b'Z' {
        return at + 1 == b.len();
    }
    let rest = &b[at + 1..];
    match clock(rest, rest.len()) {
        Some(()) => {
            let hour = digits(rest, 0, 2).unwrap_or(99);
            hour < 24
        }
        None => false,
    }
}

/// True when `text` is a valid ISO 8601 time.
pub(crate) fn time_ok(text: &str) -> bool {
    let b = text.strip_prefix('T').unwrap_or(text).as_bytes();
    time_with_zone(b)
}

/// True when `text` is a valid ISO 8601 datetime.
pub(crate) fn datetime_ok(text: &str) -> bool {
    let b = text.as_bytes();
    let len = b.len();
    if len < 7 {
        return false;
    }
    let separator = if len == 7 {
        7
    } else if b[4] == b'-' {
        if b.get(5) == Some(&b'W') {
            if len < 8 {
                return false;
            }
            if len > 8 && b[8] == b'-' {
                if len == 9 {
                    return false;
                }
                if len > 10 && b[10].is_ascii_digit() {
                    8
                } else {
                    10
                }
            } else {
                8
            }
        } else {
            10
        }
    } else {
        let mut idx = 7;
        if b[4] != b'W' || b.get(idx).is_some_and(u8::is_ascii_digit) {
            idx = 8;
        }
        idx
    };
    if separator > len || !text.is_char_boundary(separator) {
        return false;
    }
    if !matches!(separator, 7 | 8 | 10) || date_part(&b[..separator]).is_none() {
        return false;
    }
    if len == separator {
        return true;
    }
    let rest = &text[separator..];
    let mut chars = rest.chars();
    chars.next();
    time_with_zone(chars.as_str().as_bytes())
}
