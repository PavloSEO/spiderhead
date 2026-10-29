//! Typed, store-only route observations from native static/rendered documents.

use crate::discovery_ledger::canonical_json;
use crate::error::StoreError;
use crate::native_context::{bad, exact_keys, is_int, is_nonneg_int, ContextItem};
use indexmap::IndexMap;
use rusqlite::{params, Connection, OptionalExtension};
use serde_json::{json, Map, Value};
use std::collections::{BTreeMap, BTreeSet};

/// Context kind of one route occurrence.
pub const KIND: &str = "rendered_route_ledger";
/// Context kind of the per-document coverage record.
pub const COVERAGE_KIND: &str = "rendered_route_coverage";
/// Context kind of the run-level coverage record.
pub const RUN_KIND: &str = "rendered_route_run_coverage";
/// `payload_version` of every context row.
pub const OUTER_VERSION: &str = "scan_context.v1";
/// `schema_version` of a route occurrence payload.
pub const ROUTE_VERSION: &str = "rendered_route_ledger.v1";
/// `schema_version` of a coverage payload.
pub const COVERAGE_VERSION: &str = "rendered_route_coverage.v1";
/// Sorted for a stable output order.
pub const REPRESENTATIONS: &[&str] = &["legacy_fragment", "rendered", "static"];

const UNAVAILABLE_STATES: [&str; 3] = ["complete", "partial", "unavailable"];

fn is_str(v: &Value) -> bool {
    v.is_string()
}

/// The parser's omitted-link count; a missing value counts as zero.
fn omitted_count(parsed: &Map<String, Value>) -> i64 {
    parsed
        .get("link_observation")
        .and_then(|o| o.get("omitted"))
        .map_or(0, |v| {
            v.as_i64()
                .or_else(|| v.as_f64().map(|f| f as i64))
                .unwrap_or(0)
        })
}

/// The text before the first `#`, with the scheme lowercased; empty `?` and `;` delimiters are kept.
fn defrag(url: &str) -> String {
    if !url.contains('#') {
        return url.to_string();
    }
    let cleaned: String = url
        .trim_start_matches(|c: char| c <= ' ')
        .chars()
        .filter(|c| !matches!(c, '\t' | '\r' | '\n'))
        .collect();
    let before = cleaned.split('#').next().unwrap_or_default();
    match before.split_once(':') {
        Some((scheme, rest))
            if !scheme.is_empty()
                && scheme
                    .chars()
                    .all(|c| c.is_ascii_alphanumeric() || "+-.".contains(c)) =>
        {
            format!("{}:{rest}", scheme.to_ascii_lowercase())
        }
        _ => before.to_string(),
    }
}

/// Route values plus their coverage record, as returned by [`observations`].
pub type Observed = (Vec<Map<String, Value>>, Map<String, Value>);

/// Return only parser-emitted eligible anchors; never admit or fetch a route.
///
/// `batch` carries `decisions` (objects with `url` and `reason`) and `partial_reasons` (strings),
/// the two fields this function reads.
pub fn observations(
    parsed: Option<&Map<String, Value>>,
    batch: &Value,
    representation: &str,
) -> Result<Observed, StoreError> {
    let Some((parsed, links)) = parsed.and_then(|p| Some((p, p.get("links")?.as_array()?))) else {
        let coverage = json!({
            "representation": representation,
            "observed": 0,
            "omitted": 0,
            "completeness": "unavailable",
            "reason": "eligible anchor extraction is unavailable because the document was not parsed as HTML",
        });
        return Ok((Vec::new(), object(coverage)));
    };
    let omitted = omitted_count(parsed);
    let mut reasons: BTreeMap<String, String> = BTreeMap::new();
    for decision in batch["decisions"].as_array().into_iter().flatten() {
        if let Some(url) = decision.get("url").and_then(Value::as_str) {
            let reason = match decision.get("reason") {
                Some(Value::String(s)) if !s.is_empty() => s.clone(),
                _ => "excluded".to_string(),
            };
            reasons.entry(url.to_string()).or_insert(reason);
        }
    }
    let mut values = Vec::new();
    for link in links {
        let Some(href) = link.get("href").and_then(Value::as_str) else {
            continue;
        };
        let resolved = defrag(href.trim());
        let target = spiderhead_parse::py_url::urlsplit(&resolved)
            .map_err(|e| StoreError::Invalid(e.to_string()))?;
        if !matches!(target.scheme.as_str(), "http" | "https") || target.netloc.is_empty() {
            continue;
        }
        let reason = reasons.get(href).or_else(|| reasons.get(&resolved));
        let raw = link.get("raw_href").map_or(String::new(), |v| match v {
            Value::String(s) => s.clone(),
            Value::Null => String::new(),
            other => other.to_string(),
        });
        values.push(object(json!({
            "ordinal": values.len(),
            "raw_value": raw,
            "resolved_url": resolved,
            "outcome": {
                "state": if reason.is_some() { "excluded" } else { "stored" },
                "reason": reason.map_or("stored without frontier admission", String::as_str),
            },
        })));
    }
    let capped = batch["partial_reasons"]
        .as_array()
        .is_some_and(|r| r.iter().any(|x| x == "link_observations_omitted"));
    let partial = omitted != 0 || capped;
    let coverage = json!({
        "representation": representation,
        "observed": values.len(),
        "omitted": omitted,
        "completeness": if partial { "partial" } else { "complete" },
        "reason": if partial { "link observations omitted by parser cap" } else { "" },
    });
    Ok((values, object(coverage)))
}

