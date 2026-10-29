//! Stream occurrence-level content evidence from one retained scan.v1 artifact.
//!
//!
//! The module deliberately reads only retained bytes. It never fetches a URL, recovers an omitted
//! body, or treats a missing/corrupt body as an absence. A caller may write records through
//! `on_record`; this reader keeps no corpus-sized result list in memory.
//!
//! Offsets are Unicode code points (zero-based, end-exclusive).

use std::collections::BTreeMap;
use std::path::Path;
use std::sync::OnceLock;
use std::time::{Duration, Instant};

use regex::Regex;
use rusqlite::types::Value as SqlValue;
use rusqlite::{params_from_iter, Connection};
use serde_json::{json, Map, Value};
use sha2::{Digest, Sha256};

use crate::error::StoreError;
use crate::scan_artifact::{open_scan, ScanError, READ_TIMEOUT_SECONDS};

/// Longest query accepted.
pub const MAX_QUERY_CHARS: i64 = 512;
/// Largest decoded document searched.
pub const MAX_DOCUMENT_BYTES: i64 = 5242880;
/// Longest snippet.
pub const MAX_SNIPPET_CHARS: i64 = 320;
/// Occurrences listed per document.
pub const MAX_OCCURRENCES_PER_DOCUMENT: i64 = 10000;
/// Longest context window around a match.
pub const MAX_MATCH_WINDOW_CHARS: i64 = 4000;
/// Largest serialized record.
pub const MAX_SERIALIZED_RECORD_BYTES: i64 = 65536;
/// Credential-shaped tokens redacted from snippets and windows.
pub const REDACTION_PATTERN: &str = r#"(?i)(?:\b(?:set[-_ ]?cookie|cookie)\s*[:=]\s*[^\r\n]+|\b(?:authorization|proxy[-_ ]?authorization|access[-_ ]?token|refresh[-_ ]?token|id[-_ ]?token|client[-_ ]?secret|api[-_ ]?key|secret[-_ ]?key|token|secret|password|passwd|credential|private[-_ ]?key)(?:[\"'])?\s*[:=]\s*(?:bearer\s+)?(?:'[^']*'|\"[^\"]*\"|[^\s,;<'\">]+)|\bbearer\s+[A-Za-z0-9._~+/=-]{8,})"#;
/// Valid search scopes.
pub const SCOPES: &[&str] = &["body_text", "head_markup", "raw_html", "selector_markup"];
/// Valid representations.
pub const REPRESENTATIONS: &[&str] = &["rendered", "static"];
/// Valid modes.
pub const MODES: &[&str] = &["contains", "not_contains"];
/// Valid query kinds.
pub const KINDS: &[&str] = &["literal", "regex"];

/// Wall-clock budget of one document's matching (the custom-extraction default).
const MATCH_BUDGET: Duration = Duration::from_secs(2);

fn invalid(message: impl Into<String>) -> StoreError {
    StoreError::Invalid(message.into())
}

/// Quoted text form of a string.
fn py_repr(text: &str) -> String {
    let quote = if text.contains('\'') && !text.contains('"') {
        '"'
    } else {
        '\''
    };
    let mut out = String::from(quote);
    for c in text.chars() {
        match c {
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if c == quote => {
                out.push('\\');
                out.push(c);
            }
            c if (c as u32) < 0x20 || c as u32 == 0x7f => {
                out.push_str(&format!("\\x{:02x}", c as u32))
            }
            c => out.push(c),
        }
    }
    out.push(quote);
    out
}

/// Validate the search arguments; returns the representations and the (possibly empty) selector.
pub fn arguments(
    query: &str,
    scope: &str,
    mode: &str,
    representations: &[String],
    selector: Option<&str>,
    kind: &str,
) -> Result<(Vec<String>, String), StoreError> {
    if query.trim().is_empty() {
        return Err(invalid("content search query must be a nonempty string"));
    }
    if query.chars().count() as i64 > MAX_QUERY_CHARS {
        return Err(invalid(format!(
            "content search query exceeds {MAX_QUERY_CHARS} characters"
        )));
    }
    if !SCOPES.contains(&scope) {
        return Err(invalid(format!(
            "unknown content search scope {}",
            py_repr(scope)
        )));
    }
    if !MODES.contains(&mode) {
        return Err(invalid(format!(
            "unknown content search mode {}",
            py_repr(mode)
        )));
    }
    if !KINDS.contains(&kind) {
        return Err(invalid(format!(
            "unknown content search kind {}",
            py_repr(kind)
        )));
    }
    if kind == "regex" {
        // Syntax is checked once here; a runtime failure per document is a budget or decode
        // problem, never a silent "no match".
        if let Err(e) = fancy_regex::Regex::new(query) {
            return Err(invalid(format!("invalid regex {}: {e}", py_repr(query))));
        }
    }
    let mut unique: Vec<&String> = representations.iter().collect();
    unique.sort();
    unique.dedup();
    if representations.is_empty()
        || unique.len() != representations.len()
        || representations
            .iter()
            .any(|r| !REPRESENTATIONS.contains(&r.as_str()))
    {
        return Err(invalid(
            "representations must be a unique nonempty subset of static,rendered",
        ));
    }
    let selector = selector.unwrap_or("");
    if scope == "selector_markup" {
        if selector.is_empty() {
            return Err(invalid("selector_markup scope requires a CSS selector"));
        }
        seam_validate_selector(selector)?;
    } else if !selector.is_empty() {
        return Err(invalid("selector is only valid for selector_markup scope"));
    }
    Ok((representations.to_vec(), selector.to_string()))
}

