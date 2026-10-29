//! Closed versioned native context; observations remain data, never instructions.

use crate::discovery_ledger::py_dumps;
use crate::error::StoreError;
use crate::scan_artifact::{insert, ScanError};
use rusqlite::{params, Connection, OptionalExtension};
use serde_json::{json, Map, Value};
use std::collections::BTreeSet;

/// Settings that live in the `native_config_extension` context item instead of the frozen config
/// header, so an older reader never meets a key it does not know.
pub const CONFIG_EXTENSION_PATHS: &[&str] = &[
    "scope.normalize_tracking_parameters",
    "http.auth",
    "http.auth_acknowledged",
    "http.zstd",
    "rendering.actions",
    "rendering.presets",
    "rendering.artifacts.screenshot_mode",
];

/// One context row as a string-keyed map.
pub type ContextItem = Map<String, Value>;

/// Build a `ScanError` store error.
pub(crate) fn bad(message: &str) -> StoreError {
    StoreError::ScanError(ScanError {
        message: message.to_string(),
    })
}

/// True for a JSON integer; booleans and floats do not count.
pub(crate) fn is_int(v: &Value) -> bool {
    v.is_i64() || v.is_u64()
}

/// True for a non-negative JSON integer.
pub(crate) fn is_nonneg_int(v: &Value) -> bool {
    v.is_u64() || v.as_i64().is_some_and(|n| n >= 0)
}

/// True when the map has exactly the given keys.
pub(crate) fn exact_keys(map: &Map<String, Value>, keys: &[&str]) -> bool {
    map.len() == keys.len() && keys.iter().all(|k| map.contains_key(*k))
}

/// True for a 64-character lowercase hex string.
fn is_hex(v: &Value, len: usize) -> bool {
    v.as_str().is_some_and(|s| {
        s.len() == len && s.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'))
    })
}

fn text<'a>(item: &'a ContextItem, key: &str) -> &'a str {
    item.get(key).and_then(Value::as_str).unwrap_or("")
}

/// Numeric equality with an integer; `0.0` and `false` also count as `0`.
fn eq_num(v: &Value, n: i64) -> bool {
    match v {
        Value::Bool(b) => i64::from(*b) == n,
        Value::Number(x) => x.as_f64() == Some(n as f64),
        _ => false,
    }
}

fn exists(con: &Connection, sql: &str, args: impl rusqlite::Params) -> Result<bool, StoreError> {
    Ok(con.query_row(sql, args, |_| Ok(())).optional()?.is_some())
}

/// Parses integer text the way context keys carry it: sign, whitespace and `_` separators.
fn py_int(text: &str) -> Option<i64> {
    let t = text.trim();
    let digits = t.strip_prefix(['+', '-']).unwrap_or(t);
    if digits.is_empty()
        || digits.starts_with('_')
        || digits.ends_with('_')
        || digits.contains("__")
        || !digits.chars().all(|c| c.is_ascii_digit() || c == '_')
    {
        return None;
    }
    t.replace('_', "").parse().ok()
}

/// Validate closed rule results and bind their envelope to one document.
pub fn validate_extraction_rule_evidence(
    con: &Connection,
    item: &ContextItem,
    payload: &Value,
) -> Result<(), StoreError> {
    if spiderhead_parse::extraction_rules::validate_result(payload).is_err() {
        return Err(bad("native extraction rule evidence is invalid"));
    }
    let rules = payload["rules"].as_array().map_or(0, Vec::len);
    if rules > 100
        || item.get("completeness") != payload.get("state")
        || item.get("reason") != payload.get("reason")
    {
        return Err(bad("native extraction rule evidence envelope disagrees"));
    }
    let key_error = || bad("native extraction rule evidence key is invalid");
    let key = text(item, "item_key");
    let (prefix, suffix) = key
        .split_once(":document:")
        .filter(|(p, s)| p.starts_with("page:") && s.contains(":representation:"))
        .ok_or_else(key_error)?;
    let (document_text, representation) = suffix
        .split_once(":representation:")
        .ok_or_else(key_error)?;
    let (Some(page_url_id), Some(document_id)) = (py_int(&prefix[5..]), py_int(document_text))
    else {
        return Err(key_error());
    };
    if page_url_id < 1
        || document_id < 1
        || payload.get("representation").and_then(Value::as_str) != Some(representation)
        || !exists(
            con,
            "SELECT 1 FROM documents WHERE document_id=?1 AND url_id=?2 AND representation=?3",
            params![document_id, page_url_id, representation],
        )?
    {
        return Err(bad(
            "native extraction rule evidence binds the wrong document",
        ));
    }
    Ok(())
}

fn validate_elapsed(item: &ContextItem, payload: &Value, kind: &str) -> Result<(), StoreError> {
    let ok = payload.as_object().is_some_and(|p| {
        exact_keys(p, &["schema_version", "seconds", "active"])
            && p["schema_version"].as_str() == Some(&format!("{kind}.v1"))
            && p["seconds"]
                .as_f64()
                .is_some_and(|s| s.is_finite() && s >= 0.0)
            && p["active"].is_boolean()
    }) && text(item, "item_key") == "run"
        && text(item, "completeness") == "complete"
        && text(item, "reason").is_empty();
    if ok {
        Ok(())
    } else {
        let what = if kind == "render_elapsed" {
            "render"
        } else {
            "list"
        };
        Err(bad(&format!("native {what} elapsed context is invalid")))
    }
}

fn complete_run(item: &ContextItem) -> bool {
    text(item, "item_key") == "run"
        && text(item, "completeness") == "complete"
        && text(item, "reason").is_empty()
}

