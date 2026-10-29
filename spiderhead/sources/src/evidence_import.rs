//! Offline import and normalization of analytics and search evidence into
//! `spiderhead.normalized-evidence.v1`.

use std::collections::BTreeSet;
use std::collections::HashMap;
use std::path::Path;

use calamine::Reader;
use serde_json::{json, Map, Value};
use sha2::Digest;

use crate::cloudflare::CfError;
use crate::external_join::normalize_join_key;
use crate::pystr;

mod load_file;
pub use load_file::*;
mod manifest;
pub use manifest::*;
mod mapping;
pub use mapping::*;
mod rows;
pub use rows::*;

/// Format identifier of a mapping manifest document.
pub const MAPPING_FORMAT: &str = "spiderhead.evidence-mapping.v1";
/// Format identifier of a normalized evidence document.
pub const NORMALIZED_FORMAT: &str = "spiderhead.normalized-evidence.v1";
/// Largest accepted input file, in bytes.
pub const MAX_FILE_BYTES: u64 = 16 * 1024 * 1024;
/// Largest accepted row count per import.
pub const MAX_ROWS: usize = 100_000;
/// Largest accepted column count per import.
pub const MAX_COLUMNS: usize = 256;

const COLLECTION_STATES: [&str; 5] = ["complete", "partial", "failed", "skipped", "not_configured"];
const FILE_KINDS: [&str; 4] = ["csv", "xlsx", "json", "provider_evidence"];
const URL_KINDS: [&str; 2] = ["absolute", "relative_path"];
const DUPLICATE_POLICIES: [&str; 2] = ["mark", "reject"];
const ROW_SHAPES: [&str; 3] = ["flat", "gsc", "ga4"];
const EVIDENCE_FORMAT: &str = "spiderhead.provider-evidence.v1";

const SOURCE_KEYS: [&str; 10] = [
    "kind",
    "provider",
    "operation",
    "reporting_identity",
    "privacy",
    "search_engine",
    "search_type",
    "attribution",
    "timezone",
    "site_origin",
];
const COLLECTION_KEYS: [&str; 6] = [
    "collected_at",
    "state",
    "sampled",
    "thresholded",
    "truncated",
    "reason",
];
const URL_KEYS: [&str; 3] = ["field", "dimension", "kind"];
const METRIC_KEYS: [&str; 3] = ["name", "type", "unit"];
const MANIFEST_KEYS: [&str; 9] = [
    "format",
    "source",
    "period",
    "url",
    "metrics",
    "dimensions",
    "collection",
    "duplicate_policy",
    "row_shape",
];
const PRIVATE_FIELDS: [&str; 2] = ["reporting_identity", "site_origin"];

fn err(message: impl Into<String>) -> CfError {
    CfError::Python("EvidenceImportError", message.into())
}

fn sha256(data: &[u8]) -> String {
    format!("{:x}", sha2::Sha256::digest(data))
}

fn get<'a>(value: &'a Value, name: &str) -> Option<&'a Value> {
    value.get(name).filter(|v| !v.is_null())
}

fn truthy(value: Option<&Value>) -> bool {
    pystr::truthy(value)
}

fn sorted_repr(values: BTreeSet<String>) -> String {
    pystr::repr(&Value::Array(
        values.into_iter().map(Value::String).collect(),
    ))
}

fn unknown_keys(map: &Map<String, Value>, allowed: &[&str]) -> BTreeSet<String> {
    map.keys()
        .filter(|k| !allowed.contains(&k.as_str()))
        .cloned()
        .collect()
}

fn is_iso_date(value: &Value) -> bool {
    value
        .as_str()
        .map(|text| crate::pytime::date_fromisoformat(text).is_some())
        .unwrap_or(false)
}

fn tz_known(name: &str) -> bool {
    if name.is_empty() || name.starts_with('/') || name.contains("..") {
        return false;
    }
    Path::new("/usr/share/zoneinfo").join(name).is_file()
        || matches!(name, "UTC" | "GMT" | "localtime")
}

fn str_field<'a>(value: &'a Value, name: &str) -> Option<&'a str> {
    value.get(name).and_then(Value::as_str)
}

