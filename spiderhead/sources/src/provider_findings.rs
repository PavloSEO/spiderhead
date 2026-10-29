//! Join retained project provider history to one audit run without fetching sources.
//!
//! Provider history is an independent measurement: collection gaps stay `partial` and measured
//! per-URL findings enter the registered issue stream. Every project-bound read arrives through
//! [`ProviderOps`]; the checks layer turns the returned evidence document into issues.

use indexmap::IndexMap;
use serde_json::{json, Map, Value};
use sha2::{Digest, Sha256};

use crate::analytics_findings::{analytics_findings, FindingOptions};
use crate::cloudflare::CfError;
use crate::evidence_import::{normalize_inline, MAPPING_FORMAT};
use crate::external_join::{default_key_fn, join_external_data};
use crate::pystr;
use crate::pytime;
use crate::search_console_findings::{evaluate, evaluate_inspections, public_url};

/// Page cap feeding the provider joins.
pub const MAX_PROVIDER_ROWS: i64 = 100_000;
/// Per-finding URL cap inside one provider source.
pub const MAX_FINDING_URLS: usize = 500;
/// Crawl pages above this count are dropped from the provider input.
pub const MAX_CRAWL_PAGES: usize = 100_000;
/// Issues emitted per provider check inside the audit stream.
pub const MAX_ISSUES_PER_PROVIDER_CHECK: usize = 500;

/// GSC search-analytics check ids, in registry order.
pub const GSC_ANALYTICS_CHECKS: [&str; 3] = [
    "GSC_ORPHAN",
    "NO_SEARCH_DATA",
    "NON_INDEXABLE_WITH_SEARCH_DATA",
];
/// GSC URL-Inspection check ids, in registry order.
pub const GSC_INSPECTION_CHECKS: [&str; 6] = [
    "GSC_INDEXED_WITH_ISSUES",
    "GSC_AMP_INVALID",
    "GSC_RICH_RESULTS_INVALID",
    "GSC_INDEXABLE_NOT_INDEXED",
    "GSC_USER_CANONICAL_NOT_SELECTED",
    "GSC_URL_NOT_ON_GOOGLE",
];

/// `analytics_findings` source check → audit issue id, per provider.
pub const ANALYTICS_CHECKS: [(&str, [(&str, &str); 4]); 2] = [
    (
        "ga4",
        [
            ("GA_ORPHAN_URL", "GA_ORPHAN_URL"),
            ("NO_GA_DATA", "NO_GA_DATA"),
            ("NON_INDEXABLE_WITH_GA_DATA", "NON_INDEXABLE_WITH_GA_DATA"),
            ("BOUNCE_RATE_ABOVE_70", "BOUNCE_RATE_ABOVE_70"),
        ],
    ),
    (
        "metrika",
        [
            ("GA_ORPHAN_URL", "METRIKA_ORPHAN_URL"),
            ("NO_GA_DATA", "NO_METRIKA_DATA"),
            (
                "NON_INDEXABLE_WITH_GA_DATA",
                "NON_INDEXABLE_WITH_METRIKA_DATA",
            ),
            ("BOUNCE_RATE_ABOVE_70", "METRIKA_BOUNCE_RATE_ABOVE_70"),
        ],
    ),
];

/// `_url_id`: `url:v1:` plus the first 24 hex of the URL's SHA-256.
pub fn url_id(value: &str) -> String {
    format!(
        "url:v1:{:.24}",
        format!("{:x}", Sha256::digest(value.as_bytes()))
    )
}

/// `_url`: an absolute http(s) URL, or an origin-relative path joined to `origin`.
/// `urlsplit` failures propagate as errors, not like an unreadable row.
fn url_of(value: Option<&Value>, origin: Option<&str>) -> Result<Option<String>, CfError> {
    let Some(text) = value.and_then(Value::as_str).map(str::trim) else {
        return Ok(None);
    };
    if text.is_empty() {
        return Ok(None);
    }
    let parts = spiderhead_parse::py_url::urlsplit(text)
        .map_err(|e| CfError::Python("ValueError", format!("{e}")))?;
    if (parts.scheme == "http" || parts.scheme == "https") && !parts.netloc.is_empty() {
        return Ok(Some(text.to_owned()));
    }
    if let Some(origin) = origin {
        if text.starts_with('/') {
            return Ok(Some(format!("{origin}{text}")));
        }
    }
    Ok(None)
}

