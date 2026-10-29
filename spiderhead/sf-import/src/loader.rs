//! Discover and load crawler exports from a directory.
//!
//! keys by filename tokens; reading is encoding-tolerant (UTF-8-with-BOM,
//! UTF-16, sometimes XLSX or a single-byte legacy encoding).

use encoding_rs::{Encoding, UTF_16BE, UTF_16LE, UTF_8, WINDOWS_1250, WINDOWS_1251, WINDOWS_1252};
use serde_json::Value;
use spiderhead_parse::py_str::py_repr;
use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};
use thiserror::Error;

use crate::table::Table;

/// One filename matcher: every `all` token must appear in the normalized
/// filename; no `none` token may appear.
pub struct Matcher {
    /// All tokens (AND). An empty list never matches.
    pub all: &'static [&'static str],
    /// Forbidden tokens.
    pub none: &'static [&'static str],
}

/// `EXPORT_MATCHERS`: logical export key -> filename token matchers.
/// Iteration order is significant — `found`/`missing` lists follow it.
pub static EXPORT_MATCHERS: &[(&str, Matcher)] = &[
    (
        "internal_all",
        Matcher {
            all: &["internal", "all"],
            none: &["inlinks", "outlinks"],
        },
    ),
    (
        "resp_4xx",
        Matcher {
            all: &["4xx"],
            none: &["inlinks"],
        },
    ),
    (
        "resp_5xx",
        Matcher {
            all: &["5xx"],
            none: &["inlinks"],
        },
    ),
    (
        "resp_3xx",
        Matcher {
            all: &["3xx"],
            none: &["inlinks"],
        },
    ),
    (
        "resp_no_response",
        Matcher {
            all: &["no_response"],
            none: &["inlinks"],
        },
    ),
    (
        "resp_blocked",
        Matcher {
            all: &["blocked"],
            none: &["inlinks"],
        },
    ),
    (
        "inlinks_4xx",
        Matcher {
            all: &["4xx", "inlinks"],
            none: &[],
        },
    ),
    (
        "inlinks_5xx",
        Matcher {
            all: &["5xx", "inlinks"],
            none: &[],
        },
    ),
    (
        "inlinks_3xx",
        Matcher {
            all: &["3xx", "inlinks"],
            none: &[],
        },
    ),
    (
        "all_inlinks",
        Matcher {
            all: &["all_inlinks"],
            none: &[],
        },
    ),
    (
        "sitemap_in",
        Matcher {
            all: &["urls_in_sitemap"],
            none: &["not", "non", "redirect", "orphan"],
        },
    ),
    (
        "sitemap_not_in",
        Matcher {
            all: &["urls_not_in_sitemap"],
            none: &[],
        },
    ),
    (
        "sitemap_orphan",
        Matcher {
            all: &["orphan"],
            none: &[],
        },
    ),
    (
        "sitemap_non_indexable",
        Matcher {
            all: &["non", "indexable", "sitemap"],
            none: &[],
        },
    ),
    (
        "sitemap_redirects",
        Matcher {
            all: &["redirect", "sitemap"],
            none: &[],
        },
    ),
    (
        "sitemap_non_200",
        Matcher {
            all: &["non", "200", "sitemap"],
            none: &[],
        },
    ),
    (
        "images_missing_alt",
        Matcher {
            all: &["missing_alt"],
            none: &[],
        },
    ),
    (
        "images_over_kb",
        Matcher {
            all: &["images", "kb"],
            none: &[],
        },
    ),
    (
        "images_missing_size",
        Matcher {
            all: &["missing_size"],
            none: &[],
        },
    ),
    (
        "titles_duplicate",
        Matcher {
            all: &["page_titles", "duplicate"],
            none: &[],
        },
    ),
    (
        "titles_multiple",
        Matcher {
            all: &["page_titles", "multiple"],
            none: &[],
        },
    ),
    (
        "hreflang",
        Matcher {
            all: &["hreflang"],
            none: &["all_hreflang", "hreflang_all", "contains_hreflang"],
        },
    ),
    (
        "all_hreflang",
        Matcher {
            all: &["all_hreflang"],
            none: &[],
        },
    ),
    (
        "desc_duplicate",
        Matcher {
            all: &["description", "duplicate"],
            none: &[],
        },
    ),
    (
        "redirect_chains",
        Matcher {
            all: &["redirect_chains"],
            none: &[],
        },
    ),
    (
        "security_mixed",
        Matcher {
            all: &["mixed_content"],
            none: &[],
        },
    ),
    (
        "security_hsts",
        Matcher {
            all: &["hsts"],
            none: &[],
        },
    ),
    (
        "structured_data_missing",
        Matcher {
            all: &["structured", "missing"],
            none: &[],
        },
    ),
];

