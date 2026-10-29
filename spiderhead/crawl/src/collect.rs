//! `fetch_one`: pacing, GET, retries, capture events and the body-to-record projection.

use std::collections::BTreeMap;

use serde_json::{json, Map, Value};
use spiderhead_fetch::client::{self, FetchErrorKind, RawResponse};
use spiderhead_fetch::throttle::{Outcome, Throttle};
use spiderhead_parse::models::ParsedPage;
use spiderhead_parse::{parse_html, ParseOptions};
use spiderhead_store::crawl_capture::CaptureEvent;
use spiderhead_store::scan_lease::utc;

use crate::record;

/// The result of one URL's fetch+parse pipeline.
pub struct Fetched {
    /// The full page record, ready for `commit_page` (`record` field).
    pub record: Map<String, Value>,
    /// The transport observation for the scan's `responses` lane.
    pub capture: CaptureEvent,
    /// Parsed page — `None` when the body was not parsed (non-HTML, oversized).
    pub parsed: Option<ParsedPage>,
    /// Wire size of the response body.
    pub size_bytes: usize,
    /// Raw response headers (lowercased, wire order).
    pub headers: Vec<(String, String)>,
    /// The decoded body text, exactly as parsed (`None` when not parsed).
    pub html: Option<String>,
}

/// The `charset=` parameter of a content type, lowercased, quotes stripped.
pub fn content_charset(content_type: &str) -> Option<String> {
    for part in content_type.split(';').skip(1) {
        let part = part.trim();
        if let Some(value) = part.strip_prefix("charset=") {
            let label = value.trim().trim_matches('"').trim_matches('\'');
            if !label.is_empty() {
                return Some(label.to_ascii_lowercase());
            }
        }
    }
    None
}

/// Response headers kept verbatim in the capture.
const ALLOW: &[&str] = &[
    "content-type",
    "content-encoding",
    "location",
    "etag",
    "last-modified",
    "cache-control",
    "vary",
    "x-robots-tag",
    "content-language",
    "content-length",
    "user-agent",
    "accept",
    "accept-language",
    "accept-encoding",
    "host",
    "date",
    "expires",
    "age",
    "pragma",
    "if-none-match",
    "if-modified-since",
    "range",
    "content-range",
    "link",
    "strict-transport-security",
    "content-security-policy",
    "x-frame-options",
    "x-content-type-options",
    "referrer-policy",
    "permissions-policy",
];

/// Sensitive headers: recorded as presence only, never their values.
const SENSITIVE: &[&str] = &[
    "authorization",
    "proxy-authorization",
    "cookie",
    "set-cookie",
    "x-api-key",
    "x-auth-token",
];

/// Header list as recorded: wire order kept, and sensitive headers collapsed into one marker header listing their names.
pub fn redact_headers(headers: &[(String, String)]) -> Vec<(String, String)> {
    let mut kept: Vec<(String, String)> = headers
        .iter()
        .filter(|(name, _)| ALLOW.contains(&name.as_str()))
        .cloned()
        .collect();
    let mut sensitive: Vec<&str> = headers
        .iter()
        .map(|(name, _)| name.as_str())
        .filter(|name| SENSITIVE.contains(name))
        .collect();
    sensitive.sort();
    sensitive.dedup();
    if !sensitive.is_empty() {
        kept.push((
            "x-seohead-redacted-headers".to_string(),
            sensitive.join(","),
        ));
    }
    kept
}

fn header<'a>(headers: &'a [(String, String)], name: &str) -> &'a str {
    headers
        .iter()
        .find(|(key, _)| key == name)
        .map(|(_, value)| value.as_str())
        .unwrap_or("")
}

/// Resolve `Location` against the request URL for a 3xx record.
fn redirect_target(url: &str, location: &str) -> String {
    if location.is_empty() {
        return String::new();
    }
    match url::Url::parse(url) {
        Ok(base) => match base.join(location) {
            Ok(resolved) => resolved.to_string(),
            Err(_) => location.to_string(),
        },
        Err(_) => location.to_string(),
    }
}

