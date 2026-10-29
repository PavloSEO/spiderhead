//! Transaction-scoped writer for bounded captured transport observations.
//!

use crate::bodies::{decode_entity, encode_body, EncodedBody};
use crate::crawl_capture::{CaptureEvent, TRANSPORT_PHASES};
use crate::error::StoreError;
use crate::retention::validate_policy;
use indexmap::IndexMap;
use rusqlite::{params, Connection};
use serde_json::{json, Map, Value};
use sha2::{Digest, Sha256};
use std::collections::BTreeSet;

/// Copy of `crawl_settings::MAX_RENDER_PAGE_CONCURRENCY`, which the core module keeps private.
const MAX_RENDER_PAGE_CONCURRENCY: i64 = 16;

/// Header names whose values must never reach the artifact.
const SENSITIVE_HEADERS: &[&str] = &[
    "authorization",
    "cookie",
    "set-cookie",
    "proxy-authorization",
    "x-api-key",
];

/// Reject header pairs that carry a sensitive name.
fn checked_header_pairs(pairs: &[(String, String)], label: &str) -> Result<(), StoreError> {
    if pairs
        .iter()
        .any(|(name, _)| SENSITIVE_HEADERS.contains(&name.to_lowercase().as_str()))
    {
        return Err(StoreError::Invalid(format!(
            "captured {label} headers must be redacted string pairs"
        )));
    }
    Ok(())
}

/// Sorted for a stable output order.
pub const PURPOSES: &[&str] = &["page", "robots", "script", "sitemap", "stylesheet"];

/// Sorted for a stable output order.
pub const UNAVAILABLE: &[&str] = &[
    "fetch_failed",
    "legacy_not_retained",
    "not_fetched",
    "not_in_corpus",
];

/// Sorted for a stable output order.
pub const OMITTED: &[&str] = &[
    "body_budget_exhausted",
    "cache_control_no_store",
    "credentialed",
    "not_enabled",
    "resource_budget_exhausted",
    "unsupported_media",
];

/// Sorted for a stable output order.
pub const REDIRECT_KEYS: &[&str] = &[
    "blocked",
    "location_raw",
    "next_url",
    "request_url",
    "status_code",
];

pub fn text(value: &Value, label: &str, nonempty: bool) -> Result<String, StoreError> {
    match value {
        Value::String(text) if !nonempty || !text.is_empty() => Ok(text.clone()),
        _ => Err(StoreError::Invalid(format!(
            "captured {label} must be a{} string",
            if nonempty { " nonempty" } else { "" }
        ))),
    }
}

pub fn timestamp(value: &Value, label: &str) -> Result<String, StoreError> {
    let text = text(value, label, true)?;
    if !crate::crawl_events::is_rfc3339(&text, false) {
        return Err(StoreError::Invalid(format!(
            "captured {label} must be an RFC3339 timestamp"
        )));
    }
    Ok(text)
}

pub fn status(value: &Value, label: &str, nullable: bool) -> Result<Option<i64>, StoreError> {
    match value {
        Value::Null if nullable => Ok(None),
        Value::Number(number) => match number.as_i64() {
            Some(code) if (100..=599).contains(&code) => Ok(Some(code)),
            _ => Err(StoreError::Invalid(format!(
                "captured {label} must be an HTTP status code"
            ))),
        },
        _ => Err(StoreError::Invalid(format!(
            "captured {label} must be an HTTP status code"
        ))),
    }
}

pub fn intern_url(con: &Connection, value: &str) -> Result<i64, StoreError> {
    text(&Value::String(value.to_string()), "response URL", true)?;
    con.execute("INSERT OR IGNORE INTO urls(url) VALUES(?1)", [value])?;
    let url_id: Option<i64> =
        match con.query_row("SELECT url_id FROM urls WHERE url=?1", [value], |row| {
            row.get(0)
        }) {
            Ok(id) => Some(id),
            Err(rusqlite::Error::QueryReturnedNoRows) => None,
            Err(err) => return Err(err.into()),
        };
    url_id.ok_or_else(|| {
        StoreError::Invalid("captured response URL could not be interned".to_string())
    })
}

pub fn checked_pairs(pairs: &Value, label: &str) -> Result<Vec<(String, String)>, StoreError> {
    let items = pairs.as_array().ok_or_else(|| {
        StoreError::Invalid(format!(
            "captured {label} headers must be redacted string pairs"
        ))
    })?;
    let mut out = Vec::with_capacity(items.len());
    for item in items {
        let pair = match item.as_array().map(Vec::as_slice) {
            Some([name, value]) => match (name.as_str(), value.as_str()) {
                (Some(name), Some(value))
                    if !SENSITIVE_HEADERS.contains(&name.to_lowercase().as_str()) =>
                {
                    (name.to_string(), value.to_string())
                }
                _ => {
                    return Err(StoreError::Invalid(format!(
                        "captured {label} headers must be redacted string pairs"
                    )))
                }
            },
            _ => {
                return Err(StoreError::Invalid(format!(
                    "captured {label} headers must be redacted string pairs"
                )))
            }
        };
        out.push(pair);
    }
    Ok(out)
}

pub fn pairs(pairs: &[(String, String)], label: &str) -> Result<String, StoreError> {
    checked_header_pairs(pairs, label)?;
    let rows: Vec<[&str; 2]> = pairs
        .iter()
        .map(|(name, value)| [name.as_str(), value.as_str()])
        .collect();
    Ok(serde_json::to_string(&rows)?)
}

pub fn no_store(pairs: &[(String, String)]) -> bool {
    pairs.iter().any(|(name, value)| {
        name.to_lowercase() == "cache-control"
            && spiderhead_core::cache_control::parse_cache_control(value).contains_key("no-store")
    })
}

pub fn content_length(pairs: &[(String, String)]) -> Option<i64> {
    let values: Vec<&str> = pairs
        .iter()
        .filter(|(name, _)| name.to_lowercase() == "content-length")
        .map(|(_, value)| value.trim())
        .collect();
    if values.is_empty()
        || values
            .iter()
            .any(|v| !v.is_ascii() || v.is_empty() || !v.bytes().all(|b| b.is_ascii_digit()))
    {
        return None;
    }
    // Integers are unbounded: a value that overflows i64 still parses and then
    // counts as a conflicting value, so an unparseable member must reject the set.
    let parsed: Option<BTreeSet<i64>> = values.iter().map(|v| v.parse().ok()).collect();
    let parsed = parsed?;
    if parsed.len() == 1 {
        parsed.into_iter().next()
    } else {
        None
    }
}

pub fn declared_charset(content_type: &str) -> String {
    content_type
        .split(';')
        .skip(1)
        .find_map(|param| {
            let (key, value) = param.split_once('=')?;
            (key.trim().eq_ignore_ascii_case("charset"))
                .then(|| value.trim().trim_matches('"').to_ascii_lowercase())
        })
        .unwrap_or_default()
}

