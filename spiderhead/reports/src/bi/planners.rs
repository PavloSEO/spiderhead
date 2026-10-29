//! Offline destination plans (no API call) and the bounded, checkpointed
//! delivery operations over a verified package: apply, filter, xlsx

use std::collections::BTreeMap;
use std::fs;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};

use serde_json::{json, Map, Value};
use sha2::Digest;
use spiderhead_core::formats::matches;
use spiderhead_core::host::Host;

use super::google_client::{
    resolved_target_mapping, BeginArgs, BeginOutcome, CommitOutcome, DestinationClient,
    PendingOutcome, GOOGLE_CHUNK_ROWS, GOOGLE_CHUNK_SOURCE_BYTES,
};
use super::schema::{
    dataset_spec, safe_dimension_fields, BIExportError, Field, BI_SCHEMA_VERSION, MANIFEST_FORMAT,
    MAX_BYTES_PER_PARTITION, MAX_CELL_BYTES, MAX_MANIFEST_BYTES, MAX_OUTPUT_BYTES,
    MAX_PROJECTION_INDEX_BYTES, MAX_ROWS_PER_PARTITION, MIN_FREE_DISK_BYTES,
};
use super::state::{
    bi_err, load_state, new_state, result_from_state, save_state, state_path, DestinationError,
};
use super::writer::{OutputBudget, PartitionWriter};
use crate::bi_destinations::sheets_max_cells;
use crate::bi_index::{
    copy_group_members_companion, projection_index, verify_group_member_references,
    PrimarySegmentIndex,
};

/// Maximum rows per Excel worksheet.
pub const EXCEL_MAX_ROWS: i64 = 1_048_576;

/// Maximum characters per Excel cell.
pub const EXCEL_MAX_CELL_CHARS: usize = 32_767;

/// Maximum worksheets per workbook.
pub const MAX_XLSX_SHEETS: usize = 10_000;

fn derr(message: impl Into<String>) -> DestinationError {
    bi_err(message.into())
}

fn from_export(exc: BIExportError) -> DestinationError {
    derr(exc.0)
}

fn is_int(value: &Value) -> bool {
    matches!(value, Value::Number(n) if n.is_i64() || n.is_u64())
}

fn as_i64(value: &Value) -> Option<i64> {
    value
        .as_i64()
        .or_else(|| value.as_u64().and_then(|v| i64::try_from(v).ok()))
}

/// Load a verified manifest from a package directory: the complete manifest, or the
/// `bi-filter.v1` selected projection.
pub fn manifest(package: &Path) -> Result<(PathBuf, Value), DestinationError> {
    let root = package.to_path_buf();
    if root.is_symlink() || !root.is_dir() {
        return Err(derr(
            "package must be an existing non-symlink BI package directory",
        ));
    }
    let path = root.join("manifest.json");
    if path.is_symlink() || !path.is_file() {
        return Err(derr("package has no regular manifest.json"));
    }
    if path.metadata().map(|m| m.len()).unwrap_or(u64::MAX) > MAX_MANIFEST_BYTES {
        return Err(derr("BI manifest exceeds its byte bound"));
    }
    let value: Value = match fs::read_to_string(&path)
        .map_err(|_| ())
        .and_then(|text| serde_json::from_str(&text).map_err(|_| ()))
    {
        Ok(value) => value,
        Err(()) => {
            return Err(derr("package manifest is unreadable: invalid JSON"));
        }
    };
    let schema_version = value.get("schema_version").and_then(Value::as_str);
    if matches(value.get("format").and_then(Value::as_str), MANIFEST_FORMAT)
        && matches(schema_version, BI_SCHEMA_VERSION)
    {
        if value.get("datasets").and_then(Value::as_object).is_none() {
            return Err(derr("package manifest has no dataset declarations"));
        }
        return Ok((root, value));
    }
    if value.get("format").and_then(Value::as_str) == Some("spiderhead.bi-filter.v1")
        && value.get("source_schema_version").and_then(Value::as_str) == Some(BI_SCHEMA_VERSION)
    {
        return manifest_selected(root, &value);
    }
    Err(derr(
        "package has an unsupported BI manifest/schema version",
    ))
}

fn manifest_selected(root: PathBuf, value: &Value) -> Result<(PathBuf, Value), DestinationError> {
    let name = value.get("dataset").and_then(Value::as_str);
    let columns = value.get("columns");
    let partitions = value.get("partitions");
    let row_count = value.get("row_count");
    // A JSON null counts as absent, the same as a missing `source` key.
    let source = value.get("source").filter(|s| !s.is_null());
    let valid = match name {
        Some(name) if dataset_spec(name).is_some() => {
            columns
                .and_then(Value::as_array)
                .map(|c| !c.is_empty())
                .unwrap_or(false)
                && partitions.and_then(Value::as_array).is_some()
                && row_count.map(is_int).unwrap_or(false)
                && as_i64(row_count.unwrap_or(&Value::Null))
                    .map(|r| r >= 0)
                    .unwrap_or(false)
                && (source.is_none() || source.map(Value::is_object).unwrap_or(false))
        }
        _ => false,
    };
    if !valid {
        return Err(derr("selected BI projection manifest is invalid"));
    }
    let name = name.unwrap_or_default();
    let expected = fields_for_manifest(name, value.get("metrics_dimension_columns"))?;
    let columns: Vec<Value> = columns
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    let column_names: Vec<&str> = columns
        .iter()
        .map(|c| c.as_str().unwrap_or_default())
        .collect();
    let unique: std::collections::BTreeSet<&str> = column_names.iter().copied().collect();
    if column_names.iter().any(|c| c.is_empty())
        || unique.len() != column_names.len()
        || column_names.iter().any(|c| !expected.contains_key(*c))
    {
        return Err(derr(
            "selected BI projection has unsupported or duplicate columns",
        ));
    }
    let fields: Vec<Value> = column_names
        .iter()
        .map(|column| {
            let field = &expected[*column];
            json!({"name": column, "type": field.type_, "nullable": field.nullable})
        })
        .collect();
    let partitions_vec = partitions
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    let bytes: i64 = partitions_vec
        .iter()
        .filter_map(|p| p.get("bytes").and_then(Value::as_i64))
        .sum();
    let mut dataset = Map::new();
    if let Some(coverage) = value.get("source_coverage").and_then(Value::as_object) {
        dataset.extend(coverage.clone());
    }
    dataset.insert("fields".into(), Value::Array(fields));
    dataset.insert(
        "partitions".into(),
        partitions.cloned().unwrap_or(Value::Null),
    );
    dataset.insert(
        "row_count".into(),
        row_count.cloned().unwrap_or(Value::Null),
    );
    dataset.insert("bytes".into(), json!(bytes));
    let mut datasets = Map::new();
    datasets.insert(name.to_owned(), Value::Object(dataset));
    // A JSON null counts as absent.
    if let Some(companion) = value.get("coverage_companion").filter(|c| !c.is_null()) {
        let valid_companion = companion
            .as_object()
            .map(|c| c.get("dataset").and_then(Value::as_str) == Some("coverage"))
            .unwrap_or(false);
        if !valid_companion {
            return Err(derr("selected BI coverage companion is invalid"));
        }
        let companion_fields = companion.get("fields");
        let companion_partitions = companion.get("partitions");
        let companion_rows = companion.get("row_count");
        let expected_coverage = fields_for_manifest("coverage", None)?;
        let field_names: Vec<&str> = companion_fields
            .and_then(Value::as_array)
            .map(|fields| {
                fields
                    .iter()
                    .filter_map(|f| f.get("name").and_then(Value::as_str))
                    .collect()
            })
            .unwrap_or_default();
        let expected_names: Vec<&str> = expected_coverage.keys().map(String::as_str).collect();
        if companion_fields.and_then(Value::as_array).is_none()
            || field_names != expected_names
            || companion_partitions.and_then(Value::as_array).is_none()
            || !companion_rows.map(is_int).unwrap_or(false)
            || as_i64(companion_rows.unwrap_or(&Value::Null))
                .map(|r| r < 0)
                .unwrap_or(false)
        {
            return Err(derr("selected BI coverage companion is invalid"));
        }
        let mut coverage = Map::new();
        for key in ["state", "reason", "source_population", "coverage"] {
            coverage.insert(
                key.to_owned(),
                companion.get(key).cloned().unwrap_or(Value::Null),
            );
        }
        coverage.insert(
            "fields".into(),
            companion_fields.cloned().unwrap_or(Value::Null),
        );
        let c_parts = companion_partitions
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();
        let c_bytes: i64 = c_parts
            .iter()
            .filter_map(|p| p.get("bytes").and_then(Value::as_i64))
            .sum();
        coverage.insert(
            "partitions".into(),
            companion_partitions.cloned().unwrap_or(Value::Null),
        );
        coverage.insert(
            "row_count".into(),
            companion_rows.cloned().unwrap_or(Value::Null),
        );
        coverage.insert("bytes".into(), json!(c_bytes));
        datasets.insert("coverage".to_owned(), Value::Object(coverage));
    }
    let source_run = source
        .and_then(|s| s.get("run"))
        .cloned()
        .unwrap_or(Value::Null);
    Ok((
        root,
        json!({
            "format": MANIFEST_FORMAT,
            "schema_version": BI_SCHEMA_VERSION,
            "source_package_format": "spiderhead.bi-filter.v1",
            "selected_projection": true,
            "source": source.cloned().unwrap_or(Value::Null),
            "run": source_run,
            "group_members": value.get("group_members").cloned().unwrap_or(Value::Null),
            "source_coverage": value.get("source_coverage").cloned().unwrap_or(Value::Null),
            "conservation": value.get("conservation").cloned().unwrap_or(Value::Null),
            "metrics_dimension_columns": value.get("metrics_dimension_columns").cloned().unwrap_or(Value::Null),
            "datasets": Value::Object(datasets),
        }),
    ))
}

/// Declared fields in schema order, with dynamic metric dimension columns appended for the
/// metrics dataset.
pub fn fields_for_manifest(
    name: &str,
    dimensions: Option<&Value>,
) -> Result<indexmap::IndexMap<String, Field>, DestinationError> {
    let Some((fields, _, _)) = dataset_spec(name) else {
        return Err(derr(format!("dataset {name:?} is not declared")));
    };
    let mut expected: indexmap::IndexMap<String, Field> = indexmap::IndexMap::new();
    for field in fields {
        expected.insert(field.name.to_string(), field.clone());
    }
    if name == "metrics" {
        if let Some(dimensions) = dimensions.filter(|d| !d.is_null()) {
            let map = dimensions.as_object();
            let safe = map
                .map(|m| {
                    m.len() <= 512
                        && m.keys().all(|k| !k.is_empty() && k.len() <= 1024)
                        && dimensions
                            == &Value::Object(safe_dimension_fields(m.keys().map(String::as_str)))
                })
                .unwrap_or(false);
            if !safe {
                return Err(derr("metric dimension mapping differs from the BI schema"));
            }
            for field_name in dimensions.as_object().unwrap_or(&Map::new()).values() {
                let name = field_name.as_str().unwrap_or_default().to_owned();
                expected.insert(
                    name.clone(),
                    Field {
                        name: std::borrow::Cow::Owned(name),
                        type_: "string",
                        nullable: true,
                        description: std::borrow::Cow::Borrowed(""),
                    },
                );
            }
        }
    }
    Ok(expected)
}

