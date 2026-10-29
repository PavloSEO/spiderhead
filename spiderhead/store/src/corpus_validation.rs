//! Read-only integrity validation for the retained scan corpus lanes.

use std::sync::OnceLock;

use flate2::{Decompress, FlushDecompress, Status};
use regex::Regex;
use rusqlite::Connection;
use serde_json::{json, Map, Value};
use sha2::{Digest, Sha256};
use spiderhead_core::cache_control::parse_cache_control;
use spiderhead_parse::py_url::urljoin;

use crate::bodies::{decode_entity, renderer as validate_renderer};
use crate::discovery_ledger::query_maps;
use crate::error::StoreError;
use crate::resources::validate_resources;
use crate::scan_artifact::ScanError;

/// Headers a retained corpus row must never carry.
pub const SENSITIVE_HEADERS: &[&str] = &[
    "authorization",
    "cookie",
    "proxy-authorization",
    "set-cookie",
];
/// Reasons a body may be omitted.
pub const OMITTED: &[&str] = &[
    "body_budget_exhausted",
    "cache_control_no_store",
    "credentialed",
    "not_enabled",
    "resource_budget_exhausted",
    "unsupported_media",
];
/// Reasons a body may be unavailable.
pub const UNAVAILABLE: &[&str] = &[
    "fetch_failed",
    "legacy_not_retained",
    "not_fetched",
    "not_in_corpus",
];
/// Regex source of a SHA-256 digest (full match).
pub const SHA256: &str = r"[0-9a-f]{64}\Z";

fn scan_error(message: impl Into<String>) -> StoreError {
    ScanError {
        message: message.into(),
    }
    .into()
}

fn is_sha256(text: &str) -> bool {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new("^[0-9a-f]{64}$").expect("static pattern"))
        .is_match(text)
}

/// Every row of `sql` as an object keyed by column name (BLOB cells become `null`).
pub fn iter_rows(con: &Connection, sql: &str) -> Result<Vec<Map<String, Value>>, StoreError> {
    query_maps(con, sql, [])
}

/// Parse JSON text, labelling the failure.
pub fn json(value: &Value, label: &str) -> Result<Value, StoreError> {
    let Some(text) = value.as_str() else {
        return Err(scan_error(format!("{label} must be JSON text")));
    };
    serde_json::from_str(text).map_err(|_| scan_error(format!("{label} is invalid JSON")))
}

/// Parse an ISO-8601 timestamp, treating a `Z` suffix as `+00:00`:
/// `None` when it does not parse, `Some(None)` when naive, `Some(Some(seconds))` with its
/// UTC offset otherwise.
pub fn iso_offset(value: &str) -> Option<Option<i64>> {
    static RE: OnceLock<Regex> = OnceLock::new();
    let re = RE.get_or_init(|| {
        Regex::new(
            r"^(\d{4})-?(\d{2})-?(\d{2})(?:[T ](\d{2})(?::?(\d{2})(?::?(\d{2})(?:[.,]\d+)?)?)?)?(?:([+-])(\d{2})(?::?(\d{2})(?::?(\d{2})(?:[.,]\d+)?)?)?)?$",
        )
        .expect("static pattern")
    });
    let text = value.replace('Z', "+00:00");
    let caps = re.captures(&text)?;
    let num = |i: usize| caps.get(i).and_then(|m| m.as_str().parse::<i64>().ok());
    let (year, month, day) = (num(1)?, num(2)?, num(3)?);
    let leap = (year % 4 == 0 && year % 100 != 0) || year % 400 == 0;
    let days = match month {
        1 | 3 | 5 | 7 | 8 | 10 | 12 => 31,
        4 | 6 | 9 | 11 => 30,
        2 => 28 + i64::from(leap),
        _ => return None,
    };
    if year == 0 || day < 1 || day > days {
        return None;
    }
    if num(4).is_some_and(|h| h > 23)
        || num(5).is_some_and(|m| m > 59)
        || num(6).is_some_and(|s| s > 59)
    {
        return None;
    }
    let Some(sign) = caps.get(7) else {
        return Some(None);
    };
    let (h, m, s) = (num(8)?, num(9).unwrap_or(0), num(10).unwrap_or(0));
    if h > 23 || m > 59 || s > 59 {
        return None;
    }
    let total = h * 3600 + m * 60 + s;
    Some(Some(if sign.as_str() == "-" { -total } else { total }))
}