/// Loader failure surface.
#[derive(Debug, Error)]
pub enum LoadError {
    /// `NotADirectoryError`: the exports directory does not exist.
    #[error("Exports directory not found: {0}")]
    NotDirectory(String),
    /// Two files satisfy the same logical key.
    #[error(
        "Ambiguous export for {key}: {count} files match it in {dir}: {names}. Keep only one."
    )]
    Ambiguous {
        /// Logical key.
        key: String,
        /// Number of matching files.
        count: usize,
        /// Directory, repr-quoted.
        dir: String,
        /// Comma-joined matching filenames.
        names: String,
    },
    /// A file that must be read could not be decoded or parsed.
    #[error("Could not decode export {path}: {err}")]
    Undecodable {
        /// Path, repr-quoted.
        path: String,
        /// Last decoder/parser failure description.
        err: String,
    },
    /// A required export resolved to a file that failed to load.
    #[error("Required export {key} was found at {path} but could not be read: {err}.")]
    RequiredUnreadable {
        /// Logical key, repr-quoted.
        key: String,
        /// Path, repr-quoted.
        path: String,
        /// Underlying read error text.
        err: String,
    },
    /// A required export resolved to no file at all.
    #[error("Required export {key} not found in {dir}. Export at least Internal:All from Screaming Frog.")]
    RequiredMissing {
        /// Logical key, repr-quoted.
        key: String,
        /// Directory, repr-quoted.
        dir: String,
    },
    /// CSV structure failure (header/record parse).
    #[error("{0}")]
    Parse(String),
    /// I/O failure.
    #[error(transparent)]
    Io(#[from] std::io::Error),
}

/// `LoadedExports`: all exports found in a directory plus bookkeeping.
#[derive(Debug, Default, Clone)]
pub struct LoadedExports {
    /// key -> parsed table.
    pub frames: HashMap<String, Table>,
    /// key -> path it was read from.
    pub files: HashMap<String, String>,
    /// key -> codec that actually decoded it.
    pub encodings: HashMap<String, String>,
    /// keys read successfully, in `EXPORT_MATCHERS` order.
    pub found: Vec<String>,
    /// keys unavailable, in `EXPORT_MATCHERS` order (read errors inlined).
    pub missing: Vec<String>,
    /// key -> source key it was derived from.
    pub derived_from: HashMap<String, String>,
    /// key -> why derivation was impossible.
    pub derivation_errors: HashMap<String, String>,
}

impl LoadedExports {
    /// `get(key)`: the frame for a logical key, if loaded.
    pub fn get(&self, key: &str) -> Option<&Table> {
        self.frames.get(key)
    }

    /// `has(key)`: whether the frame is present.
    pub fn has(&self, key: &str) -> bool {
        self.frames.contains_key(key)
    }
}

/// `_sniff_encoding`: bounded coherence guess for bytes none of the standard
/// codecs decode. Approximation of `charset_normalizer.from_path(...).best()`:
/// it tries the single-byte legacy encodings exports actually arrive in and
/// keeps the first whose decoded text is mostly letters (no stray control
/// characters). Undecodable-looking data still returns `None`.
fn sniff_encoding(bytes: &[u8]) -> Option<&'static str> {
    const CANDIDATES: &[(&str, &Encoding)] = &[
        ("cp1250", WINDOWS_1250),
        ("cp1251", WINDOWS_1251),
        ("cp1252", WINDOWS_1252),
    ];
    for (name, enc) in CANDIDATES {
        let (text, _, _) = enc.decode(bytes);
        if coherent(&text) {
            return Some(name);
        }
    }
    None
}

