//! Field helpers shared by every non-Apache layout: timestamps, request targets,
//! quote-aware tokens, column-name synonyms and the draft a parser fills before it
//! becomes a [`Row`].

use std::sync::OnceLock;

use regex::Regex;

use crate::clock::{self, epoch};
use crate::format::{parse_apache_timestamp, Row};
use crate::text::{parse_float, parse_int, strip};

/// Unit of a response-time column.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Unit {
    /// Seconds, possibly fractional.
    Seconds,
    /// Milliseconds.
    Millis,
    /// Microseconds.
    Micros,
}

impl Unit {
    /// Convert `value` expressed in this unit to milliseconds.
    pub fn to_millis(self, value: f64) -> f64 {
        match self {
            Unit::Seconds => value * 1000.0,
            Unit::Millis => value,
            Unit::Micros => value / 1000.0,
        }
    }
}

/// What a named column or key carries.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Field {
    /// Client address.
    Ip,
    /// A full timestamp.
    Stamp,
    /// The date part of a split timestamp.
    Date,
    /// The clock part of a split timestamp.
    Clock,
    /// HTTP method.
    Method,
    /// Request path or URL.
    Url,
    /// Query string kept apart from the path.
    Query,
    /// A whole request line such as `GET /a HTTP/1.1`.
    Request,
    /// Response status code.
    Status,
    /// Response size in bytes.
    Bytes,
    /// Referrer.
    Referer,
    /// User agent.
    Agent,
    /// Response time.
    Taken(Unit),
    /// Anything else.
    Skip,
}

/// Classify a column or key name by its synonyms (case and punctuation are ignored).
pub fn classify(name: &str) -> Field {
    let key: String = name
        .trim_start_matches('\u{feff}')
        .chars()
        .filter(|c| c.is_alphanumeric())
        .flat_map(char::to_lowercase)
        .collect();
    match key.as_str() {
        "ip" | "clientip" | "remoteaddr" | "remotehost" | "remoteip" | "cip" | "clientaddress"
        | "clientipaddress" | "sourceip" | "srcip" | "ipaddress" | "host_ip" | "remoteaddress" => {
            Field::Ip
        }
        "time" | "clock" | "timeofday" | "csclock" => Field::Clock,
        "date" => Field::Date,
        "timestamp" | "datetime" | "timelocal" | "timeiso8601" | "requestdate"
        | "requesttimestamp" | "edgestarttimestamp" | "ts" | "msec" | "accessdate" | "logtime" => {
            Field::Stamp
        }
        "method" | "requestmethod" | "httpmethod" | "csmethod" | "verb" | "clientrequestmethod" => {
            Field::Method
        }
        "url" | "uri" | "path" | "requesturi" | "requesturl" | "urlpath" | "csuristem" | "page"
        | "requestpath" | "clientrequesturi" | "clientrequestpath" | "pathinfo" => Field::Url,
        "query" | "querystring" | "args" | "csuriquery" | "requestquery" => Field::Query,
        "request" | "requestline" | "firstline" => Field::Request,
        "status" | "statuscode" | "httpstatus" | "responsecode" | "scstatus" | "code"
        | "response" | "edgeresponsestatus" | "httpcode" | "responsestatus" => Field::Status,
        "bytes" | "size" | "bytessent" | "bodybytessent" | "responsebytes" | "scbytes"
        | "contentlength" | "responsesize" | "edgeresponsebytes" | "sentbytes" => Field::Bytes,
        "referer"
        | "referrer"
        | "httpreferer"
        | "csreferer"
        | "csrefererheader"
        | "clientrequestreferer"
        | "httpreferrer" => Field::Referer,
        "useragent"
        | "httpuseragent"
        | "ua"
        | "agent"
        | "csuseragent"
        | "clientrequestuseragent" => Field::Agent,
        "timetaken" | "tt" => Field::Taken(Unit::Millis),
        "requesttime"
        | "responsetime"
        | "duration"
        | "elapsed"
        | "latency"
        | "rt"
        | "tat"
        | "responsetimesec"
        | "responsetimeseconds"
        | "timetakenseconds" => Field::Taken(Unit::Seconds),
        "responsetimems" | "durationms" | "latencyms" | "timetakenms" | "elapsedms"
        | "requesttimems" => Field::Taken(Unit::Millis),
        "responsetimeus" | "durationus" | "latencyus" | "timetakenus" | "responsetimemicros" => {
            Field::Taken(Unit::Micros)
        }
        _ => Field::Skip,
    }
}

