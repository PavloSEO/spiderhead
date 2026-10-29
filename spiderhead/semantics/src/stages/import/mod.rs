//! `import` — preview and import CSV/XLSX phrase data with explicit field
//! mappings.

use crate::config::Cfg;
use crate::demand::has_observed_demand;
use crate::norm::normalize;
use crate::pyfmt::{py_int, py_str};
use crate::store::{zulu_seconds, Store};
use calamine::Reader;
use indexmap::IndexMap;
use serde_json::{json, Map, Value};
use std::collections::BTreeMap;
use std::path::Path;

mod path_is_file;
pub use path_is_file::*;

/// Truthiness of cell values: null, false, 0/0.0 and "" are falsy.
fn py_truthy(v: &Value) -> bool {
    !(v.is_null()
        || v.as_bool() == Some(false)
        || v.as_i64() == Some(0)
        || v.as_f64() == Some(0.0)
        || v.as_str() == Some(""))
}

const FIELDS: [&str; 6] = ["query", "frequency", "group", "subgroup", "url", "comment"];
const MAX_IMPORT_BYTES: u64 = 32 * 1024 * 1024;
const MAX_IMPORT_ROWS: usize = 100_000;
const MAX_IMPORT_COLUMNS: usize = 128;
const MAX_IMPORT_CELL_BYTES: usize = 16_384;
const MAX_IMPORT_TEXT_BYTES: usize = 16 * 1024 * 1024;

fn defaults(field: &str) -> &'static [&'static str] {
    match field {
        "query" => &["norm", "phrase", "query", "фраза", "запрос"],
        "frequency" => &["frequency", "base", "частотность", "частота"],
        "group" => &["group", "category", "категория", "группа", "cluster"],
        "subgroup" => &["subgroup", "подгруппа"],
        "url" => &["url", "landing", "landing_url", "страница"],
        "comment" => &["comment", "комментарий", "примечание"],
        "impr" => &["impr", "impressions", "показы"],
        "pos_y" => &["pos_y", "yandex_pos", "позиция_яндекс"],
        "pos_g" => &["pos_g", "google_pos", "позиция_google"],
        _ => &[],
    }
}

fn err(msg: impl Into<String>) -> crate::Error {
    crate::Error::Config(msg.into())
}

/// Width checks plus byte, row and column caps; every row padded
/// to the header width with nulls.
fn collect_rows(
    source_rows: &[Vec<Value>],
    headers: &[String],
    source: &str,
    strict_width: bool,
) -> Result<Vec<CellRow>, crate::Error> {
    if headers.is_empty() || headers.len() > MAX_IMPORT_COLUMNS {
        return Err(err(format!(
            "import must have between 1 and {MAX_IMPORT_COLUMNS} columns"
        )));
    }
    let mut rows = Vec::new();
    let mut text_bytes = 0usize;
    for header in headers {
        let size = header.len();
        if size > MAX_IMPORT_CELL_BYTES {
            return Err(err(format!(
                "column header exceeds the {MAX_IMPORT_CELL_BYTES}-byte cell limit"
            )));
        }
        text_bytes += size;
    }
    if text_bytes > MAX_IMPORT_TEXT_BYTES {
        return Err(err(format!(
            "import exceeds the {MAX_IMPORT_TEXT_BYTES}-byte text limit"
        )));
    }
    for (i, raw) in source_rows.iter().enumerate() {
        let line = i + 2;
        if line > MAX_IMPORT_ROWS + 1 {
            return Err(err(format!(
                "import exceeds the {MAX_IMPORT_ROWS}-row limit"
            )));
        }
        if raw.len() > headers.len() || (strict_width && raw.len() < headers.len()) {
            return Err(err(format!("wrong column count on {source} row {line}")));
        }
        let mut values = raw.clone();
        values.resize(headers.len(), Value::Null);
        for (header, value) in headers.iter().zip(&values) {
            if header.is_empty() && !(value.is_null() || value.as_str() == Some("")) {
                return Err(err(format!(
                    "non-empty cell has no column header on {source} row {line}"
                )));
            }
        }
        for value in &values {
            if value.is_null() {
                continue;
            }
            let size = py_str(value).len();
            if size > MAX_IMPORT_CELL_BYTES {
                return Err(err(format!(
                    "cell exceeds the {MAX_IMPORT_CELL_BYTES}-byte limit on {source} row {line}"
                )));
            }
            text_bytes += size;
            if text_bytes > MAX_IMPORT_TEXT_BYTES {
                return Err(err(format!(
                    "import exceeds the {MAX_IMPORT_TEXT_BYTES}-byte text limit"
                )));
            }
        }
        let row: IndexMap<String, Value> = headers.iter().cloned().zip(values).collect();
        rows.push(row);
    }
    Ok(rows)
}

