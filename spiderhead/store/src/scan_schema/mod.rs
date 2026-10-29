//! Artifact identity, configuration header and schema validation of native scans.
//!
//! This file holds the configuration-header helpers, the extension and effective
//! fingerprint, `read_effective_config` and the legacy Set-Cookie latch check; the
//! effective-config validator is in `native_config`, the `NativeScanSchemaMixin`
//! entry points are in `lifecycle` (create/open), `inspect` (inspect/observe) and
//! `validate` (light and full acceptance). Functions owned by other modules are reached
//! through `deps`.

use crate::error::StoreError;
use crate::scan_artifact::{dump, query_maps, sha, ScanError};
use deps::deps;
use rusqlite::Connection;
use serde_json::{Map, Value};

/// Build a `ScanError` as a store error.
pub(crate) fn bad(message: impl Into<String>) -> StoreError {
    StoreError::ScanError(ScanError {
        message: message.into(),
    })
}

pub mod deps;
mod inspect;
mod lifecycle;
mod native_config;
#[cfg(test)]
pub(crate) mod oracle_tests;
mod rows;
#[cfg(test)]
mod tests;
mod validate;

pub use lifecycle::WriterLock;
pub use native_config::{crawl_fingerprint, native_config, resume_fingerprint};
pub use validate::EXPECTED_CAPABILITY_NAMES;

pub const RUNTIME_KEYS: &[&str] = &["python", "sqlite", "httpx", "lxml", "beautifulsoup4"];

pub const OPTIONAL_BROWSER_SETTINGS: &[&str] = &[
    "transport",
    "remote_protocol",
    "remote_endpoint_env",
    "remote_playwright_version",
    "engine",
    "viewport_width",
    "viewport_height",
    "page_concurrency",
];

/// Defaults of the config extension fields, in declaration order.
///
/// Before these options existed, native crawls kept analytics query parameters and
/// the other defaults below; a legacy header with no extension must keep that behavior.
pub fn config_extension_defaults() -> Vec<(&'static str, Value)> {
    vec![
        ("scope.normalize_tracking_parameters", Value::Bool(false)),
        ("http.auth", Value::Array(vec![])),
        ("http.auth_acknowledged", Value::Bool(false)),
        ("http.zstd", Value::Bool(false)),
        ("rendering.actions", Value::Array(vec![])),
        ("rendering.presets", Value::Array(vec![])),
        (
            "rendering.artifacts.screenshot_mode",
            Value::from("full_page"),
        ),
    ]
}

pub const CONFIG_EXTENSION_SCHEMA: &str = "native_config_extension.v1";

pub const INSPECT_MIN_TIMEOUT_SECONDS: f64 = 30.0;

pub const INSPECT_BYTES_PER_SECOND: i64 = 4194304;

/// Derive inspect()'s deadline from the artifact instead of a fixed constant.
///
/// See INSPECT_BYTES_PER_SECOND above for the measurement this rate comes from.
pub fn default_inspect_timeout(size_bytes: i64) -> f64 {
    INSPECT_MIN_TIMEOUT_SECONDS.max(size_bytes as f64 / INSPECT_BYTES_PER_SECOND as f64)
}

/// Remove `parts` from `root`, following only objects (a missing or non-object parent is a no-op).
fn pop_path(root: &mut Map<String, Value>, parts: &[&str]) {
    match parts {
        [] => {}
        [last] => {
            root.remove(*last);
        }
        [first, rest @ ..] => {
            if let Some(Value::Object(child)) = root.get_mut(*first) {
                pop_path(child, rest);
            }
        }
    }
}

/// Read the value at `parts`, failing when a key is missing.
fn get_path<'a>(root: &'a Map<String, Value>, parts: &[&str]) -> Result<&'a Value, StoreError> {
    let mut node: &Value = root.get(parts[0]).ok_or_else(|| missing(parts))?;
    for part in &parts[1..] {
        node = node.get(*part).ok_or_else(|| missing(parts))?;
    }
    Ok(node)
}

fn missing(parts: &[&str]) -> StoreError {
    bad(format!("native config is missing {}", parts.join(".")))
}

/// Set `parts` to `value`, creating intermediate objects along the way.
fn set_default_path(
    root: &mut Map<String, Value>,
    parts: &[&str],
    value: Value,
    message: &str,
) -> Result<(), StoreError> {
    match parts {
        [] => Ok(()),
        [last] => {
            root.entry((*last).to_owned()).or_insert(value);
            Ok(())
        }
        [first, rest @ ..] => {
            let child = root
                .entry((*first).to_owned())
                .or_insert_with(|| Value::Object(Map::new()));
            match child {
                Value::Object(map) => set_default_path(map, rest, value, message),
                _ => Err(bad(message.to_owned())),
            }
        }
    }
}

