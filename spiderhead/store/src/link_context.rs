//! Offline per-occurrence link context from one retained complete document.
//!
//! reference source: the reference suite (slice `storage-pack-05`).

use std::path::Path;

use rusqlite::types::ValueRef;
use rusqlite::{params, Connection, OptionalExtension};
use serde_json::{json, Map, Value};

use crate::crawl_records::spider::{strip_fragment, Scope};
use crate::error::StoreError;
use crate::scan_artifact::{APPLICATION_ID, USER_VERSION};
use crate::schema_sql::SCAN_USER_VERSION;

/// Schema of one occurrence record (mirror of the reference checks constant).
pub const SCHEMA_VERSION: &str = "link_occurrence_context.v1";
/// Parser cap on anchor observations per document (mirror of the reference checks constant).
pub const MAX_ANCHORS: i64 = 20_000;

/// Convert one SQLite cell into a JSON value.
pub(crate) fn cell_value(value: ValueRef<'_>) -> Value {
    match value {
        ValueRef::Null => Value::Null,
        ValueRef::Integer(i) => json!(i),
        ValueRef::Real(f) => json!(f),
        ValueRef::Text(t) => json!(String::from_utf8_lossy(t)),
        ValueRef::Blob(b) => json!(String::from_utf8_lossy(b)),
    }
}

/// Read the named columns of one row into a JSON object.
pub(crate) fn row_map(
    row: &rusqlite::Row<'_>,
    columns: &[&str],
) -> rusqlite::Result<Map<String, Value>> {
    let mut out = Map::new();
    for (index, name) in columns.iter().enumerate() {
        out.insert((*name).to_string(), cell_value(row.get_ref(index)?));
    }
    Ok(out)
}

/// Open a scan for light inspection: identity, schema version and the header row.
///
/// Returns the connection and the validation mode (`"light"`). The full object inventory
/// check belongs to `storage-__init__` (`open_scan_mode`); until that merges, only the
/// identity, version and header are verified here.
pub(crate) fn open_light(path: &Path) -> Result<(Connection, &'static str), StoreError> {
    let con = spiderhead_core::sqlite::open_readonly(path, std::time::Duration::from_secs(5), &[])?;
    let app: i64 = con.query_row("PRAGMA application_id", [], |r| r.get(0))?;
    let version: i64 = con.query_row("PRAGMA user_version", [], |r| r.get(0))?;
    if app != APPLICATION_ID || version != USER_VERSION || version != SCAN_USER_VERSION {
        return Err(StoreError::Invalid(
            "unsupported scan identity or version".into(),
        ));
    }
    let header_rows: i64 = con.query_row("SELECT COUNT(*) FROM scan", [], |r| r.get(0))?;
    if header_rows != 1 {
        return Err(StoreError::Invalid(
            "scan.v1 requires one matching format header".into(),
        ));
    }
    Ok((con, "light"))
}

/// Interrupt a SQLite statement once `after` has elapsed (the statement reports
/// "interrupted"). Dropping the returned guard cancels the timer.
pub(crate) fn arm_deadline(
    con: &Connection,
    after: std::time::Duration,
) -> std::sync::mpsc::Sender<()> {
    let handle = con.get_interrupt_handle();
    let (cancel, waiting) = std::sync::mpsc::channel::<()>();
    std::thread::spawn(move || {
        if let Err(std::sync::mpsc::RecvTimeoutError::Timeout) = waiting.recv_timeout(after) {
            handle.interrupt();
        }
    });
    cancel
}

/// Validate the paging and budget arguments of the context queries.
pub fn validate_limits(
    offset: i64,
    limit: i64,
    max_body_bytes: i64,
    max_result_bytes: i64,
) -> Result<(), StoreError> {
    if offset < 0
        || !(1..=500).contains(&limit)
        || !(1..=8 * 1024 * 1024).contains(&max_body_bytes)
        || !(1024..=8 * 1024 * 1024).contains(&max_result_bytes)
    {
        return Err(StoreError::Invalid(
            "link context requires offset >= 0, limit 1..500, body bytes 1..8388608, \
             result bytes 1024..8388608"
                .into(),
        ));
    }
    Ok(())
}