/// Fetch, validate, capture and parse one URL. The caller sets `depth` before the call.
/// Each retry is a real request that takes a pacing turn.
#[allow(clippy::too_many_arguments)]
pub async fn fetch_one(
    client: &reqwest::Client,
    throttle: &mut Throttle,
    url: &str,
    user_agent: &str,
    retry_on_timeout: u32,
    retry_server_errors: u32,
    max_response_bytes: i64,
    entity_capture_limit: i64,
    parse_options: &ParseOptions,
    scope_media_rejection: &dyn Fn(&str) -> Option<String>,
) -> Fetched {
    let mut record = base_record(url);
    let mut attempts: u32 = 0;
    let mut response = raw_with_retries(
        client,
        throttle,
        url,
        user_agent,
        retry_on_timeout,
        retry_server_errors,
        &mut attempts,
    )
    .await;
    let requested_at = utc().unwrap_or_default();
    let received_at = utc().unwrap_or_default();

    let status = response.status;
    let headers = std::mem::take(&mut response.headers);
    let content_type = header(&headers, "content-type").to_string();
    let redacted = redact_headers(&headers);
    record.insert(
        "status_code".into(),
        status.map(|code| json!(code)).unwrap_or(Value::Null),
    );
    record.insert("content_type".into(), json!(content_type));
    record.insert("response_time".into(), json!(response.response_time));
    record.insert("x_robots".into(), json!(header(&headers, "x-robots-tag")));
    record.insert(
        "content_encoding".into(),
        json!(header(&headers, "content-encoding")),
    );
    record.insert("http_refresh".into(), json!(header(&headers, "refresh")));
    let location = header(&headers, "location").to_string();
    record.insert(
        "redirect_url".into(),
        json!(if status.is_some_and(|code| (300..400).contains(&code)) {
            redirect_target(url, &location)
        } else {
            String::new()
        }),
    );

    if let Some(kind) = response.error_kind {
        record.insert("error".into(), json!(response.error));
        record.insert("error_kind".into(), json!(kind.as_str()));
    } else {
        record.insert("error".into(), json!(""));
        record.insert("error_kind".into(), json!(""));
    }

    let raw_body = std::mem::take(&mut response.body);
    let size_bytes = raw_body.len();
    record.insert("size_bytes".into(), json!(size_bytes as u64));

    // Media-type scope filter, before any body handling. The transport size
    // stays recorded even though the body is never parsed.
    if let Some(reason) = scope_media_rejection(&content_type) {
        record.insert("body_unavailable".into(), json!(reason));
        let capture = capture_event(
            url,
            &requested_at,
            &received_at,
            &response,
            &redacted,
            &content_type,
            None,
            "unsupported_media",
            user_agent,
        );
        return finish(record, capture, None, size_bytes, headers, None);
    }

    if size_bytes as i64 > max_response_bytes {
        record.insert("error".into(), json!("response too large to parse"));
        let reason = if content_type.to_ascii_lowercase().contains("html") {
            "oversized"
        } else {
            ""
        };
        record.insert("body_unavailable".into(), json!(reason));
        let capture = capture_event(
            url,
            &requested_at,
            &received_at,
            &response,
            &redacted,
            &content_type,
            None,
            "truncated",
            user_agent,
        );
        return finish(record, capture, None, size_bytes, headers, None);
    }

    let is_html = content_type.to_ascii_lowercase().contains("html");
    let mut html_text: Option<String> = None;
    let parsed = if is_html && !raw_body.is_empty() {
        let decoded = spiderhead_store::bodies::decode_html(&raw_body, &content_type);
        let text = decoded.text;
        match parse_html(&text, url, parse_options) {
            Ok(parsed) => {
                record.insert("body_unavailable".into(), json!(""));
                for (key, value) in record::from_parsed(&parsed) {
                    record.insert(key, value);
                }
                let found = (parsed.jsonld.len() + parsed.jsonld_invalid.len()) as u64;
                record.insert("jsonld_blocks_found".into(), json!(found));
                record.insert(
                    "jsonld_blocks_parsed".into(),
                    json!(parsed.jsonld.len() as u64),
                );
                let text_len = parsed.text.len();
                let markup_len = record::markup_only_bytes(&text);
                record.insert(
                    "text_ratio".into(),
                    if markup_len > 0 {
                        json!(
                            ((text_len as f64) / (markup_len as f64) * 100.0 * 100.0).round()
                                / 100.0
                        )
                    } else {
                        Value::Null
                    },
                );
                html_text = Some(text);
                Some(parsed)
            }
            Err(_) => {
                record.insert("error_kind".into(), json!("decoding"));
                None
            }
        }
    } else {
        None
    };

    let entity = spiderhead_store::crawl_capture::bounded_entity(
        &raw_body,
        header(&headers, "content-encoding"),
        entity_capture_limit,
    )
    .ok();
    let capture = capture_event(
        url,
        &requested_at,
        &received_at,
        &response,
        &redacted,
        &content_type,
        entity,
        "none",
        user_agent,
    );
    finish(record, capture, parsed, size_bytes, headers, html_text)
}