fn coherent(text: &str) -> bool {
    let mut letters = 0usize;
    let mut total = 0usize;
    for c in text.chars() {
        if c.is_control() && !matches!(c, '\t' | '\n' | '\r') {
            return false;
        }
        if !c.is_whitespace() {
            total += 1;
            if c.is_alphabetic() {
                letters += 1;
            }
        }
    }
    total > 0 && letters * 2 >= total
}

/// One decoded cell's numeric class for pandas-style column inference.
enum CsvCell {
    /// i64-parseable integer token.
    Int(i64),
    /// Other float token (`5.`, `.5`, `1e3`, `inf`, ...).
    Float(f64),
    /// pandas default `na_values` token.
    Missing,
    /// Anything else — the column is `object` dtype and keeps the raw string.
    Str,
}

const NA_VALUES: &[&str] = &[
    "", "#N/A", "#N/A N/A", "#NA", "-1.#IND", "-1.#QNAN", "-NaN", "-nan", "1.#IND", "1.#QNAN",
    "<NA>", "N/A", "NA", "NULL", "NaN", "None", "n/a", "nan", "null",
];

fn csv_cell(raw: &str) -> CsvCell {
    if NA_VALUES.contains(&raw) {
        return CsvCell::Missing;
    }
    let t = raw.trim();
    if let Ok(i) = t.parse::<i64>() {
        return CsvCell::Int(i);
    }
    if !t.is_empty() {
        if let Ok(f) = t.parse::<f64>() {
            return CsvCell::Float(f);
        }
    }
    CsvCell::Str
}

/// pandas column inference: every cell an integer token -> int64; every cell
/// numeric-or-missing -> float64 (int64 cannot hold NaN); otherwise `object`
/// (raw strings pass through verbatim).
fn infer_column(cells: &[&str]) -> Vec<Value> {
    let kinds: Vec<CsvCell> = cells.iter().map(|s| csv_cell(s)).collect();
    let all_int = !kinds.is_empty() && kinds.iter().all(|k| matches!(k, CsvCell::Int(_)));
    let all_num = kinds
        .iter()
        .all(|k| matches!(k, CsvCell::Int(_) | CsvCell::Float(_) | CsvCell::Missing));
    kinds
        .iter()
        .zip(cells.iter())
        .map(|(k, raw)| match k {
            CsvCell::Missing => Value::Null,
            CsvCell::Int(i) if all_int => Value::from(*i),
            CsvCell::Int(i) if all_num => Value::from(*i as f64),
            CsvCell::Float(f) if all_num && f.is_finite() => Value::from(*f),
            CsvCell::Float(_) if all_num => Value::Null,
            _ => Value::from(*raw),
        })
        .collect()
}

/// pandas `mangle_dupe_cols`: repeated header names gain `.1`, `.2`, ...
fn dedup_names(names: &[String]) -> Vec<String> {
    let mut counts: HashMap<&str, usize> = HashMap::new();
    names
        .iter()
        .map(|n| {
            let c = counts.entry(n.as_str()).or_insert(0);
            *c += 1;
            if *c > 1 {
                format!("{n}.{}", *c - 1)
            } else {
                n.clone()
            }
        })
        .collect()
}