/// Field names whose declarations exactly match the BI schema
/// schema (or, for a selected projection, its declared subset).
pub fn validated_fields(
    name: &str,
    fields: &Value,
    selected_projection: bool,
    dimensions: Option<&Value>,
) -> Result<Vec<String>, DestinationError> {
    if dataset_spec(name).is_none()
        || !fields.is_array()
        || fields.as_array().map(|f| f.is_empty()).unwrap_or(true)
    {
        return Err(derr(format!("dataset {name:?} has no declared fields")));
    }
    let expected = fields_for_manifest(name, dimensions)?;
    let mut names: Vec<String> = Vec::new();
    for value in fields.as_array().unwrap_or(&Vec::new()) {
        let field_name = value.get("name").and_then(Value::as_str);
        let Some(field_name) = field_name else {
            return Err(derr(format!(
                "dataset {name:?} field declaration is invalid"
            )));
        };
        let Some(field) = expected.get(field_name) else {
            return Err(derr(format!(
                "dataset {name:?} fields do not match the BI schema"
            )));
        };
        if value.get("type").and_then(Value::as_str) != Some(field.type_)
            || value.get("nullable").and_then(Value::as_bool) != Some(field.nullable)
        {
            return Err(derr(format!(
                "dataset {name:?} fields do not match the BI schema"
            )));
        }
        names.push(field.name.to_string());
    }
    if names
        .iter()
        .collect::<std::collections::BTreeSet<_>>()
        .len()
        != names.len()
    {
        return Err(derr(format!("dataset {name:?} field names are duplicated")));
    }
    let complete: Vec<String> = expected.keys().cloned().collect();
    if !selected_projection && names != complete {
        return Err(derr(format!(
            "dataset {name:?} fields do not match the complete BI schema"
        )));
    }
    Ok(names)
}

/// Checksum of one file.
pub fn checksum_file(path: &Path) -> Result<String, DestinationError> {
    let mut digest = sha2::Sha256::new();
    let mut stream =
        fs::File::open(path).map_err(|e| derr(format!("package file is unreadable: {e}")))?;
    let mut buffer = vec![0u8; 1024 * 1024];
    loop {
        let read = stream
            .read(&mut buffer)
            .map_err(|e| derr(format!("package file is unreadable: {e}")))?;
        if read == 0 {
            break;
        }
        digest.update(&buffer[..read]);
    }
    Ok(format!("{:x}", digest.finalize()))
}

fn row_byte_bound(row: &[String]) -> usize {
    row.iter().map(|v| v.len() + 4).sum()
}

/// SHA-256, byte count and data-row count of a CSV file, checked against the expected header.
pub fn checksum_and_rows(
    path: &Path,
    expected_header: &[String],
) -> Result<(String, i64, i64), DestinationError> {
    let digest = checksum_file(path)?;
    let bytes_count = path.metadata().map(|m| m.len() as i64).unwrap_or(0);
    let mut reader = csv::ReaderBuilder::new()
        .has_headers(false)
        .from_path(path)
        .map_err(|_| derr("CSV partition cannot be read as a declared BI dataset"))?;
    let mut records = reader.records();
    let header = match records.next() {
        Some(Ok(record)) => record.iter().map(|s| s.to_owned()).collect::<Vec<String>>(),
        _ => {
            return Err(derr(
                "CSV partition cannot be read as a declared BI dataset",
            ))
        }
    };
    if header != expected_header {
        return Err(derr(
            "CSV partition header does not match the declared BI schema",
        ));
    }
    let mut rows = 0i64;
    for record in records {
        let record =
            record.map_err(|_| derr("CSV partition cannot be read as a declared BI dataset"))?;
        let row: Vec<String> = record.iter().map(|s| s.to_owned()).collect();
        if row.len() != header.len() {
            return Err(derr("CSV partition row width does not match the BI schema"));
        }
        if row_byte_bound(&row) > GOOGLE_CHUNK_SOURCE_BYTES {
            return Err(derr("BI CSV row exceeds the bounded Google request size"));
        }
        rows += 1;
    }
    Ok((digest, bytes_count, rows))
}

/// Verify every partition against the manifest and return per-dataset `rows`, `fields` and `bytes`.
pub fn verify_partitions(
    root: &Path,
    manifest: &Value,
) -> Result<Map<String, Value>, DestinationError> {
    let mut datasets = Map::new();
    let declared = manifest
        .get("datasets")
        .and_then(Value::as_object)
        .cloned()
        .unwrap_or_default();
    let selected = manifest.get("selected_projection").and_then(Value::as_bool) == Some(true);
    for (name, dataset) in &declared {
        if !dataset.is_object() {
            return Err(derr("package dataset declaration is invalid"));
        }
        let fields = validated_fields(
            name,
            dataset.get("fields").unwrap_or(&Value::Null),
            selected,
            manifest.get("metrics_dimension_columns"),
        )?;
        if !selected {
            let (_, grain, primary_key) = dataset_spec(name).unwrap_or((&[], "", &[]));
            let dataset_schema = dataset.get("schema_version").and_then(Value::as_str);
            let grain_ok = dataset.get("grain").and_then(Value::as_str) == Some(grain);
            let key_ok = dataset
                .get("primary_key")
                .and_then(Value::as_array)
                .map(|k| {
                    k.iter()
                        .map(|v| v.as_str().unwrap_or_default())
                        .collect::<Vec<_>>()
                        == primary_key.to_vec()
                })
                .unwrap_or(false);
            if !matches(dataset_schema, &format!("spiderhead.bi.{name}.v1")) || !grain_ok || !key_ok
            {
                return Err(derr(format!(
                    "dataset {name:?} version, grain or key differs from the BI schema"
                )));
            }
        }
        let row_count = dataset.get("row_count");
        if !row_count.map(is_int).unwrap_or(false)
            || as_i64(row_count.unwrap_or(&Value::Null))
                .map(|r| r < 0)
                .unwrap_or(true)
        {
            return Err(derr(format!("dataset {name:?} row count is invalid")));
        }
        let bytes_total = dataset.get("bytes");
        if !bytes_total.map(is_int).unwrap_or(false)
            || as_i64(bytes_total.unwrap_or(&Value::Null))
                .map(|b| b < 0)
                .unwrap_or(true)
        {
            return Err(derr(format!("dataset {name:?} byte count is invalid")));
        }
        let mut rows: i64 = 0;
        let mut bytes_count: i64 = 0;
        for part in dataset
            .get("partitions")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default()
        {
            if !part.is_object() {
                return Err(derr(format!(
                    "dataset {name:?} partition declaration is invalid"
                )));
            }
            let relative = part.get("path").and_then(Value::as_str).unwrap_or_default();
            let path = root.join(relative);
            let invalid_path = path.parent() != Some(root) || path.is_symlink() || !path.is_file();
            if invalid_path {
                return Err(derr(format!(
                    "dataset {name:?} has an invalid partition path"
                )));
            }
            let (checksum, part_bytes, part_rows) = checksum_and_rows(&path, &fields)?;
            if checksum
                != part
                    .get("sha256")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
            {
                return Err(derr(format!(
                    "dataset {name:?} partition checksum does not match manifest"
                )));
            }
            if !part.get("rows").map(is_int).unwrap_or(false)
                || as_i64(part.get("rows").unwrap_or(&Value::Null)) != Some(part_rows)
            {
                return Err(derr(format!(
                    "dataset {name:?} partition row count does not match CSV"
                )));
            }
            if !part.get("bytes").map(is_int).unwrap_or(false)
                || as_i64(part.get("bytes").unwrap_or(&Value::Null)) != Some(part_bytes)
            {
                return Err(derr(format!(
                    "dataset {name:?} partition byte count does not match CSV"
                )));
            }
            rows += part_rows;
            bytes_count += part_bytes;
        }
        if Some(rows) != as_i64(row_count.unwrap_or(&Value::Null))
            || Some(bytes_count) != as_i64(bytes_total.unwrap_or(&Value::Null))
        {
            return Err(derr(format!(
                "dataset {name:?} partition totals do not match manifest"
            )));
        }
        datasets.insert(
            name.clone(),
            json!({"rows": rows, "fields": fields.len() as i64, "bytes": bytes_count}),
        );
    }
    verify_group_member_references(root, manifest).map_err(from_export)?;
    Ok(datasets)
}

/// Disclose complete local companions excluded from destination datasets.
pub fn external_companion_refs(manifest: &Value) -> Vec<Value> {
    let companion = manifest.get("group_members");
    let Some(companion) = companion.filter(|c| !c.is_null()) else {
        return Vec::new();
    };
    vec![json!({
        "format": companion.get("format").cloned().unwrap_or(Value::Null),
        "sha256": companion.get("sha256").cloned().unwrap_or(Value::Null),
        "run_id": companion.get("run_id").cloned().unwrap_or(Value::Null),
        "source_audit_sha256": companion.get("source_audit_sha256").cloned().unwrap_or(Value::Null),
        "manifest_reference": "manifest.json#/group_members",
        "groups": companion.get("groups").and_then(|g| g.get("row_count")).cloned().unwrap_or(Value::Null),
        "members": companion.get("members").and_then(|g| g.get("row_count")).cloned().unwrap_or(Value::Null),
        "publication": "local_only",
        "published": false,
        "reason": "Complete group membership remains in the local package; only main datasets are published",
    })]
}

/// Preflight a one-worksheet-per-dataset Sheets import, with no API call.
pub fn sheets_plan(package: &Path, max_cells: i64) -> Result<Value, DestinationError> {
    let capacity = sheets_max_cells();
    if !(1..=capacity).contains(&max_cells) {
        return Err(derr(format!("max_cells must be 1..{capacity}")));
    }
    let (root, manifest) = manifest(package)?;
    let datasets = verify_partitions(&root, &manifest)?;
    let mut worksheets = Vec::new();
    let mut used_cells: i64 = 0;
    for (name, info) in &datasets {
        let rows = info.get("rows").and_then(Value::as_i64).unwrap_or(0);
        let columns = info.get("fields").and_then(Value::as_i64).unwrap_or(0);
        let cells = (rows + 1) * columns;
        used_cells += cells;
        worksheets.push(json!({
            "worksheet": name,
            "rows": rows,
            "header_rows": 1,
            "columns": columns,
            "cells": cells,
            "operation": "append_or_replace_requires_explicit_apply",
        }));
    }
    let ready = used_cells <= max_cells;
    Ok(json!({
        "format": "spiderhead.bi-sheets-plan.v1",
        "network": false,
        "apply": false,
        "package_schema_version": manifest.get("schema_version").cloned().unwrap_or(Value::Null),
        "worksheets": worksheets,
        "external_companion_refs": external_companion_refs(&manifest),
        "required_cells": used_cells,
        "capacity_cells": max_cells,
        "state": if ready { "ready" } else { "unavailable" },
        "reason": if ready { Value::Null } else { json!("package exceeds declared Sheets cell capacity") },
        "required_authorization": "explicit spreadsheet target, least-privilege Google grant, and apply confirmation",
    }))
}