/// Delimiter guess: pick the character that occurs most often in the
/// first sample line; default to comma when nothing wins.
fn sniff_delimiter(sample: &str) -> u8 {
    let line = sample.lines().next().unwrap_or("");
    let mut best = (b',', 0usize);
    for cand in *b",;\t" {
        let n = line.as_bytes().iter().filter(|&&b| b == cand).count();
        if n > best.1 {
            best = (cand, n);
        }
    }
    best.0
}

fn data_to_value(d: &calamine::Data) -> Value {
    use calamine::Data;
    match d {
        Data::Int(v) => json!(v),
        Data::Float(v) => json!(v),
        Data::String(s) => json!(s),
        Data::Bool(b) => json!(b),
        Data::Error(_) | Data::Empty => Value::Null,
        other => json!(other.to_string()),
    }
}

type CellRow = IndexMap<String, Value>;
type ParsedRows = (Vec<Value>, IndexMap<String, String>, usize);

/// Read a worksheet or CSV with BOM handling, header checks and the
/// row collector.
fn read_table(
    path: &Path,
    sheet: Option<&str>,
) -> Result<(Vec<String>, Vec<CellRow>), crate::Error> {
    let ext = path
        .extension()
        .map(|e| e.to_string_lossy().to_lowercase())
        .unwrap_or_default();
    if ext == "xlsx" || ext == "xlsm" {
        let mut workbook: calamine::Xlsx<_> =
            calamine::open_workbook(path).map_err(|e| err(format!("cannot open workbook: {e}")))?;
        let names = workbook.sheet_names().to_vec();
        let title = match sheet {
            Some(name) => {
                if !names.iter().any(|n| n == name) {
                    return Err(err(format!("worksheet not found: {name}")));
                }
                name.to_string()
            }
            None => names.first().cloned().unwrap_or_default(),
        };
        let range = workbook
            .worksheet_range(&title)
            .map_err(|e| err(format!("cannot read worksheet {title}: {e}")))?;
        let mut it = range.rows();
        let header_row = it
            .next()
            .ok_or_else(|| err("worksheet needs a header row"))?;
        let headers: Vec<String> = header_row
            .iter()
            .map(|d| {
                let v = data_to_value(d);
                if v.is_null() {
                    String::new()
                } else {
                    py_str(&v).trim().to_string()
                }
            })
            .collect();
        let folded: std::collections::HashSet<String> = headers
            .iter()
            .filter(|h| !h.is_empty())
            .map(|h| h.to_lowercase())
            .collect();
        if folded.len() != headers.iter().filter(|h| !h.is_empty()).count() {
            return Err(err("worksheet headers must be unique"));
        }
        // The row cap must bite while rows stream in, not after the whole
        // worksheet is materialized: the 32 MiB file cap does not bound the
        // decompressed row set.
        let source_rows: Vec<Vec<Value>> = it
            .take(MAX_IMPORT_ROWS + 1)
            .map(|r| r.iter().map(data_to_value).collect())
            .collect();
        let rows = collect_rows(&source_rows, &headers, &format!("worksheet {title}"), false)?;
        return Ok((headers, rows));
    }
    if sheet.is_some() {
        return Err(err("--sheet is available only for .xlsx and .xlsm imports"));
    }
    let bytes = std::fs::read(path).map_err(|e| err(format!("cannot open import file: {e}")))?;
    let text =
        String::from_utf8(bytes).map_err(|e| err(format!("import file is not UTF-8: {e}")))?;
    let text = text.strip_prefix('\u{feff}').unwrap_or(&text);
    let sample: String = text.chars().take(4096).collect();
    let mut reader = csv::ReaderBuilder::new()
        .delimiter(sniff_delimiter(&sample))
        .flexible(true)
        .has_headers(true)
        .from_reader(text.as_bytes());
    let headers: Vec<String> = reader
        .headers()
        .map_err(|e| err(format!("import file needs a header row ({e})")))?
        .iter()
        .map(|h| h.trim().to_string())
        .collect();
    if headers.iter().all(|h| h.is_empty()) {
        return Err(err("import file needs a header row"));
    }
    let folded: std::collections::HashSet<String> = headers
        .iter()
        .filter(|h| !h.is_empty())
        .map(|h| h.to_lowercase())
        .collect();
    if folded.len() != headers.iter().filter(|h| !h.is_empty()).count() {
        return Err(err("CSV headers must be unique"));
    }
    let mut source_rows: Vec<Vec<Value>> = Vec::new();
    for rec in reader.records() {
        let rec = rec.map_err(|e| err(format!("malformed CSV: {e}")))?;
        source_rows.push(rec.iter().map(|c| json!(c)).collect());
    }
    let rows = collect_rows(&source_rows, &headers, "CSV", true)?;
    Ok((headers, rows))
}