fn object(v: Value) -> Map<String, Value> {
    match v {
        Value::Object(m) => m,
        _ => Map::new(),
    }
}

fn doc_text(document: &Value) -> String {
    document
        .as_i64()
        .map_or("None".to_string(), |n| n.to_string())
}

fn context_row(
    kind: &str,
    key: String,
    payload: &Value,
    completeness: &str,
    reason: &str,
) -> Map<String, Value> {
    object(json!({
        "kind": kind,
        "item_key": key,
        "payload_version": OUTER_VERSION,
        "payload_json": canonical_json(payload),
        "completeness": completeness,
        "reason": reason,
    }))
}

/// Build immutable route and coverage contexts after a document is stored.
pub fn context_items(
    page_url_id: i64,
    document_id: Option<i64>,
    coverage: &Map<String, Value>,
    values: &[Map<String, Value>],
) -> Result<Vec<Map<String, Value>>, StoreError> {
    let representation = coverage
        .get("representation")
        .and_then(Value::as_str)
        .unwrap_or("");
    let observed_ok = coverage
        .get("observed")
        .is_some_and(|o| is_nonneg_int(o) && o.as_u64() == Some(values.len() as u64));
    let ordinals_ok = values
        .iter()
        .enumerate()
        .all(|(i, v)| v.get("ordinal").and_then(Value::as_u64) == Some(i as u64));
    if !REPRESENTATIONS.contains(&representation) || !observed_ok || !ordinals_ok {
        return Err(bad(
            "rendered route ledger has invalid page identity or representation",
        ));
    }
    let document = document_id.map_or(Value::Null, Value::from);
    let base = format!(
        "page:{page_url_id}:document:{}:representation:{representation}",
        doc_text(&document)
    );
    let mut items = Vec::new();
    for (ordinal, value) in values.iter().enumerate() {
        let payload = json!({
            "schema_version": ROUTE_VERSION,
            "page_url_id": page_url_id,
            "source_document_id": document,
            "representation": representation,
            "ordinal": ordinal,
            "extraction_method": "a[href]",
            "raw_value": value["raw_value"],
            "resolved_url": value["resolved_url"],
            "outcome": value["outcome"],
        });
        items.push(context_row(
            KIND,
            format!("{base}:ordinal:{ordinal}"),
            &payload,
            "complete",
            "",
        ));
    }
    let mut payload = object(json!({
        "schema_version": COVERAGE_VERSION,
        "page_url_id": page_url_id,
        "source_document_id": document,
    }));
    payload.extend(coverage.clone());
    let completeness = coverage
        .get("completeness")
        .and_then(Value::as_str)
        .unwrap_or("");
    let reason = coverage.get("reason").and_then(Value::as_str).unwrap_or("");
    items.push(context_row(
        COVERAGE_KIND,
        base,
        &Value::Object(payload),
        completeness,
        reason,
    ));
    Ok(items)
}