pub fn validate_event(event: &CaptureEvent) -> Result<(), StoreError> {
    let bad = |m: &str| StoreError::Invalid(m.to_string());
    text(&json!(event.method), "method", true)?;
    text(&json!(event.requested_url), "requested URL", true)?;
    timestamp(&json!(event.requested_at), "requested_at")?;
    timestamp(&json!(event.received_at), "received_at")?;
    status(&json!(event.status_code), "status", true)?;
    status(
        &json!(event.effective_status_code),
        "effective status",
        true,
    )?;
    checked_header_pairs(&event.request_headers, "request")?;
    checked_header_pairs(&event.response_headers, "response")?;
    checked_header_pairs(&event.effective_headers, "effective")?;
    if event
        .response_time
        .is_some_and(|t| !t.is_finite() || t < 0.0)
    {
        return Err(bad(
            "captured response time must be a finite nonnegative number",
        ));
    }
    if event
        .http_version
        .as_ref()
        .is_some_and(|v| v.chars().count() > 32)
    {
        return Err(bad("captured HTTP protocol is invalid"));
    }
    if event.timing_state != "partial" && event.timing_state != "unavailable" {
        return Err(bad("captured timing state is invalid"));
    }
    if let Some(phases) = &event.transport_phases {
        let names: BTreeSet<&str> = phases.keys().map(String::as_str).collect();
        if names != TRANSPORT_PHASES.iter().copied().collect() {
            return Err(bad("captured transport phases are invalid"));
        }
        for (name, phase) in phases {
            let keys: BTreeSet<&str> = phase.keys().map(String::as_str).collect();
            if keys != ["seconds", "state", "reason"].into_iter().collect() {
                return Err(StoreError::Invalid(format!(
                    "captured {name} phase has invalid fields"
                )));
            }
            let state = phase["state"].as_str();
            if !matches!(
                state,
                Some("measured" | "partial" | "unavailable" | "failed")
            ) {
                return Err(StoreError::Invalid(format!(
                    "captured {name} phase state is invalid"
                )));
            }
            let seconds = &phase["seconds"];
            if !seconds.is_null() && !seconds.as_f64().is_some_and(|s| s.is_finite() && s >= 0.0) {
                return Err(StoreError::Invalid(format!(
                    "captured {name} phase duration is invalid"
                )));
            }
            if state == Some("measured") && seconds.is_null() {
                return Err(StoreError::Invalid(format!(
                    "captured {name} phase duration is missing"
                )));
            }
            text(&phase["reason"], &format!("{name} phase reason"), false)?;
        }
    }
    for hop in &event.redirect_history {
        let keys: BTreeSet<&str> = hop.keys().map(String::as_str).collect();
        if keys != REDIRECT_KEYS.iter().copied().collect() {
            return Err(bad("captured redirect history has invalid fields"));
        }
        text(&hop["request_url"], "redirect request URL", true)?;
        status(&hop["status_code"], "redirect status", false)?;
        text(&hop["location_raw"], "redirect location", false)?;
        let Some(blocked) = hop["blocked"].as_bool() else {
            return Err(bad("captured redirect blocked flag must be boolean"));
        };
        if blocked {
            if !hop["next_url"].is_null() {
                return Err(bad("blocked redirect must not name a next URL"));
            }
        } else {
            text(&hop["next_url"], "redirect next URL", true)?;
        }
    }
    Ok(())
}

pub fn state(
    event: &CaptureEvent,
    purpose: &str,
    policy: &IndexMap<String, Value>,
    effective_headers: &[(String, String)],
    retain_no_store: bool,
) -> (String, String) {
    let owned = |a: &str, b: &str| (a.to_string(), b.to_string());
    if event.body_state == "truncated" || event.body_reason == "truncated" {
        return owned("truncated", "truncated");
    }
    if event.body_state == "omitted" && OMITTED.contains(&event.body_reason.as_str()) {
        return owned("omitted", &event.body_reason);
    }
    let Some(entity) = event
        .entity_bytes
        .as_ref()
        .filter(|_| event.body_state == "complete")
    else {
        let reason = if UNAVAILABLE.contains(&event.body_reason.as_str()) {
            event.body_reason.as_str()
        } else {
            "fetch_failed"
        };
        return owned("unavailable", reason);
    };
    if event.credentials_used {
        return owned("omitted", "credentialed");
    }
    if (no_store(&event.response_headers) || no_store(effective_headers)) && !retain_no_store {
        return owned("omitted", "cache_control_no_store");
    }
    if policy["body_mode"] == "off" {
        return owned("omitted", "not_enabled");
    }
    if purpose == "page" && !event.content_type.to_lowercase().contains("html") {
        return owned("omitted", "unsupported_media");
    }
    if (purpose == "script" || purpose == "stylesheet")
        && !crate::resources::media_matches(purpose, &event.content_type)
    {
        return owned("omitted", "unsupported_media");
    }
    if entity.len() as i64 > policy["max_body_bytes"].as_i64().unwrap_or(0) {
        return owned("omitted", "body_budget_exhausted");
    }
    owned("complete", "none")
}

pub fn redirect_chain(
    con: &Connection,
    history: &[IndexMap<String, Value>],
) -> Result<String, StoreError> {
    let mut rows: Vec<Value> = Vec::new();
    for hop in history {
        let request = intern_url(con, hop["request_url"].as_str().unwrap_or(""))?;
        let next = match hop["next_url"].as_str() {
            Some(u) => json!(intern_url(con, u)?),
            None => Value::Null,
        };
        rows.push(json!({
            "request_url_id": request,
            "status_code": hop["status_code"],
            "location_raw": hop["location_raw"],
            "next_url_id": next,
            "blocked": hop["blocked"],
        }));
    }
    Ok(serde_json::to_string(&rows)?)
}

/// Count once per writer connection, then follow transactional body changes.
///
/// TEMP state preserves the durable scan format and rolls back with its writes.
/// Reopened connections rebuild from retained evidence before enforcing budgets.
pub fn retained_body_bytes(con: &Connection) -> Result<i64, StoreError> {
    let present = con
        .prepare("SELECT 1 FROM sqlite_temp_master WHERE name='native_body_store_bytes'")?
        .exists([])?;
    if !present {
        con.execute_batch("SAVEPOINT native_body_bytes_init")?;
        let built = (|| -> Result<(), StoreError> {
            con.execute_batch("CREATE TEMP TABLE native_body_store_bytes(value INTEGER NOT NULL)")?;
            con.execute_batch(
                "INSERT INTO native_body_store_bytes SELECT COALESCE(SUM(stored_bytes),0) FROM bodies",
            )?;
            for (action, delta) in [
                ("INSERT", "NEW.stored_bytes"),
                ("DELETE", "-OLD.stored_bytes"),
                (
                    "UPDATE OF stored_bytes",
                    "NEW.stored_bytes-OLD.stored_bytes",
                ),
            ] {
                let name = action
                    .split_whitespace()
                    .next()
                    .unwrap_or("")
                    .to_lowercase();
                con.execute_batch(&format!(
                    "CREATE TEMP TRIGGER native_body_bytes_{name} AFTER {action} ON main.bodies BEGIN UPDATE native_body_store_bytes SET value=value+({delta}); END"
                ))?;
            }
            Ok(())
        })();
        if built.is_err() {
            con.execute_batch("ROLLBACK TO native_body_bytes_init")?;
        }
        con.execute_batch("RELEASE native_body_bytes_init")?;
        built?;
    }
    Ok(
        con.query_row("SELECT value FROM temp.native_body_store_bytes", [], |r| {
            r.get(0)
        })?,
    )
}

