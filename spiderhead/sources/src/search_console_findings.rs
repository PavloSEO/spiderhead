//! Search Console evidence as page-level crawl findings. Pure: no network, no registry.
//!
//! Joins crawl pages to a normalized GSC document (`spiderhead.normalized-evidence.v1`) and
//! evaluates the three analytics checks plus the bounded URL-Inspection sample checks. A check
//! that cannot be decided is listed under `_skipped` with its reason; an absent or skipped
//! check is never reported as clean.

use std::collections::{BTreeMap, BTreeSet};

use serde_json::{json, Map, Value};

use crate::cloudflare::CfError;
use crate::evidence_join::join_evidence;
use crate::external_join::{normalize_join_key, orphan_urls};
use crate::pystr;

/// `GSC_ORPHAN` — GSC rows naming a same-origin URL the crawl never reached.
pub const GSC_ORPHAN: &str = "GSC_ORPHAN";
/// `NO_SEARCH_DATA` — indexable crawled pages with no search-analytics row.
pub const NO_SEARCH_DATA: &str = "NO_SEARCH_DATA";
/// `NON_INDEXABLE_WITH_SEARCH_DATA` — non-indexable crawled pages that still earn impressions.
pub const NON_INDEXABLE_WITH_SEARCH_DATA: &str = "NON_INDEXABLE_WITH_SEARCH_DATA";
/// The analytics check ids in registry order.
pub const CHECK_IDS: [&str; 3] = [GSC_ORPHAN, NO_SEARCH_DATA, NON_INDEXABLE_WITH_SEARCH_DATA];

/// Check id → its Search Console verdict label (`INSPECTION_CHECKS`).
pub const INSPECTION_CHECKS: [(&str, &str); 6] = [
    (
        "GSC_INDEXED_WITH_ISSUES",
        "URL is on Google, But Has Issues",
    ),
    ("GSC_AMP_INVALID", "AMP URL is Invalid"),
    ("GSC_RICH_RESULTS_INVALID", "Rich Result is Invalid"),
    ("GSC_INDEXABLE_NOT_INDEXED", "Indexable URL Not Indexed"),
    (
        "GSC_USER_CANONICAL_NOT_SELECTED",
        "User-Declared Canonical Not Selected",
    ),
    ("GSC_URL_NOT_ON_GOOGLE", "URL Is Not on Google"),
];

/// The skip reason when no GSC document was supplied at all.
pub const NO_EVIDENCE_REASON: &str = "no Search Console evidence";

/// `_public_url`: scheme, host and path only — query values and userinfo stay out of findings.
pub fn public_url(value: &str) -> String {
    let Ok(parts) = spiderhead_parse::py_url::urlsplit(value) else {
        return value.to_owned();
    };
    let (host, raw_port) = hostinfo(&parts.netloc);
    let mut host = host.to_lowercase();
    if host.contains(':') && !host.starts_with('[') {
        host = format!("[{host}]");
    }
    let port = raw_port
        .and_then(|p| p.parse::<i64>().ok())
        .filter(|p| (0..=65535).contains(p));
    let netloc = match port {
        Some(p) => format!("{host}:{p}"),
        None => host,
    };
    spiderhead_parse::py_url::urlunsplit(&parts.scheme, &netloc, &parts.path, "", "")
}

/// Host and raw port string, split from the authority part.
fn hostinfo(netloc: &str) -> (String, Option<String>) {
    let host = netloc.rsplit('@').next().unwrap_or_default();
    match host.find('[') {
        Some(open) => {
            let bracketed = &host[open + 1..];
            let (hostname, rest) = match bracketed.split_once(']') {
                Some((h, p)) => (h.to_owned(), p),
                None => (bracketed.to_owned(), ""),
            };
            let port = rest.split_once(':').map(|(_, p)| p.to_owned());
            (hostname, port)
        }
        None => match host.split_once(':') {
            Some((h, p)) => (h.to_owned(), Some(p.to_owned())),
            None => (host.to_owned(), None),
        },
    }
}

/// `_indexable`: the page's boolean indexable flag, else its Indexability text, else unknown.
fn indexable(page: &Value) -> Option<bool> {
    if let Some(Value::Bool(flag)) = page.get("indexable") {
        return Some(*flag);
    }
    match page.get("indexability").and_then(Value::as_str) {
        Some(state) if !state.trim().is_empty() => {
            Some(state.trim().eq_ignore_ascii_case("indexable"))
        }
        _ => None,
    }
}

