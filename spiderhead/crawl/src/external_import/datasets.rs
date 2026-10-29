//! Third-party dataset files: path admission, CSV parsing and per-field
//! coverage for one declared dataset of a mapped CSV bundle.

use std::collections::{HashMap, HashSet};
use std::io::Read;
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};

use serde_json::{json, Map, Value};
use sha2::{Digest, Sha256};

use super::ExternalCrawlImportError;
use super::ImportLimits;

/// Canonical field order and types per dataset — part of third_party_crawl.v1.
pub(super) const DATASET_FIELDS: [(&str, &[(&str, &str)]); 4] = [
    (
        "pages",
        &[
            ("url", "url"),
            ("status_code", "integer"),
            ("content_type", "string"),
            ("title", "string"),
            ("canonical_url", "url"),
            ("indexability", "string"),
        ],
    ),
    (
        "links",
        &[
            ("source_url", "url"),
            ("destination_url", "url"),
            ("anchor", "string"),
            ("rel", "string"),
            ("position", "string"),
            ("status_code", "integer"),
        ],
    ),
    (
        "statuses",
        &[
            ("url", "url"),
            ("status_code", "integer"),
            ("status_text", "string"),
        ],
    ),
    (
        "redirects",
        &[
            ("source_url", "url"),
            ("destination_url", "url"),
            ("status_code", "integer"),
            ("hop", "integer"),
        ],
    ),
];

/// The normalized-URL keys a record carries beside each URL field; a collision
/// on these is what the duplicate summary describes.
fn url_key_name(field: &str) -> &'static str {
    match field {
        "source_url" => "source_url_key",
        "destination_url" => "destination_url_key",
        "canonical_url" => "canonical_url_key",
        _ => "url_key",
    }
}

/// The record fields a duplicate observation is counted on, per dataset.
fn identity_fields(dataset: &str) -> &'static [&'static str] {
    match dataset {
        "links" => &["source_url_key", "destination_url_key", "anchor"],
        "statuses" => &["url_key", "status_code"],
        "redirects" => &["source_url_key", "hop"],
        _ => &["url_key"],
    }
}

/// A non-empty string of at most `max_length` characters, stripped.
pub(super) fn required_text(
    value: Option<&Value>,
    label: &str,
    max_length: usize,
) -> Result<String, ExternalCrawlImportError> {
    let raw = match value {
        Some(Value::String(text))
            if !text.trim().is_empty() && text.chars().count() <= max_length =>
        {
            text
        }
        _ => {
            return Err(ExternalCrawlImportError(format!(
                "{label} must be a non-empty string of at most {max_length} characters"
            )))
        }
    };
    Ok(raw.trim().to_string())
}

/// The object `value`, with unknown-field admission against `allowed`.
pub(super) fn mapping<'a>(
    value: Option<&'a Value>,
    label: &str,
    allowed: &[&str],
) -> Result<&'a Map<String, Value>, ExternalCrawlImportError> {
    let object = match value {
        Some(Value::Object(map)) => map,
        _ => {
            return Err(ExternalCrawlImportError(format!(
                "{label} must be an object"
            )))
        }
    };
    let mut unknown: Vec<&str> = object
        .keys()
        .map(String::as_str)
        .filter(|key| !allowed.contains(key))
        .collect();
    unknown.sort_unstable();
    if !unknown.is_empty() {
        return Err(ExternalCrawlImportError(format!(
            "{label} has unknown field(s): {}",
            unknown.join(", ")
        )));
    }
    Ok(object)
}

/// The extension of the last path component, reference-style: a leading dot does
/// not start the suffix.
fn posix_suffix(name: &str) -> &str {
    match name.rfind('.') {
        Some(i) if i > 0 && i < name.len() - 1 => &name[i..],
        _ => "",
    }
}

/// Path components under PurePosixPath rules: empty and single-dot components
/// collapse away, `..` is kept.
fn posix_parts(relative: &str) -> Vec<&str> {
    relative
        .split('/')
        .filter(|part| !part.is_empty() && *part != ".")
        .collect()
}

