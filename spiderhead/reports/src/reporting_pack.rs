//! `reporting_pack`: build the committed synthetic reporting-pack worksheets.
//!
//! Regenerates the BI CSV partitions and their manifest from a supplied synthetic audit
//! plus committed `seohead.reporting-pack-source.v1` provider inputs, so the example
//! fixture always matches the versioned `seohead.bi-manifest.v1` projection.
//! Performs no network, provider or Google call.

use std::path::{Path, PathBuf};

use serde_json::{json, Value};
use thiserror::Error;

use crate::bi::project::{export_bi, ExportLimits};
use crate::evidence_import::{normalize_inline, EvidenceImportError};

/// Format marker of provider source files.
pub const SOURCE_FORMAT: &str = "spiderhead.reporting-pack-source.v1";
/// Size cap for one provider source file.
pub const MAX_SOURCE_BYTES: usize = 1_000_000;
/// Format marker of the worksheet manifest.
pub const WORKSHEET_FORMAT: &str = "spiderhead.reporting-pack-worksheets.v1";

/// Reporting-pack failure.
#[derive(Debug, Error)]
#[error("{0}")]
pub struct ReportingPackError(pub String);

fn err(msg: impl Into<String>) -> ReportingPackError {
    ReportingPackError(msg.into())
}

impl From<EvidenceImportError> for ReportingPackError {
    fn from(e: EvidenceImportError) -> Self {
        err(e.0)
    }
}

/// Sorted-key JSON with a one-space indent and a trailing newline.
fn dumps_indent1(value: &Value) -> String {
    let mut out = String::new();
    write_indent1(value, 0, &mut out);
    out
}

fn write_indent1(value: &Value, level: usize, out: &mut String) {
    match value {
        Value::Null => out.push_str("null"),
        Value::Bool(flag) => out.push_str(if *flag { "true" } else { "false" }),
        Value::Number(n) => out.push_str(&n.to_string()),
        Value::String(text) => {
            out.push_str(&serde_json::to_string(text).unwrap_or_else(|_| "\"\"".into()))
        }
        Value::Array(items) => {
            if items.is_empty() {
                out.push_str("[]");
                return;
            }
            out.push_str("[\n");
            for (index, item) in items.iter().enumerate() {
                if index > 0 {
                    out.push_str(",\n");
                }
                out.push_str(&" ".repeat(level + 1));
                write_indent1(item, level + 1, out);
            }
            out.push('\n');
            out.push_str(&" ".repeat(level));
            out.push(']');
        }
        Value::Object(map) => {
            if map.is_empty() {
                out.push_str("{}");
                return;
            }
            let mut entries: Vec<(&String, &Value)> = map.iter().collect();
            entries.sort_by(|a, b| a.0.cmp(b.0));
            out.push_str("{\n");
            for (index, (key, item)) in entries.iter().enumerate() {
                if index > 0 {
                    out.push_str(",\n");
                }
                out.push_str(&" ".repeat(level + 1));
                out.push_str(&serde_json::to_string(key).unwrap_or_else(|_| "\"\"".into()));
                out.push_str(": ");
                write_indent1(item, level + 1, out);
            }
            out.push('\n');
            out.push_str(&" ".repeat(level));
            out.push('}');
        }
    }
}

/// Load the normalized evidence document for one committed source file.
pub fn load_provider_input(path: &Path) -> Result<Value, ReportingPackError> {
    if path.is_symlink() || !path.is_file() {
        return Err(err("provider input must be an existing regular JSON file"));
    }
    let raw = std::fs::read(path).map_err(|_| err("provider input could not be read"))?;
    if raw.len() > MAX_SOURCE_BYTES {
        return Err(err("provider input exceeds the 1 MiB fixture bound"));
    }
    let document: Value =
        serde_json::from_slice(&raw).map_err(|_| err("provider input must be valid UTF-8 JSON"))?;
    if !document.is_object() || document.get("format") != Some(&json!(SOURCE_FORMAT)) {
        return Err(err(format!(
            "provider input requires format {SOURCE_FORMAT}"
        )));
    }
    let mapping = document.get("mapping").cloned().unwrap_or(Value::Null);
    if !mapping.is_object() {
        return Err(err("provider input requires an evidence-mapping.v1 object"));
    }
    let rows = document.get("rows").cloned().unwrap_or(Value::Null);
    if !rows.is_array() || rows.as_array().unwrap().is_empty() {
        return Err(err("provider input requires a non-empty row list"));
    }
    Ok(normalize_inline(&rows, &mapping)?)
}

