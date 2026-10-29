//! Offline publication and branded-search cohort projections.
//!
//! Projections consume saved, normalized evidence and emit small CSV/JSON packages.
//! They never collect provider responses or turn an absent query into a zero.
//!

use std::collections::BTreeMap;
use std::collections::BTreeSet;
use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};

use serde_json::{json, Map, Value};
use thiserror::Error;
use unicode_casefold::UnicodeCaseFold;
use unicode_normalization::UnicodeNormalization;

use spiderhead_core::formats::matches;
use spiderhead_core::tabular::neutralize_formula;

use crate::bi::schema::NORMALIZED_FORMAT;
use crate::join::{normalize_join_key, IsoDate};
use crate::py;

pub const PUBLICATION_FORMAT: &str = "spiderhead.publication-cohort-input.v1";
pub const GSC_PROGRESS_FORMAT: &str = "spiderhead.gsc-progress-input.v1";
pub const PUBLICATION_RESULT_FORMAT: &str = "spiderhead.publication-cohort-result.v1";
pub const GSC_PROGRESS_RESULT_FORMAT: &str = "spiderhead.gsc-progress-result.v1";
const MAX_INPUT_BYTES: u64 = 16 * 1024 * 1024;
const MAX_ROWS: usize = 100_000;

/// A saved evidence package cannot support the requested projection.
#[derive(Debug, Error)]
#[error("{0}")]
pub struct CohortProjectionError(pub String);

impl CohortProjectionError {
    fn new(message: impl Into<String>) -> Self {
        CohortProjectionError(message.into())
    }
}

type Res<T> = Result<T, CohortProjectionError>;

/// Canonical compact JSON: sorted keys, no whitespace.
fn canonical(value: &Value) -> String {
    spiderhead_store::audit_v2::canonical(value).unwrap_or_default()
}

fn read_input(document: Option<&Value>, file: Option<&str>, expected: &str) -> Res<Value> {
    if document.is_none() == file.is_none() {
        return Err(CohortProjectionError::new(
            "supply exactly one of document or file",
        ));
    }
    let document = if let Some(file) = file {
        let path = Path::new(file);
        let meta = fs::symlink_metadata(path)
            .map_err(|_| CohortProjectionError::new("file must be a bounded regular JSON file"))?;
        if meta.file_type().is_symlink() || !meta.is_file() || meta.len() > MAX_INPUT_BYTES {
            return Err(CohortProjectionError::new(
                "file must be a bounded regular JSON file",
            ));
        }
        let raw = fs::read_to_string(path)
            .map_err(|e| CohortProjectionError::new(format!("cannot read JSON input: {e}")))?;
        let text = raw.strip_prefix('\u{feff}').unwrap_or(&raw);
        serde_json::from_str(text)
            .map_err(|e| CohortProjectionError::new(format!("cannot read JSON input: {e}")))?
    } else {
        document.cloned().unwrap_or(Value::Null)
    };
    if !document.is_object() || !matches(document.get("format").and_then(Value::as_str), expected) {
        return Err(CohortProjectionError::new(format!(
            "document must use {expected}"
        )));
    }
    Ok(document)
}

fn date_entry(value: Option<&Value>, name: &str) -> Res<Value> {
    // A missing key and an explicit null both mean no date.
    let Some(value) = value.filter(|v| !v.is_null()) else {
        return Ok(json!({"raw": null, "value": null, "state": "unknown", "source": null}));
    };
    let map = match value.as_object() {
        Some(m)
            if m.keys()
                .all(|k| matches!(k.as_str(), "raw" | "value" | "state" | "source" | "reason")) =>
        {
            m
        }
        _ => {
            return Err(CohortProjectionError::new(format!(
                "{name} must be a date evidence object"
            )))
        }
    };
    let state = value
        .get("state")
        .and_then(Value::as_str)
        .unwrap_or("measured");
    if !matches!(
        state,
        "measured" | "unknown" | "ambiguous" | "conflicting" | "missing"
    ) {
        return Err(CohortProjectionError::new(format!(
            "{name}.state is unsupported"
        )));
    }
    let parsed = map.get("value");
    if let Some(parsed) = parsed {
        if !parsed.is_null() {
            let text = parsed.as_str().ok_or_else(|| {
                CohortProjectionError::new(format!("{name}.value must be an ISO calendar date"))
            })?;
            if IsoDate::parse(text).is_none() {
                return Err(CohortProjectionError::new(format!(
                    "{name}.value must be an ISO calendar date"
                )));
            }
        }
    }
    if state == "measured" && parsed.is_none_or(Value::is_null) {
        return Err(CohortProjectionError::new(format!(
            "{name} cannot be measured without value"
        )));
    }
    let mut out = Map::new();
    out.insert(
        "raw".to_owned(),
        map.get("raw").cloned().unwrap_or(Value::Null),
    );
    out.insert("value".to_owned(), parsed.cloned().unwrap_or(Value::Null));
    out.insert("state".to_owned(), Value::String(state.to_owned()));
    out.insert(
        "source".to_owned(),
        map.get("source").cloned().unwrap_or(Value::Null),
    );
    out.insert(
        "reason".to_owned(),
        map.get("reason").cloned().unwrap_or(Value::Null),
    );
    Ok(Value::Object(out))
}

/// Returns (value, state, reason) for a metric; the reason is the raw recorded value.
fn metric(entry: Option<&Value>, name: &str) -> Res<(Option<f64>, String, Option<Value>)> {
    let Some(entry) = entry else {
        return Ok((
            None,
            "unknown".to_owned(),
            Some(json!("metric was not supplied")),
        ));
    };
    if !entry.is_object() {
        return Ok((
            None,
            "unknown".to_owned(),
            Some(json!("metric was not supplied")),
        ));
    }
    let state = entry
        .get("state")
        .and_then(Value::as_str)
        .unwrap_or("unknown")
        .to_owned();
    let value = entry.get("value");
    if state != "measured" {
        let reason = entry
            .get("reason")
            .filter(|r| py::truthy(Some(r)))
            .cloned()
            .unwrap_or_else(|| json!("metric is not measured"));
        return Ok((None, state, Some(reason)));
    }
    let numeric = match value {
        Some(Value::Number(n)) => Some(n.as_f64().unwrap_or_default()),
        _ => None,
    };
    match numeric {
        Some(v) => Ok((Some(v), state, entry.get("reason").cloned())),
        None => Err(CohortProjectionError::new(format!(
            "{name} measured value must be numeric"
        ))),
    }
}

fn normalized_documents(value: Option<&Value>) -> Res<Vec<Value>> {
    let documents: Vec<Value> = match value {
        Some(Value::Array(items)) => items.clone(),
        other => vec![other.cloned().unwrap_or(Value::Null)],
    };
    if documents.is_empty() || documents.len() > 32 {
        return Err(CohortProjectionError::new(
            "evidence must contain 1 to 32 normalized documents",
        ));
    }
    for item in &documents {
        if !item.is_object()
            || !matches(
                item.get("format").and_then(Value::as_str),
                NORMALIZED_FORMAT,
            )
        {
            return Err(CohortProjectionError::new(format!(
                "evidence entries must use {NORMALIZED_FORMAT}"
            )));
        }
    }
    Ok(documents)
}