/// Set `parts` to `value`, refusing an existing leaf.
fn set_extension_path(
    root: &mut Map<String, Value>,
    parts: &[&str],
    value: Value,
) -> Result<(), StoreError> {
    match parts {
        [] => Ok(()),
        [last] => {
            if root.contains_key(*last) {
                return Err(bad(
                    "native config extension setting is duplicated in the header",
                ));
            }
            root.insert((*last).to_owned(), value);
            Ok(())
        }
        [first, rest @ ..] => {
            let child = root
                .entry((*first).to_owned())
                .or_insert_with(|| Value::Object(Map::new()));
            match child {
                Value::Object(map) => set_extension_path(map, rest, value),
                _ => Err(bad("native config extension disagrees with its header")),
            }
        }
    }
}

/// Keep the older reader's config shape; additive settings live in a run context.
pub fn frozen_header_config(config: &Map<String, Value>) -> Map<String, Value> {
    let mut header = config.clone();
    pop_path(&mut header, &["analysis", "asset_thresholds"]);
    // The opt-in redirect-hop probe (#1414): rolled-back builds neither know the
    // key nor the extension vocabulary, so it is simply absent from the header.
    pop_path(&mut header, &["analysis", "redirect_hop_hsts_probe"]);
    for (path, _) in config_extension_defaults() {
        let parts: Vec<&str> = path.split('.').collect();
        pop_path(&mut header, &parts);
    }
    header
}

/// Version and fingerprint settings omitted from the rollback-compatible header.
pub fn config_extension_context(
    config: &Map<String, Value>,
) -> Result<Map<String, Value>, StoreError> {
    let fields = config_extension_values(config)?;
    let payload = serde_json::json!({
        "schema_version": CONFIG_EXTENSION_SCHEMA,
        "settings": fields,
        "effective_fingerprint": effective_config_fingerprint(config),
    });
    let mut item = Map::new();
    item.insert("kind".into(), Value::from("native_config_extension"));
    item.insert("item_key".into(), Value::from("run"));
    item.insert("payload_version".into(), Value::from("scan_context.v1"));
    item.insert("payload_json".into(), Value::String(dump(&payload)));
    item.insert("completeness".into(), Value::from("complete"));
    item.insert("reason".into(), Value::from(""));
    Ok(item)
}

/// Current values of the extension settings, keyed by dotted path.
pub fn config_extension_values(
    config: &Map<String, Value>,
) -> Result<Map<String, Value>, StoreError> {
    let mut fields = Map::new();
    for (path, _) in config_extension_defaults() {
        let parts: Vec<&str> = path.split('.').collect();
        fields.insert(path.to_owned(), get_path(config, &parts)?.clone());
    }
    Ok(fields)
}

/// Bind all redacted effective values, including operational settings omitted by crawl identity.
///
/// The opt-in redirect-hop probe (#1414) is excluded: it rides neither the frozen
/// header nor the extension vocabulary, so an older reader that never sees the
/// key must hash the same shape a current writer does.
pub fn effective_config_fingerprint(config: &Map<String, Value>) -> String {
    let mut bound = config.clone();
    pop_path(&mut bound, &["analysis", "redirect_hop_hsts_probe"]);
    sha(&dump(&Value::Object(bound)))
}

/// Restore additive settings from typed contexts without changing the frozen header.
pub fn read_effective_config(con: &Connection) -> Result<Map<String, Value>, StoreError> {
    let config_text: String =
        con.query_row("SELECT config_json FROM scan WHERE singleton=1", [], |r| {
            r.get(0)
        })?;
    let mut config = match serde_json::from_str::<Value>(&config_text)
        .map_err(|e| bad(format!("native scan config header: {e}")))?
    {
        Value::Object(map) => map,
        _ => return Err(bad("native scan config header is not an object")),
    };
    let extensions = context_rows(
        con,
        "SELECT * FROM context_items WHERE kind='native_config_extension' LIMIT 2",
    )?;
    if extensions.len() > 1 {
        return Err(bad("native config extension context is duplicated"));
    }
    if let Some(extension) = extensions.first() {
        deps().validate_context(con, extension, None)?;
        let payload = payload_of(extension, "native config extension")?;
        let settings = payload["settings"]
            .as_object()
            .cloned()
            .ok_or_else(|| bad("native config extension has no settings"))?;
        for (path, value) in settings {
            let parts: Vec<&str> = path.split('.').collect();
            set_extension_path(&mut config, &parts, value)?;
        }
    } else {
        // Older artifacts have no extension item: restore the semantics that existed when
        // their frozen header was written (tracking parameters were not normalized).
        for (path, value) in config_extension_defaults() {
            let parts: Vec<&str> = path.split('.').collect();
            set_default_path(
                &mut config,
                &parts,
                value,
                "native config header is invalid",
            )?;
        }
    }
    // The opt-in redirect-hop probe (#1414) is absent from the header and the extension
    // vocabulary; its absence means the default (off).
    set_default_path(
        &mut config,
        &["analysis", "redirect_hop_hsts_probe"],
        Value::Bool(false),
        "native config header is invalid",
    )?;
    let thresholds_rows = context_rows(
        con,
        "SELECT * FROM context_items WHERE kind='analysis_asset_thresholds' LIMIT 2",
    )?;
    if thresholds_rows.len() > 1 {
        return Err(bad(
            "native analysis asset thresholds context is duplicated",
        ));
    }
    if let Some(item) = thresholds_rows.first() {
        deps().validate_context(con, item, None)?;
        let thresholds = payload_of(item, "asset thresholds")?["thresholds"].clone();
        let analysis_thresholds = config
            .get("analysis")
            .and_then(|a| a.get("asset_thresholds"))
            .cloned()
            .unwrap_or_else(|| thresholds.clone());
        if analysis_thresholds != thresholds {
            return Err(bad(
                "native analysis asset thresholds context disagrees with the header",
            ));
        }
        set_default_path(
            &mut config,
            &["analysis", "asset_thresholds"],
            Value::Null,
            "native config header is invalid",
        )?;
        if let Some(Value::Object(analysis)) = config.get_mut("analysis") {
            analysis.insert("asset_thresholds".into(), thresholds);
        }
    } else {
        let default_thresholds =
            spiderhead_core::settings_data::defaults()["analysis"]["asset_thresholds"].clone();
        set_default_path(
            &mut config,
            &["analysis", "asset_thresholds"],
            default_thresholds,
            "native config header is invalid",
        )?;
    }
    if let Some(extension) = extensions.first() {
        let payload = payload_of(extension, "native config extension")?;
        if effective_config_fingerprint(&config)
            != payload["effective_fingerprint"]
                .as_str()
                .unwrap_or_default()
        {
            return Err(bad(
                "native effective config extension fingerprint disagrees",
            ));
        }
    }
    Ok(config)
}