/// Publish the BI CSV partitions and their manifest to `out_dir`.
pub fn build_worksheets(
    audit: &Path,
    provider_inputs: &[PathBuf],
    search_metric: Option<&str>,
    out_dir: &Path,
) -> Result<Value, ReportingPackError> {
    let destination = out_dir;
    if destination.is_symlink() || destination.exists() {
        return Err(err("worksheet output directory must be a new path"));
    }
    let parent = destination
        .parent()
        .map(Path::to_path_buf)
        .unwrap_or_else(|| PathBuf::from("."));
    if !parent.is_dir() || parent.is_symlink() {
        return Err(err("worksheet output parent must be a real directory"));
    }
    // Create a hidden staging directory next to the destination.
    let leaf = destination
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    let work = unique_temp_dir(&parent, &format!(".{leaf}-"))?;
    let outcome = (|| -> Result<Value, ReportingPackError> {
        let mut normalized: Vec<PathBuf> = Vec::with_capacity(provider_inputs.len());
        for (index, path) in provider_inputs.iter().enumerate() {
            let document = load_provider_input(path)?;
            let normalized_path = work.join(format!("provider-{index:02}.json"));
            std::fs::write(&normalized_path, format!("{}\n", dumps_indent1(&document)))
                .map_err(|e| err(format!("provider input could not be written: {e}")))?;
            normalized.push(normalized_path);
        }
        let audit_value = Value::String(audit.to_string_lossy().into_owned());
        let joins: Vec<String> = normalized
            .iter()
            .map(|p| p.to_string_lossy().into_owned())
            .collect();
        let limits = ExportLimits {
            search_metric: search_metric.map(|s| s.to_string()),
            ..ExportLimits::default()
        };
        let result = export_bi(None, Some(&audit_value), &joins, Some(destination), limits)
            .map_err(|e| err(e.0))?;
        let manifest_text = std::fs::read_to_string(destination.join("manifest.json"))
            .map_err(|e| err(format!("manifest could not be read: {e}")))?;
        let manifest: Value = serde_json::from_str(&manifest_text)
            .map_err(|e| err(format!("manifest is not valid JSON: {e}")))?;
        let mut worksheets = serde_json::Map::new();
        if let Some(datasets) = manifest.get("datasets").and_then(Value::as_object) {
            for (name, dataset) in datasets {
                worksheets.insert(
                    name.clone(),
                    json!({
                        "rows": dataset.get("row_count").cloned().unwrap_or(Value::Null),
                        "bytes": dataset.get("bytes").cloned().unwrap_or(Value::Null),
                    }),
                );
            }
        }
        Ok(json!({
            "format": WORKSHEET_FORMAT,
            "network": false,
            "writes": "local worksheets only",
            "output_directory": destination.to_string_lossy(),
            "worksheets": Value::Object(worksheets),
            "run_id": result.get("run_id").cloned().unwrap_or(Value::Null),
            "search_metric": search_metric.map(Value::from).unwrap_or(Value::Null),
        }))
    })();
    // Remove the staging directory, ignoring errors.
    let _ = std::fs::remove_dir_all(&work);
    outcome
}

/// Create a uniquely named `.<name>-XXXXX` directory inside `parent`.
fn unique_temp_dir(parent: &Path, prefix: &str) -> Result<PathBuf, ReportingPackError> {
    for _ in 0..1000 {
        let suffix: String = format!("{:08x}", rand_u32());
        let candidate = parent.join(format!("{prefix}{suffix}"));
        match std::fs::create_dir(&candidate) {
            Ok(()) => return Ok(candidate),
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(e) => return Err(err(format!("could not create worksheet scratch dir: {e}"))),
        }
    }
    Err(err("could not create worksheet scratch dir"))
}

fn rand_u32() -> u32 {
    use std::time::{SystemTime, UNIX_EPOCH};
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    let pid = std::process::id() as u128;
    // xorshift over time+pid; only used for scratch dir uniqueness.
    let mut x = nanos ^ (pid << 32) ^ 0x9E3779B97F4A7C15;
    x ^= x << 13;
    x ^= x >> 7;
    x ^= x << 17;
    x as u32
}