/// `_history_source`: the one binding for `service` in a usable state, else `None`.
/// More than one match is as unusable as none.
pub fn history_source<'a>(bindings: &'a [Value], service: &str) -> Option<&'a Value> {
    let matches: Vec<&Value> = bindings
        .iter()
        .filter(|item| {
            item.get("service").and_then(Value::as_str) == Some(service)
                && matches!(
                    item.get("status").and_then(Value::as_str),
                    Some("selected" | "available" | "stale" | "unavailable" | "no_access")
                )
        })
        .collect();
    (matches.len() == 1).then(|| matches[0])
}

/// Reads a stored count as an integer.
fn int_of(value: &Value) -> Result<i64, CfError> {
    let err = || {
        CfError::Python(
            "ValueError",
            format!(
                "invalid literal for int() with base 10: {}",
                pystr::repr(value)
            ),
        )
    };
    match value {
        Value::Bool(flag) => Ok(i64::from(*flag)),
        Value::Number(n) => Ok(n.as_f64().unwrap_or(0.0).trunc() as i64),
        Value::String(text) => text.trim().parse::<i64>().map_err(|_| err()),
        _ => Err(err()),
    }
}

/// `_analytics_rows` output: normalized rows, the visit column name, the resolved
/// bounce column, and the count of rows dropped as unusable.
type AnalyticsRows = (Vec<Value>, &'static str, Option<String>, i64);

/// `_analytics_rows`: `(rows, visits_column, bounce_column, unreadable_rows)` — per-URL
/// visit sums and weighted bounce fractions for GA4/Metrika retained history.
fn analytics_rows(
    source: &str,
    history: &Value,
    origin: Option<&str>,
) -> Result<AnalyticsRows, CfError> {
    let visit_column = if source == "ga4" {
        "sessions"
    } else {
        "visits"
    };
    let nonneg_number =
        |v: &Value| v.is_number() && !v.is_boolean() && v.as_f64().unwrap_or(-1.0) >= 0.0;
    let mut totals: IndexMap<String, Map<String, Value>> = IndexMap::new();
    let mut unreadable = 0i64;
    for row in history["rows"]
        .as_array()
        .map(Vec::as_slice)
        .unwrap_or_default()
    {
        let Some(url) = url_of(row.get("landing_page"), origin)? else {
            unreadable += 1;
            continue;
        };
        let item = totals.entry(url.clone()).or_insert_with(|| {
            Map::from_iter([
                ("url".into(), json!(url)),
                (visit_column.into(), json!(0.0)),
                ("visits_seen".into(), json!(false)),
                ("visits_unreadable".into(), json!(false)),
                ("bounce_weighted".into(), json!(0.0)),
                ("bounce_weight".into(), json!(0.0)),
                ("bounce_unreadable".into(), json!(false)),
            ])
        });
        let visits = row.get(visit_column).cloned().unwrap_or(Value::Null);
        if nonneg_number(&visits) {
            let n = visits.as_f64().unwrap_or(0.0);
            item[visit_column] = json!(item[visit_column].as_f64().unwrap_or(0.0) + n);
            item["visits_seen"] = json!(true);
            let bounce = row.get("bounce_rate").cloned().unwrap_or(Value::Null);
            let readable = source == "metrika"
                && bounce.is_number()
                && !bounce.is_boolean()
                && (0.0..=1.0).contains(&bounce.as_f64().unwrap_or(-1.0))
                && n > 0.0;
            if readable {
                let b = bounce.as_f64().unwrap_or(0.0);
                item["bounce_weighted"] =
                    json!(item["bounce_weighted"].as_f64().unwrap_or(0.0) + b * n);
                item["bounce_weight"] = json!(item["bounce_weight"].as_f64().unwrap_or(0.0) + n);
            } else if source == "metrika" && n > 0.0 {
                item["bounce_unreadable"] = json!(true);
            }
        } else {
            item["visits_unreadable"] = json!(true);
            unreadable += 1;
        }
    }
    let mut rows = Vec::new();
    for (_, item) in &totals {
        let mut row = Map::from_iter([
            ("url".into(), item["url"].clone()),
            (
                visit_column.into(),
                if item["visits_seen"] == json!(true) && item["visits_unreadable"] != json!(true) {
                    item[visit_column].clone()
                } else {
                    Value::Null
                },
            ),
        ]);
        if source == "metrika" {
            let weight = item["bounce_weight"].as_f64().unwrap_or(0.0);
            let bounce = if weight != 0.0 && item["bounce_unreadable"] != json!(true) {
                json!(item["bounce_weighted"].as_f64().unwrap_or(0.0) / weight)
            } else {
                Value::Null
            };
            if item["bounce_unreadable"] == json!(true) {
                unreadable += 1;
            }
            row.insert("bounce_rate".into(), bounce);
        }
        rows.push(Value::Object(row));
    }
    let bounce_column = if source == "metrika"
        && rows
            .iter()
            .any(|r| r.get("bounce_rate").is_some_and(|v| !v.is_null()))
    {
        Some("bounce_rate".to_owned())
    } else {
        None
    };
    Ok((rows, visit_column, bounce_column, unreadable))
}

/// `_cap_findings`: cap each finding's URL list at `MAX_FINDING_URLS`; reports `truncated`.
fn cap_findings(findings: &Map<String, Value>) -> (Map<String, Value>, bool) {
    let mut truncated = false;
    let mut result = Map::new();
    for (check, item) in findings {
        match item {
            Value::Array(list) if list.len() > MAX_FINDING_URLS => {
                result.insert(check.clone(), json!(list[..MAX_FINDING_URLS]));
                truncated = true;
            }
            Value::Object(map) => {
                let urls = map.get("urls").and_then(Value::as_array);
                match urls {
                    Some(urls) if urls.len() > MAX_FINDING_URLS => {
                        let mut item = map.clone();
                        item.insert("urls".into(), json!(urls[..MAX_FINDING_URLS]));
                        item.insert("omitted_urls".into(), json!(urls.len() - MAX_FINDING_URLS));
                        for key in ["safe_urls", "url_ids"] {
                            if let Some(values) = item.get(key).and_then(Value::as_array) {
                                let capped: Vec<Value> =
                                    values[..MAX_FINDING_URLS.min(values.len())].to_vec();
                                item.insert(key.into(), json!(capped));
                            }
                        }
                        result.insert(check.clone(), Value::Object(item));
                        truncated = true;
                    }
                    _ => {
                        result.insert(check.clone(), item.clone());
                    }
                }
            }
            other => {
                result.insert(check.clone(), other.clone());
            }
        }
    }
    (result, truncated)
}

/// Add `safe_url`/`url_id` to every `{"url": ...}` item in every list value of `findings`.
fn add_safe_urls(findings: &mut Map<String, Value>) {
    for item in findings.values_mut() {
        let Some(list) = item.as_array_mut() else {
            continue;
        };
        for entry in list.iter_mut() {
            let url = entry.get("url").and_then(Value::as_str).map(str::to_owned);
            if let Some(url) = url {
                entry["safe_url"] = json!(public_url(&url));
                entry["url_id"] = json!(url_id(&url));
            }
        }
    }
}

/// `_gsc_findings`: normalize retained search-analytics rows to a GSC evidence document
/// and evaluate the three analytics checks over the crawl join.
fn gsc_findings(
    pages: &[Value],
    history: &Value,
    resource: &str,
    crawl_partial: bool,
) -> Result<Value, CfError> {
    let coverage_complete = pystr::truthy(history.get("coverage_complete"));
    let manifest = json!({
        "format": MAPPING_FORMAT,
        "source": {
            "kind": "provider_evidence",
            "provider": "gsc",
            "operation": "search_analytics",
            "reporting_identity": resource,
            "privacy": "restricted",
            "search_engine": "google",
            "search_type": "web",
            "timezone": "America/Los_Angeles",
        },
        "period": {
            "start_date": history["period"]["from"],
            "end_date": history["period"]["to"],
        },
        "url": {"field": "page", "kind": "absolute"},
        "metrics": [
            {"name": "clicks", "type": "number", "unit": "count"},
            {"name": "impressions", "type": "number", "unit": "count"},
            {"name": "ctr", "type": "number", "unit": "ratio"},
            {"name": "position", "type": "number", "unit": "position"},
        ],
        "dimensions": ["date", "query", "page"],
        "collection": {
            "state": if coverage_complete { "complete" } else { "partial" },
            "sampled": false,
            "thresholded": false,
            "truncated": pystr::truthy(history.get("truncated")),
            "reason": if coverage_complete { Value::Null } else {
                json!("retained GSC coverage has gaps or is truncated")
            },
        },
        "duplicate_policy": "mark",
        "row_shape": "flat",
    });
    let projected: Vec<Value> = history["rows"]
        .as_array()
        .map(Vec::as_slice)
        .unwrap_or_default()
        .iter()
        .map(|row| {
            json!({
                "page": row.get("page"),
                "query": row.get("query"),
                "date": row.get("date"),
                "clicks": row.get("clicks"),
                "impressions": row.get("impressions"),
                "ctr": row.get("ctr"),
                "position": row.get("position"),
            })
        })
        .collect();
    let evidence = normalize_inline(&projected, Some(&manifest))?;
    let mut result = evaluate(pages, Some(&evidence), crawl_partial, coverage_complete)?;
    let Some(map) = result.as_object_mut() else {
        return Ok(result);
    };
    add_safe_urls(map);
    let (findings, truncated) = cap_findings(map);
    let coverage: Map<String, Value> = history
        .as_object()
        .map(|m| {
            m.iter()
                .filter(|(key, _)| key.as_str() != "rows")
                .map(|(k, v)| (k.clone(), v.clone()))
                .collect()
        })
        .unwrap_or_default();
    Ok(json!({
        "schema_version": "search_console_findings.v1",
        "findings": Value::Object(findings),
        "coverage": Value::Object(coverage),
        "truncated_findings": truncated,
    }))
}

/// `_inspection_urls`: a deterministic ≤50-URL sample of crawled pages the retained
/// history has not completed, strided when larger.
fn inspection_urls(pages: &[Value], existing: &[Value]) -> Vec<String> {
    let mut candidates: Vec<String> = pages
        .iter()
        .filter_map(|page| page.get("url").and_then(Value::as_str))
        .filter(|url| {
            !existing.iter().any(|item| {
                item.get("state").and_then(Value::as_str) == Some("complete")
                    && item.get("url").and_then(Value::as_str) == Some(*url)
            })
        })
        .map(str::to_owned)
        .collect();
    candidates.sort();
    candidates.dedup();
    if candidates.len() <= 50 {
        return candidates;
    }
    (0..50)
        .map(|index| candidates[index * (candidates.len() - 1) / 49].clone())
        .collect()
}

/// The `settle` seam of [`InspectionCollect`]: reservation id, stored rows, request count.
type SettleFn<'a> = &'a dyn Fn(&Value, &[Value], i64) -> Result<Value, ()>;

/// Live URL Inspection collection seams — needed only when `inspect_gsc` is set.
/// Every call is a project-bound read or a provider request the caller resolved.
pub struct InspectionCollect<'a> {
    /// `reserve(site_url, wanted) -> {"reservation_id", "urls", "reserved", "deferred", ...}`.
    pub reserve: &'a dyn Fn(&str, &[String]) -> Value,
    /// `inspect(site_url, urls) -> {"ok", "state", "requests_made", "urls"}`; the caller
    /// turns a raised failure into `{"ok": false, "state": "failed", "urls": []}`.
    pub inspect: &'a dyn Fn(&str, &[String]) -> Value,
    /// `settle(reservation_id, rows, requests_made) -> {"state", ...}`; `Err` is a write fault.
    pub settle: SettleFn<'a>,
}

