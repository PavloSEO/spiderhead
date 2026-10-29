//! Normalize an explicitly mapped third-party crawl CSV bundle.
//!
//! The returned document remains foreign crawl evidence. It is not a native
//! `scan.v1` artifact, an Export Analyzer audit, or a claim that missing
//! fields were measured cleanly.

mod datasets;
mod strict;

use std::collections::{BTreeSet, HashSet};
use std::path::{Path, PathBuf};

use serde_json::{json, Map, Value};
use sha2::{Digest, Sha256};

use datasets::DATASET_FIELDS;

/// The schema identifier a manifest must declare.
pub const MANIFEST_VERSION: &str = "third_party_crawl_manifest.v1";
/// The schema identifier of the envelope this import returns.
pub const SCHEMA_VERSION: &str = "third_party_crawl.v1";

/// The manifest or one of its source files violates the import contract.
#[derive(Debug, thiserror::Error)]
#[error("{0}")]
pub struct ExternalCrawlImportError(pub String);

/// The size and row ceilings the import enforces. Tests pin narrower budgets
/// the way the reference tests monkeypatch the module constants.
#[derive(Clone, Copy)]
pub struct ImportLimits {
    /// Largest accepted manifest document.
    pub max_manifest_bytes: i64,
    /// Largest accepted total across all source CSVs of one import.
    pub max_total_csv_bytes: i64,
    /// Largest accepted number of data rows in one dataset file.
    pub max_rows_per_dataset: usize,
}

impl Default for ImportLimits {
    fn default() -> Self {
        Self {
            max_manifest_bytes: 64 * 1024,
            max_total_csv_bytes: 32 * 1024 * 1024,
            max_rows_per_dataset: 100_000,
        }
    }
}

const SOURCE_FIELDS: [&str; 7] = [
    "product",
    "version",
    "format",
    "format_version",
    "exported_at",
    "crawl_state",
    "crawl_state_reason",
];
const MANIFEST_FIELDS: [&str; 3] = ["schema_version", "source", "datasets"];

/// An OS error's message without Rust's ` (os error N)` tail, matching the
/// bare strerror wording reference carries in its messages.
fn io_message(error: &std::io::Error) -> String {
    let text = error.to_string();
    match text.find(" (os error ") {
        Some(i) => text[..i].to_string(),
        None => text,
    }
}

/// `checks.sitemap.normalize_url`: lowercase host, default port dropped,
/// fragment removed — the collision key beside each raw URL field.
pub(super) fn normalize_url_checked(url: &str) -> Result<String, String> {
    crate::sitemap::normalize_url(url)
}

/// Read the manifest file: a regular non-symlink JSON object within the byte
/// budget, decoded strictly. Returns the document and the original bytes.
fn read_manifest(
    path: &Path,
    limits: &ImportLimits,
) -> Result<(Value, Vec<u8>), ExternalCrawlImportError> {
    let name = path
        .file_name()
        .map(|n| n.to_string_lossy().to_string())
        .unwrap_or_default();
    let is_symlink = std::fs::symlink_metadata(path)
        .map(|meta| meta.file_type().is_symlink())
        .unwrap_or(false);
    if is_symlink || !path.is_file() {
        return Err(ExternalCrawlImportError(
            "manifest must be a regular, non-symlink JSON file".into(),
        ));
    }
    let size = std::fs::metadata(path)
        .map_err(|error| {
            ExternalCrawlImportError(format!(
                "cannot read manifest '{name}': {}",
                io_message(&error)
            ))
        })?
        .len();
    if size as i64 > limits.max_manifest_bytes {
        return Err(ExternalCrawlImportError(format!(
            "manifest exceeds the {}-byte limit",
            limits.max_manifest_bytes
        )));
    }
    let raw = std::fs::read(path).map_err(|error| {
        ExternalCrawlImportError(format!(
            "cannot read manifest '{name}': {}",
            io_message(&error)
        ))
    })?;
    if raw.len() as u64 != size {
        return Err(ExternalCrawlImportError(
            "manifest changed while it was read".into(),
        ));
    }
    let decoded = String::from_utf8(raw.clone()).map_err(|error| {
        ExternalCrawlImportError(format!("manifest is not valid UTF-8 JSON: {error}"))
    })?;
    let text = decoded
        .strip_prefix('\u{feff}')
        .map(str::to_string)
        .unwrap_or(decoded);
    let document = strict::parse(&text)?;
    if !document.is_object() {
        return Err(ExternalCrawlImportError(
            "manifest root must be a JSON object".into(),
        ));
    }
    Ok((document, raw))
}