/// `custom_search._validate_selector_syntax("element", selector)`, via the parse crate.
fn seam_validate_selector(selector: &str) -> Result<(), StoreError> {
    spiderhead_parse::custom_search::validate_css_selector(selector)
        .map_err(|e| StoreError::Invalid(e.to_string()))
}

/// `custom_search._target_text({"html": html}, ...)` for the scopes that need an HTML parser.
fn seam_target_text(html: &str, scope: &str, selector: &str) -> Result<String, StoreError> {
    spiderhead_parse::custom_search::html_target_text(html, scope, selector)
        .map_err(|e| StoreError::Invalid(e.to_string()))
}

/// The string a query is matched against, for one document.
fn target_text(html: &str, scope: &str, selector: &str) -> Result<String, StoreError> {
    match scope {
        "raw_html" => Ok(html.to_string()),
        other => seam_target_text(html, other, selector),
    }
}

/// Code-point text with byte-offset conversion for increasing offsets.
pub struct Text {
    /// The characters.
    pub chars: Vec<char>,
}

impl Text {
    /// Index the characters of `text`.
    pub fn new(text: &str) -> Self {
        Text {
            chars: text.chars().collect(),
        }
    }

    fn slice(&self, start: usize, end: usize) -> String {
        self.chars[start.min(self.chars.len())..end.min(self.chars.len())]
            .iter()
            .collect()
    }
}

/// Converts increasing byte offsets of one string to code-point offsets.
struct Offsets<'a> {
    text: &'a str,
    byte: usize,
    chars: usize,
}

impl<'a> Offsets<'a> {
    fn new(text: &'a str) -> Self {
        Offsets {
            text,
            byte: 0,
            chars: 0,
        }
    }

    fn at(&mut self, byte: usize) -> usize {
        if byte < self.byte {
            self.byte = 0;
            self.chars = 0;
        }
        self.chars += self.text[self.byte..byte].chars().count();
        self.byte = byte;
        self.chars
    }
}

/// Whitespace for word splitting: Unicode whitespace plus U+001C..U+001F.
fn is_py_space(c: char) -> bool {
    c.is_whitespace() || ('\u{1c}'..='\u{1f}').contains(&c)
}

fn collapse(text: &str) -> String {
    text.split(is_py_space)
        .filter(|p| !p.is_empty())
        .collect::<Vec<_>>()
        .join(" ")
}

/// Credential-shaped spans: start and end code-point offsets.
pub type Spans = (Vec<usize>, Vec<usize>);

fn redaction_regex() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(REDACTION_PATTERN).expect("static pattern"))
}

/// Offsets of every credential-shaped token in `target`.
pub fn redaction_spans(target: &str) -> Spans {
    let mut offsets = Offsets::new(target);
    let (mut starts, mut ends) = (Vec::new(), Vec::new());
    for m in redaction_regex().find_iter(target) {
        starts.push(offsets.at(m.start()));
        ends.push(offsets.at(m.end()));
    }
    (starts, ends)
}

/// Redact credential-shaped tokens intersecting one bounded source slice.
pub fn redact_slice(target: &Text, start: usize, end: usize, spans: &Spans) -> String {
    let (starts, ends) = spans;
    let mut index = starts.partition_point(|s| *s <= start).saturating_sub(1);
    let mut cursor = start;
    let mut pieces = String::new();
    while index < starts.len() && starts[index] < end {
        let (token_start, token_end) = (starts[index], ends[index]);
        if token_end > start {
            let overlap_start = start.max(token_start);
            let overlap_end = end.min(token_end);
            if overlap_start > cursor {
                pieces.push_str(&target.slice(cursor, overlap_start));
            }
            if overlap_end > cursor {
                pieces.push_str("[REDACTED]");
                cursor = overlap_end;
            }
        }
        index += 1;
    }
    if cursor < end {
        pieces.push_str(&target.slice(cursor, end));
    }
    pieces
}

/// Return a bounded match excerpt without commonly shaped credential values.
///
/// `span` is the match position for regex searches; literal searches locate the marker
/// themselves.
pub fn snippet(
    target: &Text,
    query: &str,
    case_sensitive: bool,
    span: Option<(usize, usize)>,
    redaction: Option<&Spans>,
) -> String {
    let (start, end) = match span {
        Some(s) => s,
        None => {
            let haystack: Vec<char> = if case_sensitive {
                target.chars.clone()
            } else {
                target.chars.iter().flat_map(|c| c.to_lowercase()).collect()
            };
            let needle: Vec<char> = if case_sensitive {
                query.chars().collect()
            } else {
                query.chars().flat_map(|c| c.to_lowercase()).collect()
            };
            let Some(start) = find_chars(&haystack, &needle) else {
                return String::new();
            };
            (start, start + query.chars().count())
        }
    };
    let left = start.saturating_sub((MAX_SNIPPET_CHARS / 3) as usize);
    let right = target
        .chars
        .len()
        .min(end + (MAX_SNIPPET_CHARS * 2 / 3) as usize);
    let computed;
    let spans = match redaction {
        Some(s) => s,
        None => {
            computed = redaction_spans(&target.chars.iter().collect::<String>());
            &computed
        }
    };
    let value = collapse(&redact_slice(target, left, right, spans));
    value.chars().take(MAX_SNIPPET_CHARS as usize).collect()
}

