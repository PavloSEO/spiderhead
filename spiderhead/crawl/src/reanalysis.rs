//! Offline replay of one captured scan: rebuild every page record from the
//! retained transport evidence and re-parse retained documents. No fetching,
//! no frontier work, no run-wide graph.

use std::collections::BTreeMap;

use indexmap::IndexMap;
use rusqlite::{Connection, OptionalExtension};
use serde_json::{json, Map, Value};

use spiderhead_parse::ParseOptions;
use spiderhead_store::bodies::read_document;
use spiderhead_store::error::StoreError;
use spiderhead_store::reanalysis::PageReplay;
use spiderhead_store::scan_artifact::ScanError;

use crate::collect;
use crate::scope::Scope;

/// A scan rejected as unusable for offline reanalysis.
fn unavailable(message: impl Into<String>) -> StoreError {
    StoreError::ScanError(ScanError {
        message: message.into(),
    })
}

/// The replayed run: every page's rebuilt observations plus the start-page
/// gate the audit assembly consumes.
pub struct Replay {
    pub pages: Vec<PageReplay>,
    /// `html`, `outlinks` and `external_outlinks` of the start page, when its
    /// static document was replayed.
    pub start_page_gate: Option<Map<String, Value>>,
    /// The start page's raw document text.
    pub raw_html: Option<String>,
    pub start_url: String,
    pub list_mode: bool,
    pub page_count: i64,
}

fn body_limit(config: &Map<String, Value>) -> Result<i64, StoreError> {
    let storage = config
        .get("storage")
        .and_then(Value::as_object)
        .ok_or_else(|| {
            unavailable("reanalysis unavailable: recorded scan has no body-retention configuration")
        })?;
    match storage.get("max_body_bytes").and_then(Value::as_i64) {
        Some(limit) if limit > 0 => Ok(limit),
        _ => Err(unavailable(
            "reanalysis unavailable: recorded body byte limit is invalid",
        )),
    }
}

fn parse_limit(config: &Map<String, Value>) -> Result<i64, StoreError> {
    let value = config
        .get("limits")
        .and_then(|limits| limits.get("max_response_bytes"))
        .and_then(Value::as_i64);
    match value {
        Some(limit) if limit > 0 => Ok(limit),
        _ => Err(unavailable(
            "reanalysis unavailable: recorded parser byte limit is invalid",
        )),
    }
}

/// The document id of the page's active static representation, resolved from
/// the per-document resource-inventory rows.
fn active_static_document(
    source: &Connection,
    page_url_id: i64,
    representation: &str,
    document_id: Option<i64>,
) -> Result<Option<i64>, StoreError> {
    if representation == "static" {
        return Ok(document_id);
    }
    let mut statement = source.prepare(
        "SELECT d.document_id FROM documents d JOIN context_items c \
         ON c.item_key='document:'||d.document_id \
         WHERE c.kind='resource_inventory' AND d.url_id=?1 AND d.representation='static' \
         AND NOT EXISTS(SELECT 1 FROM context_items later JOIN documents newer \
         ON later.item_key='document:'||newer.document_id \
         WHERE later.kind='resource_inventory' AND newer.url_id=d.url_id \
         AND newer.representation=d.representation AND newer.document_id>d.document_id) \
         ORDER BY d.document_id LIMIT 2",
    )?;
    let rows: Vec<i64> = statement
        .query_map([page_url_id], |row| row.get(0))
        .map_err(StoreError::from)?
        .collect::<Result<_, _>>()
        .map_err(StoreError::from)?;
    match rows.as_slice() {
        [] => Ok(None),
        [one] => Ok(Some(*one)),
        _ => Err(unavailable(
            "reanalysis unavailable: static document for this page: ambiguous resource inventory",
        )),
    }
}