fn validate_run(item: &ContextItem, payload: &Value) -> Result<(), StoreError> {
    let state = payload.get("state");
    let ok = payload.as_object().is_some_and(|p| {
        exact_keys(
            p,
            &[
                "schema_version",
                "state",
                "reason",
                "store_enabled",
                "rendering_mode",
                "static_sources",
                "rendered_sources",
            ],
        ) && p["schema_version"] == "rendered_route_run_coverage.v1"
            && state
                .and_then(Value::as_str)
                .is_some_and(|s| UNAVAILABLE_STATES.contains(&s))
            && is_str(&p["reason"])
            && p["store_enabled"].is_boolean()
            && matches!(
                p["rendering_mode"].as_str(),
                Some("raw" | "js" | "legacy_fragment")
            )
            && is_nonneg_int(&p["static_sources"])
            && is_nonneg_int(&p["rendered_sources"])
            && item.get("completeness") == state
            && item.get("reason") == Some(&p["reason"])
            && item.get("item_key").and_then(Value::as_str) == Some("run")
    });
    if ok {
        Ok(())
    } else {
        Err(bad("native rendered route run coverage is invalid"))
    }
}

fn item_str<'a>(item: &'a ContextItem, key: &str) -> &'a str {
    item.get(key).and_then(Value::as_str).unwrap_or("")
}

/// Shared identity fields of a route or coverage payload, already type-checked.
fn identity_ok(p: &Map<String, Value>) -> bool {
    is_int(&p["page_url_id"])
        && (p["source_document_id"].is_null() || is_int(&p["source_document_id"]))
        && p["representation"]
            .as_str()
            .is_some_and(|r| REPRESENTATIONS.contains(&r))
}

fn validate_route(item: &ContextItem, payload: &Value) -> Result<(), StoreError> {
    let ok = payload.as_object().is_some_and(|p| {
        exact_keys(
            p,
            &[
                "schema_version",
                "page_url_id",
                "source_document_id",
                "representation",
                "ordinal",
                "extraction_method",
                "raw_value",
                "resolved_url",
                "outcome",
            ],
        ) && p["schema_version"] == ROUTE_VERSION
            && identity_ok(p)
            && is_nonneg_int(&p["ordinal"])
            && p["extraction_method"] == "a[href]"
            && is_str(&p["raw_value"])
            && p["resolved_url"].as_str().is_some_and(|u| !u.is_empty())
            && p["outcome"].as_object().is_some_and(|o| {
                exact_keys(o, &["state", "reason"])
                    && matches!(
                        o["state"].as_str(),
                        Some("stored" | "excluded" | "unavailable")
                    )
                    && is_str(&o["reason"])
            })
            && item_str(item, "completeness") == "complete"
            && item_str(item, "reason").is_empty()
    });
    if !ok {
        return Err(bad("native rendered route ledger context is invalid"));
    }
    let key = item_str(item, "item_key");
    if !key_matches(payload, key, Some(&payload["ordinal"])) {
        return Err(bad("native rendered route ledger key is invalid"));
    }
    Ok(())
}

/// Both accepted spellings of an item key (with and without the document segment).
fn key_matches(payload: &Value, key: &str, ordinal: Option<&Value>) -> bool {
    let page = payload["page_url_id"].as_i64().unwrap_or_default();
    let rep = payload["representation"].as_str().unwrap_or("");
    let tail = ordinal.map_or(String::new(), |o| {
        format!(":ordinal:{}", o.as_i64().unwrap_or_default())
    });
    key == format!("page:{page}:representation:{rep}{tail}")
        || key
            == format!(
                "page:{page}:document:{}:representation:{rep}{tail}",
                doc_text(&payload["source_document_id"])
            )
}

fn validate_coverage(item: &ContextItem, payload: &Value) -> Result<(), StoreError> {
    let ok = payload.as_object().is_some_and(|p| {
        let completeness = p["completeness"].as_str().unwrap_or("");
        let reason = p["reason"].as_str();
        exact_keys(
            p,
            &[
                "schema_version",
                "page_url_id",
                "source_document_id",
                "representation",
                "observed",
                "omitted",
                "completeness",
                "reason",
            ],
        ) && p["schema_version"] == COVERAGE_VERSION
            && identity_ok(p)
            && is_nonneg_int(&p["observed"])
            && is_nonneg_int(&p["omitted"])
            && UNAVAILABLE_STATES.contains(&completeness)
            && reason.is_some()
            && !(completeness == "complete" && (p["omitted"] != 0 || p["reason"] != ""))
            && !(completeness == "partial" && p["reason"] == "")
            && !(completeness == "unavailable"
                && (p["observed"] != 0 || p["omitted"] != 0 || p["reason"] == ""))
            && item.get("completeness") == Some(&p["completeness"])
            && item.get("reason") == Some(&p["reason"])
            && key_matches(payload, item_str(item, "item_key"), None)
    });
    if ok {
        Ok(())
    } else {
        Err(bad("native rendered route coverage context is invalid"))
    }
}