/// Require an RFC 3339 UTC timestamp.
pub fn timestamp(value: &Value, label: &str, nullable: bool) -> Result<(), StoreError> {
    if value.is_null() && nullable {
        return Ok(());
    }
    let Some(text) = value.as_str() else {
        return Err(scan_error(format!("{label} must be an RFC3339 timestamp")));
    };
    match iso_offset(text) {
        None => Err(scan_error(format!("{label} is not an RFC3339 timestamp"))),
        Some(Some(0)) => Ok(()),
        Some(_) => Err(scan_error(format!("{label} must be UTC"))),
    }
}

/// Require an ordered list of `[name, value]` string pairs without credential headers.
pub fn headers(value: &Value, label: &str) -> Result<(), StoreError> {
    let pairs = json(value, label)?;
    let Some(pairs) = pairs.as_array() else {
        return Err(scan_error(format!(
            "{label} must be an ordered header-pair list"
        )));
    };
    for pair in pairs {
        let ok = pair.as_array().is_some_and(|p| {
            p.len() == 2
                && p.iter().all(Value::is_string)
                && !SENSITIVE_HEADERS.contains(&p[0].as_str().unwrap_or("").to_lowercase().as_str())
        });
        if !ok {
            return Err(scan_error(format!(
                "{label} contains an unredacted or malformed header"
            )));
        }
    }
    Ok(())
}

/// True when any `Cache-Control` header of the JSON pair list carries `no-store`.
pub fn has_no_store(value: &Value) -> Result<bool, StoreError> {
    let pairs = json(value, "response headers")?;
    Ok(pairs.as_array().into_iter().flatten().any(|pair| {
        let Some(p) = pair.as_array().filter(|p| p.len() == 2) else {
            return false;
        };
        match (p[0].as_str(), p[1].as_str()) {
            (Some(name), Some(directives)) => {
                name.to_lowercase() == "cache-control"
                    && parse_cache_control(directives).contains_key("no-store")
            }
            _ => false,
        }
    }))
}

/// Validate the body state, reason, hash and fidelity of a response or document row.
pub fn state(row: &Map<String, Value>, document: bool) -> Result<(), StoreError> {
    let get = |k: &str| row.get(k).unwrap_or(&Value::Null);
    let (state, reason, sha) = (get("body_state"), get("body_reason"), get("body_sha256"));
    let fidelity = get(if document {
        "fidelity"
    } else {
        "body_fidelity"
    });
    let reason = reason.as_str();
    let fidelity = fidelity.as_str();
    match state.as_str() {
        Some("complete") => {
            let allowed: &[&str] = if document {
                &["entity_bytes", "reencoded_text", "serialized_dom"]
            } else {
                &["entity_bytes", "reencoded_text"]
            };
            if sha.is_null()
                || !matches!(reason, Some("none" | "preexisting_cache_snapshot"))
                || !fidelity.is_some_and(|f| allowed.contains(&f))
            {
                return Err(scan_error(
                    "complete corpus row has invalid hash, reason, or fidelity",
                ));
            }
        }
        Some("truncated") => {
            if !sha.is_null() || reason != Some("truncated") || fidelity != Some("unavailable") {
                return Err(scan_error("truncated corpus row must retain no body"));
            }
        }
        Some("omitted") => {
            if !sha.is_null()
                || !reason.is_some_and(|r| OMITTED.contains(&r))
                || fidelity != Some("unavailable")
            {
                return Err(scan_error(
                    "omitted corpus row has an invalid reason or body",
                ));
            }
        }
        Some("unavailable") => {
            if !sha.is_null()
                || !reason.is_some_and(|r| UNAVAILABLE.contains(&r))
                || fidelity != Some("unavailable")
            {
                return Err(scan_error(
                    "unavailable corpus row has an invalid reason or body",
                ));
            }
        }
        _ => return Err(scan_error("unknown corpus body state")),
    }
    Ok(())
}

/// One selected `bodies` row with its BLOB and measured stored length.
#[derive(Debug, Clone, Default)]
pub struct BodyRow {
    /// Body hash.
    pub sha256: Option<String>,
    /// `identity` or `zlib`.
    pub codec: Option<String>,
    /// Declared decoded length.
    pub decoded_bytes: Option<i64>,
    /// Declared stored length.
    pub stored_bytes: Option<i64>,
    /// `length(data)` measured by SQLite.
    pub actual_size: Option<i64>,
    /// The stored BLOB.
    pub data: Option<Vec<u8>>,
}