pub fn reserve_unique_bytes(
    con: &Connection,
    encoded: &EncodedBody,
    policy: &IndexMap<String, Value>,
) -> Result<bool, StoreError> {
    let existing = con
        .prepare("SELECT 1 FROM bodies WHERE sha256=?")?
        .exists([&encoded.sha256])?;
    if existing {
        return Ok(true);
    }
    let budget = policy["max_body_store_bytes"].as_i64().unwrap_or(0);
    if encoded.stored_bytes != 0 && retained_body_bytes(con)? + encoded.stored_bytes > budget {
        return Ok(false);
    }
    con.execute(
        "INSERT INTO bodies(sha256,codec,decoded_bytes,stored_bytes,data) VALUES(?,?,?,?,?)",
        params![
            encoded.sha256,
            encoded.codec,
            encoded.decoded_bytes,
            encoded.stored_bytes,
            encoded.data
        ],
    )?;
    Ok(true)
}

pub fn renderer_bool(value: &Value, label: &str) -> Result<bool, StoreError> {
    match value {
        Value::Bool(flag) => Ok(*flag),
        _ => Err(StoreError::Invalid(format!(
            "rendered {label} must be boolean"
        ))),
    }
}

pub fn renderer_count(value: &Value, label: &str) -> Result<i64, StoreError> {
    match value.as_i64() {
        Some(count) if count >= 0 && value.is_i64() => Ok(count),
        _ => Err(StoreError::Invalid(format!(
            "rendered {label} must be a nonnegative integer"
        ))),
    }
}

