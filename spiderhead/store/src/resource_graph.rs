//! scan.v2 declared-resource graph, independent of the frozen v1 script/style lane.

use crate::error::StoreError;
use crate::scan_artifact::ScanError;
use indexmap::IndexMap;
use rusqlite::{Connection, OptionalExtension};
use serde_json::Value;
use spiderhead_parse::dom::{self, parse_document};
use spiderhead_parse::parser::tokenize::{document_base_url, DocumentSource};
use std::sync::OnceLock;

/// The `context_items.kind` of resource-graph coverage envelopes.
pub const KIND: &str = "resource_graph_coverage";
/// Allowed document representations.
pub const REPRESENTATIONS: &[&str] = &["legacy_fragment", "rendered", "static"];
/// Occurrence and fetch states.
pub const STATES: &[&str] = &[
    "budget",
    "complete",
    "disabled",
    "empty",
    "excluded",
    "failed",
    "partial",
    "unavailable",
];
/// Regex source for `url(...)` references (case-insensitive; group 2 is the URL); compiled with fancy-regex for the `\1` backreference.
pub const CSS_URL: &str = r#"url\(\s*(['\"]?)(.*?)\1\s*\)"#;
/// Regex source for `@import` targets (case-insensitive; group 1 is the URL).
pub const CSS_IMPORT: &str = r#"@import\s+(?:url\()?\s*['\"]([^'\"]+)['\"]"#;
/// Allowed `integrity` states.
pub const INTEGRITY_STATES: &[&str] = &["absent", "declared", "unknown"];

/// One declared occurrence.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Declaration {
    /// Resource kind: `stylesheet`, `script`, `image`, `font`, `css_url`, ...
    pub kind: String,
    /// Where the declaration was found, e.g. `img[src]` or `style`.
    pub carrier: String,
    /// Trimmed raw value as written.
    pub raw_url: String,
    /// Absolute HTTP(S) URL after resolution against the base.
    pub resolved_url: String,
    /// Declared `integrity` value, when present.
    pub integrity: Option<String>,
    /// `declared`, `absent` or `unknown`.
    pub integrity_state: String,
}

/// Compiled `CSS_URL`; `None` only if the constant stops being a valid pattern (no matches then).
fn css_url_re() -> Option<&'static fancy_regex::Regex> {
    static RE: OnceLock<Option<fancy_regex::Regex>> = OnceLock::new();
    RE.get_or_init(|| fancy_regex::Regex::new(&format!("(?i){CSS_URL}")).ok())
        .as_ref()
}

/// Compiled `CSS_IMPORT`; `None` only if the constant stops being a valid pattern.
fn css_import_re() -> Option<&'static fancy_regex::Regex> {
    static RE: OnceLock<Option<fancy_regex::Regex>> = OnceLock::new();
    RE.get_or_init(|| fancy_regex::Regex::new(&format!("(?i){CSS_IMPORT}")).ok())
        .as_ref()
}

/// Resolve `raw` against `base_url`; keep only absolute HTTP(S) URLs with a host.
fn append_one(
    values: &mut Vec<Declaration>,
    kind: &str,
    carrier: &str,
    raw: &str,
    base_url: &str,
    integrity: Option<&str>,
    integrity_state: &str,
) {
    let value = raw.trim();
    if value.is_empty() {
        return;
    }
    let Ok(resolved) = spiderhead_parse::py_url::urljoin(base_url, value) else {
        return;
    };
    let http = spiderhead_parse::py_url::urlsplit(&resolved)
        .is_ok_and(|parts| matches!(parts.scheme.as_str(), "http" | "https"));
    let has_host = crate::ledger::urlparts::urlsplit(&resolved)
        .hostname()
        .is_some_and(|host| !host.is_empty());
    if !http || !has_host {
        return;
    }
    values.push(Declaration {
        kind: kind.to_owned(),
        carrier: carrier.to_owned(),
        raw_url: value.to_owned(),
        resolved_url: resolved,
        integrity: integrity.map(str::to_owned),
        integrity_state: integrity_state.to_owned(),
    });
}

