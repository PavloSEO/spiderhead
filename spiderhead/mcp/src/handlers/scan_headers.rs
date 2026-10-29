//! `headers_analyze`: retained response headers and transport phase timings of
//! a scan, judged for security headers; never issues a request.

use std::path::Path;

use rusqlite::{types::ValueRef, Connection};
use serde_json::{json, Map, Value};

use spiderhead_checks::audit::rules::stored::evaluate_page;
use spiderhead_store::scan_artifact::open_scan_mode;

use crate::error::ToolError;

use super::{need_str, opt_float, opt_int, opt_str};

const FORMAT: &str = "spiderhead.headers-analysis.v1";
const PHASES: [&str; 5] = ["dns", "connect", "tls", "ttfb", "download"];
const ACCEPTED: [&str; 6] = [
    "input_path",
    "url",
    "offset",
    "limit",
    "max_bytes",
    "timeout_seconds",
];

/// Rows of `sql` as column-name maps (blobs never occur in scan tables).
fn rows(
    con: &Connection,
    sql: &str,
    params: &[&dyn rusqlite::ToSql],
) -> Result<Vec<Map<String, Value>>, rusqlite::Error> {
    let mut statement = con.prepare(sql)?;
    let names: Vec<String> = statement
        .column_names()
        .iter()
        .map(|n| (*n).to_owned())
        .collect();
    let mut cursor = statement.query(params)?;
    let mut out = Vec::new();
    while let Some(row) = cursor.next()? {
        let mut map = Map::new();
        for (index, name) in names.iter().enumerate() {
            let value = match row.get_ref(index)? {
                ValueRef::Integer(n) => json!(n),
                ValueRef::Real(f) => json!(f),
                ValueRef::Text(t) => json!(String::from_utf8_lossy(t)),
                _ => Value::Null,
            };
            map.insert(name.clone(), value);
        }
        out.push(map);
    }
    Ok(out)
}

fn truthy(value: &Value) -> bool {
    match value {
        Value::Null => false,
        Value::Bool(b) => *b,
        Value::Number(n) => n.as_f64().is_some_and(|f| f != 0.0),
        Value::String(s) => !s.is_empty(),
        _ => true,
    }
}

fn invalid(message: &str) -> Value {
    json!({
        "ok": false,
        "state": "invalid",
        "reason_code": "invalid_headers_analysis",
        "error": message,
    })
}

/// `headers_analyze`: paged header and transport-phase analysis of one scan.
pub fn headers_analyze(args: &Value) -> Result<Value, ToolError> {
    let args = super::kwargs(args);
    let input = need_str(&args, "input_path", "headers-analyze", &ACCEPTED)?;
    if input.is_empty() {
        return Ok(invalid("input_path is required"));
    }
    let url = opt_str(&args, "url");
    if url.is_some_and(|u| u.is_empty() || u.chars().count() > 8192) {
        return Ok(invalid(
            "url must be an exact retained URL of at most 8192 characters",
        ));
    }
    let offset = opt_int(&args, "offset").unwrap_or(0);
    if !(0..=(1i64 << 40)).contains(&offset) {
        return Ok(invalid("offset must be 0..2^40"));
    }
    let limit = opt_int(&args, "limit").unwrap_or(100);
    if !(1..=200).contains(&limit) {
        return Ok(invalid("limit must be 1..200"));
    }
    let max_bytes = opt_int(&args, "max_bytes").unwrap_or(1_048_576);
    if !(4096..=8 * 1024 * 1024).contains(&max_bytes) {
        return Ok(invalid("max_bytes must be 4096..8388608"));
    }
    let timeout = opt_float(&args, "timeout_seconds").unwrap_or(15.0);
    if !(0.1..=30.0).contains(&timeout) {
        return Ok(invalid("timeout_seconds must be 0.1..30"));
    }
    let (con, validation) = match open_scan_mode(Path::new(input), false, None, true) {
        Ok(opened) => opened,
        Err(error) => {
            return Ok(json!({
                "ok": false,
                "state": "unavailable",
                "format": FORMAT,
                "reason_code": "scan_not_available",
                "error": super::plain(&error),
            }))
        }
    };
    match analyze(&con, url, offset, limit, max_bytes, validation) {
        Ok(answer) => Ok(answer),
        Err(error) => Ok(json!({
            "ok": false,
            "state": "unavailable",
            "format": FORMAT,
            "reason_code": "scan_read_failed",
            "error": error.to_string(),
            "validation": validation,
        })),
    }
}