fn source_of(document: &Value, ordinal: usize) -> Value {
    let mapping = py::get(document, "mapping").cloned().unwrap_or(Value::Null);
    let source = py::get(&mapping, "source").cloned().unwrap_or(Value::Null);
    let collection = py::get(&mapping, "collection")
        .cloned()
        .unwrap_or(Value::Null);
    let period = py::get(&mapping, "period")
        .filter(|v| v.is_object())
        .cloned()
        .unwrap_or_else(|| json!({}));
    json!({
        "source_id": format!("evidence-{ordinal}"),
        "provider": source.get("provider").filter(|v| py::truthy(Some(v))).cloned().unwrap_or_else(|| json!("unknown")),
        "operation": source.get("operation").cloned().unwrap_or(Value::Null),
        "reporting_identity": source.get("reporting_identity").cloned().unwrap_or(Value::Null),
        "timezone": source.get("timezone").cloned().unwrap_or(Value::Null),
        "attribution": source.get("attribution").cloned().unwrap_or(Value::Null),
        "search_type": source.get("search_type").cloned().unwrap_or(Value::Null),
        "period_start": period.get("start_date").cloned().unwrap_or(Value::Null),
        "period_end": period.get("end_date").cloned().unwrap_or(Value::Null),
        "collection": collection,
        "dimensions": py::get(&mapping, "dimensions")
            .filter(|v| py::truthy(Some(v)))
            .cloned()
            .unwrap_or_else(|| json!([])),
    })
}

/// Stage the CSV files and manifest, then publish the directory atomically.
fn atomic_package(
    out_dir: Option<&str>,
    files: &[(String, Vec<Map<String, Value>>)],
    mut manifest: Value,
) -> Res<Value> {
    let Some(out_dir) = out_dir else {
        return Err(CohortProjectionError::new("out_dir is required"));
    };
    let destination = PathBuf::from(out_dir);
    if destination.exists()
        || destination
            .symlink_metadata()
            .is_ok_and(|m| m.file_type().is_symlink())
    {
        return Err(CohortProjectionError::new("out_dir must not already exist"));
    }
    let parent = destination.parent().unwrap_or_else(|| Path::new("."));
    if !parent.is_dir() {
        return Err(CohortProjectionError::new(
            "out_dir parent must be an existing regular directory",
        ));
    }
    let stage = Stage::new(parent, ".seohead-cohort-")?;
    let mut output = Map::new();
    for (name, rows) in files {
        let mut columns: Vec<String> = rows
            .iter()
            .flat_map(|row| row.keys().cloned())
            .collect::<BTreeSet<_>>()
            .into_iter()
            .collect();
        columns.sort();
        let path = stage.dir().join(name);
        if columns.is_empty() {
            // Without columns every line is a bare terminator; the csv writer would quote an empty field.
            fs::write(&path, "\r\n".repeat(rows.len() + 1)).map_err(io_err)?;
        } else {
            let mut writer = csv::WriterBuilder::new()
                .terminator(csv::Terminator::CRLF)
                .from_path(&path)
                .map_err(io_err)?;
            writer.write_record(columns.iter()).map_err(io_err)?;
            for row in rows {
                let record: Vec<String> = columns
                    .iter()
                    .map(|key| {
                        let value = row.get(key).unwrap_or(&Value::Null);
                        match value {
                            Value::Object(_) | Value::Array(_) => {
                                neutralize_formula(&canonical(value))
                            }
                            Value::String(text) => neutralize_formula(text),
                            other => py::str_value(other),
                        }
                    })
                    .collect();
                writer.write_record(record.iter()).map_err(io_err)?;
            }
        }
        let raw = fs::read(&path).map_err(io_err)?;
        output.insert(
            name.clone(),
            json!({
                "rows": rows.len(),
                "bytes": raw.len(),
                "sha256": crate::bi::schema::digest(&raw),
            }),
        );
    }
    manifest
        .as_object_mut()
        .expect("manifest object")
        .insert("files".to_owned(), Value::Object(output.clone()));
    let payload = format!("{}\n", json_sorted_pretty(&manifest));
    fs::write(stage.dir().join("manifest.json"), payload).map_err(io_err)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = fs::set_permissions(stage.dir(), fs::Permissions::from_mode(0o700));
    }
    fs::rename(stage.dir(), &destination).map_err(io_err)?;
    stage.keep();
    Ok(json!({
        "output_directory": destination.to_string_lossy(),
        "manifest": destination.join("manifest.json").to_string_lossy(),
        "files": output,
    }))
}

fn io_err(error: impl std::fmt::Display) -> CohortProjectionError {
    CohortProjectionError(error.to_string())
}

/// Manifest JSON: sorted keys, two-space indent.
fn json_sorted_pretty(value: &Value) -> String {
    fn sort(value: &Value) -> Value {
        match value {
            Value::Object(map) => {
                let mut sorted = Map::new();
                for key in {
                    let mut keys: Vec<&String> = map.keys().collect();
                    keys.sort();
                    keys
                } {
                    sorted.insert(key.clone(), sort(&map[key]));
                }
                Value::Object(sorted)
            }
            Value::Array(items) => Value::Array(items.iter().map(sort).collect()),
            other => other.clone(),
        }
    }
    serde_json::to_string_pretty(&sort(value)).unwrap_or_default()
}

/// Guard for a temporary staging directory.
pub(crate) struct Stage(PathBuf);

impl Stage {
    pub(crate) fn new(parent: &Path, prefix: &str) -> Res<Stage> {
        for attempt in 0..100u32 {
            let dir = parent.join(format!("{prefix}{}-{}", std::process::id(), attempt));
            match fs::create_dir(&dir) {
                Ok(()) => return Ok(Stage(dir)),
                Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => continue,
                Err(e) => return Err(io_err(e)),
            }
        }
        Err(CohortProjectionError::new(
            "could not create staging directory",
        ))
    }

    pub(crate) fn dir(&self) -> &Path {
        &self.0
    }

    /// Keep the staged directory (already published by `fs::rename`).
    pub(crate) fn keep(self) {
        std::mem::forget(self);
    }
}

