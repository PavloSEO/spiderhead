//! Streamed offline position derivation with a transactional document checkpoint.

use rusqlite::{params, Connection, OptionalExtension};
use serde_json::{json, Map, Value};

use crate::error::StoreError;
use crate::link_context::{derive, row_map};

/// Checkpoint payload schema.
const CHECKPOINT_SCHEMA: &str = "links.position.v1";

fn invalid(message: &str) -> StoreError {
    StoreError::Invalid(message.to_string())
}

/// `classified` when a position is recorded, otherwise `not_classified`.
pub fn position_status(position: Option<&str>) -> String {
    match position {
        Some(p) if !p.is_empty() => "classified".into(),
        _ => "not_classified".into(),
    }
}

/// Bind resumable progress to the current derived scan and retained document.
pub fn validate_checkpoint(
    con: &Connection,
    item: &Map<String, Value>,
    payload: &Value,
) -> Result<(), StoreError> {
    const FIELDS: [&str; 6] = [
        "schema_version",
        "evidence_revision",
        "last_document_id",
        "documents",
        "filled",
        "unavailable_documents",
    ];
    let Some(obj) = payload.as_object() else {
        return Err(invalid("invalid link position checkpoint"));
    };
    let mut keys: Vec<&str> = obj.keys().map(String::as_str).collect();
    keys.sort_unstable();
    let mut expected = FIELDS.to_vec();
    expected.sort_unstable();
    let counts_ok = FIELDS[1..].iter().all(|key| {
        obj.get(*key)
            .and_then(Value::as_i64)
            .is_some_and(|v| v >= 0)
    });
    let unavailable = obj.get("unavailable_documents").and_then(Value::as_i64);
    let documents = obj.get("documents").and_then(Value::as_i64);
    let completeness = item.get("completeness").and_then(Value::as_str);
    if keys != expected
        || obj.get("schema_version").and_then(Value::as_str) != Some(CHECKPOINT_SCHEMA)
        || !counts_ok
        || item.get("item_key").and_then(Value::as_str) != Some("checkpoint")
        || !matches!(completeness, Some("complete") | Some("partial"))
        || unavailable > documents
    {
        return Err(invalid("invalid link position checkpoint"));
    }
    let (source_kind, revision): (String, i64) = con.query_row(
        "SELECT source_kind,evidence_revision FROM scan WHERE singleton=1",
        [],
        |r| Ok((r.get(0)?, r.get(1)?)),
    )?;
    let last_document = obj["last_document_id"].as_i64().unwrap_or(0);
    let document_exists: Option<i64> = con
        .query_row(
            "SELECT 1 FROM documents WHERE document_id=?1",
            params![last_document],
            |r| r.get(0),
        )
        .optional()?;
    if source_kind != "reanalysis"
        || obj["evidence_revision"].as_i64() != Some(revision)
        || document_exists.is_none()
    {
        return Err(invalid(
            "link position checkpoint belongs to another derived revision or document",
        ));
    }
    Ok(())
}

/// Count a bounded prefix, keeping blanks separate from observed content links.
/// Default `count_cap`: 100000.
pub fn position_summary(
    con: &Connection,
    count_cap: i64,
) -> Result<Map<String, Value>, StoreError> {
    let (count, classified): (i64, Option<i64>) = con.query_row(
        "SELECT COUNT(*),SUM(position!='') FROM (SELECT position FROM links LIMIT ?1)",
        params![count_cap + 1],
        |r| Ok((r.get(0)?, r.get(1)?)),
    )?;
    let classified = classified.unwrap_or(0);
    let capped = count > count_cap;
    let state = if capped || (0 < classified && classified < count) {
        "partial"
    } else if classified > 0 {
        "complete"
    } else {
        "not_classified"
    };
    let position_status = if state == "complete" {
        "classified"
    } else {
        state
    };
    let has_positions = if classified > 0 || !capped {
        json!(classified > 0)
    } else {
        Value::Null
    };
    let mut out = Map::new();
    out.insert("state".into(), json!(state));
    out.insert("position_status".into(), json!(position_status));
    out.insert("has_positions".into(), has_positions);
    out.insert("observed_links".into(), json!(count));
    out.insert("classified".into(), json!(classified));
    out.insert("not_classified".into(), json!(count - classified));
    out.insert("capped".into(), json!(capped));
    out.insert("count_cap".into(), json!(count_cap));
    out.insert("scope".into(), json!("retained hyperlink occurrences"));
    Ok(out)
}