/// Unavailable occurrence record carrying `reason` and no placement.
pub fn unavailable(link: &Map<String, Value>, reason: &str) -> Map<String, Value> {
    let get = |key: &str| link.get(key).cloned().unwrap_or(Value::Null);
    let mut out = Map::new();
    out.insert("schema_version".into(), json!(SCHEMA_VERSION));
    out.insert("link_id".into(), get("link_id"));
    out.insert("source_url_id".into(), get("source_url_id"));
    out.insert("destination_url_id".into(), get("destination_url_id"));
    out.insert("source_document_id".into(), get("source_document_id"));
    out.insert("representation".into(), get("evidence_representation"));
    out.insert("ordinal".into(), get("ordinal"));
    out.insert("source_url".into(), get("source_url"));
    out.insert("destination_url".into(), get("destination_url"));
    out.insert("anchor".into(), get("anchor"));
    out.insert(
        "nofollow".into(),
        json!(link.get("nofollow").and_then(Value::as_i64).unwrap_or(0) != 0),
    );
    out.insert(
        "raw_href".into(),
        match link.get("raw_href") {
            Some(Value::String(s)) if !s.is_empty() => json!(s),
            _ => Value::Null,
        },
    );
    out.insert(
        "stored_position".into(),
        match link.get("position") {
            Some(Value::String(s)) if !s.is_empty() => json!(s),
            _ => Value::Null,
        },
    );
    out.insert("state".into(), json!("unavailable"));
    out.insert("reason".into(), json!(reason));
    out.insert("placement".into(), Value::Null);
    out.insert("heading".into(), Value::Null);
    out.insert("heading_relation".into(), Value::Null);
    out.insert("disclosure".into(), Value::Null);
    out
}

const LINK_COLUMNS: &[&str] = &[
    "link_id",
    "source_url_id",
    "destination_url_id",
    "source_document_id",
    "evidence_representation",
    "ordinal",
    "anchor",
    "nofollow",
    "position",
    "raw_href",
    "rel_json",
    "target",
];

/// Stored occurrences of one source document, ordered by ordinal and link ID.
pub fn links(con: &Connection, document_id: i64) -> Result<Vec<Map<String, Value>>, StoreError> {
    let sql = "SELECT l.link_id,l.source_url_id,l.destination_url_id,l.source_document_id,\
        l.evidence_representation,l.ordinal,l.anchor,l.nofollow,l.position,l.raw_href,\
        l.rel_json,l.target,s.url AS source_url,d.url AS destination_url \
        FROM links l JOIN urls s ON s.url_id=l.source_url_id \
        JOIN urls d ON d.url_id=l.destination_url_id \
        WHERE l.source_document_id=?1 ORDER BY l.ordinal,l.link_id LIMIT ?2";
    let mut columns: Vec<&str> = LINK_COLUMNS.to_vec();
    columns.extend(["source_url", "destination_url"]);
    let mut statement = con.prepare(sql)?;
    let rows = statement.query_map(params![document_id, MAX_ANCHORS + 1], |row| {
        row_map(row, &columns)
    })?;
    Ok(rows.collect::<Result<_, _>>()?)
}

/// URL that the source document was served from, after redirects.
pub fn final_url(
    con: &Connection,
    document: &Map<String, Value>,
    source_url: &str,
) -> Result<String, StoreError> {
    let url_id: Option<i64> =
        if document.get("representation").and_then(Value::as_str) == Some("rendered") {
            let renderer = document
                .get("renderer_json")
                .and_then(Value::as_str)
                .and_then(|s| serde_json::from_str::<Value>(s).ok());
            match renderer.and_then(|r| r.get("final_url_id").and_then(Value::as_i64)) {
                Some(id) => Some(id),
                None => return Ok(source_url.to_string()),
            }
        } else {
            let response_id = document.get("source_response_id").and_then(Value::as_i64);
            con.query_row(
                "SELECT effective_url_id FROM responses WHERE response_id=?1",
                params![response_id],
                |r| r.get::<_, Option<i64>>(0),
            )
            .optional()?
            .flatten()
        };
    let Some(url_id) = url_id else {
        return Ok(source_url.to_string());
    };
    let url: Option<String> = con
        .query_row(
            "SELECT url FROM urls WHERE url_id=?1",
            params![url_id],
            |r| r.get(0),
        )
        .optional()?;
    Ok(url.unwrap_or_else(|| source_url.to_string()))
}