/// Create the optional v2 resource extension only in an explicit v2 writer.
pub fn ensure_schema(con: &Connection) -> Result<(), StoreError> {
    let version: i64 = con.query_row("PRAGMA user_version", [], |r| r.get(0))?;
    if version != 2 {
        return Ok(());
    }
    con.execute_batch(
        "CREATE TABLE IF NOT EXISTS resource_graph_occurrences (occurrence_id INTEGER PRIMARY KEY,\
         page_url_id INTEGER NOT NULL,source_document_id INTEGER NOT NULL,representation TEXT NOT NULL,\
         ordinal INTEGER NOT NULL,kind TEXT NOT NULL,carrier TEXT NOT NULL,raw_url TEXT NOT NULL,\
         resolved_url TEXT NOT NULL,integrity TEXT,integrity_state TEXT NOT NULL DEFAULT 'unknown',\
         nesting_depth INTEGER NOT NULL,state TEXT NOT NULL,reason TEXT NOT NULL,\
         UNIQUE(page_url_id,source_document_id,representation,ordinal));",
    )?;
    let occurrence_columns = table_columns(con, "resource_graph_occurrences")?;
    for (name, definition) in [
        ("integrity", "TEXT"),
        ("integrity_state", "TEXT NOT NULL DEFAULT 'unknown'"),
    ] {
        if !occurrence_columns.iter().any(|c| c == name) {
            con.execute_batch(&format!(
                "ALTER TABLE resource_graph_occurrences ADD COLUMN {name} {definition};"
            ))?;
        }
    }
    con.execute_batch(
        "CREATE TABLE IF NOT EXISTS resource_graph_fetches (resolved_url TEXT PRIMARY KEY,state TEXT NOT NULL,\
         reason TEXT NOT NULL,status_code INTEGER,content_type TEXT NOT NULL,bytes_received INTEGER NOT NULL,\
         elapsed_seconds REAL,origin_host TEXT NOT NULL,redirects INTEGER NOT NULL,nesting_depth INTEGER NOT NULL);",
    )?;
    let existing = table_columns(con, "resource_graph_fetches")?;
    for (name, definition) in [
        ("final_url", "TEXT NOT NULL DEFAULT ''"),
        ("compression", "TEXT NOT NULL DEFAULT 'unknown'"),
        ("cache_state", "TEXT NOT NULL DEFAULT 'unknown'"),
        ("integrity_state", "TEXT NOT NULL DEFAULT 'unknown'"),
        ("width", "INTEGER"),
        ("height", "INTEGER"),
        ("body_state", "TEXT NOT NULL DEFAULT 'unavailable'"),
        ("delivery_json", "TEXT"),
    ] {
        if !existing.iter().any(|c| c == name) {
            con.execute_batch(&format!(
                "ALTER TABLE resource_graph_fetches ADD COLUMN {name} {definition};"
            ))?;
        }
    }
    con.execute_batch(
        "CREATE INDEX IF NOT EXISTS resource_graph_occurrences_url ON resource_graph_occurrences(resolved_url);\
         CREATE INDEX IF NOT EXISTS resource_graph_pending ON resource_graph_occurrences(occurrence_id) WHERE state='disabled';",
    )?;
    Ok(())
}

/// Column names of `table` in declaration order.
fn table_columns(con: &Connection, table: &str) -> Result<Vec<String>, StoreError> {
    let mut stmt = con.prepare(&format!("PRAGMA table_info({table})"))?;
    let names = stmt
        .query_map([], |r| r.get::<_, String>(1))?
        .collect::<Result<Vec<_>, _>>()?;
    Ok(names)
}

/// Append one raw value with the given kind and carrier.
/// Defaults: no integrity value, integrity state `unknown`.
pub fn append(
    values: &mut Vec<Declaration>,
    kind: &str,
    carrier: &str,
    raw: &str,
    base_url: &str,
    integrity: Option<&str>,
    integrity_state: &str,
) {
    append_one(
        values,
        kind,
        carrier,
        raw,
        base_url,
        integrity,
        integrity_state,
    );
}

/// Each comma-separated `srcset` candidate contributes its first token as an image.
pub fn srcset(
    values: &mut Vec<Declaration>,
    raw: &str,
    base_url: &str,
    integrity: Option<&str>,
    integrity_state: &str,
) {
    for candidate in raw.split(',') {
        let token = candidate.split_whitespace().next().unwrap_or("");
        append_one(
            values,
            "image",
            "srcset",
            token,
            base_url,
            integrity,
            integrity_state,
        );
    }
}

/// `@import` targets and `url(...)` references in a stylesheet text.
/// A `url(...)` whose path ends in a font extension is kind `font`, otherwise `css_url`.
pub fn css(values: &mut Vec<Declaration>, text: &str, base_url: &str, carrier: &str) {
    let import = css_import_re()
        .into_iter()
        .flat_map(|re| re.captures_iter(text).flatten());
    for captures in import {
        if let Some(raw) = captures.get(1) {
            append_one(
                values,
                "css_import",
                carrier,
                raw.as_str(),
                base_url,
                None,
                "unknown",
            );
        }
    }
    let urls = css_url_re()
        .into_iter()
        .flat_map(|re| re.captures_iter(text).flatten());
    for captures in urls {
        let Some(raw) = captures.get(2) else {
            continue;
        };
        let raw = raw.as_str();
        let path = raw.split(['?', '#']).next().unwrap_or("").to_lowercase();
        let font = [".woff", ".woff2", ".ttf", ".otf"]
            .iter()
            .any(|ext| path.ends_with(ext));
        let kind = if font { "font" } else { "css_url" };
        append_one(values, kind, carrier, raw, base_url, None, "unknown");
    }
}