/// `_gsc_inspections`: the bounded URL Inspection sample over retained rows, plus the
/// live collection flow when `collect` and `ops` are both present.
fn gsc_inspections(
    pages: &[Value],
    site_url: &str,
    collect: bool,
    latest: &dyn Fn(&str) -> Result<Vec<Value>, ()>,
    ops: Option<&InspectionCollect>,
) -> Result<Value, CfError> {
    let mut rows = latest(site_url).unwrap_or_default();
    let mut collection_state = if rows.is_empty() {
        json!("not_requested")
    } else {
        json!("cached")
    };
    let mut quota: Option<Map<String, Value>> = None;
    let mut requests_made = 0i64;
    if collect {
        if let Some(ops) = ops {
            let wanted = inspection_urls(pages, &rows);
            let reservation = (ops.reserve)(site_url, &wanted);
            quota = Some(
                reservation
                    .as_object()
                    .map(|m| {
                        m.iter()
                            .filter(|(k, _)| {
                                !["reservation_id", "site_url", "urls"].contains(&k.as_str())
                            })
                            .map(|(k, v)| (k.clone(), v.clone()))
                            .collect()
                    })
                    .unwrap_or_default(),
            );
            let reserved_urls: Vec<String> = reservation["urls"]
                .as_array()
                .map(|a| {
                    a.iter()
                        .filter_map(Value::as_str)
                        .map(str::to_owned)
                        .collect()
                })
                .unwrap_or_default();
            if !reserved_urls.is_empty() {
                let result = (ops.inspect)(site_url, &reserved_urls);
                requests_made = match result.get("requests_made") {
                    Some(v) if pystr::truthy(Some(v)) => int_of(v)?,
                    _ => 0,
                };
                let mut settlement: Option<Value> = None;
                let mut settlement_failed = false;
                if pystr::truthy(reservation.get("reservation_id")) {
                    let result_urls: Vec<Value> =
                        result["urls"].as_array().cloned().unwrap_or_default();
                    match (ops.settle)(&reservation["reservation_id"], &result_urls, requests_made)
                    {
                        Ok(value) => settlement = Some(value),
                        Err(()) => settlement_failed = true,
                    }
                }
                collection_state = if settlement_failed {
                    json!("history_write_failed")
                } else if result.get("state").and_then(Value::as_str) == Some("not_configured") {
                    json!("not_configured")
                } else if settlement
                    .as_ref()
                    .and_then(|s| s.get("state"))
                    .and_then(Value::as_str)
                    == Some("history_full")
                {
                    json!("history_full")
                } else if requests_made > 0 {
                    if result.get("state").and_then(Value::as_str) == Some("complete") {
                        json!("collected")
                    } else {
                        json!("partial")
                    }
                } else {
                    match result.get("state") {
                        Some(v) if pystr::truthy(Some(v)) => v.clone(),
                        _ => json!("failed"),
                    }
                };
                rows = match latest(site_url) {
                    Ok(rows) => rows,
                    Err(()) => result["urls"].as_array().cloned().unwrap_or_default(),
                };
            } else if wanted.is_empty() {
                collection_state = json!("cached");
            } else {
                collection_state = json!("quota_limited");
            }
        }
    }
    let mut summary = evaluate_inspections(
        pages,
        &rows,
        quota
            .as_ref()
            .and_then(|q| q.get("reserved"))
            .and_then(Value::as_i64)
            .unwrap_or(0),
        quota
            .as_ref()
            .and_then(|q| q.get("deferred"))
            .and_then(Value::as_i64)
            .unwrap_or(0),
    );
    summary["history_urls"] = json!(rows.len());
    let mut crawl_urls: Vec<&str> = pages
        .iter()
        .filter_map(|p| p.get("url").and_then(Value::as_str))
        .collect();
    crawl_urls.sort();
    crawl_urls.dedup();
    let verdicts = ["PASS", "FAIL", "NEUTRAL"];
    let mut inspected_urls: Vec<&str> = rows
        .iter()
        .filter(|row| {
            row.get("state").and_then(Value::as_str) == Some("complete")
                && row.get("index_status").is_some_and(|s| s.is_object())
                && row["index_status"]
                    .get("verdict")
                    .and_then(Value::as_str)
                    .is_some_and(|v| verdicts.contains(&v))
                && row
                    .get("url")
                    .and_then(Value::as_str)
                    .is_some_and(|u| crawl_urls.contains(&u))
        })
        .filter_map(|row| row.get("url").and_then(Value::as_str))
        .collect();
    inspected_urls.sort();
    inspected_urls.dedup();
    summary["crawl_pages"] = json!(crawl_urls.len());
    summary["crawl_urls_inspected"] = json!(inspected_urls.len());
    summary["sample_population_complete"] =
        json!(!crawl_urls.is_empty() && inspected_urls == crawl_urls);
    if let Some(findings) = summary["findings"].as_object_mut() {
        add_safe_urls(findings);
        let (capped, truncated) = cap_findings(findings);
        summary["findings"] = Value::Object(capped);
        summary["truncated_findings"] = json!(truncated);
    }
    let mut out = Map::from_iter([
        ("state".into(), collection_state),
        (
            "coverage".into(),
            json!("bounded URL sample; unsampled pages are unknown"),
        ),
        (
            "daily_budget".into(),
            quota.map(Value::Object).unwrap_or(Value::Null),
        ),
        ("requests_made".into(), json!(requests_made)),
    ]);
    if let Value::Object(map) = summary {
        out.extend(map);
    }
    Ok(Value::Object(out))
}