/// Occurrences the crawler would have stored under the recorded discovery policy.
///
/// reference raised `KeyError`/`TypeError`/`ValueError` on a malformed recorded policy; the
/// caller maps that to `recorded_link_policy_unavailable`.
pub fn stored_candidates(
    extracted: &[Map<String, Value>],
    config: &Value,
    depth: i64,
    start_host: &str,
) -> Result<Vec<Map<String, Value>>, StoreError> {
    let max_depth = config
        .get("limits")
        .and_then(|l| l.get("max_depth"))
        .and_then(Value::as_i64)
        .ok_or_else(|| StoreError::Invalid("recorded_link_policy_unavailable".into()))?;
    if depth >= max_depth {
        return Ok(Vec::new());
    }
    let scope_map: Option<std::collections::BTreeMap<String, Value>> = config
        .get("scope")
        .and_then(Value::as_object)
        .map(|m| m.iter().map(|(k, v)| (k.clone(), v.clone())).collect());
    let scope = Scope::from_config(scope_map.as_ref());
    let discovery = config
        .get("discovery")
        .ok_or_else(|| StoreError::Invalid("recorded_link_policy_unavailable".into()))?;
    let store_of = |kind: &str| {
        discovery
            .get(kind)
            .and_then(|k| k.get("store"))
            .and_then(Value::as_bool)
            .ok_or_else(|| StoreError::Invalid("recorded_link_policy_unavailable".into()))
    };
    let store_external = store_of("external")?;
    let store_hyperlinks = store_of("hyperlinks")?;
    let mut selected = Vec::new();
    for item in extracted {
        let href = item.get("href").and_then(Value::as_str).unwrap_or("");
        let target = strip_fragment(href);
        let external = scope.rejection(&target, start_host) == "outside_host";
        let should_store = if external {
            store_external
        } else {
            store_hyperlinks
        };
        if should_store {
            selected.push(item.clone());
        }
    }
    Ok(selected)
}

/// Result of parsing a retained document's anchors. Filled by the checks-side extractor.
pub struct Extracted {
    /// Anchor observations in document order.
    pub occurrences: Vec<Map<String, Value>>,
    /// Eligible anchors seen before the cap.
    pub eligible_total: i64,
    /// Eligible anchors dropped by the cap.
    pub eligible_omitted: i64,
}

/// Parse one document's anchors into stored-candidate shape. `content_area` and
/// `position_rules` come from the recorded scan config, as in reference `_derive`.
fn extract_occurrences(
    html: &str,
    final_url: &str,
    config: &Value,
    cap: i64,
) -> Result<Extracted, StoreError> {
    let content_area: Option<spiderhead_parse::types::ContentAreaConfig> = config
        .get("evidence")
        .and_then(|e| e.get("content_area"))
        .filter(|v| !v.is_null())
        .map(|v| serde_json::from_value(v.clone()))
        .transpose()
        .map_err(|e| StoreError::Invalid(e.to_string()))?;
    let position_rules = config.get("link_position").and_then(|l| l.get("rules"));
    let extracted = spiderhead_parse::link_context::extract_occurrences(
        html,
        final_url,
        content_area.as_ref(),
        position_rules,
        cap as usize,
    )
    .map_err(|e| StoreError::Invalid(e.to_string()))?;
    Ok(Extracted {
        occurrences: extracted
            .occurrences
            .iter()
            .map(|item| match serde_json::to_value(item) {
                Ok(Value::Object(map)) => map,
                _ => Map::new(),
            })
            .collect(),
        eligible_total: extracted.eligible_total as i64,
        eligible_omitted: extracted.eligible_omitted as i64,
    })
}

/// Occurrence records plus document-level coverage.
pub type Derived = (Vec<Map<String, Value>>, Map<String, Value>);