/// Full rows of `context_items` selected by a query (at most two are requested).
fn context_rows(con: &Connection, sql: &str) -> Result<Vec<Map<String, Value>>, StoreError> {
    query_maps(con, sql)
}

/// The parsed `payload_json` of a context item.
pub(crate) fn payload_of(item: &Map<String, Value>, label: &str) -> Result<Value, StoreError> {
    let text = item
        .get("payload_json")
        .and_then(Value::as_str)
        .ok_or_else(|| bad(format!("{label}: payload is missing")))?;
    serde_json::from_str(text).map_err(|e| bad(format!("{label}: {e}")))
}

/// Truthiness of a JSON value: null, false, zero, empty text and empty containers are false.
fn truthy(value: &Value) -> bool {
    match value {
        Value::Null => false,
        Value::Bool(b) => *b,
        Value::Number(n) => n.as_f64().is_some_and(|f| f != 0.0),
        Value::String(s) => !s.is_empty(),
        Value::Array(a) => !a.is_empty(),
        Value::Object(o) => !o.is_empty(),
    }
}

/// Recognize the old Set-Cookie resume latch without trusting an ambiguous one.
pub fn legacy_anonymous_session(
    config: &Map<String, Value>,
    reader: &Connection,
) -> Result<bool, StoreError> {
    let Some(http) = config.get("http").and_then(Value::as_object) else {
        return Ok(false);
    };
    let Some(browser) = config
        .get("rendering")
        .and_then(|r| r.get("browser"))
        .and_then(Value::as_object)
    else {
        return Ok(false);
    };
    if http.get("credential_headers").is_some_and(truthy)
        || browser.get("persistent_profile") != Some(&Value::Bool(false))
    {
        return Ok(false);
    }
    let mut st = reader.prepare(
        "SELECT response_headers_redacted_json,effective_headers_redacted_json FROM responses \
         WHERE response_headers_redacted_json LIKE '%set-cookie%' \
         OR effective_headers_redacted_json LIKE '%set-cookie%'",
    )?;
    let rows = st.query_map([], |r| {
        Ok((
            r.get::<_, Option<String>>(0)?,
            r.get::<_, Option<String>>(1)?,
        ))
    })?;
    for row in rows {
        let (first, second) = row?;
        for text in [first, second].into_iter().flatten() {
            let Ok(Value::Array(pairs)) = serde_json::from_str::<Value>(&text) else {
                continue;
            };
            let latched = pairs.iter().any(|pair| {
                let Some(items) = pair.as_array() else {
                    return false;
                };
                items.len() == 2
                    && items[0].as_str() == Some(crate::crawl_capture::REDACTED_NAMES_HEADER)
                    && items[1]
                        .as_str()
                        .is_some_and(|v| v.split(',').any(|token| token == "set-cookie"))
            });
            if latched {
                return Ok(true);
            }
        }
    }
    Ok(false)
}

/// Mixin of NativeScan: artifact identity, config header, validation, open/create/inspect.
///
/// The entry points are associated functions implemented in `lifecycle`, `inspect` and `validate`.
pub struct NativeScanSchemaMixin {}
