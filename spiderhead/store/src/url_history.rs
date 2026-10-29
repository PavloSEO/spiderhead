//! Per-URL state of one URL across every retained scan of a project, read-only.
//!
//! Each scan is opened with the light identity and schema check, and the URL is looked up by
//! its exact retained text through the unique index on `urls.url`. A scan that never committed
//! a page for the URL reports `state: absent`; an unreadable scan is listed in `errors` and
//! never mistaken for absence. Nothing is written, and the scans are never modified.

#![allow(clippy::too_many_arguments, clippy::type_complexity)]

use crate::crawl_records::spider::Scope;
use crate::error::StoreError;
use crate::history::catalog;
use crate::link_context::cell_value;
use crate::scan_artifact::open_scan_mode;
use crate::url_links::hostname_of;
use rusqlite::{params, Connection};
use serde_json::{json, Map, Value};
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;
use std::path::Path;

/// Format label of the result.
pub const FORMAT: &str = "spiderhead.scan-url-history.v1";
/// Scans returned when the caller names no limit.
pub const DEFAULT_LIMIT: i64 = 50;
/// Largest accepted limit.
pub const MAX_LIMIT: i64 = 500;
/// Longest accepted URL text.
pub const MAX_URL_LENGTH: i64 = 8192;

/// One indexed lookup per scan. Indexability follows the url-query rule: 2xx and no noindex in
/// meta robots or X-Robots-Tag. The document's body hash is the page's content identity.
pub const PAGE_SQL: &str = "SELECT p.url_id,p.status_code, COALESCE(p.status_code BETWEEN 200 AND 299 AND instr(lower(p.meta_robots||','||p.x_robots),'noindex')=0, 0) AS indexable, p.title,p.meta_description,p.h1,p.canonical,p.redirect_url,d.body_sha256,d.body_state,d.body_reason, p.size_bytes,p.response_time,p.word_count, (SELECT COUNT(*) FROM links l WHERE l.source_url_id=p.url_id) AS outlinks, (SELECT COUNT(*) FROM links l WHERE l.destination_url_id=u.url_id) AS inlinks FROM urls u JOIN pages p ON p.url_id=u.url_id LEFT JOIN documents d ON d.document_id=p.document_id WHERE u.url=?";

/// Fields compared between adjacent scans, in report order.
pub const CHANGE_FIELDS: &[&str] = &[
    "status_code",
    "title",
    "meta_description",
    "h1",
    "word_count",
    "inlinks",
    "outlinks",
    "canonical",
    "response_time_seconds",
    "indexability",
    "html_size_bytes",
    "external_link_response_codes",
];

/// `value` when it is a non-empty string.
pub fn text_or_none(value: &Value) -> Option<String> {
    value.as_str().filter(|s| !s.is_empty()).map(String::from)
}

/// Fields of an empty observation, in report order.
pub fn empty_state() -> Map<String, Value> {
    let mut out = Map::new();
    for key in [
        "status_code",
        "indexability",
        "title_hash",
        "title",
        "meta_description",
        "h1",
        "canonical",
        "redirect_target",
        "content_hash",
        "word_count",
        "inlinks",
        "outlinks",
        "response_time_seconds",
        "html_size_bytes",
        "external_link_response_codes",
    ] {
        out.insert(key.into(), Value::Null);
    }
    out
}

fn unavailable(reason: String) -> Map<String, Value> {
    let mut out = Map::new();
    out.insert("state".into(), json!("unavailable"));
    out.insert("reason".into(), json!(reason));
    out
}