fn json_safe_cell(value: calamine::Data) -> Value {
    use calamine::Data;
    match value {
        Data::Empty => Value::Null,
        Data::String(s) => json!(s),
        Data::Float(f) => {
            if f.fract() == 0.0 && f.abs() < 9e15 {
                json!(f as i64)
            } else {
                json!(f)
            }
        }
        Data::Int(i) => json!(i),
        Data::Bool(b) => json!(b),
        Data::Error(_) => Value::Null,
        Data::DateTime(dt) => json!(dt.as_f64()),
        Data::DateTimeIso(s) => json!(s),
        Data::DurationIso(s) => json!(s),
    }
}

/// Loads a CSV, XLSX or JSON file and normalizes it with the mapping manifest.
/// `site_origin`, when given, overrides the manifest source value.
pub fn normalize_file(
    path: &str,
    manifest: Option<&Value>,
    sheet: Option<&str>,
    site_origin: Option<&str>,
) -> Result<Value, CfError> {
    let loaded = load_import_file(path, sheet)?;
    let mut manifest = manifest
        .map(validate_manifest)
        .transpose()?
        .unwrap_or(json!({}));
    if let Some(origin) = site_origin {
        manifest["format"] = json!(MAPPING_FORMAT);
        let mut source = manifest
            .get("source")
            .and_then(Value::as_object)
            .cloned()
            .unwrap_or_default();
        source.insert("site_origin".to_owned(), json!(origin));
        manifest["source"] = Value::Object(source);
        manifest = validate_manifest(&manifest)?;
    }
    let (resolved, provenance) = resolve_mapping(&manifest, &loaded)?;
    let mut document = normalize_rows(
        loaded
            .get("rows")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default()
            .as_slice(),
        &resolved,
        &provenance,
        &loaded["file"],
    )?;
    document["provenance"]["artifact_reference"] = loaded
        .get("envelope")
        .and_then(|e| e.get("artifact_reference"))
        .cloned()
        .unwrap_or(Value::Null);
    Ok(document)
}

/// Normalizes in-memory row objects with an optional mapping manifest.
pub fn normalize_inline(rows: &[Value], manifest: Option<&Value>) -> Result<Value, CfError> {
    if rows.len() > MAX_ROWS || !rows.iter().all(Value::is_object) {
        return Err(err("rows must be a bounded list of row objects"));
    }
    let manifest = manifest
        .map(validate_manifest)
        .transpose()?
        .unwrap_or(json!({}));
    let kind = manifest
        .get("source")
        .and_then(|s| str_field(s, "kind"))
        .unwrap_or("json");
    let mut names = BTreeSet::new();
    for row in rows {
        if let Value::Object(map) = row {
            names.extend(map.keys().cloned());
        }
    }
    let loaded = json!({
        "kind": kind,
        "rows": rows,
        "columns": Value::Array(names.into_iter().map(Value::String).collect()),
    });
    let (resolved, provenance) = resolve_mapping(&manifest, &loaded)?;
    normalize_rows(
        rows,
        &resolved,
        &provenance,
        &json!({"name": "inline", "sha256": Value::Null, "bytes": Value::Null, "kind": kind}),
    )
}

/// Provenance for public output, with private fields redacted.
pub fn public_provenance(provenance: &Value) -> Value {
    let mut fields = Map::new();
    if let Some(entries) = provenance.get("fields").and_then(Value::as_object) {
        for (name, entry) in entries {
            if PRIVATE_FIELDS.contains(&name.as_str()) && get(entry, "value").is_some() {
                let mut redacted = entry.as_object().cloned().unwrap_or_default();
                redacted.insert("value".to_owned(), Value::Null);
                redacted.insert("redacted".to_owned(), json!(true));
                fields.insert(name.clone(), Value::Object(redacted));
            } else {
                fields.insert(name.clone(), entry.clone());
            }
        }
    }
    let mut file_meta = provenance
        .get("file")
        .and_then(Value::as_object)
        .cloned()
        .unwrap_or_default();
    file_meta.remove("name");
    json!({
        "fields": Value::Object(fields),
        "file": Value::Object(file_meta),
        "artifact_reference": provenance.get("artifact_reference").cloned().unwrap_or(Value::Null),
    })
}