/// Describe an optional BigQuery load without a project, credential or write.
pub fn bigquery_plan(
    package: &Path,
    dataset: &str,
    operation: &str,
) -> Result<Value, DestinationError> {
    if dataset.is_empty() {
        return Err(derr("dataset is required for a BigQuery plan"));
    }
    if !matches!(operation, "replace" | "append") {
        return Err(derr("operation must be 'replace' or 'append'"));
    }
    let (root, manifest) = manifest(package)?;
    let datasets = verify_partitions(&root, &manifest)?;
    let tables: Vec<Value> = datasets
        .iter()
        .map(|(name, info)| {
            json!({
                "table": format!("seohead_{name}_v1"),
                "rows": info.get("rows").cloned().unwrap_or(Value::Null),
                "columns": info.get("fields").cloned().unwrap_or(Value::Null),
            })
        })
        .collect();
    Ok(json!({
        "format": "spiderhead.bi-bigquery-plan.v1",
        "network": false,
        "apply": false,
        "billing_required": true,
        "package_schema_version": manifest.get("schema_version").cloned().unwrap_or(Value::Null),
        "dataset": dataset,
        "operation": operation,
        "tables": tables,
        "external_companion_refs": external_companion_refs(&manifest),
        "required_authorization": "explicit billed project/dataset target and separate cost plus apply confirmation",
    }))
}

/// Yield the header once, then bounded data chunks; the row byte bound applies to each raw CSV row.
/// The callback returns `false` to stop early.
pub fn csv_chunks(
    root: &Path,
    dataset: &Value,
    size: usize,
    mut emit: impl FnMut(Vec<Vec<String>>) -> Result<bool, DestinationError>,
) -> Result<(), DestinationError> {
    let mut header: Option<Vec<String>> = None;
    let mut chunk: Vec<Vec<String>> = Vec::new();
    let mut chunk_bytes = 0usize;
    for part in dataset
        .get("partitions")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default()
    {
        let path = root.join(part.get("path").and_then(Value::as_str).unwrap_or_default());
        let mut reader = csv::ReaderBuilder::new()
            .has_headers(false)
            .from_path(&path)
            .map_err(|_| derr("dataset partition is unreadable"))?;
        let mut records = reader.records();
        let current_header: Vec<String> = match records.next() {
            Some(Ok(record)) => record.iter().map(|s| s.to_owned()).collect(),
            _ => return Err(derr("dataset partition is unreadable")),
        };
        match &header {
            None => {
                if row_byte_bound(&current_header) > GOOGLE_CHUNK_SOURCE_BYTES {
                    return Err(derr(
                        "BI CSV header exceeds the bounded Google request size",
                    ));
                }
                header = Some(current_header.clone());
                if !emit(vec![current_header])? {
                    return Ok(());
                }
            }
            Some(expected) if *expected != current_header => {
                return Err(derr("dataset partition headers disagree"));
            }
            _ => {}
        }
        for record in records {
            let record = record.map_err(|_| derr("dataset partition is unreadable"))?;
            let row: Vec<String> = record.iter().map(|s| s.to_owned()).collect();
            let row_bytes = row_byte_bound(&row);
            if row_bytes > GOOGLE_CHUNK_SOURCE_BYTES {
                return Err(derr("BI CSV row exceeds the bounded Google request size"));
            }
            if !chunk.is_empty()
                && (chunk.len() >= size || chunk_bytes + row_bytes > GOOGLE_CHUNK_SOURCE_BYTES)
            {
                if !emit(std::mem::take(&mut chunk))? {
                    return Ok(());
                }
                chunk_bytes = 0;
            }
            chunk_bytes += row_bytes;
            chunk.push(row);
        }
    }
    if !chunk.is_empty() {
        emit(chunk)?;
    }
    Ok(())
}

/// Stable per-dataset package identities.
pub fn dataset_hashes(manifest: &Value) -> Map<String, Value> {
    let mut hashes = Map::new();
    for (name, dataset) in manifest
        .get("datasets")
        .and_then(Value::as_object)
        .cloned()
        .unwrap_or_default()
    {
        let partitions: Vec<Value> = dataset
            .get("partitions")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default()
            .iter()
            .map(|part| {
                json!({
                    "path": part.get("path").cloned().unwrap_or(Value::Null),
                    "rows": part.get("rows").cloned().unwrap_or(Value::Null),
                    "sha256": part.get("sha256").cloned().unwrap_or(Value::Null),
                })
            })
            .collect();
        // Build the sorted-key compact JSON by hand so the key order is deterministic.
        let identity = json!({"fields": dataset.get("fields").cloned().unwrap_or(Value::Null), "partitions": partitions});
        let mut digest = sha2::Sha256::new();
        digest.update(sorted_compact_json(&identity).as_bytes());
        hashes.insert(name, json!(format!("{:x}", digest.finalize())));
    }
    hashes
}

/// Compact JSON with sorted keys and non-ASCII text kept as-is.
pub fn sorted_compact_json(value: &Value) -> String {
    let mut out = String::new();
    write_sorted(value, &mut out);
    out
}

fn write_sorted(value: &Value, out: &mut String) {
    match value {
        Value::Object(map) => {
            out.push('{');
            let mut first = true;
            for (key, child) in map {
                if !first {
                    out.push(',');
                }
                first = false;
                out.push_str(&serde_json::to_string(key).unwrap_or_default());
                out.push(':');
                write_sorted(child, out);
            }
            out.push('}');
        }
        _ => out.push_str(&serde_json::to_string(value).unwrap_or_default()),
    }
}

/// Show the exact package, target and operation before an explicit write.
pub fn destination_preview(
    host: &dyn Host,
    package: &Path,
    target: &str,
    destination: &str,
    operation: &str,
) -> Result<Value, DestinationError> {
    if !matches!(destination, "sheets" | "bigquery") {
        return Err(derr("destination must be 'sheets' or 'bigquery'"));
    }
    if !matches!(operation, "replace" | "append") {
        return Err(derr("operation must be 'replace' or 'append'"));
    }
    if target.is_empty() {
        return Err(derr("an explicit destination target is required"));
    }
    let (root, manifest) = manifest(package)?;
    let datasets = verify_partitions(&root, &manifest)?;
    let declared_datasets = manifest
        .get("datasets")
        .and_then(Value::as_object)
        .cloned()
        .unwrap_or_default();
    let resolved_target = resolved_target_mapping(
        host,
        destination,
        target,
        &declared_datasets,
        manifest.get("selected_projection").and_then(Value::as_bool) == Some(true),
    )?;
    Ok(json!({
        "format": "spiderhead.bi-destination-preview.v1",
        "state": "ready_to_apply",
        "destination": destination,
        "target": target,
        "resolved_target": resolved_target,
        "operation": operation,
        "package_schema_version": manifest.get("schema_version").cloned().unwrap_or(Value::Null),
        "manifest_sha256": checksum_file(&root.join("manifest.json"))?,
        "datasets": datasets.iter().map(|(n, info)| (n.clone(), json!({
            "rows": info.get("rows").cloned().unwrap_or(Value::Null),
            "columns": info.get("fields").cloned().unwrap_or(Value::Null),
            "bytes": info.get("bytes").cloned().unwrap_or(Value::Null),
        }))).collect::<Map<_, _>>(),
        "external_companion_refs": external_companion_refs(&manifest),
        "required_action": "rerun with apply=true after reviewing this exact target and operation",
    }))
}

/// Recreate exactly the chunk whose remote outcome is uncertain.
pub fn rows_for_pending(
    root: &Path,
    dataset: &Value,
    pending: &Value,
) -> Result<Vec<Vec<String>>, DestinationError> {
    let chunk = pending.get("chunk").and_then(Value::as_i64);
    let Some(chunk) = chunk.filter(|c| *c >= 0) else {
        return Err(derr(
            "destination pending state has an invalid chunk cursor",
        ));
    };
    let mut found: Option<Vec<Vec<String>>> = None;
    let mut index = 0i64;
    csv_chunks(root, dataset, GOOGLE_CHUNK_ROWS, |rows| {
        if index == chunk {
            found = Some(rows);
            return Ok(false);
        }
        index += 1;
        Ok(true)
    })?;
    found.ok_or_else(|| derr("destination pending chunk is absent from the verified package"))
}

/// Describe the fixed remote range or job before sending it.
pub fn pending_write(
    client: &dyn DestinationClient,
    transaction: &Value,
    dataset: &str,
    rows: &[Vec<String>],
    chunk: usize,
) -> Result<Value, DestinationError> {
    let pending = match client.pending_write(transaction, dataset, rows, chunk)? {
        Some(pending) => pending,
        None => json!({"kind": "generic_write", "dataset": dataset, "chunk": chunk}),
    };
    if pending.get("dataset").and_then(Value::as_str) != Some(dataset) {
        return Err(derr("destination client returned an invalid pending write"));
    }
    let mut pending = pending;
    pending["data_rows"] = json!(rows.len() as i64 - i64::from(chunk == 0));
    Ok(pending)
}

/// Record a proven chunk exactly once.
pub fn advance_pending(state: &mut Value, pending: &Value) -> Result<(), DestinationError> {
    let dataset = pending.get("dataset").and_then(Value::as_str);
    let Some(dataset) = dataset else {
        return Err(derr("destination pending state names an unknown dataset"));
    };
    let mut entry = match state.get("datasets").and_then(|d| d.get(dataset)).cloned() {
        Some(entry) if entry.is_object() => entry,
        _ => return Err(derr("destination pending state names an unknown dataset")),
    };
    let next_chunk = entry.get("next_chunk").and_then(Value::as_i64);
    if pending.get("chunk").and_then(Value::as_i64) != next_chunk {
        return Err(derr("destination pending state has a non-monotonic cursor"));
    }
    let data_rows = pending.get("data_rows").and_then(Value::as_i64);
    let Some(data_rows) = data_rows.filter(|r| *r >= 0) else {
        return Err(derr("destination pending state has invalid row accounting"));
    };
    let written = entry
        .get("written_rows")
        .and_then(Value::as_i64)
        .unwrap_or(0);
    entry["written_rows"] = json!(written + data_rows);
    entry["next_chunk"] = json!(next_chunk.unwrap_or(0) + 1);
    if let Some(datasets) = state.get_mut("datasets").and_then(Value::as_object_mut) {
        datasets.insert(dataset.to_owned(), entry);
    }
    state["pending"] = Value::Null;
    state["status"] = json!("staging");
    Ok(())
}

fn save(path: &Path, state: &Value) -> Result<(), DestinationError> {
    save_state(path, state).map_err(DestinationError::Error)
}

fn result(state: &Value, reason: Option<&str>) -> Result<Value, DestinationError> {
    result_from_state(state, reason).map_err(DestinationError::Error)
}

