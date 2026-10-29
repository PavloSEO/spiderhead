//! Versioned `scan_archive.v1` offline WARC archive of the retained scan responses.
//!
//! `scan-export` exports the retained scan data as records; this module exports the retained
//! HTTP responses themselves as one WARC 1.1 (ISO 28500) file, entirely offline. The archive
//! carries exactly the retained complete responses, one `response` record per stored response
//! row in request order, preceded by one `warcinfo` record. Status lines are synthesized
//! (version and reason phrase), headers are the retained redacted ones, payloads are the
//! content-decoded entity bytes with `Content-Length` rewritten. URLs without a retained
//! complete body are listed with a machine reason in a sibling `<out>.manifest.json`.

#![allow(clippy::too_many_arguments, clippy::type_complexity)]

use crate::bodies::read_body;
use crate::error::StoreError;
use crate::exports::{unlink_owned, write_file, Owned};
use crate::scan_artifact::{loads, open_scan, ScanError};
use rusqlite::Connection;
use serde_json::{json, Map, Value};
use sha1::{Digest, Sha1};
use spiderhead_core::filesystem::{file_identity, PrivateMode};
use std::fs::OpenOptions;
use std::io::Write;
use std::path::{Path, PathBuf};

/// Format label written to the manifest and the result.
pub const ARCHIVE_FORMAT_VERSION: &str = "scan_archive.v1";
/// WARC version line.
pub const WARC_VERSION: &str = "WARC/1.1";
/// Content type of a response record block.
pub const RESPONSE_CONTENT_TYPE: &str = "application/http; msgtype=response";
/// Content type of the warcinfo record block.
pub const WARCINFO_CONTENT_TYPE: &str = "application/warc-fields";
/// Per-body decoded-byte cap (64 MiB); a larger body is skipped with a named reason.
pub const MAX_ARCHIVE_BODY_BYTES: i64 = 67108864;
/// Bound of the `skipped` preview in the result; the manifest file stays complete.
pub const SKIPPED_PREVIEW_LIMIT: i64 = 1000;
/// Coded-transfer header fields that would misdescribe the decoded payload.
pub const DROP_HEADERS: &[&str] = &["content-encoding", "content-length", "transfer-encoding"];

const NAMESPACE_URL: [u8; 16] = [
    0x6b, 0xa7, 0xb8, 0x11, 0x9d, 0xad, 0x11, 0xd1, 0x80, 0xb4, 0x00, 0xc0, 0x4f, 0xd4, 0x30, 0xc8,
];
const UUID_NAMESPACE_NAME: &str = "https://github.com/PavloSEO/seotools/scan_archive.v1";

/// Standard HTTP reason phrases by status code.
const HTTP_REASONS: &[(i64, &str)] = &[
    (100, "Continue"),
    (101, "Switching Protocols"),
    (102, "Processing"),
    (103, "Early Hints"),
    (200, "OK"),
    (201, "Created"),
    (202, "Accepted"),
    (203, "Non-Authoritative Information"),
    (204, "No Content"),
    (205, "Reset Content"),
    (206, "Partial Content"),
    (207, "Multi-Status"),
    (208, "Already Reported"),
    (226, "IM Used"),
    (300, "Multiple Choices"),
    (301, "Moved Permanently"),
    (302, "Found"),
    (303, "See Other"),
    (304, "Not Modified"),
    (305, "Use Proxy"),
    (307, "Temporary Redirect"),
    (308, "Permanent Redirect"),
    (400, "Bad Request"),
    (401, "Unauthorized"),
    (402, "Payment Required"),
    (403, "Forbidden"),
    (404, "Not Found"),
    (405, "Method Not Allowed"),
    (406, "Not Acceptable"),
    (407, "Proxy Authentication Required"),
    (408, "Request Timeout"),
    (409, "Conflict"),
    (410, "Gone"),
    (411, "Length Required"),
    (412, "Precondition Failed"),
    (413, "Content Too Large"),
    (414, "URI Too Long"),
    (415, "Unsupported Media Type"),
    (416, "Range Not Satisfiable"),
    (417, "Expectation Failed"),
    (418, "I'm a Teapot"),
    (421, "Misdirected Request"),
    (422, "Unprocessable Content"),
    (423, "Locked"),
    (424, "Failed Dependency"),
    (425, "Too Early"),
    (426, "Upgrade Required"),
    (428, "Precondition Required"),
    (429, "Too Many Requests"),
    (431, "Request Header Fields Too Large"),
    (451, "Unavailable For Legal Reasons"),
    (500, "Internal Server Error"),
    (501, "Not Implemented"),
    (502, "Bad Gateway"),
    (503, "Service Unavailable"),
    (504, "Gateway Timeout"),
    (505, "HTTP Version Not Supported"),
    (506, "Variant Also Negotiates"),
    (507, "Insufficient Storage"),
    (508, "Loop Detected"),
    (510, "Not Extended"),
    (511, "Network Authentication Required"),
];

fn scan_err(message: impl Into<String>) -> StoreError {
    StoreError::ScanError(ScanError {
        message: message.into(),
    })
}

