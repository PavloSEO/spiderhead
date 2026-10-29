//! Versioned `scan_export.v1` scan-data export (CSV, XLSX, JSON, XML).
//!
//! `report-build` renders a human-facing report from an audit document; this module exports the
//! retained scan data itself (page, link and finding records plus run statistics, provenance and
//! explicit coverage) under a stable, field-selectable data contract. It accepts a validated
//! scan artifact or an Export Analyzer `audit.json` document, and refuses to leave a plausible
//! partial artifact behind when serialization or publication fails.
//!
//! The envelope: `format`, `provenance`, `projection`, `statistics`, `coverage`, then `records`.
//! `None` stays `null` in JSON and becomes `state="absent"` in XML. CSV and XLSX use reversible
//! text escapes for absent, empty and formula-leading values.

pub mod fields;
pub mod serialize;
pub mod source;
pub mod xlsx;

use spiderhead_core::filesystem::{file_identity, PrivateMode};
use std::fs::OpenOptions;
use std::io::Write;
use std::path::{Path, PathBuf};

use indexmap::IndexMap;
use rusqlite::Connection;
use serde_json::{json, Map, Value};

use crate::audit_v2::{audit_v2_path, AuditV2Reader};
use crate::error::StoreError;
use crate::exports::{unlink_owned, Owned};
use crate::inputs::is_sqlite_input;
use crate::link_context::cell_value;
use crate::scan_artifact::{loads, open_scan, ScanError};
pub use fields::{FINDING_FIELDS, LINK_FIELDS, PAGE_FIELDS_DOCUMENT, PAGE_FIELDS_SCAN};
use serialize::{csv_chunks, json_chunks, manifest_rows, xml_chunks, Sink};
pub use source::*;
pub use xlsx::write_xlsx;

/// Format tag of the export envelope.
pub const EXPORT_FORMAT_VERSION: &str = "scan_export.v1";
/// XML namespace of the XML serialization.
pub const XML_NAMESPACE: &str = "https://github.com/PavloSEO/seotools/schema/scan_export.v1";
/// Output formats.
pub const FORMATS: &[&str] = &["csv", "xlsx", "json", "xml"];
/// Exportable record types.
pub const RECORD_TYPES: &[&str] = &["pages", "links", "findings"];
/// Record type to its XML element name.
pub const RECORD_ELEMENTS: &[(&str, &str)] = &[
    ("pages", "page"),
    ("links", "link"),
    ("findings", "finding"),
];
/// Record type to its XLSX sheet label.
pub const RECORD_LABELS: &[(&str, &str)] = &[
    ("pages", "Pages"),
    ("links", "Links"),
    ("findings", "Findings"),
];
/// Excel limit: rows per sheet.
pub const EXCEL_MAX_ROWS: i64 = 1_048_576;
/// Excel limit: columns per sheet.
pub const EXCEL_MAX_COLUMNS: i64 = 16_384;
/// Excel limit: characters per cell.
pub const EXCEL_MAX_CELL_TEXT: i64 = 32_767;

/// One exported record.
pub type Record = Map<String, Value>;
/// Fallible record stream of one record type.
pub type RecordStream<'a> = Box<dyn Iterator<Item = Result<Record, StoreError>> + 'a>;
/// Writer of one output file: receives the temporary target and the owned-file ledger.
pub type Writer<'a> = Box<dyn FnOnce(&Path, &mut Owned) -> Result<(), StoreError> + 'a>;

/// One record type queued for writing.
pub struct Selection<'a> {
    /// Record type name.
    pub name: String,
    /// Projected field names, in output order.
    pub fields: Vec<String>,
    /// The rows.
    pub stream: RecordStream<'a>,
}

fn scan_error(message: impl Into<String>) -> StoreError {
    ScanError {
        message: message.into(),
    }
    .into()
}

fn py_repr(value: &Value) -> String {
    match value {
        Value::String(s) => format!("'{s}'"),
        Value::Null => "None".into(),
        Value::Bool(true) => "True".into(),
        Value::Bool(false) => "False".into(),
        other => other.to_string(),
    }
}