fn validate_reanalysis_provenance(item: &ContextItem, payload: &Value) -> Result<(), StoreError> {
    const KEYS: [&str; 14] = [
        "parent_scan_uuid",
        "capture_scan_uuid",
        "source_evidence_revision",
        "derived_evidence_revision",
        "source_audit_sha256",
        "source_writer_version",
        "source_writer_revision",
        "source_runtime_versions_json",
        "source_config_sha256",
        "capture_writer_version",
        "capture_writer_revision",
        "capture_runtime_versions_json",
        "capture_config_sha256",
        "capture_run",
    ];
    let invalid = || bad("native reanalysis provenance is invalid");
    let p = payload.as_object().ok_or_else(invalid)?;
    let s = |k: &str| p.get(k).is_some_and(Value::is_string);
    let source_revision = p.get("source_evidence_revision");
    let ok = text(item, "item_key") == "run"
        && text(item, "completeness") == "complete"
        && text(item, "reason") == "offline reanalysis"
        && exact_keys(p, &KEYS)
        && s("parent_scan_uuid")
        && s("capture_scan_uuid")
        && source_revision.is_some_and(is_nonneg_int)
        && p["derived_evidence_revision"].as_i64().is_some()
        && p["derived_evidence_revision"].as_i64()
            == source_revision.and_then(Value::as_i64).map(|n| n + 1)
        && (p["source_audit_sha256"].is_null() || is_hex(&p["source_audit_sha256"], 64))
        && s("source_writer_version")
        && is_hex(&p["source_writer_revision"], 40)
        && s("source_runtime_versions_json")
        && is_hex(&p["source_config_sha256"], 64)
        && s("capture_writer_version")
        && is_hex(&p["capture_writer_revision"], 40)
        && s("capture_runtime_versions_json")
        && is_hex(&p["capture_config_sha256"], 64);
    if !ok {
        return Err(invalid());
    }
    let capture = &p["capture_run"];
    let capture_ok = capture.as_object().is_some_and(|c| {
        exact_keys(
            c,
            &[
                "lifecycle",
                "finish_reason",
                "crawl_partial",
                "created_at",
                "finished_at",
            ],
        ) && matches!(
            c["lifecycle"].as_str(),
            Some("running" | "interrupted" | "finished" | "failed")
        ) && c["finish_reason"].is_string()
            && matches!(c["crawl_partial"].as_i64(), Some(0 | 1))
    });
    if !capture_ok {
        return Err(bad("reanalysis capture state is invalid"));
    }
    crate::corpus_validation::timestamp(&capture["created_at"], "capture created_at", false)?;
    crate::corpus_validation::timestamp(&capture["finished_at"], "capture finished_at", true)
}

fn validate_robots_group(group: &Value) -> Result<(), StoreError> {
    let legacy = ["user_agents", "allow", "disallow", "crawl_delay"];
    let shape = || bad("native robots group shape is invalid");
    let g = group.as_object().ok_or_else(shape)?;
    // The rule-pattern line map (1-based) is optional bookkeeping.
    let keys: BTreeSet<&str> = g
        .keys()
        .map(String::as_str)
        .filter(|k| *k != "_line_of")
        .collect();
    let with_rate: BTreeSet<&str> = legacy
        .iter()
        .copied()
        .chain(["request_rate_delay"])
        .collect();
    if keys != legacy.iter().copied().collect() && keys != with_rate {
        return Err(shape());
    }
    if let Some(line_of) = g.get("_line_of") {
        let falsy = matches!(line_of, Value::Null | Value::Bool(false))
            || line_of.as_array().is_some_and(Vec::is_empty)
            || line_of.as_object().is_some_and(Map::is_empty);
        // Booleans count as integers in this check.
        let ok = line_of
            .as_object()
            .is_some_and(|m| m.values().all(|v| is_int(v) || v.is_boolean()));
        if !falsy && !ok {
            return Err(shape());
        }
    }
    for key in ["user_agents", "allow", "disallow"] {
        if !g[key]
            .as_array()
            .is_some_and(|a| a.iter().all(Value::is_string))
        {
            return Err(bad("native robots rules must be string lists"));
        }
    }
    let delay = &g["crawl_delay"];
    if !delay.is_null() && !delay.as_f64().is_some_and(|d| d.is_finite() && d >= 0.0) {
        return Err(bad("native robots delay must be finite and nonnegative"));
    }
    if let Some(rate) = g.get("request_rate_delay").filter(|r| !r.is_null()) {
        if !rate.as_f64().is_some_and(|d| d.is_finite() && d > 0.0) {
            return Err(bad(
                "native robots request-rate delay must be finite and positive",
            ));
        }
    }
    Ok(())
}

fn validate_robots_summary(item: &ContextItem, payload: &Value) -> Result<(), StoreError> {
    let shape = || bad("native robots summary shape is invalid");
    let p = payload
        .as_object()
        .filter(|_| text(item, "item_key") == "run")
        .ok_or_else(shape)?;
    if !exact_keys(
        p,
        &[
            "policy",
            "token",
            "fetch_state",
            "final_response_id",
            "note",
            "parsed",
        ],
    ) {
        return Err(shape());
    }
    if !matches!(
        p["policy"].as_str(),
        Some("respect" | "report_only" | "ignore")
    ) || !p["token"].is_string()
        || !p["note"].is_string()
        || !matches!(
            p["fetch_state"].as_str(),
            Some("fetched" | "unavailable" | "not_fetched")
        )
        || !p["final_response_id"].is_null()
    {
        return Err(bad("native robots summary provenance is invalid"));
    }
    let parsed = &p["parsed"];
    let groups = parsed
        .as_object()
        .filter(|m| exact_keys(m, &["groups", "sitemaps"]))
        .and_then(|m| Some((m["groups"].as_array()?, m["sitemaps"].as_array()?)))
        .filter(|(_, sitemaps)| sitemaps.iter().all(Value::is_string))
        .ok_or_else(|| bad("native parsed robots summary is invalid"))?
        .0;
    if p["fetch_state"] != "fetched" && parsed != &json!({"groups": [], "sitemaps": []}) {
        return Err(bad("unavailable robots summary cannot invent parsed rules"));
    }
    groups.iter().try_for_each(validate_robots_group)
}