/// `_cloudflare_history`: judge the one retained aggregate snapshot against `as_of`
/// (epoch days) and return the sources entry.
fn cloudflare_history(snapshots: &Value, as_of: i64) -> Value {
    let rows = snapshots["rows"].as_array().cloned().unwrap_or_default();
    let Some(snapshot) = rows.first() else {
        return json!({"state": "unavailable", "reason": "no retained Cloudflare aggregate snapshot"});
    };
    let facts = snapshot["url_facts"]
        .as_array()
        .cloned()
        .unwrap_or_default();
    let mut reasons: Vec<String> = Vec::new();
    let observed = snapshot.get("observed_at").cloned().unwrap_or(Value::Null);
    let mut age_days = Value::Null;
    match observed.as_str() {
        None => reasons.push("snapshot timestamp is missing or invalid".into()),
        Some(text) => match pytime::fromisoformat_offset(&text.replace('Z', "+00:00")) {
            Some((stamp, _)) if stamp.aware => {
                let observed_date = stamp.micros.div_euclid(86_400_000_000);
                let days = as_of - observed_date;
                age_days = json!(days);
                if days < 0 {
                    reasons.push("snapshot timestamp is in the future".into());
                } else if days > 28 {
                    reasons.push("latest Cloudflare snapshot is older than 28 days".into());
                }
            }
            _ => reasons.push("snapshot timestamp is missing or invalid".into()),
        },
    }
    let period = snapshot.get("period").cloned().unwrap_or(Value::Null);
    let mut native_period_days = Value::Null;
    let mut data_age_days = Value::Null;
    match (
        period.get("from").and_then(Value::as_str),
        period.get("to").and_then(Value::as_str),
    ) {
        (Some(from), Some(to)) => match (
            pytime::date_fromisoformat(from),
            pytime::date_fromisoformat(to),
        ) {
            (Some(start), Some(end)) => {
                let span = end - start + 1;
                if span < 1 {
                    reasons.push("snapshot has no valid native reporting interval".into());
                } else {
                    native_period_days = json!(span);
                    let data_age = as_of - end;
                    data_age_days = json!(data_age);
                    if data_age < 0 {
                        reasons.push("snapshot reporting interval ends in the future".into());
                    } else if data_age > 28 {
                        reasons
                            .push("snapshot reporting interval ends more than 28 days ago".into());
                    }
                }
            }
            _ => reasons.push("snapshot has no valid native reporting interval".into()),
        },
        _ => reasons.push("snapshot has no valid native reporting interval".into()),
    }
    let int_nonneg = |v: Option<&Value>| -> i64 {
        v.filter(|v| pystr::is_py_int(v))
            .and_then(Value::as_i64)
            .filter(|n| *n >= 0)
            .unwrap_or(0)
    };
    let source_omitted = int_nonneg(snapshot.get("url_facts_omitted"));
    let page_omitted = int_nonneg(snapshot.get("url_facts_page_omitted"));
    let omitted =
        source_omitted + page_omitted + (facts.len() as i64 - MAX_FINDING_URLS as i64).max(0);
    let source_truncated =
        pystr::truthy(snapshot.get("url_facts_source_truncated")) || source_omitted > 0;
    if source_truncated || page_omitted > 0 || omitted > 0 {
        reasons.push("retained URL facts are truncated or paged".into());
    }
    if pystr::truthy(snapshot.get("sampled")) || pystr::truthy(snapshot.get("truncated_days")) {
        reasons.push("retained Cloudflare collection was sampled or truncated".into());
    }
    if snapshot.get("state") != Some(&json!("complete")) && reasons.is_empty() {
        reasons.push("retained Cloudflare snapshot is partial".into());
    }
    let state = if snapshot.get("state") == Some(&json!("complete")) && reasons.is_empty() {
        "complete"
    } else {
        "partial"
    };
    json!({
        "state": state,
        "reason": if reasons.is_empty() { Value::Null } else { json!(reasons.join("; ")) },
        "observed_at": observed,
        "age_days": age_days,
        "data_age_days": data_age_days,
        "period": period,
        "period_days": native_period_days,
        "coverage": "one retained aggregate snapshot for this native interval; not a 28-day history",
        "sampled": snapshot.get("sampled").cloned().unwrap_or(Value::Null),
        "truncated_days": if pystr::truthy(snapshot.get("truncated_days")) {
            snapshot.get("truncated_days").cloned().unwrap_or(json!([]))
        } else {
            json!([])
        },
        "url_facts": facts[..facts.len().min(MAX_FINDING_URLS)].to_vec(),
        "omitted_url_facts": omitted,
        "url_facts_truncated": source_truncated || page_omitted > 0 || omitted > 0,
        "fact_scope": "grouped host/path/day counts; no query string, client IP or sub-day timestamp",
    })
}

