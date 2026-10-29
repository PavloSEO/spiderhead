//! `scan_evidence`: read one section of the evidence retained in a scan.

use std::path::Path;

use rusqlite::params_from_iter;
use serde_json::{json, Map, Value};

use spiderhead_store::scan_artifact::open_scan;
use spiderhead_store::{
    discovery_ledger, events, evidence_contract, rendered_routes, resource_graph,
};

use crate::error::ToolError;

use super::{failed, need_str, opt, opt_str};

const SECTIONS: [&str; 9] = [
    "capabilities",
    "corpus",
    "structured",
    "routes",
    "resources",
    "timeline",
    "relations",
    "browser",
    "extraction",
];

/// Context-item kinds behind the sections that read `context_items`.
fn context_kinds(section: &str) -> Option<&'static [&'static str]> {
    match section {
        "corpus" => Some(&["content_evidence"]),
        "structured" => Some(&["structured_evidence", "language_evidence"]),
        "browser" => Some(&["browser_artifacts"]),
        "extraction" => Some(&["extraction_rule_evidence"]),
        _ => None,
    }
}

/// `scan_evidence`: one evidence section, bounded by `limit`/`offset`.
pub fn scan_evidence(args: &Value) -> Result<Value, ToolError> {
    let args = super::kwargs(args);
    const ACCEPTED: [&str; 4] = ["input_path", "section", "limit", "offset"];
    let input = need_str(&args, "input_path", "scan-evidence", &ACCEPTED)?;
    let limit = opt(&args, "limit").map_or(Some(1000), Value::as_i64);
    let offset = opt(&args, "offset").map_or(Some(0), Value::as_i64);
    let bad_page = ToolError::Failed("limit must be 1..10000 and offset nonnegative".into());
    let (Some(limit), Some(offset)) = (limit, offset) else {
        return Err(bad_page);
    };
    if !(1..=10_000).contains(&limit) || offset < 0 {
        return Err(bad_page);
    }
    let section = opt_str(&args, "section").unwrap_or("capabilities");
    if !SECTIONS.contains(&section) {
        return Err(ToolError::Failed("unknown saved evidence section".into()));
    }
    let con = open_scan(Path::new(input), false, None).map_err(failed)?;
    let evidence = match section {
        "relations" => Value::Object(discovery_ledger::read(&con, limit, offset).map_err(failed)?),
        "resources" => Value::Object(
            resource_graph::read(&con, limit, offset)
                .map_err(failed)?
                .into_iter()
                .collect(),
        ),
        "timeline" => Value::Object(events::timeline(&con, limit).map_err(failed)?),
        "routes" => Value::Object(rendered_routes::read(&con).map_err(failed)?),
        "capabilities" => capabilities(&con)?,
        _ => context_section(&con, section, limit, offset)?,
    };
    Ok(json!({"ok": true, "section": section, "evidence": evidence}))
}

fn sql_error(error: rusqlite::Error) -> ToolError {
    ToolError::Failed(error.to_string())
}

fn context_section(
    con: &rusqlite::Connection,
    section: &str,
    limit: i64,
    offset: i64,
) -> Result<Value, ToolError> {
    let kinds = context_kinds(section).unwrap_or(&[]);
    let marks = vec!["?"; kinds.len()].join(",");
    let total: i64 = con
        .query_row(
            &format!("SELECT COUNT(*) FROM context_items WHERE kind IN ({marks})"),
            params_from_iter(kinds.iter()),
            |r| r.get(0),
        )
        .map_err(sql_error)?;
    let mut statement = con
        .prepare(&format!(
            "SELECT kind,item_key,payload_json,completeness,reason FROM context_items \
             WHERE kind IN ({marks}) ORDER BY kind,item_key LIMIT ? OFFSET ?"
        ))
        .map_err(sql_error)?;
    let mut bound: Vec<rusqlite::types::Value> = kinds
        .iter()
        .map(|k| rusqlite::types::Value::Text((*k).to_owned()))
        .collect();
    bound.push(limit.into());
    bound.push(offset.into());
    let saved = statement
        .query_map(params_from_iter(bound), |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, String>(2)?,
                r.get::<_, Option<String>>(3)?,
                r.get::<_, Option<String>>(4)?,
            ))
        })
        .map_err(sql_error)?
        .collect::<Result<Vec<_>, _>>()
        .map_err(sql_error)?;
    let mut items = Vec::new();
    for (kind, key, payload, state, reason) in &saved {
        let observation: Value =
            serde_json::from_str(payload).map_err(|e| ToolError::Failed(e.to_string()))?;
        items.push(json!({
            "kind": kind,
            "key": key,
            "state": state,
            "reason": reason,
            "observation": observation,
        }));
    }
    Ok(json!({
        "state": if total > 0 { "recorded" } else { "unavailable" },
        "reason": if total > 0 { "" } else { "requested evidence was not retained" },
        "total": total,
        "offset": offset,
        "limit": limit,
        "has_more": offset + (saved.len() as i64) < total,
        "items": items,
    }))
}