/// `_impressions`: the measured impressions total over normalized metric rows.
/// A `measured` entry without a numeric `value` is a `KeyError`/`TypeError`, like `+=`.
fn impressions(rows: &[&Value]) -> Result<f64, CfError> {
    let mut total = 0.0;
    for row in rows {
        let entry = &row["metrics"]["impressions"];
        if entry.get("state").and_then(Value::as_str) == Some("measured") {
            let value = entry
                .get("value")
                .ok_or_else(|| CfError::Python("KeyError", "'value'".into()))?;
            total += match value {
                Value::Bool(flag) => f64::from(u8::from(*flag)),
                other => other.as_f64().ok_or_else(|| {
                    CfError::Python(
                        "TypeError",
                        format!(
                            "unsupported operand type(s) for +=: 'float' and '{}'",
                            pystr::type_name(other)
                        ),
                    )
                })?,
            };
        }
    }
    Ok(total)
}

/// `_origin`: scheme plus netloc of a join key.
fn origin_of(key: &str) -> String {
    spiderhead_parse::py_url::urlsplit(key)
        .map(|p| format!("{}://{}", p.scheme, p.netloc))
        .unwrap_or_else(|_| format!("://{key}"))
}

/// `evaluate`: `{check_id: [{url, details}], "_skipped": {check_id: reason}}`.
pub fn evaluate(
    crawl_pages: &[Value],
    gsc_document: Option<&Value>,
    crawl_partial: bool,
    window_complete: bool,
) -> Result<Value, CfError> {
    let mut findings = Map::new();
    for check in CHECK_IDS {
        findings.insert(check.to_owned(), json!([]));
    }
    let Some(document) = gsc_document else {
        let skipped: Map<String, Value> = CHECK_IDS
            .iter()
            .map(|c| ((*c).to_owned(), json!(NO_EVIDENCE_REASON)))
            .collect();
        findings.insert("_skipped".into(), Value::Object(skipped));
        return Ok(Value::Object(findings));
    };
    let join = join_evidence(crawl_pages, document, None, None)?;
    let matched = join["matched"].as_array().cloned().unwrap_or_default();
    let crawl_only = join["crawl_only"].as_array().cloned().unwrap_or_default();
    let external_only = join["external_only"]
        .as_array()
        .cloned()
        .unwrap_or_default();

    let mut skipped = Map::new();
    if crawl_partial || !window_complete {
        let reason = if crawl_partial {
            "crawl is partial"
        } else {
            "reporting window is incomplete"
        };
        skipped.insert(GSC_ORPHAN.into(), json!(reason));
        skipped.insert(NO_SEARCH_DATA.into(), json!(reason));
    }
    let empty = Value::Object(Map::new());
    let collection = document
        .get("mapping")
        .and_then(|m| m.get("collection"))
        .unwrap_or(&empty);
    let mut degraded: Vec<String> = ["sampled", "thresholded", "truncated"]
        .iter()
        .filter(|flag| pystr::truthy(collection.get(flag)))
        .map(|flag| (*flag).to_owned())
        .collect();
    match collection.get("state") {
        Some(Value::String(state)) if state != "complete" => degraded.push(state.clone()),
        None | Some(Value::Null) | Some(Value::String(_)) => {}
        Some(other) => {
            return Err(CfError::Python(
                "TypeError",
                format!(
                    "sequence item {}: expected str instance, {} found",
                    degraded.len(),
                    pystr::type_name(other)
                ),
            ))
        }
    }
    if !degraded.is_empty() {
        skipped
            .entry(NO_SEARCH_DATA.to_owned())
            .or_insert_with(|| json!(format!("collection coverage is {}", degraded.join(", "))));
    }
    let unknown = crawl_pages
        .iter()
        .filter(|page| indexable(page).is_none())
        .count();
    if unknown > 0 {
        let reason = format!("crawl indexability missing on {unknown} pages");
        skipped
            .entry(NO_SEARCH_DATA.to_owned())
            .or_insert_with(|| json!(reason.clone()));
        skipped
            .entry(NON_INDEXABLE_WITH_SEARCH_DATA.to_owned())
            .or_insert_with(|| json!(reason));
    }

    if !skipped.contains_key(GSC_ORPHAN) {
        let mut origins: BTreeSet<String> = BTreeSet::new();
        for entry in matched.iter().chain(crawl_only.iter()) {
            if let Some(key) = entry.get("key").and_then(Value::as_str) {
                origins.insert(origin_of(key));
            }
        }
        if origins.is_empty() {
            skipped.insert(
                GSC_ORPHAN.into(),
                json!("no crawled origin to scope orphans against"),
            );
        } else {
            let mut impression_map: Map<String, Value> = Map::new();
            for entry in &external_only {
                if let Some(key) = entry.get("key").and_then(Value::as_str) {
                    let n = impressions(&[entry.get("row").unwrap_or(&Value::Null)])?;
                    impression_map.insert(key.to_owned(), json!(n));
                }
            }
            let external: Vec<Value> = impression_map
                .iter()
                .filter(|(_, n)| n.as_f64().unwrap_or(0.0) > 0.0)
                .map(|(key, _)| json!({"url": key}))
                .collect();
            let scoped = json!({
                "external_only": external,
                "crawl_origins": origins.iter().cloned().collect::<Vec<_>>(),
            });
            let orphans = orphan_urls(&scoped, "url", None)?;
            let found: Vec<Value> = orphans
                .iter()
                .map(|url| {
                    json!({
                        "url": url,
                        "details": {"impressions": impression_map[url.as_str()].clone()},
                    })
                })
                .collect();
            findings.insert(GSC_ORPHAN.into(), Value::Array(found));
        }
    }

    if !skipped.contains_key(NO_SEARCH_DATA) {
        let found: Vec<Value> = crawl_only
            .iter()
            .filter(|entry| indexable(&entry["page"]) == Some(true))
            .map(|entry| json!({"url": entry["url"].clone(), "details": {"impressions": 0}}))
            .collect();
        findings.insert(NO_SEARCH_DATA.into(), Value::Array(found));
    }

    if !skipped.contains_key(NON_INDEXABLE_WITH_SEARCH_DATA) {
        let mut totals: BTreeMap<String, f64> = BTreeMap::new();
        for entry in &matched {
            if let Some(key) = entry.get("key").and_then(Value::as_str) {
                let rows: Vec<&Value> = entry["rows"]
                    .as_array()
                    .map(|a| a.iter().collect())
                    .unwrap_or_default();
                totals.insert(key.to_owned(), impressions(&rows)?);
            }
        }
        let mut seen: BTreeSet<String> = BTreeSet::new();
        let mut found = Vec::new();
        for entry in &matched {
            let key = entry.get("key").and_then(Value::as_str).unwrap_or("");
            if seen.contains(key)
                || indexable(&entry["page"]) != Some(false)
                || totals.get(key).copied().unwrap_or(0.0) <= 0.0
            {
                continue;
            }
            seen.insert(key.to_owned());
            found.push(json!({
                "url": entry["url"].clone(),
                "details": {
                    "impressions": totals[key],
                    "crawl_partial": crawl_partial,
                },
            }));
        }
        findings.insert(NON_INDEXABLE_WITH_SEARCH_DATA.into(), Value::Array(found));
    }

    findings.insert("_skipped".into(), Value::Object(skipped));
    Ok(Value::Object(findings))
}

