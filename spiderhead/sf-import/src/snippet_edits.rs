//! reviewable search-snippet edit candidates (#1010).
//!
//! CSV parsing is pure. Optional typography measurements go through an
//! explicit local font profile supplied as a [`TextMeasure`] loader — the
//! reference original reaches PIL's `ImageFont` here; the seam keeps the Rust
//! port identical in behavior without vendoring a font engine. Optional
//! retained content comes only from a named saved scan and exact URL; this
//! module never fetches a site or selects search-engine pixel thresholds.

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::rc::Rc;

use serde_json::{json, Map, Value};
use sha2::{Digest, Sha256};
use spiderhead_parse::content_area::{extract_area_text, resolve_content_area};
use spiderhead_parse::dom::{self, has_attr};
use spiderhead_parse::py_str::py_strip;
use spiderhead_parse::py_url::urlsplit;
use spiderhead_parse::types::ContentAreaConfig;
use spiderhead_parse::{parse_html, ParseOptions};
use spiderhead_store::bodies::read_page_document_content;
use spiderhead_store::error::StoreError;
use spiderhead_store::scan_artifact::open_scan;
use thiserror::Error;

/// Maximum accepted CSV size, in characters.
pub const MAX_CHARS: usize = 5_000_000;
/// The two snippet fields an edit may carry.
pub const EDITABLE: [&str; 2] = ["title", "description"];
/// Schema tag of the measurement output.
pub const METRICS_SCHEMA: &str = "snippet_metrics.v1";
/// Schema tag of the task output.
pub const TASKS_SCHEMA: &str = "snippet_tasks.v1";
/// Preview text cap before measurement is refused.
pub const MAX_PREVIEW_CHARS: usize = 10_000;
/// Upper bound of the retained-body read cap.
pub const MAX_RETAINED_BODY_BYTES: i64 = 64 * 1024 * 1024;
/// Default retained-body read cap.
pub const DEFAULT_RETAINED_BODY_BYTES: i64 = 8 * 1024 * 1024;
/// Query-excerpt window, in characters.
pub const MAX_QUERY_EXCERPT_CHARS: usize = 320;

/// reference exception text: `ClassName: message`, as the oracle's tests record
/// `f"{type(exc).__name__}: {exc}"`.
#[derive(Debug, Error)]
#[error("{0}")]
pub struct SnippetError(pub String);

/// `ValueError: message`.
fn err(message: impl Into<String>) -> SnippetError {
    SnippetError(format!("ValueError: {}", message.into()))
}

/// `ClassName: message` for the other exception types reference raises here.
fn named_err(class: &str, message: impl Into<String>) -> SnippetError {
    SnippetError(format!("{class}: {}", message.into()))
}

/// `KeyError: 'key'` — Creference renders the missing key through `repr`.
fn key_err(key: &str) -> SnippetError {
    named_err("KeyError", format!("'{key}'"))
}

/// `TypeError: message`.
fn type_err(message: impl Into<String>) -> SnippetError {
    named_err("TypeError", message)
}

/// `AttributeError: message`.
fn attr_err(message: impl Into<String>) -> SnippetError {
    named_err("AttributeError", message)
}

/// An inner error propagating verbatim (`io`/`json`/`parse`/`store` texts
/// already carry their own class names, as reference lets them surface).
fn raw_err(message: impl Into<String>) -> SnippetError {
    SnippetError(message.into())
}

/// `TypeError` raised by `obj[key]` when `obj` is not subscriptable or the
/// key is a string on a sequence.
fn subscript_err(container: &Value) -> SnippetError {
    match container {
        Value::String(_) => type_err("string indices must be integers, not 'str'"),
        Value::Array(_) => type_err("list indices must be integers or slices, not str"),
        other => type_err(format!(
            "'{}' object is not subscriptable",
            crate::pyvalue::py_type_name(other)
        )),
    }
}

/// `TypeError` raised by `key in obj` when `obj` is not a container.
fn contains_err(container: &Value) -> SnippetError {
    type_err(format!(
        "argument of type '{}' is not a container or iterable",
        crate::pyvalue::py_type_name(container)
    ))
}

/// `TypeError` raised by `for x in obj` when `obj` is not iterable.
fn iterate_err(container: &Value) -> SnippetError {
    type_err(format!(
        "'{}' object is not iterable",
        crate::pyvalue::py_type_name(container)
    ))
}

fn sha256_hex(text: &str) -> String {
    let digest = Sha256::digest(text.as_bytes());
    digest.iter().map(|b| format!("{b:02x}")).collect()
}

// --- csv.reader (QUOTE_MINIMAL, quotechar '"', doublequote) -----------------

/// One CSV record plus the physical line number it ended on (reference
/// `reader.line_num` semantics: StringIO lines consumed so far).
struct CsvReader<'a> {
    text: &'a [u8],
    pos: usize,
    /// Count of `'\n'` consumed; a non-empty tail without a newline adds one.
    lines_read: usize,
}

impl<'a> CsvReader<'a> {
    fn new(text: &'a str) -> Self {
        Self {
            text: text.as_bytes(),
            pos: 0,
            lines_read: 0,
        }
    }

    /// Next `(record, line_num)`; `None` at end of input. A lone `\r` in an
    /// unquoted field is the `_csv.Error` Creference raises for `newline=''`
    /// input read without it; `\r\n` or `\r` at absolute EOF ends the record.
    fn next_record(&mut self, delimiter: u8) -> Result<Option<(Vec<String>, usize)>, SnippetError> {
        if self.pos >= self.text.len() {
            return Ok(None);
        }
        let mut fields: Vec<String> = Vec::new();
        let mut field: Vec<u8> = Vec::new();
        // in_quotes / quoted_done mirror the C parser's states.
        let mut in_quotes = false;
        let mut quoted_done = false;
        let mut saw_any = false;
        loop {
            if self.pos >= self.text.len() {
                // EOF: a record exists only when something was seen. The final
                // partial line counts as consumed, so line_num is +1.
                if !saw_any {
                    return Ok(None);
                }
                fields.push(String::from_utf8_lossy(&field).into_owned());
                let line_num = self.lines_read + 1;
                return Ok(Some((fields, line_num)));
            }
            let b = self.text[self.pos];
            if in_quotes {
                if b == b'"' {
                    if self.text.get(self.pos + 1) == Some(&b'"') {
                        field.push(b'"');
                        self.pos += 2;
                    } else {
                        in_quotes = false;
                        quoted_done = true;
                        self.pos += 1;
                    }
                } else {
                    field.push(b);
                    if b == b'\n' {
                        self.lines_read += 1;
                    }
                    self.pos += 1;
                }
                saw_any = true;
                continue;
            }
            match b {
                b'"' if field.is_empty() && !quoted_done => {
                    in_quotes = true;
                    self.pos += 1;
                    saw_any = true;
                }
                _ if b == delimiter => {
                    fields.push(String::from_utf8_lossy(&field).into_owned());
                    field.clear();
                    quoted_done = false;
                    self.pos += 1;
                    saw_any = true;
                }
                b'\r' | b'\n' => {
                    // Record terminator outside quotes. A bare '\r' ends the
                    // record only at absolute EOF; '\r' followed by anything
                    // but '\n' is Cthe reference _csv.Error.
                    let line_num = if b == b'\n' {
                        self.pos += 1;
                        self.lines_read += 1;
                        self.lines_read
                    } else {
                        self.pos += 1;
                        match self.text.get(self.pos) {
                            Some(&b'\n') => {
                                self.pos += 1;
                                self.lines_read += 1;
                                self.lines_read
                            }
                            None => self.lines_read + 1,
                            Some(_) => {
                                return Err(named_err(
                                    "Error",
                                    "new-line character seen in unquoted field - \
                                     do you need to open the file with newline=''?",
                                ));
                            }
                        }
                    };
                    if fields.is_empty() && field.is_empty() {
                        // A blank line is one empty record (line counts).
                        return Ok(Some((Vec::new(), line_num)));
                    }
                    fields.push(String::from_utf8_lossy(&field).into_owned());
                    return Ok(Some((fields, line_num)));
                }
                _ => {
                    field.push(b);
                    self.pos += 1;
                    saw_any = true;
                }
            }
        }
    }
}