fn find_chars(haystack: &[char], needle: &[char]) -> Option<usize> {
    if needle.is_empty() {
        return Some(0);
    }
    haystack.windows(needle.len()).position(|w| w == needle)
}

/// Public provenance of one search.
pub fn source(header: &Map<String, Value>, query: &str, active: bool) -> Map<String, Value> {
    let get = |k: &str| header.get(k).cloned().unwrap_or(Value::Null);
    let mut out = Map::new();
    out.insert("kind".into(), json!("scan.v1.retained-content-search"));
    out.insert("search_id".into(), json!(uuid::Uuid::new_v4().to_string()));
    out.insert("scan_uuid".into(), get("scan_uuid"));
    out.insert("evidence_revision".into(), get("evidence_revision"));
    out.insert("lifecycle".into(), get("lifecycle"));
    out.insert("active".into(), json!(active));
    out.insert(
        "query_sha256".into(),
        json!(Sha256::digest(query.as_bytes())
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect::<String>()),
    );
    out.insert("writer_revision".into(), get("writer_revision"));
    out
}

/// Bounded match positions of one document.
pub struct Occurrences {
    /// Up to [`MAX_OCCURRENCES_PER_DOCUMENT`] code-point spans.
    pub spans: Vec<(usize, usize)>,
    /// Every match counted.
    pub total: i64,
    /// More matches than the output cap.
    pub count_limited: bool,
    /// The wall-clock budget ended the pass.
    pub timed_out: bool,
    /// Why, when it did.
    pub timeout_reason: Option<String>,
}

/// Bounded offsets, total count, output-cap state, and timeout state.
///
/// A literal query matches overlapping occurrences; a
/// regex query uses `pattern` (a per-document wall-clock budget bounds both).
pub fn occurrence_spans(
    target: &str,
    query: &str,
    case_sensitive: bool,
    pattern: Option<&fancy_regex::Regex>,
) -> Occurrences {
    let mut result = Occurrences {
        spans: Vec::new(),
        total: 0,
        count_limited: false,
        timed_out: false,
        timeout_reason: None,
    };
    let deadline = Instant::now() + MATCH_BUDGET;
    let mut offsets = Offsets::new(target);
    let record = |result: &mut Occurrences, start: usize, end: usize, offsets: &mut Offsets<'_>| {
        result.total += 1;
        if (result.spans.len() as i64) < MAX_OCCURRENCES_PER_DOCUMENT {
            result.spans.push((offsets.at(start), offsets.at(end)));
        }
    };
    match pattern {
        Some(re) => {
            for found in re.find_iter(target) {
                match found {
                    Ok(m) => record(&mut result, m.start(), m.end(), &mut offsets),
                    Err(_) => {
                        result.timed_out = true;
                        break;
                    }
                }
                if Instant::now() > deadline {
                    result.timed_out = true;
                    break;
                }
            }
        }
        None => {
            let literal = Regex::new(&format!(
                "{}{}",
                if case_sensitive { "" } else { "(?i)" },
                regex::escape(query)
            ))
            .expect("escaped literal");
            let mut from = 0usize;
            while from <= target.len() {
                let Some(m) = literal.find_at(target, from) else {
                    break;
                };
                record(&mut result, m.start(), m.end(), &mut offsets);
                // overlapping matches: resume one character after the match start
                let step = target[m.start()..].chars().next().map_or(1, char::len_utf8);
                from = m.start() + step;
                if Instant::now() > deadline {
                    result.timed_out = true;
                    break;
                }
            }
        }
    }
    result.count_limited = result.total > MAX_OCCURRENCES_PER_DOCUMENT;
    if result.timed_out {
        result.timeout_reason = Some(
            if pattern.is_some() {
                "regex_budget_exceeded"
            } else {
                "match_budget_exceeded"
            }
            .into(),
        );
    }
    result
}

/// Serialized size of a record plus its trailing newline.
pub fn serialized_record_bytes(record: &Map<String, Value>) -> i64 {
    serde_json::to_string(record).map_or(0, |s| s.len() as i64 + 1)
}

/// Update the occurrence bookkeeping fields of a record.
pub fn set_occurrence_output(
    record: &mut Map<String, Value>,
    occurrence_count: i64,
    count_limited: bool,
    record_limited: bool,
) {
    let returned = record["occurrences"].as_array().map_or(0, Vec::len) as i64;
    let mut reasons: Vec<&str> = Vec::new();
    if count_limited {
        reasons.push("occurrence_count_limit");
    }
    if record_limited {
        reasons.push("serialized_record_limit");
    }
    record.insert("occurrences_returned".into(), json!(returned));
    record.insert(
        "occurrences_omitted".into(),
        json!((occurrence_count - returned).max(0)),
    );
    record.insert(
        "occurrence_truncated".into(),
        json!(!reasons.is_empty() || returned < occurrence_count),
    );
    record.insert("occurrence_truncation_reasons".into(), json!(reasons));
}