/// Translate the safe render adjunct result into the schema's provenance shape.
pub fn rendered_provenance(
    con: &Connection,
    renderer: &Value,
    navigation_transform: &str,
) -> Result<(String, bool, bool), StoreError> {
    let bad = |m: &str| StoreError::Invalid(m.to_string());
    let scan = |m: &str| -> StoreError {
        crate::scan_artifact::ScanError {
            message: m.to_string(),
        }
        .into()
    };
    let _ = bad;
    let Some(renderer) = renderer.as_object() else {
        return Err(scan("rendered document provenance must be an object"));
    };
    if navigation_transform != "direct" && navigation_transform != "legacy_escaped_fragment" {
        return Err(scan("rendered navigation transform is unsupported"));
    }
    let supplied = renderer
        .get("navigation_transform")
        .filter(|v| !v.is_null());
    if supplied.is_some_and(|t| t.as_str() != Some(navigation_transform)) {
        return Err(scan(
            "rendered navigation transform disagrees with representation",
        ));
    }
    if navigation_transform == "legacy_escaped_fragment" && supplied.is_none() {
        return Err(scan(
            "legacy-fragment renderer must declare its navigation transform",
        ));
    }
    let null = Value::Null;
    let field = |k: &str| renderer.get(k).unwrap_or(&null);
    let engine = text(field("engine"), "renderer engine", true).map_err(to_scan)?;
    let version = text(field("engine_version"), "renderer version", true).map_err(to_scan)?;
    let (Some(raw_settings), Some(raw_navigation)) = (
        field("settings").as_object(),
        field("navigation").as_object(),
    ) else {
        return Err(scan(
            "rendered document lacks settings or navigation provenance",
        ));
    };
    let empty = json!({});
    let policy_value = renderer.get("policy").unwrap_or(&empty);
    let (Some(raw_transforms), Some(raw_policy)) =
        (field("transforms").as_object(), policy_value.as_object())
    else {
        return Err(scan(
            "rendered document lacks transform or policy provenance",
        ));
    };
    let page_concurrency = match renderer.get("page_concurrency") {
        None => 1,
        Some(v) => match v.as_i64() {
            Some(n) if (1..=MAX_RENDER_PAGE_CONCURRENCY).contains(&n) && v.is_i64() => n,
            _ => {
                return Err(scan(&format!(
                    "rendered page_concurrency must be an integer between 1 and {MAX_RENDER_PAGE_CONCURRENCY}"
                )))
            }
        },
    };
    let viewport = raw_settings.get("viewport").and_then(Value::as_object);
    let viewport_ok = viewport.is_some_and(|v| {
        v.len() == 2
            && v.contains_key("width")
            && v.contains_key("height")
            && v.values()
                .all(|x| x.as_i64().is_some_and(|n| n >= 1) && x.is_i64())
    });
    if !viewport_ok {
        return Err(scan("rendered viewport provenance is invalid"));
    }
    let viewport = viewport.cloned().unwrap_or_default();
    let ratio = raw_settings.get("device_pixel_ratio").unwrap_or(&null);
    if !ratio.is_number() || !ratio.as_f64().is_some_and(|r| r.is_finite() && r > 0.0) {
        return Err(scan("rendered device-pixel-ratio provenance is invalid"));
    }
    let script_timeout = raw_settings.get("script_timeout_seconds").unwrap_or(&null);
    let resize_cap = raw_settings
        .get("resize_to_content_max_height_px")
        .unwrap_or(&null);
    if !script_timeout
        .as_f64()
        .is_some_and(|t| t.is_finite() && t >= 0.0)
        || !script_timeout.is_number()
        || !resize_cap.as_i64().is_some_and(|n| n >= 1)
        || !resize_cap.is_i64()
    {
        return Err(scan("rendered timing or resize provenance is invalid"));
    }
    let flag = |m: &Map<String, Value>, k: &str, label: &str| {
        renderer_bool(m.get(k).unwrap_or(&null), label).map_err(to_scan)
    };
    let count = |m: &Map<String, Value>, k: &str, label: &str| {
        renderer_count(m.get(k).unwrap_or(&null), label).map_err(to_scan)
    };
    let mut settings = Map::new();
    settings.insert(
        "viewport".into(),
        json!({"width": viewport["width"], "height": viewport["height"]}),
    );
    settings.insert("device_pixel_ratio".into(), ratio.clone());
    settings.insert(
        "mobile_emulation".into(),
        json!(flag(
            raw_settings,
            "mobile_emulation",
            "mobile-emulation setting"
        )?),
    );
    settings.insert(
        "touch_emulation".into(),
        json!(flag(
            raw_settings,
            "touch_emulation",
            "touch-emulation setting"
        )?),
    );
    settings.insert("script_timeout_seconds".into(), script_timeout.clone());
    settings.insert(
        "resize_to_content".into(),
        json!(flag(raw_settings, "resize_to_content", "resize setting")?),
    );
    settings.insert("resize_to_content_max_height_px".into(), resize_cap.clone());
    settings.insert(
        "persistent_profile".into(),
        json!(flag(
            raw_settings,
            "persistent_profile",
            "persistent-profile setting"
        )?),
    );
    let iframes_applied = count(raw_transforms, "flatten_iframes_applied", "iframe count")?;
    settings.insert(
        "transforms".into(),
        json!({
            "flatten_shadow_dom_requested": flag(raw_transforms, "flatten_shadow_dom_requested", "shadow-DOM request")?,
            "flatten_shadow_dom_applied": count(raw_transforms, "flatten_shadow_dom_applied", "shadow-DOM count")?,
            "flatten_iframes_requested": flag(raw_transforms, "flatten_iframes_requested", "iframe request")?,
            "flatten_iframes_applied": iframes_applied,
        }),
    );
    let nav = |k: &str, label: &str| {
        text(raw_navigation.get(k).unwrap_or(&null), label, true).map_err(to_scan)
    };
    let requested_url = nav("requested_url", "renderer requested URL")?;
    let final_url = nav("final_url", "renderer final URL")?;
    let wait_until = nav("wait_until", "renderer wait strategy")?;
    let navigation_timeout = raw_navigation.get("timeout_seconds").unwrap_or(&null);
    if !navigation_timeout.is_number()
        || !navigation_timeout
            .as_f64()
            .is_some_and(|t| t.is_finite() && t > 0.0)
    {
        return Err(scan("rendered navigation-timeout provenance is invalid"));
    }
    settings.insert(
        "navigation".into(),
        json!({"wait_until": wait_until, "timeout_seconds": navigation_timeout}),
    );
    let credentials_used = renderer_bool(
        raw_policy.get("credentials_used").unwrap_or(&json!(false)),
        "credential policy fact",
    )
    .map_err(to_scan)?;
    let cache_control_no_store = renderer_bool(
        raw_policy
            .get("cache_control_no_store")
            .unwrap_or(&json!(false)),
        "cache-control policy fact",
    )
    .map_err(to_scan)?;
    let mut intern_error: Option<StoreError> = None;
    let navigation = {
        let mut intern = |url: &str| -> u64 {
            match intern_url(con, url) {
                Ok(id) => id as u64,
                Err(e) => {
                    intern_error.get_or_insert(e);
                    0
                }
            }
        };
        spiderhead_parse::navigation::retain_navigation(
            &Value::Object(raw_navigation.clone()),
            &mut intern,
        )
    };
    if let Some(e) = intern_error {
        return Err(e);
    }
    let navigation =
        navigation.map_err(|e| scan(&format!("rendered navigation evidence is invalid: {e}")))?;
    // scan.v1 readers already accept extensible renderer settings, while the top-level renderer
    // envelope is closed. Keep rollback readers compatible.
    settings.insert("navigation_evidence".into(), navigation);
    // Asset summaries travel with their original renderer settings on offline replay. Never copy
    // source bodies or an unbounded arbitrary object into this adjunct.
    if let Some(assets) = raw_settings.get("asset_evidence").filter(|v| !v.is_null()) {
        if !assets.is_object() {
            return Err(scan("rendered asset evidence must be an object"));
        }
        if crate::discovery_ledger::py_dumps(assets, false, true).len() > 1024 * 1024 {
            return Err(scan(
                "rendered asset evidence exceeds the retained record budget",
            ));
        }
        settings.insert("asset_evidence".into(), assets.clone());
    }
    let mut payload = Map::new();
    payload.insert("engine".into(), json!(engine));
    payload.insert("engine_version".into(), json!(version));
    payload.insert("page_concurrency".into(), json!(page_concurrency));
    payload.insert("settings".into(), Value::Object(settings));
    payload.insert("flattened_iframes".into(), json!(iframes_applied != 0));
    payload.insert("capture_limitations".into(), json!([]));
    payload.insert(
        "navigation_url_id".into(),
        json!(intern_url(con, &requested_url)?),
    );
    payload.insert("final_url_id".into(), json!(intern_url(con, &final_url)?));
    payload.insert("navigation_transform".into(), json!(navigation_transform));
    Ok((
        serde_json::to_string(&Value::Object(payload))?,
        credentials_used,
        cache_control_no_store,
    ))
}

/// Error for a message produced by the shared validators.
fn to_scan(error: StoreError) -> StoreError {
    match error {
        StoreError::Invalid(message) => crate::scan_artifact::ScanError { message }.into(),
        other => other,
    }
}

pub fn rendered_input_state(
    html: &Value,
    body_state: &Value,
    body_reason: &Value,
) -> Result<(Option<Vec<u8>>, String, String), StoreError> {
    let scan = |m: &str| -> StoreError {
        crate::scan_artifact::ScanError {
            message: m.to_string(),
        }
        .into()
    };
    let state = text(body_state, "document body state", true).map_err(to_scan)?;
    let reason = text(body_reason, "document body reason", true).map_err(to_scan)?;
    if state == "complete" {
        if reason != "none" || html.is_null() {
            return Err(scan(
                "complete rendered document requires DOM bytes and reason none",
            ));
        }
        // Text is encoded as UTF-8; bytes arrive as an array of 0..=255 numbers.
        let raw: Vec<u8> = match html {
            Value::String(text) => text.as_bytes().to_vec(),
            Value::Array(items) => items
                .iter()
                .map(|v| v.as_u64().filter(|n| *n <= 255).map(|n| n as u8))
                .collect::<Option<Vec<u8>>>()
                .ok_or_else(|| scan("rendered DOM must be exact UTF-8 bytes or text"))?,
            _ => return Err(scan("rendered DOM must be exact UTF-8 bytes or text")),
        };
        if std::str::from_utf8(&raw).is_err() {
            return Err(scan("rendered DOM bytes are not valid UTF-8"));
        }
        return Ok((Some(raw), state, reason));
    }
    if !html.is_null() {
        return Err(scan(
            "non-complete rendered document cannot retain DOM bytes",
        ));
    }
    let ok = (state == "truncated" && reason == "truncated")
        || (state == "unavailable" && UNAVAILABLE.contains(&reason.as_str()))
        || (state == "omitted" && OMITTED.contains(&reason.as_str()));
    if ok {
        return Ok((None, state, reason));
    }
    Err(scan("rendered document body state or reason is invalid"))
}

