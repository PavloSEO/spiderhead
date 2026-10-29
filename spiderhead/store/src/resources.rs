//! Transactional JS/CSS declaration inventory; fetching remains outside this module.

use crate::error::StoreError;
use indexmap::IndexMap;
use rusqlite::types::ValueRef;
use rusqlite::{Connection, OptionalExtension};
use serde_json::Value;

/// Metadata bound per record set: 8 MiB.
const MAX_RECORD_BYTES: usize = 8 * 1024 * 1024;

/// Admit declared text resources by their measured media type, without sniffing.
pub fn media_matches(kind: &str, content_type: &str) -> bool {
    let media = content_type
        .split(';')
        .next()
        .unwrap_or("")
        .trim()
        .to_lowercase();
    let allowed: &[&str] = match kind {
        "stylesheet" => &["text/css"],
        "script" => &[
            "text/javascript",
            "application/javascript",
            "text/ecmascript",
            "application/ecmascript",
            "application/x-javascript",
        ],
        _ => &[],
    };
    allowed.contains(&media.as_str())
}

/// Return the URL id for `url`, inserting the URL row when it is new.
fn url_id(con: &Connection, url: &str) -> Result<i64, StoreError> {
    con.execute("INSERT OR IGNORE INTO urls(url) VALUES(?)", [url])?;
    Ok(con.query_row("SELECT url_id FROM urls WHERE url=?", [url], |r| r.get(0))?)
}

/// UTF-8 byte length of a flat string map serialized as JSON, with non-ASCII text kept.
fn python_json_len(item: &IndexMap<String, Value>) -> usize {
    let body: Vec<String> = item
        .iter()
        .map(|(k, v)| {
            format!(
                "{}: {}",
                json_string(k),
                json_string(v.as_str().unwrap_or(""))
            )
        })
        .collect();
    format!("{{{}}}", body.join(", ")).len()
}

/// JSON string literal: quotes, backslashes and control characters are escaped; other text is kept.
fn json_string(value: &str) -> String {
    serde_json::to_string(value).unwrap_or_else(|_| "\"\"".to_owned())
}