/// Resolve one declared dataset file inside the manifest directory: relative,
/// CSV, free of symlinks, and never shared with another dataset.
pub(super) fn dataset_path(
    root: &Path,
    value: Option<&Value>,
    used: &mut HashSet<PathBuf>,
    label: &str,
) -> Result<PathBuf, ExternalCrawlImportError> {
    let relative = required_text(value, &format!("{label}.file"), 512)?;
    let parts = posix_parts(&relative);
    if relative.starts_with('/')
        || relative.contains('\\')
        || relative.contains(':')
        || parts.contains(&"..")
    {
        return Err(ExternalCrawlImportError(format!(
            "{label}.file must be a relative path inside the manifest directory"
        )));
    }
    let last = parts.last().copied().unwrap_or("");
    if !posix_suffix(last).eq_ignore_ascii_case(".csv") {
        return Err(ExternalCrawlImportError(format!(
            "{label}.file must name a UTF-8 CSV file; XLSX is not supported"
        )));
    }
    let path: PathBuf = parts
        .iter()
        .fold(root.to_path_buf(), |acc, part| acc.join(part));
    let mut component = root.to_path_buf();
    for part in &parts {
        component = component.join(part);
        let is_symlink = std::fs::symlink_metadata(&component)
            .map(|meta| meta.file_type().is_symlink())
            .unwrap_or(false);
        if is_symlink {
            return Err(ExternalCrawlImportError(format!(
                "{label}.file cannot contain symlink path components"
            )));
        }
    }
    if !path.is_file() {
        return Err(ExternalCrawlImportError(format!(
            "{label}.file must be an existing regular file inside the manifest directory"
        )));
    }
    let resolved = std::fs::canonicalize(&path).map_err(|error| {
        ExternalCrawlImportError(format!(
            "cannot inspect {label}.file '{relative}': {}",
            io_message(&error)
        ))
    })?;
    if !resolved.starts_with(root) {
        return Err(ExternalCrawlImportError(format!(
            "{label}.file must be an existing regular file inside the manifest directory"
        )));
    }
    if used.contains(&resolved) {
        return Err(ExternalCrawlImportError(format!(
            "source file is assigned to more than one dataset: '{relative}'"
        )));
    }
    used.insert(resolved.clone());
    Ok(resolved)
}

/// An OS error's message without Rust's ` (os error N)` tail, matching the
/// bare strerror wording the reference `strerror` attribute carries.
fn io_message(error: &std::io::Error) -> String {
    let text = error.to_string();
    match text.find(" (os error ") {
        Some(i) => text[..i].to_string(),
        None => text,
    }
}

/// Parse one CSV cell into its field value, or name the reason it is invalid.
/// `None` and the empty string mean "absent", not invalid.
fn parse_value(
    raw: Option<&str>,
    kind: &str,
    label: &str,
) -> Result<(Option<Value>, Option<String>), ExternalCrawlImportError> {
    let raw = match raw {
        None | Some("") => return Ok((None, None)),
        Some(text) => text,
    };
    match kind {
        "string" => Ok((Some(Value::String(raw.to_string())), None)),
        "integer" => {
            let Some(value) = python_int(raw) else {
                return Ok((None, Some(format!("{label} is not an integer"))));
            };
            if label.starts_with("status_code") && !(100..=599).contains(&value) {
                return Ok((
                    None,
                    Some(format!(
                        "{label} is outside the HTTP status-code range 100..599"
                    )),
                ));
            }
            if label.starts_with("hop") && value < 0 {
                return Ok((None, Some(format!("{label} must not be negative"))));
            }
            Ok((Some(json!(value)), None))
        }
        "url" => {
            let parts = match spiderhead_parse::py_url::urlsplit(raw) {
                Ok(parts) => parts,
                Err(_) => {
                    return Ok((
                        None,
                        Some(format!("{label} is not an absolute HTTP(S) URL")),
                    ))
                }
            };
            let hostname = hostname_of(&parts.netloc);
            if !(parts.scheme == "http" || parts.scheme == "https") || hostname.is_none() {
                return Ok((
                    None,
                    Some(format!("{label} is not an absolute HTTP(S) URL")),
                ));
            }
            if parts.netloc.contains('@') {
                return Ok((
                    None,
                    Some(format!("{label} contains embedded URL credentials")),
                ));
            }
            if super::normalize_url_checked(raw).is_err() {
                return Ok((
                    None,
                    Some(format!("{label} is not an absolute HTTP(S) URL")),
                ));
            }
            Ok((Some(Value::String(raw.to_string())), None))
        }
        _ => Err(ExternalCrawlImportError(format!(
            "unhandled field type: {kind}"
        ))),
    }
}