/// `uuid.uuid5(namespace, name)` as the hyphenated lowercase text.
fn uuid5(namespace: &[u8; 16], name: &str) -> String {
    let mut hasher = Sha1::new();
    hasher.update(namespace);
    hasher.update(name.as_bytes());
    let digest = hasher.finalize();
    let mut bytes = [0u8; 16];
    bytes.copy_from_slice(&digest[..16]);
    bytes[6] = (bytes[6] & 0x0f) | 0x50;
    bytes[8] = (bytes[8] & 0x3f) | 0x80;
    let hex: String = bytes.iter().map(|b| format!("{b:02x}")).collect();
    format!(
        "{}-{}-{}-{}-{}",
        &hex[0..8],
        &hex[8..12],
        &hex[12..16],
        &hex[16..20],
        &hex[20..32]
    )
}

fn uuid_namespace() -> [u8; 16] {
    let text = uuid5(&NAMESPACE_URL, UUID_NAMESPACE_NAME).replace('-', "");
    let mut out = [0u8; 16];
    for (i, byte) in out.iter_mut().enumerate() {
        *byte = u8::from_str_radix(&text[i * 2..i * 2 + 2], 16).unwrap_or(0);
    }
    out
}

/// WARC digest label: SHA-1 in base32, per ISO 28500.
pub fn sha1_digest(data: &[u8]) -> String {
    const ALPHABET: &[u8; 32] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZ234567";
    let digest = Sha1::digest(data);
    let mut out = String::from("sha1:");
    let (mut buffer, mut bits) = (0u32, 0u32);
    for byte in digest.iter() {
        buffer = (buffer << 8) | u32::from(*byte);
        bits += 8;
        while bits >= 5 {
            bits -= 5;
            out.push(ALPHABET[((buffer >> bits) & 31) as usize] as char);
        }
    }
    if bits > 0 {
        out.push(ALPHABET[((buffer << (5 - bits)) & 31) as usize] as char);
    }
    out
}