/// `parse_snippet_csv`: ``{"edits": [...], "errors": [...], "source_sha256"}``,
/// or a `SnippetError` on bad input.
pub fn parse_snippet_csv(text: &str) -> Result<Value, SnippetError> {
    if text.chars().count() > MAX_CHARS {
        return Err(err(format!(
            "snippet CSV is larger than {MAX_CHARS} characters"
        )));
    }
    let text = text.trim_start_matches('\u{feff}');
    let first_line = text.split('\n').next().unwrap_or("");
    let delimiter = if first_line.matches(';').count() > first_line.matches(',').count() {
        b';'
    } else {
        b','
    };
    let mut reader = CsvReader::new(text);
    let Some((header_row, _)) = reader.next_record(delimiter)? else {
        return Err(err("snippet CSV is empty"));
    };
    let header: Vec<String> = header_row
        .iter()
        .map(|cell| py_strip(cell).to_lowercase())
        .collect();
    if !header.iter().any(|h| h == "url") || !EDITABLE.iter().any(|f| header.iter().any(|h| h == f))
    {
        return Err(err(
            "snippet CSV needs a 'url' column and 'title' or 'description'",
        ));
    }

    let mut edits: Vec<Value> = Vec::new();
    let mut errors: Vec<Value> = Vec::new();
    let mut seen: HashSet<String> = HashSet::new();
    while let Some((row, line)) = reader.next_record(delimiter)? {
        if !row.iter().any(|cell| !py_strip(cell).is_empty()) {
            continue;
        }
        let cells: Vec<String> = header
            .iter()
            .enumerate()
            .map(|(i, _)| {
                row.get(i)
                    .map(|c| py_strip(c).to_owned())
                    .unwrap_or_default()
            })
            .collect();
        let cell_of = |name: &str| -> &str {
            header
                .iter()
                .rposition(|h| h == name)
                .and_then(|i| cells.get(i))
                .map(|s| s.as_str())
                .unwrap_or("")
        };
        let url = cell_of("url");
        let mut push_error = |message: &str| {
            errors.push(json!({"line": line, "message": message}));
        };
        let Ok(parts) = urlsplit(url) else {
            push_error("url is malformed");
            continue;
        };
        if !["http", "https"].contains(&parts.scheme.as_str()) || parts.netloc.is_empty() {
            push_error("url must be an absolute http(s) URL");
            continue;
        }
        if has_credentials(&parts.netloc) {
            push_error("url must not contain credentials");
            continue;
        }
        if seen.contains(url) {
            push_error("duplicate url");
            continue;
        }
        let mut fields = Map::new();
        for name in EDITABLE {
            let value = cell_of(name);
            if !value.is_empty() {
                fields.insert(name.to_owned(), json!(value));
            }
        }
        if fields.is_empty() {
            push_error("no title or description to change");
            continue;
        }
        seen.insert(url.to_owned());
        let mut edit = Map::new();
        edit.insert("line".into(), json!(line));
        edit.insert("url".into(), json!(url));
        edit.extend(fields);
        edits.push(Value::Object(edit));
    }
    Ok(json!({
        "edits": edits,
        "errors": errors,
        "source_sha256": sha256_hex(text),
    }))
}

/// reference `parts.username is not None or parts.password is not None`: urlsplit
/// reports an (possibly empty) username whenever the netloc carries an `@`.
fn has_credentials(netloc: &str) -> bool {
    netloc.contains('@')
}

// --- local typography measurement -----------------------------------------

/// Measures a text's width in pixels — the seam reference fills with PIL's
/// `ImageFont.getlength`.
pub trait TextMeasure {
    /// Width of `text` in the loaded font.
    fn getlength(&self, text: &str) -> f64;
}

/// Loads a local font file at a pixel size (reference: `ImageFont.truetype`).
/// Errors must carry the same text the reference `OSError` branch produces.
pub type FontLoader<'a> = &'a dyn Fn(&str, i64) -> Result<Rc<dyn TextMeasure>, String>;

/// `measure_text_pixels`: measure text in a caller-supplied local font; this
/// is not a search-engine preview.
pub fn measure_text_pixels(
    text: &str,
    font_path: &str,
    font_size: i64,
    loader: FontLoader<'_>,
) -> Result<i64, SnippetError> {
    if font_path.is_empty() {
        return Err(err("font_path must name a local font file"));
    }
    if font_size < 1 {
        return Err(err("font_size must be a positive integer"));
    }
    let font = loader(font_path, font_size).map_err(|_| err("font_path could not be loaded"))?;
    Ok(font.getlength(text).ceil() as i64)
}

/// `_clusters`: keep combining marks, joiners, variation selectors, modifiers,
/// and flag pairs together.
fn clusters(text: &str) -> Vec<String> {
    let mut groups: Vec<String> = Vec::new();
    let mut regional_count = 0usize;
    let mut join_next = false;
    for char_ in text.chars() {
        let code = char_ as u32;
        let extender = unicode_normalization::char::canonical_combining_class(char_) != 0
            || (0xFE00..=0xFE0F).contains(&code)
            || (0xE0100..=0xE01EF).contains(&code)
            || (0x1F3FB..=0x1F3FF).contains(&code);
        let regional = (0x1F1E6..=0x1F1FF).contains(&code);
        if (!groups.is_empty() && (extender || join_next || char_ == '\u{200d}'))
            || (regional && !groups.is_empty() && regional_count % 2 == 1)
        {
            if let Some(last) = groups.last_mut() {
                last.push(char_);
            }
        } else {
            groups.push(char_.to_string());
        }
        if regional {
            regional_count += 1;
        } else {
            regional_count = 0;
        }
        join_next = char_ == '\u{200d}';
    }
    groups
}

fn preview_fit(
    text: &str,
    font: &dyn TextMeasure,
    width_px: i64,
    ellipsis: &str,
) -> Result<Value, SnippetError> {
    if text.chars().count() > MAX_PREVIEW_CHARS {
        return Err(err(format!(
            "snippet preview text exceeds {MAX_PREVIEW_CHARS} characters"
        )));
    }
    let input_width = font.getlength(text).ceil() as i64;
    if input_width <= width_px {
        return Ok(json!({
            "input_width_px": input_width,
            "available_width_px": width_px,
            "displayed_text": text,
            "displayed_width_px": input_width,
            "truncated": false,
        }));
    }
    let ellipsis_width = font.getlength(ellipsis).ceil() as i64;
    let (mut visible, mut visible_width) = (String::new(), 0i64);
    if ellipsis_width <= width_px {
        visible_width = ellipsis_width;
        for cluster in clusters(text) {
            let candidate = format!("{visible}{cluster}");
            let candidate_width = font.getlength(&format!("{candidate}{ellipsis}")).ceil() as i64;
            if candidate_width > width_px {
                break;
            }
            visible = candidate;
            visible_width = candidate_width;
        }
        visible.push_str(ellipsis);
    }
    Ok(json!({
        "input_width_px": input_width,
        "available_width_px": width_px,
        "displayed_text": visible,
        "displayed_width_px": visible_width,
        "truncated": true,
    }))
}