/// Offsets where each line starts (after `\r\n`, `\r` or `\n`).
pub fn line_starts(target: &Text) -> Vec<usize> {
    let chars = &target.chars;
    let mut starts = vec![0usize];
    let mut index = 0;
    while index < chars.len() {
        if chars[index] == '\r' {
            index += if chars.get(index + 1) == Some(&'\n') {
                2
            } else {
                1
            };
            starts.push(index);
        } else if chars[index] == '\n' {
            index += 1;
            starts.push(index);
        } else {
            index += 1;
        }
    }
    starts
}

/// Return a six-line context window and its one-based first line: `(line, text, truncated)`.
pub fn match_window(
    target: &Text,
    starts: &[usize],
    span: (usize, usize),
    redaction: &Spans,
) -> (usize, String, bool) {
    let bisect = |x: usize| starts.partition_point(|s| *s <= x);
    let match_line = bisect(span.0);
    let mut start_line = match_line.saturating_sub(3).max(1);
    let end_line = starts.len().min(start_line + 5);
    if end_line - start_line < 5 {
        start_line = end_line.saturating_sub(5).max(1);
    }
    let start = starts[start_line - 1];
    let mut end = if end_line < starts.len() {
        starts[end_line]
    } else {
        target.chars.len()
    };
    if end_line < starts.len() {
        let crlf = end >= 2 && target.chars[end - 2] == '\r' && target.chars[end - 1] == '\n';
        end -= if crlf { 2 } else { 1 };
    }
    let window = MAX_MATCH_WINDOW_CHARS as usize;
    let truncated = end - start > window;
    let (mut left, mut right) = (start, end);
    if truncated {
        let content = window - 2;
        left = start.max(span.0.saturating_sub(content / 2));
        right = end.min(left + content);
        left = start.max(right.saturating_sub(content));
        if right < span.1 {
            right = end.min(span.1);
            left = start.max(right.saturating_sub(content));
        }
    }
    let mut value = redact_slice(target, left, right, redaction);
    if truncated {
        value = format!(
            "{}{}{}",
            if left > start { "…" } else { "" },
            value,
            if right < end { "…" } else { "" }
        );
    }
    let value: String = value.chars().take(window).collect();
    (bisect(left), value, truncated)
}

/// SQL and parameters of the observation stream: the newest retained document per URL and
/// representation, plus an explicit unavailable row for every page without one.
pub fn row_stream(representations: &[String], url: Option<&str>) -> (String, Vec<SqlValue>) {
    let requested = vec!["(?)"; representations.len()].join(",");
    let url_predicate = if url.is_some() { "u.url=?" } else { "1" };
    let sql = format!(
        "WITH requested(representation) AS (VALUES {requested}), latest AS (SELECT d.*,ROW_NUMBER() OVER (PARTITION BY d.url_id,d.representation ORDER BY d.document_id DESC) AS position FROM documents d JOIN requested USING(representation) JOIN urls u USING(url_id) WHERE {url_predicate}), observations AS (SELECT l.document_id,l.url_id,l.representation,l.body_state,l.body_reason,l.fidelity,l.decoder_errors,l.decoder_source,u.url,p.page_ordinal,p.status_code,COALESCE(r.content_type,'text/html') AS content_type FROM latest l JOIN urls u USING(url_id) LEFT JOIN pages p USING(url_id) LEFT JOIN responses r ON r.response_id=l.source_response_id WHERE l.position=1 UNION ALL SELECT NULL,p.url_id,requested.representation,'unavailable',CASE requested.representation WHEN 'static' THEN COALESCE((SELECT r.body_reason FROM responses r WHERE r.request_url_id=p.url_id AND r.purpose='page' ORDER BY r.response_id DESC LIMIT 1),'not_in_corpus') ELSE 'not_rendered' END,'unavailable','unknown','not_applicable',u.url,p.page_ordinal,p.status_code,p.content_type FROM pages p JOIN urls u USING(url_id) CROSS JOIN requested WHERE {url_predicate} AND NOT EXISTS (SELECT 1 FROM documents d WHERE d.url_id=p.url_id AND d.representation=requested.representation)) SELECT * FROM observations ORDER BY page_ordinal IS NULL,page_ordinal,url,representation"
    );
    let mut params: Vec<SqlValue> = representations
        .iter()
        .map(|r| SqlValue::Text(r.clone()))
        .collect();
    if let Some(u) = url {
        params.push(SqlValue::Text(u.to_string()));
        params.push(SqlValue::Text(u.to_string()));
    }
    (sql, params)
}

/// Thin wrapper over `bodies::read_document`, which returns a `Result`.
fn read_document(con: &Connection, document_id: i64) -> Result<String, StoreError> {
    crate::bodies::read_document(con, document_id, MAX_DOCUMENT_BYTES)
}