fn days_from_civil(y: i64, m: i64, d: i64) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = y.div_euclid(400);
    let yoe = y - era * 400;
    let doy = (153 * (if m > 2 { m - 3 } else { m + 9 }) + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

fn civil_from_days(z: i64) -> (i64, i64, i64) {
    let z = z + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    (yoe + era * 400 + i64::from(m <= 2), m, d)
}

fn digits(text: &str, len: usize) -> Option<i64> {
    (text.len() == len && text.bytes().all(|b| b.is_ascii_digit())).then(|| text.parse().ok())?
}

/// Parse the ISO 8601 shapes scans retain into epoch seconds; a naive time is UTC.
fn parse_instant(text: &str) -> Option<i64> {
    let text = text.replace('Z', "+00:00");
    // Every field below is byte-sliced at fixed offsets; ISO instants are ASCII-only.
    if !text.is_ascii() {
        return None;
    }
    let (date, rest) = if text.len() > 10 && !text.is_char_boundary(10) {
        return None;
    } else if text.len() >= 10 && text.as_bytes()[4] == b'-' {
        (&text[..10], &text[10..])
    } else if text.len() >= 8 {
        (&text[..8], &text[8..])
    } else {
        return None;
    };
    let (y, m, d) = if date.len() == 10 {
        (
            digits(&date[0..4], 4)?,
            digits(&date[5..7], 2)?,
            digits(&date[8..10], 2)?,
        )
    } else {
        (
            digits(&date[0..4], 4)?,
            digits(&date[4..6], 2)?,
            digits(&date[6..8], 2)?,
        )
    };
    let leap = (y % 4 == 0 && y % 100 != 0) || y % 400 == 0;
    let month_days = [
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
    ];
    if y < 1 || !(1..=12).contains(&m) || d < 1 || d > month_days[(m - 1) as usize] {
        return None;
    }
    let mut seconds = days_from_civil(y, m, d) * 86_400;
    let mut offset = 0i64;
    if !rest.is_empty() {
        let mut chars = rest.chars();
        let sep = chars.next()?;
        if sep != 'T' && sep != ' ' {
            return None;
        }
        let body = chars.as_str();
        let (clock, zone) = match body.find(['+', '-']) {
            Some(i) => (&body[..i], &body[i..]),
            None => (body, ""),
        };
        let (clock, fraction) = match clock.split_once('.') {
            Some((c, f)) => (c, Some(f)),
            None => (clock, None),
        };
        if let Some(f) = fraction {
            if f.is_empty() || !f.bytes().all(|b| b.is_ascii_digit()) {
                return None;
            }
        }
        let basic = !clock.contains(':');
        let parts: Vec<&str> = if basic {
            (0..clock.len() / 2)
                .map(|i| &clock[i * 2..i * 2 + 2])
                .collect()
        } else {
            clock.split(':').collect()
        };
        if parts.is_empty() || parts.len() > 3 || (basic && clock.len() % 2 != 0) {
            return None;
        }
        let h = digits(parts[0], 2)?;
        let mi = parts.get(1).map_or(Some(0), |p| digits(p, 2))?;
        let s = parts.get(2).map_or(Some(0), |p| digits(p, 2))?;
        if h > 23 || mi > 59 || s > 59 {
            return None;
        }
        seconds += h * 3600 + mi * 60 + s;
        if !zone.is_empty() {
            let sign = if zone.starts_with('-') { -1 } else { 1 };
            let digits_zone = &zone[1..];
            // Accepted offsets: extended "+HH:MM[:SS]", basic "+HHMM"/"+HHMMSS", and bare "+HH".
            let z: Vec<&str> = if digits_zone.contains(':') {
                digits_zone.split(':').collect()
            } else {
                match digits_zone.len() {
                    2 => vec![digits_zone],
                    4 => vec![&digits_zone[0..2], &digits_zone[2..4]],
                    6 => vec![&digits_zone[0..2], &digits_zone[2..4], &digits_zone[4..6]],
                    _ => return None,
                }
            };
            if z.len() > 3 || z.is_empty() {
                return None;
            }
            let zh = digits(z[0], 2)?;
            let zm = z.get(1).map_or(Some(0), |p| digits(p, 2))?;
            let zs = z.get(2).map_or(Some(0), |p| digits(p, 2))?;
            if zh > 23 || zm > 59 || zs > 59 {
                return None;
            }
            offset = sign * (zh * 3600 + zm * 60 + zs);
        }
    }
    Some(seconds - offset)
}

/// Normalize one retained ISO timestamp to WARC's `Z` form, or refuse.
pub fn warc_date(value: &Value) -> Result<String, StoreError> {
    let text = value
        .as_str()
        .filter(|t| !t.is_empty())
        .ok_or_else(|| scan_err("retained timestamp is absent; cannot write a WARC-Date"))?;
    let epoch = parse_instant(text).ok_or_else(|| {
        scan_err(format!(
            "retained timestamp {} is not an ISO-8601 instant",
            py_repr(text)
        ))
    })?;
    let (y, m, d) = civil_from_days(epoch.div_euclid(86_400));
    let rem = epoch.rem_euclid(86_400);
    Ok(format!(
        "{y:04}-{m:02}-{d:02}T{:02}:{:02}:{:02}Z",
        rem / 3600,
        rem % 3600 / 60,
        rem % 60
    ))
}

/// Quote a string for the notes: single quotes unless it contains only single quotes.
fn py_repr(text: &str) -> String {
    let quote = if text.contains('\'') && !text.contains('"') {
        '"'
    } else {
        '\''
    };
    let mut out = String::from(quote);
    for c in text.chars() {
        match c {
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if c == quote => {
                out.push('\\');
                out.push(c);
            }
            c => out.push(c),
        }
    }
    out.push(quote);
    out
}

/// Deterministic URN, so re-running the export reproduces the same archive.
pub fn record_id(scan_uuid: &str, ordinal: i64) -> String {
    format!(
        "<urn:uuid:{}>",
        uuid5(&uuid_namespace(), &format!("{scan_uuid}/{ordinal}"))
    )
}

/// `HTTP/1.1 <code> <reason>` for a retained status code.
pub fn status_line(status_code: &Value) -> Result<Vec<u8>, StoreError> {
    let code = status_code
        .as_i64()
        .filter(|c| (100..=599).contains(c) && !status_code.is_boolean())
        .ok_or_else(|| {
            scan_err(format!(
                "retained status code {} is not an HTTP status",
                py_value_repr(status_code)
            ))
        })?;
    let reason = HTTP_REASONS
        .iter()
        .find(|(c, _)| *c == code)
        .map_or("Unknown", |(_, r)| r);
    Ok(format!("HTTP/1.1 {code} {reason}").into_bytes())
}

fn py_value_repr(value: &Value) -> String {
    match value {
        Value::Null => "None".into(),
        Value::Bool(true) => "True".into(),
        Value::Bool(false) => "False".into(),
        Value::String(s) => py_repr(s),
        other => other.to_string(),
    }
}

/// Render the retained redacted header pairs as HTTP header lines.
pub fn header_lines(redacted_json: &str) -> Result<Vec<Vec<u8>>, StoreError> {
    let pairs = loads(redacted_json, "response headers")?;
    let malformed = || scan_err("retained response headers are malformed");
    let items = pairs.as_array().ok_or_else(malformed)?;
    let mut lines = Vec::new();
    let mut parsed = Vec::new();
    for item in items {
        let pair = item
            .as_array()
            .filter(|p| p.len() == 2)
            .ok_or_else(malformed)?;
        let (Some(name), Some(value)) = (pair[0].as_str(), pair[1].as_str()) else {
            return Err(malformed());
        };
        parsed.push((name, value));
    }
    for (name, value) in parsed {
        if name.contains(['\r', '\n']) || value.contains(['\r', '\n']) {
            return Err(scan_err(format!(
                "retained header {} contains a line break",
                py_repr(name)
            )));
        }
        if !DROP_HEADERS.contains(&name.to_lowercase().as_str()) {
            lines.push(format!("{name}: {value}").into_bytes());
        }
    }
    Ok(lines)
}

/// Assemble one response record's block: status line, headers, payload.
pub fn http_block(
    status_code: &Value,
    redacted_json: &str,
    body: &[u8],
) -> Result<Vec<u8>, StoreError> {
    let mut lines = vec![String::from_utf8_lossy(&status_line(status_code)?).into_owned()];
    for line in header_lines(redacted_json)? {
        lines.push(String::from_utf8_lossy(&line).into_owned());
    }
    lines.push(format!("Content-Length: {}", body.len()));
    let mut out = (lines.join("\r\n") + "\r\n\r\n").into_bytes();
    out.extend_from_slice(body);
    Ok(out)
}

/// One complete WARC response record: headers, block, separator.
pub fn record_chunks(
    scan_uuid: &str,
    ordinal: i64,
    date: &str,
    target_uri: &str,
    block: &[u8],
    payload: &[u8],
) -> Vec<Vec<u8>> {
    let fields = [
        "WARC/1.1".to_string(),
        "WARC-Type: response".to_string(),
        format!("WARC-Record-ID: {}", record_id(scan_uuid, ordinal)),
        format!("WARC-Date: {date}"),
        format!("WARC-Target-URI: {target_uri}"),
        format!("WARC-Block-Digest: {}", sha1_digest(block)),
        format!("WARC-Payload-Digest: {}", sha1_digest(payload)),
        format!("Content-Type: {RESPONSE_CONTENT_TYPE}"),
        format!("Content-Length: {}", block.len()),
    ];
    vec![
        (fields.join("\r\n") + "\r\n\r\n").into_bytes(),
        block.to_vec(),
        b"\r\n\r\n".to_vec(),
    ]
}

fn py_str(value: Option<&Value>) -> String {
    match value {
        None => String::new(),
        Some(Value::String(s)) => s.clone(),
        Some(Value::Null) => "None".into(),
        Some(Value::Bool(true)) => "True".into(),
        Some(Value::Bool(false)) => "False".into(),
        Some(other) => other.to_string(),
    }
}

/// The leading warcinfo record, including the reconstruction notes.
pub fn warcinfo_chunks(scan_uuid: &str, header: &Map<String, Value>, date: &str) -> Vec<Vec<u8>> {
    let fields: [(&str, String); 11] = [
        (
            "software",
            format!("seohead scan-archive ({ARCHIVE_FORMAT_VERSION})"),
        ),
        ("format", WARC_VERSION.to_string()),
        ("scan-uuid", scan_uuid.to_string()),
        ("scan-format", py_str(header.get("format_version"))),
        ("scan-source-kind", py_str(header.get("source_kind"))),
        ("scan-created-at", py_str(header.get("created_at"))),
        ("scan-start-url", py_str(header.get("start_url"))),
        (
            "headers",
            "retained redacted response headers; sensitive field values were removed \
             at capture time and are named by the retention marker header"
                .into(),
        ),
        (
            "status-line",
            "the HTTP version and reason phrase are synthesized; the status code is retained"
                .into(),
        ),
        (
            "payload",
            "content-decoded entity bytes; Content-Length is rewritten to the payload \
             length and Content-Encoding/Transfer-Encoding are dropped"
                .into(),
        ),
        (
            "scope",
            "rendered (serialized-DOM) documents are renderer output, not captured HTTP \
             responses, and are not archived"
                .into(),
        ),
    ];
    let body: String = fields
        .iter()
        .map(|(n, v)| format!("{n}: {v}\r\n"))
        .collect();
    let head = [
        WARC_VERSION.to_string(),
        "WARC-Type: warcinfo".to_string(),
        format!("WARC-Record-ID: {}", record_id(scan_uuid, 0)),
        format!("WARC-Date: {date}"),
        format!("Content-Type: {WARCINFO_CONTENT_TYPE}"),
        format!("Content-Length: {}", body.len()),
        String::new(),
        String::new(),
    ]
    .join("\r\n");
    vec![head.into_bytes(), body.into_bytes(), b"\r\n\r\n".to_vec()]
}

/// One retained complete response, in stored request order.
#[derive(Debug, Clone)]
pub struct ResponseRow {
    /// Request instant.
    pub requested_at: Value,
    /// Request URL.
    pub request_url: String,
    /// Retained status code.
    pub status_code: Value,
    /// Redacted header pairs (JSON text).
    pub headers_json: String,
    /// Body hash.
    pub body_sha256: String,
    /// Declared decoded size.
    pub decoded_bytes: i64,
}

/// Stream every retained complete response in stored request order, one row at a time.
fn for_each_response(
    con: &Connection,
    each: &mut dyn FnMut(ResponseRow) -> Result<(), StoreError>,
) -> Result<(), StoreError> {
    let mut stmt = con.prepare(
        "SELECT r.requested_at, ru.url AS request_url, r.status_code, \
         r.response_headers_redacted_json AS headers_json, r.body_sha256, \
         b.decoded_bytes FROM responses r \
         JOIN urls ru ON ru.url_id=r.request_url_id \
         JOIN bodies b ON b.sha256=r.body_sha256 \
         WHERE r.body_state='complete' AND r.body_sha256 IS NOT NULL \
         ORDER BY r.request_ordinal",
    )?;
    let mut rows = stmt.query([])?;
    while let Some(r) = rows.next()? {
        let cell = |i: usize| -> Result<Value, StoreError> {
            Ok(crate::link_context::cell_value(r.get_ref(i)?))
        };
        each(ResponseRow {
            requested_at: cell(0)?,
            request_url: r.get(1)?,
            status_code: cell(2)?,
            headers_json: r.get(3)?,
            body_sha256: r.get(4)?,
            decoded_bytes: r.get(5)?,
        })?;
    }
    Ok(())
}

/// Every retained complete response in stored request order.
pub fn response_rows(con: &Connection) -> Result<Vec<ResponseRow>, StoreError> {
    let mut out = Vec::new();
    for_each_response(con, &mut |row| {
        out.push(row);
        Ok(())
    })?;
    Ok(out)
}

/// `(url, reason)` for every URL without a retained complete body, ordered by URL text.
///
/// The page's own fetch error first, then why the latest response's body is missing, then why
/// the URL was excluded before fetching, then plain `not_fetched`.
pub fn skip_causes(con: &Connection) -> Result<Vec<(String, String)>, StoreError> {
    let mut stmt = con.prepare(
        "SELECT u.url, \
         (SELECT p.error_kind FROM pages p WHERE p.url_id=u.url_id AND p.error_kind!=''), \
         EXISTS(SELECT 1 FROM responses r WHERE r.request_url_id=u.url_id \
         AND r.body_state='complete' AND r.body_sha256 IS NOT NULL) AS covered, \
         (SELECT r.body_state||':'||r.body_reason FROM responses r \
         WHERE r.request_url_id=u.url_id ORDER BY r.request_ordinal DESC LIMIT 1), \
         (SELECT d.reason FROM decisions d WHERE d.url=u.url ORDER BY d.decision_id LIMIT 1), \
         f.state FROM urls u LEFT JOIN frontier f ON f.url_id=u.url_id ORDER BY u.url",
    )?;
    let rows = stmt.query_map([], |r| {
        Ok((
            r.get::<_, String>(0)?,
            r.get::<_, Option<String>>(1)?,
            r.get::<_, i64>(2)?,
            r.get::<_, Option<String>>(3)?,
            r.get::<_, Option<String>>(4)?,
            r.get::<_, Option<String>>(5)?,
        ))
    })?;
    let mut out = Vec::new();
    for row in rows {
        let (url, error_kind, covered, latest, decision, frontier) = row?;
        if covered != 0 {
            continue;
        }
        let reason = if let Some(kind) = error_kind.filter(|k| !k.is_empty()) {
            format!("error:{kind}")
        } else if let Some(state) = latest {
            state
        } else if frontier.as_deref() == Some("excluded") {
            match decision.filter(|d| !d.is_empty()) {
                Some(d) => format!("excluded:{d}"),
                None => "excluded".into(),
            }
        } else {
            "not_fetched".into()
        };
        out.push((url, reason));
    }
    Ok(out)
}

/// A URL left out of the archive, with its machine reason.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct Skip {
    /// The URL.
    pub url: String,
    /// Why it has no archived response.
    pub reason: String,
}