fn pending_is_begin(state: &Value) -> bool {
    state.get("pending") == Some(&json!({"kind": "begin"}))
}

/// Require explicit read reconciliation before an uncertain request may continue.
pub fn reconcile_pending(
    root: &Path,
    manifest: &Value,
    state: &mut Value,
    client: &dyn DestinationClient,
    checkpoint: &Path,
    reconcile: bool,
) -> Result<Option<Value>, DestinationError> {
    let pending = state.get("pending").cloned().unwrap_or(Value::Null);
    if pending.is_null() {
        return Ok(None);
    }
    if !pending.is_object() {
        return Err(derr(
            "destination progress state has an invalid pending operation",
        ));
    }
    if !reconcile {
        state["status"] = json!("reconciliation_required");
        save(checkpoint, state)?;
        return Ok(Some(result(
            state,
            Some(
                "a prior remote operation is uncertain; rerun with reconcile=true before any retry",
            ),
        )?));
    }
    if pending.get("kind").and_then(Value::as_str) == Some("commit") {
        let transaction = state.get_mut("transaction").cloned().unwrap_or(Value::Null);
        let mut tx = transaction;
        let outcome = match client.reconcile_commit(&mut tx)? {
            Some(outcome) => outcome,
            None => {
                state["status"] = json!("reconciliation_required");
                save(checkpoint, state)?;
                return Ok(Some(result(
                    state,
                    Some("destination client cannot reconcile an uncertain final publication"),
                )?));
            }
        };
        state["transaction"] = tx;
        match outcome {
            CommitOutcome::Applied => {
                state["pending"] = Value::Null;
                state["status"] = json!("committed");
                save(checkpoint, state)?;
                let mut res = result(state, None)?;
                if let Some(o) = res.as_object_mut() {
                    o.insert(
                        "dataset_sha256".to_owned(),
                        Value::Object(dataset_hashes(manifest)),
                    );
                    o.insert("row_conservation".to_owned(), json!("verified"));
                    o.insert(
                        "publication".to_owned(),
                        json!("reconciled_after_uncertain_commit"),
                    );
                }
                return Ok(Some(res));
            }
            CommitOutcome::NotApplied => {
                state["pending"] = Value::Null;
                state["status"] = json!("staging");
                save(checkpoint, state)?;
                return Ok(None);
            }
        }
    }
    let dataset = pending.get("dataset").and_then(Value::as_str);
    let declared = manifest
        .get("datasets")
        .and_then(Value::as_object)
        .cloned()
        .unwrap_or_default();
    let Some(dataset) = dataset.filter(|d| declared.contains_key(*d)) else {
        return Err(derr("destination pending state names an unknown dataset"));
    };
    let rows = rows_for_pending(
        root,
        declared.get(dataset).unwrap_or(&Value::Null),
        &pending,
    )?;
    let mut tx = state.get("transaction").cloned().unwrap_or(Value::Null);
    let outcome = match client.reconcile_pending(&mut tx, &pending, &rows)? {
        Some(outcome) => outcome,
        None => {
            state["status"] = json!("reconciliation_required");
            save(checkpoint, state)?;
            return Ok(Some(result(
                state,
                Some("destination client cannot reconcile the uncertain bounded write"),
            )?));
        }
    };
    state["transaction"] = tx;
    match outcome {
        PendingOutcome::Applied => {
            advance_pending(state, &pending)?;
            save(checkpoint, state)?;
            Ok(None)
        }
        PendingOutcome::NotApplied => {
            state["pending"] = Value::Null;
            state["status"] = json!("staging");
            save(checkpoint, state)?;
            Ok(None)
        }
    }
}

pub fn begin_transaction(
    state: &mut Value,
    checkpoint: &Path,
    client: &dyn DestinationClient,
    target: &str,
    operation: &str,
    manifest: &Value,
    manifest_sha256: &str,
) -> Result<(), DestinationError> {
    let transaction = client.begin(&BeginArgs {
        target,
        operation,
        schema_version: BI_SCHEMA_VERSION,
        datasets: manifest
            .get("datasets")
            .and_then(Value::as_object)
            .unwrap_or(&Map::new()),
        package_sha256: manifest_sha256,
        selected_projection: manifest.get("selected_projection").and_then(Value::as_bool)
            == Some(true),
    })?;
    state["transaction"] = transaction;
    state["pending"] = Value::Null;
    state["status"] = json!("staging");
    save(checkpoint, state)
}

#[allow(clippy::too_many_arguments)]
pub fn reconcile_begin(
    state: &mut Value,
    checkpoint: &Path,
    client: &dyn DestinationClient,
    target: &str,
    operation: &str,
    manifest: &Value,
    manifest_sha256: &str,
    reconcile: bool,
) -> Result<Option<Value>, DestinationError> {
    if !pending_is_begin(state) {
        return Ok(None);
    }
    if !reconcile {
        state["status"] = json!("reconciliation_required");
        save(checkpoint, state)?;
        return Ok(Some(result(
            state,
            Some("a prior destination begin is uncertain; rerun with reconcile=true before retry"),
        )?));
    }
    let datasets = manifest
        .get("datasets")
        .and_then(Value::as_object)
        .cloned()
        .unwrap_or_default();
    let args = BeginArgs {
        target,
        operation,
        schema_version: BI_SCHEMA_VERSION,
        datasets: &datasets,
        package_sha256: manifest_sha256,
        selected_projection: manifest.get("selected_projection").and_then(Value::as_bool)
            == Some(true),
    };
    let outcome = match client.reconcile_begin(&args)? {
        Some(outcome) => outcome,
        None => {
            state["status"] = json!("reconciliation_required");
            save(checkpoint, state)?;
            return Ok(Some(result(
                state,
                Some("destination client cannot reconcile an uncertain begin operation"),
            )?));
        }
    };
    match outcome {
        BeginOutcome::NotApplied => {
            begin_transaction(
                state,
                checkpoint,
                client,
                target,
                operation,
                manifest,
                manifest_sha256,
            )
        }
        .map(|_| None),
        BeginOutcome::Uncertain => {
            state["status"] = json!("reconciliation_required");
            save(checkpoint, state)?;
            Ok(Some(result(
                state,
                Some("destination begin reconciliation did not reach a safe verdict"),
            )?))
        }
        BeginOutcome::Transaction(transaction) => {
            state["transaction"] = transaction;
            state["pending"] = Value::Null;
            state["status"] = json!("staging");
            save(checkpoint, state)?;
            Ok(None)
        }
    }
}

/// Stream a package through a checkpointed, explicitly authorized destination client.
pub fn apply_with_client(
    package: &Path,
    target: &str,
    operation: &str,
    client: &dyn DestinationClient,
    apply: bool,
    reconcile: bool,
) -> Result<Value, DestinationError> {
    if !apply {
        return Err(derr("apply=True is required for a destination write"));
    }
    if !matches!(operation, "replace" | "append") {
        return Err(derr("operation must be 'replace' or 'append'"));
    }
    let (root, manifest) = manifest(package)?;
    let datasets = verify_partitions(&root, &manifest)?;
    if target.is_empty() {
        return Err(derr("an explicit destination target is required"));
    }
    if !client.authorize_target(target) {
        return Err(derr(
            "injected client did not authorize the requested target",
        ));
    }
    let manifest_sha256 = checksum_file(&root.join("manifest.json"))?;
    let destination = {
        let name = client.destination_name();
        if name.is_empty() {
            "destination".to_owned()
        } else {
            name.to_owned()
        }
    };
    let checkpoint = state_path(&root, &destination, target, operation, &manifest_sha256);
    let mut expected = Map::from_iter([
        ("destination".to_owned(), json!(destination)),
        ("target".to_owned(), json!(target)),
        ("operation".to_owned(), json!(operation)),
        ("manifest_sha256".to_owned(), json!(manifest_sha256)),
    ]);
    if let Some(identity) = client.checkpoint_identity() {
        let mut digest = sha2::Sha256::new();
        digest.update(sorted_compact_json(&identity).as_bytes());
        expected.insert(
            "target_sha256".to_owned(),
            json!(format!("{:x}", digest.finalize())),
        );
    }
    let mut state = load_state(&checkpoint, &expected).map_err(DestinationError::Error)?;
    if let Some(state) = &mut state {
        state["external_companion_refs"] = json!(external_companion_refs(&manifest));
    }
    if let Some(state) = &state {
        if state.get("status").and_then(Value::as_str) == Some("committed") {
            let mut res = result(state, None)?;
            if let Some(o) = res.as_object_mut() {
                o.insert(
                    "dataset_sha256".to_owned(),
                    Value::Object(dataset_hashes(&manifest)),
                );
                o.insert("row_conservation".to_owned(), json!("verified"));
            }
            return Ok(res);
        }
        if state.get("status").and_then(Value::as_str) == Some("failed") {
            let reason = state
                .get("reason")
                .and_then(Value::as_str)
                .unwrap_or("prior write failed")
                .to_owned();
            return result(state, Some(&reason));
        }
    }
    let mut state = match state {
        Some(state) => state,
        None => {
            let mut state = new_state(
                &destination,
                target,
                operation,
                &manifest_sha256,
                &datasets,
                &Value::Null,
            );
            if let Some(o) = state.as_object_mut() {
                o.extend(expected.clone());
            }
            state["external_companion_refs"] = json!(external_companion_refs(&manifest));
            state["status"] = json!("begin_pending");
            state["pending"] = json!({"kind": "begin"});
            save(&checkpoint, &state)?;
            match begin_transaction(
                &mut state,
                &checkpoint,
                client,
                target,
                operation,
                &manifest,
                &manifest_sha256,
            ) {
                Ok(()) => state,
                Err(exc) => {
                    state["status"] = json!("reconciliation_required");
                    state["reason"] =
                        json!("destination begin failed after its durable pre-request checkpoint");
                    let _ = save(&checkpoint, &state);
                    return Err(exc);
                }
            }
        }
    };
    if pending_is_begin(&state) {
        match reconcile_begin(
            &mut state,
            &checkpoint,
            client,
            target,
            operation,
            &manifest,
            &manifest_sha256,
            reconcile,
        ) {
            Ok(None) => {}
            Ok(Some(res)) => return Ok(res),
            Err(exc) => {
                state["status"] = json!("reconciliation_required");
                state["reason"] = json!("destination begin reconciliation failed");
                let _ = save(&checkpoint, &state);
                return Err(exc);
            }
        }
    }
    match reconcile_pending(&root, &manifest, &mut state, client, &checkpoint, reconcile) {
        Ok(None) => {}
        Ok(Some(res)) => return Ok(res),
        Err(exc) if exc.is_uncertain() => {
            state["status"] = json!("reconciliation_required");
            state["reason"] = json!(exc.to_string());
            let _ = save(&checkpoint, &state);
            return result(&state, Some(&exc.to_string()));
        }
        Err(exc) => return Err(exc),
    }
    let mut transaction = state.get("transaction").cloned().unwrap_or(Value::Null);
    let stage_result: Result<Value, DestinationError> = (|| {
        for (name, info) in &datasets {
            let next_chunk = state
                .get("datasets")
                .and_then(|d| d.get(name))
                .and_then(|e| e.get("next_chunk"))
                .and_then(Value::as_i64);
            if next_chunk.is_none() {
                return Err(derr(
                    "destination progress state has an invalid chunk cursor",
                ));
            }
            let source = manifest["datasets"][name].clone();
            let mut cursor = next_chunk.unwrap_or(0);
            let mut index = 0i64;
            csv_chunks(&root, &source, GOOGLE_CHUNK_ROWS, |rows| {
                let chunk = index;
                index += 1;
                if chunk < cursor {
                    return Ok(true);
                }
                if chunk > cursor {
                    return Err(derr("destination chunk cursor is not contiguous"));
                }
                let pending = pending_write(client, &transaction, name, &rows, chunk as usize)?;
                state["pending"] = pending.clone();
                state["status"] = json!("staging");
                save(&checkpoint, &state)?;
                let before = transaction.clone();
                if let Err(exc) = client.write(&mut transaction, name, &rows) {
                    // The durable pre-request checkpoint is authoritative until a
                    // read reconciliation proves this exact range or job landed.
                    state["transaction"] = before.clone();
                    transaction = before;
                    return Err(exc);
                }
                advance_pending(&mut state, &pending)?;
                state["transaction"] = transaction.clone();
                save(&checkpoint, &state)?;
                cursor += 1;
                Ok(true)
            })?;
            let written = state
                .get("datasets")
                .and_then(|d| d.get(name))
                .and_then(|e| e.get("written_rows"))
                .and_then(Value::as_i64)
                .unwrap_or(0);
            let expected_rows = info.get("rows").and_then(Value::as_i64).unwrap_or(0);
            if written != expected_rows {
                return Err(derr(format!(
                    "destination row conservation failed for {name:?}"
                )));
            }
        }
        if checksum_file(&root.join("manifest.json"))? != manifest_sha256 {
            return Err(derr("BI manifest changed during destination staging"));
        }
        if verify_partitions(&root, &manifest)? != datasets {
            return Err(derr("BI package changed during destination staging"));
        }
        state["pending"] = json!({"kind": "commit"});
        state["status"] = json!("commit_pending");
        state["transaction"] = transaction.clone();
        save(&checkpoint, &state)?;
        client.commit(&mut transaction)
    })();
    match stage_result {
        Err(exc) if exc.is_uncertain() => {
            state["status"] = json!("reconciliation_required");
            state["reason"] = json!(exc.to_string());
            let _ = save(&checkpoint, &state);
            result(&state, Some(&exc.to_string()))
        }
        Err(exc) => {
            let pending = state.get("pending").cloned().unwrap_or(Value::Null);
            if let Some(data_rows) = pending
                .get("data_rows")
                .and_then(Value::as_i64)
                .filter(|_| pending.is_object())
            {
                let dataset = pending.get("dataset").and_then(Value::as_str);
                if let Some(dataset) = dataset {
                    if let Some(entry) = state
                        .get_mut("datasets")
                        .and_then(Value::as_object_mut)
                        .and_then(|d| d.get_mut(dataset))
                    {
                        let failed = entry
                            .get("failed_rows")
                            .and_then(Value::as_i64)
                            .unwrap_or(0);
                        entry["failed_rows"] = json!(failed + data_rows);
                    }
                }
            }
            state["status"] = json!("failed");
            state["reason"] = json!(exc.to_string());
            let _ = save(&checkpoint, &state);
            // A raised cleanup error replaces the result.
            client.abort(&mut transaction)?;
            result(&state, Some(&exc.to_string()))
        }
        Ok(publication) => {
            state["pending"] = Value::Null;
            state["status"] = json!("committed");
            save(&checkpoint, &state)?;
            let mut res = result(&state, None)?;
            if let Some(o) = res.as_object_mut() {
                o.insert(
                    "dataset_sha256".to_owned(),
                    Value::Object(dataset_hashes(&manifest)),
                );
                o.insert("row_conservation".to_owned(), json!("verified"));
                if let Value::Object(extra) = publication {
                    o.extend(extra);
                }
            }
            Ok(res)
        }
    }
}