fn split_names(text: &str) -> Vec<String> {
    text.split(',')
        .map(str::trim)
        .filter(|p| !p.is_empty())
        .map(str::to_string)
        .collect()
}

/// Resolve the requested record types (default: every available type).
pub fn normalize_records(records: &Value, source: &Source) -> Result<Vec<String>, StoreError> {
    let state_of = |name: &str| -> String {
        source.record_states[name]
            .get("state")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string()
    };
    let requested: Vec<Value> = match records {
        Value::Null => {
            let available: Vec<String> = RECORD_TYPES
                .iter()
                .filter(|n| state_of(n) != "unavailable")
                .map(|s| s.to_string())
                .collect();
            if available.is_empty() {
                return Err(scan_error("input exposes no exportable record types"));
            }
            return Ok(available);
        }
        Value::String(s) => split_names(s).into_iter().map(Value::String).collect(),
        Value::Array(items) => items.clone(),
        _ => return Err(scan_error("records must be a list of record type names")),
    };
    let unknown: Vec<String> = requested
        .iter()
        .filter(|i| !i.as_str().is_some_and(|s| RECORD_TYPES.contains(&s)))
        .map(|i| i.as_str().map_or_else(|| i.to_string(), str::to_string))
        .collect();
    if !unknown.is_empty() {
        return Err(scan_error(format!(
            "unknown record type(s): {}; supported: {}",
            unknown.join(", "),
            RECORD_TYPES.join(", ")
        )));
    }
    let mut selected: Vec<String> = Vec::new();
    for item in &requested {
        let s = item.as_str().unwrap_or("").to_string();
        if !selected.contains(&s) {
            selected.push(s);
        }
    }
    if selected.is_empty() {
        return Err(scan_error("record selection is empty"));
    }
    let unavailable: Vec<String> = selected
        .iter()
        .filter(|n| state_of(n) == "unavailable")
        .map(|n| {
            format!(
                "{n} ({})",
                text_of_value(&source.record_states[n.as_str()], "reason")
            )
        })
        .collect();
    if !unavailable.is_empty() {
        return Err(scan_error(format!(
            "record type(s) unavailable in this input: {}",
            unavailable.join(", ")
        )));
    }
    Ok(RECORD_TYPES
        .iter()
        .filter(|n| selected.iter().any(|s| s == *n))
        .map(|s| s.to_string())
        .collect())
}

fn text_of_value(value: &Value, key: &str) -> String {
    value
        .get(key)
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_string()
}