/// Derive per-occurrence context from one retained complete document.
///
/// Returns one record per link plus document-level coverage. Any document that cannot be
/// replayed yields explicit `unavailable` records; a corrupt binding is an error.
pub fn derive(
    con: &Connection,
    document_id: i64,
    links: &[Map<String, Value>],
    max_body_bytes: i64,
) -> Result<Derived, StoreError> {
    let document: Option<Map<String, Value>> = {
        let mut statement = con.prepare("SELECT * FROM documents WHERE document_id=?1")?;
        let names: Vec<String> = statement
            .column_names()
            .iter()
            .map(|s| s.to_string())
            .collect();
        let names_ref: Vec<&str> = names.iter().map(String::as_str).collect();
        statement
            .query_row(params![document_id], |row| row_map(row, &names_ref))
            .optional()?
    };
    let Some(document) = document else {
        return Err(StoreError::Invalid("source document is absent".into()));
    };
    let coverage_unavailable = |reason: &str| {
        let items = links
            .iter()
            .map(|l| unavailable(l, reason))
            .collect::<Vec<_>>();
        let mut coverage = Map::new();
        coverage.insert("state".into(), json!("unavailable"));
        coverage.insert("reason".into(), json!(reason));
        (items, coverage)
    };
    if links.len() as i64 > MAX_ANCHORS {
        let mut coverage = Map::new();
        coverage.insert("state".into(), json!("unavailable"));
        coverage.insert("reason".into(), json!("link_occurrence_budget_exhausted"));
        return Ok((Vec::new(), coverage));
    }
    let row_url_id = document.get("url_id").and_then(Value::as_i64);
    let row_repr = document
        .get("representation")
        .and_then(Value::as_str)
        .unwrap_or("");
    if links.iter().any(|link| {
        link.get("source_url_id").and_then(Value::as_i64) != row_url_id
            || link.get("evidence_representation").and_then(Value::as_str) != Some(row_repr)
    }) {
        return Err(StoreError::Invalid(
            "link occurrence points to a different source document".into(),
        ));
    }
    let (scan_config, start_url): (String, String) = con.query_row(
        "SELECT config_json,start_url FROM scan WHERE singleton=1",
        [],
        |r| {
            Ok((
                r.get(0)?,
                r.get::<_, Option<String>>(1)?.unwrap_or_default(),
            ))
        },
    )?;
    let config: Value = serde_json::from_str(&scan_config)?;
    let body_state = document
        .get("body_state")
        .and_then(Value::as_str)
        .unwrap_or("");
    let body_reason = document
        .get("body_reason")
        .and_then(Value::as_str)
        .unwrap_or("");
    let mut reason = String::new();
    if body_state != "complete" {
        reason = format!("body_{body_state}:{body_reason}");
    } else if row_repr != "rendered" {
        let response_id = document.get("source_response_id").and_then(Value::as_i64);
        let content_type: Option<Option<String>> = con
            .query_row(
                "SELECT content_type FROM responses WHERE response_id=?1",
                params![response_id],
                |r| r.get(0),
            )
            .optional()?;
        let media_type = content_type
            .flatten()
            .unwrap_or_default()
            .split(';')
            .next()
            .unwrap_or("")
            .trim()
            .to_lowercase();
        if media_type != "text/html" {
            let shown = if media_type.is_empty() {
                "missing".to_string()
            } else {
                media_type
            };
            reason = format!("unsupported_source_mime:{shown}");
        }
    }
    if !reason.is_empty() {
        let (items, coverage) = coverage_unavailable(&reason);
        return Ok((items, coverage));
    }
    let html = crate::bodies::read_document(con, document_id, max_body_bytes)?;
    let source_url = match links.first() {
        Some(first) => first
            .get("source_url")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string(),
        None => con.query_row(
            "SELECT url FROM urls WHERE url_id=?1",
            params![row_url_id],
            |r| r.get::<_, String>(0),
        )?,
    };
    let final_source = final_url(con, &document, &source_url)?;
    let extracted = extract_occurrences(&html, &final_source, &config, MAX_ANCHORS)?;
    let depth: i64 = con.query_row(
        "SELECT crawl_depth FROM pages WHERE url_id=?1",
        params![row_url_id],
        |r| r.get(0),
    )?;
    let start_host = url_host(&start_url);
    let selected = match stored_candidates(&extracted.occurrences, &config, depth, &start_host) {
        Ok(selected) => selected,
        Err(_) => {
            let (items, coverage) = coverage_unavailable("recorded_link_policy_unavailable");
            return Ok((items, coverage));
        }
    };
    let attributes_captured = config
        .get("link_attributes")
        .and_then(|a| a.get("capture"))
        .and_then(Value::as_bool)
        == Some(true);
    let position_captured = config
        .get("link_position")
        .and_then(|a| a.get("classify"))
        .and_then(Value::as_bool)
        == Some(true);
    let mismatch = selected.len() != links.len()
        || selected.iter().zip(links.iter()).any(|(item, link)| {
            let link_position = link.get("position").and_then(Value::as_str).unwrap_or("");
            let item_position = item.get("position");
            item.get("href") != link.get("destination_url")
                || item.get("anchor") != link.get("anchor")
                || item
                    .get("nofollow")
                    .and_then(Value::as_bool)
                    .unwrap_or(false)
                    != (link.get("nofollow").and_then(Value::as_i64).unwrap_or(0) != 0)
                || ((position_captured || !link_position.is_empty())
                    && item_position != link.get("position"))
                || (attributes_captured
                    && (item.get("raw_href") != link.get("raw_href")
                        || item.get("target") != link.get("target")
                        || item.get("rel")
                            != link
                                .get("rel_json")
                                .and_then(|v| v.as_str())
                                .and_then(|s| serde_json::from_str::<Value>(s).ok())
                                .as_ref()))
        });
    if mismatch {
        let (items, coverage) =
            coverage_unavailable("stored_links_do_not_replay_from_retained_document");
        return Ok((items, coverage));
    }
    let mut items = Vec::new();
    for (link, item) in links.iter().zip(selected.iter()) {
        let mut context = unavailable(link, "");
        let stored = match link.get("position") {
            Some(Value::String(s)) if !s.is_empty() => json!(s),
            _ => Value::Null,
        };
        let placement = json!({
            "position": item.get("position").cloned().unwrap_or(Value::Null),
            "basis": item.get("placement_basis").cloned().unwrap_or(Value::Null),
            "matched_selector": item.get("matched_selector").cloned().unwrap_or(Value::Null),
            "matched_selector_truncated": item.get("matched_selector_truncated").cloned().unwrap_or(Value::Null),
            "content_root_strategy": item.get("content_root_strategy").cloned().unwrap_or(Value::Null),
            "dom_path": item.get("dom_path").cloned().unwrap_or(Value::Null),
            "dom_path_truncated": item.get("dom_path_truncated").cloned().unwrap_or(Value::Null),
            "stored_position": stored,
        });
        let heading_relation = item.get("heading_relation").cloned().unwrap_or(Value::Null);
        let heading = match item.get("heading") {
            Some(h) if h.is_object() => {
                let mut merged = h.as_object().cloned().unwrap_or_default();
                merged.insert("relation".into(), heading_relation.clone());
                Value::Object(merged)
            }
            _ => Value::Null,
        };
        context.insert("state".into(), json!("measured"));
        context.insert("reason".into(), json!(""));
        context.insert("placement".into(), placement);
        context.insert("heading".into(), heading);
        context.insert("heading_relation".into(), heading_relation);
        context.insert(
            "disclosure".into(),
            item.get("disclosure").cloned().unwrap_or(Value::Null),
        );
        items.push(context);
    }
    let omitted = extracted.eligible_omitted;
    let mut coverage = Map::new();
    coverage.insert(
        "state".into(),
        json!(if omitted > 0 { "partial" } else { "complete" }),
    );
    coverage.insert(
        "reason".into(),
        json!(if omitted > 0 {
            "eligible anchor observations exceeded parser cap"
        } else {
            ""
        }),
    );
    coverage.insert("eligible_total".into(), json!(extracted.eligible_total));
    coverage.insert("eligible_omitted".into(), json!(omitted));
    Ok((items, coverage))
}