/// Parse CSV text into a `Table` with pandas-compatible typing.
fn parse_csv(text: &str) -> Result<Table, LoadError> {
    if text.is_empty() {
        return Err(LoadError::Parse("No columns to parse from file".to_owned()));
    }
    // Cthe reference csv reader raises "line contains NUL"; a wrong-codec decode
    // of UTF-16 bytes fails here the same way it does in pandas.
    if text.contains('\0') {
        return Err(LoadError::Parse("line contains NUL".to_owned()));
    }
    let mut rdr = csv::ReaderBuilder::new()
        .has_headers(true)
        .flexible(true)
        .from_reader(text.as_bytes());
    let headers: Vec<String> = rdr
        .headers()
        .map_err(|e| LoadError::Parse(e.to_string()))?
        .iter()
        .map(str::to_owned)
        .collect();
    if headers.is_empty() {
        return Err(LoadError::Parse("No columns to parse from file".to_owned()));
    }
    let mut raw_rows: Vec<Vec<String>> = Vec::new();
    for rec in rdr.records() {
        let rec = rec.map_err(|e| LoadError::Parse(e.to_string()))?;
        // pandas `skip_blank_lines`: a truly empty line yields no row.
        if rec.len() == 1 && rec.get(0).is_some_and(str::is_empty) {
            continue;
        }
        raw_rows.push(rec.iter().map(str::to_owned).collect());
    }
    let columns = dedup_names(&headers);
    let width = columns.len();
    // Infer dtype per column, then materialize aligned rows.
    let mut rows: Vec<Vec<Value>> = vec![Vec::new(); raw_rows.len()];
    for (c, _) in columns.iter().enumerate() {
        let cells: Vec<&str> = raw_rows
            .iter()
            .map(|row| row.get(c).map(String::as_str).unwrap_or(""))
            .collect();
        for (r, v) in infer_column(&cells).into_iter().enumerate() {
            rows[r].push(v);
        }
    }
    debug_assert!(rows.iter().all(|r| r.len() == width));
    Ok(Table { columns, rows })
}

/// Read an `.xlsx`/`.xls` workbook's first sheet into a `Table`.
fn read_excel(path: &Path) -> Result<Table, LoadError> {
    use calamine::{open_workbook_auto, Reader};
    let mut wb = open_workbook_auto(path).map_err(|e| LoadError::Parse(format!("{e}")))?;
    let range = wb
        .worksheet_range_at(0)
        .ok_or_else(|| LoadError::Parse("no worksheets".to_owned()))
        .and_then(|r| r.map_err(|e| LoadError::Parse(format!("{e}"))))?;
    let mut it = range.rows();
    let headers: Vec<String> = it
        .next()
        .map(|row| row.iter().map(excel_text).collect())
        .unwrap_or_default();
    let columns = dedup_names(&headers);
    let mut rows: Vec<Vec<Value>> = Vec::new();
    for row in it {
        if row.iter().all(|c| matches!(c, calamine::Data::Empty)) {
            continue; // skip_blank_lines
        }
        rows.push(
            (0..columns.len())
                .map(|i| row.get(i).map_or(Value::Null, excel_value))
                .collect(),
        );
    }
    Ok(Table { columns, rows })
}

fn excel_text(cell: &calamine::Data) -> String {
    match cell {
        calamine::Data::String(s) => s.clone(),
        calamine::Data::Int(i) => i.to_string(),
        calamine::Data::Float(f) => f.to_string(),
        calamine::Data::Bool(b) => b.to_string(),
        calamine::Data::Empty => String::new(),
        other => format!("{other}"),
    }
}

fn excel_value(cell: &calamine::Data) -> Value {
    match cell {
        calamine::Data::String(s) => Value::from(s.as_str()),
        calamine::Data::Int(i) => Value::from(*i),
        // openpyxl surfaces whole numbers as `int`, so the pandas column lands
        // int64; calamine always gives `Float` — emit i64 when integral.
        calamine::Data::Float(f) if f.is_finite() && f.fract() == 0.0 => {
            if *f >= i64::MIN as f64 && *f <= i64::MAX as f64 {
                Value::from(*f as i64)
            } else {
                Value::from(*f)
            }
        }
        calamine::Data::Float(f) if f.is_finite() => Value::from(*f),
        calamine::Data::Bool(b) => Value::from(*b),
        calamine::Data::DateTime(d) => Value::from(format!("{d}")),
        calamine::Data::DateTimeIso(s) | calamine::Data::DurationIso(s) => Value::from(s.as_str()),
        _ => Value::Null,
    }
}

/// Decode bytes with `enc`, strictly — `Encoding::decode` would sniff a BOM
/// and happily decode UTF-16 under the `UTF_8` label, so the no-BOM strict
/// variant is required. Any undecodable sequence is an error.
fn decode(enc: &'static Encoding, bytes: &[u8]) -> Result<String, String> {
    enc.decode_without_bom_handling_and_without_replacement(bytes)
        .map(|t| t.into_owned())
        .ok_or_else(|| format!("invalid {} bytes", enc.name()))
}