/// The host part of a netloc: userinfo and port dropped, brackets kept out,
/// lower-cased — `urlsplit(...).hostname` with `None` for a missing host.
fn hostname_of(netloc: &str) -> Option<String> {
    if netloc.is_empty() {
        return None;
    }
    let hostport = match netloc.rsplit_once('@') {
        Some((_, rest)) => rest,
        None => netloc,
    };
    let host = if let Some(bracketed) = hostport.strip_prefix('[') {
        match bracketed.split_once(']') {
            Some((inner, _)) => inner,
            None => return None,
        }
    } else {
        hostport.split(':').next().unwrap_or("")
    };
    if host.is_empty() {
        None
    } else {
        Some(host.to_lowercase())
    }
}

/// `int(text, 10)`: optional sign, ASCII digits, underscores only between
/// digits, surrounding whitespace ignored.
fn python_int(raw: &str) -> Option<i64> {
    let trimmed = raw.trim_matches(|c: char| c.is_whitespace());
    let (sign, digits) = match trimmed.strip_prefix('-') {
        Some(rest) => (-1i64, rest),
        None => (1i64, trimmed.strip_prefix('+').unwrap_or(trimmed)),
    };
    if digits.is_empty() {
        return None;
    }
    let chars: Vec<char> = digits.chars().collect();
    let mut cleaned = String::with_capacity(digits.len());
    for (i, c) in chars.iter().enumerate() {
        if *c == '_' {
            let between = i > 0
                && chars[i - 1].is_ascii_digit()
                && i + 1 < chars.len()
                && chars[i + 1].is_ascii_digit();
            if !between {
                return None;
            }
        } else if c.is_ascii_digit() {
            cleaned.push(*c);
        } else {
            return None;
        }
    }
    cleaned.parse::<i64>().ok().map(|value| sign * value)
}

/// Per-field counters for one dataset's rows.
#[derive(Default, Clone, Copy)]
struct FieldStat {
    rows: i64,
    present: i64,
    missing: i64,
    invalid: i64,
}

/// What one dataset's CSV parses into: rows, declared headers, per-field
/// counters and duplicate groups.
type ParsedDataset = (Vec<Value>, Vec<String>, Vec<FieldStat>, Vec<Value>);