#[allow(clippy::too_many_arguments)] // Python signature: keyword-only inputs kept positional
/// Replace one document representation's resource occurrences in the caller transaction.
/// Defaults: inventory state `complete`, omitted count 0.
pub fn put_declarations(
    con: &Connection,
    page_url_id: i64,
    document_id: i64,
    representation: &str,
    declarations: &[IndexMap<String, Value>],
    fetch_enabled: bool,
    inventory_state: &str,
    omitted: i64,
) -> Result<IndexMap<String, i64>, StoreError> {
    if !matches!(representation, "static" | "rendered" | "legacy_fragment") {
        return Err(invalid("resource representation is invalid"));
    }
    if !matches!(inventory_state, "complete" | "partial" | "unavailable") || omitted < 0 {
        return Err(invalid("resource inventory state is invalid"));
    }
    if (inventory_state == "complete" || inventory_state == "unavailable") && omitted != 0 {
        return Err(invalid(
            "resource omission count disagrees with inventory state",
        ));
    }
    let document: Option<(i64, String)> = con
        .query_row(
            "SELECT url_id,representation FROM documents WHERE document_id=?",
            [document_id],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .optional()?;
    match document {
        Some((url, rep)) if url == page_url_id && rep == representation => {}
        _ => {
            return Err(invalid(
                "resource document URL or representation disagrees with its page",
            ))
        }
    }
    con.execute(
        "DELETE FROM resource_refs WHERE page_url_id=? AND representation=?",
        (page_url_id, representation),
    )?;
    let mut counters: IndexMap<&str, i64> = IndexMap::from([("script", 0), ("stylesheet", 0)]);
    let state = if fetch_enabled {
        "not_fetched"
    } else {
        "resources_disabled"
    };
    let reason = if fetch_enabled {
        "not fetched"
    } else {
        "resources.fetch disabled"
    };
    let mut size = 0usize;
    for (index, item) in declarations.iter().enumerate() {
        if index >= 20_000 || inventory_state == "unavailable" {
            return Err(invalid(
                "resource declarations exceed their bounded measured scope",
            ));
        }
        let kind = item.get("kind").and_then(Value::as_str).unwrap_or("");
        let shape_ok = item.len() == 3
            && item.contains_key("kind")
            && item.contains_key("url")
            && item.contains_key("raw_url")
            && counters.contains_key(kind)
            && ["url", "raw_url"].iter().all(|key| {
                item.get(*key)
                    .and_then(Value::as_str)
                    .is_some_and(|s| !s.is_empty())
            });
        if !shape_ok {
            return Err(invalid(
                "resource declaration must be a kind/url/raw_url object",
            ));
        }
        size += python_json_len(item);
        if size > MAX_RECORD_BYTES {
            return Err(invalid("resource declaration metadata exceeds 8 MiB"));
        }
        let url = item["url"].as_str().unwrap_or("");
        let raw_url = item["raw_url"].as_str().unwrap_or("");
        let resource_url = url_id(con, url)?;
        let ordinal = counters.get(kind).copied().unwrap_or(0);
        con.execute(
            "INSERT INTO resource_refs(page_url_id,ordinal,resource_url_id,source_document_id,kind,representation,raw_url,capture_state,reason) \
             VALUES(?,?,?,?,?,?,?,?,?)",
            (
                page_url_id,
                ordinal,
                resource_url,
                document_id,
                kind,
                representation,
                raw_url,
                state,
                reason,
            ),
        )?;
        if let Some(count) = counters.get_mut(kind) {
            *count += 1;
        }
    }
    // Keys are sorted, with the default JSON separators.
    let payload = format!(
        "{{\"document_id\": {document_id}, \"omitted\": {omitted}, \"state\": \"{inventory_state}\"}}"
    );
    con.execute(
        "DELETE FROM context_items WHERE kind='resource_inventory' AND item_key=?",
        [format!("document:{document_id}")],
    )?;
    let context_reason = match inventory_state {
        "complete" => "",
        "partial" => "resource declarations omitted",
        _ => "resource declarations were not measured",
    };
    con.execute(
        "INSERT INTO context_items(kind,item_key,payload_version,payload_json,completeness,reason) VALUES(?,?,?,?,?,?)",
        (
            "resource_inventory",
            format!("document:{document_id}"),
            "scan_context.v1",
            payload,
            inventory_state,
            context_reason,
        ),
    )?;
    let mut out = IndexMap::new();
    out.insert("script".to_owned(), counters["script"]);
    out.insert("stylesheet".to_owned(), counters["stylesheet"]);
    out.insert("omitted".to_owned(), omitted);
    Ok(out)
}

/// Validate the closed zero/partial/unavailable inventory marker.
/// `item` is one `context_items` row as produced by [`dict_rows`].
pub fn validate_inventory_context(
    con: &Connection,
    item: &IndexMap<String, Value>,
) -> Result<(), StoreError> {
    let text = |key: &str| {
        item.get(key)
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_owned()
    };
    let payload: Value = serde_json::from_str(&text("payload_json"))
        .map_err(|_| invalid("resource inventory context is invalid JSON"))?;
    let obj = payload
        .as_object()
        .ok_or_else(|| invalid("resource inventory context is invalid"))?;
    let document_id = obj.get("document_id").and_then(Value::as_i64);
    let state = obj.get("state").and_then(Value::as_str).unwrap_or("");
    let omitted = obj.get("omitted").and_then(Value::as_i64);
    let valid = obj.len() == 3
        && document_id.is_some_and(|d| d >= 1)
        && text("item_key") == format!("document:{}", document_id.unwrap_or(0))
        && text("payload_version") == "scan_context.v1"
        && matches!(state, "complete" | "partial" | "unavailable")
        && text("completeness") == state
        && omitted.is_some_and(|o| o >= 0)
        && !(state != "partial" && omitted != Some(0))
        && !(state == "complete" && !text("reason").is_empty())
        && !(state != "complete" && text("reason").is_empty());
    if !valid {
        return Err(invalid("resource inventory context is invalid"));
    }
    let exists: Option<i64> = con
        .query_row(
            "SELECT 1 FROM documents WHERE document_id=?",
            [document_id.unwrap_or(0)],
            |r| r.get(0),
        )
        .optional()?;
    if exists.is_none() {
        return Err(invalid("resource inventory context is invalid"));
    }
    Ok(())
}

/// Read every row of `sql` as a column-name keyed map.
pub fn dict_rows(con: &Connection, sql: &str) -> Result<Vec<IndexMap<String, Value>>, StoreError> {
    let mut stmt = con.prepare(sql)?;
    let names: Vec<String> = stmt
        .column_names()
        .iter()
        .map(|s| (*s).to_owned())
        .collect();
    let mut rows = stmt.query([])?;
    let mut out = Vec::new();
    while let Some(row) = rows.next()? {
        let mut map = IndexMap::new();
        for (index, name) in names.iter().enumerate() {
            let value = match row.get_ref(index)? {
                ValueRef::Null => Value::Null,
                ValueRef::Integer(i) => Value::from(i),
                ValueRef::Real(f) => serde_json::Number::from_f64(f)
                    .map(Value::Number)
                    .unwrap_or(Value::Null),
                ValueRef::Text(t) => Value::String(String::from_utf8_lossy(t).into_owned()),
                ValueRef::Blob(_) => Value::Null,
            };
            map.insert(name.clone(), value);
        }
        out.push(map);
    }
    Ok(out)
}

/// Check inventory, references and observed bodies without loading the graph.
pub fn validate_resources(con: &Connection) -> Result<(), StoreError> {
    for item in dict_rows(
        con,
        "SELECT * FROM context_items WHERE kind='resource_inventory'",
    )? {
        validate_inventory_context(con, &item)?;
    }
    for item in dict_rows(con, "SELECT * FROM resource_refs")? {
        let int = |key: &str| item.get(key).and_then(Value::as_i64);
        let text = |key: &str| {
            item.get(key)
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_owned()
        };
        let document: Option<(i64, String)> = con
            .query_row(
                "SELECT url_id,representation FROM documents WHERE document_id=?",
                [int("source_document_id").unwrap_or(0)],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .optional()?;
        if document != Some((int("page_url_id").unwrap_or(0), text("representation"))) {
            return Err(invalid("resource reference document provenance disagrees"));
        }
        let representation = text("representation");
        if !matches!(
            representation.as_str(),
            "static" | "rendered" | "legacy_fragment"
        ) {
            return Err(invalid("resource representation is invalid"));
        }
        let inventory: Option<i64> = con
            .query_row(
                "SELECT 1 FROM context_items WHERE kind='resource_inventory' AND item_key=?",
                [format!(
                    "document:{}",
                    int("source_document_id").unwrap_or(0)
                )],
                |r| r.get(0),
            )
            .optional()?;
        if inventory.is_none() {
            return Err(invalid("resource reference lacks its inventory context"));
        }
        let state = text("capture_state");
        let has_response = int("response_id").is_some();
        let reason = text("reason");
        if matches!(state.as_str(), "resources_disabled" | "not_fetched") && has_response {
            return Err(invalid(
                "unfetched resource reference cannot name a response",
            ));
        }
        if state == "measured" && (!has_response || !reason.is_empty()) {
            return Err(invalid(
                "measured resource requires a complete response and empty reason",
            ));
        }
        if state == "body_unavailable" && !has_response {
            return Err(invalid(
                "unavailable resource body requires an observed response",
            ));
        }
        if state != "measured" && reason.is_empty() {
            return Err(invalid("unmeasured resource requires a named reason"));
        }
        if let Some(response_id) = int("response_id") {
            let response: Option<StoredResponse> = con
                .query_row(
                    "SELECT request_url_id,purpose,body_state,body_reason,effective_status_code \
                     FROM responses WHERE response_id=?",
                    [response_id],
                    |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?)),
                )
                .optional()?;
            let Some((request_url, purpose, body_state, body_reason, status)) = response else {
                return Err(invalid("resource response URL or purpose disagrees"));
            };
            if (request_url, purpose.as_str())
                != (int("resource_url_id").unwrap_or(0), text("kind").as_str())
            {
                return Err(invalid("resource response URL or purpose disagrees"));
            }
            if state == "measured"
                && (body_state != "complete" || !status.is_some_and(|s| (200..300).contains(&s)))
            {
                return Err(invalid(
                    "measured resource lacks a successful complete response body",
                ));
            }
            if state == "body_unavailable"
                && (body_state == "complete" || body_reason.unwrap_or_default() != reason)
            {
                return Err(invalid(
                    "resource body unavailability disagrees with its response",
                ));
            }
        }
        if int("ordinal").is_none_or(|o| o < 0) {
            return Err(invalid("resource ordinal is invalid"));
        }
    }
    let gap: Option<i64> = con
        .query_row(
            "SELECT 1 FROM resource_refs GROUP BY page_url_id,representation,kind \
             HAVING MIN(ordinal)!=0 OR MAX(ordinal)!=COUNT(*)-1 LIMIT 1",
            [],
            |r| r.get(0),
        )
        .optional()?;
    if gap.is_some() {
        return Err(invalid("resource occurrence ordinals are not contiguous"));
    }
    Ok(())
}