/// Explicit per-field columns first, then alias lookup
/// (case-insensitive); `query` is mandatory.
fn mapping_for(
    headers: &[String],
    mapping: Option<&Map<String, Value>>,
) -> Result<IndexMap<String, String>, crate::Error> {
    let mapping = mapping.cloned().unwrap_or_default();
    let mut unknown: Vec<String> = mapping
        .keys()
        .filter(|k| {
            !FIELDS.contains(&k.as_str())
                && !["base", "quoted", "exact", "impr", "pos_y", "pos_g"].contains(&k.as_str())
        })
        .cloned()
        .collect();
    unknown.sort();
    if !unknown.is_empty() {
        return Err(err(format!(
            "unsupported import mapping fields: {}",
            unknown.join(", ")
        )));
    }
    let mut result = IndexMap::new();
    let lower: BTreeMap<String, String> = headers
        .iter()
        .filter(|h| !h.is_empty())
        .map(|h| (h.to_lowercase(), h.clone()))
        .collect();
    for field in FIELDS
        .iter()
        .copied()
        .chain(["quoted", "exact", "impr", "pos_y", "pos_g"])
    {
        if let Some(requested) = mapping.get(field).and_then(Value::as_str) {
            if !requested.is_empty() {
                if !headers.iter().any(|h| h == requested) {
                    return Err(err(format!("column for {field} not found: {requested}")));
                }
                result.insert(field.to_string(), requested.to_string());
                continue;
            }
        }
        let found = defaults(field)
            .iter()
            .find_map(|alias| lower.get(&alias.to_lowercase()))
            .cloned();
        if let Some(found) = found.filter(|f| !f.is_empty()) {
            result.insert(field.to_string(), found);
        }
    }
    if let Some(base_col) = mapping.get("base").and_then(Value::as_str) {
        if !headers.iter().any(|h| h == base_col) {
            return Err(err(format!("column for base not found: {base_col}")));
        }
        result.insert("frequency".into(), base_col.to_string());
    }
    if !result.contains_key("query") {
        return Err(err(
            "import needs an explicit query mapping or a norm/phrase/query column",
        ));
    }
    Ok(result)
}

/// Integer value of a cell: bools and non-integral floats are rejected by
/// the caller; strings go through `py_int`.
fn cell_int(value: &Value) -> Result<i64, ()> {
    match value {
        Value::Bool(_) | Value::Null => Err(()),
        Value::Number(n) => {
            if let Some(i) = n.as_i64() {
                Ok(i)
            } else if let Some(f) = n.as_f64() {
                if f.fract() == 0.0 {
                    Ok(f as i64)
                } else {
                    Err(())
                }
            } else {
                Err(())
            }
        }
        Value::String(s) => py_int(s).map_err(|_| ()),
        _ => Err(()),
    }
}