fn document(
    source: &Connection,
    document_id: Option<i64>,
) -> Result<Option<Map<String, Value>>, StoreError> {
    let Some(document_id) = document_id else {
        return Ok(None);
    };
    let mut statement = source.prepare("SELECT * FROM documents WHERE document_id=?")?;
    let names: Vec<String> = statement
        .column_names()
        .iter()
        .map(|name| name.to_string())
        .collect();
    let mut rows = statement.query([document_id]).map_err(StoreError::from)?;
    if let Some(row) = rows.next().map_err(StoreError::from)? {
        let mut map = Map::new();
        for name in names {
            let value: Value = match row.get_ref(name.as_str()).map_err(StoreError::from)? {
                rusqlite::types::ValueRef::Null => Value::Null,
                rusqlite::types::ValueRef::Integer(n) => json!(n),
                rusqlite::types::ValueRef::Real(f) => json!(f),
                rusqlite::types::ValueRef::Text(text) => {
                    json!(String::from_utf8_lossy(text).into_owned())
                }
                rusqlite::types::ValueRef::Blob(_) => Value::Null,
            };
            map.insert(name, value);
        }
        return Ok(Some(map));
    }
    Ok(None)
}

/// The final URL of a document's evidence: the recorded effective URL for a
/// static document, the renderer's final URL otherwise.
fn document_final_url(
    source: &Connection,
    document: &Map<String, Value>,
    fallback: &str,
) -> Result<String, StoreError> {
    let representation = document
        .get("representation")
        .and_then(Value::as_str)
        .unwrap_or_default();
    if representation == "static" {
        let response_id = document
            .get("source_response_id")
            .and_then(Value::as_i64)
            .ok_or_else(|| {
                unavailable(format!(
                    "reanalysis unavailable: static document for {fallback}: source response is absent"
                ))
            })?;
        let url: String = source
            .query_row(
                "SELECT u.url FROM responses r JOIN urls u ON u.url_id=r.effective_url_id \
                 WHERE r.response_id=?1",
                [response_id],
                |row| row.get(0),
            )
            .optional()
            .map_err(StoreError::from)?
            .ok_or_else(|| {
                unavailable(format!(
                    "reanalysis unavailable: static document for {fallback}: response effective URL is absent"
                ))
            })?;
        return Ok(url);
    }
    Err(unavailable(format!(
        "reanalysis unavailable: {representation} replay of {fallback} is not supported in this build"
    )))
}

/// Evidence facts for one representation's document.
fn facts(
    document_id: Option<i64>,
    document: Option<&Map<String, Value>>,
    parsed: Option<&Map<String, Value>>,
) -> Map<String, Value> {
    let Some(document) = document else {
        return Map::from_iter([
            ("document_id".into(), json!(document_id)),
            ("state".into(), json!("unavailable")),
            ("reason".into(), json!("not_in_corpus")),
            ("partial_reasons".into(), json!([])),
            ("resource_inventory_state".into(), json!("unavailable")),
            (
                "resource_inventory_reason".into(),
                json!("document is not in the retained corpus"),
            ),
            ("resource_omitted".into(), json!(0)),
            ("head_links".into(), Value::Null),
        ]);
    };
    // Declarations were measured when the parsed view carries the key at all:
    // an empty list is a clean measurement, a missing key means the opt-in
    // parse stage never ran.
    let measured = parsed
        .map(|p| p.contains_key("resource_declarations"))
        .unwrap_or(false);
    let omitted = parsed
        .and_then(|p| p.get("resource_declarations_omitted"))
        .and_then(Value::as_i64)
        .unwrap_or(0);
    let resource_state = if omitted != 0 {
        "partial"
    } else if measured {
        "complete"
    } else {
        "unavailable"
    };
    Map::from_iter([
        ("document_id".into(), json!(document_id)),
        (
            "state".into(),
            document.get("body_state").cloned().unwrap_or(Value::Null),
        ),
        (
            "reason".into(),
            document.get("body_reason").cloned().unwrap_or(Value::Null),
        ),
        ("partial_reasons".into(), json!([])),
        ("resource_inventory_state".into(), json!(resource_state)),
        (
            "resource_inventory_reason".into(),
            json!(if measured {
                ""
            } else {
                "resource declarations were not parsed"
            }),
        ),
        ("resource_omitted".into(), json!(omitted)),
        (
            "head_links".into(),
            parsed
                .and_then(|p| p.get("head_links"))
                .cloned()
                .unwrap_or(Value::Null),
        ),
    ])
}

/// Parse one retained document; the document must be complete in the corpus.
/// One retained document's replay: the decoded text and its parsed view.
struct ParsedDocument {
    html: Option<String>,
    parsed: Option<Map<String, Value>>,
}