/// Resolve the per-type field selection into the projection actually emitted.
pub fn normalize_fields(
    fields: &Value,
    source: &Source,
    selected: &[String],
) -> Result<IndexMap<String, Vec<String>>, StoreError> {
    let items: Vec<(String, Value)> = match fields {
        Value::Null => Vec::new(),
        Value::Object(map) => map.iter().map(|(k, v)| (k.clone(), v.clone())).collect(),
        Value::Array(specs) => {
            let mut parsed: IndexMap<String, Vec<String>> = IndexMap::new();
            for spec in specs {
                let Some((name, value)) = spec.as_str().and_then(|s| s.split_once('=')) else {
                    return Err(scan_error(format!(
                        "invalid field selection {}; expected TYPE=field,field",
                        py_repr(spec)
                    )));
                };
                parsed
                    .entry(name.trim().to_string())
                    .or_default()
                    .extend(split_names(value));
            }
            parsed
                .into_iter()
                .map(|(k, v)| (k, Value::Array(v.into_iter().map(Value::String).collect())))
                .collect()
        }
        _ => {
            return Err(scan_error(
                "fields must be a mapping of record type to field list",
            ))
        }
    };
    for (name, _) in &items {
        if !RECORD_TYPES.contains(&name.as_str()) {
            return Err(scan_error(format!(
                "unknown record type {} in field selection; supported: {}",
                py_repr(&json!(name)),
                RECORD_TYPES.join(", ")
            )));
        }
    }
    let mut projection: IndexMap<String, Vec<String>> = selected
        .iter()
        .map(|n| (n.clone(), source.catalogs[n.as_str()].clone()))
        .collect();
    for (name, names) in items {
        if !selected.contains(&name) {
            return Err(scan_error(format!(
                "field selection names record type {}, which is not selected for export",
                py_repr(&json!(name))
            )));
        }
        let list: Vec<Value> = match &names {
            Value::String(s) => split_names(s).into_iter().map(Value::String).collect(),
            Value::Array(items) => items.clone(),
            _ => Vec::new(),
        };
        if list.is_empty() {
            return Err(scan_error(format!(
                "field selection for {} is empty",
                py_repr(&json!(name))
            )));
        }
        let catalog = &source.catalogs[name.as_str()];
        let unknown: Vec<String> = list
            .iter()
            .map(|i| i.as_str().map_or_else(|| i.to_string(), str::to_string))
            .filter(|i| !catalog.contains(i))
            .collect();
        if !unknown.is_empty() {
            let others: Vec<&String> = source
                .catalogs
                .iter()
                .filter(|(other, _)| **other != name)
                .flat_map(|(_, v)| v.iter())
                .collect();
            let misplaced: Vec<&String> = unknown.iter().filter(|i| others.contains(i)).collect();
            let unsupported: Vec<&String> =
                unknown.iter().filter(|i| !others.contains(i)).collect();
            let mut details = Vec::new();
            let join = |v: &[&String]| v.iter().map(|s| s.as_str()).collect::<Vec<_>>().join(", ");
            if !misplaced.is_empty() {
                details.push(format!(
                    "field(s) invalid for {}: {}",
                    py_repr(&json!(name)),
                    join(&misplaced)
                ));
            }
            if !unsupported.is_empty() {
                details.push(format!(
                    "unknown field(s): {} for {}",
                    join(&unsupported),
                    py_repr(&json!(name))
                ));
            }
            return Err(scan_error(format!(
                "{}; supported: {}",
                details.join("; "),
                catalog.join(", ")
            )));
        }
        let mut unique: Vec<String> = Vec::new();
        for i in list {
            let s = i.as_str().unwrap_or("").to_string();
            if !unique.contains(&s) {
                unique.push(s);
            }
        }
        projection.insert(name, unique);
    }
    Ok(projection)
}

/// The versioned envelope (everything except `records`).
pub fn envelope(
    source: &Source,
    selected: &[String],
    projection: &IndexMap<String, Vec<String>>,
) -> Value {
    let count = |n: &str| source.counts[n].map_or(Value::Null, |c| json!(c));
    let mut proj = Map::new();
    let mut rows = Map::new();
    for n in selected {
        proj.insert(n.clone(), json!(projection[n.as_str()]));
        rows.insert(n.clone(), count(n));
    }
    let mut records = Map::new();
    for n in RECORD_TYPES {
        let mut entry = source.record_states[*n]
            .as_object()
            .cloned()
            .unwrap_or_default();
        entry.insert("exported".into(), json!(selected.iter().any(|s| s == n)));
        entry.insert("source_rows".into(), count(n));
        records.insert(n.to_string(), Value::Object(entry));
    }
    json!({
        "format": EXPORT_FORMAT_VERSION,
        "provenance": source.provenance,
        "projection": proj,
        "statistics": {"record_types": selected, "rows": rows, "run": source.run_statistics},
        "coverage": {
            "records": records,
            "checks": source.checks,
            "capabilities": source.capabilities,
            "limitations": source.limitations,
        },
    })
}

/// Refuse a projection wider than an Excel sheet.
pub fn check_excel_limits(
    selected: &[String],
    projection: &IndexMap<String, Vec<String>>,
) -> Result<(), StoreError> {
    for name in selected {
        let n = projection[name.as_str()].len() as i64;
        if n > EXCEL_MAX_COLUMNS {
            return Err(scan_error(format!(
                "{name}: {n} fields exceed the Excel column limit ({EXCEL_MAX_COLUMNS})"
            )));
        }
    }
    Ok(())
}

