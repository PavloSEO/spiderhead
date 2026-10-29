//! Value coercion and case-insensitive column mapping for the exports.
//!
//! drift between crawler versions and configuration profiles; everything is
//! tolerant — a missing column yields `None`, never an exception.

use serde_json::{Map, Value};
use spiderhead_core::url_normalize;
use std::collections::HashMap;

use crate::pyvalue::py_float;
use crate::table::Table;

/// JSON object map shorthand.
pub type Object = Map<String, Value>;

/// `INTERNAL_FIELD_MAP`: canonical fields of the `Internal:All` export and
/// their candidate header names (first match wins, case-insensitive).
pub static INTERNAL_FIELD_MAP: &[(&str, &[&str])] = &[
    ("url", &["Address", "URL"]),
    ("content_type", &["Content Type"]),
    ("status_code", &["Status Code"]),
    ("status", &["Status"]),
    ("indexability", &["Indexability"]),
    ("indexability_status", &["Indexability Status"]),
    ("title", &["Title 1", "Title"]),
    ("title_length", &["Title 1 Length"]),
    ("title_px", &["Title 1 Pixel Width"]),
    (
        "meta_description",
        &["Meta Description 1", "Meta Description"],
    ),
    ("desc_length", &["Meta Description 1 Length"]),
    ("desc_px", &["Meta Description 1 Pixel Width"]),
    ("meta_keywords", &["Meta Keywords 1", "Meta Keywords"]),
    ("meta_description_count", &["Meta Description Count"]),
    ("h1", &["H1-1"]),
    ("h1_length", &["H1-1 Length"]),
    ("h1_2", &["H1-2"]),
    ("h1_alt_text", &["H1 Alt Text Only"]),
    ("h2", &["H2-1"]),
    ("heading_outline", &["Heading Outline"]),
    ("invalid_link_targets", &["Invalid Link Targets"]),
    ("link_placement", &["Link Placement"]),
    ("trust_signals", &["Trust Signals"]),
    ("duplicate_ids", &["Duplicate IDs"]),
    ("h2_2", &["H2-2"]),
    ("meta_robots", &["Meta Robots 1"]),
    ("x_robots", &["X-Robots-Tag 1"]),
    (
        "canonical",
        &["Canonical Link Element 1", "Canonical Link Element"],
    ),
    ("canonical_2", &["Canonical Link Element 2"]),
    ("meta_refresh", &["Meta Refresh 1"]),
    ("amphtml", &["amphtml Link Element", "AMP HTML"]),
    ("rel_next", &["rel=\"next\" 1", "rel=next 1"]),
    ("rel_prev", &["rel=\"prev\" 1", "rel=prev 1"]),
    ("rel_next_2", &["rel=\"next\" 2", "rel=next 2"]),
    ("rel_prev_2", &["rel=\"prev\" 2", "rel=prev 2"]),
    ("size_bytes", &["Size (bytes)", "Size (Bytes)", "Size"]),
    (
        "transferred_bytes",
        &["Transferred (bytes)", "Transferred (Bytes)"],
    ),
    ("word_count", &["Word Count"]),
    ("content_frames", &["Content Frames"]),
    (
        "content_frames_same_origin",
        &["Content Frames Same-Origin"],
    ),
    ("sentence_count", &["Sentence Count"]),
    ("avg_words_per_sentence", &["Average Words Per Sentence"]),
    ("flesch", &["Flesch Reading Ease Score"]),
    ("readability", &["Readability"]),
    ("http_version", &["HTTP Version"]),
    ("cookies", &["Cookies"]),
    ("text_ratio", &["Text Ratio"]),
    ("crawl_depth", &["Crawl Depth"]),
    ("folder_depth", &["Folder Depth"]),
    ("link_score", &["Link Score"]),
    ("inlinks", &["Inlinks"]),
    ("unique_inlinks", &["Unique Inlinks"]),
    ("outlinks", &["Outlinks"]),
    ("unique_outlinks", &["Unique Outlinks"]),
    ("external_outlinks", &["External Outlinks"]),
    ("closest_similarity", &["Closest Similarity Match"]),
    ("near_duplicates", &["No. Near Duplicates"]),
    ("hash", &["Hash", "Page Hash"]),
    ("response_time", &["Response Time"]),
    ("last_modified", &["Last Modified"]),
    ("redirect_url", &["Redirect URL", "Redirect URI"]),
    ("redirect_type", &["Redirect Type"]),
    ("hreflang", &["Hreflang"]),
    ("structured_data", &["Structured Data"]),
    ("structured_data_parsed", &["Structured Data Parsed"]),
    ("validation_errors", &["Validation Errors"]),
    ("og_title", &["OG:Title"]),
    ("og_description", &["OG:Description"]),
    ("og_image", &["OG:Image", "OG:Image URL"]),
    ("og_url", &["OG:URL"]),
    ("spelling_errors", &["Spelling Errors"]),
    ("grammar_errors", &["Grammar Errors"]),
    (
        "content_encoding",
        &["Content-Encoding", "Content Encoding"],
    ),
    ("render_blocking", &["Render Blocking Resources"]),
    ("meta_charset", &["Meta Charset", "Charset"]),
    ("doctype", &["Doctype", "Doctype Declaration"]),
    (
        "viewport",
        &["Viewport", "Meta Viewport", "Mobile Viewport"],
    ),
    ("representation", &["Representation"]),
    ("body_unavailable", &["Body Unavailable"]),
    ("title_outside_head", &["Title Outside Head"]),
    (
        "meta_description_outside_head",
        &["Meta Description Outside Head"],
    ),
    ("canonical_outside_head", &["Canonical Outside Head"]),
    ("directives_outside_head", &["Directives Outside Head"]),
    ("hreflang_outside_head", &["Hreflang Outside Head"]),
    ("head_count", &["Head Count"]),
    ("body_count", &["Body Count"]),
    ("head_not_first", &["Head Not First"]),
    ("invalid_head_elements", &["Invalid Head Elements"]),
    ("http_refresh", &["Refresh"]),
    ("images_total", &["Images Total"]),
    ("images_missing_alt_attr", &["Images Missing Alt Attribute"]),
    ("images_max_alt_length", &["Images Max Alt Length"]),
    ("lorem_ipsum_count", &["Lorem Ipsum Occurrences"]),
    ("plugin_elements", &["Unsupported Plugin Elements"]),
    ("mobile_alternate_broken", &["Mobile Alternate Broken"]),
    ("meta_fragment", &["Meta Fragment"]),
    ("ajax_scheme_outlinks", &["AJAX Scheme Outlinks"]),
];