fn font_sha256(path: &Path) -> Result<String, SnippetError> {
    let bytes = std::fs::read(path).map_err(|e| raw_err(e.to_string()))?;
    let digest = Sha256::digest(&bytes);
    Ok(digest.iter().map(|b| format!("{b:02x}")).collect())
}

/// reference `dict(edit)`: a mapping copies; an iterable of pairs converts (each
/// element must be a len-2 sequence — strings count as char sequences);
/// anything else raises the Creference TypeError/ValueError text.
fn dict_edit(edit: &Value) -> Result<Map<String, Value>, SnippetError> {
    let seq_err = |i: usize| -> SnippetError {
        type_err(format!(
            "cannot convert dictionary update sequence element #{i} to a sequence"
        ))
    };
    match edit {
        Value::Object(o) => Ok(o.clone()),
        Value::Array(items) => {
            let mut m = Map::new();
            for (i, pair) in items.iter().enumerate() {
                let (k, v): (Value, Value) = match pair {
                    Value::Array(kv) if kv.len() == 2 => (kv[0].clone(), kv[1].clone()),
                    Value::Array(kv) => {
                        return Err(err(format!(
                            "dictionary update sequence element #{i} has length {}; 2 is required",
                            kv.len()
                        )));
                    }
                    Value::String(s) => {
                        let chars: Vec<char> = s.chars().collect();
                        if chars.len() == 2 {
                            m.insert(chars[0].to_string(), json!(chars[1].to_string()));
                            continue;
                        }
                        return Err(err(format!(
                            "dictionary update sequence element #{i} has length {}; 2 is required",
                            chars.len()
                        )));
                    }
                    // `dict([{"a": 1, "b": 2}])` iterates the dict's keys as
                    // the pair; length mismatches are the same ValueError.
                    Value::Object(o) => {
                        let mut keys = o.keys();
                        match (keys.next(), keys.next(), keys.next()) {
                            (Some(k0), Some(k1), None) => (json!(k0.clone()), json!(k1.clone())),
                            _ => {
                                return Err(err(format!(
                                    "dictionary update sequence element #{} has length {}; 2 is required",
                                    i,
                                    o.len()
                                )));
                            }
                        }
                    }
                    _ => return Err(seq_err(i)),
                };
                // Unhashable keys: `cannot use 'list' as a dict key
                // (unhashable type: 'list')`.
                if matches!(k, Value::Array(_) | Value::Object(_)) {
                    return Err(type_err(format!(
                        "cannot use '{}' as a dict key (unhashable type: '{}')",
                        crate::pyvalue::py_type_name(&k),
                        crate::pyvalue::py_type_name(&k)
                    )));
                }
                // reference keys need only be hashable; JSON renders them as
                // strings, so the same textual form is stored here.
                let key = match k {
                    Value::String(s) => s,
                    other => crate::pyvalue::str_or_dumps(&other),
                };
                m.insert(key, v.clone());
            }
            Ok(m)
        }
        // `dict("ab")` iterates characters: each element is a len-1 string.
        Value::String(s) => {
            if s.is_empty() {
                Ok(Map::new())
            } else {
                Err(err(
                    "dictionary update sequence element #0 has length 1; 2 is required",
                ))
            }
        }
        other => Err(iterate_err(other)),
    }
}

/// `measure_snippet_edits`: add measured title/description widths for
/// explicitly supplied font profiles. Each metric is
/// ``{"font_path": ..., "font_size": ...}``; no profile names, thresholds,
/// verdicts, or engine equivalence are inferred here.
pub fn measure_snippet_edits(
    edits: &[Value],
    metrics: &Value,
    loader: FontLoader<'_>,
) -> Result<Value, SnippetError> {
    // `not isinstance(metrics, dict) or not metrics`
    let metrics_obj = metrics.as_object();
    if metrics_obj.is_none_or(Map::is_empty) {
        return Err(err("metrics must contain at least one named font profile"));
    }
    let metrics = metrics_obj.cloned().unwrap_or_default();
    type Loaded = (String, Rc<dyn TextMeasure>, Option<Map<String, Value>>);
    let mut loaded: Vec<Loaded> = Vec::new();
    let mut provenance = Map::new();
    for (profile, spec) in &metrics {
        if spec.as_object().is_none() || py_strip(profile).is_empty() {
            return Err(err("each metric profile needs a name and font settings"));
        }
        let spec = spec.as_object().cloned().unwrap_or_default();
        let font_path = spec.get("font_path").and_then(Value::as_str).unwrap_or("");
        if font_path.is_empty() {
            return Err(err("each metric profile needs a local font_path"));
        }
        // reference `spec.get("font_size_px", spec.get("font_size"))`: a key that
        // is present even with value `null` wins over the fallback name.
        let font_size = match spec.get("font_size_px") {
            Some(v) => v.clone(),
            None => spec.get("font_size").cloned().unwrap_or(Value::Null),
        };
        if font_size.is_boolean() || font_size.as_i64().is_none_or(|v| v < 1) {
            return Err(err(
                "each metric profile needs a positive integer font_size_px",
            ));
        }
        let font_size = font_size.as_i64().unwrap_or(0);
        // `spec.get("widths_px")` treats a present `null` like a missing key
        // (`widths is None`), so `available_width_px` still synthesizes.
        let mut widths = spec.get("widths_px").cloned().filter(|w| !w.is_null());
        if widths.is_none() {
            if let Some(available) = spec.get("available_width_px") {
                widths = Some(Value::Object(
                    EDITABLE
                        .iter()
                        .map(|f| ((*f).to_owned(), available.clone()))
                        .collect::<Map<String, Value>>(),
                ));
            }
        }
        if let Some(w) = &widths {
            let bad = !w.is_object()
                || w.as_object().is_none_or(|o| o.is_empty())
                || w.as_object().is_some_and(|o| {
                    o.iter().any(|(field, value)| {
                        !EDITABLE.contains(&field.as_str())
                            || value.is_boolean()
                            || value.as_i64().is_none_or(|v| v < 1)
                    })
                });
            if bad {
                return Err(err(
                    "widths_px must map title/description to positive integer widths",
                ));
            }
        }
        let font_file = PathBuf::from(font_path);
        if !font_file.is_file() {
            return Err(err("font_path must name an existing local font file"));
        }
        let font =
            loader(font_path, font_size).map_err(|_| err("font_path could not be loaded"))?;
        let ellipsis = match spec.get("ellipsis") {
            None => "…",
            Some(v) => match v.as_str() {
                Some(s) => s,
                None => return Err(err("ellipsis must be nonempty text")),
            },
        };
        if ellipsis.is_empty() {
            return Err(err("ellipsis must be nonempty text"));
        }
        loaded.push((
            profile.clone(),
            font,
            widths.as_ref().and_then(Value::as_object).cloned(),
        ));
        let mut entry = Map::new();
        entry.insert("profile_id".into(), json!(profile));
        for key in ["engine", "device", "locale", "source"] {
            let name = match key {
                "engine" => "intended_engine",
                other => other,
            };
            entry.insert(name.into(), spec.get(key).cloned().unwrap_or(Value::Null));
        }
        let file_name = font_file
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default();
        entry.insert("font_name".into(), json!(file_name));
        entry.insert("font_sha256".into(), json!(font_sha256(&font_file)?));
        entry.insert("font_size_px".into(), json!(font_size));
        entry.insert("widths_px".into(), widths.clone().unwrap_or(Value::Null));
        entry.insert("ellipsis".into(), json!(ellipsis));
        entry.insert(
            "method".into(),
            json!("Pillow ImageFont.getlength local typography fit"),
        );
        provenance.insert(profile.clone(), Value::Object(entry));
    }

    let mut measured: Vec<Value> = Vec::new();
    for edit in edits {
        let mut row = dict_edit(edit)?;
        let mut widths_out = Map::new();
        let mut previews_out = Map::new();
        for (profile, spec) in &metrics {
            let spec_obj = spec.as_object().cloned().unwrap_or_default();
            let (_, font, available) = loaded
                .iter()
                .find(|(p, _, _)| p == profile)
                .expect("profiles are loaded in metrics order");
            let mut profile_widths = Map::new();
            let mut profile_previews = Map::new();
            for field in EDITABLE {
                if let Some(value) = row.get(field) {
                    let Some(text) = value.as_str() else {
                        return Err(err(format!("{field} must be text")));
                    };
                    profile_widths.insert(field.into(), json!(font.getlength(text).ceil() as i64));
                    if let Some(avail) = available {
                        if let Some(w) = avail.get(field).and_then(Value::as_i64) {
                            let ellipsis = spec_obj
                                .get("ellipsis")
                                .and_then(Value::as_str)
                                .unwrap_or("…");
                            profile_previews.insert(
                                field.into(),
                                preview_fit(text, font.as_ref(), w, ellipsis)?,
                            );
                        }
                    }
                }
            }
            widths_out.insert(profile.clone(), Value::Object(profile_widths));
            previews_out.insert(profile.clone(), Value::Object(profile_previews));
        }
        row.insert("pixel_widths".into(), Value::Object(widths_out));
        if previews_out
            .values()
            .any(|p| !p.as_object().is_none_or(|o| o.is_empty()))
        {
            row.insert("previews".into(), Value::Object(previews_out));
        }
        measured.push(Value::Object(row));
    }
    Ok(json!({
        "schema_version": METRICS_SCHEMA,
        "preview_kind": "local_typographic_fit_not_search_engine_rendering",
        "profiles": provenance,
        "edits": measured,
    }))
}

