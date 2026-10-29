//! Header-level (`validate_light`) and full (`validate_native`) acceptance of native scans.

use std::collections::BTreeSet;

use rusqlite::Connection;
use serde_json::{Map, Value};

use super::deps::deps;
use super::native_config::{crawl_fingerprint, native_config};
use super::rows::validate_rows;
use super::{bad, read_effective_config, NativeScanSchemaMixin, RUNTIME_KEYS};
use crate::error::StoreError;
use crate::retention::{policy_for_config, validate_policy};
use crate::scan_artifact::{
    expected, is_uuid, objects, query_maps, validate_scalar_storage, APPLICATION_ID,
    FORMAT_VERSION, USER_VERSION,
};

pub(super) const LIFECYCLES: [&str; 4] = ["running", "interrupted", "finished", "failed"];

/// Integer test for a JSON value; booleans and floats are excluded.
pub(super) fn is_int(value: &Value) -> bool {
    value.is_i64() || value.is_u64()
}

/// Number test for a JSON value: integers or floats, booleans included, and finite.
pub(super) fn finite_number(value: &Value) -> Option<f64> {
    match value {
        Value::Bool(b) => Some(f64::from(u8::from(*b))),
        Value::Number(n) => n.as_f64().filter(|f| f.is_finite()),
        _ => None,
    }
}

pub(super) fn text<'a>(map: &'a Map<String, Value>, key: &str) -> Option<&'a str> {
    map.get(key).and_then(Value::as_str)
}

pub(super) fn exists(con: &Connection, sql: &str) -> Result<bool, StoreError> {
    Ok(con.prepare(sql)?.exists([])?)
}

pub(super) fn count(con: &Connection, sql: &str) -> Result<i64, StoreError> {
    Ok(con.query_row(sql, [], |r| r.get(0))?)
}

/// Pragma value as an integer.
fn pragma_int(con: &Connection, name: &str) -> Result<i64, StoreError> {
    Ok(con.query_row(&format!("PRAGMA {name}"), [], |r| r.get(0))?)
}

/// The single `scan` header row, if present.
fn header_row(con: &Connection) -> Result<Option<Map<String, Value>>, StoreError> {
    Ok(query_maps(con, "SELECT * FROM scan WHERE singleton=1")?
        .into_iter()
        .next())
}

fn parse(text: &str) -> Result<Value, StoreError> {
    Ok(serde_json::from_str(text)?)
}

/// Failures that are value, type or key errors rather than `ScanError`.
fn is_value_error(error: &StoreError) -> bool {
    matches!(error, StoreError::Json(_) | StoreError::Invalid(_))
}

impl NativeScanSchemaMixin {
    /// Header-level acceptance for a native scan: identity, schema, one header row.
    ///
    /// Skips every O(file) or O(pages) pass (quick_check, scalar storage, foreign keys,
    /// corpus and per-page frontier checks, saved-audit hashing). A scan accepted here is
    /// reported as `validation: "light"`; byte-level integrity is not claimed.
    pub fn validate_light(con: &Connection) -> Result<(), StoreError> {
        if pragma_int(con, "application_id")? != APPLICATION_ID {
            return Err(bad("foreign application_id"));
        }
        let version = pragma_int(con, "user_version")?;
        if version != USER_VERSION && version != 2 {
            return Err(bad("unsupported scan user_version"));
        }
        if version == 2 {
            crate::retry::v2_tables(con)?;
        } else if objects(con)? != expected()?.objects {
            return Err(bad("scan.v1 schema differs"));
        }
        if count(con, "SELECT COUNT(*) FROM scan")? != 1 {
            return Err(bad("native scan requires exactly one header"));
        }
        let scan = header_row(con)?;
        let Some(scan) =
            scan.filter(|s| matches!(text(s, "source_kind"), Some("native" | "reanalysis")))
        else {
            return Err(bad(
                "not a native scan artifact with the declared format version",
            ));
        };
        let wanted = if version == 2 {
            "scan.v2"
        } else {
            FORMAT_VERSION
        };
        if text(&scan, "format_version") != Some(wanted) {
            return Err(bad("scan user_version disagrees with its format_version"));
        }
        if !text(&scan, "lifecycle").is_some_and(|l| LIFECYCLES.contains(&l)) {
            return Err(bad("invalid native scan lifecycle"));
        }
        let pinned = scan.get("pinned").and_then(Value::as_i64);
        if text(&scan, "evidence_version") != Some("crawl.v1") || !matches!(pinned, Some(0 | 1)) {
            return Err(bad(
                "native evidence version or pin metadata is unsupported",
            ));
        }
        let fingerprint = (|| -> Result<String, StoreError> {
            if !is_uuid(text(&scan, "scan_uuid").unwrap_or_default()) {
                return Err(StoreError::Invalid("scan_uuid".into()));
            }
            let header_config = parse(text(&scan, "config_json").unwrap_or_default())?;
            native_config(&Value::Object(read_effective_config(con)?), true)?;
            let fingerprint =
                crawl_fingerprint(header_config.as_object().ok_or_else(|| {
                    StoreError::Invalid("header config is not an object".into())
                })?)?;
            for name in ["capabilities_json", "retention_json", "limitations_json"] {
                parse(text(&scan, name).unwrap_or_default())?;
            }
            Ok(fingerprint)
        })()
        .map_err(|e| {
            if is_value_error(&e) {
                bad("native scan header metadata is invalid")
            } else {
                e
            }
        })?;
        if text(&scan, "config_fingerprint") != Some(fingerprint.as_str()) {
            return Err(bad(
                "native scan configuration fingerprint disagrees with effective config",
            ));
        }
        if count(con, "SELECT COUNT(*) FROM resume_state")? != 1 {
            return Err(bad("native scan requires one resume_state"));
        }
        Ok(())
    }

