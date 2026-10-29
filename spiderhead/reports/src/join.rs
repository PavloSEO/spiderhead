//! URL join keys and a minimal ISO-date type shared by cohort/BI projections.
//!
//! `normalize_url` ports `seohead.checks.sitemap.normalize_url` (host
//! lower-cased, default ports dropped, fragment removed, trailing slashes
//! stripped); `normalize_join_key` ports `checks.external_join`'s key on top.

/// Minimal ISO calendar date (`datetime.date` semantics needed here).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct IsoDate {
    /// Ordinal day count (proleptic Gregorian).
    days: i64,
}

impl IsoDate {
    /// `date.fromisoformat`: accepts exactly `YYYY-MM-DD`.
    pub fn parse(value: &str) -> Option<IsoDate> {
        let bytes = value.as_bytes();
        if bytes.len() != 10 || bytes[4] != b'-' || bytes[7] != b'-' {
            return None;
        }
        let year: i64 = value[0..4].parse().ok()?;
        let month: i64 = value[5..7].parse().ok()?;
        let day: i64 = value[8..10].parse().ok()?;
        if !(1..=12).contains(&month) {
            return None;
        }
        let leap = (year % 4 == 0 && year % 100 != 0) || year % 400 == 0;
        let dim = [
            31,
            if leap { 29 } else { 28 },
            31,
            30,
            31,
            30,
            31,
            31,
            30,
            31,
            30,
            31,
        ][(month - 1) as usize];
        if !(1..=dim).contains(&day) {
            return None;
        }
        Some(IsoDate {
            days: days_from_civil(year, month, day),
        })
    }

    /// `date.isoformat()` -> `YYYY-MM-DD`.
    pub fn isoformat(&self) -> String {
        let (y, m, d) = civil_from_days(self.days);
        format!("{y:04}-{m:02}-{d:02}")
    }

    /// `(a - b).days`.
    pub fn diff_days(&self, other: &IsoDate) -> i64 {
        self.days - other.days
    }

    /// Raw day count used for range comparisons.
    pub fn days(&self) -> i64 {
        self.days
    }

    /// `date + timedelta(days=n)`.
    pub fn add_days(&self, n: i64) -> IsoDate {
        IsoDate {
            days: self.days + n,
        }
    }

    /// `value[:7]` — the year-month bucket used for publication cohorts.
    pub fn month_bucket(&self) -> String {
        self.isoformat()[..7].to_owned()
    }

    /// `date.year`.
    pub fn year(&self) -> i64 {
        civil_from_days(self.days).0
    }

    /// `date.month` (1-12).
    pub fn month(&self) -> i64 {
        civil_from_days(self.days).1
    }

    /// `date.day` (1-31).
    pub fn day(&self) -> i64 {
        civil_from_days(self.days).2
    }
}

/// Howard Hinnant's days-from-civil (proleptic Gregorian epoch at 1970-01-01).
fn days_from_civil(y: i64, m: i64, d: i64) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = y.div_euclid(400);
    let yoe = y - era * 400;
    let mp = (m + 9) % 12;
    let doy = (153 * mp + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

fn civil_from_days(z: i64) -> (i64, i64, i64) {
    let z = z + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    (if m <= 2 { y + 1 } else { y }, m, d)
}

/// `urllib.parse.urlsplit` — a light structural split with no normalization.
pub fn urlsplit(url: &str) -> (String, String, String, String, String) {
    let (rest, fragment) = match url.find('#') {
        Some(i) => (&url[..i], &url[i + 1..]),
        None => (url, ""),
    };
    let (rest, query) = match rest.find('?') {
        Some(i) => (&rest[..i], &rest[i + 1..]),
        None => (rest, ""),
    };
    let mut scheme = String::new();
    let mut rest = rest;
    // scheme = [a-zA-Z][a-zA-Z0-9+.-]* before the first '/', '?', '#'
    if let Some(colon) = rest.find(':') {
        let prefix = &rest[..colon];
        let first = rest.find(['/', '?', '#']).unwrap_or(rest.len());
        if colon < first
            && !prefix.is_empty()
            && prefix
                .chars()
                .next()
                .is_some_and(|c| c.is_ascii_alphabetic())
            && prefix
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || matches!(c, '+' | '-' | '.'))
        {
            scheme = prefix.to_owned();
            rest = &rest[colon + 1..];
        }
    }
    let mut netloc = String::new();
    if let Some(stripped) = rest.strip_prefix("//") {
        let end = stripped.find('/').unwrap_or(stripped.len());
        netloc = stripped[..end].to_owned();
        rest = &stripped[end..];
    }
    (
        scheme,
        netloc,
        rest.to_owned(),
        query.to_owned(),
        fragment.to_owned(),
    )
}