fn validate_list_input(item: &ContextItem, payload: &Value) -> Result<(), StoreError> {
    let ok = complete_run(item)
        && payload.as_object().is_some_and(|p| {
            let counts = p.get("counts").and_then(Value::as_object);
            exact_keys(p, &["sha256", "raw_count", "counts"])
                && is_nonneg_int(&p["raw_count"])
                && is_hex(&p["sha256"], 64)
                && counts.is_some_and(|c| {
                    exact_keys(c, &["blank", "url_too_long", "accepted", "duplicate"])
                        && c.values().all(is_nonneg_int)
                        && c.values()
                            .map(|v| v.as_i64().unwrap_or(i64::MAX))
                            .fold(0i64, i64::saturating_add)
                            == p["raw_count"].as_i64().unwrap_or(-1)
                })
        });
    if ok {
        Ok(())
    } else {
        Err(bad("invalid explicit-list input identity"))
    }
}

fn validate_list_robots(
    con: &Connection,
    item: &ContextItem,
    payload: &Value,
) -> Result<(), StoreError> {
    let invalid = || bad("invalid explicit-list robots origin");
    let p = payload
        .as_object()
        .filter(|p| exact_keys(p, &["origin", "parsed"]))
        .ok_or_else(invalid)?;
    let origin = p["origin"]
        .as_array()
        .filter(|o| o.len() == 2 && o.iter().all(|x| x.as_str().is_some_and(|s| !s.is_empty())))
        .ok_or_else(invalid)?;
    if text(item, "item_key") != py_dumps(&Value::Array(origin.clone()), true, true) {
        return Err(invalid());
    }
    let mut inner = item.clone();
    inner.insert("kind".into(), json!("robots_summary"));
    inner.insert("item_key".into(), json!("run"));
    let body = json!({
        "policy": "respect", "token": "*", "fetch_state": "fetched",
        "final_response_id": null, "note": "", "parsed": p["parsed"],
    });
    inner.insert("payload_json".into(), Value::String(body.to_string()));
    validate_context(con, &inner, None)
}

/// Validate one context item: closed shape, closed kind, and its binding to the stored rows.
/// `sitemap_roots` defaults to none.
pub fn validate_context(
    con: &Connection,
    item: &ContextItem,
    sitemap_roots: Option<&BTreeSet<i64>>,
) -> Result<(), StoreError> {
    if !exact_keys(
        item,
        &[
            "kind",
            "item_key",
            "payload_version",
            "payload_json",
            "completeness",
            "reason",
        ],
    ) {
        return Err(bad("native context has unknown or missing fields"));
    }
    if item.values().any(|v| !v.is_string()) {
        return Err(bad("native context metadata must be strings"));
    }
    if !matches!(
        text(item, "completeness"),
        "complete" | "partial" | "unavailable"
    ) {
        return Err(bad("native context completeness is invalid"));
    }
    if text(item, "payload_version") != "scan_context.v1" {
        return Err(bad("native scan context payload version is invalid"));
    }
    let payload: Value = serde_json::from_str(text(item, "payload_json"))
        .map_err(|_| bad("native scan context payload is invalid JSON"))?;
    let kind = text(item, "kind");
    let key = text(item, "item_key");
    let obj = payload.as_object();

    match kind {
        "native_config_extension" => {
            let ok = key == "run"
                && text(item, "completeness") == "complete"
                && text(item, "reason").is_empty()
                && obj.is_some_and(|p| {
                    exact_keys(p, &["schema_version", "settings", "effective_fingerprint"])
                        && p["schema_version"] == "native_config_extension.v1"
                        && p["settings"]
                            .as_object()
                            .is_some_and(|s| exact_keys(s, CONFIG_EXTENSION_PATHS))
                        && is_hex(&p["effective_fingerprint"], 64)
                });
            if !ok {
                return Err(bad("native config extension context is invalid"));
            }
        }
        "analysis_asset_thresholds" => {
            let defaults =
                &spiderhead_core::settings_data::defaults()["analysis"]["asset_thresholds"];
            let ok = complete_run(item)
                && obj.is_some_and(|p| {
                    exact_keys(p, &["schema_version", "thresholds"])
                        && p["schema_version"] == "analysis_asset_thresholds.v1"
                        && p["thresholds"].as_object().is_some_and(|t| {
                            defaults.as_object().is_some_and(|d| {
                                t.len() == d.len() && d.keys().all(|k| t.contains_key(k))
                            }) && t.values().all(|v| v.as_i64().is_some_and(|n| n > 0))
                        })
                });
            if !ok {
                return Err(bad("native analysis asset thresholds context is invalid"));
            }
        }
        "links.position" => {
            crate::link_positions::validate_checkpoint(con, item, &payload)?;
        }
        "list_mode" => {
            if !(complete_run(item) && payload == json!({"mode": "list"})) {
                return Err(bad("invalid explicit-list mode context"));
            }
        }
        "list_input" => validate_list_input(item, &payload)?,
        "list_robots" => validate_list_robots(con, item, &payload)?,
        k if crate::sitemaps::KINDS.contains(&k) => {
            crate::sitemaps::validate_context(con, item, &payload, sitemap_roots)?;
        }
        "render_phase_summary" => {
            crate::render_summary::validate(&payload)?;
            if !key.starts_with("phase:")
                || text(item, "completeness") != "complete"
                || !text(item, "reason").is_empty()
            {
                return Err(bad("invalid render phase summary envelope"));
            }
        }
        "external_check" => crate::external_checks::validate_check(item, &payload)?,
        "external_checks_summary" => crate::external_checks::validate_summary(item, &payload)?,
        "render_elapsed" | "list_elapsed" => validate_elapsed(item, &payload, kind)?,
        "resource_inventory" => {
            let ordered: indexmap::IndexMap<String, Value> =
                item.iter().map(|(k, v)| (k.clone(), v.clone())).collect();
            crate::resources::validate_inventory_context(con, &ordered)?;
        }
        "resource_graph_coverage" => {
            let strings: indexmap::IndexMap<String, String> = item
                .iter()
                .map(|(k, v)| (k.clone(), v.as_str().unwrap_or("").to_string()))
                .collect();
            crate::resource_graph::validate_coverage_context(con, &strings, &payload)?;
        }
        "rendered_route_ledger" | "rendered_route_coverage" | "rendered_route_run_coverage" => {
            crate::rendered_routes::validate_context(con, item, &payload)?;
        }
        "content_evidence" => {
            let ordered: indexmap::IndexMap<String, Value> =
                item.iter().map(|(k, v)| (k.clone(), v.clone())).collect();
            crate::content_evidence::validate_context(con, &ordered, &payload)?;
        }
        "markup_evidence" => crate::markup_evidence::validate_context(
            con,
            item,
            &payload,
            &spiderhead_parse::markup::AMP_CODES,
            &spiderhead_parse::markup::A11Y_CODES,
        )?,
        "browser_artifacts" => {
            let ordered: indexmap::IndexMap<String, Value> =
                item.iter().map(|(k, v)| (k.clone(), v.clone())).collect();
            crate::browser_artifacts::validate_context(con, &ordered, &payload)?;
        }
        "structured_evidence" | "language_evidence" => {
            crate::structured_evidence::validate_context(con, item, &payload)?;
        }
        "extraction_rule_evidence" => validate_extraction_rule_evidence(con, item, &payload)?,
        "head_links" => crate::head_links::validate_context(con, item, &payload)?,
        "target_probe" => {
            crate::target_probes::validate_context(item, &payload)?;
        }
        "redirect_hop_probe" => {
            crate::crawl_records::security_headers::validate_redirect_hop_probe(item, &payload)?;
        }
        "resource_commit" => {
            let ok = obj.is_some_and(|p| {
                exact_keys(p, &["digest", "requests_used"])
                    && is_hex(&p["digest"], 64)
                    && is_nonneg_int(&p["requests_used"])
            }) && text(item, "completeness") == "complete"
                && text(item, "reason").is_empty()
                && key
                    .strip_prefix("resource:")
                    .is_some_and(|d| !d.is_empty() && d.bytes().all(|b| b.is_ascii_digit()));
            if !ok {
                return Err(bad("native resource commit context is invalid"));
            }
        }
        "reanalysis_provenance" => validate_reanalysis_provenance(item, &payload)?,
        "credential_context" => {
            let ok = complete_run(item)
                && obj.is_some_and(|p| {
                    exact_keys(p, &["verifier", "implicit_state"])
                        && (p["verifier"].is_null() || is_hex(&p["verifier"], 64))
                        && p["implicit_state"].is_boolean()
                });
            if !ok {
                return Err(bad("native credential context is invalid"));
            }
        }
        "anonymous_session" => {
            if !(complete_run(item) && payload == json!({"server_set_cookie": true})) {
                return Err(bad("native anonymous session context is invalid"));
            }
        }
        "anonymous_session_resume" => {
            if !(complete_run(item) && payload == json!({"fresh_session": true})) {
                return Err(bad("native anonymous session resume context is invalid"));
            }
        }
        "native_commit" => validate_native_commit(con, item, &payload)?,
        "seed_url" => validate_seed_url(con, item, &payload)?,
        "robots_summary" => validate_robots_summary(item, &payload)?,
        "robots_blocked_url" => validate_robots_blocked_url(con, item, &payload)?,
        _ => {
            return Err(bad(
                "native context kind is unsupported; expected native_commit, \
                 robots_blocked_url, robots_summary or seed_url",
            ))
        }
    }
    Ok(())
}