/// Write one event inside the caller's transaction; never commits or associates pages.
#[allow(clippy::too_many_arguments)]
pub fn store_response(
    con: &Connection,
    event: &CaptureEvent,
    purpose: &str,
    policy: &Value,
    retain_no_store: bool,
    logical_url: Option<&str>,
    representation: &str,
    renderer: Option<&Value>,
) -> Result<(i64, Option<i64>), StoreError> {
    let scan = |m: &str| -> StoreError {
        crate::scan_artifact::ScanError {
            message: m.to_string(),
        }
        .into()
    };
    if !PURPOSES.contains(&purpose)
        || (representation != "static" && representation != "legacy_fragment")
    {
        return Err(scan(
            "captured response purpose or representation is unsupported",
        ));
    }
    if representation == "static" && renderer.is_some() {
        return Err(scan("static documents cannot carry renderer provenance"));
    }
    if representation == "legacy_fragment" && !renderer.is_some_and(Value::is_object) {
        return Err(scan(
            "legacy-fragment documents require renderer provenance",
        ));
    }
    if representation == "legacy_fragment" && purpose != "page" {
        return Err(scan("legacy-fragment response purpose must be page"));
    }
    if let Some(url) = logical_url {
        text(&json!(url), "logical URL", true).map_err(to_scan)?;
    }
    let policy = validate_policy(policy)?;
    validate_event(event).map_err(to_scan)?;
    let response_headers = &event.response_headers;
    let effective_status = event.effective_status_code.or(event.status_code);
    let effective_headers = if event.effective_headers.is_empty() {
        &event.response_headers
    } else {
        &event.effective_headers
    };
    if !event.redirect_history.is_empty() && event.effective_url.is_empty() {
        return Err(scan("captured redirect response lacks an effective URL"));
    }
    let request_url_id = intern_url(con, &event.requested_url)?;
    let effective_url_id = if event.effective_url.is_empty() {
        None
    } else {
        Some(intern_url(con, &event.effective_url)?)
    };
    let (mut body_state, mut body_reason) =
        state(event, purpose, &policy, effective_headers, retain_no_store);
    if body_state == "complete" && effective_url_id.is_none() {
        return Err(scan("complete captured response lacks an effective URL"));
    }
    let mut body_sha: Option<String> = None;
    let mut fidelity = "unavailable";
    if body_state == "complete" {
        let encoded = encode_body(event.entity_bytes.as_deref().unwrap_or(&[]))?;
        if reserve_unique_bytes(con, &encoded, &policy)? {
            body_sha = Some(encoded.sha256.clone());
            fidelity = "entity_bytes";
        } else {
            body_state = "omitted".into();
            body_reason = "body_budget_exhausted".into();
        }
    }
    let ordinal: i64 = con.query_row(
        "SELECT COALESCE(MAX(request_ordinal)+1,0) FROM responses",
        [],
        |r| r.get(0),
    )?;
    let request_headers = pairs(&event.request_headers, "request")?;
    let variant_key: String = {
        let rows: Value = serde_json::from_str(&request_headers)?;
        let body = serde_json::to_string(
            &json!({"method": event.method.to_uppercase(), "request_headers": rows}),
        )?;
        Sha256::digest(body.as_bytes())
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect()
    };
    let reported_size_bytes =
        content_length(response_headers).or_else(|| content_length(effective_headers));
    con.execute(
        "INSERT INTO responses(request_url_id,request_ordinal,effective_url_id,redirect_chain_json,method,purpose,requested_at,received_at,request_headers_redacted_json,credentials_used,variant_key,status_code,effective_status_code,response_headers_redacted_json,effective_headers_redacted_json,content_type,charset,content_encoding,reported_size_bytes,response_time,transport_source,cache_status,source_response_id,body_sha256,body_fidelity,body_state,body_reason,error,error_kind) VALUES(?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?)",
        params![
            request_url_id,
            ordinal,
            effective_url_id,
            redirect_chain(con, &event.redirect_history)?,
            event.method.to_uppercase(),
            purpose,
            event.requested_at,
            (!event.received_at.is_empty()).then_some(event.received_at.as_str()),
            request_headers,
            i64::from(event.credentials_used),
            variant_key,
            event.status_code,
            effective_status,
            pairs(response_headers, "response")?,
            pairs(effective_headers, "effective")?,
            event.content_type,
            declared_charset(&event.content_type),
            event.content_encoding,
            reported_size_bytes,
            event.response_time,
            "network",
            "",
            Option::<i64>::None,
            body_sha,
            fidelity,
            body_state,
            body_reason,
            event.error,
            event.error_kind
        ],
    )?;
    let response_id = con.last_insert_rowid();
    crate::transport::record(
        con,
        response_id,
        &crate::transport::TransportEvent {
            http_version: event.http_version.clone(),
            timing_state: event.timing_state.clone(),
            transport_phases: event.transport_phases.clone().unwrap_or_default(),
            response_time: event.response_time,
        },
    )?;
    let mut document_id = None;
    if purpose == "page" {
        let url_id = intern_url(con, logical_url.unwrap_or(&event.requested_url))?;
        let decoder: Map<String, Value> = if body_state == "complete" {
            decode_entity(
                event.entity_bytes.as_deref().unwrap_or(&[]),
                &event.content_type,
            )?
            .1
        } else {
            [
                ("decoder_version", "scan_decoder.v1"),
                ("decoder_source", "not_applicable"),
                ("decoder_charset", "unknown"),
                ("decoder_errors", "not_applicable"),
            ]
            .into_iter()
            .map(|(k, v)| (k.to_string(), json!(v)))
            .collect()
        };
        let mut renderer_json = "{}".to_string();
        if representation == "legacy_fragment" {
            let (raw, _credentials, _no_store) = rendered_provenance(
                con,
                renderer.unwrap_or(&Value::Null),
                "legacy_escaped_fragment",
            )?;
            let mut data: Value = serde_json::from_str(&raw)?;
            if data["navigation_url_id"] != json!(request_url_id) {
                return Err(scan(
                    "legacy-fragment renderer navigation URL must match response request URL",
                ));
            }
            data["capture_limitations"] = if body_state == "complete" {
                json!([])
            } else {
                json!([body_reason])
            };
            renderer_json = serde_json::to_string(&data)?;
        }
        let captured_at = if event.received_at.is_empty() {
            &event.requested_at
        } else {
            &event.received_at
        };
        let get = |k: &str| decoder[k].as_str().unwrap_or("").to_string();
        con.execute(
            "INSERT INTO documents(url_id,representation,source_response_id,body_sha256,captured_at,decoder_version,decoder_source,decoder_charset,decoder_errors,fidelity,body_state,body_reason,renderer_json) VALUES(?,?,?,?,?,?,?,?,?,?,?,?,?)",
            params![
                url_id,
                representation,
                response_id,
                body_sha,
                captured_at,
                get("decoder_version"),
                get("decoder_source"),
                get("decoder_charset"),
                get("decoder_errors"),
                fidelity,
                body_state,
                body_reason,
                renderer_json
            ],
        )?;
        document_id = Some(con.last_insert_rowid());
    }
    Ok((response_id, document_id))
}