fn finish(
    record: Map<String, Value>,
    capture: CaptureEvent,
    parsed: Option<ParsedPage>,
    size_bytes: usize,
    headers: Vec<(String, String)>,
    html: Option<String>,
) -> Fetched {
    Fetched {
        record,
        capture,
        parsed,
        size_bytes,
        headers,
        html,
    }
}

async fn raw_with_retries(
    client: &reqwest::Client,
    throttle: &mut Throttle,
    url: &str,
    user_agent: &str,
    retry_on_timeout: u32,
    retry_server_errors: u32,
    attempts: &mut u32,
) -> RawResponse {
    let mut response = dispatch(client, throttle, url, user_agent).await;
    *attempts += 1;
    // Timeout retries: only timeout-classified failures, each a paced request.
    while response.error_kind == Some(FetchErrorKind::Timeout) && *attempts <= retry_on_timeout {
        throttle.record(Outcome::Timeout, 0.0, None);
        response = dispatch(client, throttle, url, user_agent).await;
        *attempts += 1;
    }
    // 5xx retries are real requests at the widened delay; only the last response is the page.
    while response
        .status
        .is_some_and(|code| (500..600).contains(&code))
        && *attempts <= retry_server_errors
    {
        let retry_after = retry_after_of(&response);
        throttle.record(Outcome::ServerError, response.response_time, retry_after);
        response = dispatch(client, throttle, url, user_agent).await;
        *attempts += 1;
    }
    let outcome = match (response.status, response.error_kind) {
        (None, Some(FetchErrorKind::Timeout)) => Outcome::Timeout,
        (None, _) => Outcome::ClientError, // connection failures pace but do not retry
        (Some(code), _) if (200..300).contains(&code) => Outcome::Ok,
        (Some(429), _) => Outcome::RateLimited,
        (Some(code), _) if (500..600).contains(&code) => Outcome::ServerError,
        (Some(_), _) => Outcome::ClientError,
    };
    let retry_after = retry_after_of(&response);
    throttle.record(outcome, response.response_time, retry_after);
    response
}

/// Numeric `Retry-After` in seconds; negative values clamp to zero.
fn retry_after_of(response: &RawResponse) -> Option<f64> {
    let value = header(&response.headers, "retry-after").trim();
    if value.is_empty() {
        return None;
    }
    value.parse::<f64>().ok().map(|seconds| seconds.max(0.0))
}

async fn dispatch(
    client: &reqwest::Client,
    throttle: &mut Throttle,
    url: &str,
    user_agent: &str,
) -> RawResponse {
    throttle.wait().await;
    client::get(client, url, user_agent).await
}

