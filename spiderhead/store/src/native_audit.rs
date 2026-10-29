//! Validate the existing audit snapshot against its native scan revision.

use std::collections::BTreeSet;

use rusqlite::types::ValueRef;
use rusqlite::{Connection, OptionalExtension};
use serde_json::{Map, Value};
use spiderhead_core::crawl_settings::manifest;
use spiderhead_core::host::SystemHost;

use crate::error::StoreError;
use crate::scan_artifact::{dump, loads, sha, MAX_JSON_BYTES};

/// The complete audit cannot fit the format reader's finite JSON budget.
#[derive(Debug, Clone, thiserror::Error)]
#[error("{message}")]
pub struct AuditSizeError {
    /// Human-readable reason.
    pub message: String,
}

/// Convert one SQLite cell into a JSON value for comparison with the document.
fn cell(value: ValueRef<'_>) -> Value {
    match value {
        ValueRef::Null => Value::Null,
        ValueRef::Integer(i) => Value::from(i),
        ValueRef::Real(f) => Value::from(f),
        ValueRef::Text(t) => Value::from(String::from_utf8_lossy(t).into_owned()),
        ValueRef::Blob(b) => Value::from(String::from_utf8_lossy(b).into_owned()),
    }
}

fn invalid(message: &str) -> StoreError {
    StoreError::Invalid(message.to_string())
}

/// Validate the saved audit of `con` against the native `scan` header.
///
/// Returns `Ok(None)` when no audit exists and `required` is false; the parsed document
/// otherwise. `scan` holds the header columns read from the `scan` row.
/// Pass `required = false` when a missing audit is acceptable.
pub fn validate_audit(
    con: &Connection,
    scan: &Map<String, Value>,
    required: bool,
) -> Result<Option<Value>, StoreError> {
    let row = con
        .query_row(
            "SELECT schema_version, evidence_revision, analyzer_version, analyzer_revision, \
             document_json, sha256 FROM audit WHERE singleton=1",
            [],
            |r| {
                Ok((
                    cell(r.get_ref(0)?),
                    cell(r.get_ref(1)?),
                    cell(r.get_ref(2)?),
                    cell(r.get_ref(3)?),
                    r.get::<_, String>(4)?,
                    r.get::<_, String>(5)?,
                ))
            },
        )
        .optional()?;
    let Some((schema_version, evidence_revision, analyzer_version, analyzer_revision, raw, digest)) =
        row
    else {
        if required {
            return Err(invalid(
                "native scan has no current audit; collection evidence is available separately",
            ));
        }
        return Ok(None);
    };
    if raw.len() as i64 > MAX_JSON_BYTES || sha(&raw) != digest {
        return Err(invalid("native saved audit size or hash is invalid"));
    }
    let document = loads(&raw, "audit")?;
    let doc_field = |key: &str| document.get(key).cloned().unwrap_or(Value::Null);
    let scan_field = |key: &str| scan.get(key).cloned().unwrap_or(Value::Null);
    if (
        schema_version,
        evidence_revision,
        analyzer_version,
        analyzer_revision,
    ) != (
        doc_field("schema_version"),
        scan_field("evidence_revision"),
        scan_field("writer_version"),
        scan_field("writer_revision"),
    ) {
        return Err(invalid(
            "native saved audit version, revision or analyzer identity disagrees",
        ));
    }
    if document["tool"]["version"] != scan_field("writer_version") {
        return Err(invalid(
            "native audit tool version differs from the producing build",
        ));
    }
    let config_json = scan_field("config_json");
    let mut effective: Value = serde_json::from_str(config_json.as_str().unwrap_or("{}"))?;
    let has_config_extension: bool = con
        .query_row(
            "SELECT 1 FROM context_items WHERE kind='native_config_extension' AND item_key='run'",
            [],
            |_| Ok(()),
        )
        .optional()?
        .is_some();
    let has_asset_thresholds: bool = con
        .query_row(
            "SELECT 1 FROM context_items WHERE kind='analysis_asset_thresholds' LIMIT 1",
            [],
            |_| Ok(()),
        )
        .optional()?
        .is_some();
    if has_config_extension {
        // The effective config read from the database replaces the header-derived one.
        effective = serde_json::to_value(crate::scan_schema::read_effective_config(con)?)?;
    } else if has_asset_thresholds {
        // Legacy audits were emitted from the frozen header plus the threshold adjunct;
        // do not inject newer default settings into that historical manifest.
        let thresholds = crate::scan_schema::read_effective_config(con)?;
        let analysis = thresholds
            .get("analysis")
            .and_then(|a| a.get("asset_thresholds"))
            .cloned()
            .unwrap_or(Value::Null);
        let Some(obj) = effective.as_object_mut() else {
            return Err(invalid("native audit effective configuration disagrees"));
        };
        if !obj.get("analysis").is_some_and(Value::is_object) {
            obj.insert("analysis".into(), Value::Object(Map::new()));
        }
        obj["analysis"]["asset_thresholds"] = analysis;
    }
    let config = manifest(&effective, &SystemHost)
        .map_err(|e| StoreError::Invalid(format!("crawl settings: {e}")))?;
    let config_value = serde_json::to_value(&config)?;
    if dump(&document["run"]["crawl_config"]) != dump(&config_value) {
        return Err(invalid("native audit effective configuration disagrees"));
    }
    let finding_exclusions = effective
        .get("analysis")
        .and_then(|a| a.get("finding_exclusions"))
        .cloned()
        .unwrap_or_else(|| Value::Array(Vec::new()));
    let saved_policy = document["run"]
        .get("finding_exclusion_policy")
        .cloned()
        .unwrap_or_else(|| Value::Array(Vec::new()));
    if dump(&saved_policy) != dump(&finding_exclusions) {
        return Err(invalid(
            "native audit finding-exclusion policy disagrees with saved scan config",
        ));
    }
    let saved_partial = document["run"]["crawl_partial"].as_bool().unwrap_or(false);
    let scan_partial = scan_field("crawl_partial").as_i64().unwrap_or(0) != 0
        || scan_field("crawl_partial").as_bool().unwrap_or(false);
    if saved_partial != scan_partial {
        return Err(invalid("native audit collection completeness disagrees"));
    }
    let pages = document["pages"].as_array().cloned().unwrap_or_default();
    let urls: BTreeSet<String> = pages
        .iter()
        .filter_map(|page| page["url"].as_str().map(str::to_owned))
        .collect();
    let page_count: i64 = con.query_row("SELECT COUNT(*) FROM pages", [], |r| r.get(0))?;
    if urls.len() != pages.len() || urls.len() as i64 != page_count {
        return Err(invalid("native audit page population disagrees"));
    }
    let mut statement = con.prepare("SELECT u.url FROM pages p JOIN urls u USING(url_id)")?;
    let collected = statement.query_map([], |r| r.get::<_, String>(0))?;
    for url in collected {
        if !urls.contains(&url?) {
            return Err(invalid("native audit is missing a collected page"));
        }
    }
    Ok(Some(document))
}