/// Attributes carrying a resource, per element: `(attribute, kind)`.
fn carried(tag: &str) -> &'static [(&'static str, &'static str)] {
    match tag {
        "img" => &[("src", "image"), ("srcset", "image")],
        "source" => &[("src", "media"), ("srcset", "image")],
        "video" => &[("src", "media"), ("poster", "image")],
        "audio" | "track" => &[("src", "media")],
        "iframe" | "embed" => &[("src", "document")],
        "object" => &[("data", "document")],
        "script" => &[("src", "module")],
        _ => &[],
    }
}

/// Extract declared HTTP(S) resources and inline CSS references in document order.
/// Default cap: 20000 occurrences; the second value counts occurrences beyond the cap.
pub fn extract(
    html: Option<&str>,
    base_url: &str,
    max_occurrences: i64,
) -> Result<(Vec<Declaration>, i64), StoreError> {
    let Some(html) = html else {
        return Ok((Vec::new(), 0));
    };
    let soup = parse_document(html);
    let base_url = document_base_url(DocumentSource::Tree(&soup), base_url);
    let mut values = Vec::new();
    for tag in dom::all_elements(&soup) {
        let name = dom::tag_name(&tag);
        let integrity = dom::attr(&tag, "integrity");
        let integrity_state = if integrity.is_some() {
            "declared"
        } else {
            "absent"
        };
        if name == "link" {
            let rel: Vec<String> = dom::attr(&tag, "rel")
                .unwrap_or("")
                .split_whitespace()
                .map(str::to_lowercase)
                .collect();
            let has = |token: &str| rel.iter().any(|r| r == token);
            let kind = if has("stylesheet") {
                Some("stylesheet")
            } else if has("manifest") {
                Some("manifest")
            } else if has("modulepreload") {
                Some("module")
            } else {
                None
            };
            if let (Some(kind), Some(href)) = (kind, dom::attr(&tag, "href")) {
                append_one(
                    &mut values,
                    kind,
                    "link[href]",
                    href,
                    &base_url,
                    integrity,
                    integrity_state,
                );
            }
        }
        for (attribute, kind) in carried(&name) {
            let Some(raw) = dom::attr(&tag, attribute) else {
                continue;
            };
            if *attribute == "srcset" {
                srcset(&mut values, raw, &base_url, integrity, integrity_state);
                continue;
            }
            let script_kind = name == "script"
                && dom::attr(&tag, "type").unwrap_or("").to_lowercase() != "module";
            let kind = if script_kind { "script" } else { kind };
            let carrier = format!("{name}[{attribute}]");
            append_one(
                &mut values,
                kind,
                &carrier,
                raw,
                &base_url,
                integrity,
                integrity_state,
            );
        }
        if let Some(style) = dom::attr(&tag, "style") {
            css(&mut values, style, &base_url, &format!("{name}[style]"));
        }
        if name == "style" {
            css(&mut values, &dom::get_text(&tag, ""), &base_url, "style");
        }
    }
    let cap = usize::try_from(max_occurrences.max(0)).unwrap_or(usize::MAX);
    let omitted = values.len().saturating_sub(cap) as i64;
    values.truncate(cap);
    Ok((values, omitted))
}