/// Consumer of emitted records; return `Some(false)` to stop the stream.
pub type RecordSink<'a> = dyn FnMut(&Map<String, Value>) -> Option<bool> + 'a;

/// Arguments of [`search_scan`].
#[derive(Debug, Clone)]
pub struct SearchOptions {
    /// What to search: `raw_html`, `head_markup`, `body_text` or `selector_markup`.
    pub scope: String,
    /// `contains` or `not_contains`.
    pub mode: String,
    /// `static` and/or `rendered`.
    pub representations: Vec<String>,
    /// CSS selector for `selector_markup`.
    pub selector: Option<String>,
    /// Case-sensitive matching.
    pub case_sensitive: bool,
    /// Allow searching an unfinished scan.
    pub allow_active: bool,
    /// Attach snippets and context windows.
    pub include_snippets: bool,
    /// `literal` or `regex`.
    pub kind: String,
    /// Restrict to one exact logical URL.
    pub url: Option<String>,
}

impl Default for SearchOptions {
    fn default() -> Self {
        SearchOptions {
            scope: "raw_html".into(),
            mode: "contains".into(),
            representations: vec!["static".into()],
            selector: None,
            case_sensitive: false,
            allow_active: false,
            include_snippets: false,
            kind: "literal".into(),
            url: None,
        }
    }
}

fn failure(
    query: &str,
    options: &SearchOptions,
    selected: &[String],
    error: &str,
) -> Map<String, Value> {
    json!({
        "ok": false,
        "search_completed": false,
        "source": {
            "kind": "scan.v1.retained-content-search",
            "search_id": uuid::Uuid::new_v4().to_string(),
            "scan_uuid": null,
            "evidence_revision": null,
            "query_sha256": Sha256::digest(query.as_bytes()).iter().map(|b| format!("{b:02x}")).collect::<String>(),
        },
        "scope": options.scope,
        "mode": options.mode,
        "kind": options.kind,
        "representations": selected,
        "coverage": {
            "state": "unavailable",
            "url_pages_selected": 0,
            "documents_selected": 0,
            "documents_measured": 0,
            "filter_matching_documents": 0,
            "present_documents": 0,
            "absent_documents": 0,
            "unavailable_documents": 0,
            "non_html_documents": 0,
            "unavailable_reasons": {},
            "partial_reasons": [],
            "integrity_error": error,
            "stream_error": null,
            "body_byte_limit": MAX_DOCUMENT_BYTES,
            "occurrence_output_limit_per_document": MAX_OCCURRENCES_PER_DOCUMENT,
            "serialized_record_byte_limit": MAX_SERIALIZED_RECORD_BYTES,
            "window_character_limit": MAX_MATCH_WINDOW_CHARS,
            "occurrence_truncated_documents": 0,
            "occurrence_truncation_reasons": {},
            "record_consumer_stopped": false,
            "url_filter": options.url,
        },
        "absence_confirmed": false,
    })
    .as_object()
    .cloned()
    .unwrap_or_default()
}