/// The `source` block as the identity the envelope reports: required texts,
/// an optional export timestamp and a claimed completion state.
fn source_identity(value: Option<&Value>) -> Result<Value, ExternalCrawlImportError> {
    let source = datasets::mapping(value, "source", &SOURCE_FIELDS)?;
    let product = datasets::required_text(source.get("product"), "source.product", 200)?;
    let version = datasets::required_text(source.get("version"), "source.version", 200)?;
    let format = datasets::required_text(source.get("format"), "source.format", 200)?;
    let format_version =
        datasets::required_text(source.get("format_version"), "source.format_version", 200)?;
    let exported_at = match source.get("exported_at") {
        None | Some(Value::Null) => Value::Null,
        Some(Value::String(text)) => Value::String(text.clone()),
        Some(_) => {
            return Err(ExternalCrawlImportError(
                "source.exported_at must be a string or null".into(),
            ))
        }
    };
    let crawl_state = match source.get("crawl_state") {
        None => "unknown".to_string(),
        Some(Value::String(text))
            if matches!(text.as_str(), "complete" | "partial" | "unknown") =>
        {
            text.clone()
        }
        Some(_) => {
            return Err(ExternalCrawlImportError(
                "source.crawl_state must be complete, partial, or unknown".into(),
            ))
        }
    };
    let crawl_state_reason = match source.get("crawl_state_reason") {
        None | Some(Value::Null) => Value::Null,
        Some(Value::String(text)) => Value::String(text.clone()),
        Some(_) => {
            return Err(ExternalCrawlImportError(
                "source.crawl_state_reason must be a string or null".into(),
            ))
        }
    };
    Ok(json!({
        "product": product,
        "version": version,
        "format": format,
        "format_version": format_version,
        "exported_at": exported_at,
        "crawl_state": crawl_state,
        "crawl_state_reason": crawl_state_reason,
    }))
}

/// The placeholder document for a dataset the manifest never declared.
fn undeclared_dataset(fields: &[(&str, &str)]) -> Value {
    let mut field_coverage = Map::new();
    for (field, _) in fields {
        field_coverage.insert(
            (*field).to_string(),
            json!({
                "state": "not_exported",
                "source_column": Value::Null,
                "rows": 0,
                "present": 0,
                "missing": 0,
                "invalid": 0,
                "reason": "dataset was not declared in the manifest",
            }),
        );
    }
    json!({
        "state": "unavailable",
        "reason": "dataset was not declared in the manifest",
        "row_count": 0,
        "records": [],
        "field_coverage": Value::Object(field_coverage),
        "duplicate_summary": {"duplicate_groups": 0, "duplicate_rows": 0},
    })
}

/// Read a mapped CSV bundle into the versioned third_party_crawl.v1 envelope.
pub fn import_third_party_crawl(manifest_path: &str) -> Result<Value, ExternalCrawlImportError> {
    import_third_party_crawl_with_limits(manifest_path, &ImportLimits::default())
}