/// Output files by name: one for json/xml/xlsx, per-type files plus a manifest for CSV.
pub fn output_files(out: &Path, fmt: &str, selected: &[String]) -> IndexMap<String, PathBuf> {
    if fmt != "csv" {
        return [(fmt.to_string(), out.to_path_buf())].into_iter().collect();
    }
    let name = out
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    let base = name
        .strip_suffix(".csv")
        .filter(|_| out.extension().is_some_and(|e| e == "csv"))
        .unwrap_or(&name)
        .to_string();
    let parent = out.parent().map(Path::to_path_buf).unwrap_or_default();
    let mut files: IndexMap<String, PathBuf> = selected
        .iter()
        .map(|n| (n.clone(), parent.join(format!("{base}.{n}.csv"))))
        .collect();
    files.insert(
        "manifest".into(),
        parent.join(format!("{base}.manifest.csv")),
    );
    files
}

/// Create `path` exclusively (mode 0600), record its identity, run `fill`, fsync.
pub fn write_stream(
    path: &Path,
    owned: &mut Owned,
    fill: impl FnOnce(Sink<'_>) -> Result<(), StoreError>,
) -> Result<(), StoreError> {
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .private_mode(0o600)
        .open(path)?;
    let state = file_identity(&file)?;
    owned.insert(
        path.to_string_lossy().into_owned(),
        (state.0 as i64, state.1 as i64),
    );
    let mut sink = |chunk: &[u8]| -> Result<(), StoreError> { Ok(file.write_all(chunk)?) };
    fill(&mut sink)?;
    file.flush()?;
    file.sync_all()?;
    Ok(())
}

/// Write one record type as CSV.
pub fn write_csv_bytes(
    target: &Path,
    fields: &[String],
    stream: RecordStream<'_>,
    owned: &mut Owned,
) -> Result<(), StoreError> {
    write_stream(target, owned, |sink| csv_chunks(fields, stream, sink))
}

fn fsync_directory(path: &Path) -> Result<(), StoreError> {
    Ok(std::fs::File::open(path)?.sync_all()?)
}

/// Write every target via a sibling temp file, then link it into place.
///
/// Publication never overwrites an existing destination and tracks each owned inode so a failure
/// removes exactly what this run created.
pub fn publish_files(
    files: &IndexMap<String, PathBuf>,
    writers: Vec<(String, Writer<'_>)>,
) -> Result<(), StoreError> {
    let mut owned = Owned::new();
    let outcome = publish_inner(files, writers, &mut owned);
    if outcome.is_err() {
        let paths: Vec<String> = owned.keys().cloned().collect();
        for path in paths.iter().rev() {
            let _ = unlink_owned(Path::new(path), &owned);
        }
    }
    outcome
}

fn publish_inner(
    files: &IndexMap<String, PathBuf>,
    writers: Vec<(String, Writer<'_>)>,
    owned: &mut Owned,
) -> Result<(), StoreError> {
    let mut writers: IndexMap<String, Writer<'_>> = writers.into_iter().collect();
    for (name, target) in files {
        let temporary = target.with_file_name(format!(
            ".{}.tmp",
            target
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_default()
        ));
        let writer = writers
            .shift_remove(name)
            .ok_or_else(|| scan_error(format!("no writer for {name}")))?;
        writer(&temporary, owned)?;
        std::fs::hard_link(&temporary, target)?;
        let key = temporary.to_string_lossy().into_owned();
        if let Some(identity) = owned.get(&key).copied() {
            owned.insert(target.to_string_lossy().into_owned(), identity);
        }
        unlink_owned(&temporary, owned)?;
    }
    if let Some(first) = files.values().next() {
        fsync_directory(first.parent().unwrap_or(Path::new(".")))?;
    }
    Ok(())
}

/// Export retained scan data under the `scan_export.v1` contract.
///
/// `records` selects record types (default: every type the input makes available). `fields` is a
/// `{record_type: [field, ...]}` mapping or repeatable `TYPE=f1,f2` specs; unknown names are
/// refused before any output file is created. Returns the shared `{"ok": ...}` result shape.
/// Defaults: `fmt` is `json`; `records` and `fields` select everything.
pub fn export_scan_data(
    input_path: &Value,
    out: Option<&Path>,
    fmt: &str,
    records: &Value,
    fields: &Value,
) -> Map<String, Value> {
    match export_inner(input_path, out, fmt, records, fields) {
        Ok(v) => v,
        Err(StoreError::ScanError(e)) => json!({"ok": false, "error": e.message})
            .as_object()
            .cloned()
            .unwrap_or_default(),
        Err(e) => json!({"ok": false, "error": format!("cannot export scan data: {e}")})
            .as_object()
            .cloned()
            .unwrap_or_default(),
    }
}

fn export_inner(
    input_path: &Value,
    out: Option<&Path>,
    fmt: &str,
    records: &Value,
    fields: &Value,
) -> Result<Map<String, Value>, StoreError> {
    if !FORMATS.contains(&fmt) {
        return Err(scan_error(format!(
            "unknown export format '{fmt}'; supported: {}",
            FORMATS.join(", ")
        )));
    }
    let Some(destination) = out else {
        return Err(scan_error("--out is required"));
    };
    let source = open_source(input_path)?;
    let selected = normalize_records(records, &source)?;
    let projection = normalize_fields(fields, &source, &selected)?;
    if fmt == "xlsx" {
        check_excel_limits(&selected, &projection)?;
    }
    let head = envelope(&source, &selected, &projection);
    let files = output_files(destination, fmt, &selected);
    let parent = destination
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or(Path::new("."));
    if !parent.is_dir() {
        return Err(scan_error(format!(
            "output directory does not exist: {}",
            parent.display()
        )));
    }
    if let Some(conflict) = files
        .values()
        .find(|t| std::fs::symlink_metadata(t).is_ok())
    {
        return Err(scan_error(format!(
            "output already exists: {}",
            conflict.display()
        )));
    }
    fn build<'a>(
        source: &'a Source,
        selected: &'a [String],
        projection: &'a IndexMap<String, Vec<String>>,
    ) -> Result<Vec<Selection<'a>>, StoreError> {
        selected
            .iter()
            .map(|name| {
                Ok(Selection {
                    name: name.clone(),
                    fields: projection[name.as_str()].clone(),
                    stream: record_stream(source, name, &projection[name.as_str()])?,
                })
            })
            .collect()
    }
    let mut writers: Vec<(String, Writer<'_>)> = Vec::new();
    match fmt {
        "json" => writers.push((
            "json".into(),
            Box::new(|t, o| {
                let selection = build(&source, &selected, &projection)?;
                write_stream(t, o, |sink| json_chunks(&head, selection, sink))
            }),
        )),
        "xml" => writers.push((
            "xml".into(),
            Box::new(|t, o| {
                let selection = build(&source, &selected, &projection)?;
                write_stream(t, o, |sink| xml_chunks(&head, selection, sink))
            }),
        )),
        "xlsx" => writers.push((
            "xlsx".into(),
            Box::new(|t, o| {
                let selection = build(&source, &selected, &projection)?;
                write_xlsx(t, &head, selection, o)
            }),
        )),
        _ => {
            let src = &source;
            for name in &selected {
                let f = &projection[name.as_str()];
                writers.push((
                    name.clone(),
                    Box::new(move |t, o| write_csv_bytes(t, f, record_stream(src, name, f)?, o)),
                ));
            }
            let manifest_files = files.clone();
            let manifest_head = head.clone();
            writers.push((
                "manifest".into(),
                Box::new(move |t, o| {
                    let bytes = manifest_rows(&manifest_head, &manifest_files)?;
                    write_stream(t, o, |sink| sink(&bytes))
                }),
            ));
        }
    }
    publish_files(&files, writers)?;
    let counts: Map<String, Value> = selected
        .iter()
        .map(|n| {
            (
                n.clone(),
                source.counts[n.as_str()].map_or(Value::Null, |c| json!(c)),
            )
        })
        .collect();
    Ok(json!({
        "ok": true,
        "format": EXPORT_FORMAT_VERSION,
        "fmt": fmt,
        "files": files.values().map(|t| t.to_string_lossy().into_owned()).collect::<Vec<_>>(),
        "counts": counts,
    })
    .as_object()
    .cloned()
    .unwrap_or_default())
}