fn iso_re() -> Option<&'static Regex> {
    static CELL: OnceLock<Option<Regex>> = OnceLock::new();
    CELL.get_or_init(|| {
        Regex::new(
            r"^(\d{4})-(\d{2})-(\d{2})[T ](\d{2}):(\d{2}):(\d{2})(?:[.,]\d+)?\s*(Z|z|UTC|GMT|[+-]\d{2}(?::?\d{2})?)?$",
        )
        .ok()
    })
    .as_ref()
}

/// Parse a timestamp in any of the shapes logs use: Apache `10/Oct/2000:13:55:36 -0700`,
/// ISO 8601 with optional zone and fraction, or Unix epoch seconds, milliseconds,
/// microseconds or nanoseconds. A value without a zone is read as UTC.
pub fn parse_time(raw: &str) -> Option<i64> {
    let text = strip(raw).trim_matches('"');
    let text = text.trim_start_matches('[').trim_end_matches(']');
    if text.is_empty() || text == "-" {
        return None;
    }
    if text.contains('/') {
        return parse_apache_timestamp(text);
    }
    if text.bytes().all(|b| b.is_ascii_digit() || b == b'.') {
        let value = parse_float(text)?;
        let digits = text.split('.').next().map_or(0, str::len);
        let seconds = match digits {
            0..=11 => value,
            12..=14 => value / 1e3,
            15..=17 => value / 1e6,
            _ => value / 1e9,
        };
        let whole = seconds.floor() as i64;
        return clock::in_range(whole).then_some(whole);
    }
    let caps = iso_re()?.captures(text)?;
    let n = |i: usize| parse_int(&caps[i]).unwrap_or(0);
    let base = epoch(n(1), n(2), n(3), n(4), n(5), n(6))?;
    let shifted = match caps.get(7).map(|m| m.as_str()) {
        Some(zone) if zone.starts_with(['+', '-']) => {
            let digits: String = zone[1..].chars().filter(char::is_ascii_digit).collect();
            let hours = parse_int(digits.get(..2).unwrap_or("0")).unwrap_or(0);
            let minutes = parse_int(digits.get(2..4).unwrap_or("0")).unwrap_or(0);
            let offset = hours * 3600 + minutes * 60;
            if zone.starts_with('-') {
                base + offset
            } else {
                base - offset
            }
        }
        _ => base,
    };
    clock::in_range(shifted).then_some(shifted)
}

/// Drop `scheme://host` from an absolute URL, leaving path and query; other text is kept.
pub fn path_of(url: &str) -> String {
    let lower = url.get(..8).unwrap_or(url).to_ascii_lowercase();
    if lower.starts_with("http://") || lower.starts_with("https://") {
        let rest = &url[lower.find("://").map_or(0, |i| i + 3)..];
        return match rest.find('/') {
            Some(i) => rest[i..].to_string(),
            None => "/".to_string(),
        };
    }
    url.to_string()
}

/// Split `GET /a HTTP/1.1` into method and target. A line with a single token is not a
/// request line.
pub fn split_request(line: &str) -> Option<(String, String)> {
    let mut parts = strip(line).split_whitespace();
    let method = parts.next()?;
    let target = parts.next()?;
    let known = !method.is_empty()
        && method
            .chars()
            .all(|c| c.is_ascii_uppercase() || c == '!' || c == '-');
    known.then(|| (method.to_string(), target.to_string()))
}

/// Whitespace-separated tokens where a double-quoted run is one token (quotes removed).
pub fn tokens(line: &str) -> Vec<&str> {
    let bytes = line.as_bytes();
    let mut out = Vec::new();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i].is_ascii_whitespace() {
            i += 1;
        } else if bytes[i] == b'"' {
            let start = i + 1;
            let mut j = start;
            while j < bytes.len() && bytes[j] != b'"' {
                j += 1;
            }
            out.push(line.get(start..j).unwrap_or(""));
            i = j + 1;
        } else {
            let start = i;
            while i < bytes.len() && !bytes[i].is_ascii_whitespace() {
                i += 1;
            }
            out.push(line.get(start..i).unwrap_or(""));
        }
    }
    out
}

fn dash_to_none(value: &str) -> Option<String> {
    let value = strip(value);
    (!value.is_empty() && value != "-").then(|| value.to_string())
}

