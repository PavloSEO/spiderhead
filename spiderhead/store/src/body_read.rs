//! Read one bounded, hash-verified retained body window by exact URL.
//!
//! Command handler for body reads, over
//! `bodies::read_page_document_content`, including the detail-URL query
//! redaction the handler applies before answering.

use serde_json::{json, Map, Value};

use crate::bodies::read_page_document_content;
use crate::error::StoreError;
use crate::scan_artifact::open_scan_mode;

/// Window cap of one read (`storage.bodies.MAX_WINDOW_CHARS`).
pub const MAX_WINDOW_CHARS: i64 = 64 * 1024;

fn invalid(message: impl Into<String>) -> StoreError {
    StoreError::Invalid(message.into())
}

/// `history_handlers._detail_url`: keep a retained URL's identity, never its query values.
pub fn detail_url(value: &str) -> String {
    let Some((head, query_and_fragment)) = value.split_once('?') else {
        return value.to_owned();
    };
    let (query, fragment) = match query_and_fragment.split_once('#') {
        Some((query, fragment)) => (query, Some(fragment)),
        None => (query_and_fragment, None),
    };
    // A URL split failure collapses the whole value to one marker; query parsing
    // itself never raises (invalid escapes pass through unquoted).
    let Some((scheme, netloc, path)) = split_authority(head) else {
        return "[query-redacted]".to_owned();
    };
    let pairs = parse_qsl_pairs(query);
    let redacted_query = if pairs.is_empty() {
        "[redacted]".to_owned()
    } else {
        pairs
            .iter()
            .map(|(name, _)| quote_pair(name, "[redacted]"))
            .collect::<Vec<_>>()
            .join("&")
    };
    urlunsplit_parts(&scheme, &netloc, &path, &redacted_query, fragment)
}

// The helpers below are local, minimal ports of the `urllib.parse` pieces
// `detail_url` needs; keeping them here avoids a store → parse dependency edge
// between two layer-2 crates.

fn split_authority(url: &str) -> Option<(String, String, String)> {
    let (scheme, rest) = url.split_once(':')?;
    if scheme.is_empty()
        || !scheme
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '+' || c == '-' || c == '.')
    {
        return None;
    }
    let rest = rest.strip_prefix("//").unwrap_or(rest);
    let netloc = rest
        .split(['/', '?', '#'])
        .next()
        .unwrap_or_default()
        .to_owned();
    let path = rest.strip_prefix(&netloc).unwrap_or(rest).to_owned();
    Some((scheme.to_owned(), netloc, path))
}

/// `urllib.parse.parse_qsl` (keep_blank_values) for the redaction path.
fn parse_qsl_pairs(query: &str) -> Vec<(String, String)> {
    let mut pairs = Vec::new();
    for part in query.split('&') {
        if part.is_empty() {
            continue;
        }
        let (name, value) = match part.split_once('=') {
            Some((name, value)) => (name, value),
            None => (part, ""),
        };
        pairs.push((unquote_plus(name), unquote_plus(value)));
    }
    pairs
}

fn unquote_plus(text: &str) -> String {
    let bytes: Vec<u8> = text
        .chars()
        .flat_map(|c| {
            if c == '+' {
                vec![b' ']
            } else {
                c.to_string().into_bytes()
            }
        })
        .collect();
    unquote_bytes(&bytes)
}

fn unquote_bytes(bytes: &[u8]) -> String {
    let mut out: Vec<u8> = Vec::with_capacity(bytes.len());
    let mut index = 0;
    while index < bytes.len() {
        if bytes[index] == b'%' && index + 3 <= bytes.len() {
            if let Ok(byte) = u8::from_str_radix(
                std::str::from_utf8(&bytes[index + 1..index + 3]).unwrap_or(""),
                16,
            ) {
                out.push(byte);
                index += 3;
                continue;
            }
        }
        out.push(bytes[index]);
        index += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// `urllib.parse.urlencode` of already-redacted pairs (quote_via=quote_plus).
fn quote_pair(name: &str, value: &str) -> String {
    format!("{}={}", quote_plus(name), quote_plus(value))
}

fn quote_plus(text: &str) -> String {
    let mut out = String::new();
    for byte in text.bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'.' | b'_' | b'-' | b'~' => {
                out.push(byte as char)
            }
            b' ' => out.push('+'),
            other => out.push_str(&format!("%{other:02X}")),
        }
    }
    out
}