/// `INLINKS_FIELD_MAP`: canonical fields of a `*:Inlinks` bulk export.
pub static INLINKS_FIELD_MAP: &[(&str, &[&str])] = &[
    ("type", &["Type"]),
    ("source_url", &["Source", "From"]),
    ("destination_url", &["Destination", "To"]),
    ("anchor", &["Anchor Text", "Anchor"]),
    ("alt_text", &["Alt Text"]),
    ("status_code", &["Status Code"]),
    ("status", &["Status"]),
    ("follow", &["Follow"]),
    ("rel", &["Rel"]),
    ("target", &["Target"]),
    ("link_position", &["Link Position"]),
    ("link_path", &["Link Path"]),
    ("link_origin", &["Link Origin"]),
];

/// `HREFLANG_FIELD_MAP`: canonical fields of the bulk `All Hreflang` report.
pub static HREFLANG_FIELD_MAP: &[(&str, &[&str])] = &[
    ("source_url", &["Source", "From", "Address"]),
    (
        "destination_url",
        &["Destination", "To", "Hreflang URL", "Hreflang Target URL"],
    ),
    (
        "hreflang",
        &["Hreflang", "Hreflang Language", "Language", "Lang"],
    ),
];

/// Fields coerced to integers (reference `INT_FIELDS`).
pub static INT_FIELDS: &[&str] = &[
    "status_code",
    "title_length",
    "title_px",
    "desc_length",
    "desc_px",
    "h1_length",
    "size_bytes",
    "transferred_bytes",
    "word_count",
    "content_frames",
    "content_frames_same_origin",
    "sentence_count",
    "crawl_depth",
    "folder_depth",
    "inlinks",
    "unique_inlinks",
    "outlinks",
    "unique_outlinks",
    "external_outlinks",
    "near_duplicates",
    "validation_errors",
    "spelling_errors",
    "grammar_errors",
    "head_count",
    "body_count",
    "meta_description_count",
    "structured_data_parsed",
    "images_total",
    "images_missing_alt_attr",
    "images_max_alt_length",
    "lorem_ipsum_count",
    "plugin_elements",
    "mobile_alternate_broken",
    "ajax_scheme_outlinks",
];