// --- retained-document context --------------------------------------------

/// What the injected spelling callback sees: the seam for
/// `spellcheck.run(html, backend, content_area=..., ignore_words=..., visible_text_override=...)`
/// — the check itself lives in the `checks` crate and is passed in, like the
/// reference tests' injected backend.
pub struct SpellcheckArgs<'a> {
    /// The full retained HTML document.
    pub html: &'a str,
    /// Words excluded from the spelling run.
    pub ignore_words: &'a [String],
    /// The visible content-area text after data-nosnippet removal.
    pub visible_text_override: &'a str,
    /// The content-area config the caller supplied (forwarded verbatim).
    pub content_area: Option<&'a Value>,
}

/// `inspect_retained_document`: extract bounded snippet context from one
/// complete retained HTML document. `document` is the storage reader's result,
/// not a live response. `spellcheck` is the injected checker (see
/// [`SpellcheckArgs`]). The `query`/`content_area`/`ignore_words` values are
/// the reference keyword arguments: `None`/`Some(Null)` is the reference `None`,
/// except `ignore_words`, where `Some(Null)` is `tuple(None)`'s TypeError.
pub fn inspect_retained_document(
    document: &Value,
    query: Option<&Value>,
    content_area: Option<&Value>,
    ignore_words: Option<&Value>,
    spellcheck: &dyn Fn(&SpellcheckArgs<'_>) -> Value,
) -> Result<Value, SnippetError> {
    // `document.get("state")` — a non-dict document dies on `.get`.
    let Some(document_obj) = document.as_object() else {
        return Err(attr_err(format!(
            "'{}' object has no attribute 'get'",
            crate::pyvalue::py_type_name(document)
        )));
    };
    let identity = document_identity(document_obj);
    if document_obj.get("state").and_then(Value::as_str) != Some("available")
        || !document_obj.get("text").is_some_and(Value::is_string)
    {
        return Ok(json!({
            "state": "unavailable",
            "reason": document_obj
                .get("reason")
                .filter(|v| crate::pyvalue::truthy(v))
                .map(str_or_empty)
                .unwrap_or_else(|| "retained document is unavailable".into()),
            "source": identity,
        }));
    }
    let html = document_obj["text"].as_str().unwrap_or("");
    // `document.get("truncated") is True` — the exact JSON boolean only.
    if document_obj.get("truncated").and_then(Value::as_bool) == Some(true) {
        return Ok(json!({
            "state": "partial",
            "reason": "retained document read was truncated",
            "source": identity,
        }));
    }
    let content_type = document_obj
        .get("content_type")
        .filter(|v| crate::pyvalue::truthy(v))
        .map(str_or_empty)
        .unwrap_or_default();
    let content_type = content_type
        .split(';')
        .next()
        .unwrap_or("")
        .trim()
        .to_lowercase();
    if !content_type.is_empty()
        && content_type != "text/html"
        && content_type != "application/xhtml+xml"
    {
        return Ok(json!({
            "state": "unavailable",
            "reason": format!("retained document content type is not HTML: {content_type}"),
            "source": identity,
        }));
    }
    let url = document_obj
        .get("url")
        .and_then(Value::as_str)
        .unwrap_or("");
    if url.is_empty() {
        return Ok(json!({
            "state": "unavailable",
            "reason": "retained document has no exact URL",
            "source": identity,
        }));
    }
    let page =
        parse_html(html, url, &ParseOptions::default()).map_err(|e| raw_err(e.to_string()))?;
    let soup = dom::parse_document_lxml(html);
    let content_area = content_area.filter(|v| !v.is_null());
    // `config.get("exclude_tags", …)` inside `resolve_content_area` — a
    // non-dict config dies on `.get`.
    if let Some(v) = content_area {
        if !v.is_object() {
            return Err(attr_err(format!(
                "'{}' object has no attribute 'get'",
                crate::pyvalue::py_type_name(v)
            )));
        }
    }
    let config: Option<ContentAreaConfig> = match content_area {
        None => None,
        Some(v) => Some(serde_json::from_value(v.clone()).map_err(|e| raw_err(e.to_string()))?),
    };
    let mut area =
        resolve_content_area(&soup, config.as_ref()).map_err(|e| raw_err(e.to_string()))?;
    let content_area_strategy = area.strategy.clone();
    let content_root_excluded = has_attr(&area.root(), "data-nosnippet");
    let visible = if content_root_excluded {
        String::new()
    } else {
        let doomed: Vec<_> = area
            .root()
            .descendants()
            .filter_map(dom::ElementRef::wrap)
            .filter(|el| has_attr(el, "data-nosnippet"))
            .map(|el| el.id())
            .collect();
        for id in doomed {
            if let Some(mut node) = area.fragment.tree.get_mut(id) {
                node.detach();
            }
        }
        extract_area_text(&area)
    };

    // `query` is reference `None` when absent or JSON `null`.
    let query = query.filter(|v| !v.is_null());
    let mut excerpt = Map::new();
    if content_root_excluded {
        excerpt.insert("state".into(), json!("unavailable"));
        excerpt.insert(
            "reason".into(),
            json!("content area is marked data-nosnippet"),
        );
    } else if let Some(q) = query {
        if !q.is_string() {
            return Err(err("search query must be text"));
        }
        let q = q.as_str().unwrap_or_default();
        if py_strip(q).is_empty() {
            excerpt.insert("state".into(), json!("unavailable"));
            excerpt.insert("reason".into(), json!("search query was not supplied"));
        } else if q.chars().count() > 512 {
            excerpt.insert("state".into(), json!("unavailable"));
            excerpt.insert(
                "reason".into(),
                json!("search query exceeds 512 characters"),
            );
        } else {
            let pattern = regex::RegexBuilder::new(&regex::escape(q))
                .case_insensitive(true)
                .build()
                .map_err(|e| raw_err(e.to_string()))?;
            match pattern.find(&visible) {
                None => {
                    excerpt.insert("state".into(), json!("unavailable"));
                    excerpt.insert(
                        "reason".into(),
                        json!("query does not appear in retained visible content"),
                    );
                    excerpt.insert("query".into(), json!(q));
                }
                Some(m) => {
                    // Regex offsets are byte positions; reference `re` reports
                    // character positions — convert so every offset the
                    // result carries matches the oracle.
                    let start_char = visible[..m.start()].chars().count();
                    let end_char = start_char + m.as_str().chars().count();
                    let bounded = bounded_query_excerpt(&visible, start_char, end_char);
                    excerpt.extend(bounded);
                    excerpt.insert("state".into(), json!("available"));
                    excerpt.insert("query".into(), json!(q));
                    excerpt.insert("match_start_offset".into(), json!(start_char));
                    excerpt.insert("match_end_offset".into(), json!(end_char));
                    if excerpt.get("match_in_excerpt") == Some(&json!(false)) {
                        excerpt.insert("state".into(), json!("partial"));
                        excerpt.insert(
                            "reason".into(),
                            json!("query exceeds the bounded excerpt window"),
                        );
                    }
                }
            }
        }
    } else {
        excerpt.insert("state".into(), json!("unavailable"));
        excerpt.insert("reason".into(), json!("search query was not supplied"));
    }
    // `isinstance(ignore_words, str)` → ValueError; `tuple(x)` TypeError →
    // ValueError; a dict iterates its (string) keys.
    let ignored: Vec<String> = match ignore_words {
        None => Vec::new(),
        Some(Value::Array(items)) => {
            if items.iter().any(|w| !w.is_string()) {
                return Err(err("ignore_words must be an iterable of strings"));
            }
            items
                .iter()
                .map(|w| w.as_str().unwrap_or_default().to_owned())
                .collect()
        }
        Some(Value::Object(o)) => o.keys().cloned().collect(),
        Some(_) => return Err(err("ignore_words must be an iterable of strings")),
    };
    let spelling = spellcheck(&SpellcheckArgs {
        html,
        ignore_words: &ignored,
        visible_text_override: &visible,
        content_area,
    });
    Ok(json!({
        "state": "complete",
        "url": url,
        "representation": document_obj.get("representation").cloned().unwrap_or(Value::Null),
        "source": identity,
        "metadata": {"title": page.title, "description": page.meta_description},
        "visible_text": {
            "characters": visible.chars().count(),
            "sha256": sha256_hex(&visible),
            "content_area_strategy": content_area_strategy,
            "data_nosnippet_excluded": true,
        },
        "query_excerpt": excerpt,
        "spelling": spelling,
    }))
}

fn str_or_empty(v: &Value) -> String {
    crate::pyvalue::str_or_dumps(v)
}

/// `_bounded_query_excerpt`: the clipped excerpt window around a match, with
/// provenance offsets and whether the whole match survived the crop. All
/// offsets are character positions, as the reference string indexing and `re` use.
pub fn bounded_query_excerpt(text: &str, start: usize, end: usize) -> Map<String, Value> {
    let chars: Vec<char> = text.chars().collect();
    let left = start.saturating_sub(MAX_QUERY_EXCERPT_CHARS / 3);
    let right = (end + MAX_QUERY_EXCERPT_CHARS * 2 / 3).min(chars.len());
    let mut excerpt = redacted_slice(text, left, right);
    if excerpt.chars().count() > MAX_QUERY_EXCERPT_CHARS {
        excerpt = format!(
            "{}…",
            excerpt
                .chars()
                .take(MAX_QUERY_EXCERPT_CHARS - 1)
                .collect::<String>()
        );
    }
    let query_end_limit = if right - left > MAX_QUERY_EXCERPT_CHARS {
        MAX_QUERY_EXCERPT_CHARS - 1
    } else {
        MAX_QUERY_EXCERPT_CHARS
    };
    let mut out = Map::new();
    out.insert("text".into(), json!(excerpt));
    out.insert("start_offset".into(), json!(left));
    out.insert("end_offset".into(), json!(right));
    out.insert(
        "context_truncated".into(),
        json!(left > 0 || right < chars.len()),
    );
    out.insert(
        "match_in_excerpt".into(),
        json!(start >= left && end - left <= query_end_limit),
    );
    out
}

fn credential_pattern() -> &'static regex::Regex {
    use std::sync::OnceLock;
    static RE: OnceLock<regex::Regex> = OnceLock::new();
    RE.get_or_init(|| {
        regex::Regex::new(
            r#"(?i)(authorization|bearer|access[_-]?token|refresh[_-]?token|token|api[_ -]?key|client[_ -]?secret|secret|password|passwd|cookie|set-cookie)\s*[:=]\s*(?:bearer\s+)?(?:'[^']*'|"[^"]*"|[^\s<'">]+)"#,
        )
        .expect("static regex")
    })
}