/// The segment definitions helper: copied here because the projects crate is not ported.
pub fn segment_definitions(document: &Value) -> Option<Vec<Value>> {
    const MAX_FILTER_VALUES: usize = 10_000;
    let config = document.get("run")?.get("crawl_config")?.as_object()?;
    let analysis = config
        .get("analysis.segments")
        .cloned()
        .unwrap_or(Value::Null);
    if crate::py::truthy(Some(&analysis)) && !analysis.is_array() {
        return None;
    }
    let analysis_list = analysis.as_array().cloned().unwrap_or_default();
    if !analysis_list.is_empty() {
        return Some(analysis_list);
    }
    let scope = config.get("scope.segments").cloned().unwrap_or(Value::Null);
    let scope = match scope.as_array() {
        Some(list) if list.len() <= MAX_FILTER_VALUES => list.clone(),
        Some(_) => return None,
        None if scope.is_null() => Vec::new(),
        None => return None,
    };
    let mut definitions = Vec::new();
    for item in &scope {
        let name = item.get("name").and_then(Value::as_str)?;
        let mut rules = Vec::new();
        if let Some(prefix) = item
            .get("prefix")
            .and_then(Value::as_str)
            .filter(|v| !v.is_empty())
        {
            rules.push(json!({"op": "prefix", "field": "path", "value": prefix}));
        }
        if let Some(host) = item
            .get("host")
            .and_then(Value::as_str)
            .filter(|v| !v.is_empty())
        {
            rules.push(json!({"op": "eq", "field": "host", "value": host}));
        }
        if let Some(pattern) = item
            .get("pattern")
            .and_then(Value::as_str)
            .filter(|v| !v.is_empty())
        {
            rules.push(json!({"op": "regex", "field": "url", "value": pattern}));
        }
        definitions.push(json!({"name": name, "rules": rules}));
    }
    if definitions.is_empty() {
        None
    } else {
        Some(definitions)
    }
}

/// Retained raw page records for the segment engine, as a lazy iterator over the partitions.
pub fn segment_page_records(
    root: &Path,
    dataset: &Value,
) -> (
    impl Iterator<Item = Value>,
    std::sync::Arc<std::sync::Mutex<Option<String>>>,
) {
    let paths: Vec<PathBuf> = dataset
        .get("partitions")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default()
        .iter()
        .map(|part| root.join(part.get("path").and_then(Value::as_str).unwrap_or_default()))
        .collect();
    let field_types: BTreeMap<String, String> = dataset
        .get("fields")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default()
        .iter()
        .filter_map(|f| {
            match (
                f.get("name").and_then(Value::as_str),
                f.get("type").and_then(Value::as_str),
            ) {
                (Some(name), Some(kind)) => Some((name.to_owned(), kind.to_owned())),
                _ => None,
            }
        })
        .collect();
    let failure = std::sync::Arc::new(std::sync::Mutex::new(None::<String>));
    let failure_ref = failure.clone();
    let mut files = paths.into_iter();
    let mut current: Option<csv::StringRecordsIntoIter<fs::File>> = None;
    let mut header: Vec<String> = Vec::new();
    let fail = |flag: &std::sync::Arc<std::sync::Mutex<Option<String>>>, message: &str| {
        if let Ok(mut slot) = flag.lock() {
            *slot = Some(message.to_owned());
        }
    };
    let iter = std::iter::from_fn(move || loop {
        if current.is_none() {
            let path = files.next()?;
            let reader = match csv::ReaderBuilder::new()
                .has_headers(false)
                .from_path(&path)
            {
                Ok(reader) => reader,
                Err(_) => {
                    fail(&failure_ref, "BI page source is unreadable");
                    return None;
                }
            };
            let mut rows = reader.into_records();
            header = match rows.next() {
                Some(Ok(record)) => record.iter().map(|s| s.to_owned()).collect(),
                _ => {
                    fail(&failure_ref, "BI page source is unreadable");
                    return None;
                }
            };
            current = Some(rows);
        }
        let rows = current.as_mut()?;
        match rows.next() {
            None => {
                current = None;
            }
            Some(Err(_)) => {
                fail(&failure_ref, "BI page source is unreadable");
                return None;
            }
            Some(Ok(record)) => {
                let row: BTreeMap<String, String> = header
                    .iter()
                    .cloned()
                    .zip(record.iter().map(|s| s.to_owned()))
                    .collect();
                let url = row.get("url").cloned().unwrap_or_default();
                if url.is_empty() {
                    fail(
                        &failure_ref,
                        "BI page source lacks a URL for segment selection",
                    );
                    return None;
                }
                let raw = row.get("source_fields_json").cloned().unwrap_or_default();
                let source: Map<String, Value> = match if raw.is_empty() {
                    serde_json::from_str("{}")
                } else {
                    serde_json::from_str(&raw)
                } {
                    Ok(Value::Object(map)) => map,
                    _ => {
                        fail(&failure_ref, "BI page source has invalid retained fields");
                        return None;
                    }
                };
                let mut record_out = source;
                record_out.insert("url".to_owned(), json!(url));
                let mut metrics = record_out
                    .get("metrics")
                    .and_then(Value::as_object)
                    .cloned()
                    .unwrap_or_default();
                for (field, field_type) in &field_types {
                    if record_out.contains_key(field)
                        || field.ends_with("_state")
                        || field.ends_with("_reason")
                        || field.ends_with("_json")
                    {
                        continue;
                    }
                    let state_key = match field.as_str() {
                        "response_time_seconds" => "response_time_state".to_owned(),
                        "size_bytes" => "size_state".to_owned(),
                        _ => format!("{field}_state"),
                    };
                    let state = row.get(&state_key).cloned().unwrap_or_default();
                    if state != "measured" && state != "derived" {
                        continue;
                    }
                    let raw_value = row.get(field).cloned().unwrap_or_default();
                    let value = match field_type.as_str() {
                        "integer" => {
                            if raw_value.is_empty() {
                                Value::Null
                            } else {
                                raw_value
                                    .parse::<i64>()
                                    .map(Value::from)
                                    .unwrap_or(Value::Null)
                            }
                        }
                        "number" => {
                            if raw_value.is_empty() {
                                Value::Null
                            } else {
                                raw_value
                                    .parse::<f64>()
                                    .ok()
                                    .and_then(serde_json::Number::from_f64)
                                    .map(Value::Number)
                                    .unwrap_or(Value::Null)
                            }
                        }
                        "boolean" => {
                            if raw_value.is_empty() {
                                Value::Null
                            } else {
                                Value::Bool(raw_value == "true")
                            }
                        }
                        _ => Value::String(raw_value),
                    };
                    record_out.insert(field.clone(), value.clone());
                    metrics.entry(field.clone()).or_insert(value);
                }
                if !metrics.is_empty() {
                    record_out.insert("metrics".to_owned(), Value::Object(metrics));
                }
                return Some(Value::Object(record_out));
            }
        }
    });
    (iter, failure)
}