/// Running totals of one archive build.
#[derive(Debug, Clone, Default)]
pub struct ArchiveStats {
    /// Response records written.
    pub written: i64,
    /// Bytes written to the WARC.
    pub bytes: i64,
    /// Per-body cap in force.
    pub max_body_bytes: i64,
}

fn header_str<'a>(header: &'a Map<String, Value>, key: &str) -> &'a str {
    header.get(key).and_then(Value::as_str).unwrap_or("")
}

/// Stream the warcinfo record plus one response record per retained body.
///
/// One body is decoded in memory at a time; a body whose declared decoded size exceeds
/// `max_body_bytes` is skipped with reason `body_too_large`.
pub fn stream_warc(
    target: &Path,
    con: &Connection,
    header: &Map<String, Value>,
    scan_uuid: &str,
    max_body_bytes: i64,
    stats: &mut ArchiveStats,
    skips: &mut Vec<Skip>,
    owned: &mut Owned,
) -> Result<(), StoreError> {
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .private_mode(0o600)
        .open(target)?;
    let state = file_identity(&file)?;
    owned.insert(
        target.to_string_lossy().into_owned(),
        (state.0 as i64, state.1 as i64),
    );
    let created = header.get("created_at").cloned().unwrap_or(Value::Null);
    for chunk in warcinfo_chunks(scan_uuid, header, &warc_date(&created)?) {
        file.write_all(&chunk)?;
        stats.bytes += chunk.len() as i64;
    }
    let mut index = 0i64;
    for_each_response(con, &mut |row| {
        index += 1;
        if row.decoded_bytes > max_body_bytes {
            skips.push(Skip {
                url: row.request_url.clone(),
                reason: "body_too_large".into(),
            });
            return Ok(());
        }
        let body = read_body(con, &row.body_sha256, row.decoded_bytes)?;
        let block = http_block(&row.status_code, &row.headers_json, &body)?;
        let date = warc_date(&row.requested_at)?;
        for chunk in record_chunks(scan_uuid, index, &date, &row.request_url, &block, &body) {
            file.write_all(&chunk)?;
            stats.bytes += chunk.len() as i64;
        }
        stats.written += 1;
        Ok(())
    })?;
    file.flush()?;
    file.sync_all()?;
    Ok(())
}