/// `_redacted_slice`: match credentials in the full text before cropping,
/// replacing any overlapping span. `start`/`end` are character positions.
fn redacted_slice(text: &str, start: usize, end: usize) -> String {
    let chars: Vec<char> = text.chars().collect();
    let mut cursor = start;
    let mut parts: Vec<String> = Vec::new();
    let re = credential_pattern();
    let mut from = 0usize;
    while let Some(m) = re.find_at(text, from) {
        // reference `(?<![a-z0-9])` (case-insensitive): reject matches that
        // directly follow an ASCII letter or digit.
        if m.start() > 0
            && text[..m.start()]
                .chars()
                .next_back()
                .is_some_and(|c| c.is_ascii_alphanumeric())
        {
            from = m.start() + 1;
            continue;
        }
        let match_start = text[..m.start()].chars().count();
        let match_end = match_start + m.as_str().chars().count();
        if match_end <= start {
            from = m.end();
            continue;
        }
        if match_start >= end {
            break;
        }
        let overlap_start = start.max(match_start);
        let overlap_end = end.min(match_end);
        if overlap_start > cursor {
            parts.push(chars[cursor..overlap_start].iter().collect());
        }
        if overlap_end > cursor {
            parts.push("[REDACTED]".to_owned());
            cursor = overlap_end;
        }
        from = m.end().max(from);
    }
    if cursor < end {
        parts.push(chars[cursor..end].iter().collect());
    }
    parts.concat()
}