/// Measure current declaration coverage separately from resource body coverage.
pub fn resource_capabilities(
    con: &Connection,
    fetch_enabled: bool,
) -> Result<IndexMap<String, IndexMap<String, String>>, StoreError> {
    // Only the most recent extraction for each logical URL/representation is
    // active; an older partial attempt must not poison its replacement.
    let mut states: IndexMap<&str, i64> =
        IndexMap::from([("complete", 0), ("partial", 0), ("unavailable", 0)]);
    let mut stmt = con.prepare(
        "SELECT c.completeness FROM documents d CROSS JOIN context_items c \
         WHERE c.kind='resource_inventory' AND c.item_key='document:'||d.document_id \
         AND NOT EXISTS(SELECT 1 FROM context_items later JOIN documents nd \
         ON later.item_key='document:'||nd.document_id WHERE later.kind='resource_inventory' \
         AND nd.url_id=d.url_id AND nd.representation=d.representation AND nd.document_id>d.document_id)",
    )?;
    let completeness: Vec<String> = stmt
        .query_map([], |r| r.get(0))?
        .collect::<Result<_, _>>()?;
    for state in completeness {
        let slot = states
            .get_mut(state.as_str())
            .ok_or_else(|| invalid("resource inventory completeness is invalid"))?;
        *slot += 1;
    }
    let missing: i64 = con.query_row(
        "SELECT COUNT(*) FROM pages p WHERE lower(p.content_type) LIKE '%html%' AND \
         (NOT EXISTS(SELECT 1 FROM context_items c JOIN documents d ON c.item_key='document:'||d.document_id \
         WHERE c.kind='resource_inventory' AND d.url_id=p.url_id AND d.representation='static') \
         OR (p.representation!='static' AND NOT EXISTS(SELECT 1 FROM context_items c \
         WHERE c.kind='resource_inventory' AND c.item_key='document:'||p.document_id)))",
        [],
        |r| r.get(0),
    )?;
    let total: i64 = states.values().sum();
    let (refs_state, refs_reason) = if total == 0
        || (states["unavailable"] > 0 && states["complete"] == 0 && states["partial"] == 0)
    {
        (
            "unavailable",
            "resource declarations were not measured".to_owned(),
        )
    } else if missing > 0 || states["partial"] > 0 || states["unavailable"] > 0 {
        (
            "partial",
            "some resource declarations were not measured or were omitted".to_owned(),
        )
    } else {
        ("complete", String::new())
    };
    let (count, measured): (i64, i64) = con.query_row(
        "SELECT COUNT(*),COALESCE(SUM(capture_state='measured'),0) FROM resource_refs",
        [],
        |r| Ok((r.get(0)?, r.get(1)?)),
    )?;
    let (bodies_state, bodies_reason) = if !fetch_enabled {
        ("unavailable", "resources.fetch disabled".to_owned())
    } else if refs_state == "complete" && count == measured {
        ("complete", String::new())
    } else {
        let mut stmt = con.prepare(
            "SELECT DISTINCT capture_state FROM resource_refs WHERE capture_state!='measured' ORDER BY capture_state",
        )?;
        let reasons: Vec<String> = stmt
            .query_map([], |r| r.get(0))?
            .collect::<Result<_, _>>()?;
        let joined = reasons.join(", ");
        (
            if measured > 0 {
                "partial"
            } else {
                "unavailable"
            },
            if joined.is_empty() {
                "resource inventory is incomplete".to_owned()
            } else {
                joined
            },
        )
    };
    let entry = |state: &str, reason: String| {
        IndexMap::from([
            ("state".to_owned(), state.to_owned()),
            ("reason".to_owned(), reason),
        ])
    };
    let mut out = IndexMap::new();
    out.insert("resource_refs".to_owned(), entry(refs_state, refs_reason));
    out.insert(
        "resource_bodies".to_owned(),
        entry(bodies_state, bodies_reason),
    );
    Ok(out)
}