fn validate_native_commit(
    con: &Connection,
    item: &ContextItem,
    payload: &Value,
) -> Result<(), StoreError> {
    let key = text(item, "item_key");
    let canonical = key.bytes().all(|b| b.is_ascii_digit())
        && !key.is_empty()
        && (key == "0" || !key.starts_with('0'));
    let ok = canonical
        && payload
            .as_object()
            .is_some_and(|p| exact_keys(p, &["digest"]) && is_hex(&p["digest"], 64))
        && text(item, "completeness") == "complete"
        && text(item, "reason") == "atomic page commit";
    if !ok {
        return Err(bad("native scan commit idempotency context is invalid"));
    }
    let ordinal: Option<i64> = key.parse().ok();
    if !exists(
        con,
        "SELECT 1 FROM frontier WHERE queue_ordinal=?1 AND state='done'",
        params![ordinal],
    )? {
        return Err(bad(
            "native scan commit context does not name a completed lease",
        ));
    }
    Ok(())
}

fn validate_seed_url(
    con: &Connection,
    item: &ContextItem,
    payload: &Value,
) -> Result<(), StoreError> {
    let url_id = payload
        .as_object()
        .filter(|p| exact_keys(p, &["url_id", "depth", "source"]))
        .filter(|p| {
            is_int(&p["url_id"])
                && eq_num(&p["depth"], 0)
                && p["source"] == "sitemap"
                && Some(text(item, "item_key"))
                    == p["url_id"].as_i64().map(|n| format!("url:{n}")).as_deref()
        })
        .and_then(|p| p["url_id"].as_i64())
        .ok_or_else(|| bad("native seed context is invalid"))?;
    if !exists(
        con,
        "SELECT 1 FROM frontier WHERE url_id=?1 AND depth=0 AND state!='excluded'",
        params![url_id],
    )? {
        return Err(bad("native seed context must reference an accepted seed"));
    }
    Ok(())
}

fn validate_robots_blocked_url(
    con: &Connection,
    item: &ContextItem,
    payload: &Value,
) -> Result<(), StoreError> {
    let url_id = payload
        .as_object()
        .filter(|p| exact_keys(p, &["url_id", "token", "policy"]))
        .filter(|p| {
            p["token"].is_string()
                && matches!(p["policy"].as_str(), Some("respect" | "report_only"))
                && p["url_id"]
                    .as_i64()
                    .is_some_and(|n| n > 0 && text(item, "item_key") == format!("url:{n}"))
        })
        .and_then(|p| p["url_id"].as_i64())
        .ok_or_else(|| bad("native scan robots_blocked_url context is invalid"))?;
    let url: Option<String> = con
        .query_row(
            "SELECT url FROM urls WHERE url_id=?1",
            params![url_id],
            |r| r.get(0),
        )
        .optional()?;
    let url = url.ok_or_else(|| bad("native scan robots context references an unknown URL"))?;
    if payload["policy"] == "respect"
        && !exists(
            con,
            "SELECT 1 FROM decisions WHERE url=?1 AND reason='blocked_by_robots'",
            params![url],
        )?
    {
        return Err(bad(
            "native robots exclusion lacks its blocked_by_robots decision",
        ));
    }
    Ok(())
}