fn parse_document(
    source: &Connection,
    document: &Map<String, Value>,
    record: &mut Map<String, Value>,
    url: &str,
    parse_options: &ParseOptions,
    body_limit: i64,
    parse_limit: i64,
) -> Result<ParsedDocument, StoreError> {
    let body_state = document
        .get("body_state")
        .and_then(Value::as_str)
        .unwrap_or_default();
    let body_sha = document.get("body_sha256").and_then(Value::as_str);
    if body_state != "complete" || body_sha.is_none() {
        let reason = document
            .get("body_reason")
            .map(|reason| reason.to_string())
            .unwrap_or_else(|| json!("").to_string());
        let representation = document
            .get("representation")
            .and_then(Value::as_str)
            .unwrap_or("static");
        return Err(unavailable(format!(
            "reanalysis unavailable: {representation} document for {url}: {reason}"
        )));
    }
    let document_id = document
        .get("document_id")
        .and_then(Value::as_i64)
        .unwrap_or_default();
    let html = read_document(source, document_id, body_limit).map_err(|error| {
        unavailable(format!(
            "reanalysis unavailable: retained document for {url}: {error}"
        ))
    })?;
    let final_url = document_final_url(source, document, url)?;
    let parsed = collect::apply_body(record, &html, &final_url, parse_options, parse_limit);
    let is_html = record
        .get("content_type")
        .and_then(Value::as_str)
        .map(|t| t.to_ascii_lowercase().contains("html"))
        .unwrap_or(false);
    if is_html && !html.is_empty() && parsed.is_none() {
        let oversized = record.get("body_unavailable").and_then(Value::as_str) == Some("oversized");
        if !oversized {
            return Err(unavailable(format!(
                "reanalysis unavailable: static document for {url}: retained HTML could not be parsed"
            )));
        }
    }
    let parsed_map: Option<Map<String, Value>> = parsed.map(|parsed| {
        let mut map = serde_json::to_value(&parsed)
            .ok()
            .and_then(|value| value.as_object().cloned())
            .unwrap_or_default();
        // The parse ran: an absent opt-in list is an empty measurement, and the
        // evidence facts read the key's presence as "measured".
        map.entry("resource_declarations".to_string())
            .or_insert_with(|| json!([]));
        map
    });
    Ok(ParsedDocument {
        html: Some(html),
        parsed: parsed_map,
    })
}