pub(crate) fn base_record(url: &str) -> Map<String, Value> {
    let mut record = Map::new();
    record.insert("url".into(), json!(url));
    record.insert("status_code".into(), Value::Null);
    record.insert("response_time".into(), Value::Null);
    // Text fields default to empty, counters to 0, unmeasured flags stay null;
    // every `pages` column the store writer requires is set.
    let strings = [
        "content_type",
        "redirect_url",
        "title",
        "meta_description",
        "h1",
        "h1_2",
        "h2",
        "canonical",
        "meta_robots",
        "x_robots",
        "og_title",
        "og_description",
        "og_image",
        "og_url",
        "h1_alt_text",
        "meta_fragment",
        "content_encoding",
        "meta_refresh",
        "http_refresh",
        "charset",
        "doctype",
        "viewport",
        "amphtml",
        "final_canonical",
        "final_url",
        "error",
        "error_kind",
        "cache_status",
        "body_unavailable",
        "readability",
        "invalid_head_elements",
    ];
    for key in strings {
        record.insert(key.into(), json!(""));
    }
    let counters = [
        "word_count",
        "meta_description_count",
        "lorem_ipsum_count",
        "images_total",
        "images_missing_alt_attr",
        "images_max_alt_length",
        "plugin_elements",
        "mobile_alternate_broken",
        "ajax_scheme_outlinks",
        "crawl_depth",
        "content_frames",
        "content_frames_same_origin",
        "head_count",
        "body_count",
        "outlinks",
        "external_outlinks",
        "jsonld_blocks_found",
        "jsonld_blocks_parsed",
    ];
    for key in counters {
        record.insert(key.into(), json!(0));
    }
    // Optional measurements stay null until a parse fills them: unmeasured is not zero.
    for key in [
        "sentence_count",
        "avg_words_per_sentence",
        "flesch",
        "text_ratio",
    ] {
        record.insert(key.into(), Value::Null);
    }
    // The five document-position flags may stay null when unmeasured; the rest must be non-null booleans.
    for key in [
        "title_outside_head",
        "meta_description_outside_head",
        "canonical_outside_head",
        "directives_outside_head",
        "hreflang_outside_head",
    ] {
        record.insert(key.into(), Value::Null);
    }
    for key in ["transport_upgraded", "head_not_first"] {
        record.insert(key.into(), json!(false));
    }
    record.insert("representation".into(), json!("static"));
    let lists = [
        "hreflang",
        "heading_outline",
        "duplicate_ids",
        "canonical_chain",
        "redirect_chain",
    ];
    for key in lists {
        record.insert(key.into(), json!([]));
    }
    record.insert("trust_signals".into(), Value::Null);
    // link_placement keeps its empty object shape even when unparsed; duplicate_ids stays null.
    record.insert(
        "link_placement".into(),
        json!({
            "in_heading": [],
            "in_heading_total": 0,
            "image_no_text": [],
            "image_no_text_total": 0
        }),
    );
    record.insert("duplicate_ids".into(), Value::Null);
    // The rejected-href list stays null unless `discovery.record_invalid_targets` is enabled.
    record.insert("invalid_link_targets".into(), Value::Null);
    record
}