fn capabilities(con: &rusqlite::Connection) -> Result<Value, ToolError> {
    let document: Option<String> = con
        .query_row(
            "SELECT document_json FROM audit WHERE singleton=1",
            [],
            |r| r.get(0),
        )
        .ok();
    let Some(document) = document else {
        return Ok(json!({
            "state": "unavailable",
            "reason": "no saved audit capability projection",
        }));
    };
    let audit: Map<String, Value> =
        serde_json::from_str(&document).map_err(|e| ToolError::Failed(e.to_string()))?;
    let identity: String = con
        .query_row("SELECT scan_uuid FROM scan WHERE singleton=1", [], |r| {
            r.get(0)
        })
        .map_err(sql_error)?;
    let attached =
        evidence_contract::attach_contract(&audit, Some(&identity), Some(con)).map_err(failed)?;
    Ok(attached["summary"]["evidence_contract"].clone())
}

/// `scan_extract`: apply bounded declarative rules to retained bodies; no fetch, no mutation.
pub fn scan_extract(args: &Value) -> Result<Value, ToolError> {
    use spiderhead_parse::extraction_rules::{evaluate, validate_rules};
    use spiderhead_parse::parser::parse_html;
    use spiderhead_parse::types::ParseOptions;
    use spiderhead_store::bodies::read_document;

    const ACCEPTED: [&str; 5] = ["input_path", "rules", "url", "representation", "limit"];
    const OUTPUT_LIMIT: usize = 8 * 1024 * 1024;
    let args = super::kwargs(args);
    let input = need_str(&args, "input_path", "scan-extract", &ACCEPTED)?;
    let Some(rules) = opt(&args, "rules") else {
        return Err(super::missing_option("scan-extract", "rules", &ACCEPTED));
    };
    let representation = opt_str(&args, "representation").unwrap_or("static");
    let limit = opt(&args, "limit").map_or(Some(100), Value::as_i64);
    let limit = match limit {
        Some(n)
            if (1..=1000).contains(&n)
                && ["static", "rendered", "legacy_fragment"].contains(&representation) =>
        {
            n
        }
        _ => {
            return Err(ToolError::Failed(
                "invalid representation or page limit".into(),
            ))
        }
    };
    validate_rules(rules).map_err(|e| ToolError::Failed(e.to_string()))?;
    let con = open_scan(Path::new(input), false, None).map_err(failed)?;
    let mut sql = "SELECT d.document_id,u.url FROM documents d JOIN urls u USING(url_id) \
                   WHERE d.representation=?"
        .to_owned();
    let mut bound: Vec<rusqlite::types::Value> =
        vec![rusqlite::types::Value::Text(representation.to_owned())];
    if let Some(url) = opt_str(&args, "url") {
        sql.push_str(" AND u.url=?");
        bound.push(rusqlite::types::Value::Text(url.to_owned()));
    }
    sql.push_str(" ORDER BY document_id LIMIT ?");
    bound.push((limit + 1).into());
    let mut statement = con.prepare(&sql).map_err(sql_error)?;
    let rows: Vec<(i64, String)> = statement
        .query_map(params_from_iter(bound), |r| Ok((r.get(0)?, r.get(1)?)))
        .map_err(sql_error)?
        .collect::<Result<_, _>>()
        .map_err(sql_error)?;
    let mut items = Vec::new();
    let (mut used, mut output_limited) = (0usize, false);
    for (document_id, url) in rows.iter().take(limit as usize) {
        let (html, reason) = match read_document(&con, *document_id, 8 * 1024 * 1024) {
            Ok(text) => (Some(text), String::new()),
            Err(spiderhead_store::error::StoreError::ScanError(e)) => (None, e.message),
            Err(other) => return Err(failed(other)),
        };
        let parsed = match &html {
            Some(text) => Some(
                serde_json::to_value(
                    parse_html(text, url, &ParseOptions::default())
                        .map_err(|e| ToolError::Failed(e.to_string()))?,
                )
                .map_err(|e| ToolError::Failed(e.to_string()))?,
            ),
            None => None,
        };
        let result = evaluate(html.as_deref(), parsed.as_ref(), rules, representation)
            .map_err(|e| ToolError::Failed(e.to_string()))?;
        let mut result =
            serde_json::to_value(result).map_err(|e| ToolError::Failed(e.to_string()))?;
        if !reason.is_empty() {
            result["reason"] = json!(reason);
        }
        let item = json!({
            "url": url,
            "document_id": document_id,
            "representation": representation,
            "result": result,
        });
        let size = spiderhead_core::common::python_json(&item).len();
        if used + size > OUTPUT_LIMIT {
            output_limited = true;
            break;
        }
        used += size;
        items.push(item);
    }
    Ok(json!({
        "ok": true,
        "items": items,
        "truncated": rows.len() as i64 > limit || output_limited,
        "output_limit_bytes": OUTPUT_LIMIT,
        "scope": "retained complete bodies only; no network or artifact mutation",
    }))
}