/// Validate and decode one selected `bodies` row without a second lookup.
///
/// `validate_corpus` already performs a full scan. Keeping the BLOB with its metadata in that
/// scan preserves `read_body`'s byte-level checks while avoiding two indexed point queries per
/// retained body during resume.
pub fn decode_body_row(row: &BodyRow, max_decoded_bytes: i64) -> Result<Vec<u8>, StoreError> {
    let (Some(sha256), Some(codec), Some(decoded), Some(stored), Some(actual), Some(data)) = (
        row.sha256.as_deref().filter(|s| is_sha256(s)),
        row.codec.as_deref(),
        row.decoded_bytes,
        row.stored_bytes,
        row.actual_size,
        row.data.as_deref(),
    ) else {
        return Err(scan_error("body metadata or stored length is invalid"));
    };
    if decoded < 0 || stored < 0 || stored != actual {
        return Err(scan_error("body metadata or stored length is invalid"));
    }
    if decoded > max_decoded_bytes {
        return Err(scan_error("body unavailable: decoded byte limit exceeded"));
    }
    if stored > decoded {
        return Err(scan_error(
            "body stored size exceeds its decoded-byte declaration",
        ));
    }
    let raw: Vec<u8> = match codec {
        "identity" => {
            if data.len() as i64 != decoded {
                return Err(scan_error("identity body decoded length disagrees"));
            }
            data.to_vec()
        }
        "zlib" => {
            let mut decoder = Decompress::new(true);
            let limit = (max_decoded_bytes + 1) as usize;
            let mut out: Vec<u8> = Vec::new();
            let mut chunk = vec![0u8; 64 * 1024];
            let mut eof = false;
            while out.len() < limit {
                let consumed = decoder.total_in() as usize;
                let take = (limit - out.len()).min(chunk.len());
                let before = decoder.total_out();
                let status = decoder
                    .decompress(&data[consumed..], &mut chunk[..take], FlushDecompress::None)
                    .map_err(|_| scan_error("compressed body has an invalid zlib stream"))?;
                let produced = (decoder.total_out() - before) as usize;
                out.extend_from_slice(&chunk[..produced]);
                if status == Status::StreamEnd {
                    eof = true;
                    break;
                }
                if produced == 0 && decoder.total_in() as usize == consumed {
                    break;
                }
            }
            if out.len() as i64 > max_decoded_bytes {
                return Err(scan_error("compressed body exceeds decoded byte limit"));
            }
            let unused = data.len() - decoder.total_in() as usize;
            if !eof || unused != 0 || out.len() as i64 != decoded {
                return Err(scan_error(
                    "compressed body is truncated, trailing, or length-mismatched",
                ));
            }
            out
        }
        _ => return Err(scan_error("body codec is unsupported")),
    };
    let digest: String = Sha256::digest(&raw)
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect();
    if digest != sha256 {
        return Err(scan_error("body SHA-256 disagrees with decoded bytes"));
    }
    Ok(raw)
}

fn int(row: &Map<String, Value>, key: &str) -> Option<i64> {
    row.get(key).and_then(Value::as_i64)
}

fn url_of(con: &Connection, url_id: &Value) -> Result<Option<String>, StoreError> {
    let Some(id) = url_id.as_i64() else {
        return Ok(None);
    };
    Ok(con
        .query_row("SELECT url FROM urls WHERE url_id=?", [id], |r| {
            r.get::<_, String>(0)
        })
        .ok())
}