fn free_bytes(path: &Path) -> Option<u64> {
    spiderhead_core::filesystem::free_space(path).ok()
}

fn private_temp_dir(parent: &Path, prefix: &str) -> Result<PathBuf, DestinationError> {
    for _ in 0..100 {
        let candidate = parent.join(format!("{prefix}{}", uuid::Uuid::new_v4().simple()));
        match fs::create_dir(&candidate) {
            Ok(()) => {
                #[cfg(unix)]
                {
                    use std::os::unix::fs::PermissionsExt;
                    let _ = fs::set_permissions(&candidate, fs::Permissions::from_mode(0o700));
                }
                return Ok(candidate);
            }
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(e) => return Err(derr(format!("temporary staging directory failed: {e}"))),
        }
    }
    Err(derr("temporary staging directory failed"))
}

fn chmod_private(path: &Path) {
    let _ = spiderhead_core::filesystem::set_permissions_mode(path, 0o600);
}

/// Publish an exact selected CSV view with source coverage and bounded output.
#[allow(clippy::too_many_arguments)]
pub fn filter_package(
    package: &Path,
    dataset: &str,
    out_dir: &Path,
    where_: Option<&Map<String, Value>>,
    columns: Option<&[String]>,
    max_rows_per_file: usize,
    max_bytes_per_file: u64,
    max_output_bytes: u64,
) -> Result<Value, DestinationError> {
    if !(1..=MAX_ROWS_PER_PARTITION).contains(&max_rows_per_file) {
        return Err(derr(format!(
            "max_rows_per_file must be 1..{MAX_ROWS_PER_PARTITION}"
        )));
    }
    if !(1024..=MAX_BYTES_PER_PARTITION).contains(&max_bytes_per_file) {
        return Err(derr(format!(
            "max_bytes_per_file must be 1024..{MAX_BYTES_PER_PARTITION}"
        )));
    }
    if !(max_bytes_per_file..=MAX_OUTPUT_BYTES).contains(&max_output_bytes) {
        return Err(derr(
            "max_output_bytes must cover one partition and stay within the hard bound",
        ));
    }
    let (root, manifest) = manifest(package)?;
    let datasets = verify_partitions(&root, &manifest)?;
    if !datasets.contains_key(dataset) {
        return Err(derr("dataset is not declared by the BI package"));
    }
    let source_dataset = manifest["datasets"][dataset].clone();
    let declared_fields = source_dataset
        .get("fields")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    let declared: indexmap::IndexMap<String, Value> = declared_fields
        .iter()
        .filter_map(|f| {
            f.get("name")
                .and_then(Value::as_str)
                .map(|n| (n.to_owned(), f.clone()))
        })
        .collect();
    let selected: Vec<String> = match columns {
        None => declared.keys().cloned().collect(),
        Some(columns) => columns.to_vec(),
    };
    if selected.is_empty()
        || selected.iter().any(|c| c.is_empty())
        || selected
            .iter()
            .collect::<std::collections::BTreeSet<_>>()
            .len()
            != selected.len()
        || selected.iter().any(|c| !declared.contains_key(c))
    {
        return Err(derr(
            "columns must be a non-empty unique subset of declared fields",
        ));
    }
    let predicates = where_.cloned().unwrap_or_default();
    let virtual_fields: std::collections::BTreeSet<&str> = if dataset == "findings" {
        ["segment"].into_iter().collect()
    } else {
        Default::default()
    };
    if predicates
        .keys()
        .any(|k| !declared.contains_key(k) && !virtual_fields.contains(k.as_str()))
    {
        return Err(derr("where keys must be declared fields"));
    }
    for values in predicates.values() {
        let Some(list) = values.as_array() else {
            return Err(derr("where values must be bounded lists of exact strings"));
        };
        if list.len() > 10_000
            || list.iter().any(|v| {
                !v.is_string() || v.as_str().unwrap_or_default().len() as u64 > MAX_CELL_BYTES
            })
        {
            return Err(derr("where values must be bounded lists of exact strings"));
        }
    }
    let predicates_json = serde_json::to_string(&predicates).unwrap_or_default();
    if predicates_json.len() as u64 > MAX_MANIFEST_BYTES {
        return Err(derr("where exceeds the bounded selection definition"));
    }
    let allowed_values: BTreeMap<String, std::collections::BTreeSet<String>> = predicates
        .iter()
        .map(|(k, v)| {
            (
                k.clone(),
                v.as_array()
                    .map(|a| {
                        a.iter()
                            .filter_map(|v| v.as_str().map(str::to_owned))
                            .collect()
                    })
                    .unwrap_or_default(),
            )
        })
        .collect();
    let needs_segments = predicates.contains_key("segment");
    let mut definitions: Vec<Value> = Vec::new();
    if needs_segments {
        let settings = manifest
            .get("run")
            .and_then(|r| r.get("crawl_settings"))
            .cloned()
            .unwrap_or(Value::Null);
        if !settings.is_object() {
            return Err(derr("segment selection requires retained crawl settings"));
        }
        definitions =
            segment_definitions(&json!({"run": {"crawl_config": settings}})).unwrap_or_default();
        if definitions.is_empty() {
            return Err(derr(
                "segment selection requires retained analysis or scope segments",
            ));
        }
        if !manifest
            .get("datasets")
            .and_then(|d| d.get("pages"))
            .map(Value::is_object)
            .unwrap_or(false)
        {
            return Err(derr("segment selection requires retained source page rows"));
        }
    }
    // Absolute path, lexical only: no filesystem resolution.
    let destination = std::path::absolute(out_dir).unwrap_or_else(|_| out_dir.to_path_buf());
    if destination.is_symlink()
        || destination.exists()
        || !destination
            .parent()
            .map(|p| p.is_dir() && !p.is_symlink())
            .unwrap_or(false)
    {
        return Err(derr(
            "out_dir must be a new child of an existing non-symlink directory",
        ));
    }
    let source_manifest_sha256 = checksum_file(&root.join("manifest.json"))?;
    let mut source_rows: i64 = 0;
    let mut selected_states: BTreeMap<String, i64> = BTreeMap::new();
    let mut source_states: BTreeMap<String, i64> = BTreeMap::new();
    let stage = private_temp_dir(
        destination.parent().unwrap_or_else(|| Path::new(".")),
        ".seohead-bi-filter-",
    )?;
    let stage_cleanup = TempDirGuard(stage.clone());
    let result = (|| -> Result<Value, DestinationError> {
        // The projection index lives in the destination's parent, so it is
        // dropped before the staging directory.
        let index = if needs_segments {
            Some(
                projection_index(
                    destination.parent().unwrap_or_else(|| Path::new(".")),
                    MAX_PROJECTION_INDEX_BYTES,
                )
                .map_err(from_export)?,
            )
        } else {
            None
        };
        let segment_index = match &index {
            Some(index) => {
                let segments =
                    PrimarySegmentIndex::new(&index.con, &definitions).map_err(from_export)?;
                let (records, failure) =
                    segment_page_records(&root, &manifest["datasets"]["pages"]);
                segments.build(records).map_err(from_export)?;
                if let Some(message) = failure.lock().ok().and_then(|f| f.clone()) {
                    return Err(derr(message));
                }
                let mut known: std::collections::BTreeSet<String> =
                    ["default".to_owned()].into_iter().collect();
                known.extend(segments.order.iter().map(|s| s.name.clone()));
                let mut unknown: Vec<String> = allowed_values
                    .get("segment")
                    .cloned()
                    .unwrap_or_default()
                    .into_iter()
                    .filter(|s| !known.contains(s))
                    .collect();
                unknown.sort();
                if !unknown.is_empty() {
                    return Err(derr(format!(
                        "segment selection names segments absent from the source run: {unknown:?}"
                    )));
                }
                Some(segments)
            }
            None => None,
        };
        let budget = OutputBudget::new(max_output_bytes);
        let mut writer = PartitionWriter::new(
            &stage,
            dataset,
            &selected
                .iter()
                .map(|name| Field {
                    name: std::borrow::Cow::Owned(name.clone()),
                    type_: match declared
                        .get(name.as_str())
                        .and_then(|d| d.get("type"))
                        .and_then(Value::as_str)
                    {
                        Some("boolean") => "boolean",
                        Some("integer") => "integer",
                        Some("number") => "number",
                        Some("date") => "date",
                        Some("json") => "json",
                        _ => "string",
                    },
                    nullable: declared
                        .get(name.as_str())
                        .and_then(|d| d.get("nullable"))
                        .and_then(Value::as_bool)
                        .unwrap_or(false),
                    description: std::borrow::Cow::Borrowed(""),
                })
                .collect::<Vec<Field>>(),
            max_rows_per_file,
            max_bytes_per_file,
            &budget,
        );
        let write_result = (|| -> Result<Value, DestinationError> {
            for part in source_dataset
                .get("partitions")
                .and_then(Value::as_array)
                .cloned()
                .unwrap_or_default()
            {
                let path = root.join(part.get("path").and_then(Value::as_str).unwrap_or_default());
                let reader = csv::ReaderBuilder::new()
                    .has_headers(false)
                    .from_path(&path)
                    .map_err(|_| derr("source partition is unreadable"))?;
                let mut rows = reader.into_records();
                let header: Vec<String> = match rows.next() {
                    Some(Ok(record)) => record.iter().map(|s| s.to_owned()).collect(),
                    _ => return Err(derr("source partition is unreadable")),
                };
                let positions: Vec<usize> = selected
                    .iter()
                    .filter_map(|name| header.iter().position(|h| h == name))
                    .collect();
                for record in rows {
                    let record = record.map_err(|_| derr("source partition is unreadable"))?;
                    let row: BTreeMap<String, String> = header
                        .iter()
                        .cloned()
                        .zip(record.iter().map(|s| s.to_owned()))
                        .collect();
                    source_rows += 1;
                    let state = row
                        .get("state")
                        .cloned()
                        .unwrap_or_else(|| "not_declared".into());
                    *source_states.entry(state.clone()).or_insert(0) += 1;
                    let matches_predicates = allowed_values
                        .iter()
                        .filter(|(k, _)| k.as_str() != "segment")
                        .all(|(key, allowed)| {
                            row.get(key).map(|v| allowed.contains(v)).unwrap_or(false)
                        });
                    let mut matched = matches_predicates;
                    if matched && allowed_values.contains_key("segment") {
                        let url = row.get("url").cloned().unwrap_or_default();
                        matched = match &segment_index {
                            Some(index) if !url.is_empty() => match index.primary(&url) {
                                Ok(primary) => allowed_values
                                    .get("segment")
                                    .map(|allowed| allowed.contains(primary.as_str()))
                                    .unwrap_or(false),
                                Err(exc) => return Err(derr(exc.0)),
                            },
                            _ => false,
                        };
                    }
                    if matched {
                        *selected_states.entry(state).or_insert(0) += 1;
                        let mut values: Vec<String> = positions
                            .iter()
                            .map(|&i| record.iter().nth(i).unwrap_or_default().to_owned())
                            .collect();
                        for (index, name) in selected.iter().enumerate() {
                            if declared
                                .get(name.as_str())
                                .and_then(|d| d.get("type"))
                                .and_then(Value::as_str)
                                == Some("string")
                            {
                                let safe =
                                    spiderhead_core::tabular::neutralize_formula(&values[index]);
                                if safe != values[index] {
                                    writer.neutralized_cells += 1;
                                }
                                values[index] = safe;
                            }
                        }
                        writer.write_cells(&values).map_err(from_export)?;
                    }
                }
            }
            writer.finish().map_err(from_export)
        })();
        writer.close();
        let output = write_result?;
        if source_rows
            != datasets
                .get(dataset)
                .and_then(|d| d.get("rows"))
                .and_then(Value::as_i64)
                .unwrap_or(0)
        {
            return Err(derr("source row conservation changed during selection"));
        }
        if checksum_file(&root.join("manifest.json"))? != source_manifest_sha256
            || verify_partitions(&root, &manifest)? != datasets
        {
            return Err(derr("source package changed during selection"));
        }
        let mut coverage_companion = Value::Null;
        let mut group_companion = Value::Null;
        if dataset == "findings" {
            group_companion = copy_group_members_companion(
                &root,
                &stage,
                manifest.get("group_members").unwrap_or(&Value::Null),
                &budget,
            )
            .map_err(from_export)?
            .unwrap_or(Value::Null);
            let source_coverage = manifest
                .get("datasets")
                .and_then(|d| d.get("coverage"))
                .cloned()
                .unwrap_or(Value::Null);
            if !source_coverage.is_object() {
                return Err(derr("source package has no immutable coverage dataset"));
            }
            let coverage_fields = source_coverage.get("fields").and_then(Value::as_array);
            let coverage_parts = source_coverage.get("partitions").and_then(Value::as_array);
            let (Some(coverage_fields), Some(coverage_parts)) = (coverage_fields, coverage_parts)
            else {
                return Err(derr("source coverage dataset has no field declarations"));
            };
            let mut coverage_output = Map::from_iter([
                ("row_count".to_owned(), json!(0)),
                ("partition_count".to_owned(), json!(0)),
                ("bytes".to_owned(), json!(0)),
                ("partitions".to_owned(), json!([])),
            ]);
            for part in coverage_parts {
                let relative = part.get("path").and_then(Value::as_str);
                let byte_count = part.get("bytes").and_then(Value::as_i64);
                let (Some(relative), Some(byte_count)) = (relative, byte_count) else {
                    return Err(derr("source coverage partition is invalid"));
                };
                if byte_count < 0 {
                    return Err(derr("source coverage partition is invalid"));
                }
                budget
                    .reserve_bytes(byte_count as u64)
                    .map_err(from_export)?;
                budget.reserve_partition().map_err(from_export)?;
                let source_path = root.join(relative);
                let copied_path = stage.join(relative);
                if let Some(parent) = copied_path.parent() {
                    fs::create_dir_all(parent)
                        .map_err(|e| derr(format!("coverage companion failed: {e}")))?;
                }
                let mut source_file = fs::File::open(&source_path)
                    .map_err(|e| derr(format!("coverage companion failed: {e}")))?;
                let mut copied = fs::OpenOptions::new()
                    .write(true)
                    .create_new(true)
                    .open(&copied_path)
                    .map_err(|e| derr(format!("coverage companion failed: {e}")))?;
                std::io::copy(&mut source_file, &mut copied)
                    .map_err(|e| derr(format!("coverage companion failed: {e}")))?;
                chmod_private(&copied_path);
                let rows = part.get("rows").cloned().unwrap_or(Value::Null);
                coverage_output["row_count"] = json!(
                    coverage_output["row_count"].as_i64().unwrap_or(0) + rows.as_i64().unwrap_or(0)
                );
                coverage_output["partition_count"] =
                    json!(coverage_output["partition_count"].as_i64().unwrap_or(0) + 1);
                coverage_output["bytes"] =
                    json!(coverage_output["bytes"].as_i64().unwrap_or(0) + byte_count);
                if let Some(a) = coverage_output["partitions"].as_array_mut() {
                    a.push(part.clone());
                }
            }
            coverage_output.insert(
                "formula_safe_cells_prefixed".to_owned(),
                source_coverage
                    .get("formula_safe_cells_prefixed")
                    .cloned()
                    .unwrap_or(json!(0)),
            );
            if coverage_output["row_count"].as_i64().unwrap_or(0)
                != datasets
                    .get("coverage")
                    .and_then(|c| c.get("rows"))
                    .and_then(Value::as_i64)
                    .unwrap_or(0)
            {
                return Err(derr("coverage companion row conservation changed"));
            }
            coverage_companion = json!({
                "dataset": "coverage",
                "fields": coverage_fields,
                "row_count": coverage_output["row_count"].clone(),
                "partition_count": coverage_output["partition_count"].clone(),
                "bytes": coverage_output["bytes"].clone(),
                "partitions": coverage_output["partitions"].clone(),
                "formula_safe_cells_prefixed": coverage_output["formula_safe_cells_prefixed"].clone(),
                "state": source_coverage.get("state").cloned().unwrap_or(Value::Null),
                "reason": source_coverage.get("reason").cloned().unwrap_or(Value::Null),
                "source_population": source_coverage.get("source_population").cloned().unwrap_or(Value::Null),
                "coverage": source_coverage.get("coverage").cloned().unwrap_or(Value::Null),
                "selection": "complete immutable source coverage; not filtered with findings",
            });
            if checksum_file(&root.join("manifest.json"))? != source_manifest_sha256
                || verify_partitions(&root, &manifest)? != datasets
            {
                return Err(derr("source package changed during coverage preservation"));
            }
        }
        let result = json!({
            "format": "spiderhead.bi-filter.v1",
            "source_schema_version": manifest.get("schema_version").cloned().unwrap_or(Value::Null),
            "dataset": dataset,
            "metrics_dimension_columns": manifest.get("metrics_dimension_columns").cloned().unwrap_or(Value::Null),
            "columns": selected,
            "where": Value::Object(predicates.clone()),
            "row_count": output.get("row_count").cloned().unwrap_or(Value::Null),
            "partition_count": output.get("partition_count").cloned().unwrap_or(Value::Null),
            "bytes": output.get("bytes").cloned().unwrap_or(Value::Null),
            "partitions": output.get("partitions").cloned().unwrap_or(Value::Null),
            "formula_safe_cells_prefixed": output.get("formula_safe_cells_prefixed").cloned().unwrap_or(Value::Null),
            "selection_state": "complete",
            "source_manifest_sha256": source_manifest_sha256,
            "source": {
                "run": manifest.get("run").cloned().unwrap_or(Value::Null),
                "input": manifest.get("input").cloned().unwrap_or(Value::Null),
                "crawl_completeness": manifest.get("crawl_completeness").cloned().unwrap_or(Value::Null),
                "provider_sources": manifest.get("provider_sources").cloned().unwrap_or(Value::Null),
                "source": manifest.get("source").cloned().unwrap_or(Value::Null),
            },
            "source_coverage": {
                "state": source_dataset.get("state").cloned().unwrap_or(Value::Null),
                "reason": source_dataset.get("reason").cloned().unwrap_or(Value::Null),
                "source_population": source_dataset.get("source_population").cloned().unwrap_or(Value::Null),
                "coverage": source_dataset.get("coverage").cloned().unwrap_or(Value::Null),
            },
            "coverage_companion": coverage_companion,
            "group_members": group_companion,
            "external_companion_refs": external_companion_refs(&json!({"group_members": group_companion})),
            "conservation": {
                "source_rows": source_rows,
                "selected_rows": output.get("row_count").cloned().unwrap_or(Value::Null),
                "omitted_rows": source_rows - output.get("row_count").and_then(Value::as_i64).unwrap_or(0),
                "source_states": source_states,
                "selected_states": selected_states,
                "omission_reason": "explicit exact-filter selection; not evidence of resolution",
            },
            "resource_bounds": {
                "max_rows_per_partition": max_rows_per_file,
                "max_bytes_per_partition": max_bytes_per_file,
                "max_output_bytes": max_output_bytes,
                "max_output_partitions": super::schema::max_output_partitions(),
                "max_cell_bytes": MAX_CELL_BYTES,
                "min_free_disk_bytes": MIN_FREE_DISK_BYTES,
            },
        });
        let encoded =
            crate::bi::writer::dump_manifest(&result).map_err(|exc| derr(exc.to_string()))?;
        if encoded.len() as u64 > MAX_MANIFEST_BYTES {
            return Err(derr("selected manifest exceeds its byte bound"));
        }
        budget
            .reserve_bytes(encoded.len() as u64)
            .map_err(from_export)?;
        let manifest_path = stage.join("manifest.json");
        fs::write(&manifest_path, &encoded)
            .map_err(|e| derr(format!("selected manifest write failed: {e}")))?;
        chmod_private(&manifest_path);
        let (_selected_root, selected_manifest) = self::manifest(&stage)?;
        verify_partitions(&stage, &selected_manifest)?;
        fs::rename(&stage, &destination)
            .map_err(|e| derr(format!("selected package publication failed: {e}")))?;
        Ok(result)
    })();
    let _ = stage_cleanup;
    result.map(|res| {
        let mut out = Map::from_iter([
            ("ok".to_owned(), json!(true)),
            (
                "output_directory".to_owned(),
                json!(destination.to_string_lossy()),
            ),
        ]);
        if let Value::Object(extra) = res {
            out.extend(extra);
        }
        Value::Object(out)
    })
}