/// Rebuild every page of the capture source from its retained evidence.
pub fn iterate_reparsed_pages(
    source: &Connection,
    config: &Map<String, Value>,
) -> Result<Replay, StoreError> {
    let body_limit = body_limit(config)?;
    let parse_limit = parse_limit(config)?;
    let parse_options = ParseOptions::default();
    let scope = Scope::from_config(
        config
            .get("scope")
            .cloned()
            .unwrap_or(Value::Null)
            .as_object()
            .map(|m| Value::Object(m.clone()).to_string().into_bytes())
            .as_deref()
            .map(|bytes| serde_json::from_slice(bytes).unwrap_or(Value::Null))
            .as_ref()
            .unwrap_or(&Value::Null),
    )
    .map_err(|message| unavailable(format!("reanalysis unavailable: {message}")))?;
    let start_url: String = source
        .query_row("SELECT start_url FROM scan WHERE singleton=1", [], |row| {
            row.get(0)
        })
        .optional()
        .map_err(StoreError::from)?
        .filter(|url: &String| !url.is_empty())
        .ok_or_else(|| unavailable("reanalysis unavailable: source scan has no start URL"))?;
    let list_mode: bool = source
        .query_row(
            "SELECT 1 FROM context_items WHERE kind='list_mode' AND item_key='run'",
            [],
            |row| row.get::<_, i64>(0),
        )
        .optional()
        .map_err(StoreError::from)?
        .is_some();
    let host = hostname_of(&start_url);
    if host.is_empty() && !list_mode {
        return Err(unavailable(
            "reanalysis unavailable: source start URL has no host",
        ));
    }

    let mut statement = source.prepare(
        "SELECT p.*,u.url FROM pages p JOIN urls u USING(url_id) ORDER BY p.page_ordinal",
    )?;
    let page_count: i64 = statement
        .query_map([], row_to_map)?
        .collect::<Result<Vec<_>, _>>()
        .map_err(StoreError::from)?
        .len() as i64;
    drop(statement);
    let rows: Vec<Map<String, Value>> = {
        let mut statement = source.prepare(
            "SELECT p.*,u.url FROM pages p JOIN urls u USING(url_id) ORDER BY p.page_ordinal",
        )?;
        let loaded = statement
            .query_map([], row_to_map)?
            .collect::<Result<Vec<_>, _>>()
            .map_err(StoreError::from)?;
        loaded
    };

    let mut pages = Vec::new();
    let mut start_page_gate: Option<Map<String, Value>> = None;
    let mut raw_html: Option<String> = None;
    for row in &rows {
        let url = row
            .get("url")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_owned();
        let page_host = if list_mode {
            hostname_of(&url)
        } else {
            host.clone()
        };
        let mut record = collect::base_record(&url);
        for key in [
            "status_code",
            "content_type",
            "size_bytes",
            "response_time",
            "redirect_url",
            "x_robots",
            "http_refresh",
            "content_encoding",
            "error",
            "error_kind",
            "cache_status",
            "final_url",
            "final_canonical",
        ] {
            if let Some(value) = row.get(key) {
                record.insert(key.to_string(), value.clone());
            }
        }
        record.insert(
            "crawl_depth".into(),
            row.get("crawl_depth").cloned().unwrap_or(json!(0)),
        );
        record.insert(
            "transport_upgraded".into(),
            json!(
                row.get("transport_upgraded")
                    .and_then(Value::as_i64)
                    .unwrap_or(0)
                    != 0
            ),
        );
        record.insert(
            "redirect_chain".into(),
            serde_json::from_str(
                row.get("redirect_chain_json")
                    .and_then(Value::as_str)
                    .unwrap_or("[]"),
            )
            .unwrap_or(json!([])),
        );
        let canonical_chain = row
            .get("canonical_chain_json")
            .and_then(Value::as_str)
            .filter(|text| !text.is_empty());
        record.insert(
            "canonical_chain".into(),
            canonical_chain
                .and_then(|text| serde_json::from_str(text).ok())
                .unwrap_or(json!([])),
        );
        record.insert("representation".into(), json!("static"));

        let page_url_id = row
            .get("url_id")
            .and_then(Value::as_i64)
            .unwrap_or_default();
        let representation = row
            .get("representation")
            .and_then(Value::as_str)
            .unwrap_or("static")
            .to_owned();
        let static_id = active_static_document(
            source,
            page_url_id,
            &representation,
            row.get("document_id").and_then(Value::as_i64),
        )?;
        let selected_id = row.get("document_id").and_then(Value::as_i64);
        let static_doc = document(source, static_id)?;
        let is_html = record
            .get("content_type")
            .and_then(Value::as_str)
            .map(|t| t.to_ascii_lowercase().contains("html"))
            .unwrap_or(false);
        let has_status = record.get("status_code").and_then(Value::as_i64).is_some();

        let mut links: BTreeMap<String, Vec<Map<String, Value>>> = BTreeMap::new();
        let forms: BTreeMap<String, Vec<Map<String, Value>>> = BTreeMap::new();
        let mut resources: BTreeMap<String, Vec<IndexMap<String, Value>>> = BTreeMap::new();
        let mut facts_by: BTreeMap<String, Map<String, Value>> = BTreeMap::new();
        if is_html && has_status {
            let Some(static_doc) = static_doc.as_ref() else {
                return Err(unavailable(format!(
                    "reanalysis unavailable: static document for {url}: no active resource-inventory document"
                )));
            };
            let replayed = parse_document(
                source,
                static_doc,
                &mut record,
                &url,
                &parse_options,
                body_limit,
                parse_limit,
            )?;
            let mut edges = Vec::new();
            if let Some(map) = replayed.parsed.as_ref() {
                edges = link_edges_from(&url, map);
            }
            links.insert("static".to_string(), edges);
            if let Some(document_id) = static_id {
                resources.insert(
                    "static".to_string(),
                    declarations_of(replayed.parsed.as_ref()),
                );
                facts_by.insert(
                    "static".to_string(),
                    facts(
                        Some(document_id),
                        Some(static_doc),
                        replayed.parsed.as_ref(),
                    ),
                );
            }
            if url == start_url {
                raw_html = replayed.html.clone();
                start_page_gate = Some(Map::from_iter([
                    ("html".into(), json!(replayed.html.unwrap_or_default())),
                    (
                        "outlinks".into(),
                        row.get("outlinks").cloned().unwrap_or(json!(0)),
                    ),
                    (
                        "external_outlinks".into(),
                        row.get("external_outlinks").cloned().unwrap_or(json!(0)),
                    ),
                ]));
            }
        } else {
            links.insert("static".to_string(), Vec::new());
            resources.insert("static".to_string(), Vec::new());
            facts_by.insert(
                "static".to_string(),
                facts(static_id, static_doc.as_ref(), None),
            );
        }
        if representation != "static" {
            return Err(unavailable(format!(
                "reanalysis unavailable: {representation} replay of {url} is not supported in this build"
            )));
        }
        let _ = (&page_host, &scope);
        pages.push(PageReplay {
            page: record,
            selected_document_id: selected_id,
            links,
            forms,
            resources,
            representation_facts: facts_by,
        });
    }
    Ok(Replay {
        pages,
        start_page_gate,
        raw_html,
        start_url,
        list_mode,
        page_count,
    })
}