/// Project-bound reads the orchestration needs. A real project resolves them from
/// `sources_db`, `gsc_inspection_history`, `cloudflare_history` and the binding store;
/// tests resolve them from fixtures. `Err(())` marks an unreadable store.
pub struct ProviderOps<'a> {
    /// `list_bindings(project)["bindings"]`; `None` marks an unreadable binding store.
    pub bindings: Option<&'a [Value]>,
    /// The project site origin (`scheme://netloc`), or `None` when the target is not a URL.
    pub origin: Option<&'a str>,
    /// `(source, resource) -> Ok(history)`; `Err` = no readable retained history.
    pub history: &'a dyn Fn(&str, &str) -> Result<Value, ()>,
    /// `site_url -> Ok(rows)` of retained URL Inspection history; `Err` = unreadable.
    pub inspection_latest: &'a dyn Fn(&str) -> Result<Vec<Value>, ()>,
    /// `binding -> Ok({"rows": [...]})` of `list_snapshots`; `Err` = unreadable.
    pub cloudflare_snapshots: &'a dyn Fn(&Value) -> Result<Value, ()>,
    /// The crawl stopped early or pages were dropped above [`MAX_CRAWL_PAGES`].
    pub crawl_partial: bool,
    /// Pages dropped from the provider input above [`MAX_CRAWL_PAGES`].
    pub crawl_pages_omitted: i64,
    /// Collect a live URL Inspection sample; requires `inspect_ops`.
    pub inspect_gsc: bool,
    /// Live collection seams; used only when `inspect_gsc` is set.
    pub inspect_ops: Option<InspectionCollect<'a>>,
    /// UTC "today" as epoch days for age checks and the inspection `since` window.
    pub today: i64,
}