/// Validate one route-ledger context row against its payload and the stored pages/documents.
pub fn validate_context(
    con: &Connection,
    item: &ContextItem,
    payload: &Value,
) -> Result<(), StoreError> {
    match item_str(item, "kind") {
        RUN_KIND => return validate_run(item, payload),
        KIND => validate_route(item, payload)?,
        COVERAGE_KIND => validate_coverage(item, payload)?,
        _ => return Err(bad("native rendered route context kind is invalid")),
    }
    let page = payload["page_url_id"].as_i64().unwrap_or_default();
    let has_page = con
        .query_row("SELECT 1 FROM pages WHERE url_id=?1", params![page], |_| {
            Ok(())
        })
        .optional()?
        .is_some();
    if !has_page {
        return Err(bad(
            "native rendered route context references an unknown page",
        ));
    }
    if let Some(document) = payload["source_document_id"].as_i64() {
        let found = con
            .query_row(
                "SELECT 1 FROM documents WHERE document_id=?1 AND url_id=?2 AND representation=?3",
                params![document, page, payload["representation"].as_str()],
                |_| Ok(()),
            )
            .optional()?;
        if found.is_none() {
            return Err(bad(
                "native rendered route context document does not match its page or representation",
            ));
        }
    }
    if item_str(item, "kind") == COVERAGE_KIND {
        validate_coverage_routes(con, payload.as_object().unwrap_or(&Map::new()))?;
    }
    Ok(())
}

/// Check a stored coverage record against the occurrence rows it closes.
pub fn validate_coverage_routes(
    con: &Connection,
    coverage: &Map<String, Value>,
) -> Result<(), StoreError> {
    let page = coverage.get("page_url_id").cloned().unwrap_or(Value::Null);
    let null = Value::Null;
    let mut stmt =
        con.prepare("SELECT payload_json FROM context_items WHERE kind=?1 AND item_key LIKE ?2")?;
    let like = format!(
        "page:{}:%",
        page.as_i64().map_or("None".to_string(), |n| n.to_string())
    );
    let rows = stmt.query_map(params![KIND, like], |r| r.get::<_, String>(0))?;
    let mut ordinals = Vec::new();
    for row in rows {
        let route: Value = serde_json::from_str(&row?)
            .map_err(|_| bad("native rendered route ledger context is invalid JSON"))?;
        let field = |k: &str| route.get(k).unwrap_or(&null);
        if field("source_document_id") != coverage.get("source_document_id").unwrap_or(&null)
            || field("representation") != coverage.get("representation").unwrap_or(&null)
        {
            continue;
        }
        if field("page_url_id") != &page || !is_int(field("ordinal")) {
            return Err(bad(
                "native rendered route coverage does not match its occurrence rows",
            ));
        }
        ordinals.push(field("ordinal").as_i64().unwrap_or(-1));
    }
    ordinals.sort_unstable();
    let observed = coverage
        .get("observed")
        .and_then(Value::as_i64)
        .unwrap_or(-1);
    if !ordinals.iter().copied().eq(0..observed) {
        return Err(bad(
            "native rendered route coverage has noncontiguous ordinals or wrong observed count",
        ));
    }
    Ok(())
}

/// Validate every closed occurrence population after all contexts are available.
pub fn validate_ledger(con: &Connection) -> Result<(), StoreError> {
    let mut coverage = Vec::new();
    let mut without_coverage: BTreeSet<String> = BTreeSet::new();
    let mut stmt =
        con.prepare("SELECT kind,payload_json FROM context_items WHERE kind IN (?1,?2)")?;
    let rows = stmt.query_map(params![KIND, COVERAGE_KIND], |r| {
        Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?))
    })?;
    let identity = |p: &Value| {
        canonical_json(&json!([
            p["page_url_id"],
            p["source_document_id"],
            p["representation"]
        ]))
    };
    for row in rows {
        let (kind, text) = row?;
        let payload: Value = serde_json::from_str(&text)?;
        if kind == COVERAGE_KIND {
            coverage.push(payload);
        } else {
            without_coverage.insert(identity(&payload));
        }
    }
    for item in &coverage {
        validate_coverage_routes(con, item.as_object().unwrap_or(&Map::new()))?;
        without_coverage.remove(&identity(item));
    }
    if without_coverage.is_empty() {
        Ok(())
    } else {
        Err(bad("native rendered route occurrences lack coverage"))
    }
}