/// The complete skip manifest: every unarchived URL with its reason.
///
/// `merged` is filled with the same deterministic, complete list the file carries, so the
/// command result can preview it without a second pass.
pub fn manifest_chunks(
    header: &Map<String, Value>,
    warc_name: &str,
    stats: &ArchiveStats,
    skips: &[Skip],
    merged: &mut Vec<Skip>,
    con: &Connection,
) -> Result<Vec<Vec<u8>>, StoreError> {
    let mut all: Vec<Skip> = skips.to_vec();
    all.extend(
        skip_causes(con)?
            .into_iter()
            .map(|(url, reason)| Skip { url, reason }),
    );
    all.sort();
    merged.extend(all);
    let entries: Vec<Value> = merged
        .iter()
        .map(|s| json!({"url": s.url, "reason": s.reason}))
        .collect();
    let head = json!({
        "format": ARCHIVE_FORMAT_VERSION,
        "warc": warc_name,
        "scan_uuid": header.get("scan_uuid").cloned().unwrap_or(Value::Null),
        "written": stats.written,
        "bytes": stats.bytes,
        "max_body_bytes": stats.max_body_bytes,
        "skipped_total": merged.len(),
        "skipped": entries,
    });
    let mut buffer = Vec::new();
    let formatter = serde_json::ser::PrettyFormatter::with_indent(b" ");
    let mut serializer = serde_json::Serializer::with_formatter(&mut buffer, formatter);
    serde::Serialize::serialize(&head, &mut serializer)?;
    buffer.push(b'\n');
    Ok(vec![buffer])
}