/// Report responses only for linked external targets that the scan actually fetched.
pub fn external_link_response_codes(
    con: &Connection,
    url_id: i64,
) -> Result<Map<String, Value>, StoreError> {
    let scope_and_host =
        || -> Result<(Scope, String, bool, String, Option<Map<String, Value>>), StoreError> {
            let (start, config, caps): (Option<String>, String, String) = con.query_row(
                "SELECT start_url,config_json,capabilities_json FROM scan WHERE singleton=1",
                [],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
            )?;
            let config: Value = serde_json::from_str(&config)?;
            let caps: Value = serde_json::from_str(&caps)?;
            let links = caps.get("links").and_then(Value::as_object).cloned();
            let complete = links
                .as_ref()
                .is_some_and(|l| l.get("state") == Some(&json!("complete")));
            let reason = if complete {
                String::new()
            } else {
                links
                    .as_ref()
                    .and_then(|l| l.get("reason"))
                    .and_then(Value::as_str)
                    .unwrap_or("complete link observations are unavailable")
                    .to_string()
            };
            let policy = config
                .get("discovery")
                .and_then(|d| d.get("external"))
                .and_then(Value::as_object);
            if policy.is_some_and(|p| p.get("store") == Some(&json!(false))) {
                return Ok((
                    Scope::from_config(None),
                    String::new(),
                    complete,
                    reason,
                    Some(unavailable("external links were not retained".into())),
                ));
            }
            let scope_cfg: Option<BTreeMap<String, Value>> = config
                .get("scope")
                .and_then(Value::as_object)
                .map(|o| o.iter().map(|(k, v)| (k.clone(), v.clone())).collect());
            let host = start.as_deref().and_then(hostname_of).unwrap_or_default();
            Ok((
                Scope::from_config(scope_cfg.as_ref()),
                host,
                complete,
                reason,
                None,
            ))
        };
    let (scope, host, link_complete, link_reason, early) = match scope_and_host() {
        Ok(parts) => parts,
        Err(error) => {
            let text: String = error.to_string().chars().take(512).collect();
            return Ok(unavailable(format!("scan scope is unavailable: {text}")));
        }
    };
    if let Some(done) = early {
        return Ok(done);
    }
    let mut totals: BTreeMap<i64, i64> = BTreeMap::new();
    let (mut external, mut observed) = (0i64, 0i64);
    let mut stmt = con.prepare(
        "SELECT d.url,l.destination_url_id,\
         (SELECT COALESCE(r.effective_status_code,r.status_code) FROM responses r \
         WHERE r.request_url_id=l.destination_url_id AND r.purpose='page' \
         ORDER BY r.request_ordinal DESC LIMIT 1) AS status_code \
         FROM links l JOIN urls d ON d.url_id=l.destination_url_id \
         WHERE l.source_url_id=? GROUP BY l.destination_url_id ORDER BY d.url",
    )?;
    let rows = stmt.query_map(params![url_id], |r| {
        Ok((r.get::<_, String>(0)?, r.get::<_, Option<i64>>(2)?))
    })?;
    for row in rows {
        let (url, status) = row?;
        if scope.is_internal(&url, &host) {
            continue;
        }
        external += 1;
        if let Some(code) = status.filter(|c| (100..=599).contains(c)) {
            *totals.entry(code).or_insert(0) += 1;
            observed += 1;
        }
    }
    let complete = link_complete && observed == external;
    let state = if complete {
        "measured"
    } else if external > 0 || observed > 0 {
        "partial"
    } else {
        "unavailable"
    };
    let reason = if observed < external {
        "one or more linked external targets have no retained page response".to_string()
    } else if !link_complete {
        if link_reason.is_empty() {
            "external-link coverage is incomplete".to_string()
        } else {
            link_reason
        }
    } else {
        String::new()
    };
    let mut out = Map::new();
    out.insert("state".into(), json!(state));
    out.insert(
        "external_targets".into(),
        if link_complete {
            json!(external)
        } else {
            Value::Null
        },
    );
    out.insert("external_targets_observed".into(), json!(external));
    out.insert("responses_measured".into(), json!(observed));
    let codes: Map<String, Value> = totals
        .iter()
        .map(|(c, n)| (c.to_string(), json!(n)))
        .collect();
    out.insert("codes".into(), Value::Object(codes));
    if !reason.is_empty() {
        out.insert("reason".into(), json!(reason));
    }
    Ok(out)
}

fn state_obj(measured: bool, reason: &str) -> Value {
    if measured {
        json!({"state": "measured"})
    } else {
        json!({"state": "unavailable", "reason": reason})
    }
}