/// Host part of an absolute URL, lowercased, without userinfo or port; empty when absent.
fn url_host(url: &str) -> String {
    let Some((_, rest)) = url.split_once("://") else {
        return String::new();
    };
    let authority = rest.split(['/', '?', '#']).next().unwrap_or("");
    let host_port = authority.rsplit('@').next().unwrap_or("");
    let host = if host_port.starts_with('[') {
        host_port
            .split(']')
            .next()
            .map(|h| format!("{h}]"))
            .unwrap_or_default()
    } else {
        host_port.split(':').next().unwrap_or("").to_string()
    };
    host.to_lowercase()
}

/// One page of a document's occurrence contexts with the coverage of the whole document.
pub fn document_page(
    con: &Connection,
    document_id: i64,
    offset: i64,
    limit: i64,
    max_body_bytes: i64,
    max_result_bytes: i64,
) -> Result<Map<String, Value>, StoreError> {
    let source: Option<(i64, String, String)> = con
        .query_row(
            "SELECT d.url_id,d.representation,u.url FROM documents d \
             JOIN urls u ON u.url_id=d.url_id WHERE d.document_id=?1",
            params![document_id],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        )
        .optional()?;
    let Some((source_url_id, representation, source_url)) = source else {
        return Err(StoreError::Invalid("source document is absent".into()));
    };
    let document_links = links(con, document_id)?;
    if document_links.len() as i64 > MAX_ANCHORS {
        return Err(StoreError::Invalid(
            "source document exceeds the supported link occurrence cap".into(),
        ));
    }
    let (items, coverage) = derive(con, document_id, &document_links, max_body_bytes)?;
    let (scan_uuid, evidence_revision): (String, i64) = con.query_row(
        "SELECT scan_uuid,evidence_revision FROM scan WHERE singleton=1",
        [],
        |r| Ok((r.get(0)?, r.get(1)?)),
    )?;
    let mut page = Vec::new();
    let mut used: i64 = 0;
    let start = offset.max(0) as usize;
    let end = (offset + limit).max(0) as usize;
    for item in items.iter().skip(start).take(end.saturating_sub(start)) {
        let mut projected = item.clone();
        projected.insert("scan_uuid".into(), json!(scan_uuid));
        projected.insert("evidence_revision".into(), json!(evidence_revision));
        projected.insert("document_coverage".into(), Value::Object(coverage.clone()));
        let size =
            spiderhead_core::common::python_json(&Value::Object(projected.clone())).len() as i64;
        if used + size > max_result_bytes {
            if page.is_empty() {
                return Err(StoreError::Invalid(
                    "one link context record exceeds max_result_bytes".into(),
                ));
            }
            break;
        }
        page.push(Value::Object(projected));
        used += size;
    }
    let next = offset + page.len() as i64;
    let has_more = (next as usize) < items.len();
    let mut out = Map::new();
    out.insert("schema_version".into(), json!(SCHEMA_VERSION));
    out.insert("scan_uuid".into(), json!(scan_uuid));
    out.insert("evidence_revision".into(), json!(evidence_revision));
    out.insert("source_document_id".into(), json!(document_id));
    out.insert("source_url_id".into(), json!(source_url_id));
    out.insert("source_url".into(), json!(source_url));
    out.insert("representation".into(), json!(representation));
    out.insert("coverage".into(), Value::Object(coverage));
    out.insert("total".into(), json!(items.len()));
    out.insert("offset".into(), json!(offset));
    out.insert("limit".into(), json!(limit));
    out.insert("max_result_bytes".into(), json!(max_result_bytes));
    out.insert("result_bytes".into(), json!(used));
    out.insert("items".into(), Value::Array(page));
    out.insert("has_more".into(), json!(has_more));
    out.insert(
        "next_offset".into(),
        if has_more { json!(next) } else { Value::Null },
    );
    Ok(out)
}