/// Search a closed retained scan without materializing its corpus.
///
/// `kind="regex"` treats `query` as a regular expression, run per document under a wall-clock
/// budget. A document that exceeds the budget is unavailable (`regex_budget_exceeded`), never
/// an absence.
///
/// `on_record` may return `Some(false)` to stop a streaming consumer. The result then has
/// incomplete coverage and cannot confirm an absence.
///
/// `absence_confirmed` is true only when every selected HTML observation is readable, the marker
/// is absent from all of them, and the source scan itself is complete. Non-HTML, failed,
/// unretained, and corrupt bodies remain named unavailable evidence.
pub fn search_scan(
    scan: &Path,
    query: &str,
    options: &SearchOptions,
    mut on_record: Option<&mut RecordSink<'_>>,
) -> Result<Map<String, Value>, StoreError> {
    let (selected, selector) = arguments(
        query,
        &options.scope,
        &options.mode,
        &options.representations,
        options.selector.as_deref(),
        &options.kind,
    )?;
    if options.url.as_deref().is_some_and(|u| u.trim().is_empty()) {
        return Err(invalid(
            "url must be a nonempty exact logical URL when supplied",
        ));
    }
    let pattern = if options.kind == "regex" {
        let flagged = if options.case_sensitive {
            query.to_string()
        } else {
            format!("(?i){query}")
        };
        Some(
            fancy_regex::Regex::new(&flagged)
                .map_err(|e| invalid(format!("invalid regex {}: {e}", py_repr(query))))?,
        )
    } else {
        None
    };
    if !scan.is_file() {
        return Err(invalid(format!("scan does not exist: {}", scan.display())));
    }
    let con = match open_scan(scan, false, Some(READ_TIMEOUT_SECONDS as f64)) {
        Ok(c) => c,
        Err(StoreError::ScanError(e)) => return Ok(failure(query, options, &selected, &e.message)),
        Err(other) => return Err(other),
    };
    let header =
        crate::discovery_ledger::query_maps(&con, "SELECT * FROM scan WHERE singleton=1", [])?
            .into_iter()
            .next()
            .ok_or_else(|| invalid("scan has no header"))?;
    let lifecycle = header
        .get("lifecycle")
        .and_then(Value::as_str)
        .unwrap_or("");
    let active = lifecycle != "finished";
    if active && !options.allow_active {
        return Err(ScanError {
            message: "content search requires a finished scan; set allow_active deliberately"
                .into(),
        }
        .into());
    }
    let capabilities: Value = serde_json::from_str(
        header
            .get("capabilities_json")
            .and_then(Value::as_str)
            .unwrap_or("{}"),
    )?;
    let src = source(&header, query, active);
    let mut tally = Tally::default();
    let (sql, params) = row_stream(&selected, options.url.as_deref());
    let mut stmt = con.prepare(&sql);
    let mut previous_url: Option<String> = None;
    match &mut stmt {
        Err(e) => tally.stream_error = format!("SQLite read deadline or query failure: {e}"),
        Ok(stmt) => {
            let names: Vec<String> = stmt.column_names().iter().map(|s| s.to_string()).collect();
            match stmt.query(params_from_iter(params.iter())) {
                Err(e) => {
                    tally.stream_error = format!("SQLite read deadline or query failure: {e}")
                }
                Ok(mut rows) => loop {
                    let row = match rows.next() {
                        Ok(Some(r)) => r,
                        Ok(None) => break,
                        Err(e) => {
                            tally.stream_error =
                                format!("SQLite read deadline or query failure: {e}");
                            break;
                        }
                    };
                    let mut item = Map::new();
                    for (i, name) in names.iter().enumerate() {
                        item.insert(
                            name.clone(),
                            crate::link_context::cell_value(row.get_ref(i)?),
                        );
                    }
                    tally.documents += 1;
                    let url = item["url"].as_str().unwrap_or("").to_string();
                    if previous_url.as_deref() != Some(url.as_str()) {
                        tally.url_pages += 1;
                        previous_url = Some(url);
                    }
                    let record = search_row(
                        &con,
                        &item,
                        query,
                        options,
                        &selector,
                        pattern.as_ref(),
                        &src,
                        &mut tally,
                    )?;
                    if serialized_record_bytes(&record) > MAX_SERIALIZED_RECORD_BYTES {
                        return Err(invalid(
                            "content-search record exceeds the 64 KiB serialized record limit",
                        ));
                    }
                    if record["status"] != "unavailable"
                        && record["occurrence_truncated"] == json!(true)
                    {
                        tally.occurrence_truncated += 1;
                        for reason in record["occurrence_truncation_reasons"]
                            .as_array()
                            .into_iter()
                            .flatten()
                        {
                            *tally
                                .truncation_reasons
                                .entry(reason.as_str().unwrap_or("").to_string())
                                .or_insert(0) += 1;
                        }
                    }
                    if record["status"] == "unavailable" {
                        *tally
                            .reasons
                            .entry(record["reason"].as_str().unwrap_or("").to_string())
                            .or_insert(0) += 1;
                    }
                    if let Some(consumer) = on_record.as_deref_mut() {
                        if consumer(&record) == Some(false) {
                            tally.consumer_stopped = true;
                            tally.stream_error =
                                "record consumer stopped before all selected documents were emitted".into();
                            break;
                        }
                    }
                },
            }
        }
    }
    drop(stmt);
    let mut partial_reasons: Vec<String> = Vec::new();
    if header
        .get("crawl_partial")
        .is_some_and(crate::ledger::ingest::truthy)
    {
        partial_reasons.push("scan crawl is partial".into());
    }
    let lane = capabilities
        .get("html_bodies")
        .cloned()
        .unwrap_or_else(|| json!({}));
    if lane.get("state").and_then(Value::as_str) != Some("complete") {
        partial_reasons.push(format!(
            "HTML body lane is {}: {}",
            lane.get("state")
                .and_then(Value::as_str)
                .unwrap_or("unknown"),
            lane.get("reason").and_then(Value::as_str).unwrap_or("")
        ));
    }
    if active {
        partial_reasons.push("scan lifecycle is active".into());
    }
    if !tally.stream_error.is_empty() {
        partial_reasons.push(tally.stream_error.clone());
    }
    if tally.documents == 0 {
        partial_reasons.push("no selected URL pages".into());
    }
    if tally.occurrence_truncated > 0 {
        partial_reasons.push(format!(
            "occurrence output truncated in {} document(s): {}",
            tally.occurrence_truncated,
            tally
                .truncation_reasons
                .iter()
                .map(|(r, c)| format!("{r}={c}"))
                .collect::<Vec<_>>()
                .join(", ")
        ));
    }
    let coverage = if tally.unavailable > 0 || tally.non_html > 0 || !partial_reasons.is_empty() {
        if tally.measured == 0 {
            "unavailable"
        } else {
            "partial"
        }
    } else {
        "complete"
    };
    let absence_confirmed =
        tally.measured > 0 && tally.absent == tally.measured && coverage == "complete";
    let counter = |m: &BTreeMap<String, i64>| {
        Value::Object(m.iter().map(|(k, v)| (k.clone(), json!(v))).collect())
    };
    let mut out = Map::new();
    out.insert("ok".into(), json!(coverage == "complete"));
    out.insert(
        "search_completed".into(),
        json!(tally.stream_error.is_empty()),
    );
    out.insert("source".into(), Value::Object(src));
    out.insert("scope".into(), json!(options.scope));
    out.insert("mode".into(), json!(options.mode));
    out.insert("kind".into(), json!(options.kind));
    out.insert("representations".into(), json!(selected));
    out.insert(
        "coverage".into(),
        json!({
            "state": coverage,
            "url_pages_selected": tally.url_pages,
            "documents_selected": tally.documents,
            "documents_measured": tally.measured,
            "filter_matching_documents": tally.filter_matches,
            "present_documents": tally.present,
            "absent_documents": tally.absent,
            "unavailable_documents": tally.unavailable,
            "non_html_documents": tally.non_html,
            "unavailable_reasons": counter(&tally.reasons),
            "partial_reasons": partial_reasons,
            "stream_error": if tally.stream_error.is_empty() { Value::Null } else { json!(tally.stream_error) },
            "body_byte_limit": MAX_DOCUMENT_BYTES,
            "occurrence_output_limit_per_document": MAX_OCCURRENCES_PER_DOCUMENT,
            "serialized_record_byte_limit": MAX_SERIALIZED_RECORD_BYTES,
            "window_character_limit": MAX_MATCH_WINDOW_CHARS,
            "occurrence_truncated_documents": tally.occurrence_truncated,
            "occurrence_truncation_reasons": counter(&tally.truncation_reasons),
            "record_consumer_stopped": tally.consumer_stopped,
            "url_filter": options.url,
        }),
    );
    out.insert("absence_confirmed".into(), json!(absence_confirmed));
    Ok(out)
}