fn validate_response(
    con: &Connection,
    scan: &Map<String, Value>,
    row: &Map<String, Value>,
    retain_no_store: bool,
) -> Result<(), StoreError> {
    let get = |k: &str| row.get(k).unwrap_or(&Value::Null);
    timestamp(get("requested_at"), "response requested_at", false)?;
    timestamp(get("received_at"), "response received_at", true)?;
    headers(get("request_headers_redacted_json"), "request headers")?;
    headers(get("response_headers_redacted_json"), "response headers")?;
    headers(get("effective_headers_redacted_json"), "effective headers")?;
    if !get("variant_key").as_str().is_some_and(|v| !v.is_empty()) {
        return Err(scan_error(
            "response variant key must be a nonempty opaque string",
        ));
    }
    if get("transport_source").as_str() == Some("cache")
        && matches!(
            scan.get("source_kind").and_then(Value::as_str),
            Some("native" | "reanalysis")
        )
    {
        return Err(scan_error(
            "native corpus response cannot claim legacy cache transport",
        ));
    }
    state(row, false)?;
    let complete = get("body_state").as_str() == Some("complete");
    if complete
        && (get("credentials_used").as_i64() != Some(0)
            || (!retain_no_store
                && (has_no_store(get("response_headers_redacted_json"))?
                    || has_no_store(get("effective_headers_redacted_json"))?)))
    {
        return Err(scan_error(
            "credentialed or no-store response cannot retain a complete body",
        ));
    }
    let source = get("source_response_id");
    if int(row, "status_code") == Some(304) && complete && source.is_null() {
        return Err(scan_error(
            "complete 304 response requires a source response",
        ));
    }
    if !source.is_null() {
        let ordinal = int(row, "request_ordinal");
        let source_ordinal = int(row, "source_request_ordinal");
        let mismatch = source_ordinal.is_none()
            || source_ordinal >= ordinal
            || int(row, "status_code") != Some(304)
            || get("source_body_sha256") != get("body_sha256")
            || get("source_request_url_id") != get("request_url_id")
            || get("source_variant_key") != get("variant_key")
            || get("source_method") != get("method")
            || (complete
                && (get("source_body_state").as_str() != Some("complete")
                    || get("source_body_fidelity") != get("body_fidelity")
                    || get("source_effective_url_id") != get("effective_url_id")
                    || get("source_effective_status_code") != get("effective_status_code")));
        if mismatch {
            return Err(scan_error(
                "304 response source ordering or body lineage disagrees",
            ));
        }
    }
    let chain = json(get("redirect_chain_json"), "response redirect chain")?;
    let Some(chain) = chain.as_array() else {
        return Err(scan_error(
            "response redirect chain must be an ordered list",
        ));
    };
    let mut current = get("request_url_id").clone();
    let hop_keys = [
        "request_url_id",
        "status_code",
        "location_raw",
        "next_url_id",
        "blocked",
    ];
    for hop in chain {
        let valid = hop.as_object().is_some_and(|h| {
            h.len() == 5
                && hop_keys.iter().all(|k| h.contains_key(*k))
                && h["request_url_id"] == current
                && h["status_code"].is_i64()
                && h["location_raw"].is_string()
                && h["blocked"].is_boolean()
        });
        if !valid {
            return Err(scan_error(
                "response redirect hop is malformed or disconnected",
            ));
        }
        if !matches!(
            hop["status_code"].as_i64(),
            Some(301 | 302 | 303 | 307 | 308)
        ) {
            return Err(scan_error("redirect hop status is not an HTTP redirect"));
        }
        if hop["blocked"] == json!(true) {
            if !hop["next_url_id"].is_null() {
                return Err(scan_error("blocked redirect cannot name a next URL"));
            }
            current = Value::Null;
            break;
        }
        if !hop["next_url_id"].is_i64() {
            return Err(scan_error("redirect next URL is invalid"));
        }
        let request_url = url_of(con, &hop["request_url_id"])?;
        let next_url = url_of(con, &hop["next_url_id"])?;
        let joined = match (&request_url, hop["location_raw"].as_str()) {
            (Some(base), Some(raw)) => urljoin(base, raw).ok(),
            _ => None,
        };
        if request_url.is_none() || next_url.is_none() || joined != next_url {
            return Err(scan_error(
                "redirect hop location does not reproduce its next URL",
            ));
        }
        current = hop["next_url_id"].clone();
    }
    if complete && current != *get("effective_url_id") {
        return Err(scan_error(
            "complete response effective URL disagrees with redirect chain",
        ));
    }
    Ok(())
}