    /// Full acceptance of a native scan (identity, storage, header, corpus and per-row checks).
    pub fn validate_native(con: &Connection) -> Result<(), StoreError> {
        if pragma_int(con, "application_id")? != APPLICATION_ID {
            return Err(bad("foreign application_id"));
        }
        let version = pragma_int(con, "user_version")?;
        let v2 = version == 2;
        if v2 {
            let declared: Option<String> = con
                .query_row(
                    "SELECT format_version FROM scan WHERE singleton=1",
                    [],
                    |r| r.get(0),
                )
                .ok();
            if declared.as_deref() != Some("scan.v2") {
                return Err(bad("scan user_version disagrees with its format_version"));
            }
            deps().validate_v2(con)?;
        }
        if version != USER_VERSION && version != 2 {
            return Err(bad("unsupported scan user_version"));
        }
        if !v2 && objects(con)? != expected()?.objects {
            return Err(bad("scan.v1 schema differs"));
        }
        validate_scalar_storage(con)?;
        let quick: String = con.query_row("PRAGMA quick_check", [], |r| r.get(0))?;
        if quick != "ok" {
            return Err(bad("native scan failed quick_check"));
        }
        if exists(con, "PRAGMA foreign_key_check")? {
            return Err(bad("native scan has inconsistent foreign-key references"));
        }
        if count(con, "SELECT COUNT(*) FROM scan")? != 1 {
            return Err(bad("native scan requires exactly one header"));
        }
        let scan =
            header_row(con)?.ok_or_else(|| bad("native scan requires exactly one header"))?;
        let source_kind = text(&scan, "source_kind").unwrap_or_default().to_owned();
        let wanted = if v2 { "scan.v2" } else { FORMAT_VERSION };
        if !matches!(source_kind.as_str(), "native" | "reanalysis")
            || text(&scan, "format_version") != Some(wanted)
        {
            return Err(bad(
                "not a native scan artifact with the declared format version",
            ));
        }
        let pinned = scan.get("pinned").and_then(Value::as_i64);
        if text(&scan, "evidence_version") != Some("crawl.v1") || !matches!(pinned, Some(0 | 1)) {
            return Err(bad(
                "native evidence version, corpus completeness, parent or pin metadata is unsupported",
            ));
        }
        if source_kind == "native" && !scan["parent_scan_uuid"].is_null() {
            return Err(bad("native metadata: capture cannot name a parent scan"));
        }
        if source_kind == "reanalysis" && !is_uuid(text(&scan, "parent_scan_uuid").unwrap_or("")) {
            return Err(bad("derived reanalysis requires a parent scan UUID"));
        }
        if !text(&scan, "lifecycle").is_some_and(|l| LIFECYCLES.contains(&l)) {
            return Err(bad("invalid native scan lifecycle"));
        }
        crate::native_audit::validate_audit(con, &scan, false)?;
        if count(con, "SELECT COUNT(*) FROM resume_state")? != 1 {
            return Err(bad("native scan requires one resume_state"));
        }
        let (config, stored_fingerprint) =
            (|| -> Result<(Map<String, Value>, String), StoreError> {
                let header_config = parse(text(&scan, "config_json").unwrap_or_default())?;
                let config = native_config(&Value::Object(read_effective_config(con)?), true)?;
                let fingerprint =
                    crawl_fingerprint(header_config.as_object().ok_or_else(|| {
                        StoreError::Invalid("header config is not an object".into())
                    })?)?;
                Ok((config, fingerprint))
            })()
            .map_err(|e| match e {
                // ScanError is a value error, so any non-database failure lands here.
                StoreError::Sqlite(_) | StoreError::Io(_) => e,
                _ => bad("native scan has invalid effective configuration"),
            })?;
        if text(&scan, "config_fingerprint") != Some(stored_fingerprint.as_str()) {
            return Err(bad(
                "native scan configuration fingerprint disagrees with effective config",
            ));
        }
        let metadata = (|| -> Result<(Value, Value, Value), StoreError> {
            Ok((
                parse(text(&scan, "capabilities_json").unwrap_or_default())?,
                parse(text(&scan, "retention_json").unwrap_or_default())?,
                parse(text(&scan, "limitations_json").unwrap_or_default())?,
            ))
        })()
        .map_err(|_| bad("native scan has invalid metadata JSON"))?;
        let (capabilities, retention, limitations) = metadata;
        let capability_map = check_capabilities(con, &capabilities, &config, &source_kind)?;
        if capability_map["offline_reanalysis"]["state"] != "unavailable"
            && source_kind == "native"
            && capability_map["offline_reanalysis"]["state"].as_str()
                != Some(deps().reanalysis_state(con)?.as_str())
        {
            return Err(bad(
                "offline reanalysis capability disagrees with retained inputs",
            ));
        }
        validate_policy(&retention).map_err(plain)?;
        let config_index: indexmap::IndexMap<String, Value> =
            config.iter().map(|(k, v)| (k.clone(), v.clone())).collect();
        let retention_object = retention.as_object().cloned().unwrap_or_default();
        let retention_index: indexmap::IndexMap<String, Value> = retention_object
            .iter()
            .map(|(k, v)| (k.clone(), v.clone()))
            .collect();
        if retention_index != policy_for_config(&config_index).map_err(plain)? {
            return Err(bad(
                "native retention policy disagrees with its recorded configuration",
            ));
        }
        deps().validate_corpus(con, &scan, &retention_object)?;
        if config.contains_key("storage") {
            if !exists(
                con,
                "SELECT 1 FROM context_items WHERE kind='credential_context' AND item_key='run'",
            )? {
                return Err(bad("native scan is missing its credential resume context"));
            }
            if exists(con, "SELECT 1 FROM pages LIMIT 1")? {
                let coverage = deps().corpus_summary(con, &retention_object)?;
                let partial = scan
                    .get("corpus_partial")
                    .and_then(Value::as_i64)
                    .unwrap_or(0)
                    != 0;
                let states_differ = coverage["capabilities"].as_object().is_some_and(|map| {
                    map.iter()
                        .any(|(name, value)| capability_map[name]["state"] != value["state"])
                });
                if Value::Bool(partial) != coverage["corpus_partial"] || states_differ {
                    return Err(bad(
                        "native corpus capability metadata disagrees with recorded observations",
                    ));
                }
            }
        }
        if scan.get("corpus_partial").and_then(Value::as_i64) == Some(0)
            && !exists(con, "SELECT 1 FROM bodies LIMIT 1")?
        {
            return Err(bad(
                "native corpus metadata claims complete bytes without a retained body",
            ));
        }
        if !limitations
            .as_array()
            .is_some_and(|items| items.iter().all(Value::is_string))
        {
            return Err(bad("native scan limitations must be a string list"));
        }
        check_provenance(&scan)?;
        validate_rows(con, &scan, &config, &source_kind)
    }
}