impl Drop for Stage {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn publication_contents(document: &Value) -> Res<Vec<Value>> {
    let content = document.get("content").cloned().unwrap_or(Value::Null);
    let content = content
        .as_array()
        .ok_or_else(|| CohortProjectionError::new("content must be a bounded list"))?;
    if content.len() > MAX_ROWS {
        return Err(CohortProjectionError::new("content must be a bounded list"));
    }
    let mut rows = Vec::new();
    for (ordinal, item) in content.iter().enumerate() {
        let url = item
            .as_object()
            .and_then(|_| item.get("url"))
            .and_then(Value::as_str)
            .map(str::to_owned);
        let Some(url) = url else {
            return Err(CohortProjectionError::new("every content row needs a URL"));
        };
        let publication = date_entry(item.get("publication"), "publication")?;
        let modified = date_entry(item.get("modified"), "modified")?;
        let first_seen = date_entry(item.get("first_observed"), "first_observed")?;
        let (authors, author_state) = match item.get("authors") {
            None | Some(Value::Null) => (Vec::new(), "missing"),
            Some(Value::Array(list))
                if list
                    .iter()
                    .all(|a| a.as_str().is_some_and(|s| !s.is_empty())) =>
            {
                (list.clone(), "measured")
            }
            _ => {
                return Err(CohortProjectionError::new(
                    "authors must be a list of non-empty strings when supplied",
                ))
            }
        };
        let (word_count, word_state, word_reason) = metric(item.get("word_count"), "word_count")?;
        if word_count.is_some_and(|v| v < 0.0) {
            return Err(CohortProjectionError::new("word_count cannot be negative"));
        }
        let key = normalize_join_key(Some(&url));
        rows.push(json!({
            "content_id": format!("content-{ordinal}"),
            "url": url,
            "url_key": key.clone().map(Value::from).unwrap_or(Value::Null),
            "url_key_state": if key.is_some() { "keyed" } else { "unkeyable" },
            "publication": publication,
            "modified": modified,
            "first_observed": first_seen,
            "authors": authors,
            "author_state": author_state,
            "word_count": word_count,
            "word_count_state": word_state,
            "word_count_reason": word_reason,
        }));
    }
    Ok(rows)
}

fn age_windows(document: &Value) -> Res<(Vec<Value>, Option<IsoDate>)> {
    let value = document
        .get("age_windows")
        .cloned()
        .unwrap_or_else(|| json!([]));
    let items = value
        .as_array()
        .ok_or_else(|| CohortProjectionError::new("age_windows must be a list when supplied"))?;
    let mut windows = Vec::new();
    for item in items {
        let keys: BTreeSet<&String> = item
            .as_object()
            .map(|m| m.keys().collect())
            .unwrap_or_default();
        let wanted: BTreeSet<&str> = ["id", "start_day", "end_day"].into_iter().collect();
        if !item.is_object() || keys.iter().map(|k| k.as_str()).collect::<BTreeSet<_>>() != wanted {
            return Err(CohortProjectionError::new(
                "age windows require id, start_day and end_day",
            ));
        }
        let start = item.get("start_day");
        let end = item.get("end_day");
        let is_int =
            |v: Option<&Value>| matches!(v, Some(Value::Number(n)) if n.is_i64() || n.is_u64());
        if !item.get("id").is_some_and(Value::is_string)
            || !is_int(start)
            || !is_int(end)
            || start.and_then(py::to_i64).unwrap_or(-1) < 0
            || start.and_then(py::to_i64) > end.and_then(py::to_i64)
        {
            return Err(CohortProjectionError::new(
                "age windows require ordered non-negative integer days",
            ));
        }
        windows.push(item.clone());
    }
    let mut ids = BTreeSet::new();
    if windows
        .iter()
        .any(|w| !ids.insert(w.get("id").and_then(Value::as_str).unwrap_or_default()))
    {
        return Err(CohortProjectionError::new("age window ids must be unique"));
    }
    match document.get("as_of_date") {
        None | Some(Value::Null) => Ok((windows, None)),
        Some(v) => {
            let text = v.as_str();
            match text.and_then(IsoDate::parse) {
                Some(day) => Ok((windows, Some(day))),
                None => Err(CohortProjectionError::new(
                    "as_of_date must be an ISO calendar date",
                )),
            }
        }
    }
}

/// Build the publication cohort packages from a document or a file, into `out_dir`.
pub fn publication_cohorts(
    document: Option<&Value>,
    file: Option<&str>,
    out_dir: Option<&str>,
) -> Res<Value> {
    let input = read_input(document, file, PUBLICATION_FORMAT)?;
    let content = publication_contents(&input)?;
    let (windows, as_of) = age_windows(&input)?;
    let mut by_key: HashMap<String, Vec<usize>> = HashMap::new();
    for (index, row) in content.iter().enumerate() {
        if let Some(key) = row.get("url_key").and_then(Value::as_str) {
            by_key.entry(key.to_owned()).or_default().push(index);
        }
    }
    let collisions: BTreeSet<String> = by_key
        .iter()
        .filter(|(_, rows)| rows.len() > 1)
        .map(|(k, _)| k.clone())
        .collect();

    let mut inventory = Vec::new();
    let mut authors = Vec::new();
    let mut summary: BTreeMap<String, (i64, i64)> = BTreeMap::new();
    for row in &content {
        let publication = py::get(row, "publication").cloned().unwrap_or(Value::Null);
        let measured = publication.get("state").and_then(Value::as_str) == Some("measured");
        let cohort = if measured {
            py::str_value(publication.get("value").unwrap_or(&Value::Null))[..7].to_owned()
        } else {
            "unknown".to_owned()
        };
        let url_key = row
            .get("url_key")
            .and_then(Value::as_str)
            .unwrap_or_default();
        let join_state = if collisions.contains(url_key) {
            "collision"
        } else {
            row.get("url_key_state")
                .and_then(Value::as_str)
                .unwrap_or_default()
        };
        inventory.push({
            let mut row_out = Map::new();
            row_out.insert("content_id".to_owned(), row["content_id"].clone());
            row_out.insert("url".to_owned(), row["url"].clone());
            row_out.insert("url_key".to_owned(), row["url_key"].clone());
            row_out.insert("join_state".to_owned(), json!(join_state));
            row_out.insert("publication_cohort".to_owned(), json!(cohort));
            row_out.insert(
                "publication_raw".to_owned(),
                publication.get("raw").cloned().unwrap_or(Value::Null),
            );
            row_out.insert(
                "publication_date".to_owned(),
                publication.get("value").cloned().unwrap_or(Value::Null),
            );
            row_out.insert(
                "publication_state".to_owned(),
                publication.get("state").cloned().unwrap_or(Value::Null),
            );
            row_out.insert(
                "publication_source".to_owned(),
                publication.get("source").cloned().unwrap_or(Value::Null),
            );
            let modified = py::get(row, "modified").cloned().unwrap_or(Value::Null);
            row_out.insert(
                "modified_raw".to_owned(),
                modified.get("raw").cloned().unwrap_or(Value::Null),
            );
            row_out.insert(
                "modified_date".to_owned(),
                modified.get("value").cloned().unwrap_or(Value::Null),
            );
            row_out.insert(
                "modified_state".to_owned(),
                modified.get("state").cloned().unwrap_or(Value::Null),
            );
            let first = py::get(row, "first_observed")
                .cloned()
                .unwrap_or(Value::Null);
            row_out.insert(
                "first_observed_raw".to_owned(),
                first.get("raw").cloned().unwrap_or(Value::Null),
            );
            row_out.insert(
                "first_observed_date".to_owned(),
                first.get("value").cloned().unwrap_or(Value::Null),
            );
            row_out.insert(
                "first_observed_state".to_owned(),
                first.get("state").cloned().unwrap_or(Value::Null),
            );
            row_out.insert("authors_json".to_owned(), row["authors"].clone());
            row_out.insert("author_state".to_owned(), row["author_state"].clone());
            row_out.insert("word_count".to_owned(), row["word_count"].clone());
            row_out.insert(
                "word_count_state".to_owned(),
                row["word_count_state"].clone(),
            );
            row_out.insert(
                "word_count_reason".to_owned(),
                row["word_count_reason"].clone(),
            );
            row_out
        });
        let entry = summary.entry(cohort.clone()).or_default();
        entry.0 += 1;
        for author in row["authors"].as_array().cloned().unwrap_or_default() {
            authors.push({
                let mut r = Map::new();
                r.insert("content_id".to_owned(), row["content_id"].clone());
                r.insert("publication_cohort".to_owned(), json!(cohort.clone()));
                r.insert("author".to_owned(), author);
                r
            });
            summary.entry(cohort.clone()).or_default().1 += 1;
        }
    }

    let evidence_docs = normalized_documents(input.get("evidence"))?;
    let mut observations = Vec::new();
    let mut age_observations = Vec::new();
    for (doc_ordinal, evidence) in evidence_docs.iter().enumerate() {
        let source = source_of(evidence, doc_ordinal);
        for row in py::get_list(evidence, "rows") {
            let url = py::get(row, "url")
                .and_then(|u| u.get("normalized"))
                .and_then(Value::as_str);
            let key = url.and_then(|u| normalize_join_key(Some(u)));
            let matched: Vec<&Value> = match &key {
                Some(k) if !collisions.contains(k) => by_key
                    .get(k)
                    .map(|idx| idx.iter().map(|i| &content[*i]).collect())
                    .unwrap_or_default(),
                _ => Vec::new(),
            };
            let observed_date = py::get(row, "dimensions")
                .and_then(|d| d.get("date"))
                .and_then(Value::as_str);
            let observed_day = observed_date.and_then(IsoDate::parse);
            let metrics = py::get(row, "metrics").cloned().unwrap_or(Value::Null);
            if let Some(metric_map) = metrics.as_object() {
                for (metric_name, metric_entry) in metric_map {
                    let (value, state, reason) = metric(Some(metric_entry), metric_name)?;
                    let collection = py::get(&source, "collection")
                        .cloned()
                        .unwrap_or(Value::Null);
                    observations.push({
                        let mut r = Map::new();
                        r.insert("source_id".to_owned(), source["source_id"].clone());
                        r.insert("provider".to_owned(), source["provider"].clone());
                        r.insert("metric".to_owned(), json!(metric_name));
                        r.insert(
                            "url".to_owned(),
                            url.map(Value::from).unwrap_or(Value::Null),
                        );
                        r.insert(
                            "url_key".to_owned(),
                            key.clone().map(Value::from).unwrap_or(Value::Null),
                        );
                        r.insert(
                            "content_ids_json".to_owned(),
                            Value::Array(
                                matched
                                    .iter()
                                    .map(|item| item["content_id"].clone())
                                    .collect(),
                            ),
                        );
                        let mut cohorts: Vec<String> = matched
                            .iter()
                            .map(|item| {
                                let publication = &item["publication"];
                                if publication.get("state").and_then(Value::as_str)
                                    == Some("measured")
                                {
                                    py::str_value(publication.get("value").unwrap_or(&Value::Null))
                                        [..7]
                                        .to_owned()
                                } else {
                                    "unknown".to_owned()
                                }
                            })
                            .collect::<BTreeSet<_>>()
                            .into_iter()
                            .collect();
                        cohorts.sort();
                        r.insert(
                            "publication_cohorts_json".to_owned(),
                            Value::Array(cohorts.into_iter().map(Value::from).collect()),
                        );
                        r.insert(
                            "join_state".to_owned(),
                            json!(if !matched.is_empty() {
                                "matched"
                            } else if key.as_ref().is_some_and(|k| collisions.contains(k)) {
                                "collision"
                            } else {
                                "unmatched"
                            }),
                        );
                        for (k, v) in [
                            ("period_start", source["period_start"].clone()),
                            ("period_end", source["period_end"].clone()),
                            ("timezone", source["timezone"].clone()),
                            ("attribution", source["attribution"].clone()),
                        ] {
                            r.insert(k.to_owned(), v);
                        }
                        r.insert(
                            "collection_state".to_owned(),
                            collection.get("state").cloned().unwrap_or(Value::Null),
                        );
                        r.insert(
                            "sampled".to_owned(),
                            collection.get("sampled").cloned().unwrap_or(Value::Null),
                        );
                        r.insert(
                            "thresholded".to_owned(),
                            collection
                                .get("thresholded")
                                .cloned()
                                .unwrap_or(Value::Null),
                        );
                        r.insert(
                            "truncated".to_owned(),
                            collection.get("truncated").cloned().unwrap_or(Value::Null),
                        );
                        r.insert(
                            "metric_value".to_owned(),
                            value.map(|v| json!(v)).unwrap_or(Value::Null),
                        );
                        r.insert("metric_state".to_owned(), json!(state));
                        r.insert(
                            "metric_reason".to_owned(),
                            reason.clone().unwrap_or(Value::Null),
                        );
                        r.insert(
                            "dimensions_json".to_owned(),
                            py::get(row, "dimensions")
                                .cloned()
                                .unwrap_or_else(|| json!({})),
                        );
                        r.insert(
                            "ambiguous_row".to_owned(),
                            json!(py::truthy(row.get("ambiguous"))),
                        );
                        r
                    });
                    for item in &matched {
                        let publication = &item["publication"];
                        if publication.get("state").and_then(Value::as_str) != Some("measured")
                            || observed_day.is_none()
                        {
                            continue;
                        }
                        let published_day = publication
                            .get("value")
                            .and_then(Value::as_str)
                            .and_then(IsoDate::parse)
                            .expect("measured publication date");
                        let age_day = observed_day.unwrap().diff_days(&published_day);
                        for window in &windows {
                            let start = window.get("start_day").and_then(py::to_i64).unwrap_or(0);
                            let end = window.get("end_day").and_then(py::to_i64).unwrap_or(0);
                            if !(start <= age_day && age_day <= end) {
                                continue;
                            }
                            let mature = as_of.map(|asof| asof >= published_day.add_days(end));
                            age_observations.push({
                                let mut r = Map::new();
                                r.insert("content_id".to_owned(), item["content_id"].clone());
                                r.insert("source_id".to_owned(), source["source_id"].clone());
                                r.insert("provider".to_owned(), source["provider"].clone());
                                r.insert("metric".to_owned(), json!(metric_name));
                                r.insert("window_id".to_owned(), window["id"].clone());
                                r.insert(
                                    "window_start_day".to_owned(),
                                    window["start_day"].clone(),
                                );
                                r.insert("window_end_day".to_owned(), window["end_day"].clone());
                                r.insert("expected_days".to_owned(), json!(end - start + 1));
                                r.insert("age_day".to_owned(), json!(age_day));
                                r.insert(
                                    "observation_date".to_owned(),
                                    py::get(row, "dimensions")
                                        .and_then(|d| d.get("date"))
                                        .cloned()
                                        .unwrap_or(Value::Null),
                                );
                                r.insert(
                                    "metric_value".to_owned(),
                                    value.map(|v| json!(v)).unwrap_or(Value::Null),
                                );
                                r.insert("metric_state".to_owned(), json!(state.clone()));
                                r.insert(
                                    "mature".to_owned(),
                                    mature.map(Value::from).unwrap_or(Value::Null),
                                );
                                r.insert(
                                    "history_state".to_owned(),
                                    json!(if collection.get("state").and_then(Value::as_str)
                                        != Some("complete")
                                    {
                                        "partial"
                                    } else {
                                        "observed_day_only"
                                    }),
                                );
                                r
                            });
                        }
                    }
                }
            }
        }
    }

    let cohort_rows: Vec<Map<String, Value>> = summary
        .iter()
        .map(|(key, (content, authors))| {
            let mut r = Map::new();
            r.insert("publication_cohort".to_owned(), json!(key));
            r.insert("content_count".to_owned(), json!(content));
            r.insert("author_contributions".to_owned(), json!(authors));
            r
        })
        .collect();

    let manifest = json!({
        "format": PUBLICATION_RESULT_FORMAT,
        "source_format": PUBLICATION_FORMAT,
        "policy": {
            "publication_precedence": "publication, modification and first observation remain separate evidence fields",
            "aggregation": "GSC and analytics observations remain separately labelled and are never combined",
            "absence": "an absent or suppressed provider row is not a measured zero",
            "causality": "publication timing is descriptive and does not establish traffic causation",
        },
        "colliding_url_keys": collisions.iter().cloned().collect::<Vec<_>>(),
        "input_content_rows": content.len(),
        "input_evidence_documents": evidence_docs.len(),
        "age_windows": windows,
        "as_of_date": as_of.map(|d| Value::String(d.isoformat())).unwrap_or(Value::Null),
    });
    let observation_rows = observations.len();
    let package = atomic_package(
        out_dir,
        &[
            ("publication_inventory.csv".to_owned(), inventory),
            ("publication_authors.csv".to_owned(), authors),
            ("provider_observations.csv".to_owned(), observations),
            ("publication_cohorts.csv".to_owned(), cohort_rows),
            ("age_window_observations.csv".to_owned(), age_observations),
        ],
        manifest,
    )?;
    let mut result = Map::new();
    result.insert("format".to_owned(), json!(PUBLICATION_RESULT_FORMAT));
    if let Some(map) = package.as_object() {
        result.extend(map.clone());
    }
    result.insert("content_rows".to_owned(), json!(content.len()));
    result.insert("observation_rows".to_owned(), json!(observation_rows));
    Ok(Value::Object(result))
}

fn query_text(value: Option<&Value>) -> Option<String> {
    let text = value?.as_str()?;
    if text.trim().is_empty() {
        return None;
    }
    let normalized: String = text.nfkc().collect();
    let folded: String = normalized.chars().flat_map(|c| c.case_fold()).collect();
    let trimmed = folded.trim();
    if trimmed.is_empty() {
        None
    } else {
        Some(trimmed.to_owned())
    }
}

fn brand_rules(value: Option<&Value>) -> Res<Vec<(String, String, String)>> {
    let items = match value {
        Some(Value::Array(items)) if !items.is_empty() => items,
        _ => {
            return Err(CohortProjectionError::new(
                "brand_rules must be a non-empty list",
            ))
        }
    };
    let mut rules = Vec::new();
    for item in items {
        let keys: BTreeSet<&str> = item
            .as_object()
            .map(|m| m.keys().map(String::as_str).collect())
            .unwrap_or_default();
        if !item.is_object() || !keys.iter().all(|k| ["id", "match", "value"].contains(k)) {
            return Err(CohortProjectionError::new(
                "brand rules may contain id, match and value",
            ));
        }
        let match_kind = item
            .get("match")
            .and_then(Value::as_str)
            .unwrap_or_default();
        if !["exact", "tokens"].contains(&match_kind)
            || !item.get("id").is_some_and(Value::is_string)
        {
            return Err(CohortProjectionError::new(
                "brand rules require id and exact/tokens match",
            ));
        }
        let Some(text) = query_text(item.get("value")) else {
            return Err(CohortProjectionError::new(
                "brand rule values must be non-empty text",
            ));
        };
        rules.push((
            item["id"].as_str().unwrap_or_default().to_owned(),
            match_kind.to_owned(),
            text,
        ));
    }
    Ok(rules)
}

/// Classify a query: returns (classification, matched rule id, reason).
fn classify(
    query: Option<&Value>,
    rules: &[(String, String, String)],
) -> (String, Option<String>, Option<String>) {
    let Some(text) = query_text(query) else {
        return (
            "unknown".to_owned(),
            None,
            Some("query dimension is missing".to_owned()),
        );
    };
    let mut hits = Vec::new();
    for (id, match_kind, value) in rules {
        let exact = match_kind == "exact" && text == *value;
        let tokens = match_kind == "tokens" && token_search(value, &text);
        if exact || tokens {
            hits.push(id.clone());
        }
    }
    if hits.len() > 1 {
        return (
            "ambiguous".to_owned(),
            None,
            Some("multiple brand rules matched".to_owned()),
        );
    }
    if let Some(hit) = hits.into_iter().next() {
        return ("branded".to_owned(), Some(hit), None);
    }
    ("non_branded".to_owned(), None, None)
}

/// Whole-word match: `value` in `text`, not part of a longer word.
fn token_search(needle: &str, text: &str) -> bool {
    let is_word = |c: char| c.is_alphanumeric() || c == '_';
    let mut start = 0usize;
    while let Some(index) = text[start..].find(needle) {
        let index = start + index;
        let before_ok = index == 0 || !text[..index].chars().last().is_some_and(is_word);
        let after_index = index + needle.len();
        let after_ok =
            after_index >= text.len() || !text[after_index..].chars().next().is_some_and(is_word);
        if before_ok && after_ok {
            return true;
        }
        start = index + 1;
    }
    false
}

fn overrides(value: Option<&Value>) -> Res<Map<String, Value>> {
    let Some(value) = value else {
        return Ok(Map::new());
    };
    if value.is_null() {
        return Ok(Map::new());
    }
    let items = value
        .as_array()
        .ok_or_else(|| CohortProjectionError::new("overrides must be a list when supplied"))?;
    let mut output = Map::new();
    for item in items {
        let keys_ok = item.as_object().is_some_and(|m| {
            m.keys()
                .all(|k| ["query", "classification", "rule_id", "reason"].contains(&k.as_str()))
        });
        if !item.is_object() || !keys_ok {
            return Err(CohortProjectionError::new(
                "overrides may contain query, classification, rule_id and reason",
            ));
        }
        let query = query_text(item.get("query"));
        let classification = item.get("classification").and_then(Value::as_str);
        if query.is_none()
            || !matches!(
                classification,
                Some("branded" | "non_branded" | "unclassified")
            )
        {
            return Err(CohortProjectionError::new(
                "overrides need a query and a recorded classification",
            ));
        }
        let query = query.unwrap();
        if output.contains_key(&query) {
            return Err(CohortProjectionError::new(
                "overrides cannot classify one normalized query twice",
            ));
        }
        output.insert(
            query,
            json!({
                "classification": classification,
                "rule_id": item.get("rule_id").cloned().unwrap_or(Value::Null),
                "reason": item.get("reason").filter(|r| py::truthy(Some(r))).cloned().unwrap_or_else(|| json!("operator override")),
            }),
        );
    }
    Ok(output)
}

fn periods(value: Option<&Value>) -> Res<Vec<Value>> {
    let items = match value {
        Some(Value::Array(items)) if items.len() >= 2 => items,
        _ => {
            return Err(CohortProjectionError::new(
                "periods must contain at least two comparable windows",
            ))
        }
    };
    let mut output = Vec::new();
    for item in items {
        let keys: BTreeSet<&str> = item
            .as_object()
            .map(|m| m.keys().map(String::as_str).collect())
            .unwrap_or_default();
        if !item.is_object() || keys != ["id", "start_date", "end_date"].into_iter().collect() {
            return Err(CohortProjectionError::new(
                "periods require id, start_date and end_date",
            ));
        }
        let start = item
            .get("start_date")
            .and_then(Value::as_str)
            .and_then(IsoDate::parse);
        let end = item
            .get("end_date")
            .and_then(Value::as_str)
            .and_then(IsoDate::parse);
        let (Some(start), Some(end)) = (start, end) else {
            return Err(CohortProjectionError::new(
                "period dates must be ISO calendar dates",
            ));
        };
        if start > end || !item.get("id").is_some_and(Value::is_string) {
            return Err(CohortProjectionError::new(
                "periods must have ordered dates and string ids",
            ));
        }
        let mut row = item.as_object().cloned().unwrap_or_default();
        row.insert("_start".to_owned(), json!(start.days()));
        row.insert("_end".to_owned(), json!(end.days()));
        output.push(Value::Object(row));
    }
    let mut ids = BTreeSet::new();
    if output
        .iter()
        .any(|p| !ids.insert(p["id"].as_str().unwrap_or_default()))
    {
        return Err(CohortProjectionError::new("period ids must be unique"));
    }
    Ok(output)
}

fn position_bands(document: &Value) -> Res<Vec<(String, f64, Option<f64>)>> {
    let default = json!([
        {"id": "1-3", "min": 1, "max": 3},
        {"id": "4-10", "min": 4, "max": 10},
        {"id": "11-20", "min": 11, "max": 20},
        {"id": "21+", "min": 21, "max": null},
    ]);
    let value = match document.get("position_bands") {
        Some(v) => v.clone(),
        None => default,
    };
    let items = match &value {
        Value::Array(items) if !items.is_empty() => items.clone(),
        _ => {
            return Err(CohortProjectionError::new(
                "position_bands must be a non-empty list",
            ))
        }
    };
    let mut bands = Vec::new();
    for item in &items {
        let keys: BTreeSet<&str> = item
            .as_object()
            .map(|m| m.keys().map(String::as_str).collect())
            .unwrap_or_default();
        if !item.is_object() || keys != ["id", "min", "max"].into_iter().collect() {
            return Err(CohortProjectionError::new(
                "position bands require id, min and max",
            ));
        }
        let lower = item.get("min");
        let upper = item.get("max");
        let numeric = |v: Option<&Value>| match v {
            Some(Value::Number(n)) => n.as_f64(),
            _ => None,
        };
        let lower_f = numeric(lower);
        let upper_ok = match upper {
            None | Some(Value::Null) => true,
            _ => numeric(upper).is_some_and(|u| u >= lower_f.unwrap_or(f64::MAX)),
        };
        if !item.get("id").is_some_and(Value::is_string)
            || lower_f.is_none()
            || lower_f.is_some_and(|l| l < 0.0)
            || !upper_ok
        {
            return Err(CohortProjectionError::new(
                "position bands need ordered numeric boundaries",
            ));
        }
        bands.push((
            item["id"].as_str().unwrap_or_default().to_owned(),
            lower_f.unwrap_or(0.0),
            numeric(upper),
        ));
    }
    let mut ids = BTreeSet::new();
    if bands.iter().any(|(id, _, _)| !ids.insert(id.clone())) {
        return Err(CohortProjectionError::new(
            "position band ids must be unique",
        ));
    }
    Ok(bands)
}

fn band(value: Option<f64>, bands: &[(String, f64, Option<f64>)]) -> Option<String> {
    let value = value?;
    for (id, min, max) in bands {
        if value >= *min && (max.is_none() || value <= max.unwrap()) {
            return Some(id.clone());
        }
    }
    None
}

/// Build the GSC progress package from a document or a file, into `out_dir`.
pub fn gsc_progress(
    document: Option<&Value>,
    file: Option<&str>,
    out_dir: Option<&str>,
) -> Res<Value> {
    let input = read_input(document, file, GSC_PROGRESS_FORMAT)?;
    let rules = brand_rules(input.get("brand_rules"))?;
    let periods = periods(input.get("periods"))?;
    let bands = position_bands(&input)?;
    let overrides = overrides(input.get("overrides"))?;

    #[derive(Default)]
    struct Bucket {
        clicks: f64,
        impressions: f64,
        ctr_clicks: f64,
        ctr_impressions: f64,
        rows: i64,
        queries: BTreeSet<String>,
        unmeasured: i64,
        collection_states: BTreeSet<String>,
    }

    let mut queries: Vec<Map<String, Value>> = Vec::new();
    let mut groups: BTreeMap<(String, String, String), Bucket> = BTreeMap::new();
    for (doc_ordinal, evidence) in normalized_documents(input.get("evidence"))?
        .iter()
        .enumerate()
    {
        let source = source_of(evidence, doc_ordinal);
        if source["provider"].as_str() != Some("gsc") {
            return Err(CohortProjectionError::new(
                "gsc progress accepts only normalized GSC evidence",
            ));
        }
        for row in py::get_list(evidence, "rows") {
            let dimensions = py::get(row, "dimensions").cloned().unwrap_or(Value::Null);
            let query = dimensions.get("query");
            let query_normalized = query_text(query);
            let (classification, rule_id, reason, classification_source) =
                match query_normalized.as_ref().and_then(|q| overrides.get(q)) {
                    Some(over) => (
                        py::str_value(over.get("classification").unwrap_or(&Value::Null)),
                        over.get("rule_id").cloned().unwrap_or(Value::Null),
                        over.get("reason").cloned().unwrap_or(Value::Null),
                        "operator_override",
                    ),
                    None => {
                        let (c, r, why) = classify(query, &rules);
                        (
                            c,
                            r.map(Value::from).unwrap_or(Value::Null),
                            why.map(Value::from).unwrap_or(Value::Null),
                            "rule",
                        )
                    }
                };
            let row_date = dimensions.get("date").and_then(Value::as_str);
            let mut selected: Vec<&Value> = Vec::new();
            if let Some(row_date) = row_date {
                if let Some(day) = IsoDate::parse(row_date) {
                    selected = periods
                        .iter()
                        .filter(|p| {
                            let s = p.get("_start").and_then(py::to_i64).unwrap_or(0);
                            let e = p.get("_end").and_then(py::to_i64).unwrap_or(0);
                            s <= day.days() && day.days() <= e
                        })
                        .collect();
                }
            } else if source["period_start"].is_string() && source["period_end"].is_string() {
                selected = periods
                    .iter()
                    .filter(|p| {
                        p.get("start_date") == source.get("period_start")
                            && p.get("end_date") == source.get("period_end")
                    })
                    .collect();
            }
            if selected.len() != 1 {
                continue;
            }
            let period = selected[0];
            let mut scope = Map::new();
            if let Some(dims) = dimensions.as_object() {
                for (key, value) in dims {
                    if !["query", "page", "date"].contains(&key.as_str()) {
                        scope.insert(key.clone(), value.clone());
                    }
                }
            }
            scope.insert("timezone".to_owned(), source["timezone"].clone());
            scope.insert("search_type".to_owned(), source["search_type"].clone());
            scope.insert("source_id".to_owned(), source["source_id"].clone());
            let scope_json = Value::Object(scope);
            let scope_id =
                crate::bi::schema::digest(canonical(&scope_json).as_bytes())[..16].to_owned();
            let metrics = py::get(row, "metrics").cloned().unwrap_or(Value::Null);
            let (clicks, clicks_state, clicks_reason) = metric(metrics.get("clicks"), "clicks")?;
            let (impressions, impressions_state, impressions_reason) =
                metric(metrics.get("impressions"), "impressions")?;
            let (position, position_state, position_reason) =
                metric(metrics.get("position"), "position")?;
            let collection = py::get(&source, "collection")
                .cloned()
                .unwrap_or(Value::Null);
            let position_band = if position_state == "measured" {
                band(position, &bands)
            } else {
                None
            };
            let mut query_row = Map::new();
            for (k, v) in [
                ("source_id", source["source_id"].clone()),
                ("scope_id", json!(scope_id)),
                ("scope_json", scope_json),
                ("period_id", period["id"].clone()),
                ("query_raw", query.cloned().unwrap_or(Value::Null)),
                (
                    "query_normalized",
                    query_normalized
                        .clone()
                        .map(Value::from)
                        .unwrap_or(Value::Null),
                ),
                ("classification", json!(classification)),
                ("matched_rule_id", rule_id.clone()),
                ("classification_reason", reason.clone()),
                ("classification_source", json!(classification_source)),
                ("clicks", clicks.map(|v| json!(v)).unwrap_or(Value::Null)),
                ("clicks_state", json!(clicks_state)),
                (
                    "clicks_reason",
                    clicks_reason.clone().unwrap_or(Value::Null),
                ),
                (
                    "impressions",
                    impressions.map(|v| json!(v)).unwrap_or(Value::Null),
                ),
                ("impressions_state", json!(impressions_state)),
                (
                    "impressions_reason",
                    impressions_reason.clone().unwrap_or(Value::Null),
                ),
                (
                    "average_position",
                    position.map(|v| json!(v)).unwrap_or(Value::Null),
                ),
                ("position_state", json!(position_state)),
                (
                    "position_reason",
                    position_reason.clone().unwrap_or(Value::Null),
                ),
                (
                    "collection_state",
                    collection.get("state").cloned().unwrap_or(Value::Null),
                ),
                (
                    "sampled",
                    collection.get("sampled").cloned().unwrap_or(Value::Null),
                ),
                (
                    "thresholded",
                    collection
                        .get("thresholded")
                        .cloned()
                        .unwrap_or(Value::Null),
                ),
                (
                    "truncated",
                    collection.get("truncated").cloned().unwrap_or(Value::Null),
                ),
                ("ambiguous_row", json!(py::truthy(row.get("ambiguous")))),
                (
                    "position_band",
                    position_band.map(Value::from).unwrap_or(Value::Null),
                ),
            ] {
                query_row.insert(k.to_owned(), v);
            }
            let period_id = period["id"].as_str().unwrap_or_default().to_owned();
            queries.push(query_row);
            let bucket = groups
                .entry((scope_id.clone(), period_id, classification.clone()))
                .or_default();
            bucket.rows += 1;
            {
                let state = collection.get("state").cloned().unwrap_or(Value::Null);
                bucket
                    .collection_states
                    .insert(if py::truthy(Some(&state)) {
                        py::str_value(&state)
                    } else {
                        "unknown".to_owned()
                    });
            }
            match clicks {
                Some(v) => bucket.clicks += v,
                None => bucket.unmeasured += 1,
            }
            match impressions {
                Some(v) => bucket.impressions += v,
                None => bucket.unmeasured += 1,
            }
            if let (Some(c), Some(i)) = (clicks, impressions) {
                bucket.ctr_clicks += c;
                bucket.ctr_impressions += i;
            }
            if let Some(qn) = &query_normalized {
                bucket.queries.insert(qn.clone());
            }
        }
    }

    let mut summaries = Vec::new();
    for ((scope_id, period_id, classification), bucket) in &groups {
        let ctr = if bucket.ctr_impressions != 0.0 {
            json!(bucket.ctr_clicks / bucket.ctr_impressions)
        } else {
            Value::Null
        };
        let mut r = Map::new();
        r.insert("scope_id".to_owned(), json!(scope_id));
        r.insert("period_id".to_owned(), json!(period_id));
        r.insert("classification".to_owned(), json!(classification));
        r.insert("clicks".to_owned(), json!(bucket.clicks));
        r.insert("impressions".to_owned(), json!(bucket.impressions));
        r.insert("ctr".to_owned(), ctr);
        r.insert(
            "contributing_query_count".to_owned(),
            json!(bucket.queries.len()),
        );
        r.insert("source_rows".to_owned(), json!(bucket.rows));
        r.insert(
            "unmeasured_metric_cells".to_owned(),
            json!(bucket.unmeasured),
        );
        r.insert(
            "collection_states_json".to_owned(),
            Value::Array(
                bucket
                    .collection_states
                    .iter()
                    .cloned()
                    .map(Value::from)
                    .collect(),
            ),
        );
        summaries.push(r);
    }

    let order: HashMap<String, usize> = periods
        .iter()
        .enumerate()
        .map(|(index, p)| (p["id"].as_str().unwrap_or_default().to_owned(), index))
        .collect();
    let mut by_summary: BTreeMap<(String, String), Vec<&Map<String, Value>>> = BTreeMap::new();
    for item in &summaries {
        by_summary
            .entry((
                item["scope_id"].as_str().unwrap_or_default().to_owned(),
                item["classification"]
                    .as_str()
                    .unwrap_or_default()
                    .to_owned(),
            ))
            .or_default()
            .push(item);
    }
    let mut changes = Vec::new();
    for ((scope_id, classification), mut entries) in by_summary {
        entries.sort_by_key(|item| order[item["period_id"].as_str().unwrap_or_default()]);
        let mut prior: Option<&Map<String, Value>> = None;
        for current in entries {
            if let Some(prior) = prior {
                let prior_clicks = prior["clicks"].as_f64().unwrap_or(0.0);
                let current_clicks = current["clicks"].as_f64().unwrap_or(0.0);
                let (percent, state) = if prior_clicks == 0.0 {
                    if current_clicks == 0.0 {
                        (json!(0.0), "unchanged_zero")
                    } else {
                        (Value::Null, "new_from_zero")
                    }
                } else {
                    (
                        json!((current_clicks - prior_clicks) / prior_clicks),
                        "defined",
                    )
                };
                let mut r = Map::new();
                r.insert("scope_id".to_owned(), json!(scope_id));
                r.insert("classification".to_owned(), json!(classification));
                r.insert("from_period_id".to_owned(), prior["period_id"].clone());
                r.insert("to_period_id".to_owned(), current["period_id"].clone());
                r.insert(
                    "click_change".to_owned(),
                    json!(current_clicks - prior_clicks),
                );
                r.insert("click_change_percent".to_owned(), percent);
                r.insert("percent_change_state".to_owned(), json!(state));
                changes.push(r);
            }
            prior = Some(current);
        }
    }

    type QueryKey = (String, String, String);
    let mut by_query: BTreeMap<QueryKey, Vec<&Map<String, Value>>> = BTreeMap::new();
    for row in &queries {
        if row["position_state"].as_str() == Some("measured")
            && !py::truthy(row.get("ambiguous_row"))
            && row["query_normalized"].is_string()
        {
            by_query
                .entry((
                    row["scope_id"].as_str().unwrap_or_default().to_owned(),
                    row["classification"]
                        .as_str()
                        .unwrap_or_default()
                        .to_owned(),
                    row["query_normalized"]
                        .as_str()
                        .unwrap_or_default()
                        .to_owned(),
                ))
                .or_default()
                .push(row);
        }
    }
    let mut position_rows = Vec::new();
    for (identity, mut entries) in by_query {
        entries.sort_by_key(|item| order[item["period_id"].as_str().unwrap_or_default()]);
        let mut prior: Option<&Map<String, Value>> = None;
        for row in entries {
            let movement = prior.map(|prior| {
                let cur = row["average_position"].as_f64().unwrap_or(0.0);
                let prev = prior["average_position"].as_f64().unwrap_or(0.0);
                if cur < prev {
                    "improved"
                } else if cur > prev {
                    "worsened"
                } else {
                    "unchanged"
                }
            });
            let mut r = Map::new();
            r.insert("scope_id".to_owned(), json!(identity.0));
            r.insert("classification".to_owned(), json!(identity.1));
            r.insert("query_normalized".to_owned(), json!(identity.2));
            r.insert("period_id".to_owned(), row["period_id"].clone());
            r.insert(
                "average_position".to_owned(),
                row["average_position"].clone(),
            );
            r.insert("position_band".to_owned(), row["position_band"].clone());
            r.insert(
                "previous_period_id".to_owned(),
                prior.map(|p| p["period_id"].clone()).unwrap_or(Value::Null),
            );
            r.insert(
                "previous_average_position".to_owned(),
                prior
                    .map(|p| p["average_position"].clone())
                    .unwrap_or(Value::Null),
            );
            r.insert(
                "movement".to_owned(),
                movement.map(Value::from).unwrap_or(Value::Null),
            );
            r.insert(
                "interpretation".to_owned(),
                json!("average-position band; not a tracked rank or page-placement claim"),
            );
            position_rows.push(r);
            prior = Some(row);
        }
    }

    let manifest = json!({
        "format": GSC_PROGRESS_RESULT_FORMAT,
        "source_format": GSC_PROGRESS_FORMAT,
        "brand_rules": rules.iter().map(|(id, m, v)| json!({"id": id, "match": m, "value": v})).collect::<Vec<_>>(),
        "overrides": overrides.values().cloned().collect::<Vec<_>>(),
        "periods": periods.iter().map(|p| json!({"id": p["id"], "start_date": p["start_date"], "end_date": p["end_date"]})).collect::<Vec<_>>(),
        "position_bands": bands.iter().map(|(id, min, max)| json!({"id": id, "min": min, "max": max})).collect::<Vec<_>>(),
        "policy": {
            "ctr": "CTR is clicks divided by impressions for rows where both are measured; row CTR values are never averaged",
            "absence": "query rows absent from a supplied, limited or anonymized export remain unknown",
            "scope": "country/device/date/source scope is retained per scope_id and never silently combined",
            "position": "average-position values are not rank snapshots or page-one placement claims",
            "causality": "the projection describes observations and makes no SEO diagnosis or growth claim",
        },
        "source_rows": queries.len(),
    });
    let query_rows = queries.len();
    let summary_rows = summaries.len();
    let package = atomic_package(
        out_dir,
        &[
            ("query_contributions.csv".to_owned(), queries),
            ("period_summaries.csv".to_owned(), summaries),
            ("period_changes.csv".to_owned(), changes),
            ("position_bands.csv".to_owned(), position_rows),
        ],
        manifest,
    )?;
    let mut result = Map::new();
    result.insert("format".to_owned(), json!(GSC_PROGRESS_RESULT_FORMAT));
    if let Some(map) = package.as_object() {
        result.extend(map.clone());
    }
    result.insert("query_rows".to_owned(), json!(query_rows));
    result.insert("summary_rows".to_owned(), json!(summary_rows));
    Ok(Value::Object(result))
}