fn row_to_map(row: &rusqlite::Row<'_>) -> rusqlite::Result<Map<String, Value>> {
    let mut map = Map::new();
    for (index, name) in row.as_ref().column_names().iter().enumerate() {
        let value = match row.get_ref(index)? {
            rusqlite::types::ValueRef::Null => Value::Null,
            rusqlite::types::ValueRef::Integer(n) => json!(n),
            rusqlite::types::ValueRef::Real(f) => json!(f),
            rusqlite::types::ValueRef::Text(text) => {
                json!(String::from_utf8_lossy(text).into_owned())
            }
            rusqlite::types::ValueRef::Blob(_) => Value::Null,
        };
        map.insert((*name).to_string(), value);
    }
    Ok(map)
}

/// The stored `<a href>` edges of one parsed document, in document order.
fn link_edges_from(source_url: &str, parsed: &Map<String, Value>) -> Vec<Map<String, Value>> {
    let mut links = Vec::new();
    let Some(items) = parsed.get("links").and_then(Value::as_array) else {
        return links;
    };
    for link in items {
        let href = link.get("href").and_then(Value::as_str).unwrap_or_default();
        let mut edge = Map::new();
        edge.insert("source".into(), json!(source_url));
        edge.insert("destination".into(), json!(href));
        edge.insert(
            "anchor".into(),
            link.get("text").cloned().unwrap_or(json!("")),
        );
        edge.insert(
            "nofollow".into(),
            json!(link
                .get("nofollow")
                .and_then(Value::as_bool)
                .unwrap_or(false)),
        );
        edge.insert("position".into(), json!(""));
        edge.insert(
            "rel".into(),
            json!(link
                .get("rel")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .split_whitespace()
                .map(str::to_string)
                .collect::<Vec<String>>()),
        );
        edge.insert(
            "target".into(),
            link.get("target").cloned().unwrap_or(json!("")),
        );
        edge.insert("raw_href".into(), json!(""));
        edge.insert(
            "anchor_source".into(),
            link.get("anchor_source").cloned().unwrap_or(json!("")),
        );
        links.push(edge);
    }
    links
}

/// The parsed resource declarations as storage-ready rows.
fn declarations_of(parsed: Option<&Map<String, Value>>) -> Vec<IndexMap<String, Value>> {
    parsed
        .and_then(|p| p.get("resource_declarations"))
        .and_then(Value::as_array)
        .map(|items| {
            items
                .iter()
                .filter_map(|item| match item {
                    Value::Object(map) => Some(
                        map.iter()
                            .map(|(k, v)| (k.clone(), v.clone()))
                            .collect::<IndexMap<String, Value>>(),
                    ),
                    _ => None,
                })
                .collect::<Vec<_>>()
        })
        .unwrap_or_default()
}

fn hostname_of(url: &str) -> String {
    let rest = url.split_once("://").map(|(_, rest)| rest).unwrap_or(url);
    let authority = rest.split(['/', '?', '#']).next().unwrap_or(rest);
    let host = authority
        .rsplit_once('@')
        .map(|(_, host)| host)
        .unwrap_or(authority);
    host.split(':').next().unwrap_or(host).to_ascii_lowercase()
}