fn unavailable() -> Value {
    json!({"state": "unavailable", "reason": "route ledger is absent from this older scan"})
}

/// Read immutable observations and derive route relation only from complete sides.
pub fn read(con: &Connection) -> Result<Map<String, Value>, StoreError> {
    let mut routes: Vec<Value> = Vec::new();
    let mut coverage: IndexMap<(i64, String), Value> = IndexMap::new();
    let mut run: Option<Value> = None;
    let mut stmt = con.prepare(
        "SELECT kind,payload_json FROM context_items WHERE kind IN (?1,?2,?3) ORDER BY kind,item_key",
    )?;
    let rows = stmt.query_map(params![KIND, COVERAGE_KIND, RUN_KIND], |r| {
        Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?))
    })?;
    let page_of = |p: &Value| {
        p["page_url_id"]
            .as_i64()
            .ok_or_else(|| bad("native rendered route context is invalid"))
    };
    for row in rows {
        let (kind, text) = row?;
        let payload: Value = serde_json::from_str(&text)?;
        match kind.as_str() {
            RUN_KIND => run = Some(payload),
            COVERAGE_KIND => {
                let rep = payload["representation"].as_str().unwrap_or("").to_string();
                coverage.insert((page_of(&payload)?, rep), payload);
            }
            _ => routes.push(payload),
        }
    }
    let mut grouped: BTreeMap<(i64, String, String), Vec<Value>> = BTreeMap::new();
    for route in routes {
        let key = (
            page_of(&route)?,
            route["extraction_method"]
                .as_str()
                .unwrap_or("")
                .to_string(),
            route["resolved_url"].as_str().unwrap_or("").to_string(),
        );
        grouped.entry(key).or_default().push(route);
    }
    let mut output = Vec::new();
    for ((page, method, url), values) in grouped {
        let reps: BTreeSet<&str> = values
            .iter()
            .filter_map(|v| v["representation"].as_str())
            .collect();
        let is_complete = |c: &Value| c["completeness"] == "complete";
        let static_side = coverage.get(&(page, "static".to_string()));
        let counterparts: Vec<&Value> = coverage
            .iter()
            .filter(|((p, rep), _)| *p == page && rep != "static")
            .map(|(_, v)| v)
            .collect();
        let complete = static_side.is_some_and(is_complete)
            && !counterparts.is_empty()
            && counterparts.iter().all(|c| is_complete(c));
        let relation = if !complete {
            "unknown"
        } else if reps.contains("static") && reps.len() > 1 {
            "shared"
        } else if reps.contains("static") {
            "raw_only"
        } else {
            "rendered_only"
        };
        output.push(json!({
            "page_url_id": page,
            "extraction_method": method,
            "resolved_url": url,
            "relation": relation,
            "occurrences": values,
        }));
    }
    let mut result = Map::new();
    result.insert("routes".into(), Value::Array(output));
    let coverage_list: Vec<Value> = if coverage.is_empty() {
        vec![unavailable()]
    } else {
        coverage.into_values().collect()
    };
    result.insert("coverage".into(), Value::Array(coverage_list));
    result.insert("run_coverage".into(), run.unwrap_or_else(unavailable));
    Ok(result)
}