/// Fields coerced to floats (reference `FLOAT_FIELDS`).
pub static FLOAT_FIELDS: &[&str] = &[
    "text_ratio",
    "link_score",
    "response_time",
    "closest_similarity",
    "flesch",
    "avg_words_per_sentence",
];

fn int_field(name: &str) -> bool {
    INT_FIELDS.contains(&name)
}

fn float_field(name: &str) -> bool {
    FLOAT_FIELDS.contains(&name)
}

/// `_comma_to_dot`: rewrite a lone comma-decimal string (`2,500`) to dot-decimal;
/// anything with a dot already or with several commas is left alone.
pub fn comma_to_dot(value: &str) -> String {
    if is_comma_decimal(value) {
        value.replace(',', ".")
    } else {
        value.to_owned()
    }
}

fn is_comma_decimal(value: &str) -> bool {
    // ^-?\d+,\d+$ — one comma between digit runs, nothing else.
    let Some((head, tail)) = value.split_once(',') else {
        return false;
    };
    if tail.contains(',') {
        return false;
    }
    let head = head.strip_prefix('-').unwrap_or(head);
    !head.is_empty()
        && head.bytes().all(|b| b.is_ascii_digit())
        && !tail.is_empty()
        && tail.bytes().all(|b| b.is_ascii_digit())
}

fn py_strip(s: &str) -> &str {
    spiderhead_parse::py_str::py_strip(s)
}

/// `normalize_value`: empty strings and NaN -> `Null`; strings are stripped;
/// numbers, booleans and containers pass through.
pub fn normalize_value(value: &Value) -> Value {
    match value {
        Value::Null => Value::Null,
        Value::Number(n) if n.as_f64().is_some_and(|f| !f.is_finite()) => Value::Null,
        Value::String(s) => {
            let cleaned = py_strip(s);
            if cleaned.is_empty() {
                Value::Null
            } else {
                Value::String(cleaned.to_owned())
            }
        }
        other => other.clone(),
    }
}

/// `to_float`: normalize, rewrite comma decimals, parse with reference `float()`
/// semantics, keep only finite values.
pub fn to_float(value: &Value) -> Option<f64> {
    let value = normalize_value(value);
    let f = match &value {
        Value::Null => return None,
        Value::Bool(b) => {
            if *b {
                1.0
            } else {
                0.0
            }
        }
        Value::Number(n) => n.as_f64()?,
        Value::String(s) => py_float(&comma_to_dot(s))?,
        _ => return None,
    };
    if f.is_finite() {
        Some(f)
    } else {
        None
    }
}

/// `to_int`: reference `int(float(...))` — truncation toward zero. `i64`
/// saturates where the reference bigint would not; no export field reaches that.
pub fn to_int(value: &Value) -> Option<i64> {
    to_float(value).map(|f| f.trunc() as i64)
}

/// `is_true`: numbers count `!= 0`; strings match `true`/`yes`/`1`/`y`
/// case-insensitively; anything else is false.
pub fn is_true(value: &Value) -> bool {
    let value = normalize_value(value);
    match &value {
        Value::Null => false,
        Value::Bool(b) => *b,
        Value::Number(n) => n.as_f64().is_some_and(|f| f != 0.0),
        Value::String(s) => {
            matches!(
                py_strip(s).to_lowercase().as_str(),
                "true" | "yes" | "1" | "y"
            )
        }
        // str(container) is repr(...) — never one of the true words.
        _ => false,
    }
}

/// `norm_url`: strip, lowercase scheme and host, drop trailing path slashes.
pub fn norm_url(url: Option<&str>) -> Result<String, url_normalize::NormUrlError> {
    url_normalize::norm_url(url)
}

/// Lower-cased column lookup: `{str(col).lower(): col}` — a later column
/// shadows an earlier one with the same folded name (dict-comprehension
/// overwrite order).
fn lower_columns(columns: &[String]) -> HashMap<String, &str> {
    let mut map = HashMap::with_capacity(columns.len());
    for c in columns {
        map.insert(c.to_lowercase(), c.as_str());
    }
    map
}

/// `find_column`: the actual column matching any candidate, case-insensitively.
pub fn find_column<'c, S: AsRef<str>>(columns: &'c [String], candidates: &[S]) -> Option<&'c str> {
    let lower = lower_columns(columns);
    candidates
        .iter()
        .find_map(|c| lower.get(&c.as_ref().to_lowercase()).copied())
}