#[derive(Default)]
struct Tally {
    measured: i64,
    present: i64,
    absent: i64,
    filter_matches: i64,
    unavailable: i64,
    non_html: i64,
    documents: i64,
    occurrence_truncated: i64,
    url_pages: i64,
    consumer_stopped: bool,
    stream_error: String,
    reasons: BTreeMap<String, i64>,
    truncation_reasons: BTreeMap<String, i64>,
}

#[allow(clippy::too_many_arguments)]
fn search_row(
    con: &Connection,
    item: &Map<String, Value>,
    query: &str,
    options: &SearchOptions,
    selector: &str,
    pattern: Option<&fancy_regex::Regex>,
    src: &Map<String, Value>,
    tally: &mut Tally,
) -> Result<Map<String, Value>, StoreError> {
    let text = |k: &str| {
        item.get(k)
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string()
    };
    let mut record = Map::new();
    record.insert("scan_uuid".into(), src["scan_uuid"].clone());
    record.insert("evidence_revision".into(), src["evidence_revision"].clone());
    record.insert("url".into(), item["url"].clone());
    record.insert("page_ordinal".into(), item["page_ordinal"].clone());
    record.insert("document_id".into(), item["document_id"].clone());
    record.insert("capture_mode".into(), item["representation"].clone());
    record.insert("status_code".into(), item["status_code"].clone());
    record.insert("presence".into(), Value::Null);
    record.insert("occurrence_count".into(), Value::Null);
    record.insert("occurrence_truncated".into(), Value::Null);
    record.insert("occurrences".into(), json!([]));
    record.insert("occurrences_returned".into(), json!(0));
    record.insert("occurrences_omitted".into(), Value::Null);
    record.insert("occurrence_truncation_reasons".into(), json!([]));
    record.insert(
        "offset_unit".into(),
        json!("unicode_codepoint_zero_based_end_exclusive"),
    );
    record.insert("line_numbering".into(), json!("one_based"));
    record.insert("status".into(), json!("unavailable"));
    record.insert("reason".into(), json!(""));
    let content_type = text("content_type").to_lowercase();
    if text("representation") == "static" && !content_type.contains("html") {
        tally.non_html += 1;
        record.insert("reason".into(), json!("non_html_content_type"));
        return Ok(record);
    }
    let document_id = item.get("document_id").and_then(Value::as_i64);
    if document_id.is_none() || text("body_state") != "complete" {
        tally.unavailable += 1;
        record.insert(
            "reason".into(),
            json!(format!(
                "body_absent/{}/{}",
                text("body_state"),
                text("body_reason")
            )),
        );
        return Ok(record);
    }
    let html = match read_document(con, document_id.unwrap_or(0)) {
        Ok(h) => h,
        Err(StoreError::ScanError(e)) => {
            tally.unavailable += 1;
            record.insert(
                "reason".into(),
                json!(format!("body_integrity_error/{}", e.message)),
            );
            return Ok(record);
        }
        Err(other) => return Err(other),
    };
    let target = target_text(&html, &options.scope, selector)?;
    let found_spans = occurrence_spans(&target, query, options.case_sensitive, pattern);
    let found = found_spans.total > 0;
    // A replacement character or a re-encoded body can prove a positive marker, never a
    // clean site-wide absence.
    let uncertain_absence = !found
        && (text("fidelity") == "reencoded_text"
            || text("decoder_errors") == "unknown"
            || html.contains('\u{fffd}'));
    if found_spans.timed_out {
        tally.unavailable += 1;
        record.insert(
            "reason".into(),
            json!(found_spans
                .timeout_reason
                .unwrap_or_else(|| "match_budget_exceeded".into())),
        );
        return Ok(record);
    }
    if uncertain_absence {
        tally.unavailable += 1;
        record.insert(
            "reason".into(),
            json!("decode_fidelity_unavailable_for_absence"),
        );
        return Ok(record);
    }
    tally.measured += 1;
    let count = found_spans.total;
    let limited = found_spans.count_limited;
    record.insert("presence".into(), json!(found));
    record.insert("occurrence_count".into(), json!(count));
    record.insert("occurrences_omitted".into(), json!(count));
    record.insert("occurrence_truncated".into(), json!(limited));
    if limited {
        record.insert(
            "occurrence_truncation_reasons".into(),
            json!(["occurrence_count_limit"]),
        );
    }
    let mut kept_redaction: Option<Spans> = None;
    if found {
        let chars = Text::new(&target);
        let starts = line_starts(&chars);
        let redaction = if options.include_snippets {
            redaction_spans(&target)
        } else {
            (Vec::new(), Vec::new())
        };
        record.insert("occurrences".into(), json!([]));
        for (index, span) in found_spans.spans.iter().enumerate() {
            let mut occurrence = Map::new();
            occurrence.insert("match_index".into(), json!(index + 1));
            occurrence.insert("match_total".into(), json!(count));
            occurrence.insert("offset_start".into(), json!(span.0));
            occurrence.insert("offset_end".into(), json!(span.1));
            occurrence.insert(
                "line".into(),
                json!(starts.partition_point(|s| *s <= span.0)),
            );
            if options.include_snippets {
                let (line, window, truncated) = match_window(&chars, &starts, *span, &redaction);
                occurrence.insert("window_start_line".into(), json!(line));
                occurrence.insert("window".into(), json!(window));
                occurrence.insert("window_truncated".into(), json!(truncated));
            }
            // The record owns the row list: append in place, never clone it.
            if let Some(Value::Array(rows)) = record.get_mut("occurrences") {
                rows.push(Value::Object(occurrence));
            }
            set_occurrence_output(&mut record, count, limited, false);
            if serialized_record_bytes(&record) > MAX_SERIALIZED_RECORD_BYTES {
                if let Some(Value::Array(rows)) = record.get_mut("occurrences") {
                    rows.pop();
                }
                set_occurrence_output(&mut record, count, limited, true);
                break;
            }
        }
        kept_redaction = Some(redaction);
    } else {
        record.insert("occurrences_omitted".into(), json!(0));
        record.insert("occurrence_truncated".into(), json!(false));
        record.insert("occurrence_truncation_reasons".into(), json!([]));
    }
    if found {
        tally.present += 1;
    } else {
        tally.absent += 1;
    }
    let is_match = if options.mode == "contains" {
        found
    } else {
        !found
    };
    record.insert(
        "status".into(),
        json!(if is_match { "matched" } else { "not_matched" }),
    );
    if is_match {
        tally.filter_matches += 1;
        if found && options.include_snippets {
            let chars = Text::new(&target);
            record.insert(
                "snippet".into(),
                json!(snippet(
                    &chars,
                    query,
                    options.case_sensitive,
                    found_spans.spans.first().copied(),
                    kept_redaction.as_ref()
                )),
            );
        }
    }
    while record["occurrences"]
        .as_array()
        .is_some_and(|a| !a.is_empty())
        && serialized_record_bytes(&record) > MAX_SERIALIZED_RECORD_BYTES
    {
        if let Some(Value::Array(a)) = record.get_mut("occurrences") {
            a.pop();
        }
        set_occurrence_output(&mut record, count, limited, true);
    }
    Ok(record)
}