/// Store one browser-returned serialized DOM inside the caller's transaction.
///
/// This is intentionally separate from :func:`store_response`: a browser DOM is not an
/// HTTP entity and therefore cannot acquire a fabricated response observation or charset.
#[allow(clippy::too_many_arguments)]
pub fn store_rendered_document(
    con: &Connection,
    logical_url: &str,
    html: &Value,
    renderer: &Value,
    policy: &Value,
    retain_no_store: bool,
    captured_at: &str,
    body_state: &str,
    body_reason: &str,
) -> Result<i64, StoreError> {
    text(&json!(logical_url), "logical URL", true).map_err(to_scan)?;
    timestamp(&json!(captured_at), "rendered captured_at").map_err(to_scan)?;
    let policy = validate_policy(policy)?;
    let (raw, mut stored_state, mut stored_reason) =
        rendered_input_state(html, &json!(body_state), &json!(body_reason))?;
    let (renderer_json, credentials_used, cache_control_no_store) =
        rendered_provenance(con, renderer, "direct")?;
    let mut body_sha: Option<String> = None;
    if stored_state == "complete" {
        let raw = raw.unwrap_or_default();
        let omit = |state: &mut String, reason: &mut String, why: &str| {
            *state = "omitted".into();
            *reason = why.into();
        };
        if credentials_used {
            omit(&mut stored_state, &mut stored_reason, "credentialed");
        } else if cache_control_no_store && !retain_no_store {
            omit(
                &mut stored_state,
                &mut stored_reason,
                "cache_control_no_store",
            );
        } else if policy["body_mode"] == "off" {
            omit(&mut stored_state, &mut stored_reason, "not_enabled");
        } else if raw.len() as i64 > policy["max_body_bytes"].as_i64().unwrap_or(0) {
            omit(
                &mut stored_state,
                &mut stored_reason,
                "body_budget_exhausted",
            );
        } else {
            let encoded = encode_body(&raw)?;
            if reserve_unique_bytes(con, &encoded, &policy)? {
                body_sha = Some(encoded.sha256.clone());
            } else {
                omit(
                    &mut stored_state,
                    &mut stored_reason,
                    "body_budget_exhausted",
                );
            }
        }
    }
    let complete = stored_state == "complete";
    let mut renderer_data: Value = serde_json::from_str(&renderer_json)?;
    renderer_data["capture_limitations"] = if complete {
        json!([])
    } else {
        json!([stored_reason])
    };
    con.execute(
        "INSERT INTO documents(url_id,representation,source_response_id,body_sha256,captured_at,decoder_version,decoder_source,decoder_charset,decoder_errors,fidelity,body_state,body_reason,renderer_json) VALUES(?,?,?,?,?,?,?,?,?,?,?,?,?)",
        params![
            intern_url(con, logical_url)?,
            "rendered",
            Option::<i64>::None,
            body_sha,
            captured_at,
            "scan_decoder.v1",
            if complete { "renderer_utf8" } else { "not_applicable" },
            if complete { "utf-8" } else { "unknown" },
            "not_applicable",
            if complete { "serialized_dom" } else { "unavailable" },
            stored_state,
            stored_reason,
            serde_json::to_string(&renderer_data)?
        ],
    )?;
    Ok(con.last_insert_rowid())
}

/// Count the HTML page bodies this corpus kept, and name why it dropped the rest.
///
/// ``capabilities['html_bodies']`` says ``partial`` whether one body is missing or
/// four fifths of them are, so a run summary cannot be built from it.
/// A crawl can report pages and links normally while most bodies were discarded, and
/// anything computed from stored HTML would silently describe only the bodies that remain.
/// These are the counts that make that visible.
pub fn html_body_retention(con: &Connection) -> Result<Map<String, Value>, StoreError> {
    let mut stmt = con.prepare(
        "SELECT d.body_state,d.body_reason,COUNT(*) FROM documents d LEFT JOIN responses r ON r.response_id=d.source_response_id WHERE d.representation IN ('static','legacy_fragment') AND lower(COALESCE(r.content_type,'')) LIKE '%html%' GROUP BY 1,2 ORDER BY 1,2",
    )?;
    let rows: Vec<(String, String, i64)> = stmt
        .query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))?
        .collect::<Result<_, _>>()?;
    Ok(retention_counts(&rows, |state| state == "omitted"))
}

/// `{total, retained, omitted}` of grouped `(state, reason, count)` rows.
fn retention_counts(
    rows: &[(String, String, i64)],
    dropped: impl Fn(&str) -> bool,
) -> Map<String, Value> {
    let total: i64 = rows.iter().map(|r| r.2).sum();
    let mut omitted = Map::new();
    let mut omitted_total = 0;
    for (state, reason, count) in rows {
        if dropped(state) {
            omitted.insert(reason.clone(), json!(count));
            omitted_total += count;
        }
    }
    let mut out = Map::new();
    out.insert("total".into(), json!(total));
    out.insert("retained".into(), json!(total - omitted_total));
    out.insert("omitted".into(), Value::Object(omitted));
    out
}