/// `responses` columns read by `validate_resources`: url, purpose, body_state, body_reason, status.
type StoredResponse = (i64, String, String, Option<String>, Option<i64>);

/// Build a `StoreError::Invalid` from a message.
fn invalid(message: &str) -> StoreError {
    StoreError::Invalid(message.to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::schema_sql::SCAN_V1_SQL;

    /// One URL, one static document and one page row, as a test fixture.
    fn fixture() -> Connection {
        let con = Connection::open_in_memory().expect("in-memory db");
        con.execute_batch(SCAN_V1_SQL).expect("scan.v1 DDL");
        con.execute(
            "INSERT INTO urls(url_id,url) VALUES(1,'https://example.test/page')",
            [],
        )
        .expect("url");
        con.execute(
            "INSERT INTO documents VALUES(1,1,'static',NULL,NULL,'2026-01-01T00:00:00Z','scan_decoder.v1','not_applicable','unknown','not_applicable','unavailable','unavailable','not_fetched','{}')",
            [],
        )
        .expect("document");
        let columns: Vec<(String, String)> = {
            let mut stmt = con.prepare("PRAGMA table_info(pages)").expect("pragma");
            stmt.query_map([], |r| Ok((r.get(1)?, r.get(2)?)))
                .expect("rows")
                .collect::<Result<_, _>>()
                .expect("collect")
        };
        let mut names = Vec::new();
        let mut values: Vec<rusqlite::types::Value> = Vec::new();
        for (name, kind) in columns {
            let value = match name.as_str() {
                "url_id" => rusqlite::types::Value::Integer(1),
                "page_ordinal" | "document_id" => {
                    rusqlite::types::Value::Integer(if name == "document_id" { 1 } else { 0 })
                }
                "content_type" => rusqlite::types::Value::Text("text/html".into()),
                "representation" => rusqlite::types::Value::Text("static".into()),
                "redirect_chain_json" => rusqlite::types::Value::Text("[]".into()),
                _ => match kind.as_str() {
                    "TEXT" => rusqlite::types::Value::Text(String::new()),
                    "INTEGER" => rusqlite::types::Value::Integer(0),
                    "REAL" => rusqlite::types::Value::Real(0.0),
                    _ => rusqlite::types::Value::Null,
                },
            };
            names.push(name);
            values.push(value);
        }
        let placeholders = vec!["?"; names.len()].join(",");
        con.execute(
            &format!(
                "INSERT INTO pages({}) VALUES({placeholders})",
                names.join(",")
            ),
            rusqlite::params_from_iter(values.iter()),
        )
        .expect("page");
        con
    }

    /// Build one `{"kind", "url", "raw_url"}` declaration.
    fn decl(kind: &str, url: &str, raw: &str) -> IndexMap<String, Value> {
        IndexMap::from([
            ("kind".to_owned(), Value::from(kind)),
            ("url".to_owned(), Value::from(url)),
            ("raw_url".to_owned(), Value::from(raw)),
        ])
    }

    // Every occurrence is kept; one representation is replaced at a time.
    #[test]
    fn declarations_preserve_every_occurrence_and_replace_one_representation() {
        let con = fixture();
        let declarations = vec![
            decl("script", "https://example.test/a.js", "/a.js"),
            decl("script", "https://example.test/a.js", "/a.js"),
            decl("stylesheet", "https://example.test/a.css", "/a.css"),
        ];
        let result = put_declarations(&con, 1, 1, "static", &declarations, false, "complete", 0)
            .expect("put");
        assert_eq!(result["script"], 2);
        assert_eq!(result["stylesheet"], 1);
        assert_eq!(result["omitted"], 0);
        let rows: Vec<(String, i64)> = con
            .prepare("SELECT kind,ordinal FROM resource_refs ORDER BY kind,ordinal")
            .expect("prepare")
            .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))
            .expect("query")
            .collect::<Result<_, _>>()
            .expect("rows");
        assert_eq!(
            rows,
            vec![
                ("script".to_owned(), 0),
                ("script".to_owned(), 1),
                ("stylesheet".to_owned(), 0)
            ]
        );
        put_declarations(&con, 1, 1, "static", &[], false, "complete", 0).expect("replace");
        let count: i64 = con
            .query_row("SELECT COUNT(*) FROM resource_refs", [], |r| r.get(0))
            .expect("count");
        assert_eq!(count, 0);
        let caps = resource_capabilities(&con, false).expect("caps");
        assert_eq!(caps["resource_refs"]["state"], "complete");
    }

    // Fetch state and response must agree.
    #[test]
    fn validation_rejects_fetch_state_response_mismatch() {
        let con = fixture();
        put_declarations(
            &con,
            1,
            1,
            "static",
            &[decl("script", "https://example.test/a.js", "/a.js")],
            false,
            "complete",
            0,
        )
        .expect("put");
        con.execute_batch("PRAGMA foreign_keys=OFF; UPDATE resource_refs SET response_id=1;")
            .expect("tamper");
        let err = validate_resources(&con).expect_err("must reject");
        assert!(err.to_string().contains("unfetched"), "{err}");
    }

    // Absent and partial inventories are distinct capabilities.
    #[test]
    fn capabilities_distinguish_absent_and_partial_inventory() {
        let con = fixture();
        let absent = resource_capabilities(&con, false).expect("caps");
        assert_eq!(absent["resource_refs"]["state"], "unavailable");
        put_declarations(&con, 1, 1, "static", &[], true, "partial", 2).expect("put");
        let caps = resource_capabilities(&con, true).expect("caps");
        assert_eq!(caps["resource_refs"]["state"], "partial");
        assert_eq!(caps["resource_bodies"]["state"], "unavailable");
    }

    // A fetch-enabled, measured empty inventory has complete body coverage.
    #[test]
    fn fetch_enabled_measured_empty_inventory_has_complete_body_coverage() {
        let con = fixture();
        put_declarations(&con, 1, 1, "static", &[], true, "complete", 0).expect("put");
        let caps = resource_capabilities(&con, true).expect("caps");
        assert_eq!(caps["resource_refs"]["state"], "complete");
        assert_eq!(caps["resource_bodies"]["state"], "complete");
    }

    #[test]
    fn media_matches_admits_only_declared_text_types() {
        assert!(media_matches("stylesheet", "text/css; charset=utf-8"));
        assert!(media_matches("script", "Application/JavaScript"));
        assert!(!media_matches("stylesheet", "text/javascript"));
        assert!(!media_matches("script", "text/plain"));
        assert!(!media_matches("image", "text/css"));
    }
}