fn count_state(state: &str) -> Value {
    match state {
        "measured" => json!({"state": "measured"}),
        "partial" => json!({"state": "partial", "reason": "link inventory is incomplete"}),
        _ => json!({"state": "unavailable", "reason": "link inventory is unavailable"}),
    }
}

/// Return the page state of `url` in one scan, or `None` when it has no page there.
pub fn observe(path: &Path, url: &str) -> Result<Option<Map<String, Value>>, StoreError> {
    let (con, _mode) = open_scan_mode(path, false, None, true)?;
    let row: Option<Map<String, Value>> = match con.query_row(PAGE_SQL, params![url], |r| {
        let mut map = Map::new();
        for (i, name) in r.as_ref().column_names().iter().enumerate() {
            map.insert((*name).to_string(), cell_value(r.get_ref(i)?));
        }
        Ok(map)
    }) {
        Ok(map) => Some(map),
        Err(rusqlite::Error::QueryReturnedNoRows) => None,
        Err(other) => return Err(other.into()),
    };
    let (caps, partial): (String, i64) = con.query_row(
        "SELECT capabilities_json,crawl_partial FROM scan WHERE singleton=1",
        [],
        |r| Ok((r.get(0)?, r.get(1)?)),
    )?;
    let caps: Value = serde_json::from_str(&caps)?;
    let links_state = match caps.get("links") {
        Some(Value::Object(l)) => l.get("state").cloned().unwrap_or(Value::Null),
        _ => json!("unavailable"),
    };
    let external_statuses = match &row {
        Some(row) => Some(external_link_response_codes(
            &con,
            row["url_id"].as_i64().unwrap_or(0),
        )?),
        None => None,
    };
    drop(con);
    let (Some(row), Some(external_statuses)) = (row, external_statuses) else {
        return Ok(None);
    };
    let title = row["title"].as_str().unwrap_or("").to_string();
    let link_count_state = if links_state == json!("complete") && partial == 0 {
        "measured"
    } else if links_state != json!("unavailable") {
        "partial"
    } else {
        "unavailable"
    };
    let canonical = text_or_none(&row["canonical"]);
    let redirect = text_or_none(&row["redirect_url"]);
    let body_hash = text_or_none(&row["body_sha256"]);
    let body_reason = row["body_reason"]
        .as_str()
        .filter(|s| !s.is_empty())
        .unwrap_or("document was not retained")
        .to_string();
    let has_status = !row["status_code"].is_null();
    let has_time = !row["response_time"].is_null();
    let mut states = Map::new();
    states.insert(
        "status_code".into(),
        state_obj(has_status, "response status was not retained"),
    );
    states.insert(
        "indexability".into(),
        state_obj(
            has_status,
            "response status is required to determine indexability",
        ),
    );
    for key in ["title", "meta_description", "h1", "word_count"] {
        states.insert(key.into(), json!({"state": "measured"}));
    }
    states.insert(
        "canonical".into(),
        json!({"state": if canonical.is_some() { "measured" } else { "absent" }}),
    );
    states.insert(
        "redirect_target".into(),
        json!({"state": if redirect.is_some() { "measured" } else { "absent" }}),
    );
    states.insert(
        "content_hash".into(),
        state_obj(body_hash.is_some(), &body_reason),
    );
    states.insert(
        "response_time_seconds".into(),
        state_obj(has_time, "response timing was not retained"),
    );
    states.insert(
        "html_size_bytes".into(),
        state_obj(has_status, "response size was not retained"),
    );
    states.insert("inlinks".into(), count_state(link_count_state));
    states.insert("outlinks".into(), count_state(link_count_state));
    states.insert(
        "external_link_response_codes".into(),
        Value::Object(external_statuses.clone()),
    );
    let digest = Sha256::digest(title.as_bytes());
    let title_hash: String = digest.iter().map(|b| format!("{b:02x}")).collect();
    let mut out = Map::new();
    out.insert("status_code".into(), row["status_code"].clone());
    out.insert(
        "indexability".into(),
        if has_status {
            json!(row["indexable"].as_i64().unwrap_or(0) != 0)
        } else {
            Value::Null
        },
    );
    out.insert("title".into(), json!(title));
    out.insert("title_hash".into(), json!(title_hash));
    out.insert("meta_description".into(), row["meta_description"].clone());
    out.insert("h1".into(), row["h1"].clone());
    out.insert("canonical".into(), json!(canonical));
    out.insert("redirect_target".into(), json!(redirect));
    out.insert("content_hash".into(), json!(body_hash));
    out.insert("word_count".into(), row["word_count"].clone());
    out.insert(
        "inlinks".into(),
        if link_count_state == "measured" {
            row["inlinks"].clone()
        } else {
            Value::Null
        },
    );
    out.insert(
        "outlinks".into(),
        if link_count_state != "unavailable" {
            row["outlinks"].clone()
        } else {
            Value::Null
        },
    );
    out.insert("response_time_seconds".into(), row["response_time"].clone());
    out.insert(
        "html_size_bytes".into(),
        if has_status {
            row["size_bytes"].clone()
        } else {
            Value::Null
        },
    );
    out.insert("field_states".into(), Value::Object(states));
    out.insert(
        "external_link_response_codes".into(),
        Value::Object(external_statuses),
    );
    Ok(Some(out))
}