/// `resolve_columns`: each canonical field mapped to its actual source column
/// name (`None` when absent), resolved once per frame.
pub fn resolve_columns<'c, K, V, S>(
    columns: &'c [String],
    field_map: &[(K, V)],
) -> Vec<(String, Option<&'c str>)>
where
    K: AsRef<str>,
    V: AsRef<[S]>,
    S: AsRef<str>,
{
    let lower = lower_columns(columns);
    field_map
        .iter()
        .map(|(field, candidates)| {
            let hit = candidates
                .as_ref()
                .iter()
                .find_map(|c| lower.get(&c.as_ref().to_lowercase()).copied());
            (field.as_ref().to_owned(), hit)
        })
        .collect()
}

/// `record_from_mapping`: normalize one mapping row into a canonical record.
pub fn record_from_mapping<K, V, S>(row: &Object, field_map: &[(K, V)]) -> Object
where
    K: AsRef<str>,
    V: AsRef<[S]>,
    S: AsRef<str>,
{
    let columns: Vec<String> = row.keys().cloned().collect();
    let resolved = resolve_columns(&columns, field_map);
    let mut record = Object::new();
    for (field, col) in resolved {
        let raw = col.and_then(|c| row.get(c)).cloned().unwrap_or(Value::Null);
        let value = if int_field(&field) {
            to_int(&raw).map_or(Value::Null, Value::from)
        } else if float_field(&field) {
            to_float(&raw).map_or(Value::Null, Value::from)
        } else {
            normalize_value(&raw)
        };
        record.insert(field, value);
    }
    record
}

/// `row_to_record`: the single-row projection (same as `record_from_mapping`).
pub fn row_to_record<K, V, S>(row: &Object, field_map: &[(K, V)]) -> Object
where
    K: AsRef<str>,
    V: AsRef<[S]>,
    S: AsRef<str>,
{
    record_from_mapping(row, field_map)
}

/// `iter_records_from_df`: normalized records row by row (streaming path —
/// resolves columns per row exactly as the reference generator does).
pub fn iter_records_from_df<'t, K, V, S>(
    table: &'t Table,
    field_map: &'t [(K, V)],
) -> impl Iterator<Item = Object> + 't
where
    K: AsRef<str>,
    V: AsRef<[S]>,
    S: AsRef<str>,
{
    table.rows().enumerate().map(move |(i, _)| {
        let map = table.row_map(i).unwrap_or_default();
        record_from_mapping(&map, field_map)
    })
}

/// `pd.to_numeric(errors="coerce")` on one cell: pandas accepts what reference
/// `float()` does except digit separators (`"1_000"` -> NaN). Integer tokens
/// produce `i64` candidates; anything else parseable yields a float.
#[derive(Clone, Copy)]
enum Coerced {
    /// Integer token or boolean — becomes `int64` when the whole column is
    /// integer-parseable with no misses.
    Int(i64),
    /// Float token or a number that did not fit `i64`.
    Float(f64),
}

fn pd_numeric(text: &str) -> Option<Coerced> {
    let s = py_strip(text);
    let b = s.as_bytes();
    let mut i = 0usize;
    if matches!(b.get(i), Some(b'+') | Some(b'-')) {
        i += 1;
    }
    let rest = &s[i..];
    let lower = rest.to_lowercase();
    if matches!(lower.as_str(), "nan" | "inf" | "infinity") {
        return Some(Coerced::Float(if s.starts_with('-') && lower != "nan" {
            f64::NEG_INFINITY
        } else if lower == "nan" {
            f64::NAN
        } else {
            f64::INFINITY
        }));
    }
    // digits [.digits] [e[+-]digits] | .digits [e[+-]digits] — no underscores.
    let rb = rest.as_bytes();
    let mut j = 0usize;
    while rb.get(j).is_some_and(u8::is_ascii_digit) {
        j += 1;
    }
    let int_digits = j;
    let mut saw_dot = false;
    if rb.get(j) == Some(&b'.') {
        saw_dot = true;
        j += 1;
        while rb.get(j).is_some_and(u8::is_ascii_digit) {
            j += 1;
        }
    }
    if int_digits == 0 && !saw_dot {
        return None;
    }
    if int_digits == 0 && saw_dot && j == 1 {
        return None; // "." with no digits on either side
    }
    let mut saw_exp = false;
    if matches!(rb.get(j), Some(b'e') | Some(b'E')) {
        saw_exp = true;
        j += 1;
        if matches!(rb.get(j), Some(b'+') | Some(b'-')) {
            j += 1;
        }
        let exp_start = j;
        while rb.get(j).is_some_and(u8::is_ascii_digit) {
            j += 1;
        }
        if j == exp_start {
            return None;
        }
    }
    if j != rb.len() {
        return None;
    }
    if !saw_dot && !saw_exp {
        if let Ok(v) = rest.parse::<i64>() {
            return Some(Coerced::Int(v));
        }
    }
    rest.parse::<f64>().ok().map(Coerced::Float)
}