/// `ScanError` text, instead of the `Invalid` wrapper that some owners return.
pub(super) fn plain(error: StoreError) -> StoreError {
    match error {
        StoreError::Invalid(message) => bad(message),
        other => other,
    }
}

/// Capability names a native scan must declare.
pub const EXPECTED_CAPABILITY_NAMES: [&str; 9] = [
    "pages",
    "links",
    "responses",
    "html_bodies",
    "rendered_bodies",
    "resource_refs",
    "resource_bodies",
    "resume",
    "offline_reanalysis",
];

/// Shape and per-lane state rules of the saved capability metadata; returns it as a map.
fn check_capabilities(
    con: &Connection,
    capabilities: &Value,
    config: &Map<String, Value>,
    source_kind: &str,
) -> Result<Value, StoreError> {
    let invalid = || bad("native scan has invalid capability metadata");
    let map = capabilities.as_object().ok_or_else(invalid)?;
    let names: BTreeSet<&str> = map.keys().map(String::as_str).collect();
    if names != EXPECTED_CAPABILITY_NAMES.iter().copied().collect() {
        return Err(invalid());
    }
    for value in map.values() {
        let entry = value.as_object().ok_or_else(invalid)?;
        let keys: BTreeSet<&str> = entry.keys().map(String::as_str).collect();
        if keys != BTreeSet::from(["state", "reason"]) {
            return Err(invalid());
        }
        let state = entry["state"].as_str();
        if !matches!(state, Some("complete" | "partial" | "unavailable"))
            || !entry["reason"].is_string()
        {
            return Err(invalid());
        }
    }
    for (name, value) in map {
        let state = value["state"].as_str().unwrap_or_default();
        let allowed: &[&str] = if name == "links"
            && exists(
                con,
                "SELECT 1 FROM context_items WHERE kind='list_mode' AND item_key='run'",
            )? {
            &["partial", "unavailable"]
        } else if name == "pages" || name == "links" {
            &["partial", "complete"]
        } else if name == "resume" && source_kind == "reanalysis" {
            &["unavailable"]
        } else if name == "resume" {
            &["partial", "complete"]
        } else if name == "offline_reanalysis" && source_kind == "reanalysis" {
            &["complete"]
        } else if matches!(
            name.as_str(),
            "responses" | "html_bodies" | "rendered_bodies" | "offline_reanalysis"
        ) || (config.contains_key("resources")
            && matches!(name.as_str(), "resource_refs" | "resource_bodies"))
        {
            &["partial", "complete", "unavailable"]
        } else {
            &["unavailable"]
        };
        if !allowed.contains(&state) {
            return Err(bad(
                "native storage core capability state claims an unavailable feature",
            ));
        }
    }
    Ok(capabilities.clone())
}