/// Read one declared dataset: parse its CSV, keep every row, and describe
/// coverage, duplicates and provenance. Returns the dataset document and the
/// byte size the import charged.
pub(super) fn read_dataset(
    dataset: &str,
    config: &Value,
    root: &Path,
    used: &mut HashSet<PathBuf>,
    remaining_bytes: i64,
    limits: &ImportLimits,
) -> Result<(Value, i64), ExternalCrawlImportError> {
    let fields = DATASET_FIELDS
        .iter()
        .find(|(name, _)| *name == dataset)
        .map(|(_, fields)| *fields)
        .unwrap_or(&[]);
    let allowed: Vec<&str> = fields.iter().map(|(field, _)| *field).collect();
    let config = mapping(
        Some(config),
        &format!("datasets.{dataset}"),
        &["file", "columns", "reason"],
    )?;
    // A missing `columns` key is an empty mapping; an explicit null is an
    // object-type violation, exactly as `config.get("columns", {})` behaves.
    let empty_columns = Value::Object(Map::new());
    let columns = mapping(
        Some(config.get("columns").unwrap_or(&empty_columns)),
        &format!("datasets.{dataset}.columns"),
        &allowed,
    )?;
    for column in columns.values() {
        if !column.is_string() || column.as_str().map(str::is_empty).unwrap_or(true) {
            return Err(ExternalCrawlImportError(format!(
                "datasets.{dataset}.columns values must be non-empty header names"
            )));
        }
    }
    let mut seen_columns: HashSet<&str> = HashSet::new();
    for column in columns.values().filter_map(Value::as_str) {
        if !seen_columns.insert(column) {
            return Err(ExternalCrawlImportError(format!(
                "datasets.{dataset}.columns cannot map two fields to one source column"
            )));
        }
    }

    let filename = config.get("file").filter(|value| !value.is_null());
    if filename.is_none() {
        let reason = match config.get("reason").and_then(Value::as_str) {
            Some(reason) if !reason.trim().is_empty() => reason.trim().to_string(),
            _ => {
                return Err(ExternalCrawlImportError(format!(
                    "datasets.{dataset} without a file requires a reason"
                )))
            }
        };
        let mut field_coverage = Map::new();
        for (field, _) in fields {
            field_coverage.insert(
                (*field).to_string(),
                json!({
                    "state": "not_exported",
                    "source_column": Value::Null,
                    "rows": 0,
                    "missing": 0,
                    "invalid": 0,
                    "reason": reason,
                }),
            );
        }
        return Ok((
            json!({
                "state": "unavailable",
                "reason": reason,
                "row_count": 0,
                "records": [],
                "field_coverage": Value::Object(field_coverage),
                "duplicate_summary": {"duplicate_groups": 0, "duplicate_rows": 0},
            }),
            0,
        ));
    }

    let path = dataset_path(root, filename, used, &format!("datasets.{dataset}"))?;
    let name = path
        .file_name()
        .map(|n| n.to_string_lossy().to_string())
        .unwrap_or_default();
    let before = std::fs::metadata(&path).map_err(|error| {
        ExternalCrawlImportError(format!(
            "cannot stat source CSV '{name}': {}",
            io_message(&error)
        ))
    })?;
    if !before.is_file() {
        return Err(ExternalCrawlImportError(format!(
            "source CSV '{name}' is not a regular file"
        )));
    }
    let budget = limits.max_total_csv_bytes.min(remaining_bytes);
    if before.len() as i64 > budget {
        return Err(ExternalCrawlImportError(format!(
            "source CSV '{name}' exceeds the remaining {budget}-byte import budget"
        )));
    }
    let mut raw_bytes = Vec::new();
    std::fs::File::open(&path)
        .and_then(|mut file| file.read_to_end(&mut raw_bytes))
        .map_err(|error| {
            ExternalCrawlImportError(format!(
                "cannot parse source CSV '{name}': {}",
                io_message(&error)
            ))
        })?;
    let digest = {
        let mut hasher = Sha256::new();
        hasher.update(&raw_bytes);
        format!("{:x}", hasher.finalize())
    };
    let text = String::from_utf8(raw_bytes).map_err(|error| {
        ExternalCrawlImportError(format!("cannot parse source CSV '{name}': {error}"))
    })?;
    let text = text
        .strip_prefix('\u{feff}')
        .map(str::to_string)
        .unwrap_or(text);

    let parse_result = parse_csv_rows(fields, columns, &text, &name, limits);
    let (records, headers, stats, row_errors) = match parse_result {
        Ok(parsed) => parsed,
        Err(error) => match error {
            RowError::Import(message) => return Err(ExternalCrawlImportError(message)),
            RowError::Parse(detail) => {
                return Err(ExternalCrawlImportError(format!(
                    "cannot parse source CSV '{name}': {detail}"
                )))
            }
        },
    };

    let after = std::fs::metadata(&path).map_err(|error| {
        ExternalCrawlImportError(format!(
            "cannot restat source CSV '{name}': {}",
            io_message(&error)
        ))
    })?;
    if (before.len(), before.mtime_nsec(), before.ino())
        != (after.len(), after.mtime_nsec(), after.ino())
    {
        return Err(ExternalCrawlImportError(format!(
            "source CSV '{name}' changed during import"
        )));
    }

    // Keep every row. A normalized URL collision is described; it never
    // silently collapses duplicate observations into one record.
    let mut seen: HashMap<String, usize> = HashMap::new();
    for record in &records {
        let values: Vec<Value> = identity_fields(dataset)
            .iter()
            .map(|field| record.get(*field).cloned().unwrap_or(Value::Null))
            .collect();
        if values.iter().all(|value| !value.is_null()) {
            let key = serde_json::to_string(&values).unwrap_or_default();
            *seen.entry(key).or_insert(0) += 1;
        }
    }
    let duplicate_groups = seen.values().filter(|&&count| count > 1).count();
    let duplicate_rows: usize = seen
        .values()
        .filter(|&&count| count > 1)
        .map(|count| count - 1)
        .sum();

    let header_set: HashSet<&str> = headers.iter().map(String::as_str).collect();
    let mut field_coverage = Map::new();
    for (index, (field, _)) in fields.iter().enumerate() {
        let stat = stats[index];
        let source_column = columns.get(*field).and_then(Value::as_str);
        let (state, reason) = match source_column {
            None => ("not_exported", "manifest does not map this field"),
            Some(column) if !header_set.contains(column) => (
                "unavailable",
                "mapped source column is absent from the CSV header",
            ),
            _ if records.is_empty() => ("unavailable", "source CSV contains no data rows"),
            _ if stat.invalid > 0 || stat.missing > 0 => {
                ("partial", "one or more source values are blank or invalid")
            }
            _ => ("complete", "all rows contain a valid mapped value"),
        };
        field_coverage.insert(
            (*field).to_string(),
            json!({
                "state": state,
                "scope": "values in the supplied CSV rows only; does not describe whole-crawl coverage",
                "source_column": source_column.map(Value::from).unwrap_or(Value::Null),
                "rows": stat.rows,
                "present": stat.present,
                "missing": stat.missing,
                "invalid": stat.invalid,
                "reason": reason,
            }),
        );
    }
    let relative_name = path
        .strip_prefix(root)
        .map(|rest| rest.to_string_lossy().to_string())
        .unwrap_or_else(|_| path.to_string_lossy().to_string());
    let unmapped: Vec<&str> = headers
        .iter()
        .map(String::as_str)
        .filter(|header| {
            !columns
                .values()
                .filter_map(Value::as_str)
                .any(|column| column == *header)
        })
        .collect();
    let dataset_result = json!({
        "state": if records.is_empty() { "unavailable" } else { "available" },
        "reason": if records.is_empty() { Value::from("source CSV contains no data rows") } else { Value::Null },
        "row_count": records.len(),
        "records": Value::Array(records.clone()),
        "field_coverage": Value::Object(field_coverage),
        "duplicate_summary": {
            "duplicate_groups": duplicate_groups,
            "duplicate_rows": duplicate_rows,
        },
        "row_errors": Value::Array(row_errors),
        "provenance": {
            "file": relative_name,
            "bytes": before.len(),
            "sha256": digest,
            "headers": headers,
            "unmapped_headers": unmapped,
        },
    });
    Ok((dataset_result, before.len() as i64))
}