/// `urllib.parse.urlunsplit` over the redacted pieces.
fn urlunsplit_parts(
    scheme: &str,
    netloc: &str,
    path: &str,
    query: &str,
    fragment: Option<&str>,
) -> String {
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
    if let Some(fragment) = fragment {
        if !fragment.is_empty() {
            out.push('#');
            out.push_str(fragment);
        }
    }
    out
}

/// Read one bounded retained body window by exact URL, offline (`scan_body_read`).
#[allow(clippy::too_many_arguments)]
pub fn scan_body_read(
    input_path: &str,
    url: &str,
    representation: &str,
    variant_key: Option<&str>,
    offset: i64,
    length: i64,
    max_decoded_bytes: i64,
) -> Result<Map<String, Value>, StoreError> {
    if url.is_empty() || url.chars().count() > 8192 {
        return Err(invalid(
            "url must be exact nonempty text of at most 8192 characters",
        ));
    }
    if !["static", "rendered", "legacy_fragment"].contains(&representation) {
        return Err(invalid(
            "representation must be static, rendered, or legacy_fragment",
        ));
    }
    if offset < 0 {
        return Err(invalid("offset must be a nonnegative integer"));
    }
    if !(1..=MAX_WINDOW_CHARS).contains(&length) {
        return Err(invalid(format!("length must be 1..{MAX_WINDOW_CHARS}")));
    }
    let (con, validation) = open_scan_mode(std::path::Path::new(input_path), false, None, true)?;
    let document =
        read_page_document_content(&con, url, representation, variant_key, max_decoded_bytes)?;
    let mut out = Map::new();
    if document["state"] != json!("available") {
        out.insert("ok".into(), json!(true));
        for (key, value) in &document {
            out.insert(key.clone(), value.clone());
        }
        out.insert("url".into(), json!(detail_url(url)));
        out.insert("validation".into(), json!(validation));
        return Ok(out);
    }
    let Some(text) = document.get("text").and_then(Value::as_str) else {
        return Err(invalid("retained document carries no readable text"));
    };
    let total_chars = text.chars().count() as i64;
    let window: String = text
        .chars()
        .skip(offset as usize)
        .take(length as usize)
        .collect();
    let window_chars = window.chars().count() as i64;
    out.insert("ok".into(), json!(true));
    out.insert("state".into(), json!("available"));
    out.insert("validation".into(), json!(validation));
    for (key, value) in &document {
        if key == "text" {
            continue;
        }
        out.insert(key.clone(), value.clone());
    }
    out.insert("url".into(), json!(detail_url(url)));
    out.insert("offset".into(), json!(offset));
    out.insert("length".into(), json!(window_chars));
    out.insert(
        "truncated".into(),
        json!(offset + window_chars < total_chars),
    );
    out.insert("text".into(), json!(window));
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detail_url_redacts_query_values_only() {
        assert_eq!(detail_url("http://x/a.html"), "http://x/a.html");
        assert_eq!(
            detail_url("http://x/a.html?q=secret&z=+1#frag"),
            "http://x/a.html?q=%5Bredacted%5D&z=%5Bredacted%5D#frag"
        );
        assert_eq!(detail_url("http://x/a.html#frag"), "http://x/a.html#frag");
    }

    #[test]
    fn bounds_are_named() {
        assert_eq!(
            scan_body_read("s", "", "static", None, 0, 10, 100)
                .unwrap_err()
                .to_string(),
            "invalid artifact: url must be exact nonempty text of at most 8192 characters"
        );
        assert_eq!(
            scan_body_read("s", "u", "rendered2", None, 0, 10, 100)
                .unwrap_err()
                .to_string(),
            "invalid artifact: representation must be static, rendered, or legacy_fragment"
        );
        assert_eq!(
            scan_body_read("s", "u", "static", None, -1, 10, 100)
                .unwrap_err()
                .to_string(),
            "invalid artifact: offset must be a nonnegative integer"
        );
        assert_eq!(
            scan_body_read("s", "u", "static", None, 0, 0, 100)
                .unwrap_err()
                .to_string(),
            format!("invalid artifact: length must be 1..{MAX_WINDOW_CHARS}")
        );
    }
}