fn validate_document(con: &Connection, row: &Map<String, Value>) -> Result<(), StoreError> {
    let get = |k: &str| row.get(k).unwrap_or(&Value::Null);
    timestamp(get("captured_at"), "document captured_at", false)?;
    state(row, true)?;
    let renderer = validate_renderer(row, Some(con))?;
    let expected: [Value; 4] = if get("body_state").as_str() != Some("complete") {
        [
            json!("scan_decoder.v1"),
            json!("not_applicable"),
            json!("unknown"),
            json!("not_applicable"),
        ]
    } else if get("fidelity").as_str() == Some("serialized_dom") {
        [
            json!("scan_decoder.v1"),
            json!("renderer_utf8"),
            json!("utf-8"),
            json!("not_applicable"),
        ]
    } else if get("fidelity").as_str() == Some("reencoded_text") {
        [
            json!("scan_decoder.v1"),
            json!("legacy_unknown"),
            json!("unknown"),
            json!("unknown"),
        ]
    } else {
        let Some(content_type) = get("response_content_type").as_str() else {
            return Err(scan_error("entity document has no source response"));
        };
        let (_, decoder) = decode_entity(b"", content_type)?;
        let mut values = decoder.values().cloned();
        [
            values.next().unwrap_or(Value::Null),
            values.next().unwrap_or(Value::Null),
            values.next().unwrap_or(Value::Null),
            values.next().unwrap_or(Value::Null),
        ]
    };
    let actual = [
        "decoder_version",
        "decoder_source",
        "decoder_charset",
        "decoder_errors",
    ]
    .map(|k| get(k).clone());
    if actual != expected {
        return Err(scan_error(
            "document decoder metadata is inconsistent with its fidelity",
        ));
    }
    if !get("source_response_id").is_null() && !renderer.is_empty() {
        let navigation = renderer
            .get("navigation_url_id")
            .cloned()
            .unwrap_or(Value::Null);
        let final_url = renderer.get("final_url_id").cloned().unwrap_or(Value::Null);
        if get("response_request_url_id").is_null()
            || *get("response_request_url_id") != navigation
            || (get("body_state").as_str() == Some("complete")
                && *get("response_effective_url_id") != final_url)
        {
            return Err(scan_error(
                "renderer navigation disagrees with its source response",
            ));
        }
    }
    if matches!(
        get("fidelity").as_str(),
        Some("entity_bytes" | "reencoded_text")
    ) && get("body_state").as_str() == Some("complete")
    {
        let mut expected_request = get("url_id").clone();
        if get("representation").as_str() == Some("legacy_fragment") {
            let renderer = json(get("renderer_json"), "legacy fragment renderer")?;
            expected_request = renderer
                .get("navigation_url_id")
                .cloned()
                .unwrap_or(Value::Null);
            if !expected_request.is_i64() {
                return Err(scan_error(
                    "legacy fragment navigation URL provenance is invalid",
                ));
            }
        }
        if *get("response_request_url_id") != expected_request
            || get("response_body_sha256") != get("body_sha256")
            || get("response_body_state").as_str() != Some("complete")
        {
            return Err(scan_error(
                "document response URL, state, or hash lineage disagrees",
            ));
        }
    }
    Ok(())
}

