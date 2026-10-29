//! Validated effective configuration of a native scan and the resume comparison.

use serde_json::{Map, Value};
use spiderhead_core::crawl_settings;
use spiderhead_core::host::SystemHost;
use spiderhead_core::settings_data;

use super::deps::deps;
use super::OPTIONAL_BROWSER_SETTINGS;
use crate::error::StoreError;
use crate::scan_artifact::{config as checked_config, ScanError};

fn bad(message: impl Into<String>) -> StoreError {
    StoreError::ScanError(ScanError {
        message: message.into(),
    })
}

/// Fingerprint of an effective configuration, over the crawl settings.
pub fn crawl_fingerprint(config: &Map<String, Value>) -> Result<String, StoreError> {
    crawl_settings::fingerprint(&Value::Object(config.clone()), &SystemHost)
        .map_err(|e| bad(e.to_string()))
}

fn defaults() -> &'static Value {
    settings_data::defaults()
}

/// `node[path...]` where every step must be an object holding the key.
fn at<'a>(root: &'a Value, path: &[&str]) -> Option<&'a Value> {
    path.iter().try_fold(root, |node, key| node.get(*key))
}

fn at_mut<'a>(root: &'a mut Value, path: &[&str]) -> Option<&'a mut Value> {
    path.iter().try_fold(root, |node, key| node.get_mut(*key))
}

fn has(root: &Value, path: &[&str]) -> bool {
    at(root, path).is_some()
}

/// Remove `key` from `parent`; a missing parent is a no-op.
fn pop(root: &mut Value, path: &[&str]) {
    if let Some((last, parent)) = path.split_last() {
        if let Some(Value::Object(map)) = at_mut(root, parent) {
            map.shift_remove(*last);
        }
    }
}

/// Insert `default` under `key` if absent, in the object at `root[parent...]`, which must be an object.
fn set_default(root: &mut Value, path: &[&str], default: Value) -> Result<(), StoreError> {
    let Some((last, parent)) = path.split_last() else {
        return Ok(());
    };
    let node = at_mut(root, parent).ok_or_else(|| bad("native config is malformed"))?;
    let map = node
        .as_object_mut()
        .ok_or_else(|| bad("native config is malformed"))?;
    map.entry((*last).to_owned()).or_insert(default);
    Ok(())
}

fn default_at(path: &[&str]) -> Result<Value, StoreError> {
    at(defaults(), path)
        .cloned()
        .ok_or_else(|| bad("native config default is missing"))
}

/// Every key of `expected` (recursively through objects) must exist in `actual`.
fn require_fields(actual: &Value, expected: &Value, path: &str) -> Result<(), StoreError> {
    let (Some(actual_map), Some(expected_map)) = (actual.as_object(), expected.as_object()) else {
        return Err(bad(format!(
            "native {path} is not a complete resolved configuration"
        )));
    };
    if expected_map.keys().any(|k| !actual_map.contains_key(k)) {
        return Err(bad(format!(
            "native {path} is not a complete resolved configuration"
        )));
    }
    for (name, default) in expected_map {
        if default.is_object() {
            require_fields(&actual_map[name], default, &format!("{path}.{name}"))?;
        }
    }
    Ok(())
}

/// The shape a recorded (older) configuration may legitimately have: options added after
/// native v1 artifacts existed are dropped from the expected projection when absent.
fn recorded_expected(config: &Value) -> Result<Value, StoreError> {
    let mut expected = defaults().clone();
    for name in ["storage", "resources"] {
        if !has(config, &[name]) {
            pop(&mut expected, &[name]);
        }
    }
    if has(config, &["resources"]) && !has(config, &["resources", "graph"]) {
        pop(&mut expected, &["resources", "graph"]);
    }
    if has(config, &["storage"]) && !has(config, &["storage", "format_version"]) {
        pop(&mut expected, &["storage", "format_version"]);
    }
    if has(config, &["storage"]) && !has(config, &["storage", "capacity_profile"]) {
        pop(&mut expected, &["storage", "capacity_profile"]);
    }
    if has(config, &["rendering"]) && !has(config, &["rendering", "rendered_links"]) {
        pop(&mut expected, &["rendering", "rendered_links"]);
    } else if has(config, &["rendering"])
        && !at(config, &["rendering", "rendered_links"])
            .and_then(|v| v.get("crawl"))
            .is_some()
    {
        pop(&mut expected, &["rendering", "rendered_links", "crawl"]);
    }
    if has(config, &["rendering", "browser"]) {
        for name in OPTIONAL_BROWSER_SETTINGS {
            if !has(config, &["rendering", "browser", name]) {
                pop(&mut expected, &["rendering", "browser", name]);
            }
        }
    }
    if has(config, &["limits"]) && !has(config, &["limits", "max_requests"]) {
        pop(&mut expected, &["limits", "max_requests"]);
    }
    if !has(config, &["fetch"]) {
        // The 5xx retry setting (#1446) postdates native v1 artifacts; the default (off)
        // is exactly what those runs did.
        pop(&mut expected, &["fetch"]);
    }
    if has(config, &["evidence"]) && !has(config, &["evidence", "retain_no_store_acknowledged"]) {
        pop(&mut expected, &["evidence", "retain_no_store_acknowledged"]);
    }
    for name in [
        "finding_exclusions",
        "redirect_hop_hsts_probe",
        "asset_thresholds",
    ] {
        if has(config, &["analysis"]) && !has(config, &["analysis", name]) {
            pop(&mut expected, &["analysis", name]);
        }
    }
    for key in PROXY_KEYS {
        if !has(config, &["http", key]) {
            pop(&mut expected, &["http", key]);
        }
    }
    Ok(expected)
}