/// `urlunsplit`.
pub fn urlunsplit(scheme: &str, netloc: &str, path: &str, query: &str, fragment: &str) -> String {
    let mut out = String::new();
    if !scheme.is_empty() {
        out.push_str(scheme);
        out.push(':');
    }
    if !netloc.is_empty() || path.starts_with("//") {
        out.push_str("//");
        out.push_str(netloc);
    }
    out.push_str(path);
    if !query.is_empty() {
        out.push('?');
        out.push_str(query);
    }
    if !fragment.is_empty() {
        out.push('#');
        out.push_str(fragment);
    }
    out
}

/// `checks.sitemap.normalize_url` — host-lowered canonical URL or `None`.
pub fn normalize_url(url_str: &str) -> Option<String> {
    let (scheme, netloc, path, query, _fragment) = urlsplit(url_str.trim());
    if scheme.is_empty() || netloc.is_empty() {
        return None;
    }
    let scheme = scheme.to_lowercase();
    // netloc -> (userinfo, host, port)
    let (userinfo, hostport) = match netloc.rfind('@') {
        Some(i) => (Some(netloc[..i].to_owned()), &netloc[i + 1..]),
        None => (None, &netloc[..]),
    };
    let (host, port) = if hostport.starts_with('[') {
        match hostport.find(']') {
            Some(i) => (
                hostport[..=i].to_owned(),
                hostport[i + 1..]
                    .strip_prefix(':')
                    .and_then(|p| p.parse::<u16>().ok()),
            ),
            None => (hostport.to_owned(), None),
        }
    } else {
        match hostport.rfind(':') {
            Some(i) if hostport[i + 1..].chars().all(|c| c.is_ascii_digit()) => (
                hostport[..i].to_owned(),
                hostport[i + 1..].parse::<u16>().ok(),
            ),
            _ => (hostport.to_owned(), None),
        }
    };
    let host = host.to_lowercase();
    let port = match (scheme.as_str(), port) {
        ("https", Some(443)) | ("http", Some(80)) => None,
        (_, p) => p,
    };
    let mut netloc_out = host;
    if let Some(p) = port {
        netloc_out = format!("{netloc_out}:{p}");
    }
    if let Some(info) = userinfo {
        netloc_out = format!("{info}@{netloc_out}");
    }
    let mut path_out = path;
    if path_out.len() > 1 && path_out.ends_with('/') {
        path_out = path_out.trim_end_matches('/').to_owned();
    }
    Some(urlunsplit(&scheme, &netloc_out, &path_out, &query, ""))
}

/// `checks.external_join.normalize_join_key` with default flags.
pub fn normalize_join_key(url: Option<&str>) -> Option<String> {
    normalize_join_key_opts(url, false, false, false)
}

/// `normalize_join_key(url, ignore_query=, ignore_scheme=, casefold_path=)`.
pub fn normalize_join_key_opts(
    url: Option<&str>,
    ignore_query: bool,
    ignore_scheme: bool,
    casefold_path: bool,
) -> Option<String> {
    let url = url?;
    if url.trim().is_empty() {
        return None;
    }
    let canonical = normalize_url(url)?;
    let (scheme, netloc, path, query, _) = urlsplit(&canonical);
    let scheme = if ignore_scheme {
        "https".to_owned()
    } else {
        scheme
    };
    let path = if casefold_path {
        path.to_lowercase()
    } else {
        path
    };
    let query = if ignore_query { String::new() } else { query };
    Some(urlunsplit(&scheme, &netloc, &path, &query, ""))
}