/// Parse a file into normalized, unique rows with typed values.
fn parse_file(
    file: &Path,
    sheet: Option<&str>,
    mapping: Option<&Map<String, Value>>,
) -> Result<ParsedRows, crate::Error> {
    if !path_is_file(file) {
        return Err(err(format!("import file not found: {}", file.display())));
    }
    let size = std::fs::metadata(file)
        .map_err(|e| err(format!("cannot stat import file: {e}")))?
        .len();
    if size > MAX_IMPORT_BYTES {
        return Err(err(format!(
            "import file exceeds the {MAX_IMPORT_BYTES}-byte limit"
        )));
    }
    let (headers, source_rows) = read_table(file, sheet)?;
    let applied = mapping_for(&headers, mapping)?;
    let mut parsed: Vec<Value> = Vec::new();
    let mut seen = std::collections::HashSet::new();
    for (i, row) in source_rows.iter().enumerate() {
        let line = i + 2;
        let raw = applied
            .get("query")
            .and_then(|c| row.get(c))
            .cloned()
            .unwrap_or(Value::Null);
        let phrase = if raw.is_null() {
            String::new()
        } else {
            normalize(&py_str(&raw))
        };
        if phrase.is_empty() {
            continue;
        }
        let mut values = IndexMap::new();
        for (source_field, target_field) in [
            ("frequency", "base"),
            ("quoted", "quoted"),
            ("exact", "exact"),
            ("impr", "impr"),
            ("pos_y", "pos_y"),
            ("pos_g", "pos_g"),
        ] {
            let column = applied.get(source_field);
            let value = column
                .and_then(|c| row.get(c))
                .cloned()
                .unwrap_or(Value::Null);
            let emptyish = value.is_null()
                || ["", "null", "--", "none"]
                    .contains(&py_str(&value).trim().to_lowercase().as_str());
            if emptyish {
                continue;
            }
            if value.is_boolean()
                || (value.is_number() && value.as_f64().is_some_and(|f| f.fract() != 0.0))
            {
                return Err(err(format!("invalid {source_field} on input row {line}")));
            }
            let number = cell_int(&value)
                .map_err(|_| err(format!("invalid {source_field} on input row {line}")))?;
            if number < 0 {
                return Err(err(format!("negative {source_field} on input row {line}")));
            }
            values.insert(target_field.to_string(), json!(number));
        }
        // Falsy cells become an empty string.
        let mut group = applied
            .get("group")
            .and_then(|c| row.get(c))
            .map(|v| {
                if py_truthy(v) {
                    py_str(v).trim().to_string()
                } else {
                    String::new()
                }
            })
            .unwrap_or_default();
        let mut subgroup = applied
            .get("subgroup")
            .and_then(|c| row.get(c))
            .map(|v| {
                if py_truthy(v) {
                    py_str(v).trim().to_string()
                } else {
                    String::new()
                }
            })
            .unwrap_or_default();
        if !group.is_empty() && subgroup.is_empty() && (group.contains('/') || group.contains('\\'))
        {
            let parts: Vec<String> = group
                .replace('\\', "/")
                .split('/')
                .map(|p| p.trim().to_string())
                .filter(|p| !p.is_empty())
                .collect();
            if parts.is_empty() {
                group = String::new();
            } else {
                group = parts[0].clone();
                subgroup = parts[1..].join(" / ");
            }
        }
        for (field, column) in [
            ("group_name", None),
            ("subgroup", None),
            ("url", Some("url")),
            ("comment", Some("comment")),
        ] {
            // url/comment use `row.get(col)` directly — `0` is a real value
            // there, unlike the `or ""` group/subgroup read.
            let value: Option<String> = match field {
                "group_name" => Some(group.clone()),
                "subgroup" => Some(subgroup.clone()),
                _ => column
                    .and_then(|col| applied.get(col))
                    .and_then(|c| row.get(c))
                    .filter(|v| !v.is_null())
                    .map(py_str),
            };
            if let Some(v) = value {
                let v = v.trim().to_string();
                if !v.is_empty() {
                    values.insert(field.to_string(), json!(v));
                }
            }
        }
        let dup = seen.contains(&phrase);
        seen.insert(phrase.clone());
        parsed.push(json!({
            "query": phrase,
            "values": Value::Object(values.into_iter().collect()),
            "duplicate_in_file": dup,
        }));
    }
    let mut deduped: IndexMap<String, Value> = IndexMap::new();
    for item in parsed.iter() {
        let q = item["query"].as_str().unwrap_or_default().to_string();
        let new_values = item["values"].as_object().cloned().unwrap_or_default();
        match deduped.get_mut(&q) {
            None => {
                deduped.insert(q, item.clone());
            }
            Some(existing) => {
                if let Some(obj) = existing["values"].as_object_mut() {
                    for (k, v) in new_values {
                        obj.insert(k, v);
                    }
                }
            }
        }
    }
    let rows: Vec<Value> = deduped.into_values().collect();
    Ok((rows, applied, parsed.len()))
}