/// Values collected from one line before they become a [`Row`].
#[derive(Debug, Default)]
pub struct Draft {
    /// Client address.
    pub ip: String,
    /// Request instant, Unix seconds UTC.
    pub time: Option<i64>,
    /// HTTP method.
    pub method: String,
    /// Path or URL from a dedicated column.
    pub url: Option<String>,
    /// Path from a request line; used when no dedicated column gave one.
    pub from_request: Option<String>,
    /// Query kept apart from the path.
    pub query: Option<String>,
    /// Response status.
    pub status: Option<i64>,
    /// Response size.
    pub bytes: i64,
    /// Referrer.
    pub referer: Option<String>,
    /// User agent.
    pub agent: Option<String>,
    /// Response time in milliseconds.
    pub ms: Option<f64>,
}

impl Draft {
    /// Store the value of a column of kind `field`. Date and clock halves are joined by the
    /// caller; they are ignored here.
    pub fn set(&mut self, field: Field, value: &str) {
        let value = strip(value);
        match field {
            Field::Ip => self.ip = value.to_string(),
            Field::Stamp => self.time = self.time.or_else(|| parse_time(value)),
            Field::Method => self.method = value.to_ascii_uppercase(),
            Field::Url => self.url = self.url.take().or_else(|| dash_to_none(value)),
            Field::Query => self.query = dash_to_none(value),
            Field::Request => {
                if let Some((method, target)) = split_request(value) {
                    if self.method.is_empty() {
                        self.method = method;
                    }
                    self.from_request = Some(path_of(&target));
                } else if value.starts_with('/') {
                    self.from_request = Some(value.to_string());
                }
            }
            Field::Status => self.status = parse_int(value).or(self.status),
            Field::Bytes => self.bytes = parse_int(value).unwrap_or(self.bytes),
            Field::Referer => self.referer = dash_to_none(value),
            Field::Agent => self.agent = dash_to_none(value),
            Field::Taken(unit) => {
                self.ms = parse_float(value).map(|v| unit.to_millis(v)).or(self.ms);
            }
            Field::Date | Field::Clock | Field::Skip => {}
        }
    }

    /// Finish the row. A line without a path or a status is not a request record.
    pub fn finish(self) -> Option<Row> {
        let mut path = path_of(&self.url.or(self.from_request)?);
        if !path.starts_with('/') && !path.contains("://") {
            path.insert(0, '/');
        }
        if let Some(query) = self.query.filter(|_| !path.contains('?')) {
            path = format!("{path}?{}", query.trim_start_matches('?'));
        }
        Some(Row {
            ip: self.ip,
            time: self.time,
            method: self.method,
            path,
            status: self.status?,
            bytes: self.bytes,
            time_taken: self.ms.map(|m| m / 1000.0),
            response_ms: self.ms,
            referer: self.referer,
            user_agent: self.agent,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn timestamps_in_common_shapes() {
        let want = Some(1_792_886_400 + 3_600);
        assert_eq!(parse_time("2026-10-25T01:00:00Z"), want);
        assert_eq!(parse_time("2026-10-25 04:00:00+03:00"), want);
        assert_eq!(parse_time("2026-10-25T01:00:00.123456Z"), want);
        assert_eq!(parse_time("1792890000"), want);
        assert_eq!(parse_time("1792890000123"), want);
        assert_eq!(parse_time("1792890000.5"), want);
        assert_eq!(parse_time("25/Oct/2026:01:00:00 +0000"), want);
        assert_eq!(parse_time("not a time"), None);
        assert_eq!(parse_time("-"), None);
    }

    #[test]
    fn absolute_urls_lose_scheme_and_host() {
        assert_eq!(path_of("https://example.test:443/a?b=1"), "/a?b=1");
        assert_eq!(path_of("http://example.test"), "/");
        assert_eq!(path_of("/plain"), "/plain");
    }

    #[test]
    fn quoted_runs_are_single_tokens() {
        assert_eq!(tokens(r#"a "b c" d"#), vec!["a", "b c", "d"]);
        assert_eq!(tokens(r#"x "" y"#), vec!["x", "", "y"]);
    }

    #[test]
    fn names_map_by_synonym() {
        assert_eq!(classify("cs(User-Agent)"), Field::Agent);
        assert_eq!(classify("\u{feff}Client IP"), Field::Ip);
        assert_eq!(classify("time-taken"), Field::Taken(Unit::Millis));
        assert_eq!(classify("request_time"), Field::Taken(Unit::Seconds));
        assert_eq!(classify("whatever"), Field::Skip);
    }
}