/// Persist one representation's declaration population within its capture transaction.
///
/// Returns `None` outside a v2 artifact; otherwise the coverage payload that was stored.
pub fn store_document(
    con: &Connection,
    page_url_id: i64,
    source_document_id: i64,
    representation: &str,
    html: Option<&str>,
) -> Result<Option<IndexMap<String, Value>>, StoreError> {
    let version: i64 = con.query_row("PRAGMA user_version", [], |r| r.get(0))?;
    if version != 2 {
        return Ok(None);
    }
    ensure_schema(con)?;
    let scan_err = |m: &str| {
        StoreError::ScanError(ScanError {
            message: m.to_owned(),
        })
    };
    if !REPRESENTATIONS.contains(&representation) {
        return Err(scan_err("resource graph representation is invalid"));
    }
    let bound: Option<(i64, String)> = con
        .query_row(
            "SELECT url_id,representation FROM documents WHERE document_id=?",
            [source_document_id],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .optional()?;
    if bound.as_ref().map(|(u, r)| (*u, r.as_str())) != Some((page_url_id, representation)) {
        return Err(scan_err("resource graph document binding is invalid"));
    }
    let url: String = con.query_row("SELECT url FROM urls WHERE url_id=?", [page_url_id], |r| {
        r.get(0)
    })?;
    let (values, omitted) = extract(html, &url, 20_000)?;
    con.execute(
        "DELETE FROM resource_graph_occurrences WHERE page_url_id=? AND source_document_id=? AND representation=?",
        (page_url_id, source_document_id, representation),
    )?;
    let (state, reason) = if html.is_none() {
        ("unavailable", "document body was not available")
    } else if omitted > 0 {
        ("partial", "resource declaration cap omitted occurrences")
    } else if values.is_empty() {
        ("empty", "")
    } else {
        ("complete", "")
    };
    for (ordinal, value) in values.iter().enumerate() {
        con.execute(
            "INSERT INTO resource_graph_occurrences(page_url_id,source_document_id,representation,ordinal,kind,carrier,raw_url,resolved_url,integrity,integrity_state,nesting_depth,state,reason) \
             VALUES(?,?,?,?,?,?,?,?,?,?,?,?,?)",
            rusqlite::params![
                page_url_id,
                source_document_id,
                representation,
                ordinal as i64,
                value.kind,
                value.carrier,
                value.raw_url,
                value.resolved_url,
                value.integrity,
                value.integrity_state,
                0,
                "disabled",
                "resource fetch is disabled",
            ],
        )?;
    }
    let mut payload = IndexMap::new();
    payload.insert(
        "schema_version".to_owned(),
        Value::from("resource_graph.v1"),
    );
    payload.insert("page_url_id".to_owned(), Value::from(page_url_id));
    payload.insert(
        "source_document_id".to_owned(),
        Value::from(source_document_id),
    );
    payload.insert("representation".to_owned(), Value::from(representation));
    payload.insert("observed".to_owned(), Value::from(values.len() as i64));
    payload.insert("omitted".to_owned(), Value::from(omitted));
    payload.insert("state".to_owned(), Value::from(state));
    payload.insert("reason".to_owned(), Value::from(reason));
    // Sorted keys, compact separators: the stored text is compared byte for byte.
    let sorted: std::collections::BTreeMap<&String, &Value> = payload.iter().collect();
    let completeness = match state {
        "unavailable" => "unavailable",
        "partial" => "partial",
        _ => "complete",
    };
    con.execute(
        "INSERT OR REPLACE INTO context_items(kind,item_key,payload_version,payload_json,completeness,reason) VALUES(?,?,?,?,?,?)",
        (
            KIND,
            format!("document:{source_document_id}"),
            "scan_context.v1",
            serde_json::to_string(&sorted)?,
            completeness,
            reason,
        ),
    )?;
    Ok(Some(payload))
}

/// Validate one document-bound declaration coverage envelope without extraction.
/// `item` is the `context_items` row (keys `item_key`, `completeness`, `reason`).
pub fn validate_coverage_context(
    con: &Connection,
    item: &IndexMap<String, String>,
    payload: &Value,
) -> Result<(), StoreError> {
    let invalid = || StoreError::Invalid("resource graph coverage context is invalid".to_owned());
    let obj = payload.as_object().ok_or_else(invalid)?;
    const FIELDS: [&str; 7] = [
        "schema_version",
        "page_url_id",
        "source_document_id",
        "representation",
        "observed",
        "omitted",
        "state",
    ];
    let reason_key = obj.get("reason");
    let has_fields =
        obj.len() == 8 && FIELDS.iter().all(|f| obj.contains_key(*f)) && reason_key.is_some();
    let page_url_id = obj.get("page_url_id").and_then(Value::as_i64).unwrap_or(0);
    let source_document_id = obj
        .get("source_document_id")
        .and_then(Value::as_i64)
        .unwrap_or(0);
    let representation = obj
        .get("representation")
        .and_then(Value::as_str)
        .unwrap_or("");
    let observed = obj.get("observed").and_then(Value::as_i64);
    let omitted = obj.get("omitted").and_then(Value::as_i64);
    let state = obj.get("state").and_then(Value::as_str).unwrap_or("");
    let reason = reason_key.and_then(Value::as_str);
    let doc_matches = if source_document_id >= 1 && page_url_id >= 1 {
        con.query_row(
            "SELECT 1 FROM documents WHERE document_id=? AND url_id=? AND representation=?",
            (source_document_id, page_url_id, representation),
            |r| r.get::<_, i64>(0),
        )
        .optional()?
        .is_some()
    } else {
        false
    };
    let valid = has_fields
        && obj.get("schema_version").and_then(Value::as_str) == Some("resource_graph.v1")
        && page_url_id >= 1
        && source_document_id >= 1
        && REPRESENTATIONS.contains(&representation)
        && observed.is_some_and(|v| v >= 0)
        && omitted.is_some_and(|v| v >= 0)
        && matches!(state, "complete" | "partial" | "unavailable" | "empty")
        && reason.is_some()
        && item.get("item_key").map(String::as_str)
            == Some(format!("document:{source_document_id}").as_str())
        && doc_matches;
    if !valid {
        return Err(invalid());
    }
    let observed = observed.unwrap_or(0);
    let omitted = omitted.unwrap_or(0);
    let reason = reason.unwrap_or("");
    let (direct, nested, invalid_nested): (Option<i64>, Option<i64>, Option<i64>) = con.query_row(
        "SELECT SUM(nesting_depth=0),SUM(nesting_depth>0),\
         SUM(nesting_depth>0 AND carrier!='css') FROM resource_graph_occurrences \
         WHERE page_url_id=? AND source_document_id=? AND representation=?",
        (page_url_id, source_document_id, representation),
        |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
    )?;
    let (direct, nested, invalid_nested) = (
        direct.unwrap_or(0),
        nested.unwrap_or(0),
        invalid_nested.unwrap_or(0),
    );
    let expected_completeness = match state {
        "unavailable" => "unavailable",
        "partial" => "partial",
        _ => "complete",
    };
    let item_completeness = item.get("completeness").map(String::as_str).unwrap_or("");
    let item_reason = item.get("reason").map(String::as_str).unwrap_or("");
    let disagrees = invalid_nested != 0
        || observed != direct
        || item_completeness != expected_completeness
        || item_reason != reason
        || (state == "unavailable"
            && (observed != 0
                || omitted != 0
                || direct != 0
                || nested != 0
                || reason != "document body was not available"))
        || (state == "empty"
            && (observed != 0 || omitted != 0 || direct != 0 || nested != 0 || !reason.is_empty()))
        || (state == "complete" && (observed == 0 || omitted != 0 || !reason.is_empty()))
        || (state == "partial"
            && (omitted == 0 || reason != "resource declaration cap omitted occurrences"));
    if disagrees {
        return Err(StoreError::Invalid(
            "resource graph coverage context disagrees with stored declarations".to_owned(),
        ));
    }
    Ok(())
}

/// Read typed resource declarations/fetches without re-extracting or fetching.
/// Defaults: limit 1000, offset 0.
pub fn read(
    con: &Connection,
    limit: i64,
    offset: i64,
) -> Result<IndexMap<String, Value>, StoreError> {
    let version: i64 = con.query_row("PRAGMA user_version", [], |r| r.get(0))?;
    if version != 2 {
        return Ok(unavailable("resource graph was not stored in this scan"));
    }
    let names = table_names(con)?;
    if !(names.contains("resource_graph_occurrences") && names.contains("resource_graph_fetches")) {
        return Ok(unavailable("resource graph extension is absent"));
    }
    if !(1..=10_000).contains(&limit) || offset < 0 {
        return Err(StoreError::Invalid(
            "resource graph limit must be 1..10000 and offset nonnegative".to_owned(),
        ));
    }
    let total: i64 = con.query_row("SELECT COUNT(*) FROM resource_graph_occurrences", [], |r| {
        r.get(0)
    })?;
    let states = group_counts(
        con,
        "SELECT state,COUNT(*) FROM resource_graph_occurrences GROUP BY state",
    )?;
    let occurrences_by_kind = group_counts(
        con,
        "SELECT kind,COUNT(*) FROM resource_graph_occurrences GROUP BY kind ORDER BY kind",
    )?;
    let weights_by_kind = weight_by_kind(con)?;
    let occurrences = crate::resources::dict_rows(
        con,
        &format!(
            "SELECT * FROM resource_graph_occurrences ORDER BY page_url_id,source_document_id,ordinal LIMIT {limit} OFFSET {offset}"
        ),
    )?;
    let fetches = crate::resources::dict_rows(
        con,
        &format!("SELECT * FROM resource_graph_fetches ORDER BY resolved_url LIMIT {limit} OFFSET {offset}"),
    )?;
    let coverage_counts = group_counts(
        con,
        "SELECT completeness,COUNT(*) FROM context_items WHERE kind='resource_graph_coverage' GROUP BY completeness",
    )?;
    let count_of = |key: &str| {
        coverage_counts
            .get(key)
            .and_then(Value::as_i64)
            .unwrap_or(0)
    };
    let coverage_state = if coverage_counts.is_empty() {
        "unavailable"
    } else if count_of("partial") > 0 || count_of("unavailable") > 0 {
        "partial"
    } else {
        "complete"
    };
    let documents: i64 = coverage_counts.values().filter_map(Value::as_i64).sum();
    let mut coverage = IndexMap::new();
    coverage.insert("state".to_owned(), Value::from(coverage_state));
    coverage.insert("documents".to_owned(), Value::from(documents));
    coverage.insert(
        "coverage_counts".to_owned(),
        Value::Object(coverage_counts.into_iter().collect()),
    );
    coverage.insert(
        "occurrence_state_counts".to_owned(),
        Value::Object(states.clone().into_iter().collect()),
    );
    let has_more = offset + (occurrences.len() as i64) < total;
    let by_kind = Value::Object(occurrences_by_kind.clone().into_iter().collect());
    let weights = Value::Object(weights_by_kind.into_iter().collect());
    let mut aggregates = IndexMap::new();
    aggregates.insert("blocks_by_kind".to_owned(), by_kind.clone());
    aggregates.insert(
        "blocks_by_state".to_owned(),
        Value::Object(states.clone().into_iter().collect()),
    );
    aggregates.insert("occurrences_by_kind".to_owned(), by_kind);
    aggregates.insert("weight_by_kind".to_owned(), weights.clone());
    aggregates.insert("unique_resource_weight_by_kind".to_owned(), weights);
    let mut out = IndexMap::new();
    out.insert("state".to_owned(), Value::from(coverage_state));
    out.insert(
        "coverage".to_owned(),
        Value::Object(coverage.into_iter().collect()),
    );
    out.insert("total".to_owned(), Value::from(total));
    out.insert("offset".to_owned(), Value::from(offset));
    out.insert("limit".to_owned(), Value::from(limit));
    out.insert("has_more".to_owned(), Value::Bool(has_more));
    out.insert(
        "occurrences".to_owned(),
        Value::Array(
            occurrences
                .into_iter()
                .map(|m| Value::Object(m.into_iter().collect()))
                .collect(),
        ),
    );
    out.insert(
        "fetches".to_owned(),
        Value::Array(
            fetches
                .into_iter()
                .map(|m| Value::Object(m.into_iter().collect()))
                .collect(),
        ),
    );
    out.insert(
        "aggregates".to_owned(),
        Value::Object(aggregates.into_iter().collect()),
    );
    Ok(out)
}

/// The `{"state": "unavailable", "reason": ...}` envelope.
fn unavailable(reason: &str) -> IndexMap<String, Value> {
    IndexMap::from([
        ("state".to_owned(), Value::from("unavailable")),
        ("reason".to_owned(), Value::from(reason)),
    ])
}

/// Names of all tables in the database.
fn table_names(con: &Connection) -> Result<std::collections::BTreeSet<String>, StoreError> {
    let mut stmt = con.prepare("SELECT name FROM sqlite_master WHERE type='table'")?;
    let names = stmt
        .query_map([], |r| r.get::<_, String>(0))?
        .collect::<Result<_, _>>()?;
    Ok(names)
}

/// Run a two-column `(key, count)` query into an ordered map of key to count.
fn group_counts(con: &Connection, sql: &str) -> Result<IndexMap<String, Value>, StoreError> {
    let mut stmt = con.prepare(sql)?;
    let rows = stmt
        .query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)?)))?
        .collect::<Result<Vec<_>, _>>()?;
    Ok(rows.into_iter().map(|(k, v)| (k, Value::from(v))).collect())
}