fn field_state(entry: &Map<String, Value>, field: &str) -> Option<String> {
    entry
        .get("field_states")
        .and_then(|s| s.get(field))
        .and_then(|f| f.get("state"))
        .and_then(Value::as_str)
        .map(String::from)
}

/// Compare adjacent observations without treating an absent URL as a fix.
pub fn transitions(scans: &[Map<String, Value>]) -> Vec<Map<String, Value>> {
    let mut out = Vec::new();
    for pair in scans.windows(2) {
        let (newer, older) = (&pair[0], &pair[1]);
        let mut entry = Map::new();
        entry.insert("before_scan_uuid".into(), older["scan_uuid"].clone());
        entry.insert("after_scan_uuid".into(), newer["scan_uuid"].clone());
        if newer["state"] != json!("present") || older["state"] != json!("present") {
            entry.insert("state".into(), json!("not_comparable"));
            entry.insert(
                "reason".into(),
                json!("both scans must contain a readable observation of this URL"),
            );
            entry.insert("changes".into(), json!({}));
        } else {
            let mut changes = Map::new();
            for field in CHANGE_FIELDS {
                let bad =
                    |s: Option<String>| matches!(s.as_deref(), Some("unavailable" | "partial"));
                if bad(field_state(older, field)) || bad(field_state(newer, field)) {
                    changes.insert(
                        (*field).into(),
                        json!({
                            "state": "unavailable",
                            "reason": "field evidence is incomplete on at least one scan",
                        }),
                    );
                } else if older[*field] != newer[*field] {
                    changes.insert(
                        (*field).into(),
                        json!({"before": older[*field], "after": newer[*field]}),
                    );
                }
            }
            entry.insert("state".into(), json!("compared"));
            entry.insert("changes".into(), Value::Object(changes));
        }
        out.push(entry);
    }
    out
}