/// Why reading one dataset stopped: an import-contract refusal named as such,
/// or a parse-level problem the caller wraps in the "cannot parse" wording.
#[derive(Debug)]
enum RowError {
    Import(String),
    Parse(String),
}

/// Parse the decoded CSV text: headers, then one record per data row with
/// per-field stats and errors collected alongside.
fn parse_csv_rows(
    fields: &[(&str, &str)],
    columns: &Map<String, Value>,
    text: &str,
    name: &str,
    limits: &ImportLimits,
) -> Result<ParsedDataset, RowError> {
    let mut reader = csv::ReaderBuilder::new()
        .has_headers(false)
        .flexible(true)
        .from_reader(text.as_bytes());
    let mut rows = reader.records();
    let headers: Vec<String> = match rows.next() {
        None => Vec::new(),
        Some(Err(error)) => return Err(RowError::Parse(error.to_string())),
        Some(Ok(record)) => record.iter().map(str::to_string).collect(),
    };
    if headers.is_empty() || headers.iter().any(String::is_empty) {
        return Err(RowError::Import(format!(
            "source CSV '{name}' requires non-empty headers"
        )));
    }
    if headers.iter().collect::<HashSet<_>>().len() != headers.len() {
        return Err(RowError::Import(format!(
            "source CSV '{name}' has duplicate headers"
        )));
    }
    let header_index: HashMap<&str, usize> = headers
        .iter()
        .enumerate()
        .map(|(i, header)| (header.as_str(), i))
        .collect();
    let mut stats: Vec<FieldStat> = vec![FieldStat::default(); fields.len()];
    let mut records: Vec<Value> = Vec::new();
    let mut row_errors: Vec<Value> = Vec::new();
    for (offset, row) in rows.enumerate() {
        let row_number = offset as i64 + 2;
        if offset + 1 > limits.max_rows_per_dataset {
            return Err(RowError::Import(format!(
                "source CSV '{name}' exceeds the {}-row dataset limit",
                limits.max_rows_per_dataset
            )));
        }
        let cells = row.map_err(|error| RowError::Parse(error.to_string()))?;
        if cells.len() > headers.len() {
            return Err(RowError::Import(format!(
                "source CSV '{name}' row {row_number} has more cells than its header"
            )));
        }
        let mut record = Map::new();
        record.insert("source_row".to_string(), json!(row_number));
        let mut invalid_fields: Vec<Value> = Vec::new();
        for (index, (field, kind)) in fields.iter().enumerate() {
            let stat = &mut stats[index];
            stat.rows += 1;
            let source_column = columns.get(*field).and_then(Value::as_str);
            let mapped = source_column.and_then(|column| header_index.get(column).copied());
            let Some(cell_index) = mapped else {
                record.insert((*field).to_string(), Value::Null);
                stat.missing += 1;
                continue;
            };
            let raw_value = cells.get(cell_index);
            let label = format!("{field} at row {row_number}");
            let (parsed, error) =
                parse_value(raw_value, kind, &label).map_err(|error| RowError::Import(error.0))?;
            match (error, parsed) {
                (Some(error), _) => {
                    let credential = error.contains("embedded URL credentials");
                    let kept = match (*kind, raw_value, credential) {
                        ("url", Some(raw), false) => Value::String(raw.to_string()),
                        _ => Value::Null,
                    };
                    record.insert((*field).to_string(), kept);
                    if *kind == "url" {
                        record.insert(url_key_name(field).to_string(), Value::Null);
                    } else if let Some(raw) = raw_value {
                        let unparsed = record
                            .entry("unparsed_values".to_string())
                            .or_insert_with(|| json!({}));
                        if let Some(map) = unparsed.as_object_mut() {
                            map.insert((*field).to_string(), Value::String(raw.to_string()));
                        }
                    }
                    stat.invalid += 1;
                    if raw_value.is_some_and(|raw| !raw.is_empty()) {
                        stat.present += 1;
                    }
                    invalid_fields.push(json!({"field": field, "reason": error}));
                }
                (None, parsed) => {
                    record.insert((*field).to_string(), parsed.clone().unwrap_or(Value::Null));
                    if *kind == "url" {
                        if let Some(Value::String(url)) = &parsed {
                            let key =
                                super::normalize_url_checked(url).map_err(RowError::Import)?;
                            record.insert(url_key_name(field).to_string(), Value::String(key));
                        }
                    }
                    match parsed {
                        None => stat.missing += 1,
                        Some(_) => stat.present += 1,
                    }
                }
            }
        }
        if !invalid_fields.is_empty() {
            record.insert(
                "field_errors".to_string(),
                Value::Array(invalid_fields.clone()),
            );
            row_errors.push(json!({"source_row": row_number, "fields": invalid_fields}));
        }
        records.push(Value::Object(record));
    }
    Ok((records, headers, stats, row_errors))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn statuses_columns() -> Map<String, Value> {
        serde_json::from_str(r#"{"url": "Address", "status_code": "Code"}"#).unwrap()
    }

    // skips blank lines; they must not become data rows or shift row numbers.
    #[test]
    fn blank_lines_are_not_data_rows() {
        let fields = DATASET_FIELDS
            .iter()
            .find(|(name, _)| *name == "statuses")
            .map(|(_, fields)| *fields)
            .unwrap();
        let (records, headers, _, _) = parse_csv_rows(
            fields,
            &statuses_columns(),
            "Address,Code\n\nhttps://a.test/,200\n\nhttps://b.test/,301\n",
            "s.csv",
            &ImportLimits::default(),
        )
        .unwrap();
        assert_eq!(headers, vec!["Address", "Code"]);
        assert_eq!(records.len(), 2);
        assert_eq!(records[1]["source_row"], serde_json::json!(3));
    }
}