/// Inspect one document's stored link occurrences; no crawl or scan mutation.
/// reference defaults: offset=0, limit=100, max_body_bytes=5 * 1024 * 1024, max_result_bytes=1048576
pub fn contexts_for_document(
    scan_path: &Path,
    document_id: i64,
    offset: i64,
    limit: i64,
    max_body_bytes: i64,
    max_result_bytes: i64,
) -> Result<Map<String, Value>, StoreError> {
    validate_limits(offset, limit, max_body_bytes, max_result_bytes)?;
    if document_id < 1 {
        return Err(StoreError::Invalid(
            "source document ID must be a positive integer".into(),
        ));
    }
    let (con, validation) = open_light(scan_path)?;
    let mut page = document_page(
        &con,
        document_id,
        offset,
        limit,
        max_body_bytes,
        max_result_bytes,
    )?;
    page.insert("validation".into(), json!(validation));
    Ok(page)
}

/// Return one occurrence's context, including an explicit unavailable state.
/// reference defaults: max_body_bytes=5 * 1024 * 1024, max_result_bytes=1048576
pub fn context_for_link(
    scan_path: &Path,
    link_id: i64,
    max_body_bytes: i64,
    max_result_bytes: i64,
) -> Result<Map<String, Value>, StoreError> {
    if link_id < 1 {
        return Err(StoreError::Invalid(
            "link ID must be a positive integer".into(),
        ));
    }
    validate_limits(0, 1, max_body_bytes, max_result_bytes)?;
    let (con, validation) = open_light(scan_path)?;
    let row: Option<Map<String, Value>> = {
        let sql = "SELECT l.link_id,l.source_url_id,l.destination_url_id,l.source_document_id,\
            l.evidence_representation,l.ordinal,l.anchor,l.nofollow,l.position,l.raw_href,\
            s.url AS source_url,d.url AS destination_url \
            FROM links l JOIN urls s ON s.url_id=l.source_url_id \
            JOIN urls d ON d.url_id=l.destination_url_id WHERE l.link_id=?1";
        let columns = [
            "link_id",
            "source_url_id",
            "destination_url_id",
            "source_document_id",
            "evidence_representation",
            "ordinal",
            "anchor",
            "nofollow",
            "position",
            "raw_href",
            "source_url",
            "destination_url",
        ];
        con.query_row(sql, params![link_id], |r| row_map(r, &columns))
            .optional()?
    };
    let Some(link) = row else {
        return Err(StoreError::Invalid(
            "link ID is not present in this scan".into(),
        ));
    };
    let (scan_uuid, evidence_revision): (String, i64) = con.query_row(
        "SELECT scan_uuid,evidence_revision FROM scan WHERE singleton=1",
        [],
        |r| Ok((r.get(0)?, r.get(1)?)),
    )?;
    let Some(document_id) = link.get("source_document_id").and_then(Value::as_i64) else {
        let mut item = unavailable(&link, "source_document_unavailable");
        item.insert("scan_uuid".into(), json!(scan_uuid));
        item.insert("evidence_revision".into(), json!(evidence_revision));
        item.insert(
            "document_coverage".into(),
            json!({"state": "unavailable", "reason": "source_document_unavailable"}),
        );
        if spiderhead_core::common::python_json(&Value::Object(item.clone())).len() as i64
            > max_result_bytes
        {
            return Err(StoreError::Invalid(
                "one link context record exceeds max_result_bytes".into(),
            ));
        }
        item.insert("validation".into(), json!(validation));
        return Ok(item);
    };
    let ordinal = link.get("ordinal").and_then(Value::as_i64).unwrap_or(0);
    let page = document_page(
        &con,
        document_id,
        ordinal,
        1,
        max_body_bytes,
        max_result_bytes,
    )?;
    let first = page
        .get("items")
        .and_then(Value::as_array)
        .and_then(|items| items.first())
        .cloned();
    let Some(Value::Object(mut item)) = first else {
        return Err(StoreError::Invalid(
            "link ordinal does not identify its source document occurrence".into(),
        ));
    };
    if item.get("link_id").and_then(Value::as_i64) != Some(link_id) {
        return Err(StoreError::Invalid(
            "link ordinal does not identify its source document occurrence".into(),
        ));
    }
    item.insert("validation".into(), json!(validation));
    Ok(item)
}