/// `evaluate_inspections`: map the bounded URL Inspection rows to six positive
/// sample findings; no unsampled URL is inferred clean.
pub fn evaluate_inspections(
    crawl_pages: &[Value],
    inspections: &[Value],
    requested: i64,
    deferred: i64,
) -> Value {
    let pages: Map<String, Value> = crawl_pages
        .iter()
        .filter_map(|p| {
            p.get("url")
                .and_then(Value::as_str)
                .map(|u| (u.to_owned(), p.clone()))
        })
        .collect();
    let rows: Vec<Value> = inspections
        .iter()
        .filter(|item| item.is_object())
        .cloned()
        .collect();
    let mut findings: Map<String, Value> = INSPECTION_CHECKS
        .iter()
        .map(|(name, _)| ((*name).to_owned(), json!([])))
        .collect();
    let mut failed = 0i64;
    let verdicts = ["PASS", "FAIL", "NEUTRAL"];
    for row in &rows {
        let status = row.get("index_status").unwrap_or(&Value::Null);
        let verdict_ok = status
            .get("verdict")
            .and_then(Value::as_str)
            .is_some_and(|v| verdicts.contains(&v));
        if row.get("state").and_then(Value::as_str) != Some("complete")
            || row.get("url").and_then(Value::as_str).is_none()
            || !status.is_object()
            || !verdict_ok
        {
            failed += 1;
            continue;
        }
        let url = row["url"].as_str().unwrap_or_default().to_owned();
        let verdict = status["verdict"].as_str().unwrap_or_default();
        let amp_verdict = row["amp_result"].get("verdict").and_then(Value::as_str);
        let rich_verdict = row["rich_results_result"]
            .get("verdict")
            .and_then(Value::as_str);
        let inspected_at = row.get("inspected_at").cloned().unwrap_or(Value::Null);
        let add = |findings: &mut Map<String, Value>, check: &str, details: Map<String, Value>| {
            let mut details = details;
            details.insert("inspected_at".into(), inspected_at.clone());
            if let Some(list) = findings.get_mut(check).and_then(Value::as_array_mut) {
                list.push(json!({"url": url.as_str(), "details": Value::Object(details)}));
            }
        };
        if verdict == "PASS" && (amp_verdict == Some("FAIL") || rich_verdict == Some("FAIL")) {
            add(
                &mut findings,
                "GSC_INDEXED_WITH_ISSUES",
                Map::from_iter([
                    ("amp_verdict".into(), json!(amp_verdict)),
                    ("rich_results_verdict".into(), json!(rich_verdict)),
                ]),
            );
        }
        if amp_verdict == Some("FAIL") {
            add(
                &mut findings,
                "GSC_AMP_INVALID",
                Map::from_iter([("amp_verdict".into(), json!(amp_verdict))]),
            );
        }
        if rich_verdict == Some("FAIL") {
            add(
                &mut findings,
                "GSC_RICH_RESULTS_INVALID",
                Map::from_iter([("rich_results_verdict".into(), json!(rich_verdict))]),
            );
        }
        let crawl_indexable = indexable(pages.get(url.as_str()).unwrap_or(&Value::Null));
        if crawl_indexable == Some(true) && (verdict == "FAIL" || verdict == "NEUTRAL") {
            add(
                &mut findings,
                "GSC_INDEXABLE_NOT_INDEXED",
                Map::from_iter([
                    ("verdict".into(), json!(verdict)),
                    (
                        "coverage_state".into(),
                        status.get("coverageState").cloned().unwrap_or(Value::Null),
                    ),
                ]),
            );
        }
        let user_canonical = status.get("userCanonical").and_then(Value::as_str);
        let google_canonical = status.get("googleCanonical").and_then(Value::as_str);
        if let (Some(user), Some(google)) = (user_canonical, google_canonical) {
            if !user.is_empty()
                && !google.is_empty()
                && canonical_key(user) != canonical_key(google)
            {
                add(
                    &mut findings,
                    "GSC_USER_CANONICAL_NOT_SELECTED",
                    Map::from_iter([
                        ("user_canonical".into(), json!(user)),
                        ("google_canonical".into(), json!(google)),
                        ("safe_user_canonical".into(), json!(public_url(user))),
                        ("safe_google_canonical".into(), json!(public_url(google))),
                    ]),
                );
            }
        }
        if verdict == "FAIL" || verdict == "NEUTRAL" {
            add(
                &mut findings,
                "GSC_URL_NOT_ON_GOOGLE",
                Map::from_iter([
                    ("verdict".into(), json!(verdict)),
                    (
                        "coverage_state".into(),
                        status.get("coverageState").cloned().unwrap_or(Value::Null),
                    ),
                ]),
            );
        }
    }
    json!({
        "scope": "bounded Search Console URL Inspection sample; not a site-wide census",
        "requested": requested,
        "inspected": rows.len() as i64 - failed,
        "failed": failed,
        "deferred": deferred,
        "sampled": !rows.is_empty(),
        "reason": if rows.is_empty() { Value::from("no retained URL Inspection sample") } else { Value::Null },
        "findings": Value::Object(findings),
        "sf_checks": Map::from_iter(INSPECTION_CHECKS.iter().map(|(k, v)| ((*k).to_owned(), json!(*v)))),
    })
}

/// `normalize_join_key` on one canonical URL for the declaration comparison.
fn canonical_key(url: &str) -> String {
    normalize_join_key(Some(&json!(url)), false, false, false)
        .ok()
        .flatten()
        .unwrap_or_default()
}