fn stored_item(con: &Connection, item: &ContextItem) -> Result<Option<ContextItem>, StoreError> {
    Ok(con
        .query_row(
            "SELECT kind,item_key,payload_version,payload_json,completeness,reason \
             FROM context_items WHERE kind=?1 AND item_key=?2",
            params![text(item, "kind"), text(item, "item_key")],
            |r| {
                let mut row = ContextItem::new();
                for (i, name) in [
                    "kind",
                    "item_key",
                    "payload_version",
                    "payload_json",
                    "completeness",
                    "reason",
                ]
                .iter()
                .enumerate()
                {
                    row.insert(
                        (*name).to_string(),
                        r.get::<_, Option<String>>(i)?
                            .map_or(Value::Null, Value::String),
                    );
                }
                Ok(row)
            },
        )
        .optional()?)
}

/// Validate and insert one context item; an identical retry is a no-op, elapsed-time items only
/// `sitemap_roots` defaults to none.
pub fn put_context(
    con: &Connection,
    item: &ContextItem,
    sitemap_roots: Option<&BTreeSet<i64>>,
) -> Result<(), StoreError> {
    validate_context(con, item, sitemap_roots)?;
    if let Some(existing) = stored_item(con, item)? {
        let kind = text(item, "kind");
        if matches!(kind, "render_elapsed" | "list_elapsed") {
            let seconds = |v: &str| -> f64 {
                serde_json::from_str::<Value>(v)
                    .ok()
                    .and_then(|p| p["seconds"].as_f64())
                    .unwrap_or(0.0)
            };
            if seconds(text(item, "payload_json")) < seconds(text(&existing, "payload_json")) {
                let what = if kind == "render_elapsed" {
                    "render"
                } else {
                    "list"
                };
                return Err(bad(&format!("{what} elapsed seconds cannot decrease")));
            }
            con.execute(
                "UPDATE context_items SET payload_json=?1 WHERE kind=?2 AND item_key='run'",
                params![text(item, "payload_json"), kind],
            )?;
            return Ok(());
        }
        if &existing != item {
            return Err(bad(
                "native context retry disagrees with committed observation",
            ));
        }
        return Ok(());
    }
    insert(con, "context_items", item)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    fn item(kind: &str, key: &str, payload: &Value) -> ContextItem {
        let Value::Object(map) = json!({
            "kind": kind,
            "item_key": key,
            "payload_version": "scan_context.v1",
            "payload_json": payload.to_string(),
            "completeness": "complete",
            "reason": "",
        }) else {
            unreachable!()
        };
        map
    }

    fn with(mut base: ContextItem, key: &str, value: &str) -> ContextItem {
        base.insert(key.into(), json!(value));
        base
    }

    fn mem() -> Connection {
        let con = Connection::open_in_memory().expect("open");
        con.execute_batch(crate::schema_sql::SCAN_V1_SQL)
            .expect("scan.v1 ddl");
        con
    }

    fn ok(item: &ContextItem) -> bool {
        validate_context(&mem(), item, None).is_ok()
    }

    fn message(con: &Connection, item: &ContextItem) -> String {
        validate_context(con, item, None).unwrap_err().to_string()
    }

    // Config extension vocabulary is declared in one place.
    #[test]
    fn config_extension_vocabulary_matches_writer_defaults() {
        let declared: BTreeSet<&str> = CONFIG_EXTENSION_PATHS.iter().copied().collect();
        let defaults: BTreeSet<&str> = crate::scan_schema::config_extension_defaults()
            .into_iter()
            .map(|(path, _)| path)
            .collect();
        assert_eq!(declared, defaults);
    }

    // Credential context accepts only a verifier or the implicit marker.
    #[test]
    fn credential_context_accepts_only_verifier_or_implicit_marker() {
        for payload in [
            json!({"verifier": null, "implicit_state": false}),
            json!({"verifier": "a".repeat(64), "implicit_state": true}),
        ] {
            validate_context(&mem(), &item("credential_context", "run", &payload), None).unwrap();
        }
    }

    // Credential context refuses secret-shaped or ambiguous values.
    #[test]
    fn credential_context_refuses_secret_shaped_or_ambiguous_values() {
        let con = mem();
        let base = |p: Value| item("credential_context", "run", &p);
        let cases = [
            base(json!({"verifier": "secret", "implicit_state": false})),
            base(json!({"verifier": "A".repeat(64), "implicit_state": false})),
            base(json!({"verifier": null, "implicit_state": 1})),
            base(json!({"verifier": null, "implicit_state": false, "token": "secret"})),
            with(
                base(json!({"verifier": null, "implicit_state": false})),
                "item_key",
                "url:1",
            ),
            with(
                base(json!({"verifier": null, "implicit_state": false})),
                "reason",
                "secret",
            ),
        ];
        for case in cases {
            assert!(
                message(&con, &case).contains("credential context"),
                "{case:?}"
            );
        }
    }

    #[test]
    fn envelope_is_closed_and_typed() {
        let con = mem();
        let good = item("list_mode", "run", &json!({"mode": "list"}));
        validate_context(&con, &good, None).unwrap();
        let mut extra = good.clone();
        extra.insert("surplus".into(), json!("x"));
        assert!(message(&con, &extra).contains("unknown or missing fields"));
        let mut missing = good.clone();
        missing.remove("reason");
        assert!(message(&con, &missing).contains("unknown or missing fields"));
        let mut numeric = good.clone();
        numeric.insert("reason".into(), json!(0));
        assert!(message(&con, &numeric).contains("must be strings"));
        assert!(message(&con, &with(good.clone(), "completeness", "done"))
            .contains("completeness is invalid"));
        assert!(
            message(&con, &with(good.clone(), "payload_version", "v2")).contains("payload version")
        );
        assert!(
            message(&con, &with(good.clone(), "payload_json", "{nope")).contains("invalid JSON")
        );
        assert!(message(&con, &with(good, "kind", "mystery")).contains("kind is unsupported"));
    }

    #[test]
    fn config_extension_and_thresholds() {
        let settings: Map<String, Value> = CONFIG_EXTENSION_PATHS
            .iter()
            .map(|k| ((*k).to_string(), json!(null)))
            .collect();
        let good = json!({"schema_version": "native_config_extension.v1", "settings": settings, "effective_fingerprint": "ab".repeat(32)});
        assert!(ok(&item("native_config_extension", "run", &good)));
        let mut short = good.clone();
        short["settings"]
            .as_object_mut()
            .unwrap()
            .remove("http.zstd");
        assert!(!ok(&item("native_config_extension", "run", &short)));
        let mut upper = good.clone();
        upper["effective_fingerprint"] = json!("AB".repeat(32));
        assert!(!ok(&item("native_config_extension", "run", &upper)));
        assert!(!ok(&item("native_config_extension", "other", &good)));

        let thresholds = json!({"schema_version": "analysis_asset_thresholds.v1",
            "thresholds": {"network_payload_max_bytes": 4194304, "page_weight_max_bytes": 2097152, "resource_max_bytes": 2097152}});
        assert!(ok(&item("analysis_asset_thresholds", "run", &thresholds)));
        for bad_value in [json!(0), json!(-1), json!(1.5), json!(true), json!("1")] {
            let mut t = thresholds.clone();
            t["thresholds"]["resource_max_bytes"] = bad_value;
            assert!(!ok(&item("analysis_asset_thresholds", "run", &t)));
        }
        let mut extra = thresholds.clone();
        extra["thresholds"]["surprise"] = json!(5);
        assert!(!ok(&item("analysis_asset_thresholds", "run", &extra)));
    }

    #[test]
    fn list_input_counts_must_add_up() {
        let good = json!({"sha256": "0".repeat(64), "raw_count": 4,
            "counts": {"blank": 1, "url_too_long": 0, "accepted": 2, "duplicate": 1}});
        assert!(ok(&item("list_input", "run", &good)));
        let mut off = good.clone();
        off["raw_count"] = json!(5);
        assert!(!ok(&item("list_input", "run", &off)));
        let mut negative = good.clone();
        negative["counts"]["blank"] = json!(-1);
        assert!(!ok(&item("list_input", "run", &negative)));
        let mut missing = good.clone();
        missing.as_object_mut().unwrap().remove("counts");
        assert!(!ok(&item("list_input", "run", &missing)));
    }

    #[test]
    fn list_robots_key_is_ascii_escaped_origin() {
        let parsed = json!({"groups": [], "sitemaps": []});
        let origin = json!(["https", "b\u{fc}cher.test"]);
        let payload = json!({"origin": origin, "parsed": parsed});
        let key = format!("[\"https\",\"b{}u00fccher.test\"]", '\\');
        validate_context(&mem(), &item("list_robots", &key, &payload), None).unwrap();
        assert!(!ok(&item(
            "list_robots",
            r#"["https","bücher.test"]"#,
            &payload
        )));
        let one = json!({"origin": ["https"], "parsed": parsed});
        assert!(!ok(&item("list_robots", r#"["https"]"#, &one)));
    }

    #[test]
    fn elapsed_contexts() {
        for (kind, label) in [("render_elapsed", "render"), ("list_elapsed", "list")] {
            let good =
                json!({"schema_version": format!("{kind}.v1"), "seconds": 1.5, "active": false});
            assert!(ok(&item(kind, "run", &good)));
            assert!(ok(&item(
                kind,
                "run",
                &json!({"schema_version": format!("{kind}.v1"), "seconds": 0, "active": true})
            )));
            let con = mem();
            let mut negative = good.clone();
            negative["seconds"] = json!(-0.5);
            assert!(message(&con, &item(kind, "run", &negative))
                .contains(&format!("native {label} elapsed")));
            let mut flag = good.clone();
            flag["active"] = json!(0);
            assert!(!ok(&item(kind, "run", &flag)));
            let mut wrong = good.clone();
            wrong["schema_version"] = json!("other.v1");
            assert!(!ok(&item(kind, "run", &wrong)));
        }
    }

    fn robots(parsed: Value, state: &str) -> ContextItem {
        item(
            "robots_summary",
            "run",
            &json!({
                "policy": "respect", "token": "*", "fetch_state": state,
                "final_response_id": null, "note": "", "parsed": parsed,
            }),
        )
    }

    #[test]
    fn robots_summary_shapes() {
        let group =
            json!({"user_agents": ["*"], "allow": ["/"], "disallow": [], "crawl_delay": null});
        let parsed = |g: Value| json!({"groups": [g], "sitemaps": ["https://h.test/s.xml"]});
        assert!(ok(&robots(parsed(group.clone()), "fetched")));
        let mut line_of = group.clone();
        line_of["_line_of"] = json!({"allow:/": 2});
        assert!(ok(&robots(parsed(line_of), "fetched")));
        let mut rate = group.clone();
        rate["request_rate_delay"] = json!(2.5);
        assert!(ok(&robots(parsed(rate.clone()), "fetched")));
        rate["request_rate_delay"] = json!(0);
        assert!(!ok(&robots(parsed(rate), "fetched")));
        let mut delay = group.clone();
        delay["crawl_delay"] = json!(-1);
        assert!(!ok(&robots(parsed(delay), "fetched")));
        let mut bool_delay = group.clone();
        bool_delay["crawl_delay"] = json!(true);
        assert!(!ok(&robots(parsed(bool_delay), "fetched")));
        let mut strings = group.clone();
        strings["allow"] = json!(["/", 1]);
        assert!(!ok(&robots(parsed(strings), "fetched")));
        let mut unknown = group.clone();
        unknown["extra"] = json!(1);
        assert!(!ok(&robots(parsed(unknown), "fetched")));
        let mut bad_lines = group.clone();
        bad_lines["_line_of"] = json!({"allow:/": "2"});
        assert!(!ok(&robots(parsed(bad_lines), "fetched")));
        // unavailable fetch cannot carry rules
        assert!(!ok(&robots(parsed(group), "unavailable")));
        assert!(ok(&robots(
            json!({"groups": [], "sitemaps": []}),
            "unavailable"
        )));
        // provenance
        let con = mem();
        let mut policy = robots(json!({"groups": [], "sitemaps": []}), "fetched");
        policy.insert(
            "payload_json".into(),
            json!(
                json!({"policy": "obey", "token": "*", "fetch_state": "fetched",
            "final_response_id": null, "note": "", "parsed": {"groups": [], "sitemaps": []}})
                .to_string()
            ),
        );
        assert!(message(&con, &policy).contains("provenance"));
        let wrong_key = with(
            robots(json!({"groups": [], "sitemaps": []}), "fetched"),
            "item_key",
            "x",
        );
        assert!(message(&con, &wrong_key).contains("summary shape"));
    }

    #[test]
    fn native_commit_needs_a_done_lease() {
        let con = mem();
        let payload = json!({"digest": "c".repeat(64)});
        let commit = |key: &str| {
            let mut i = item("native_commit", key, &payload);
            i.insert("reason".into(), json!("atomic page commit"));
            i
        };
        assert!(message(&con, &commit("4")).contains("completed lease"));
        con.execute_batch(
            "INSERT INTO urls(url_id,url) VALUES (1,'http://h.test/');
             INSERT INTO frontier(url_id,queue_ordinal,depth,state) VALUES (1,4,0,'done');",
        )
        .unwrap_or_else(|e| panic!("fixture: {e}"));
        validate_context(&con, &commit("4"), None).unwrap();
        for key in ["04", "-4", "4.0", "", "٤"] {
            assert!(
                message(&con, &commit(key)).contains("idempotency context"),
                "{key}"
            );
        }
        let mut wrong_reason = commit("4");
        wrong_reason.insert("reason".into(), json!(""));
        assert!(!ok(&wrong_reason));
    }

    #[test]
    fn seed_url_requires_an_accepted_seed() {
        let con = mem();
        let seed = |depth: Value| {
            item(
                "seed_url",
                "url:1",
                &json!({"url_id": 1, "depth": depth, "source": "sitemap"}),
            )
        };
        assert!(message(&con, &seed(json!(0))).contains("accepted seed"));
        con.execute_batch(
            "INSERT INTO urls(url_id,url) VALUES (1,'http://h.test/');
             INSERT INTO frontier(url_id,queue_ordinal,depth,state) VALUES (1,1,0,'queued');",
        )
        .unwrap_or_else(|e| panic!("fixture: {e}"));
        validate_context(&con, &seed(json!(0)), None).unwrap();
        assert!(message(&con, &seed(json!(1))).contains("seed context is invalid"));
        let mut wrong_key = seed(json!(0));
        wrong_key.insert("item_key".into(), json!("url:2"));
        assert!(!ok(&wrong_key));
    }

    #[test]
    fn robots_blocked_url_checks_decision_for_respect_policy() {
        let con = mem();
        let blocked = |policy: &str| {
            item(
                "robots_blocked_url",
                "url:1",
                &json!({"url_id": 1, "token": "*", "policy": policy}),
            )
        };
        assert!(message(&con, &blocked("respect")).contains("unknown URL"));
        con.execute_batch("INSERT INTO urls(url_id,url) VALUES (1,'http://h.test/x');")
            .unwrap_or_else(|e| panic!("fixture: {e}"));
        assert!(message(&con, &blocked("respect")).contains("blocked_by_robots decision"));
        validate_context(&con, &blocked("report_only"), None).unwrap();
        assert!(message(&con, &blocked("ignore")).contains("robots_blocked_url context is invalid"));
        con.execute_batch("INSERT INTO decisions(url,reason,source,occurrence_key) VALUES ('http://h.test/x','blocked_by_robots','robots','k');")
            .unwrap_or_else(|e| panic!("fixture: {e}"));
        validate_context(&con, &blocked("respect"), None).unwrap();
    }

    #[test]
    fn small_closed_contexts() {
        let digest = "d".repeat(64);
        assert!(ok(&item(
            "resource_commit",
            "resource:12",
            &json!({"digest": digest, "requests_used": 0})
        )));
        assert!(!ok(&item(
            "resource_commit",
            "resource:1a",
            &json!({"digest": digest, "requests_used": 0})
        )));
        assert!(!ok(&item(
            "resource_commit",
            "resource:",
            &json!({"digest": digest, "requests_used": 0})
        )));
        assert!(!ok(&item(
            "resource_commit",
            "resource:1",
            &json!({"digest": digest, "requests_used": -1})
        )));
        assert!(ok(&item(
            "anonymous_session",
            "run",
            &json!({"server_set_cookie": true})
        )));
        assert!(!ok(&item(
            "anonymous_session",
            "run",
            &json!({"server_set_cookie": false})
        )));
        assert!(ok(&item(
            "anonymous_session_resume",
            "run",
            &json!({"fresh_session": true})
        )));
        assert!(!ok(&item("anonymous_session_resume", "run", &json!({}))));
    }

    #[test]
    fn reanalysis_provenance_shape_is_closed() {
        let good = json!({
            "parent_scan_uuid": "p", "capture_scan_uuid": "c",
            "source_evidence_revision": 1, "derived_evidence_revision": 2,
            "source_audit_sha256": null,
            "source_writer_version": "1", "source_writer_revision": "a".repeat(40),
            "source_runtime_versions_json": "{}", "source_config_sha256": "b".repeat(64),
            "capture_writer_version": "1", "capture_writer_revision": "a".repeat(40),
            "capture_runtime_versions_json": "{}", "capture_config_sha256": "b".repeat(64),
            "capture_run": {"lifecycle": "finished", "finish_reason": "", "crawl_partial": 0,
                "created_at": "2026-01-01T00:00:00Z", "finished_at": null},
        });
        let make = |p: &Value| {
            let mut i = item("reanalysis_provenance", "run", p);
            i.insert("reason".into(), json!("offline reanalysis"));
            i
        };
        let con = mem();
        let mut jump = good.clone();
        jump["derived_evidence_revision"] = json!(5);
        assert!(message(&con, &make(&jump)).contains("reanalysis provenance"));
        let mut short = good.clone();
        short["source_writer_revision"] = json!("abc");
        assert!(message(&con, &make(&short)).contains("reanalysis provenance"));
        let mut lifecycle = good.clone();
        lifecycle["capture_run"]["lifecycle"] = json!("paused");
        assert!(message(&con, &make(&lifecycle)).contains("capture state"));
        let mut partial = good.clone();
        partial["capture_run"]["crawl_partial"] = json!(2);
        assert!(message(&con, &make(&partial)).contains("capture state"));
        let mut wrong_reason = make(&good);
        wrong_reason.insert("reason".into(), json!(""));
        assert!(message(&con, &wrong_reason).contains("reanalysis provenance"));
    }

    #[test]
    fn py_int_accepts_what_python_int_accepts() {
        assert_eq!(py_int("12"), Some(12));
        assert_eq!(py_int(" 7 "), Some(7));
        assert_eq!(py_int("+3"), Some(3));
        assert_eq!(py_int("1_000"), Some(1000));
        for bad in ["", "_1", "1_", "1__0", "x", "1.5", "--1"] {
            assert_eq!(py_int(bad), None, "{bad}");
        }
    }

    #[test]
    fn put_context_is_idempotent_and_forward_only() {
        let con = mem();
        let mode = item("list_mode", "run", &json!({"mode": "list"}));
        put_context(&con, &mode, None).unwrap();
        put_context(&con, &mode, None).unwrap();
        let count: i64 = con
            .query_row("SELECT COUNT(*) FROM context_items", [], |r| r.get(0))
            .unwrap();
        assert_eq!(count, 1);
        let mut other = mode.clone();
        other.insert("payload_json".into(), json!(r#"{"mode": "list" }"#));
        assert!(put_context(&con, &other, None)
            .unwrap_err()
            .to_string()
            .contains("retry disagrees"));
        // invalid item never reaches the table
        assert!(put_context(&con, &with(mode, "completeness", "x"), None).is_err());

        let elapsed = |s: f64| {
            item(
                "render_elapsed",
                "run",
                &json!({"schema_version": "render_elapsed.v1", "seconds": s, "active": false}),
            )
        };
        put_context(&con, &elapsed(2.0), None).unwrap();
        put_context(&con, &elapsed(3.0), None).unwrap();
        let err = put_context(&con, &elapsed(1.0), None)
            .unwrap_err()
            .to_string();
        assert!(
            err.contains("render elapsed seconds cannot decrease"),
            "{err}"
        );
        let stored: String = con
            .query_row(
                "SELECT payload_json FROM context_items WHERE kind='render_elapsed'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert!(stored.contains("3.0"), "{stored}");
    }

    // Every raw scan's context rows validate, and their stored payload bytes equal the
    // sorted-key JSON re-emitted from the parsed payload.
    #[test]
    fn raw_scans_validate_and_match_baselines() {
        let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../oracle");
        let own = [
            "analysis_asset_thresholds",
            "native_config_extension",
            "credential_context",
            "robots_summary",
            "native_commit",
            "seed_url",
        ];
        let origin = regex::Regex::new(r"http://127\.0\.0\.1:\d+").unwrap();
        for site in [
            "basic",
            "canonical-hreflang",
            "errors",
            "redirects",
            "robots-sitemap",
            "schema",
        ] {
            let con = Connection::open_with_flags(
                root.join("raw-scans").join(site).join("scan.sqlite"),
                rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
            )
            .expect("open raw scan");
            let mut stmt = con
                .prepare("SELECT kind,item_key,payload_version,payload_json,completeness,reason FROM context_items")
                .unwrap();
            let rows: Vec<ContextItem> = stmt
                .query_map([], |r| {
                    let mut m = ContextItem::new();
                    for (i, name) in [
                        "kind",
                        "item_key",
                        "payload_version",
                        "payload_json",
                        "completeness",
                        "reason",
                    ]
                    .iter()
                    .enumerate()
                    {
                        m.insert((*name).to_string(), Value::String(r.get(i)?));
                    }
                    Ok(m)
                })
                .unwrap()
                .collect::<Result<_, _>>()
                .unwrap();
            let mut actual = Vec::new();
            for row in &rows {
                let kind = text(row, "kind");
                let payload: Value = serde_json::from_str(text(row, "payload_json")).unwrap();
                if own.contains(&kind) {
                    validate_context(&con, row, None)
                        .unwrap_or_else(|e| panic!("{site}/{kind}: {e}"));
                    assert_eq!(
                        py_dumps(&payload, false, true),
                        text(row, "payload_json"),
                        "{site}/{kind}"
                    );
                }
                let mut shown = row.clone();
                // baseline rule 1 (NORMALISATION.md): fixture origin -> {BASE}
                let normalised = origin
                    .replace_all(&payload.to_string(), "{BASE}")
                    .into_owned();
                shown.insert(
                    "payload_json".into(),
                    serde_json::from_str(&normalised).unwrap(),
                );
                actual.push(Value::Object(shown));
            }
            let baseline: Value = serde_json::from_str(
                &std::fs::read_to_string(
                    root.join("baselines").join(site).join("scan.tables.json"),
                )
                .unwrap(),
            )
            .unwrap();
            // digests are normalised in the baselines; compare every other byte of the kinds this
            // module validates
            let pick = |rows: &Value| -> Value {
                Value::Array(
                    rows.as_array()
                        .unwrap()
                        .iter()
                        .filter(|r| own.contains(&r["kind"].as_str().unwrap_or("")))
                        .cloned()
                        .collect(),
                )
            };
            let (mut want, mut got) = (
                pick(&baseline["context_items"]),
                pick(&Value::Array(actual)),
            );
            for rows in [&mut want, &mut got] {
                for row in rows.as_array_mut().unwrap() {
                    if let Some(p) = row["payload_json"].as_object_mut() {
                        p.remove("digest");
                    }
                }
            }
            assert_eq!(want, got, "{site}");
        }
    }
}