/// Drop removes the staging tree; a successful rename already moved it, so Drop is a no-op.
struct TempDirGuard(PathBuf);

impl Drop for TempDirGuard {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

/// Drop removes a temporary file unless it was renamed away.
struct TempFileGuard(PathBuf);

impl Drop for TempFileGuard {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.0);
    }
}

/// Publish split worksheets plus a durable CSV-range and worksheet index.
/// The index file is the completion marker.
pub fn export_bi_xlsx(
    package: &Path,
    dataset: &str,
    out: &Path,
    max_rows_per_sheet: i64,
    max_output_bytes: u64,
) -> Result<Value, DestinationError> {
    if !(1..EXCEL_MAX_ROWS).contains(&max_rows_per_sheet) {
        return Err(derr(format!(
            "max_rows_per_sheet must be 1..{}",
            EXCEL_MAX_ROWS - 1
        )));
    }
    if !(1024..=MAX_OUTPUT_BYTES).contains(&max_output_bytes) {
        return Err(derr("XLSX max_output_bytes is outside the supported bound"));
    }
    let (root, manifest) = manifest(package)?;
    let datasets = verify_partitions(&root, &manifest)?;
    let Some(info) = datasets.get(dataset) else {
        return Err(derr("dataset is not declared by the BI package"));
    };
    let total_rows = info.get("rows").and_then(Value::as_i64).unwrap_or(0);
    if (total_rows + max_rows_per_sheet - 1) / max_rows_per_sheet > MAX_XLSX_SHEETS as i64 {
        return Err(derr(
            "XLSX worksheet count exceeds the bound; retain the complete CSV package",
        ));
    }
    let destination = std::path::absolute(out).unwrap_or_else(|_| out.to_path_buf());
    let suffix = destination
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or_default();
    let index_path = destination.with_extension(format!("{suffix}.index.json"));
    if !suffix.eq_ignore_ascii_case("xlsx") {
        return Err(derr("out must end in .xlsx"));
    }
    let taken = |p: &Path| p.is_symlink() || p.exists();
    if taken(&destination)
        || taken(&index_path)
        || !destination
            .parent()
            .map(|p| p.is_dir() && !p.is_symlink())
            .unwrap_or(false)
    {
        return Err(derr(
            "out and its index must be new files under an existing non-symlink directory",
        ));
    }
    let source_manifest_sha256 = checksum_file(&root.join("manifest.json"))?;
    let parent = destination.parent().unwrap_or_else(|| Path::new("."));
    let mut sheet_count = 0i64;
    let mut rows_written = 0i64;
    let mut rows_in_sheet = 0i64;
    let mut header: Option<Vec<String>> = None;
    let mut ranges: Vec<Value> = Vec::new();
    // UTF-8 CSV bytes understate XML escaping, so a running cell-byte estimate
    // bounds the spool until the saved workbook can be measured exactly.
    let mut spool_estimate: u64 = 0;
    let mut sheet_open = false;
    let temporary = parent.join(format!(
        ".seohead-bi-xlsx-{}.tmp",
        uuid::Uuid::new_v4().simple()
    ));
    let index_temporary = parent.join(format!(
        ".seohead-bi-xlsx-index-{}.tmp",
        uuid::Uuid::new_v4().simple()
    ));
    // Both temporaries are removed on exit; after a successful rename the path
    // no longer exists, so the guard is a no-op.
    let _xlsx_guard = TempFileGuard(temporary.clone());
    let _index_guard = TempFileGuard(index_temporary.clone());
    let mut workbook = rust_xlsxwriter::Workbook::new();
    let check_spool = |estimate: u64| -> Result<(), DestinationError> {
        if estimate > max_output_bytes {
            return Err(derr(
                "XLSX worksheet spool exceeds max_output_bytes; retain complete CSV",
            ));
        }
        if free_bytes(parent).unwrap_or(u64::MAX) < MIN_FREE_DISK_BYTES {
            return Err(derr(
                "insufficient free disk for XLSX output; retain complete CSV",
            ));
        }
        Ok(())
    };
    let write_result = (|| -> Result<(), DestinationError> {
        for part in manifest["datasets"][dataset]
            .get("partitions")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default()
        {
            let mut current_range: Option<Map<String, Value>> = None;
            let path = root.join(part.get("path").and_then(Value::as_str).unwrap_or_default());
            let mut reader = csv::ReaderBuilder::new()
                .has_headers(false)
                .from_path(&path)
                .map_err(|_| derr("dataset partition is unreadable"))?;
            let mut records = reader.records();
            let current_header: Vec<String> = match records.next() {
                Some(Ok(record)) => record.iter().map(|s| s.to_owned()).collect(),
                _ => return Err(derr("dataset partition is unreadable")),
            };
            match &header {
                None => header = Some(current_header.clone()),
                Some(expected) if *expected != current_header => {
                    return Err(derr("dataset partition headers disagree"));
                }
                _ => {}
            }
            let mut part_row = 0i64;
            for record in records {
                part_row += 1;
                let record = record.map_err(|_| derr("dataset partition is unreadable"))?;
                let row: Vec<String> = record.iter().map(|s| s.to_owned()).collect();
                if row.iter().any(|v| v.len() > EXCEL_MAX_CELL_CHARS) {
                    return Err(derr(
                        "CSV cell exceeds Excel's 32767-character limit; retain complete CSV",
                    ));
                }
                if !sheet_open || rows_in_sheet >= max_rows_per_sheet {
                    sheet_count += 1;
                    let name = format!("{dataset}-{sheet_count:04}");
                    {
                        let ws = workbook
                            .add_worksheet_with_constant_memory()
                            .map_err(|e| derr(format!("XLSX spool failed: {e}")))?;
                        ws.set_name(&name)
                            .map_err(|e| derr(format!("XLSX worksheet failed: {e}")))?;
                        if let Some(h) = &header {
                            ws.write_row(0, 0, h.iter().map(String::as_str))
                                .map_err(|e| derr(format!("XLSX write failed: {e}")))?;
                            spool_estimate += h.iter().map(|v| v.len() as u64 + 8).sum::<u64>();
                        }
                    }
                    sheet_open = true;
                    rows_in_sheet = 0;
                    current_range = None;
                }
                if current_range.is_none() {
                    current_range = Some(Map::from_iter([
                        (
                            "partition".to_owned(),
                            part.get("path").cloned().unwrap_or(Value::Null),
                        ),
                        (
                            "partition_sha256".to_owned(),
                            part.get("sha256").cloned().unwrap_or(Value::Null),
                        ),
                        ("source_row_start".to_owned(), json!(part_row)),
                        ("source_row_end".to_owned(), json!(part_row)),
                        (
                            "worksheet".to_owned(),
                            json!(format!("{dataset}-{sheet_count:04}")),
                        ),
                        ("worksheet_row_start".to_owned(), json!(rows_in_sheet + 2)),
                        ("worksheet_row_end".to_owned(), json!(rows_in_sheet + 2)),
                    ]));
                    if ranges.len() >= super::schema::max_output_partitions() + MAX_XLSX_SHEETS {
                        return Err(derr(
                            "XLSX range index exceeds its bound; retain complete CSV",
                        ));
                    }
                    ranges.push(Value::Object(current_range.clone().unwrap_or_default()));
                } else if let Some(range) = &mut current_range {
                    range.insert("source_row_end".to_owned(), json!(part_row));
                    range.insert("worksheet_row_end".to_owned(), json!(rows_in_sheet + 2));
                    if let Some(last) = ranges.last_mut() {
                        *last = Value::Object(range.clone());
                    }
                }
                // Strings remain text; no spreadsheet formula is evaluated.
                let cells: Vec<String> = row
                    .iter()
                    .map(|v| spiderhead_core::tabular::neutralize_formula(v))
                    .collect();
                if let Some(ws) = workbook.worksheets_mut().last_mut() {
                    ws.write_row(
                        rows_in_sheet as u32 + 1,
                        0,
                        cells.iter().map(String::as_str),
                    )
                    .map_err(|e| derr(format!("XLSX write failed: {e}")))?;
                }
                spool_estimate += cells.iter().map(|v| v.len() as u64 + 8).sum::<u64>();
                rows_in_sheet += 1;
                rows_written += 1;
                if rows_written % 256 == 1 {
                    check_spool(spool_estimate)?;
                }
            }
        }
        if header.is_none() {
            return Err(derr("dataset has no CSV header"));
        }
        if rows_written != total_rows {
            return Err(derr("BI XLSX row conservation failed"));
        }
        if !sheet_open {
            sheet_count = 1;
            let ws = workbook.add_worksheet();
            ws.set_name(format!("{dataset}-0001"))
                .map_err(|e| derr(format!("XLSX worksheet failed: {e}")))?;
            if let Some(h) = &header {
                ws.write_row(0, 0, h.iter().map(String::as_str))
                    .map_err(|e| derr(format!("XLSX write failed: {e}")))?;
            }
        }
        check_spool(spool_estimate)?;
        workbook
            .save(&temporary)
            .map_err(|e| derr(format!("XLSX save failed: {e}")))?;
        let workbook_bytes = temporary.metadata().map(|m| m.len()).unwrap_or(0);
        if workbook_bytes > max_output_bytes {
            return Err(derr("XLSX exceeds max_output_bytes; retain complete CSV"));
        }
        if checksum_file(&root.join("manifest.json"))? != source_manifest_sha256
            || verify_partitions(&root, &manifest)? != datasets
        {
            return Err(derr("source package changed during XLSX export"));
        }
        let index = json!({
            "format": "spiderhead.bi-xlsx-index.v1",
            "state": "complete",
            "workbook": destination.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default(),
            "workbook_sha256": checksum_file(&temporary)?,
            "workbook_bytes": workbook_bytes,
            "dataset": dataset,
            "source_manifest_sha256": source_manifest_sha256,
            "source_schema_version": manifest.get("schema_version").cloned().unwrap_or(Value::Null),
            "conservation": manifest.get("conservation").cloned().filter(|c| !c.is_null()).unwrap_or_else(|| json!({
                "source_rows": rows_written,
                "selected_rows": rows_written,
                "omitted_rows": 0,
            })),
            "source_coverage": manifest.get("source_coverage").cloned().filter(|c| !c.is_null()).unwrap_or_else(|| json!({
                "state": manifest["datasets"][dataset].get("state").cloned().unwrap_or(Value::Null),
                "reason": manifest["datasets"][dataset].get("reason").cloned().unwrap_or(Value::Null),
                "coverage": manifest["datasets"][dataset].get("coverage").cloned().unwrap_or(Value::Null),
            })),
            "rows": rows_written,
            "worksheets": sheet_count,
            "header_rows_per_sheet": 1,
            "ranges": ranges,
            "max_output_bytes": max_output_bytes,
            "external_companion_refs": external_companion_refs(&manifest),
        });
        let encoded =
            crate::bi::writer::dump_manifest(&index).map_err(|exc| derr(exc.to_string()))?;
        if encoded.len() as u64 > MAX_MANIFEST_BYTES
            || workbook_bytes + encoded.len() as u64 > max_output_bytes
        {
            return Err(derr("XLSX with its index exceeds the output byte bound"));
        }
        {
            let mut stream = fs::File::create(&index_temporary)
                .map_err(|e| derr(format!("XLSX index write failed: {e}")))?;
            stream
                .write_all(&encoded)
                .and_then(|_| stream.sync_all())
                .map_err(|e| derr(format!("XLSX index write failed: {e}")))?;
        }
        chmod_private(&temporary);
        chmod_private(&index_temporary);
        fs::rename(&temporary, &destination)
            .map_err(|e| derr(format!("XLSX publication failed: {e}")))?;
        let published_destination = destination.clone();
        // From here on, a failure must not leave the published workbook behind.
        if let Err(exc) = (|| -> Result<(), DestinationError> {
            fs::rename(&index_temporary, &index_path)
                .map_err(|e| derr(format!("XLSX index publication failed: {e}")))?;
            Ok(())
        })() {
            let _ = fs::remove_file(&published_destination);
            let _ = fs::remove_file(&index_temporary);
            return Err(exc);
        }
        Ok(())
    })();
    write_result?;
    Ok(json!({
        "format": "spiderhead.bi-xlsx.v1",
        "dataset": dataset,
        "rows": rows_written,
        "worksheets": sheet_count,
        "max_rows_per_sheet": max_rows_per_sheet,
        "output": destination.to_string_lossy(),
        "source_schema_version": manifest.get("schema_version").cloned().unwrap_or(Value::Null),
        "index": index_path.to_string_lossy(),
        "external_companion_refs": external_companion_refs(&manifest),
    }))
}