/// Columns of every stored occurrence plus its source and destination URLs.
fn occurrence_sql() -> &'static str {
    "SELECT l.*,s.url AS source_url,d.url AS destination_url FROM links l \
     JOIN urls s ON s.url_id=l.source_url_id JOIN urls d ON d.url_id=l.destination_url_id \
     WHERE l.source_url_id=?1 AND l.evidence_representation=?2 AND l.source_document_id=?3 \
     ORDER BY l.ordinal,l.link_id LIMIT 20001"
}

/// Fill blanks in a private derived writer, one document/commit at a time.
///
/// Calling it again on the same writer continues after the durable checkpoint.
/// Existing classified occurrences, missing bodies and identity mismatches are never
/// replaced. The source capture and its recorded classification policy stay unchanged.
/// Defaults: no document limit, body cap 5 MiB, resume enabled.
pub fn backfill_positions(
    con: &Connection,
    max_documents: Option<i64>,
    max_body_bytes: i64,
    resume: bool,
) -> Result<Map<String, Value>, StoreError> {
    if let Some(max) = max_documents {
        if max < 1 {
            return Err(invalid("max_documents must be a positive integer"));
        }
    }
    if !(1..=8 * 1024 * 1024).contains(&max_body_bytes) {
        return Err(invalid("max_body_bytes must be 1..8388608"));
    }
    let (source_kind, revision): (String, i64) = con.query_row(
        "SELECT source_kind,evidence_revision FROM scan WHERE singleton=1",
        [],
        |r| Ok((r.get(0)?, r.get(1)?)),
    )?;
    if source_kind != "reanalysis" {
        return Err(invalid(
            "link position backfill requires a private derived scan",
        ));
    }
    let prior: Option<String> = con
        .query_row(
            "SELECT payload_json FROM context_items WHERE kind='links.position' AND item_key='checkpoint'",
            [],
            |r| r.get(0),
        )
        .optional()?;
    let mut progress: Map<String, Value> = match prior {
        Some(text) => match serde_json::from_str::<Value>(&text)? {
            Value::Object(map) => map,
            _ => Map::new(),
        },
        None => Map::new(),
    };
    let prior_revision = progress.get("evidence_revision").and_then(Value::as_i64);
    if !resume || !(prior_revision == Some(revision) || prior_revision == Some(revision - 1)) {
        progress = Map::new();
        progress.insert("schema_version".into(), json!(CHECKPOINT_SCHEMA));
        progress.insert("evidence_revision".into(), json!(revision));
        progress.insert("last_document_id".into(), json!(0));
        progress.insert("documents".into(), json!(0));
        progress.insert("filled".into(), json!(0));
        progress.insert("unavailable_documents".into(), json!(0));
    }
    progress.insert("evidence_revision".into(), json!(revision));
    let counter =
        |map: &Map<String, Value>, key: &str| map.get(key).and_then(Value::as_i64).unwrap_or(0);
    let mut count: i64 = 0;
    loop {
        let next: Option<(i64, i64, String)> = con
            .query_row(
                "SELECT document_id,url_id,representation FROM documents WHERE document_id>?1 \
                 ORDER BY document_id LIMIT 1",
                params![counter(&progress, "last_document_id")],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
            )
            .optional()?;
        let Some((document_id, url_id, representation)) = next else {
            break;
        };
        count += 1;
        let links = {
            let mut statement = con.prepare(occurrence_sql())?;
            let names: Vec<String> = statement
                .column_names()
                .iter()
                .map(|s| s.to_string())
                .collect();
            let names_ref: Vec<&str> = names.iter().map(String::as_str).collect();
            let rows = statement
                .query_map(params![url_id, representation, document_id], |row| {
                    row_map(row, &names_ref)
                })?;
            rows.collect::<Result<Vec<_>, _>>()?
        };
        let mut updates: Vec<(String, i64)> = Vec::new();
        let mut unavailable = false;
        if links.iter().any(|item| {
            item.get("position")
                .and_then(Value::as_str)
                .is_none_or(str::is_empty)
        }) {
            let (items, coverage) = derive(con, document_id, &links, max_body_bytes)?;
            unavailable = coverage.get("state").and_then(Value::as_str) == Some("unavailable");
            for item in &items {
                let measured = item.get("state").and_then(Value::as_str) == Some("measured");
                let stored = item
                    .get("stored_position")
                    .and_then(Value::as_str)
                    .is_some_and(|s| !s.is_empty());
                if measured && !stored {
                    let position = item
                        .get("placement")
                        .and_then(|p| p.get("position"))
                        .and_then(Value::as_str)
                        .unwrap_or("")
                        .to_string();
                    let link_id = item.get("link_id").and_then(Value::as_i64).unwrap_or(0);
                    updates.push((position, link_id));
                }
            }
        }
        let mut next_progress = progress.clone();
        next_progress.insert("last_document_id".into(), json!(document_id));
        next_progress.insert(
            "documents".into(),
            json!(counter(&progress, "documents") + 1),
        );
        next_progress.insert(
            "filled".into(),
            json!(counter(&progress, "filled") + updates.len() as i64),
        );
        next_progress.insert(
            "unavailable_documents".into(),
            json!(counter(&progress, "unavailable_documents") + i64::from(unavailable)),
        );
        let write = (|| -> Result<(), StoreError> {
            con.execute_batch("BEGIN IMMEDIATE")?;
            for (position, link_id) in &updates {
                con.execute(
                    "UPDATE links SET position=?1 WHERE link_id=?2 AND position=''",
                    params![position, link_id],
                )?;
            }
            con.execute(
                "INSERT OR REPLACE INTO context_items VALUES ('links.position','checkpoint',\
                 'scan_context.v1',?1,'partial','offline retained-document position backfill')",
                params![Value::Object(next_progress.clone()).to_string()],
            )?;
            con.execute_batch("COMMIT")?;
            Ok(())
        })();
        if let Err(error) = write {
            if !con.is_autocommit() {
                let _ = con.execute_batch("ROLLBACK");
            }
            return Err(error);
        }
        progress = next_progress;
        if max_documents.is_some_and(|max| count >= max) {
            break;
        }
    }
    let last = counter(&progress, "last_document_id");
    let has_more: bool = con
        .query_row(
            "SELECT 1 FROM documents WHERE document_id>?1 LIMIT 1",
            params![last],
            |r| r.get::<_, i64>(0),
        )
        .optional()?
        .is_some();
    if last != 0 {
        con.execute(
            "UPDATE context_items SET completeness=?1,payload_json=?2 \
             WHERE kind='links.position' AND item_key='checkpoint'",
            params![
                if has_more { "partial" } else { "complete" },
                Value::Object(progress.clone()).to_string()
            ],
        )?;
    }
    let mut out = Map::new();
    out.insert("step".into(), json!("links.position"));
    out.insert(
        "state".into(),
        json!(if has_more { "partial" } else { "complete" }),
    );
    out.insert("network".into(), json!("disabled"));
    out.insert("has_more".into(), json!(has_more));
    for (key, value) in progress {
        out.insert(key, value);
    }
    Ok(out)
}