/// Derive capability coverage over requested captures, not unrequested lanes.
pub fn corpus_summary(con: &Connection, policy: &Value) -> Result<Map<String, Value>, StoreError> {
    let policy = validate_policy(policy)?;
    let response_total: i64 = con.query_row("SELECT COUNT(*) FROM responses", [], |r| r.get(0))?;
    let missing_pages: i64 = con.query_row(
        "SELECT COUNT(*) FROM pages p WHERE NOT EXISTS(SELECT 1 FROM responses r WHERE r.request_url_id=p.url_id AND r.purpose='page')",
        [],
        |r| r.get(0),
    )?;
    let responses = if response_total == 0 {
        json!({"state": "unavailable", "reason": "no captured responses"})
    } else if missing_pages > 0 {
        json!({"state": "partial", "reason": "some page response observations are missing"})
    } else {
        json!({"state": "complete", "reason": ""})
    };
    let body_state = |filter: &str, label: &str| -> Result<Value, StoreError> {
        if policy["body_mode"] == "off" {
            return Ok(json!({"state": "unavailable", "reason": "body retention is disabled"}));
        }
        let (total, complete): (i64, i64) = con.query_row(
            &format!(
                "SELECT COUNT(*),COALESCE(SUM(d.body_state='complete'),0) FROM documents d LEFT JOIN responses r ON r.response_id=d.source_response_id WHERE {filter}"
            ),
            [],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )?;
        Ok(if total == 0 {
            json!({"state": "unavailable", "reason": format!("no captured {label}")})
        } else if complete == total && missing_pages == 0 {
            json!({"state": "complete", "reason": ""})
        } else {
            json!({"state": "partial", "reason": format!("some {label} are missing or not retained")})
        })
    };
    let html = body_state(
        "d.representation IN ('static','legacy_fragment') AND lower(COALESCE(r.content_type,'')) LIKE '%html%'",
        "HTML documents",
    )?;
    let rendered = body_state("d.representation='rendered'", "rendered documents")?;
    let missing_body = con
        .prepare("SELECT 1 FROM responses WHERE body_state!='complete' UNION ALL SELECT 1 FROM documents WHERE body_state!='complete' LIMIT 1")?
        .exists([])?;
    let retained = con.prepare("SELECT 1 FROM bodies LIMIT 1")?.exists([])?;
    let mut capabilities = Map::new();
    capabilities.insert("responses".into(), responses);
    capabilities.insert("html_bodies".into(), html);
    capabilities.insert("rendered_bodies".into(), rendered);
    let mut corpus_partial =
        policy["body_mode"] == "off" || !retained || missing_pages > 0 || missing_body;
    let config: Value = match con.query_row("SELECT config_json FROM scan", [], |r| {
        r.get::<_, String>(0)
    }) {
        Ok(text) => serde_json::from_str(&text)?,
        Err(rusqlite::Error::QueryReturnedNoRows) => json!({}),
        Err(e) => return Err(e.into()),
    };
    if let Some(resources) = config.get("resources") {
        let fetch = resources
            .get("fetch")
            .is_some_and(crate::ledger::ingest::truthy);
        let states = crate::resources::resource_capabilities(con, fetch)?;
        for (name, state) in &states {
            capabilities.insert(name.clone(), serde_json::to_value(state)?);
        }
        if fetch
            && states
                .get("resource_bodies")
                .and_then(|s| s.get("state"))
                .map(String::as_str)
                != Some("complete")
        {
            corpus_partial = true;
        }
    }
    let mut summary = Map::new();
    summary.insert("capabilities".into(), Value::Object(capabilities));
    summary.insert("corpus_partial".into(), json!(corpus_partial));
    Ok(summary)
}