/// Weights per kind used by [`read`]: unique URLs, complete URLs and exact bytes per kind.
fn weight_by_kind(con: &Connection) -> Result<IndexMap<String, Value>, StoreError> {
    let mut stmt = con.prepare(
        "SELECT x.kind,COUNT(*),COALESCE(SUM(f.state='complete'),0),\
         SUM(CASE WHEN f.state='complete' THEN f.bytes_received ELSE 0 END) \
         FROM (SELECT DISTINCT kind,resolved_url FROM resource_graph_occurrences) x \
         LEFT JOIN resource_graph_fetches f USING(resolved_url) GROUP BY x.kind ORDER BY x.kind",
    )?;
    let rows = stmt
        .query_map([], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, i64>(1)?,
                r.get::<_, i64>(2)?,
                r.get::<_, Option<i64>>(3)?,
            ))
        })?
        .collect::<Result<Vec<_>, _>>()?;
    let mut out = IndexMap::new();
    for (kind, unique, complete, bytes) in rows {
        let exact = unique == complete;
        let bytes_state = if exact {
            "exact"
        } else if complete > 0 {
            "partial"
        } else {
            "unavailable"
        };
        let entry = IndexMap::from([
            ("unique_urls".to_owned(), Value::from(unique)),
            ("complete_urls".to_owned(), Value::from(complete)),
            ("unmeasured_urls".to_owned(), Value::from(unique - complete)),
            (
                "bytes_received".to_owned(),
                if exact {
                    Value::from(bytes.unwrap_or(0))
                } else {
                    Value::Null
                },
            ),
            ("bytes_state".to_owned(), Value::from(bytes_state)),
        ]);
        out.insert(kind, Value::Object(entry.into_iter().collect()));
    }
    Ok(out)
}