/// The same import under explicit size/row budgets, for tests.
pub fn import_third_party_crawl_with_limits(
    manifest_path: &str,
    limits: &ImportLimits,
) -> Result<Value, ExternalCrawlImportError> {
    if manifest_path.trim().is_empty() {
        return Err(ExternalCrawlImportError(
            "manifest_path must be a non-empty local path".into(),
        ));
    }
    let manifest = Path::new(manifest_path);
    let (document, raw_manifest) = read_manifest(manifest, limits)?;
    let keys: BTreeSet<&str> = document
        .as_object()
        .map(|map| map.keys().map(String::as_str).collect())
        .unwrap_or_default();
    let expected: BTreeSet<&str> = MANIFEST_FIELDS.into_iter().collect();
    if keys != expected {
        let missing: Vec<&str> = expected.difference(&keys).copied().collect();
        let unknown: Vec<&str> = keys.difference(&expected).copied().collect();
        let mut parts: Vec<String> = Vec::new();
        if !missing.is_empty() {
            parts.push(format!("missing {}", missing.join(", ")));
        }
        if !unknown.is_empty() {
            parts.push(format!("unknown {}", unknown.join(", ")));
        }
        return Err(ExternalCrawlImportError(format!(
            "manifest fields: {}",
            parts.join("; ")
        )));
    }
    if document.get("schema_version").and_then(Value::as_str) != Some(MANIFEST_VERSION) {
        return Err(ExternalCrawlImportError(format!(
            "unsupported manifest schema_version; expected '{MANIFEST_VERSION}'"
        )));
    }
    let source = source_identity(document.get("source"))?;
    let Some(datasets_value) = document.get("datasets").and_then(Value::as_object) else {
        return Err(ExternalCrawlImportError(
            "datasets must be an object keyed by pages, links, statuses, or redirects".into(),
        ));
    };
    let known: HashSet<&str> = DATASET_FIELDS.iter().map(|(name, _)| *name).collect();
    let mut unknown_datasets: Vec<&str> = datasets_value
        .keys()
        .map(String::as_str)
        .filter(|name| !known.contains(name))
        .collect();
    unknown_datasets.sort_unstable();
    if !unknown_datasets.is_empty() {
        return Err(ExternalCrawlImportError(format!(
            "unknown dataset(s): {}",
            unknown_datasets.join(", ")
        )));
    }

    let root = std::fs::canonicalize(manifest)
        .map_err(|error| {
            ExternalCrawlImportError(format!(
                "cannot read manifest '{}': {}",
                manifest
                    .file_name()
                    .map(|n| n.to_string_lossy().to_string())
                    .unwrap_or_default(),
                io_message(&error)
            ))
        })?
        .parent()
        .map(Path::to_path_buf)
        .unwrap_or_else(|| PathBuf::from("."));
    let mut used = HashSet::new();
    let mut datasets = Map::new();
    let mut total_bytes: i64 = 0;
    for (name, fields) in DATASET_FIELDS {
        match datasets_value.get(name) {
            None => {
                datasets.insert(name.to_string(), undeclared_dataset(fields));
            }
            Some(config) => {
                let (result, size) = datasets::read_dataset(
                    name,
                    config,
                    &root,
                    &mut used,
                    limits.max_total_csv_bytes - total_bytes,
                    limits,
                )?;
                total_bytes += size;
                datasets.insert(name.to_string(), result);
            }
        }
    }

    let mut rows_by_dataset = Map::new();
    let mut field_states: Vec<&str> = Vec::new();
    for (name, _) in DATASET_FIELDS {
        let dataset = &datasets[name];
        rows_by_dataset.insert(
            name.to_string(),
            dataset.get("row_count").cloned().unwrap_or(json!(0)),
        );
        if let Some(coverage) = dataset.get("field_coverage").and_then(Value::as_object) {
            field_states.extend(
                coverage
                    .values()
                    .filter_map(|field| field.get("state"))
                    .filter_map(Value::as_str),
            );
        }
    }
    let import_state = if field_states.iter().all(|&state| state == "not_exported") {
        "unavailable"
    } else if field_states.iter().all(|&state| state == "complete") {
        "complete"
    } else {
        "partial"
    };
    let crawl_state = source
        .get("crawl_state")
        .and_then(Value::as_str)
        .unwrap_or("unknown");
    let completeness_state = if crawl_state != "unknown" {
        format!("source_claimed_{crawl_state}")
    } else {
        "unknown".to_string()
    };
    let completeness_reason = source
        .get("crawl_state_reason")
        .and_then(Value::as_str)
        .filter(|reason| !reason.is_empty())
        .unwrap_or("crawl completion is a source claim and was not independently verified");
    let manifest_sha256 = {
        let mut hasher = Sha256::new();
        hasher.update(&raw_manifest);
        format!("{:x}", hasher.finalize())
    };
    let manifest_file = manifest
        .file_name()
        .map(|n| n.to_string_lossy().to_string())
        .unwrap_or_default();
    Ok(json!({
        "schema_version": SCHEMA_VERSION,
        "evidence_kind": "third_party_crawl_export",
        "compatibility": {
            "scan_v1": false,
            "sf_audit_json": false,
            "reason": "external observations are not native scan or Screaming Frog analyzer evidence",
        },
        "source": source,
        "crawl_completeness": {
            "state": completeness_state,
            "verified_by_importer": false,
            "reason": completeness_reason,
        },
        "import_coverage": {
            "state": import_state,
            "reason": "coverage describes mapped values and declared datasets in the supplied export only",
        },
        "provenance": {
            "manifest_schema_version": MANIFEST_VERSION,
            "manifest_file": manifest_file,
            "manifest_sha256": manifest_sha256,
            "input_bytes": total_bytes,
        },
        "datasets": Value::Object(datasets),
        "summary": {
            "rows_by_dataset": Value::Object(rows_by_dataset),
            "input_bytes": total_bytes,
        },
    }))
}
