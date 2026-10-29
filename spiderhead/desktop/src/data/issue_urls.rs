//! URLs affected by one audit check, for the issues screen's right pane.
//! The rows stream out of the audit.v2 companion (`/issues` plus the rollup
//! `/groups` members, `IMG_*` source pages included) and join `pages` for the
//! HTTP code — the same population `materialize_issues` builds for the table
//! filter, so row counts equal the filtered table.
//!
//! reference reference: `issue_selection.materialize` + the finding rows of
//! `audit.v2` (`desktop` shows `target_url`, evidence message and status).

use std::collections::BTreeMap;

use serde_json::{json, Value};
use spiderhead_store::audit_v2::AuditV2Reader;

use super::filters::QueryError;
use super::session::{issue_code, ScanSession};

/// Rows of one issues page at most (`limit` is clamped to this).
pub const MAX_LIMIT: i64 = 500;

/// One affected URL: address, retained HTTP code and the finding's evidence.
struct Hit {
    url: String,
    /// `issue.status_code` — what the check measured (not always the page code).
    finding_status: Option<i64>,
    /// Evidence text: the finding's `message`, once per URL.
    evidence: String,
    /// Findings of this check pointing at the URL.
    findings: i64,
}

/// Affected URLs of `check`, sorted by `page_ordinal`, paged like the table.
///
/// `state` is `available` with real rows, `unmeasured` when the check was not
/// measured in the saved audit, `unavailable` when the companion is missing.
pub fn urls_for_check(
    session: &ScanSession,
    check: &str,
    offset: i64,
    limit: i64,
) -> Result<Value, QueryError> {
    let limit = limit.clamp(1, MAX_LIMIT);
    let offset = offset.max(0);
    if !session.audit_available() {
        return Ok(json!({"state": "unavailable", "reason": "no_audit"}));
    }
    let check_code = issue_code(check);
    let reader = AuditV2Reader::new(&session.path, false, true, Some(5.0))
        .map_err(|e| QueryError::unavailable("issue_index_missing", e.to_string()))?;
    // "Measured" follows `materialize_issues`: a check listed silent/disabled
    // in the saved coverage counts as measured (it simply produced no rows);
    // a check absent from every inventory is unmeasured.
    let header = &reader.header;
    let coverage = header.get("summary").and_then(|s| s.get("check_coverage"));
    let listed = |key: &str| -> bool {
        coverage
            .and_then(|c| c.get(key))
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .filter_map(Value::as_str)
            .any(|id| issue_code(id) == check_code)
    };
    let skipped_reason = header
        .get("run")
        .and_then(|r| r.get("checks_skipped"))
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .find(|row| issue_code(row.get("id").and_then(Value::as_str).unwrap_or("")) == check_code)
        .and_then(|row| row.get("reason").and_then(Value::as_str))
        .unwrap_or("")
        .to_string();
    let mut measured = listed("checks_silent_ids") || listed("checks_disabled_ids");
    let mut hits: BTreeMap<String, Hit> = BTreeMap::new();
    let mut group_ids: Vec<String> = Vec::new();
    let issues = reader
        .iter_collection("/issues")
        .map_err(|e| QueryError::unavailable("issue_index_missing", e.to_string()))?;
    let image_check = check_code.starts_with("img_");
    for issue in &issues {
        if issue_code(issue.get("check").and_then(Value::as_str).unwrap_or("")) != check_code {
            continue;
        }
        measured = true;
        let message = issue
            .get("message")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string();
        let finding_status = issue.get("status_code").and_then(Value::as_i64);
        let mut urls = Vec::new();
        if let Some(url) = issue.get("target_url").and_then(Value::as_str) {
            if !url.is_empty() {
                urls.push(url.to_string());
            }
        }
        if image_check {
            if let Some(url) = issue
                .get("details")
                .and_then(|d| d.get("source_page"))
                .and_then(Value::as_str)
            {
                if !url.is_empty() {
                    urls.push(url.to_string());
                }
            }
        }
        if let Some(group) = issue.get("group_id").and_then(Value::as_str) {
            if !group.is_empty() && !group_ids.iter().any(|g| g == group) {
                group_ids.push(group.to_string());
            }
        }
        for url in urls {
            let hit = hits.entry(url.clone()).or_insert_with(|| Hit {
                url,
                finding_status: None,
                evidence: String::new(),
                findings: 0,
            });
            hit.findings += 1;
            if hit.finding_status.is_none() {
                hit.finding_status = finding_status;
            }
            if hit.evidence.is_empty() {
                hit.evidence = message.clone();
            }
        }
    }
    // Rollup groups: member URLs of this check's groups are affected too.
    if !group_ids.is_empty() {
        let groups = reader
            .iter_collection("/groups")
            .map_err(|e| QueryError::unavailable("issue_index_missing", e.to_string()))?;
        for group in &groups {
            let id = group.get("group_id").and_then(Value::as_str).unwrap_or("");
            if !group_ids.iter().any(|g| g == id) {
                continue;
            }
            for url in group
                .get("urls")
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
                .filter_map(Value::as_str)
            {
                hits.entry(url.to_string()).or_insert_with(|| Hit {
                    url: url.to_string(),
                    finding_status: None,
                    evidence: String::new(),
                    findings: 0,
                });
            }
        }
    }
    if !measured && hits.is_empty() {
        return Ok(json!({
            "state": "unmeasured",
            "check": check,
            "reason": skipped_reason,
            "rows": [],
            "total": 0,
        }));
    }
    // URL → (page_ordinal, status_code) of the scan population.
    let mut rows: Vec<(i64, Value)> = Vec::with_capacity(hits.len());
    {
        let mut stmt = session
            .con
            .prepare(
                "SELECT u.url, p.page_ordinal, p.status_code FROM urls u \
                 LEFT JOIN pages p ON p.url_id=u.url_id",
            )
            .map_err(|e| QueryError::unavailable("cannot_read", e.to_string()))?;
        let page = stmt
            .query_map([], |r| {
                Ok((
                    r.get::<_, String>(0)?,
                    r.get::<_, Option<i64>>(1)?,
                    r.get::<_, Option<i64>>(2)?,
                ))
            })
            .map_err(|e| QueryError::unavailable("cannot_read", e.to_string()))?;
        let mut ordinals: BTreeMap<String, (i64, Option<i64>)> = BTreeMap::new();
        for row in page {
            let (url, ordinal, status) =
                row.map_err(|e| QueryError::unavailable("cannot_read", e.to_string()))?;
            ordinals.insert(url, (ordinal.unwrap_or(i64::MAX), status));
        }
        for hit in hits.into_values() {
            let (ordinal, page_status) = ordinals
                .get(&hit.url)
                .cloned()
                .unwrap_or((i64::MAX, hit.finding_status));
            rows.push((
                ordinal,
                json!({
                    "url": hit.url,
                    "status_code": page_status.or(hit.finding_status),
                    "evidence": hit.evidence,
                    "findings": hit.findings,
                }),
            ));
        }
    }
    rows.sort_by_key(|(ordinal, _)| *ordinal);
    let total = rows.len() as i64;
    let page: Vec<Value> = rows
        .into_iter()
        .skip(offset as usize)
        .take(limit as usize)
        .map(|(_, v)| v)
        .collect();
    Ok(json!({
        "state": "available",
        "check": check,
        "rows": page,
        "total": total,
        "offset": offset,
        "has_more": offset + limit < total,
        "next_offset": if offset + limit < total { offset + limit } else { total },
    }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    fn session() -> ScanSession {
        let path =
            Path::new(env!("CARGO_MANIFEST_DIR")).join("../../oracle/raw-scans/basic/scan.sqlite");
        ScanSession::open(&path).expect("basic scan opens")
    }

    #[test]
    fn http_url_lists_its_nine_pages() {
        let s = session();
        let page = urls_for_check(&s, "HTTP_URL", 0, 100).unwrap();
        assert_eq!(page["state"], "available");
        // Oracle: HTTP_URL fires on 9 pages (see app.rs dashboard numbers).
        assert_eq!(page["total"], 9, "{page}");
        assert_eq!(page["rows"].as_array().unwrap().len(), 9);
        assert_eq!(
            page["rows"][0]["url"],
            json!("http://127.0.0.1:18701/"),
            "{page}"
        );
        assert_eq!(page["rows"][0]["status_code"], 200);
    }

    #[test]
    fn broken_link_reports_the_target_and_evidence() {
        let s = session();
        let page = urls_for_check(&s, "BROKEN_INTERNAL_LINK", 0, 50).unwrap();
        assert_eq!(page["total"], 1);
        let row = &page["rows"][0];
        assert_eq!(row["url"], "http://127.0.0.1:18701/missing-page.html");
        assert_eq!(row["status_code"], 404);
        assert!(row["evidence"].as_str().unwrap().contains("4xx"), "{row}");
    }

    #[test]
    fn duplicate_groups_expand_to_member_urls() {
        let s = session();
        let page = urls_for_check(&s, "TITLE_DUPLICATE", 0, 50).unwrap();
        // dup-a/dup-b share a title group → both member URLs listed.
        let urls: Vec<&str> = page["rows"]
            .as_array()
            .unwrap()
            .iter()
            .filter_map(|r| r["url"].as_str())
            .collect();
        assert!(urls.iter().any(|u| u.contains("dup-a")), "{urls:?}");
        assert!(urls.iter().any(|u| u.contains("dup-b")), "{urls:?}");
    }

    #[test]
    fn unmeasured_and_paging_states() {
        let s = session();
        let v = urls_for_check(&s, "NO_SUCH_CHECK", 0, 50).unwrap();
        assert_eq!(v["state"], "unmeasured");
        let page = urls_for_check(&s, "HTTP_URL", 7, 5).unwrap();
        assert_eq!(page["rows"].as_array().unwrap().len(), 2);
        assert_eq!(page["has_more"], false);
    }
}