/// `pd.to_numeric` on a single value: the numeric value when coercible and
/// finite, else `None` (underscores and non-numeric tokens are misses).
pub fn pd_coerce_f64(raw: &Value) -> Option<f64> {
    match coerce_cell(raw) {
        Some(Coerced::Int(i)) => Some(i as f64),
        Some(Coerced::Float(f)) if f.is_finite() => Some(f),
        _ => None,
    }
}

/// Coerce one raw cell the way `pd.to_numeric` coerces a Series element.
fn coerce_cell(raw: &Value) -> Option<Coerced> {
    match raw {
        Value::Bool(b) => Some(Coerced::Int(if *b { 1 } else { 0 })),
        Value::Number(n) => {
            if let Some(i) = n.as_i64() {
                Some(Coerced::Int(i))
            } else if let Some(u) = n.as_u64() {
                Some(match i64::try_from(u) {
                    Ok(i) => Coerced::Int(i),
                    Err(_) => Coerced::Float(u as f64),
                })
            } else {
                n.as_f64().map(Coerced::Float)
            }
        }
        Value::String(s) => pd_numeric(&comma_to_dot(s)),
        _ => None,
    }
}

/// `records_from_df`: the vectorized projection — columns resolved once and
/// numeric fields coerced column-wise like `pd.to_numeric`: a column becomes
/// `float64` if any cell is a float token or a miss, otherwise `int64`.
/// Int fields are truncated to `int` either way; float fields emit an int
/// only when the whole column is integer-typed.
pub fn records_from_df<K, V, S>(table: &Table, field_map: &[(K, V)]) -> Vec<Object>
where
    K: AsRef<str>,
    V: AsRef<[S]>,
    S: AsRef<str>,
{
    let resolved = resolve_columns(table.columns(), field_map);
    let resolved_idx: Vec<(String, Option<usize>)> = resolved
        .iter()
        .map(|(field, col)| (field.clone(), col.and_then(|c| table.column_index(c))))
        .collect();
    // Per numeric field: coerced cells + whether the column came out float64.
    let coerced: Vec<Option<(Vec<Option<Coerced>>, bool)>> = resolved_idx
        .iter()
        .map(|(field, idx)| {
            if !(int_field(field) || float_field(field)) {
                return None;
            }
            let cells: Vec<Option<Coerced>> = table
                .rows()
                .map(|row| {
                    idx.and_then(|i| row.get(i))
                        .map(coerce_cell)
                        .unwrap_or(None)
                })
                .collect();
            let float_col = cells.iter().any(|c| !matches!(c, Some(Coerced::Int(_))));
            Some((cells, float_col))
        })
        .collect();
    table
        .rows()
        .enumerate()
        .map(|(r, row)| {
            let mut record = Object::new();
            for ((field, idx), coercion) in resolved_idx.iter().zip(&coerced) {
                let value = match coercion {
                    Some((cells, float_col)) => {
                        let cell = cells.get(r).copied().flatten();
                        if int_field(field) {
                            match cell {
                                Some(Coerced::Int(i)) => Value::from(i),
                                Some(Coerced::Float(f)) if f.is_finite() => {
                                    Value::from(f.trunc() as i64)
                                }
                                _ => Value::Null,
                            }
                        } else {
                            match cell {
                                Some(Coerced::Int(i)) if !float_col => Value::from(i),
                                Some(Coerced::Int(i)) => Value::from(i as f64),
                                Some(Coerced::Float(f)) if f.is_finite() => Value::from(f),
                                _ => Value::Null,
                            }
                        }
                    }
                    None => {
                        let raw = idx.and_then(|i| row.get(i)).cloned().unwrap_or(Value::Null);
                        normalize_value(&raw)
                    }
                };
                record.insert(field.clone(), value);
            }
            record
        })
        .collect()
}