/// UUID, writer identity and runtime provenance of the header row.
fn check_provenance(scan: &Map<String, Value>) -> Result<(), StoreError> {
    let revision = text(scan, "writer_revision");
    let identity_ok = text(scan, "scan_uuid").is_some()
        && text(scan, "writer_version").is_some_and(|v| !v.is_empty())
        && revision.is_some_and(|r| {
            r.len() == 40
                && r.bytes()
                    .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
        })
        && text(scan, "runtime_versions_json").is_some();
    if !identity_ok {
        return Err(bad(
            "native scan UUID, writer version or writer revision is invalid",
        ));
    }
    let versions = is_uuid(text(scan, "scan_uuid").unwrap_or_default())
        .then(|| parse(text(scan, "runtime_versions_json").unwrap_or_default()).ok())
        .flatten()
        .ok_or_else(|| bad("native scan UUID or runtime provenance is invalid"))?;
    let valid = versions.as_object().is_some_and(|map| {
        map.keys().map(String::as_str).collect::<BTreeSet<_>>()
            == RUNTIME_KEYS.iter().copied().collect()
            && map
                .values()
                .all(|v| v.as_str().is_some_and(|s| !s.is_empty()))
    });
    if !valid {
        return Err(bad("native scan runtime provenance is invalid"));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    // review: entry["state"] panicked when a capability entry lacked the key.
    #[test]
    fn missing_state_key_is_an_error_not_a_panic() {
        let con = Connection::open_in_memory().unwrap();
        let mut caps = Map::new();
        for name in EXPECTED_CAPABILITY_NAMES {
            caps.insert(name.to_owned(), json!({"state": "complete", "reason": ""}));
        }
        caps.insert("pages".to_owned(), json!({"reason": "no state here"}));
        let err = check_capabilities(&con, &Value::Object(caps), &Map::new(), "native");
        assert!(err.is_err());
    }
}