const PROXY_KEYS: [&str; 4] = [
    "proxy",
    "proxy_allow_private",
    "proxy_identity",
    "proxy_authenticated",
];

/// A current-shape copy of a recorded configuration, for validation only.
fn recorded_validation_config(config: &Value) -> Result<Value, StoreError> {
    let mut v = config.clone();
    for name in ["storage", "resources"] {
        set_default(&mut v, &[name], default_at(&[name])?)?;
    }
    set_default(
        &mut v,
        &["resources", "graph"],
        default_at(&["resources", "graph"])?,
    )?;
    set_default(
        &mut v,
        &["storage", "format_version"],
        Value::from("scan.v1"),
    )?;
    set_default(
        &mut v,
        &["storage", "capacity_profile"],
        Value::from("stable"),
    )?;
    set_default(&mut v, &["rendering"], Value::Object(Map::new()))?;
    set_default(
        &mut v,
        &["rendering", "rendered_links"],
        default_at(&["rendering", "rendered_links"])?,
    )?;
    set_default(
        &mut v,
        &["rendering", "rendered_links", "crawl"],
        Value::Bool(false),
    )?;
    for name in OPTIONAL_BROWSER_SETTINGS {
        set_default(
            &mut v,
            &["rendering", "browser", name],
            default_at(&["rendering", "browser", name])?,
        )?;
    }
    set_default(&mut v, &["limits"], Value::Object(Map::new()))?;
    set_default(&mut v, &["limits", "max_requests"], Value::from(0))?;
    set_default(&mut v, &["fetch"], Value::Object(Map::new()))?;
    set_default(&mut v, &["fetch", "retry_server_errors"], Value::from(0))?;
    set_default(&mut v, &["evidence"], Value::Object(Map::new()))?;
    set_default(
        &mut v,
        &["evidence", "retain_no_store_acknowledged"],
        Value::Bool(false),
    )?;
    set_default(&mut v, &["analysis"], Value::Object(Map::new()))?;
    set_default(
        &mut v,
        &["analysis", "finding_exclusions"],
        Value::Array(vec![]),
    )?;
    set_default(
        &mut v,
        &["analysis", "redirect_hop_hsts_probe"],
        Value::Bool(false),
    )?;
    set_default(
        &mut v,
        &["analysis", "asset_thresholds"],
        default_at(&["analysis", "asset_thresholds"])?,
    )?;
    set_default(&mut v, &["http"], Value::Object(Map::new()))?;
    if let Some(Value::Object(http)) = at_mut(&mut v, &["http"]) {
        http.insert("proxy".into(), Value::from(""));
        http.insert("proxy_allow_private".into(), Value::Bool(false));
        http.insert("proxy_identity".into(), Value::from(""));
        http.insert("proxy_authenticated".into(), Value::Bool(false));
    }
    Ok(v)
}

fn invalid_config(error: impl std::fmt::Display) -> StoreError {
    bad(format!(
        "native effective configuration is invalid: {error}"
    ))
}