/// Validate the optional v2 graph without imposing it on earlier v2 artifacts.
pub fn validate(con: &Connection) -> Result<(), StoreError> {
    let names = table_names(con)?;
    let present: Vec<&str> = ["resource_graph_occurrences", "resource_graph_fetches"]
        .into_iter()
        .filter(|t| names.contains(*t))
        .collect();
    if present.is_empty() {
        return Ok(());
    }
    if present.len() != 2 {
        return Err(StoreError::Invalid(
            "resource graph schema is incomplete".to_owned(),
        ));
    }
    let occurrence_invalid =
        || StoreError::Invalid("resource graph occurrence is invalid".to_owned());
    for row in crate::resources::dict_rows(con, "SELECT * FROM resource_graph_occurrences")? {
        let text = |k: &str| row.get(k).and_then(Value::as_str).unwrap_or("").to_owned();
        let int = |k: &str| row.get(k).and_then(Value::as_i64);
        let integrity_state = text("integrity_state");
        let integrity_present = row.get("integrity").is_some_and(|v| !v.is_null());
        let doc_ok: Option<i64> = con
            .query_row(
                "SELECT 1 FROM documents WHERE document_id=? AND url_id=? AND representation=?",
                (
                    int("source_document_id").unwrap_or(0),
                    int("page_url_id").unwrap_or(0),
                    text("representation"),
                ),
                |r| r.get(0),
            )
            .optional()?;
        if !REPRESENTATIONS.contains(&text("representation").as_str())
            || !STATES.contains(&text("state").as_str())
            || int("ordinal").is_none_or(|v| v < 0)
            || int("nesting_depth").is_none_or(|v| v < 0)
            || text("kind").is_empty()
            || text("carrier").is_empty()
            || text("raw_url").is_empty()
            || text("resolved_url").is_empty()
            || !INTEGRITY_STATES.contains(&integrity_state.as_str())
            || (integrity_state != "declared" && integrity_present)
            || doc_ok.is_none()
        {
            return Err(occurrence_invalid());
        }
    }
    let gap: Option<i64> = con
        .query_row(
            "SELECT 1 FROM resource_graph_occurrences GROUP BY page_url_id,source_document_id,representation \
             HAVING MIN(ordinal)!=0 OR MAX(ordinal)!=COUNT(*)-1 LIMIT 1",
            [],
            |r| r.get(0),
        )
        .optional()?;
    if gap.is_some() {
        return Err(StoreError::Invalid(
            "resource graph occurrence ordinals are not contiguous".to_owned(),
        ));
    }
    let fetch_invalid = || StoreError::Invalid("resource graph fetch is invalid".to_owned());
    for row in crate::resources::dict_rows(con, "SELECT * FROM resource_graph_fetches")? {
        if let Some(raw) = row.get("delivery_json").and_then(Value::as_str) {
            let delivery_invalid =
                || StoreError::Invalid("resource delivery evidence is invalid".to_owned());
            if raw.chars().count() > 16 * 1024 {
                return Err(delivery_invalid());
            }
            let delivery: Value = serde_json::from_str(raw).map_err(|_| delivery_invalid())?;
            let ok = delivery.is_object()
                && delivery
                    .get("cache")
                    .and_then(Value::as_object)
                    .and_then(|c| c.get("ok"))
                    .is_some_and(Value::is_boolean)
                && delivery
                    .get("font_display")
                    .is_none_or(|v| v.is_null() || v.is_array())
                && delivery
                    .get("unminified")
                    .is_none_or(|v| v.is_null() || v.is_boolean())
                && delivery
                    .get("legacy_js")
                    .is_none_or(|v| v.is_null() || v.is_boolean());
            if !ok {
                return Err(delivery_invalid());
            }
        }
        let text = |k: &str| row.get(k).and_then(Value::as_str).unwrap_or("").to_owned();
        let int = |k: &str| row.get(k).and_then(Value::as_i64);
        let dim_ok =
            |v: Option<&Value>| v.is_none_or(|v| v.is_null() || v.as_i64().is_some_and(|n| n >= 1));
        let dims_ok = (row.get("width").is_none_or(Value::is_null)
            == row.get("height").is_none_or(Value::is_null))
            && dim_ok(row.get("width"))
            && dim_ok(row.get("height"));
        let state_ok = matches!(
            text("state").as_str(),
            "complete" | "partial" | "failed" | "excluded" | "budget"
        );
        let integrity_ok = INTEGRITY_STATES.contains(&text("integrity_state").as_str())
            || text("integrity_state") == "mixed";
        if !state_ok
            || int("bytes_received").is_none_or(|v| v < 0)
            || int("redirects").is_none_or(|v| v < 0)
            || int("nesting_depth").is_none_or(|v| v < 0)
            || !integrity_ok
            || !dims_ok
        {
            return Err(fetch_invalid());
        }
    }
    Ok(())
}