/// The run-level coverage context row.
pub fn run_context(
    state: &str,
    reason: &str,
    store_enabled: bool,
    mode: &str,
    static_: i64,
    rendered: i64,
) -> Map<String, Value> {
    let payload = json!({
        "schema_version": "rendered_route_run_coverage.v1",
        "state": state,
        "reason": reason,
        "store_enabled": store_enabled,
        "rendering_mode": mode,
        "static_sources": static_,
        "rendered_sources": rendered,
    });
    context_row(RUN_KIND, "run".to_string(), &payload, state, reason)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn db() -> Connection {
        let con = Connection::open_in_memory().expect("open");
        con.execute_batch(
            "CREATE TABLE pages(url_id INTEGER);
             CREATE TABLE documents(document_id INTEGER, url_id INTEGER, representation TEXT);
             CREATE TABLE context_items(kind TEXT, item_key TEXT, payload_version TEXT,
                 payload_json TEXT, completeness TEXT, reason TEXT);
             INSERT INTO pages VALUES (7);
             INSERT INTO documents VALUES (3, 7, 'static'), (4, 7, 'rendered');",
        )
        .expect("schema");
        con
    }

    fn store(con: &Connection, items: &[Map<String, Value>]) {
        for item in items {
            validate_context(
                con,
                item,
                &serde_json::from_str(item["payload_json"].as_str().unwrap()).unwrap(),
            )
            .expect("valid");
            con.execute(
                "INSERT INTO context_items VALUES (?1,?2,?3,?4,?5,?6)",
                params![
                    item["kind"].as_str(),
                    item["item_key"].as_str(),
                    item["payload_version"].as_str(),
                    item["payload_json"].as_str(),
                    item["completeness"].as_str(),
                    item["reason"].as_str()
                ],
            )
            .expect("insert");
        }
    }

    fn parsed(links: Value) -> Map<String, Value> {
        object(json!({"links": links, "link_observation": {"omitted": 0}}))
    }

    fn page(
        rep: &str,
        hrefs: &[&str],
        completeness: &str,
    ) -> (Vec<Map<String, Value>>, Map<String, Value>) {
        let links: Vec<Value> = hrefs
            .iter()
            .map(|h| json!({"href": h, "raw_href": h}))
            .collect();
        let (values, mut coverage) =
            observations(Some(&parsed(Value::Array(links))), &json!({}), rep)
                .expect("observations");
        coverage.insert("completeness".into(), json!(completeness));
        if completeness == "unavailable" {
            coverage = object(json!({"representation": rep, "observed": 0, "omitted": 0,
                "completeness": "unavailable", "reason": "render failed"}));
            return (vec![], coverage);
        }
        (values, coverage)
    }

    // Eligible raw observations are stored without mutating the frontier.
    // Expected strings are the exact stored values.
    #[test]
    fn observations_and_items_match_python_bytes() {
        let p = parsed(json!([
            {"href": "http://h.test/foo#x", "raw_href": "/foo#x"},
            {"href": "https://Ex.test/a?#f", "raw_href": "\u{e9}"},
            {"href": "mailto:a@b"},
            {"href": "HTTP://h.test/p;#z"},
            {"href": "http://h.test/x"},
            {"raw_href": "no href"},
        ]));
        let batch = json!({
            "decisions": [{"url": "http://h.test/x", "reason": "blocked_by_robots"}],
            "partial_reasons": ["link_observations_omitted"],
        });
        let (values, coverage) = observations(Some(&p), &batch, "static").unwrap();
        assert_eq!(values.len(), 4);
        assert_eq!(values[1]["resolved_url"], "https://Ex.test/a?");
        assert_eq!(values[2]["resolved_url"], "http://h.test/p;");
        assert_eq!(
            values[3]["outcome"],
            json!({"state": "excluded", "reason": "blocked_by_robots"})
        );
        assert_eq!(coverage["completeness"], "partial");
        let items = context_items(7, Some(3), &coverage, &values).unwrap();
        assert_eq!(items.len(), 5);
        assert_eq!(
            items[1]["payload_json"],
            r#"{"extraction_method":"a[href]","ordinal":1,"outcome":{"reason":"stored without frontier admission","state":"stored"},"page_url_id":7,"raw_value":"\u00e9","representation":"static","resolved_url":"https://Ex.test/a?","schema_version":"rendered_route_ledger.v1","source_document_id":3}"#
        );
        assert_eq!(
            items[3]["item_key"],
            "page:7:document:3:representation:static:ordinal:3"
        );
        assert_eq!(items[4]["kind"], COVERAGE_KIND);
        assert_eq!(
            items[4]["payload_json"],
            r#"{"completeness":"partial","observed":4,"omitted":0,"page_url_id":7,"reason":"link observations omitted by parser cap","representation":"static","schema_version":"rendered_route_coverage.v1","source_document_id":3}"#
        );
        assert_eq!(items[4]["completeness"], "partial");
    }

    // A non-HTML document records unavailable coverage.
    #[test]
    fn observations_without_links_are_unavailable() {
        let (values, coverage) = observations(None, &json!({}), "rendered").unwrap();
        assert!(values.is_empty());
        assert_eq!(coverage["completeness"], "unavailable");
        assert_eq!(coverage["observed"], 0);
        let (_, cov) =
            observations(Some(&object(json!({"links": "x"}))), &json!({}), "static").unwrap();
        assert_eq!(cov["completeness"], "unavailable");
    }

    #[test]
    fn parser_cap_marks_partial_and_document_none_is_spelled_none() {
        let mut p = parsed(json!([{"href": "http://h.test/a"}]));
        p.insert("link_observation".into(), json!({"omitted": 3}));
        let (values, coverage) = observations(Some(&p), &json!({}), "static").unwrap();
        assert_eq!(
            (
                coverage["omitted"].as_i64(),
                coverage["completeness"].as_str()
            ),
            (Some(3), Some("partial"))
        );
        let items = context_items(1, None, &coverage, &values).unwrap();
        assert_eq!(
            items[0]["item_key"],
            "page:1:document:None:representation:static:ordinal:0"
        );
    }

    #[test]
    fn run_context_matches_reference_bytes() {
        let row = run_context("partial", "why", true, "js", 4, 5);
        assert_eq!(
            row["payload_json"],
            r#"{"reason":"why","rendered_sources":5,"rendering_mode":"js","schema_version":"rendered_route_run_coverage.v1","state":"partial","static_sources":4,"store_enabled":true}"#
        );
        assert_eq!(
            (row["item_key"].as_str(), row["completeness"].as_str()),
            (Some("run"), Some("partial"))
        );
        let con = db();
        let payload: Value = serde_json::from_str(row["payload_json"].as_str().unwrap()).unwrap();
        validate_context(&con, &row, &payload).unwrap();
        let mut bad_state = row.clone();
        bad_state.insert("completeness".into(), json!("complete"));
        assert!(validate_context(&con, &bad_state, &payload).is_err());
    }

    #[test]
    fn context_items_refuses_wrong_identity() {
        let (values, mut coverage) = page("static", &["http://h.test/a"], "complete");
        coverage.insert("representation".into(), json!("bogus"));
        assert!(context_items(1, None, &coverage, &values).is_err());
        coverage.insert("representation".into(), json!("static"));
        coverage.insert("observed".into(), json!(2));
        assert!(context_items(1, None, &coverage, &values).is_err());
        coverage.insert("observed".into(), json!(1));
        let mut gap = values.clone();
        gap[0].insert("ordinal".into(), json!(1));
        assert!(context_items(1, None, &coverage, &gap).is_err());
    }

    // The shared relation is derived from the resolved URL, not the raw spelling.
    #[test]
    fn complete_counterpart_derives_shared_relation() {
        let con = db();
        let (sv, sc) = page(
            "static",
            &["http://h.test/foo", "http://h.test/only-raw"],
            "complete",
        );
        let (rv, rc) = page(
            "rendered",
            &["http://h.test/foo", "http://h.test/only-rendered"],
            "complete",
        );
        store(&con, &context_items(7, Some(3), &sc, &sv).unwrap());
        store(&con, &context_items(7, Some(4), &rc, &rv).unwrap());
        validate_ledger(&con).unwrap();
        let result = read(&con).unwrap();
        let relations: Vec<(&str, &str)> = result["routes"]
            .as_array()
            .unwrap()
            .iter()
            .map(|r| {
                (
                    r["resolved_url"].as_str().unwrap(),
                    r["relation"].as_str().unwrap(),
                )
            })
            .collect();
        assert_eq!(
            relations,
            [
                ("http://h.test/foo", "shared"),
                ("http://h.test/only-raw", "raw_only"),
                ("http://h.test/only-rendered", "rendered_only"),
            ]
        );
        assert_eq!(
            result["routes"][0]["occurrences"].as_array().unwrap().len(),
            2
        );
        assert_eq!(result["coverage"].as_array().unwrap().len(), 2);
        assert_eq!(result["run_coverage"]["state"], "unavailable");
    }

    // Unavailable render coverage keeps the static relation unknown and rolls back.
    #[test]
    fn unavailable_counterpart_keeps_relation_unknown() {
        let con = db();
        let (sv, sc) = page("static", &["http://h.test/foo"], "complete");
        let (rv, rc) = page("rendered", &[], "unavailable");
        store(&con, &context_items(7, Some(3), &sc, &sv).unwrap());
        store(&con, &context_items(7, Some(4), &rc, &rv).unwrap());
        assert_eq!(read(&con).unwrap()["routes"][0]["relation"], "unknown");
        // static side alone has no counterpart either
        let con2 = db();
        store(&con2, &context_items(7, Some(3), &sc, &sv).unwrap());
        assert_eq!(read(&con2).unwrap()["routes"][0]["relation"], "unknown");
    }

    #[test]
    fn read_of_an_older_scan_reports_unavailable() {
        let con = db();
        let result = read(&con).unwrap();
        assert_eq!(result["routes"], json!([]));
        assert_eq!(result["coverage"][0]["state"], "unavailable");
        assert_eq!(
            result["run_coverage"]["reason"],
            "route ledger is absent from this older scan"
        );
    }

    #[test]
    fn ledger_requires_coverage_for_every_occurrence_population() {
        let con = db();
        let (sv, sc) = page("static", &["http://h.test/foo"], "complete");
        let items = context_items(7, Some(3), &sc, &sv).unwrap();
        store(&con, &items[..1]);
        let err = validate_ledger(&con).unwrap_err().to_string();
        assert!(err.contains("lack coverage"), "{err}");
    }

    #[test]
    fn coverage_must_close_contiguous_ordinals() {
        let con = db();
        let (sv, sc) = page(
            "static",
            &["http://h.test/a", "http://h.test/b"],
            "complete",
        );
        let items = context_items(7, Some(3), &sc, &sv).unwrap();
        // drop ordinal 0 but keep the coverage that claims two observations
        store(&con, &items[1..2]);
        let cov = &items[2];
        let payload: Value = serde_json::from_str(cov["payload_json"].as_str().unwrap()).unwrap();
        let err = validate_context(&con, cov, &payload)
            .unwrap_err()
            .to_string();
        assert!(err.contains("noncontiguous"), "{err}");
    }

    #[test]
    fn context_validation_rejects_bad_shapes() {
        let con = db();
        let (sv, sc) = page("static", &["http://h.test/a"], "complete");
        let items = context_items(7, Some(3), &sc, &sv).unwrap();
        let route = &items[0];
        let payload: Value = serde_json::from_str(route["payload_json"].as_str().unwrap()).unwrap();
        validate_context(&con, route, &payload).unwrap();
        let msg = |item: &Map<String, Value>, p: &Value| {
            validate_context(&con, item, p).unwrap_err().to_string()
        };
        let mut wrong_key = route.clone();
        wrong_key.insert(
            "item_key".into(),
            json!("page:7:representation:rendered:ordinal:0"),
        );
        assert!(msg(&wrong_key, &payload).contains("ledger key"));
        let mut extra = payload.clone();
        extra["surprise"] = json!(1);
        assert!(msg(route, &extra).contains("ledger context is invalid"));
        let mut unknown_page = payload.clone();
        unknown_page["page_url_id"] = json!(99);
        let mut k = route.clone();
        k.insert(
            "item_key".into(),
            json!("page:99:document:3:representation:static:ordinal:0"),
        );
        assert!(msg(&k, &unknown_page).contains("unknown page"));
        let mut other_rep = payload.clone();
        other_rep["representation"] = json!("rendered");
        let mut k2 = route.clone();
        k2.insert(
            "item_key".into(),
            json!("page:7:document:3:representation:rendered:ordinal:0"),
        );
        assert!(msg(&k2, &other_rep).contains("document does not match"));
        let mut kind = route.clone();
        kind.insert("kind".into(), json!("nope"));
        assert!(msg(&kind, &payload).contains("kind is invalid"));
        let mut float_bool = payload.clone();
        float_bool["ordinal"] = json!(true);
        assert!(msg(route, &float_bool).contains("ledger context is invalid"));
    }

    #[test]
    fn coverage_rules_by_completeness() {
        let con = db();
        let mk = |completeness: &str, observed: i64, omitted: i64, reason: &str| {
            let payload = json!({"schema_version": COVERAGE_VERSION, "page_url_id": 7,
                "source_document_id": null, "representation": "static", "observed": observed,
                "omitted": omitted, "completeness": completeness, "reason": reason});
            let item = context_row(
                COVERAGE_KIND,
                "page:7:representation:static".into(),
                &payload,
                completeness,
                reason,
            );
            validate_context(&con, &item, &payload).is_ok()
        };
        assert!(mk("complete", 0, 0, ""));
        assert!(!mk("complete", 0, 1, ""));
        assert!(!mk("complete", 0, 0, "why"));
        assert!(mk("partial", 0, 2, "cap"));
        assert!(!mk("partial", 0, 2, ""));
        assert!(mk("unavailable", 0, 0, "no"));
        assert!(!mk("unavailable", 1, 0, "no"));
        assert!(!mk("unavailable", 0, 0, ""));
    }
}