fn fsync_dir(dir: &Path) -> Result<(), StoreError> {
    std::fs::File::open(dir)?.sync_all()?;
    Ok(())
}

type Writer<'a> = &'a mut dyn FnMut(&Path, &mut Owned) -> Result<(), StoreError>;

/// Write every target through a sibling temp file, then hard-link it into place; a failure
/// removes exactly what this run created.
fn publish_files(files: Vec<(PathBuf, Writer<'_>)>) -> Result<(), StoreError> {
    let mut owned = Owned::new();
    let mut order: Vec<PathBuf> = Vec::new();
    let parent = files
        .first()
        .and_then(|(t, _)| t.parent().map(Path::to_path_buf))
        .unwrap_or_default();
    let mut result = Ok(());
    for (target, writer) in files {
        let name = target
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default();
        let temporary = target.with_file_name(format!(".{name}.tmp"));
        order.push(temporary.clone());
        let step = (|| -> Result<(), StoreError> {
            writer(&temporary, &mut owned)?;
            std::fs::hard_link(&temporary, &target)?;
            let identity = owned
                .get(&temporary.to_string_lossy().into_owned())
                .copied()
                .unwrap_or((0, 0));
            owned.insert(target.to_string_lossy().into_owned(), identity);
            order.push(target.clone());
            unlink_owned(&temporary, &owned)
        })();
        if step.is_err() {
            result = step;
            break;
        }
    }
    if result.is_ok() {
        let dir = if parent.as_os_str().is_empty() {
            Path::new(".")
        } else {
            &parent
        };
        result = fsync_dir(dir);
    }
    if result.is_err() {
        for path in order.iter().rev() {
            let _ = unlink_owned(path, &owned);
        }
    }
    result
}

fn failure(message: String) -> Map<String, Value> {
    let mut out = Map::new();
    out.insert("ok".into(), json!(false));
    out.insert("error".into(), json!(message));
    out
}

/// Build one WARC 1.1 file from a scan's retained complete responses.
///
/// Writes `out` and a sibling `<out>.manifest.json` listing every URL that has no archived
/// response, with its reason. Never touches the network and never overwrites an existing
/// destination. Returns the shared `{"ok": ...}` result shape; `skipped` is bounded to the
/// first [`SKIPPED_PREVIEW_LIMIT`] entries while the manifest file is complete.
/// Default body cap: `MAX_ARCHIVE_BODY_BYTES`.
pub fn build_warc(
    input_path: &Path,
    out: Option<&Path>,
    max_body_bytes: i64,
) -> Map<String, Value> {
    match build_inner(input_path, out, max_body_bytes) {
        Ok(result) => result,
        Err(StoreError::ScanError(e)) => failure(e.message),
        Err(other) => failure(format!("cannot build WARC archive: {other}")),
    }
}

fn build_inner(
    input_path: &Path,
    out: Option<&Path>,
    max_body_bytes: i64,
) -> Result<Map<String, Value>, StoreError> {
    let out = out.ok_or_else(|| scan_err("--out is required"))?;
    if max_body_bytes < 1 {
        return Err(scan_err("max_body_bytes must be a positive integer"));
    }
    let parent = match out.parent() {
        Some(p) if !p.as_os_str().is_empty() => p.to_path_buf(),
        _ => PathBuf::from("."),
    };
    if !parent.is_dir() {
        return Err(scan_err(format!(
            "output directory does not exist: {}",
            parent.display()
        )));
    }
    let name = out
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    let manifest_path = out.with_file_name(format!("{name}.manifest.json"));
    for path in [out, manifest_path.as_path()] {
        if std::fs::symlink_metadata(path).is_ok() {
            return Err(scan_err(format!(
                "output already exists: {}",
                path.display()
            )));
        }
    }
    let con = open_scan(input_path, false, None)?;
    let mut header = Map::new();
    con.query_row(
        "SELECT scan_uuid, format_version, source_kind, created_at, start_url \
         FROM scan WHERE singleton=1",
        [],
        |r| {
            for (i, key) in [
                "scan_uuid",
                "format_version",
                "source_kind",
                "created_at",
                "start_url",
            ]
            .iter()
            .enumerate()
            {
                header.insert(
                    (*key).into(),
                    crate::link_context::cell_value(r.get_ref(i)?),
                );
            }
            Ok(())
        },
    )
    .map_err(|e| match e {
        rusqlite::Error::QueryReturnedNoRows => scan_err("scan header is absent"),
        other => other.into(),
    })?;
    let scan_uuid = header_str(&header, "scan_uuid").to_string();
    let mut stats = ArchiveStats {
        max_body_bytes,
        ..Default::default()
    };
    let mut skips: Vec<Skip> = Vec::new();
    let mut merged: Vec<Skip> = Vec::new();
    {
        let stats_cell = std::cell::RefCell::new(&mut stats);
        let skips_cell = std::cell::RefCell::new(&mut skips);
        let merged_cell = std::cell::RefCell::new(&mut merged);
        let mut warc_writer = |target: &Path, owned: &mut Owned| {
            stream_warc(
                target,
                &con,
                &header,
                &scan_uuid,
                max_body_bytes,
                &mut stats_cell.borrow_mut(),
                &mut skips_cell.borrow_mut(),
                owned,
            )
        };
        let mut manifest_writer = |target: &Path, owned: &mut Owned| {
            let chunks = manifest_chunks(
                &header,
                &name,
                &stats_cell.borrow(),
                &skips_cell.borrow(),
                &mut merged_cell.borrow_mut(),
                &con,
            )?;
            write_file(target, chunks, owned)
        };
        publish_files(vec![
            (out.to_path_buf(), &mut warc_writer),
            (manifest_path.clone(), &mut manifest_writer),
        ])?;
    }
    let preview: Vec<Value> = merged
        .iter()
        .take(SKIPPED_PREVIEW_LIMIT as usize)
        .map(|s| json!({"url": s.url, "reason": s.reason}))
        .collect();
    let mut result = Map::new();
    result.insert("ok".into(), json!(true));
    result.insert("format".into(), json!(ARCHIVE_FORMAT_VERSION));
    result.insert("warc".into(), json!(out.to_string_lossy()));
    result.insert("manifest".into(), json!(manifest_path.to_string_lossy()));
    result.insert("written".into(), json!(stats.written));
    result.insert("bytes".into(), json!(stats.bytes));
    result.insert(
        "manifest_bytes".into(),
        json!(std::fs::metadata(&manifest_path)?.len()),
    );
    result.insert("skipped_total".into(), json!(merged.len()));
    result.insert("skipped".into(), Value::Array(preview));
    Ok(result)
}

#[cfg(test)]
mod tests {
    use super::*;

    // WARC digest label is SHA-1 in base32.
    #[test]
    fn digest_label_is_base32_sha1() {
        // Expected value: "sha1:" followed by the base32 SHA-1 of the empty input.
        assert_eq!(sha1_digest(b""), "sha1:3I42H3S6NNFQ2MSVX7XZKYAYSCX5QBYJ");
        assert_eq!(sha1_digest(b"abc"), "sha1:VGMT4NSHA2AWVOR6EVYXQUGCNSONBWE5");
    }

    // Record ids are stable UUID5 values.
    #[test]
    fn record_ids_are_deterministic_uuid5() {
        let a = record_id("scan-1", 3);
        assert_eq!(a, record_id("scan-1", 3));
        assert_eq!(a, "<urn:uuid:ff33e083-8129-58f4-8bbe-3d72c8a35d2a>");
        assert_ne!(a, record_id("scan-1", 4));
        assert!(a.starts_with("<urn:uuid:") && a.ends_with('>') && a.len() == 47);
        // Check UUID5 against the published DNS-namespace test vector.
        let dns = [
            0x6b, 0xa7, 0xb8, 0x10, 0x9d, 0xad, 0x11, 0xd1, 0x80, 0xb4, 0x00, 0xc0, 0x4f, 0xd4,
            0x30, 0xc8,
        ];
        assert_eq!(
            uuid5(&dns, "python.org"),
            "886313e1-3b8a-5372-9b90-0c9aee199e5d"
        );
    }

    // WARC-Date uses the normalized Z form.
    #[test]
    fn warc_date_normalises_and_refuses() {
        assert_eq!(
            warc_date(&json!("2026-01-02T03:04:05Z")).unwrap(),
            "2026-01-02T03:04:05Z"
        );
        assert_eq!(
            warc_date(&json!("2026-01-02T03:04:05.123+02:00")).unwrap(),
            "2026-01-02T01:04:05Z"
        );
        assert_eq!(
            warc_date(&json!("2026-01-02T03:04:05")).unwrap(),
            "2026-01-02T03:04:05Z"
        );
        assert!(warc_date(&json!("")).is_err());
        assert!(warc_date(&Value::Null).is_err());
        assert!(warc_date(&json!("yesterday")).is_err());
        assert!(warc_date(&json!("2026-02-30T00:00:00Z")).is_err());
    }

    // The status line is HTTP/1.1 with the reason phrase.
    #[test]
    fn status_line_reasons() {
        assert_eq!(status_line(&json!(200)).unwrap(), b"HTTP/1.1 200 OK");
        assert_eq!(status_line(&json!(599)).unwrap(), b"HTTP/1.1 599 Unknown");
        assert!(status_line(&json!(99)).is_err());
        assert!(status_line(&json!(true)).is_err());
        assert!(status_line(&json!("200")).is_err());
    }

    // Headers are the retained redacted pairs.
    #[test]
    fn header_lines_drop_coded_fields_and_refuse_breaks() {
        let lines = header_lines(
            r#"[["Content-Type","text/html"],["Content-Encoding","gzip"],["Content-Length","9"]]"#,
        )
        .unwrap();
        assert_eq!(lines, vec![b"Content-Type: text/html".to_vec()]);
        assert!(header_lines(r#"[["A","b\nc"]]"#).is_err());
        assert!(header_lines(r#"{"a":"b"}"#).is_err());
        assert!(header_lines(r#"[["a"]]"#).is_err());
    }

    // The block is the status line, headers and decoded payload.
    #[test]
    fn http_block_layout() {
        let block = http_block(&json!(404), r#"[["X","1"]]"#, b"hey").unwrap();
        assert_eq!(
            block,
            b"HTTP/1.1 404 Not Found\r\nX: 1\r\nContent-Length: 3\r\n\r\nhey"
        );
    }

    // Record layout follows the WARC 1.1 structure.
    #[test]
    fn response_record_layout() {
        let chunks = record_chunks(
            "u",
            1,
            "2026-01-01T00:00:00Z",
            "https://e.test/",
            b"BLOCK",
            b"P",
        );
        let head = String::from_utf8(chunks[0].clone()).unwrap();
        assert!(head.starts_with("WARC/1.1\r\nWARC-Type: response\r\n"));
        assert!(head.contains("Content-Length: 5\r\n\r\n"));
        assert_eq!(chunks[1], b"BLOCK");
        assert_eq!(chunks[2], b"\r\n\r\n");
    }

    #[test]
    fn warcinfo_length_matches_body() {
        let mut header = Map::new();
        header.insert("format_version".into(), json!("scan.v1"));
        header.insert("start_url".into(), Value::Null);
        let chunks = warcinfo_chunks("u", &header, "2026-01-01T00:00:00Z");
        let head = String::from_utf8(chunks[0].clone()).unwrap();
        assert!(head.contains(&format!("Content-Length: {}\r\n", chunks[1].len())));
        assert!(String::from_utf8(chunks[1].clone())
            .unwrap()
            .contains("scan-start-url: None"));
    }

    // An existing output is refused and left untouched; invalid input writes nothing.
    #[test]
    fn build_refuses_missing_out_and_existing_output() {
        let dir = std::env::temp_dir().join("w2s6-archive");
        let _ = std::fs::create_dir_all(&dir);
        assert_eq!(
            build_warc(&dir, None, 10)["error"],
            json!("--out is required")
        );
        let existing = dir.join("a.warc");
        std::fs::write(&existing, b"x").unwrap();
        let result = build_warc(&dir, Some(&existing), 10);
        assert_eq!(result["ok"], json!(false));
        assert!(result["error"]
            .as_str()
            .unwrap()
            .starts_with("output already exists"));
        assert_eq!(
            build_warc(&dir, Some(&dir.join("b.warc")), 0)["ok"],
            json!(false)
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn skip_causes_classify() {
        let con = Connection::open_in_memory().unwrap();
        con.execute_batch(
            "CREATE TABLE urls(url_id INTEGER PRIMARY KEY,url TEXT);
             CREATE TABLE pages(url_id INTEGER,error_kind TEXT);
             CREATE TABLE responses(request_url_id INTEGER,body_state TEXT,body_reason TEXT,body_sha256 TEXT,request_ordinal INTEGER);
             CREATE TABLE decisions(decision_id INTEGER PRIMARY KEY,url TEXT,reason TEXT);
             CREATE TABLE frontier(url_id INTEGER,state TEXT);
             INSERT INTO urls VALUES(1,'a'),(2,'b'),(3,'c'),(4,'d'),(5,'e');
             INSERT INTO pages VALUES(1,'timeout');
             INSERT INTO responses VALUES(2,'discarded','policy',NULL,1),(5,'complete','none','h',1);
             INSERT INTO decisions(url,reason) VALUES('c','robots');
             INSERT INTO frontier VALUES(3,'excluded'),(4,'queued');",
        )
        .unwrap();
        assert_eq!(
            skip_causes(&con).unwrap(),
            vec![
                ("a".to_string(), "error:timeout".to_string()),
                ("b".to_string(), "discarded:policy".to_string()),
                ("c".to_string(), "excluded:robots".to_string()),
                ("d".to_string(), "not_fetched".to_string()),
            ]
        );
    }
}