/// `read_table`: read a CSV/XLSX export, trying the codecs exports are known
/// to be written in; returns the table and the codec that worked.
pub fn read_table(path: &Path) -> Result<(Table, String), LoadError> {
    let ext = path
        .extension()
        .and_then(|e| e.to_str())
        .map(str::to_lowercase)
        .unwrap_or_default();
    if ext == "xlsx" || ext == "xls" {
        return read_excel(path).map(|t| (t, "xlsx".to_owned()));
    }
    let bytes = fs::read(path)?;
    let mut last_err = String::from("empty file");
    let mut tried: Vec<(&str, Result<String, String>)> = Vec::new();
    tried.push(("utf-8-sig", {
        match bytes.strip_prefix(b"\xef\xbb\xbf") {
            Some(rest) => decode(UTF_8, rest),
            None => decode(UTF_8, &bytes),
        }
    }));
    tried.push((
        "utf-16",
        match bytes.as_slice() {
            [0xff, 0xfe, rest @ ..] => decode(UTF_16LE, rest),
            [0xfe, 0xff, rest @ ..] => decode(UTF_16BE, rest),
            _ => Err("UTF-16 stream does not start with BOM".to_owned()),
        },
    ));
    tried.push(("utf-8", decode(UTF_8, &bytes)));
    if let Some(enc) = sniff_encoding(&bytes) {
        let codec = match enc {
            "cp1250" => WINDOWS_1250,
            "cp1251" => WINDOWS_1251,
            _ => WINDOWS_1252,
        };
        tried.push((enc, decode(codec, &bytes)));
    }
    for (enc, outcome) in tried {
        match outcome {
            Ok(text) => match parse_csv(&text) {
                Ok(t) => return Ok((t, enc.to_owned())),
                Err(e) => last_err = e.to_string(),
            },
            Err(e) => last_err = e,
        }
    }
    Err(LoadError::Undecodable {
        path: py_repr(&path.display().to_string()),
        err: last_err,
    })
}

/// `_matches`: filename satisfies a matcher (`-`/` ` normalize to `_`).
fn matches(filename: &str, matcher: &Matcher) -> bool {
    let low = filename.to_lowercase().replace(['-', ' '], "_");
    if matcher.all.iter().any(|t| !low.contains(&t.to_lowercase())) {
        return false;
    }
    if matcher.none.iter().any(|t| low.contains(&t.to_lowercase())) {
        return false;
    }
    !matcher.all.is_empty()
}

/// `discover_exports`: map each logical export key to its matching file.
pub fn discover_exports(exports_dir: &Path) -> Result<Vec<(String, PathBuf)>, LoadError> {
    if !exports_dir.is_dir() {
        return Err(LoadError::NotDirectory(exports_dir.display().to_string()));
    }
    let mut candidates: Vec<String> = Vec::new();
    for entry in fs::read_dir(exports_dir)? {
        let name = entry?.file_name().to_string_lossy().into_owned();
        let low = name.to_lowercase();
        if low.ends_with(".csv") || low.ends_with(".xlsx") || low.ends_with(".xls") {
            candidates.push(name);
        }
    }
    candidates.sort();
    let mut found: Vec<(String, PathBuf)> = Vec::new();
    for (key, matcher) in EXPORT_MATCHERS {
        let matches: Vec<&String> = candidates
            .iter()
            .filter(|name| self::matches(name, matcher))
            .collect();
        if matches.len() > 1 {
            return Err(LoadError::Ambiguous {
                key: py_repr(key),
                count: matches.len(),
                dir: py_repr(&exports_dir.display().to_string()),
                names: matches
                    .iter()
                    .map(|s| s.as_str())
                    .collect::<Vec<_>>()
                    .join(", "),
            });
        }
        if let Some(name) = matches.first() {
            found.push(((*key).to_owned(), exports_dir.join(name)));
        }
    }
    Ok(found)
}