#[cfg(test)]
mod tests {
    use super::*;

    const HTML: &str = "<!doctype html><html><head><title>Page T</title></head>\
        <body><p class=\"a\">Hello <b>world</b></p><script>var x = 1;</script></body></html>";

    // review: parser-backed scopes resolve through `_target_text`; selector syntax is
    // validated before any page is scanned.
    #[test]
    fn parser_scopes_resolve_instead_of_panicking() {
        assert_eq!(target_text(HTML, "raw_html", "").unwrap(), HTML);
        assert_eq!(target_text(HTML, "body_text", "").unwrap(), "Hello world");
        assert_eq!(
            target_text(HTML, "head_markup", "").unwrap(),
            "<head><title>Page T</title></head>"
        );
        assert_eq!(
            target_text(HTML, "selector_markup", "p.a").unwrap(),
            "<p class=\"a\">Hello <b>world</b></p>"
        );
    }

    #[test]
    fn selector_syntax_is_validated_before_scanning() {
        assert!(seam_validate_selector("p.a").is_ok());
        let err = seam_validate_selector("p[").unwrap_err();
        assert!(
            err.to_string().contains("invalid CSS selector 'p[':"),
            "{err}"
        );
    }

    #[test]
    fn unknown_scope_is_an_error_not_a_panic() {
        let err = seam_target_text(HTML, "nope", "").unwrap_err();
        assert!(err.to_string().contains("unknown scope 'nope'"), "{err}");
    }
}