/// Validate body/response/document lineage without mutating the artifact.
pub fn validate_corpus(
    con: &Connection,
    scan: &Map<String, Value>,
    policy: &Map<String, Value>,
    verify_bodies: bool,
) -> Result<(), StoreError> {
    let any = |sql: &str| -> Result<bool, StoreError> { Ok(con.prepare(sql)?.exists([])?) };
    if any("SELECT 1 FROM resource_refs LIMIT 1")?
        || any("SELECT 1 FROM context_items WHERE kind='resource_inventory' LIMIT 1")?
    {
        validate_resources(con)?;
    }
    let mode = policy.get("body_mode").and_then(Value::as_str);
    let cap = match policy.get("max_body_bytes") {
        None => Some(0),
        Some(v) => v.as_i64().filter(|c| *c >= 0 && v.is_i64()),
    };
    let (Some(mode), Some(cap)) = (
        mode.filter(|m| ["off", "captured_entity_bytes"].contains(m)),
        cap,
    ) else {
        return Err(scan_error("corpus body policy is invalid"));
    };
    if mode == "off" && any("SELECT 1 FROM bodies LIMIT 1")? {
        return Err(scan_error("off body policy cannot retain body BLOBs"));
    }
    let stored_total: i64 = con.query_row(
        "SELECT COALESCE(SUM(stored_bytes),0) FROM bodies",
        [],
        |r| r.get(0),
    )?;
    if policy
        .get("max_body_store_bytes")
        .and_then(Value::as_i64)
        .is_some_and(|m| stored_total > m)
    {
        return Err(scan_error(
            "retained body bytes exceed the recorded store budget",
        ));
    }
    {
        let mut stmt = con.prepare(
            "WITH serialized_dom AS (SELECT body_sha256 FROM documents WHERE fidelity='serialized_dom' AND body_sha256 IS NOT NULL GROUP BY body_sha256) SELECT b.sha256,b.codec,b.decoded_bytes,b.stored_bytes,length(data) AS actual_size,b.data,s.body_sha256 IS NOT NULL AS serialized_dom FROM bodies b LEFT JOIN serialized_dom s ON s.body_sha256=b.sha256",
        )?;
        let mut rows = stmt.query([])?;
        while let Some(r) = rows.next()? {
            let body = BodyRow {
                sha256: r.get::<_, Option<String>>(0).ok().flatten(),
                codec: r.get::<_, Option<String>>(1).ok().flatten(),
                decoded_bytes: r.get::<_, Option<i64>>(2).ok().flatten(),
                stored_bytes: r.get::<_, Option<i64>>(3).ok().flatten(),
                actual_size: r.get::<_, Option<i64>>(4).ok().flatten(),
                data: r.get::<_, Option<Vec<u8>>>(5).ok().flatten(),
            };
            let serialized_dom: i64 = r.get(6)?;
            let codec_ok = matches!(body.codec.as_deref(), Some("identity" | "zlib"));
            let decoded_ok = body.decoded_bytes.is_some_and(|d| d >= 0 && d <= cap);
            if !codec_ok || !decoded_ok || body.stored_bytes != body.actual_size {
                return Err(scan_error(
                    "body metadata exceeds policy or disagrees with stored size",
                ));
            }
            if verify_bodies {
                let raw = decode_body_row(&body, cap.min(64 * 1024 * 1024))?;
                if serialized_dom != 0 && std::str::from_utf8(&raw).is_err() {
                    return Err(scan_error("serialized DOM contains invalid UTF-8"));
                }
            }
        }
    }
    let config = scan
        .get("config_json")
        .and_then(|c| json(c, "scan configuration").ok())
        .unwrap_or_else(|| json!({}));
    let retain_no_store = config
        .get("evidence")
        .and_then(|e| e.get("retain_no_store_acknowledged"))
        == Some(&json!(true));
    crate::discovery_ledger::for_each_row(
        con,
        "SELECT r.*,p.request_ordinal AS source_request_ordinal,p.body_sha256 AS source_body_sha256,p.request_url_id AS source_request_url_id,p.variant_key AS source_variant_key,p.method AS source_method,p.body_state AS source_body_state,p.body_fidelity AS source_body_fidelity,p.effective_url_id AS source_effective_url_id,p.effective_status_code AS source_effective_status_code FROM responses r LEFT JOIN responses p ON p.response_id=r.source_response_id",
        [],
        &mut |row| {
            validate_response(con, scan, &row, retain_no_store)?;
            Ok(true)
        },
    )?;
    crate::discovery_ledger::for_each_row(
        con,
        "SELECT d.*,r.content_type AS response_content_type,r.request_url_id AS response_request_url_id,r.effective_url_id AS response_effective_url_id,r.effective_status_code AS response_effective_status_code,r.body_sha256 AS response_body_sha256,r.body_state AS response_body_state,r.body_fidelity AS response_body_fidelity FROM documents d LEFT JOIN responses r ON r.response_id=d.source_response_id",
        [],
        &mut |row| {
            validate_document(con, &row)?;
            Ok(true)
        },
    )?;
    crate::discovery_ledger::for_each_row(
        con,
        "SELECT p.url_id,p.representation,p.document_id,d.document_id AS selected_document_id,d.url_id AS selected_url_id,d.representation AS selected_representation FROM pages p LEFT JOIN documents d ON d.document_id=p.document_id",
        [],
        &mut |page| {
            if !page["document_id"].is_null()
                && (page["selected_document_id"].is_null()
                    || page["selected_url_id"] != page["url_id"]
                    || page["selected_representation"] != page["representation"])
            {
                return Err(scan_error(
                    "selected page document URL or representation disagrees",
                ));
            }
            Ok(true)
        },
    )?;
    for (table, page_column, representation_column) in [
        ("links", "source_url_id", "evidence_representation"),
        ("forms", "page_url_id", "evidence_representation"),
    ] {
        crate::discovery_ledger::for_each_row(
            con,
            &format!(
                "SELECT t.{page_column},t.evidence_representation,t.source_document_id,d.document_id AS source_document_exists,d.url_id AS source_document_url_id,d.representation AS source_document_representation FROM {table} t LEFT JOIN documents d ON d.document_id=t.source_document_id"
            ),
            [],
            &mut |row| {
                if !row["source_document_id"].is_null()
                    && (row["source_document_exists"].is_null()
                        || row["source_document_url_id"] != row[page_column]
                        || row["source_document_representation"] != row[representation_column])
                {
                    return Err(scan_error(format!(
                        "{table} source document representation disagrees"
                    )));
                }
                Ok(true)
            },
        )?;
    }
    Ok(())
}