#[allow(clippy::too_many_arguments)]
fn capture_event(
    requested_url: &str,
    requested_at: &str,
    received_at: &str,
    response: &RawResponse,
    headers: &[(String, String)],
    content_type: &str,
    entity: Option<Vec<u8>>,
    body_reason: &str,
    user_agent: &str,
) -> CaptureEvent {
    let complete = entity.is_some() && response.error.is_empty();
    CaptureEvent {
        method: "GET".into(),
        requested_url: requested_url.to_string(),
        effective_url: requested_url.to_string(),
        redirect_history: Vec::new(),
        requested_at: requested_at.to_string(),
        received_at: received_at.to_string(),
        status_code: response.status.map(i64::from),
        request_headers: spiderhead_fetch::client::request_header_template(
            user_agent,
            &client::host_of(requested_url),
        ),
        credentials_used: false,
        response_headers: headers.to_vec(),
        content_type: content_type.to_string(),
        content_encoding: header(headers, "content-encoding").to_string(),
        entity_bytes: entity,
        body_fidelity: if complete {
            "entity_bytes".into()
        } else {
            "unavailable".into()
        },
        body_state: if complete {
            "complete".into()
        } else {
            "unavailable".into()
        },
        body_reason: body_reason.to_string(),
        error: response.error.clone(),
        error_kind: response
            .error_kind
            .map(|kind| kind.as_str().to_string())
            .unwrap_or_default(),
        effective_status_code: response.status.map(i64::from),
        effective_headers: headers.to_vec(),
        response_time: Some(response.response_time),
        session_changed: false,
        http_version: None,
        timing_state: "unavailable".into(),
        transport_phases: None,
    }
}

/// Parse options as a string-keyed map; the reference crawl passes them through unchanged.
pub type ParseOptionsMap = BTreeMap<String, Value>;

/// Rebuild one page's body-derived record fields from retained document text —
/// the fetch path's parse stage, replayed offline over stored evidence.
///
/// `size_bytes` stays the recorded transport size; an over-limit body keeps its
/// 200 with a `body_unavailable` marker instead of becoming an error.
pub(crate) fn apply_body(
    record: &mut Map<String, Value>,
    html: &str,
    url: &str,
    parse_options: &ParseOptions,
    max_response_bytes: i64,
) -> Option<ParsedPage> {
    if record
        .get("size_bytes")
        .and_then(Value::as_i64)
        .unwrap_or(0)
        > max_response_bytes
    {
        record.insert("error".into(), json!("response too large to parse"));
        let is_html = record
            .get("content_type")
            .and_then(Value::as_str)
            .map(|t| t.to_ascii_lowercase().contains("html"))
            .unwrap_or(false);
        if is_html {
            record.insert("body_unavailable".into(), json!("oversized"));
        }
        return None;
    }
    let is_html = record
        .get("content_type")
        .and_then(Value::as_str)
        .map(|t| t.to_ascii_lowercase().contains("html"))
        .unwrap_or(false);
    if !is_html || html.is_empty() {
        return None;
    }
    match parse_html(html, url, parse_options) {
        Ok(parsed) => {
            record.insert("body_unavailable".into(), json!(""));
            for (key, value) in record::from_parsed(&parsed) {
                record.insert(key, value);
            }
            let found = (parsed.jsonld.len() + parsed.jsonld_invalid.len()) as u64;
            record.insert("jsonld_blocks_found".into(), json!(found));
            record.insert(
                "jsonld_blocks_parsed".into(),
                json!(parsed.jsonld.len() as u64),
            );
            let text_len = parsed.text.len();
            let markup_len = record::markup_only_bytes(&parsed.text);
            record.insert(
                "text_ratio".into(),
                if markup_len > 0 {
                    json!(((text_len as f64) / (markup_len as f64) * 100.0 * 100.0).round() / 100.0)
                } else {
                    Value::Null
                },
            );
            Some(parsed)
        }
        Err(_) => {
            record.insert("error_kind".into(), json!("decoding"));
            None
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn charset_param_is_lowercased_and_unquoted() {
        assert_eq!(
            content_charset("text/html; charset=ISO-8859-1"),
            Some("iso-8859-1".to_string())
        );
        assert_eq!(
            content_charset("text/html; charset=\"utf-8\""),
            Some("utf-8".to_string())
        );
        assert_eq!(content_charset("text/html"), None);
    }

    #[test]
    fn redirect_target_joins_relative_locations() {
        assert_eq!(
            redirect_target("http://127.0.0.1:1/r301", "/final.html"),
            "http://127.0.0.1:1/final.html"
        );
        assert_eq!(redirect_target("http://127.0.0.1:1/x", ""), "");
    }
}