/// Validated effective configuration.
///
/// With `recorded` the value is an artifact's own (already redacted) configuration: it is
/// validated through an ephemeral current-shape projection but returned unchanged, so its
/// historical fingerprint holds.
pub fn native_config(value: &Value, recorded: bool) -> Result<Map<String, Value>, StoreError> {
    let source = if recorded {
        value.clone()
    } else {
        deps().redact_config(value)?
    };
    let config = checked_config(&source)?;
    let expected = if recorded {
        recorded_expected(&config)?
    } else {
        defaults().clone()
    };
    require_fields(&config, &expected, "config")?;
    let to_validate = if recorded {
        deps().validate_recorded_credentials(&recorded_validation_config(&config)?)?
    } else {
        value.clone()
    };
    crawl_settings::validate(&to_validate, &SystemHost).map_err(invalid_config)?;
    match config {
        Value::Object(map) => Ok(map),
        _ => Err(bad("native config is not an object")),
    }
}

/// `expected[parent].get(key) == default` (with the parent possibly absent).
fn equals_default(expected: &Value, path: &[&str], default: &Value) -> bool {
    at(expected, path) == Some(default)
}

/// Compare a current request using the recorded artifact's known option shape.
pub fn resume_fingerprint(
    expected_config: &Value,
    recorded_config: &Value,
) -> Result<String, StoreError> {
    let mut expected = Value::Object(native_config(expected_config, false)?);
    let recorded = Value::Object(native_config(recorded_config, true)?);
    let d = defaults();
    for name in ["storage", "resources"] {
        if !has(&recorded, &[name]) && at(&expected, &[name]) == at(d, &[name]) {
            pop(&mut expected, &[name]);
        }
    }
    if has(&recorded, &["resources"])
        && !has(&recorded, &["resources", "graph"])
        && at(&expected, &["resources", "graph"]) == at(d, &["resources", "graph"])
    {
        pop(&mut expected, &["resources", "graph"]);
    }
    if has(&recorded, &["storage"])
        && !has(&recorded, &["storage", "format_version"])
        && equals_default(
            &expected,
            &["storage", "format_version"],
            &Value::from("scan.v1"),
        )
    {
        pop(&mut expected, &["storage", "format_version"]);
    }
    if has(&recorded, &["storage"])
        && !has(&recorded, &["storage", "capacity_profile"])
        && equals_default(
            &expected,
            &["storage", "capacity_profile"],
            &Value::from("stable"),
        )
    {
        pop(&mut expected, &["storage", "capacity_profile"]);
    }
    if has(&recorded, &["rendering"])
        && !has(&recorded, &["rendering", "rendered_links"])
        && at(&expected, &["rendering", "rendered_links"])
            == at(d, &["rendering", "rendered_links"])
    {
        pop(&mut expected, &["rendering", "rendered_links"]);
    } else if has(&recorded, &["rendering"])
        && !has(&recorded, &["rendering", "rendered_links", "crawl"])
        && at(&expected, &["rendering", "rendered_links", "crawl"]) == Some(&Value::Bool(false))
    {
        pop(&mut expected, &["rendering", "rendered_links", "crawl"]);
    }
    if has(&recorded, &["rendering", "browser"]) {
        for name in OPTIONAL_BROWSER_SETTINGS {
            let path = ["rendering", "browser", name];
            if !has(&recorded, &path) && at(&expected, &path) == at(d, &path) {
                pop(&mut expected, &path);
            }
        }
    }
    if has(&recorded, &["limits"])
        && !has(&recorded, &["limits", "max_requests"])
        && equals_default(&expected, &["limits", "max_requests"], &Value::from(0))
    {
        pop(&mut expected, &["limits", "max_requests"]);
    }
    if has(&recorded, &["evidence"])
        && !has(&recorded, &["evidence", "retain_no_store_acknowledged"])
        && equals_default(
            &expected,
            &["evidence", "retain_no_store_acknowledged"],
            &Value::Bool(false),
        )
    {
        pop(&mut expected, &["evidence", "retain_no_store_acknowledged"]);
    }
    if has(&recorded, &["analysis"])
        && !has(&recorded, &["analysis", "finding_exclusions"])
        && equals_default(
            &expected,
            &["analysis", "finding_exclusions"],
            &Value::Array(vec![]),
        )
    {
        pop(&mut expected, &["analysis", "finding_exclusions"]);
    }
    if !has(&recorded, &["analysis", "asset_thresholds"])
        && at(&expected, &["analysis", "asset_thresholds"])
            == at(d, &["analysis", "asset_thresholds"])
    {
        pop(&mut expected, &["analysis", "asset_thresholds"]);
    }
    for key in PROXY_KEYS {
        let path = ["http", key];
        if !has(&recorded, &path) && at(&expected, &path) == at(d, &path) {
            pop(&mut expected, &path);
        }
    }
    match expected {
        Value::Object(map) => crawl_fingerprint(&map),
        _ => Err(bad("native config is not an object")),
    }
}