/// `_document_identity`: copy only stable retained-document provenance fields.
fn document_identity(document: &Map<String, Value>) -> Value {
    let mut out = Map::new();
    for key in ["state", "reason", "url"] {
        if let Some(v) = document.get(key) {
            out.insert(key.into(), v.clone());
        }
    }
    for key in [
        "scan_uuid",
        "evidence_revision",
        "document_id",
        "representation",
        "variant_key",
        "content_type",
        "raw_body_sha256",
        "sha256",
        "total_chars",
        "lines",
        "encoding",
        "decoder",
        "validation",
    ] {
        if let Some(v) = document.get(key) {
            out.insert(key.into(), v.clone());
        }
    }
    Value::Object(out)
}

/// Options of `attach_retained_context` (the reference keyword-only parameters).
#[derive(Debug, Clone)]
pub struct RetainedContextOptions<'a> {
    /// "static" | "rendered" | "legacy_fragment".
    pub representation: &'a str,
    /// Variant pin, when the scan carries several.
    pub variant_key: Option<&'a str>,
    /// Search query for the bounded excerpt.
    pub query: Option<&'a str>,
    /// Content-area config, forwarded to the resolver and the spellcheck seam.
    pub content_area: Option<&'a Value>,
    /// Retained-body read cap (1..=MAX_RETAINED_BODY_BYTES).
    pub max_decoded_bytes: i64,
}

/// reference defaults: `representation="static"`, the rest unset.
impl Default for RetainedContextOptions<'_> {
    fn default() -> Self {
        Self {
            representation: "static",
            variant_key: None,
            query: None,
            content_area: None,
            max_decoded_bytes: DEFAULT_RETAINED_BODY_BYTES,
        }
    }
}

/// The storage seam the reference tests patch: `open_scan` +
/// `bodies.read_page_document_content` behind one connection object.
pub trait ScanConnection {
    /// `read_page_document_content(con, url, representation=…,
    /// variant_key=…, max_decoded_bytes=…)`. `Ok` is the reader's document;
    /// `Err(ReadError::Scan)` is the `ScanError` reference catches into an
    /// unavailable document. `variant_key` arrives verbatim, as the reference
    /// untyped keyword does.
    fn read_page_document_content(
        &self,
        url: &Value,
        representation: &str,
        variant_key: Option<&Value>,
        max_decoded_bytes: i64,
    ) -> Result<Value, ReadError>;
}

/// Reader failure split the way reference splits it.
pub enum ReadError {
    /// `ScanError`: caught into `{"state": "unavailable", "reason": msg}`.
    Scan(String),
    /// Any other exception: propagates out of `attach_retained_context`.
    Fatal(SnippetError),
}

/// `open_scan(input_path, require_audit=False)`: errors propagate verbatim.
pub type ScanOpener<'a> = &'a dyn Fn(&Path) -> Result<Box<dyn ScanConnection>, SnippetError>;

/// `bodies.read_page_document_content` as a boxed callable.
type ReadFn = dyn Fn(&Value, &str, Option<&Value>, i64) -> Result<Value, ReadError>;

/// The real `open_scan` + `bodies.read_page_document_content` pair. The
/// boxed closure owns the `rusqlite::Connection`, so dropping it closes the
/// scan exactly like the reference `con.close()`.
struct StoreScan {
    read: Box<ReadFn>,
}

impl ScanConnection for StoreScan {
    fn read_page_document_content(
        &self,
        url: &Value,
        representation: &str,
        variant_key: Option<&Value>,
        max_decoded_bytes: i64,
    ) -> Result<Value, ReadError> {
        (self.read)(url, representation, variant_key, max_decoded_bytes)
    }
}