/// `_derive_sitemap_status_exports`: derive the redirect/non-200 subsets from
/// the `URLs In Sitemap` status column — only when every row has a valid
/// HTTP status.
fn derive_sitemap_status_exports(result: &mut LoadedExports) {
    let Some(source) = result.get("sitemap_in").cloned() else {
        return;
    };
    let status_col = source
        .columns()
        .iter()
        .find(|c| c.trim().to_lowercase() == "status code")
        .cloned();
    let Some(status_col) = status_col else {
        let reason =
            "cannot derive sitemap status findings: URLs In Sitemap has no Status Code column";
        for key in ["sitemap_redirects", "sitemap_non_200"] {
            if !result.frames.contains_key(key) {
                result
                    .derivation_errors
                    .insert(key.to_owned(), reason.to_owned());
            }
        }
        return;
    };
    let idx = source.column_index(&status_col).unwrap_or(0);
    let statuses: Vec<Option<f64>> = (0..source.len())
        .map(|r| crate::normalize::pd_coerce_f64(source.cell(r, idx)))
        .collect();
    let invalid = statuses
        .iter()
        .filter(|s| s.is_none_or(|v| !(100.0..=599.0).contains(&v)))
        .count();
    if invalid > 0 {
        let reason = format!(
            "cannot derive sitemap status findings: {invalid} URL(s) have missing or invalid Status Code values"
        );
        for key in ["sitemap_redirects", "sitemap_non_200"] {
            if !result.frames.contains_key(key) {
                result
                    .derivation_errors
                    .insert(key.to_owned(), reason.clone());
            }
        }
        return;
    }
    let masks: [(&str, Vec<bool>); 2] = [
        (
            "sitemap_redirects",
            statuses
                .iter()
                .map(|s| s.is_some_and(|v| (300.0..400.0).contains(&v)))
                .collect(),
        ),
        (
            "sitemap_non_200",
            statuses
                .iter()
                .map(|s| s.is_some_and(|v| v >= 400.0))
                .collect(),
        ),
    ];
    for (key, mask) in masks {
        if result.frames.contains_key(key) {
            continue;
        }
        result
            .frames
            .insert(key.to_owned(), source.select_rows(&mask));
        if let Some(f) = result.files.get("sitemap_in").cloned() {
            result.files.insert(key.to_owned(), f);
        }
        if let Some(e) = result.encodings.get("sitemap_in").cloned() {
            result.encodings.insert(key.to_owned(), e);
        } else {
            result.encodings.insert(key.to_owned(), String::new());
        }
        result.found.push(key.to_owned());
        result
            .derived_from
            .insert(key.to_owned(), "sitemap_in".to_owned());
    }
}

/// `load_exports`: discover and read recognized exports; optionally derive the
/// full-profile sitemap status subsets.
pub fn load_exports(
    exports_dir: &Path,
    required: &[&str],
    derive_sitemap_status: bool,
) -> Result<LoadedExports, LoadError> {
    let paths = discover_exports(exports_dir)?;
    let mut result = LoadedExports::default();
    let mut read_errors: HashMap<String, (String, String)> = HashMap::new();
    for (key, path) in paths {
        let path_str = path.display().to_string();
        match read_table(&path) {
            Ok((frame, enc)) => {
                result.frames.insert(key.clone(), frame);
                result.files.insert(key.clone(), path_str);
                result.encodings.insert(key.clone(), enc);
                result.found.push(key);
            }
            Err(err) => {
                let text = err.to_string();
                result.missing.push(format!("{key} (read error: {text})"));
                read_errors.insert(key, (path_str, text));
            }
        }
    }
    if derive_sitemap_status {
        derive_sitemap_status_exports(&mut result);
    }
    for (key, _) in EXPORT_MATCHERS {
        if !result.frames.contains_key(*key) && !read_errors.contains_key(*key) {
            result.missing.push((*key).to_owned());
        }
    }
    for key in required {
        if !result.frames.contains_key(*key) {
            if let Some((path, err)) = read_errors.get(*key) {
                return Err(LoadError::RequiredUnreadable {
                    key: py_repr(key),
                    path: py_repr(path),
                    err: err.clone(),
                });
            }
            return Err(LoadError::RequiredMissing {
                key: py_repr(key),
                dir: py_repr(&exports_dir.display().to_string()),
            });
        }
    }
    Ok(result)
}