fn analyze(
    con: &Connection,
    url: Option<&str>,
    offset: i64,
    limit: i64,
    max_bytes: i64,
    validation: &str,
) -> Result<Value, rusqlite::Error> {
    let source_rows = rows(
        con,
        "SELECT scan_uuid,format_version,source_kind,evidence_revision,lifecycle,\
         finish_reason,crawl_partial,corpus_partial FROM scan WHERE singleton=1",
        &[],
    )?;
    let Some(source) = source_rows.into_iter().next() else {
        return Err(rusqlite::Error::QueryReturnedNoRows);
    };
    if source["source_kind"] != json!("native") {
        return Ok(json!({
            "ok": true,
            "state": "unavailable",
            "format": FORMAT,
            "reason": "saved source does not retain native response headers",
            "source": source,
            "validation": validation,
        }));
    }
    let tables: Vec<String> = rows(
        con,
        "SELECT name FROM main.sqlite_master WHERE type='table'",
        &[],
    )?
    .iter()
    .filter_map(|r| r["name"].as_str().map(str::to_owned))
    .collect();
    let has_meta = tables.iter().any(|t| t == "response_transport_meta");
    let columns: Vec<String> = if has_meta {
        rows(con, "PRAGMA table_info(response_transport_meta)", &[])?
            .iter()
            .filter_map(|r| r["name"].as_str().map(str::to_owned))
            .collect()
    } else {
        Vec::new()
    };
    let has_phase_data = PHASES.iter().all(|phase| {
        ["seconds", "state", "reason"]
            .iter()
            .all(|suffix| columns.contains(&format!("{phase}_{suffix}")))
    });
    let phase_select = if has_phase_data {
        let list: Vec<String> = PHASES
            .iter()
            .flat_map(|p| {
                ["seconds", "state", "reason"]
                    .iter()
                    .map(move |s| format!("t.{p}_{s} AS {p}_{s}"))
            })
            .collect();
        format!(",t.protocol,t.protocol_state,{}", list.join(","))
    } else if has_meta {
        ",t.protocol,t.protocol_state".to_owned()
    } else {
        ",NULL AS protocol,'unavailable' AS protocol_state".to_owned()
    };
    let phase_join = if has_meta {
        " LEFT JOIN response_transport_meta t ON t.response_id=r.response_id"
    } else {
        ""
    };
    let filter = if url.is_some() { " WHERE u.url=?" } else { "" };
    let url_param: Vec<&dyn rusqlite::ToSql> =
        url.iter().map(|u| u as &dyn rusqlite::ToSql).collect();
    let total = rows(
        con,
        &format!("SELECT COUNT(*) AS n FROM pages p JOIN urls u USING(url_id){filter}"),
        &url_param,
    )?[0]["n"]
        .as_i64()
        .unwrap_or(0);
    if let (Some(url), 0) = (url, total) {
        return Ok(json!({
            "ok": true,
            "state": "not_found",
            "format": FORMAT,
            "source": source,
            "url": url,
            "total": 0,
            "items": [],
            "validation": validation,
        }));
    }
    let mut page_params = url_param.clone();
    page_params.push(&limit);
    page_params.push(&offset);
    let page_rows = rows(
        con,
        &format!(
            "SELECT p.url_id,p.page_ordinal,p.document_id,p.status_code,p.content_type,u.url,\
             r.response_id,r.response_time,r.effective_status_code,r.effective_headers_redacted_json\
             {phase_select} FROM pages p JOIN urls u USING(url_id) LEFT JOIN documents d ON d.document_id=p.document_id \
             LEFT JOIN responses r ON r.response_id=COALESCE(d.source_response_id,(\
             SELECT MAX(r2.response_id) FROM responses r2 WHERE r2.request_url_id=p.url_id \
             AND r2.purpose='page')){phase_join}{filter} ORDER BY p.page_ordinal LIMIT ? OFFSET ?"
        ),
        &page_params,
    )?;
    let mut items: Vec<Value> = Vec::new();
    let mut output_limited = false;
    let mut findings_by_check = Map::new();
    let (mut measured, mut unavailable, mut not_applicable) = (0i64, 0i64, 0i64);
    let mut transport_counts: Vec<Map<String, Value>> = vec![Map::new(); PHASES.len()];
    let mut used = 0i64;
    for row in &page_rows {
        let status = if truthy(&row["effective_status_code"]) {
            &row["effective_status_code"]
        } else {
            &row["status_code"]
        };
        let mut analysis = evaluate_page(
            row["url"].as_str().unwrap_or_default(),
            status.as_i64(),
            row["content_type"].as_str(),
            row["effective_headers_redacted_json"].as_str(),
        );
        if row["response_id"].is_null() && analysis["state"] == json!("measured") {
            analysis = json!({
                "state": "unavailable",
                "reason": "no retained response header record",
                "headers": null,
                "findings": [],
            });
        }
        match analysis["state"].as_str() {
            Some("measured") => measured += 1,
            Some("not_applicable") => not_applicable += 1,
            _ => unavailable += 1,
        }
        for finding in analysis["findings"].as_array().into_iter().flatten() {
            let check = finding["check"].as_str().unwrap_or_default();
            let next = findings_by_check
                .get(check)
                .and_then(Value::as_i64)
                .unwrap_or(0)
                + 1;
            findings_by_check.insert(check.to_owned(), json!(next));
        }
        let mut phases = Map::new();
        for (index, phase) in PHASES.iter().enumerate() {
            let entry = if has_phase_data && !row["response_id"].is_null() {
                let state = &row[&format!("{phase}_state")];
                let reason = &row[&format!("{phase}_reason")];
                json!({
                    "seconds": row[&format!("{phase}_seconds")],
                    "state": if state == &json!("unknown") { json!("unavailable") } else { state.clone() },
                    "reason": if truthy(reason) { reason.clone() } else { json!("") },
                })
            } else {
                json!({
                    "seconds": null,
                    "state": "unavailable",
                    "reason": "transport phase was not retained",
                })
            };
            let key = entry["state"].as_str().unwrap_or("null").to_owned();
            let count = transport_counts[index]
                .get(&key)
                .and_then(Value::as_i64)
                .unwrap_or(0)
                + 1;
            transport_counts[index].insert(key, json!(count));
            phases.insert((*phase).to_owned(), entry);
        }
        let state_text = row["protocol_state"].as_str().filter(|s| !s.is_empty());
        let item = json!({
            "url": row["url"],
            "url_id": row["url_id"],
            "page_ordinal": row["page_ordinal"],
            "response_id": row["response_id"],
            "status_code": status,
            "total_seconds": row["response_time"],
            "http_version": row["protocol"],
            "http_version_state": state_text.unwrap_or("unavailable"),
            "headers_state": analysis["state"],
            "headers_reason": analysis["reason"],
            "headers": analysis["headers"],
            "findings": analysis["findings"],
            "transport": phases,
        });
        let size = spiderhead_core::common::python_json(&item).len() as i64;
        if used + size > max_bytes {
            output_limited = true;
            break;
        }
        used += size;
        items.push(item);
    }
    let returned = items.len() as i64;
    if output_limited && returned == 0 {
        return Ok(json!({
            "ok": true,
            "state": "limit_reached",
            "format": FORMAT,
            "reason": "one header record exceeds max_bytes",
            "source": source,
            "offset": offset,
            "limit": limit,
            "total": total,
            "items": [],
            "validation": validation,
        }));
    }
    let next_offset = offset + returned;
    let has_more = next_offset < total;
    let complete = source["lifecycle"] == json!("finished")
        && !truthy(&source["crawl_partial"])
        && !truthy(&source["corpus_partial"])
        && !output_limited
        && returned == limit.min((total - offset).max(0));
    let mut phase_states = Map::new();
    for (phase, counts) in PHASES.iter().zip(transport_counts) {
        phase_states.insert((*phase).to_owned(), Value::Object(counts));
    }
    Ok(json!({
        "ok": true,
        "state": if complete { "available" } else { "partial" },
        "format": FORMAT,
        "source": source,
        "offset": offset,
        "limit": limit,
        "total": total,
        "total_state": "exact",
        "returned": returned,
        "has_more": has_more,
        "next_offset": if has_more { json!(next_offset) } else { Value::Null },
        "bytes": used,
        "summary": {
            "headers_measured": measured,
            "headers_unavailable": unavailable,
            "not_applicable": not_applicable,
            "findings_by_check": findings_by_check,
            "transport_phase_states": phase_states,
        },
        "items": items,
        "validation": validation,
        "scope": "retained redacted response headers and transport metadata; no network request",
    }))
}