/// Set state and reason on pending occurrences of `url`.
pub fn set_occurrence_state(
    con: &Connection,
    url: &str,
    state: &str,
    reason: &str,
) -> Result<(), StoreError> {
    con.execute(
        "UPDATE resource_graph_occurrences SET state=?,reason=? WHERE resolved_url=? AND state='disabled'",
        (state, reason, url),
    )?;
    Ok(())
}

/// Discard graph facts for requeued pages while retaining their captured corpus history.
pub fn invalidate_pages(con: &Connection, page_url_ids: &[i64]) -> Result<(), StoreError> {
    let mut page_ids: Vec<i64> = page_url_ids.iter().copied().filter(|v| *v > 0).collect();
    page_ids.sort_unstable();
    page_ids.dedup();
    if page_ids.is_empty() {
        return Ok(());
    }
    let names = table_names(con)?;
    if !names.contains("resource_graph_occurrences") {
        return Ok(());
    }
    let mut document_ids: std::collections::BTreeSet<i64> = Default::default();
    let mut urls: std::collections::BTreeSet<String> = Default::default();
    for chunk in chunks(&page_ids, 500) {
        let marks = placeholders(chunk.len());
        {
            let mut stmt = con.prepare(&format!(
                "SELECT document_id FROM documents WHERE url_id IN ({marks})"
            ))?;
            let ids = stmt.query_map(rusqlite::params_from_iter(chunk.iter()), |r| {
                r.get::<_, i64>(0)
            })?;
            for id in ids {
                document_ids.insert(id?);
            }
        }
        {
            let mut stmt = con.prepare(&format!(
                "SELECT DISTINCT resolved_url FROM resource_graph_occurrences WHERE page_url_id IN ({marks})"
            ))?;
            let found = stmt.query_map(rusqlite::params_from_iter(chunk.iter()), |r| {
                r.get::<_, String>(0)
            })?;
            for url in found {
                urls.insert(url?);
            }
        }
        con.execute(
            &format!("DELETE FROM resource_graph_occurrences WHERE page_url_id IN ({marks})"),
            rusqlite::params_from_iter(chunk.iter()),
        )?;
    }
    let document_ids: Vec<i64> = document_ids.into_iter().collect();
    for chunk in chunks(&document_ids, 500) {
        let keys: Vec<String> = chunk.iter().map(|d| format!("document:{d}")).collect();
        let marks = placeholders(keys.len());
        let mut params: Vec<String> = vec![KIND.to_owned()];
        params.extend(keys);
        con.execute(
            &format!("DELETE FROM context_items WHERE kind=? AND item_key IN ({marks})"),
            rusqlite::params_from_iter(params.iter()),
        )?;
    }
    if !names.contains("resource_graph_fetches") {
        return Ok(());
    }
    let urls: Vec<String> = urls.into_iter().collect();
    for chunk in chunks(&urls, 500) {
        let marks = placeholders(chunk.len());
        con.execute(
            &format!("DELETE FROM resource_graph_fetches WHERE resolved_url IN ({marks})"),
            rusqlite::params_from_iter(chunk.iter()),
        )?;
        con.execute(
            &format!(
                "UPDATE resource_graph_occurrences SET state='disabled',\
                 reason='resource fetch invalidated by requeue' WHERE resolved_url IN ({marks})"
            ),
            rusqlite::params_from_iter(chunk.iter()),
        )?;
    }
    Ok(())
}