/// `project_provider_findings`: bounded URL findings from explicitly bound,
/// already-retained history — the `provider_findings.v1` evidence document.
pub fn project_provider_findings(pages: &[Value], ops: &ProviderOps) -> Result<Value, CfError> {
    let Some(bindings) = ops.bindings else {
        return Ok(json!({
            "schema_version": "provider_findings.v1",
            "state": "unavailable",
            "reason": "project bindings are unavailable",
            "sources": {},
        }));
    };
    let mut sources = Map::new();
    for (provider, source_name) in [("gsc", "gsc"), ("ga4", "ga4"), ("metrika", "metrika")] {
        let Some(binding) = history_source(bindings, provider) else {
            sources.insert(
                provider.into(),
                json!({"state": "not_configured", "reason": "no unique selected provider binding"}),
            );
            continue;
        };
        let resource = binding
            .get("resource")
            .and_then(Value::as_str)
            .unwrap_or_default();
        let Ok(history) = (ops.history)(source_name, resource) else {
            sources.insert(
                provider.into(),
                json!({"state": "unavailable", "reason": "no readable retained provider history"}),
            );
            continue;
        };
        if provider == "gsc" {
            let mut result = gsc_findings(pages, &history, resource, ops.crawl_partial)?;
            result["inspection"] = gsc_inspections(
                pages,
                resource,
                ops.inspect_gsc,
                ops.inspection_latest,
                ops.inspect_ops.as_ref(),
            )?;
            result["state"] = json!(if pystr::truthy(history.get("coverage_complete")) {
                "complete"
            } else {
                "partial"
            });
            sources.insert(provider.into(), result);
            continue;
        }
        let (rows, visits_column, bounce_column, unusable_rows) =
            analytics_rows(provider, &history, ops.origin)?;
        let joined = join_external_data(pages, &rows, "url", "url", &default_key_fn)?;
        let coverage_complete = pystr::truthy(history.get("coverage_complete"));
        let evidence_complete = coverage_complete && unusable_rows == 0;
        let evidence_reason = (!evidence_complete).then(|| {
            "retained analytics coverage has gaps, truncation, or unusable URL/metric rows"
                .to_owned()
        });
        let bounce_reason = (provider == "metrika" && bounce_column.is_none()).then(|| {
            "retained Metrika history contains no readable per-URL bounce values".to_owned()
        });
        let mut result = analytics_findings(
            pages,
            &joined,
            &FindingOptions {
                partial: ops.crawl_partial,
                evidence_complete: Some(evidence_complete),
                evidence_reason,
                empty_is_complete: coverage_complete
                    && history.get("row_count").and_then(Value::as_i64) == Some(0),
                visits_column: Some(visits_column.to_owned()),
                bounce_column,
                bounce_reason,
            },
        )?;
        if provider == "metrika" {
            let remap: Map<String, Value> = ANALYTICS_CHECKS[1]
                .1
                .iter()
                .map(|(k, v)| ((*k).to_owned(), json!(*v)))
                .collect();
            let remapped: Map<String, Value> = result["findings"]
                .as_object()
                .map(|m| {
                    m.iter()
                        .map(|(check, finding)| {
                            let name = remap
                                .get(check)
                                .and_then(Value::as_str)
                                .unwrap_or(check)
                                .to_owned();
                            (name, finding.clone())
                        })
                        .collect()
                })
                .unwrap_or_default();
            result["findings"] = Value::Object(remapped);
        }
        let findings_obj = result["findings"].as_object().cloned().unwrap_or_default();
        let (findings, truncated) = cap_findings(&findings_obj);
        result["findings"] = Value::Object(findings);
        if let Some(map) = result["findings"].as_object_mut() {
            for finding in map.values_mut() {
                let Some(item) = finding.as_object_mut() else {
                    continue;
                };
                let Some(urls) = item.get("urls").and_then(Value::as_array).cloned() else {
                    continue;
                };
                let safe: Vec<Value> = urls
                    .iter()
                    .map(|u| {
                        u.as_str()
                            .map(|s| json!(public_url(s)))
                            .unwrap_or_else(|| u.clone())
                    })
                    .collect();
                let ids: Vec<Value> = urls
                    .iter()
                    .map(|u| {
                        u.as_str()
                            .map(url_id)
                            .map(Value::from)
                            .unwrap_or(Value::Null)
                    })
                    .collect();
                item.insert("safe_urls".into(), json!(safe));
                item.insert("url_ids".into(), json!(ids));
            }
        }
        let mut coverage: Map<String, Value> = history
            .as_object()
            .map(|m| {
                m.iter()
                    .filter(|(key, _)| key.as_str() != "rows")
                    .map(|(k, v)| (k.clone(), v.clone()))
                    .collect()
            })
            .unwrap_or_default();
        coverage.insert("unusable_history_rows".into(), json!(unusable_rows));
        result["coverage"] = Value::Object(coverage);
        result["truncated_findings"] = json!(truncated);
        result["state"] = json!(if evidence_complete {
            "complete"
        } else {
            "partial"
        });
        sources.insert(provider.into(), result);
    }
    match history_source(bindings, "cloudflare") {
        None => {
            sources.insert(
                "cloudflare".into(),
                json!({
                    "state": "not_configured",
                    "reason": "no unique selected Cloudflare zone binding",
                }),
            );
        }
        Some(binding) => match (ops.cloudflare_snapshots)(binding) {
            Ok(snapshots) => {
                sources.insert(
                    "cloudflare".into(),
                    cloudflare_history(&snapshots, ops.today),
                );
            }
            Err(()) => {
                sources.insert(
                    "cloudflare".into(),
                    json!({
                        "state": "unavailable",
                        "reason": "retained Cloudflare aggregate history could not be read",
                    }),
                );
            }
        },
    }
    let states: Vec<&str> = sources
        .values()
        .filter_map(|item| item.get("state").and_then(Value::as_str))
        .collect();
    let configured: Vec<&&str> = states.iter().filter(|s| **s != "not_configured").collect();
    let partial = ops.crawl_partial || ops.crawl_pages_omitted > 0;
    let state = if configured.is_empty() {
        "unavailable"
    } else if partial || configured.iter().any(|s| **s != "complete") {
        "partial"
    } else {
        "complete"
    };
    Ok(json!({
        "schema_version": "provider_findings.v1",
        "state": state,
        "period_days": 28,
        "period_scope": ["gsc", "ga4", "metrika"],
        "crawl_pages_omitted": ops.crawl_pages_omitted,
        "source": "project-bound provider history",
        "network_requests": ops.inspect_gsc,
        "sources": Value::Object(sources),
    }))
}