/// `attach_retained_context`: attach exact-URL body context from a saved
/// scan; this function never fetches URLs.
pub fn attach_retained_context(
    tasks: &mut Value,
    input_path: &Path,
    options: &RetainedContextOptions<'_>,
    spellcheck: &dyn Fn(&SpellcheckArgs<'_>) -> Value,
    ignore_words: &[String],
) -> Result<(), SnippetError> {
    // The typed options become the reference `**kwargs` dict the oracle sees.
    let mut kwargs = Map::new();
    kwargs.insert("representation".into(), json!(options.representation));
    if let Some(v) = options.variant_key {
        kwargs.insert("variant_key".into(), json!(v));
    }
    if let Some(q) = options.query {
        kwargs.insert("query".into(), json!(q));
    }
    if let Some(c) = options.content_area {
        kwargs.insert("content_area".into(), c.clone());
    }
    kwargs.insert("max_decoded_bytes".into(), json!(options.max_decoded_bytes));
    kwargs.insert("ignore_words".into(), json!(ignore_words));
    attach_retained_context_in(
        tasks,
        input_path,
        &|path: &Path| {
            let con = open_scan(path, false, None).map_err(|e| match e {
                StoreError::ScanError(exc) => named_err("ScanError", exc.to_string()),
                other => raw_err(other.to_string()),
            })?;
            Ok(Box::new(StoreScan {
                read: Box::new(
                    move |url: &Value,
                          representation: &str,
                          variant_key: Option<&Value>,
                          max_decoded_bytes: i64| {
                        // `type(url) is not str` hits the reader's own guard
                        // before any SQL.
                        let Some(url) = url.as_str() else {
                            return Err(ReadError::Scan(
                                "document URL must be exact nonempty text of at most 8192 characters"
                                    .to_owned(),
                            ));
                        };
                        match read_page_document_content(
                            &con,
                            url,
                            representation,
                            variant_key.and_then(Value::as_str),
                            max_decoded_bytes,
                        ) {
                            Ok(doc) => Ok(Value::Object(doc)),
                            Err(StoreError::ScanError(exc)) => {
                                Err(ReadError::Scan(exc.to_string()))
                            }
                            Err(exc) => Err(ReadError::Fatal(raw_err(exc.to_string()))),
                        }
                    },
                ),
            }) as Box<dyn ScanConnection>)
        },
        &kwargs,
        spellcheck,
    )
}

/// `attach_retained_context` with the scan connection and the reference
/// `**kwargs` dict injected — the seam the reference tests create by patching
/// `open_scan` and `read_page_document_content`.
pub fn attach_retained_context_in(
    tasks: &mut Value,
    input_path: &Path,
    open: ScanOpener<'_>,
    kwargs: &Map<String, Value>,
    spellcheck: &dyn Fn(&SpellcheckArgs<'_>) -> Value,
) -> Result<(), SnippetError> {
    // `representation not in {...}` — an explicit `None` still fails.
    let representation = match kwargs.get("representation") {
        None => "static",
        Some(Value::String(s))
            if ["static", "rendered", "legacy_fragment"].contains(&s.as_str()) =>
        {
            s.as_str()
        }
        Some(_) => {
            return Err(err(
                "representation must be static, rendered, or legacy_fragment",
            ));
        }
    };
    let content_area = kwargs.get("content_area").filter(|v| !v.is_null());
    if kwargs
        .get("content_area")
        .is_some_and(|v| !v.is_null() && !v.is_object())
    {
        return Err(err("content_area must be an object"));
    }
    // `isinstance(bool) or not isinstance(int)` — JSON bools and non-int
    // values both land on the ValueError branch.
    let max_decoded_bytes = match kwargs.get("max_decoded_bytes") {
        None => DEFAULT_RETAINED_BODY_BYTES,
        Some(v) if v.is_boolean() || v.as_i64().is_none() => {
            return Err(err(format!(
                "max_decoded_bytes must be from 1 to {MAX_RETAINED_BODY_BYTES}"
            )));
        }
        Some(v) => {
            let n = v.as_i64().unwrap_or_default();
            if !(1..=MAX_RETAINED_BODY_BYTES).contains(&n) {
                return Err(err(format!(
                    "max_decoded_bytes must be from 1 to {MAX_RETAINED_BODY_BYTES}"
                )));
            }
            n
        }
    };
    let variant_key = kwargs.get("variant_key");
    let query = kwargs.get("query").filter(|v| !v.is_null());
    let ignore_words = kwargs.get("ignore_words");
    // `tasks.get("tasks")` — a non-dict `tasks` dies on the attribute lookup.
    let Some(tasks_obj) = tasks.as_object() else {
        return Err(attr_err(format!(
            "'{}' object has no attribute 'get'",
            crate::pyvalue::py_type_name(tasks)
        )));
    };
    let task_list = match tasks_obj.get("tasks") {
        None => None,
        Some(v) if !crate::pyvalue::truthy(v) => None,
        Some(v) => Some(v.clone()),
    };
    let Some(task_list) = task_list else {
        return Ok(());
    };
    let con = open(input_path)?;
    let tasks_iter = match &task_list {
        Value::Array(items) => items.clone(),
        // Iterating a str or dict yields strings; `task["target_url"]` on a
        // string is the TypeError reference raises inside the loop.
        Value::String(s) if !s.is_empty() => return Err(subscript_err(&json!("x"))),
        Value::Object(o) if !o.is_empty() => return Err(subscript_err(&json!("x"))),
        other => return Err(iterate_err(other)),
    };
    let mut new_tasks: Vec<Value> = Vec::with_capacity(tasks_iter.len());
    for mut task in tasks_iter {
        // `task["target_url"]` — missing key is a KeyError, a non-dict task
        // is the matching subscript TypeError.
        let target_url = match &task {
            Value::Object(o) => o
                .get("target_url")
                .cloned()
                .ok_or_else(|| key_err("target_url"))?,
            other => return Err(subscript_err(other)),
        };
        let document = match con.read_page_document_content(
            &target_url,
            representation,
            variant_key,
            max_decoded_bytes,
        ) {
            Ok(doc) => doc,
            Err(ReadError::Scan(text)) => json!({
                "state": "unavailable",
                "reason": text,
            }),
            Err(ReadError::Fatal(exc)) => return Err(exc),
        };
        let retained =
            inspect_retained_document(&document, query, content_area, ignore_words, spellcheck)?;
        let source = retained.get("source").cloned().unwrap_or(Value::Null);
        let mut identity_parts: Vec<String> = Vec::new();
        for key in [
            "scan_uuid",
            "evidence_revision",
            "document_id",
            "representation",
            "sha256",
            "raw_body_sha256",
        ] {
            if let Some(v) = source.get(key) {
                identity_parts.push(format!("{key}={}", crate::pyvalue::str_or_dumps(v)));
            }
        }
        if let Some(task_obj) = task.as_object_mut() {
            task_obj.insert("retained_document".into(), retained);
            if !identity_parts.is_empty() {
                // `task["goal_proposal"]["summary"] += ...` — KeyErrors on
                // missing keys, the subscript TypeError on non-dicts, and
                // the `+=` TypeError on a non-string summary.
                let proposal = task_obj
                    .get("goal_proposal")
                    .cloned()
                    .ok_or_else(|| key_err("goal_proposal"))?;
                let summary = match &proposal {
                    Value::Object(o) => o
                        .get("summary")
                        .cloned()
                        .ok_or_else(|| key_err("summary"))?,
                    other => return Err(subscript_err(other)),
                };
                match summary {
                    Value::String(text) => {
                        task_obj["goal_proposal"]["summary"] = json!(format!(
                            "{text} Retained source: {}.",
                            identity_parts.join(", ")
                        ));
                    }
                    other => {
                        return Err(type_err(format!(
                            "unsupported operand type(s) for +=: '{}' and 'str'",
                            crate::pyvalue::py_type_name(&other)
                        )));
                    }
                }
            }
        }
        new_tasks.push(task);
    }
    drop(con);
    if let Some(obj) = tasks.as_object_mut() {
        obj.insert("tasks".into(), Value::Array(new_tasks));
    }
    Ok(())
}

/// `attach_retained_measurements`: measure retained page metadata and query
/// excerpts using the same supplied profiles.
pub fn attach_retained_measurements(
    tasks: &mut Value,
    metrics: &Value,
    loader: FontLoader<'_>,
) -> Result<(), SnippetError> {
    // `tasks.get("tasks", [])` — a non-dict `tasks` dies on `.get`.
    let Some(tasks_obj) = tasks.as_object() else {
        return Err(attr_err(format!(
            "'{}' object has no attribute 'get'",
            crate::pyvalue::py_type_name(tasks)
        )));
    };
    let task_list: Vec<Value> = match tasks_obj.get("tasks") {
        None => Vec::new(),
        // `for task in x` — strings/dicts iterate characters/keys,
        // non-iterables raise.
        Some(Value::Array(items)) => items.clone(),
        Some(Value::String(s)) => s.chars().map(|c| json!(c.to_string())).collect(),
        Some(Value::Object(o)) => o.keys().map(|k| json!(k.clone())).collect(),
        Some(other) => return Err(iterate_err(other)),
    };
    let mut rows: Vec<Value> = Vec::new();
    let mut owners: Vec<(usize, Vec<String>)> = Vec::new();
    for (index, task) in task_list.iter().enumerate() {
        // `task.get("retained_document")` — `.get` on a non-dict task is the
        // AttributeError reference raises.
        let Value::Object(task_obj) = task else {
            return Err(attr_err(format!(
                "'{}' object has no attribute 'get'",
                crate::pyvalue::py_type_name(task)
            )));
        };
        let retained = task_obj.get("retained_document");
        let is_complete = retained
            .and_then(Value::as_object)
            .is_some_and(|r| r.get("state").and_then(Value::as_str) == Some("complete"));
        if !is_complete {
            continue;
        }
        let retained_obj = retained
            .and_then(Value::as_object)
            .cloned()
            .unwrap_or_default();
        // `retained.get("metadata", {})` then `metadata.get(key)` — a non-dict
        // metadata dies on `.get`.
        let metadata = retained_obj
            .get("metadata")
            .cloned()
            .unwrap_or_else(|| json!({}));
        let Some(metadata) = metadata.as_object() else {
            return Err(attr_err(format!(
                "'{}' object has no attribute 'get'",
                crate::pyvalue::py_type_name(&metadata)
            )));
        };
        let mut meta_row = Map::new();
        for key in EDITABLE {
            if let Some(v) = metadata.get(key) {
                // `if metadata.get(key)` — reference truthiness (0, False, "",
                // [], {}, None are all skipped).
                if crate::pyvalue::truthy(v) {
                    meta_row.insert(key.into(), v.clone());
                }
            }
        }
        if !meta_row.is_empty() {
            rows.push(Value::Object(meta_row));
            owners.push((index, vec!["retained_document".into(), "metadata".into()]));
        }
        let excerpt = retained_obj
            .get("query_excerpt")
            .cloned()
            .unwrap_or_else(|| json!({}));
        // `excerpt.get("state")` — a non-dict excerpt dies on `.get`.
        let Some(excerpt) = excerpt.as_object() else {
            return Err(attr_err(format!(
                "'{}' object has no attribute 'get'",
                crate::pyvalue::py_type_name(&excerpt)
            )));
        };
        let state = excerpt.get("state").and_then(Value::as_str).unwrap_or("");
        if ["available", "partial"].contains(&state)
            && crate::pyvalue::truthy_opt(excerpt.get("text"))
        {
            rows.push(json!({"description": excerpt["text"]}));
            owners.push((
                index,
                vec!["retained_document".into(), "query_excerpt".into()],
            ));
        }
    }
    if rows.is_empty() {
        return Ok(());
    }
    let measurement = measure_snippet_edits(&rows, metrics, loader)?;
    let measured = measurement
        .get("edits")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    for ((index, path), measured_row) in owners.iter().zip(measured.iter()) {
        if let Some(task) = tasks
            .get_mut("tasks")
            .and_then(Value::as_array_mut)
            .and_then(|a| a.get_mut(*index))
        {
            let mut target = task;
            for key in path.iter().take(path.len().saturating_sub(1)) {
                target = target
                    .get_mut(key)
                    .expect("owner path exists inside a task");
            }
            let leaf = path.last().expect("nonempty owner path");
            if let Some(target_obj) = target.get_mut(leaf).and_then(Value::as_object_mut) {
                if let Some(widths) = measured_row.get("pixel_widths") {
                    target_obj.insert("pixel_widths".into(), widths.clone());
                }
                if let Some(previews) = measured_row.get("previews") {
                    target_obj.insert("previews".into(), previews.clone());
                }
            }
        }
    }
    Ok(())
}

// --- task export -----------------------------------------------------------

/// `build_snippet_tasks`: export imported edits as reviewable, unapplied task
/// candidates.
pub fn build_snippet_tasks(parsed: &Value) -> Result<Value, SnippetError> {
    // `parsed.get("edits")` — a non-dict `parsed` dies on `.get`.
    let Some(parsed_obj) = parsed.as_object() else {
        return Err(attr_err(format!(
            "'{}' object has no attribute 'get'",
            crate::pyvalue::py_type_name(parsed)
        )));
    };
    let (edits, errors, source_sha256) = (
        parsed_obj.get("edits").and_then(Value::as_array),
        parsed_obj.get("errors").and_then(Value::as_array),
        parsed_obj.get("source_sha256").and_then(Value::as_str),
    );
    let (Some(edits), Some(errors), Some(source_sha256)) = (edits, errors, source_sha256) else {
        return Err(err("parsed snippet edits need edits and errors lists"));
    };
    let mut tasks: Vec<Value> = Vec::new();
    for edit in edits {
        // `{field: edit[field] for field in EDITABLE if field in edit}` runs
        // before `edit["url"]`; on non-dict edits `in`/`[]` raise first.
        let mut changes = Map::new();
        match edit {
            Value::Object(edit_obj) => {
                for field in EDITABLE {
                    if let Some(v) = edit_obj.get(field) {
                        changes.insert(field.into(), v.clone());
                    }
                }
            }
            Value::String(s) => {
                for field in EDITABLE {
                    if s.contains(field) {
                        return Err(subscript_err(edit));
                    }
                }
                return Err(subscript_err(edit));
            }
            Value::Array(items) => {
                for field in EDITABLE {
                    if items
                        .iter()
                        .any(|e| crate::pyvalue::py_eq(e, &json!(field)))
                    {
                        return Err(subscript_err(edit));
                    }
                }
                return Err(subscript_err(edit));
            }
            other => return Err(contains_err(other)),
        }
        // `edit["url"]`/`edit["line"]` — missing keys are reference KeyErrors.
        let empty = Map::new();
        let edit_obj = edit.as_object().unwrap_or(&empty);
        let url_value = edit_obj.get("url").cloned().ok_or_else(|| key_err("url"))?;
        let line_value = edit_obj
            .get("line")
            .cloned()
            .ok_or_else(|| key_err("line"))?;
        let identity = crate::pyvalue::dumps_sorted(&json!({
            "url": url_value,
            "edits": changes,
        }));
        let digest = Sha256::digest(identity.as_bytes());
        let task_id = format!(
            "SNIPPET-{}",
            digest
                .iter()
                .take(6)
                .map(|b| format!("{b:02x}"))
                .collect::<String>()
        );
        // `"; ".join(f"{key}: {value}")` — str(value) per change, in the
        // EDITABLE order the dict carries.
        let proposal = changes
            .iter()
            .map(|(key, value)| format!("{key}: {}", crate::pyvalue::str_or_dumps(value)))
            .collect::<Vec<_>>()
            .join("; ");
        // f-string `str(edit['url'])` — raw text for strings, `str()` for
        // other JSON values.
        let url_text = crate::pyvalue::str_or_dumps(&url_value);
        tasks.push(json!({
            "id": task_id,
            "type": "snippet_edit_review",
            "state": "candidate",
            "target_url": url_value,
            "source_line": line_value,
            "source_artifact": {
                "kind": "snippet_csv",
                "content_sha256": source_sha256,
            },
            "edits": changes,
            "goal_proposal": {
                "title": format!("Review search snippet for {url_text}"),
                "summary": format!(
                    "Review the proposed snippet for {url_text}: {proposal}. Source CSV line: {}; content SHA-256: {source_sha256}.",
                    crate::pyvalue::str_or_dumps(&line_value)
                ),
                "references": [format!("section:snippet-{}", task_id.to_lowercase())],
            },
            "instruction": "Review the proposed title and description; apply only after approval.",
        }));
    }
    Ok(json!({
        "schema_version": TASKS_SCHEMA,
        "source_artifact": {"kind": "snippet_csv", "content_sha256": source_sha256},
        "summary": {"tasks": tasks.len(), "rejected_rows": errors.len()},
        "tasks": tasks,
        "errors": errors,
    }))
}

/// `write_snippet_tasks`: write a new task JSON file without replacing an
/// existing file.
pub fn write_snippet_tasks(tasks: &Value, out: &Path) -> Result<String, SnippetError> {
    if out.exists()
        || out
            .symlink_metadata()
            .map(|m| m.file_type().is_symlink())
            .unwrap_or(false)
    {
        return Err(named_err(
            "FileExistsError",
            format!("snippet task output already exists: {}", out.display()),
        ));
    }
    if let Some(parent) = out.parent() {
        // A bare filename has an empty parent; `Path("").mkdir` in reference is
        // `Path(".")` — a no-op — while Rust errors on the empty path.
        if !parent.as_os_str().is_empty() {
            std::fs::create_dir_all(parent).map_err(|e| raw_err(e.to_string()))?;
        }
    }
    let payload = format!(
        "{}\n",
        serde_json::to_string_pretty(tasks).map_err(|e| raw_err(e.to_string()))?
    );
    match std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(out)
    {
        Ok(mut handle) => {
            use std::io::Write;
            handle
                .write_all(payload.as_bytes())
                .map_err(|e| raw_err(e.to_string()))?;
        }
        Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {
            return Err(named_err(
                "FileExistsError",
                format!("[Errno 17] File exists: '{}'", out.display()),
            ));
        }
        Err(e) => return Err(raw_err(e.to_string())),
    }
    Ok(out.display().to_string())
}