/// `?,?,...` placeholder list for `n` bound parameters.
fn placeholders(n: usize) -> String {
    vec!["?"; n].join(",")
}

/// Consecutive slices of at most `size` items.
/// Default `size`: 500.
pub fn chunks<T: Clone>(values: &[T], size: usize) -> Vec<Vec<T>> {
    values.chunks(size.max(1)).map(<[T]>::to_vec).collect()
}

/// The one declared integrity state of a URL: `mixed` when several are declared,
/// `unknown` when none. Individual integrity values stay in the occurrence rows.
pub fn fetch_integrity_state(con: &Connection, url: &str) -> Result<String, StoreError> {
    let mut stmt = con.prepare(
        "SELECT DISTINCT integrity_state FROM resource_graph_occurrences WHERE resolved_url=?",
    )?;
    let states: Vec<String> = stmt
        .query_map([url], |r| r.get(0))?
        .collect::<Result<_, _>>()?;
    Ok(match states.as_slice() {
        [only] => only.clone(),
        [] => "unknown".to_owned(),
        _ => "mixed".to_owned(),
    })
}

/// Read visually oriented dimensions without decoding or transposing pixels.
pub fn image_dimensions(body: Option<&[u8]>) -> Result<(Option<i64>, Option<i64>), StoreError> {
    Ok(match body.and_then(crate::resource_image::dimensions) {
        Some((w, h)) => (Some(w), Some(h)),
        None => (None, None),
    })
}

/// Declarations found inside a fetched stylesheet, one level deeper.
pub fn store_css_children(
    con: &Connection,
    parent: &IndexMap<String, Value>,
    text: &str,
    _max_nesting: i64,
) -> Result<(), StoreError> {
    let text_of = |k: &str| {
        parent
            .get(k)
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_owned()
    };
    let int_of = |k: &str| parent.get(k).and_then(Value::as_i64).unwrap_or(0);
    let depth = int_of("nesting_depth") + 1;
    let mut values = Vec::new();
    css(&mut values, text, &text_of("resolved_url"), "css");
    let page_url_id = int_of("page_url_id");
    let source_document_id = int_of("source_document_id");
    let representation = text_of("representation");
    let base: i64 = con.query_row(
        "SELECT COALESCE(MAX(ordinal)+1,0) FROM resource_graph_occurrences \
         WHERE page_url_id=? AND source_document_id=? AND representation=?",
        (page_url_id, source_document_id, &representation),
        |r| r.get(0),
    )?;
    for (offset, value) in values.into_iter().enumerate() {
        con.execute(
            "INSERT OR IGNORE INTO resource_graph_occurrences(page_url_id,source_document_id,representation,ordinal,kind,carrier,raw_url,resolved_url,integrity,integrity_state,nesting_depth,state,reason) \
             VALUES(?,?,?,?,?,?,?,?,?,?,?,?,?)",
            rusqlite::params![
                page_url_id,
                source_document_id,
                representation,
                base + offset as i64,
                value.kind,
                value.carrier,
                value.raw_url,
                value.resolved_url,
                value.integrity,
                value.integrity_state,
                depth,
                "disabled",
                "resource fetch is pending",
            ],
        )?;
    }
    Ok(())
}

/// Stylesheet bytes as text; invalid UTF-8 becomes U+FFFD.
pub(crate) fn css_children_text(body: &[u8]) -> String {
    String::from_utf8_lossy(body).into_owned()
}