/// Return the state of `url` in the newest `limit` scans of the directory, newest first.
///
/// `number` counts every scan of the directory from 1 for the oldest, so it stays stable when
/// the window of returned scans changes.
/// `limit` defaults to `DEFAULT_LIMIT`.
pub fn url_history(
    scans_dir: &Path,
    url: &str,
    limit: i64,
) -> Result<Map<String, Value>, StoreError> {
    if url.is_empty() || url.chars().count() as i64 > MAX_URL_LENGTH {
        return Err(StoreError::Invalid(
            "url must be nonempty exact retained URL text of at most 8192 characters".into(),
        ));
    }
    if !(1..=MAX_LIMIT).contains(&limit) {
        return Err(StoreError::Invalid(format!(
            "limit must be an integer 1..{MAX_LIMIT}"
        )));
    }
    let (items, catalog_errors) = catalog(scans_dir)?;
    let total = items.len() as i64;
    let mut scans: Vec<Map<String, Value>> = Vec::new();
    let mut errors: Vec<Value> = catalog_errors.into_iter().map(Value::Object).collect();
    for (index, item) in items.iter().take(limit as usize).enumerate() {
        let mut entry = Map::new();
        entry.insert("scan_uuid".into(), item["uuid"].clone());
        entry.insert("number".into(), json!(total - index as i64));
        entry.insert("finished_at".into(), item["finished_at"].clone());
        let location = item["path"].as_str().unwrap_or("");
        match observe(Path::new(location), url) {
            Err(error) => {
                entry.insert("state".into(), json!("unavailable"));
                entry.extend(empty_state());
                let reason: String = error.to_string().chars().take(2048).collect();
                errors.push(json!({"scan_uuid": item["uuid"], "reason": reason}));
            }
            Ok(None) => {
                entry.insert("state".into(), json!("absent"));
                entry.extend(empty_state());
            }
            Ok(Some(found)) => {
                entry.insert("state".into(), json!("present"));
                entry.extend(found);
            }
        }
        scans.push(entry);
    }
    let mut out = Map::new();
    out.insert("ok".into(), json!(true));
    out.insert(
        "state".into(),
        json!(if errors.is_empty() {
            "available"
        } else {
            "partial"
        }),
    );
    out.insert("format".into(), json!(FORMAT));
    out.insert("url".into(), json!(url));
    out.insert("scan_total".into(), json!(total));
    out.insert("limit".into(), json!(limit));
    out.insert("returned".into(), json!(scans.len()));
    out.insert("has_more".into(), json!(total > scans.len() as i64));
    let moves = transitions(&scans);
    out.insert(
        "scans".into(),
        Value::Array(scans.into_iter().map(Value::Object).collect()),
    );
    out.insert(
        "transitions".into(),
        Value::Array(moves.into_iter().map(Value::Object).collect()),
    );
    out.insert("errors".into(), Value::Array(errors));
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn present(uuid: &str, title: &str, status: Option<i64>, state: &str) -> Map<String, Value> {
        let mut m = Map::new();
        m.insert("scan_uuid".into(), json!(uuid));
        m.insert("state".into(), json!("present"));
        m.extend(empty_state());
        m.insert("title".into(), json!(title));
        m.insert("status_code".into(), json!(status));
        let mut states = Map::new();
        for f in CHANGE_FIELDS {
            states.insert((*f).into(), json!({"state": "measured"}));
        }
        states.insert("inlinks".into(), json!({"state": state}));
        m.insert("field_states".into(), Value::Object(states));
        m
    }

    #[test]
    fn transitions_report_changes_and_incomplete_fields() {
        let newer = present("b", "New", Some(200), "partial");
        let older = present("a", "Old", Some(200), "measured");
        let t = transitions(&[newer, older]);
        assert_eq!(t.len(), 1);
        assert_eq!(t[0]["state"], json!("compared"));
        assert_eq!(
            t[0]["changes"]["title"],
            json!({"before": "Old", "after": "New"})
        );
        assert_eq!(t[0]["changes"]["inlinks"]["state"], json!("unavailable"));
        assert!(t[0]["changes"].get("status_code").is_none());
    }

    #[test]
    fn absent_scans_are_not_comparable() {
        let mut absent = present("b", "", None, "measured");
        absent.insert("state".into(), json!("absent"));
        let t = transitions(&[absent, present("a", "Old", Some(200), "measured")]);
        assert_eq!(t[0]["state"], json!("not_comparable"));
        assert_eq!(t[0]["changes"], json!({}));
        assert!(transitions(&[present("a", "x", None, "measured")]).is_empty());
    }

    #[test]
    fn arguments_are_validated() {
        let dir = Path::new("/nonexistent");
        assert!(url_history(dir, "", 10).is_err());
        assert!(url_history(dir, "https://e.test/", 0).is_err());
        assert!(url_history(dir, "https://e.test/", 501).is_err());
    }

    #[test]
    fn text_or_none_keeps_only_nonempty_strings() {
        assert_eq!(text_or_none(&json!("a")), Some("a".into()));
        assert_eq!(text_or_none(&json!("")), None);
        assert_eq!(text_or_none(&json!(3)), None);
    }

    #[test]
    fn empty_state_has_fifteen_null_fields() {
        let e = empty_state();
        assert_eq!(e.len(), 15);
        assert!(e.values().all(Value::is_null));
    }
}