/// Count the rendered DOMs this corpus kept, and name why it dropped the rest.
///
/// ``capabilities['rendered_bodies']`` says ``partial`` whether one DOM is missing
/// or every one of them is, so a run summary cannot be built from it.
/// A crawl can discard a DOM for every page whose session cookie came back on one
/// subresource request while still reporting pages, links and escalation as complete.
/// counts as complete. These counts make that visible, and
/// they are read back out of the artifact, so a finished scan still exposes it.
/// answers the same question afterwards.
pub fn rendered_body_retention(con: &Connection) -> Result<Map<String, Value>, StoreError> {
    let mut stmt = con.prepare(
        "SELECT body_state,body_reason,COUNT(*) FROM documents WHERE representation='rendered' GROUP BY 1,2 ORDER BY 1,2",
    )?;
    let rows: Vec<(String, String, i64)> = stmt
        .query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))?
        .collect::<Result<_, _>>()?;
    Ok(retention_counts(&rows, |state| state != "complete"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    // Behaviour of the pure writer helpers (text, status, pairs,
    // content length, declared charset, renderer count).
    #[test]
    fn text_requires_a_string_and_honours_nonempty() {
        assert_eq!(text(&json!("GET"), "method", true).expect("text"), "GET");
        assert!(text(&json!(""), "method", true).is_err());
        assert!(text(&json!(""), "location", false).is_ok());
        assert!(text(&json!(5), "location", false).is_err());
    }

    #[test]
    fn status_accepts_only_http_status_codes() {
        assert_eq!(status(&json!(200), "status", true).expect("ok"), Some(200));
        assert_eq!(status(&Value::Null, "status", true).expect("null"), None);
        assert!(status(&Value::Null, "status", false).is_err());
        assert!(status(&json!(99), "status", true).is_err());
        assert!(status(&json!(600), "status", true).is_err());
    }

    #[test]
    fn checked_pairs_rejects_sensitive_names() {
        assert!(checked_pairs(&json!([["Accept", "text/html"]]), "request").is_ok());
        assert!(checked_pairs(&json!([["Set-Cookie", "x"]]), "response").is_err());
        assert!(checked_pairs(&json!("not pairs"), "response").is_err());
    }

    #[test]
    fn pairs_serialises_compactly_and_refuses_secrets() {
        let rows = vec![("Accept".to_string(), "text/html".to_string())];
        assert_eq!(
            pairs(&rows, "request").expect("pairs"),
            r#"[["Accept","text/html"]]"#
        );
        let secret = vec![("Cookie".to_string(), "sid".to_string())];
        assert!(pairs(&secret, "request").is_err());
    }

    #[test]
    fn content_length_needs_one_consistent_digit_value() {
        let one = vec![("Content-Length".to_string(), " 42 ".to_string())];
        assert_eq!(content_length(&one), Some(42));
        let conflict = vec![
            ("content-length".to_string(), "1".to_string()),
            ("Content-Length".to_string(), "2".to_string()),
        ];
        assert_eq!(content_length(&conflict), None);
        let signed = vec![("content-length".to_string(), "+4".to_string())];
        assert_eq!(content_length(&signed), None);
    }

    #[test]
    fn declared_charset_is_lowercased_and_unquoted() {
        assert_eq!(declared_charset("text/html; charset=\"UTF-8\""), "utf-8");
        assert_eq!(declared_charset("text/html"), "");
    }

    #[test]
    fn no_store_reads_cache_control_directives() {
        let pairs = vec![("Cache-Control".to_string(), "private, no-store".to_string())];
        assert!(no_store(&pairs));
        let plain = vec![("Cache-Control".to_string(), "max-age=60".to_string())];
        assert!(!no_store(&plain));
    }

    fn event() -> CaptureEvent {
        CaptureEvent {
            method: "GET".into(),
            requested_url: "https://a.test/".into(),
            effective_url: "https://a.test/".into(),
            redirect_history: vec![],
            requested_at: "2026-01-01T00:00:00Z".into(),
            received_at: "2026-01-01T00:00:01Z".into(),
            status_code: Some(200),
            request_headers: vec![],
            credentials_used: false,
            response_headers: vec![],
            content_type: "text/html; charset=utf-8".into(),
            content_encoding: String::new(),
            entity_bytes: Some(b"<html></html>".to_vec()),
            body_fidelity: "entity_bytes".into(),
            body_state: "complete".into(),
            body_reason: "none".into(),
            error: String::new(),
            error_kind: String::new(),
            effective_status_code: None,
            effective_headers: vec![],
            response_time: Some(0.1),
            session_changed: false,
            http_version: None,
            timing_state: "unavailable".into(),
            transport_phases: None,
        }
    }

    fn policy(mode: &str, max_body: i64, max_store: i64) -> IndexMap<String, Value> {
        validate_policy(&json!({
            "policy_version": "scan_retention.v1",
            "body_mode": mode,
            "max_body_bytes": max_body,
            "max_body_store_bytes": max_store,
            "min_free_bytes": 0,
            "history_warning_bytes": 0,
            "automatic_delete": false,
        }))
        .expect("policy")
    }

    #[test]
    fn body_state_follows_the_retention_policy() {
        let on = policy("captured_entity_bytes", 100, 1000);
        let off = policy("off", 0, 0);
        let e = event();
        assert_eq!(
            state(&e, "page", &on, &e.response_headers, false),
            ("complete".into(), "none".into())
        );
        assert_eq!(
            state(&e, "page", &off, &e.response_headers, false),
            ("omitted".into(), "not_enabled".into())
        );
        let mut big = event();
        big.entity_bytes = Some(vec![b'x'; 101]);
        assert_eq!(
            state(&big, "page", &on, &[], false).1,
            "body_budget_exhausted"
        );
        let mut image = event();
        image.content_type = "image/png".into();
        assert_eq!(
            state(&image, "page", &on, &[], false).1,
            "unsupported_media"
        );
        let mut secret = event();
        secret.credentials_used = true;
        assert_eq!(state(&secret, "page", &on, &[], false).1, "credentialed");
        let mut cache = event();
        cache.response_headers = vec![("Cache-Control".into(), "no-store".into())];
        assert_eq!(
            state(&cache, "page", &on, &cache.response_headers, false).1,
            "cache_control_no_store"
        );
        assert_eq!(
            state(&cache, "page", &on, &cache.response_headers, true).0,
            "complete"
        );
        let mut cut = event();
        cut.body_state = "truncated".into();
        assert_eq!(
            state(&cut, "page", &on, &[], false),
            ("truncated".into(), "truncated".into())
        );
        let mut gone = event();
        gone.entity_bytes = None;
        gone.body_reason = "whatever".into();
        assert_eq!(
            state(&gone, "page", &on, &[], false),
            ("unavailable".into(), "fetch_failed".into())
        );
        let mut css = event();
        css.content_type = "text/css".into();
        assert_eq!(state(&css, "stylesheet", &on, &[], false).0, "complete");
        assert_eq!(
            state(&css, "script", &on, &[], false).1,
            "unsupported_media"
        );
    }

    #[test]
    fn events_are_validated_field_by_field() {
        assert!(validate_event(&event()).is_ok());
        type Mutation = Box<dyn Fn(&mut CaptureEvent)>;
        let cases: Vec<(&str, Mutation)> = vec![
            ("method", Box::new(|e| e.method.clear())),
            (
                "requested_at",
                Box::new(|e| e.requested_at = "yesterday".into()),
            ),
            ("status", Box::new(|e| e.status_code = Some(99))),
            (
                "response headers",
                Box::new(|e| e.response_headers = vec![("Set-Cookie".into(), "x".into())]),
            ),
            ("response time", Box::new(|e| e.response_time = Some(-1.0))),
            (
                "protocol",
                Box::new(|e| e.http_version = Some("h".repeat(33))),
            ),
            (
                "timing state",
                Box::new(|e| e.timing_state = "measured".into()),
            ),
        ];
        for (needle, mutate) in cases {
            let mut e = event();
            mutate(&mut e);
            let err = validate_event(&e).unwrap_err().to_string();
            assert!(err.contains(needle), "{needle}: {err}");
        }
        let mut hop = event();
        hop.redirect_history = vec![[("request_url".to_string(), json!("https://a.test/"))]
            .into_iter()
            .collect()];
        assert!(validate_event(&hop)
            .unwrap_err()
            .to_string()
            .contains("redirect history has invalid fields"));
    }

    #[test]
    fn rendered_input_states_are_strict() {
        let ok =
            rendered_input_state(&json!("<p>é</p>"), &json!("complete"), &json!("none")).unwrap();
        assert_eq!(ok.0.as_deref(), Some("<p>é</p>".as_bytes()));
        assert!(rendered_input_state(&Value::Null, &json!("complete"), &json!("none")).is_err());
        assert!(rendered_input_state(&json!("x"), &json!("complete"), &json!("other")).is_err());
        assert!(
            rendered_input_state(&json!([255, 254]), &json!("complete"), &json!("none")).is_err()
        );
        assert!(
            rendered_input_state(&json!("x"), &json!("omitted"), &json!("not_enabled")).is_err()
        );
        assert_eq!(
            rendered_input_state(&Value::Null, &json!("omitted"), &json!("not_enabled")).unwrap(),
            (None, "omitted".into(), "not_enabled".into())
        );
        assert!(rendered_input_state(&Value::Null, &json!("omitted"), &json!("weird")).is_err());
    }

    fn scratch() -> Connection {
        let con = Connection::open_in_memory().unwrap();
        con.execute_batch(crate::schema_sql::SCAN_V1_SQL).unwrap();
        con
    }

    #[test]
    fn body_store_budget_counts_unique_bytes_once() {
        let con = scratch();
        let p = policy("captured_entity_bytes", 1000, 10);
        let a = encode_body(b"aaaaaaaa").unwrap();
        assert!(reserve_unique_bytes(&con, &a, &p).unwrap());
        assert!(
            reserve_unique_bytes(&con, &a, &p).unwrap(),
            "an identical body is free"
        );
        assert_eq!(retained_body_bytes(&con).unwrap(), a.stored_bytes);
        let b = encode_body(b"bbbbbbbbbbbbbbbbbbbbbbbb-different").unwrap();
        assert!(
            !reserve_unique_bytes(&con, &b, &p).unwrap(),
            "over the store budget"
        );
        let rows: i64 = con
            .query_row("SELECT COUNT(*) FROM bodies", [], |r| r.get(0))
            .unwrap();
        assert_eq!(rows, 1);
        // the TEMP counter follows later transactional changes
        con.execute("DELETE FROM bodies", []).unwrap();
        assert_eq!(retained_body_bytes(&con).unwrap(), 0);
    }

    #[test]
    fn redirect_chain_interns_hops() {
        let con = scratch();
        let hop: IndexMap<String, Value> = [
            ("request_url", json!("https://a.test/old")),
            ("status_code", json!(301)),
            ("location_raw", json!("/new")),
            ("next_url", json!("https://a.test/new")),
            ("blocked", json!(false)),
        ]
        .into_iter()
        .map(|(k, v)| (k.to_string(), v))
        .collect();
        let chain = redirect_chain(&con, &[hop]).unwrap();
        assert_eq!(
            chain,
            r#"[{"request_url_id":1,"status_code":301,"location_raw":"/new","next_url_id":2,"blocked":false}]"#
        );
        assert_eq!(redirect_chain(&con, &[]).unwrap(), "[]");
    }

    #[test]
    fn renderer_count_must_be_a_nonnegative_integer() {
        assert_eq!(renderer_count(&json!(3), "count").expect("count"), 3);
        assert!(renderer_count(&json!(-1), "count").is_err());
        assert!(renderer_count(&json!(1.5), "count").is_err());
    }
}
