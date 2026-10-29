//! Coverage status rows and aggregation axes.

use std::collections::{BTreeSet, HashMap};
use std::path::{Path, PathBuf};

use regex::Regex;
use serde_json::{json, Map, Value};
use std::sync::LazyLock;

pub type ScanCache = HashMap<String, (Option<PathBuf>, Option<String>)>;
pub type CountCache = HashMap<(String, String), Value>;

fn agent_reviewer() -> &'static Regex {
    static RE: LazyLock<Regex> =
        LazyLock::new(|| Regex::new(r"(?i)(agent|assistant|claude|codex|llm|ai)\b").unwrap());
    &RE
}

fn finding_id_re() -> &'static Regex {
    static RE: LazyLock<Regex> =
        LazyLock::new(|| Regex::new(r"(?i)[a-z0-9][a-z0-9_.-]{0,99}\z").unwrap());
    &RE
}

/// `_affected_url_count`: retained affected-URL evidence or the saved issue selection.
pub fn affected_url_count(
    root: &Path,
    definition: &Value,
    measurement: Option<&Value>,
    cache: &mut ScanCache,
    count_cache: &mut CountCache,
) -> Value {
    let urls = measurement
        .and_then(|m| m.get("affected_urls"))
        .and_then(Value::as_array);
    if let Some(urls) = urls {
        if urls
            .iter()
            .all(|u| u.as_str().is_some_and(|s| !s.is_empty()))
        {
            let count = super::schema::unique_count(urls) as i64;
            return json!({
                "count": count,
                "state": "measured",
                "scope_state": "measured_population",
                "source": "retained_task_measurement",
                "reason": null,
            });
        }
    }
    if let Some(reference) = definition.get("finding_ref") {
        let (scan_id, finding_id) = (
            reference.get("scan_id").and_then(Value::as_str),
            reference.get("finding_id").and_then(Value::as_str),
        );
        if let (Some(scan_id), Some(finding_id)) = (scan_id, finding_id) {
            let mut unavailable = cache.get(scan_id).cloned().unwrap_or_default().1;
            let mut scan_path = cache.get(scan_id).cloned().unwrap_or_default().0;
            if !cache.contains_key(scan_id) {
                let mut offset = 0;
                loop {
                    let page = match spiderhead_store::history::list_scans(
                        &root.join("scans"),
                        offset,
                        100,
                    ) {
                        Ok(p) => p,
                        Err(_) => {
                            unavailable =
                                Some("saved scan history contains unreadable entries".into());
                            break;
                        }
                    };
                    if page
                        .get("errors")
                        .is_some_and(|e| !e.as_array().is_none_or(Vec::is_empty))
                    {
                        unavailable = Some("saved scan history contains unreadable entries".into());
                        break;
                    }
                    let items = page["items"].as_array().cloned().unwrap_or_default();
                    let found = items
                        .iter()
                        .find(|row| row.get("uuid").and_then(Value::as_str) == Some(scan_id))
                        .cloned();
                    if let Some(found) = found {
                        scan_path = found["path"].as_str().map(PathBuf::from);
                        break;
                    }
                    offset += items.len() as i64;
                    if !page["has_more"].as_bool().unwrap_or(false) {
                        unavailable =
                            Some("referenced scan is not retained in this project".into());
                        break;
                    }
                }
                cache.insert(scan_id.to_owned(), (scan_path.clone(), unavailable.clone()));
            }
            if let Some(scan_path) = scan_path {
                let check_id = finding_id.strip_prefix("check:").unwrap_or(finding_id);
                if finding_id_re().is_match(check_id) {
                    let key = (scan_id.to_owned(), check_id.to_owned());
                    if let Some(counted) = count_cache.get(&key) {
                        return counted.clone();
                    }
                    let mut kwargs = Map::new();
                    kwargs.insert("issue_check".into(), json!(check_id));
                    kwargs.insert("columns".into(), json!(["url"]));
                    kwargs.insert("offset".into(), json!(0));
                    kwargs.insert("limit".into(), json!(1));
                    kwargs.insert("count_timeout_seconds".into(), json!(5.0));
                    kwargs.insert("max_bytes".into(), json!(4096));
                    let args = spiderhead_store::url_query::QueryArgs::from_kwargs(&kwargs);
                    let selection = spiderhead_store::url_query::scan_url_query(
                        &scan_path.to_string_lossy(),
                        &args,
                    );
                    let selection = Value::Object(selection);
                    let count = selection.get("filtered_total").and_then(Value::as_i64);
                    if selection.get("ok").and_then(|v| v.as_bool()) == Some(true)
                        && selection
                            .get("filtered_total_state")
                            .and_then(|v| v.as_str())
                            == Some("exact")
                        && count.is_some()
                        && selection["source"]["scan_uuid"].as_str() == Some(scan_id)
                    {
                        let source = &selection["source"];
                        let scan_state = if source["crawl_partial"].as_bool().unwrap_or(false)
                            || source["corpus_partial"].as_bool().unwrap_or(false)
                        {
                            "partial"
                        } else {
                            "complete"
                        };
                        let counted = json!({
                            "count": count,
                            "state": "measured",
                            "scope_state": scan_state,
                            "source": "saved_issue_selection",
                            "scan_uuid": scan_id,
                            "check_id": check_id,
                            "reason": null,
                        });
                        count_cache.insert(key, counted.clone());
                        return counted;
                    }
                    unavailable = selection
                        .get("reason")
                        .or_else(|| selection.get("error"))
                        .and_then(Value::as_str)
                        .map(str::to_owned)
                        .or(Some(
                            "saved issue count was capped, unavailable, or from a different scan"
                                .into(),
                        ));
                } else {
                    unavailable = Some(
                        "finding_ref.finding_id is not a supported saved check identifier".into(),
                    );
                }
            }
            return json!({
                "count": null,
                "state": "not_measured",
                "scope_state": "unknown",
                "source": "finding_ref",
                "reason": unavailable.unwrap_or_else(|| "saved affected-URL evidence is unavailable".into()),
            });
        }
    }
    json!({
        "count": null,
        "state": "not_measured",
        "scope_state": "unknown",
        "source": "none",
        "reason": "the retained task measurement does not enumerate affected URLs",
    })
}

/// `_disposition`: resolve an item as agreed-applicable, reviewed-excluded, or pending.
pub fn disposition(
    item: &Value,
    record: Option<&Value>,
    stale_reason: &str,
) -> (String, Option<Value>) {
    if let Some(record) = record {
        if record["status"].as_str() == Some("not_applicable") {
            if !stale_reason.is_empty() {
                return (
                    "pending_exclusion".into(),
                    Some(json!({
                        "reason": "exclusion record is stale; review and record the decision again"
                    })),
                );
            }
            let mut basis = Map::new();
            if record.get("artifact").is_some() {
                basis.insert("artifact".into(), record["artifact"].clone());
                basis.insert(
                    "sha256".into(),
                    record.get("sha256").cloned().unwrap_or(Value::Null),
                );
            }
            if record.get("evidence").is_some() {
                basis.insert("evidence".into(), record["evidence"].clone());
            }
            if !basis.is_empty() {
                return (
                    "excluded".into(),
                    Some(json!({
                        "reason": record["reason"],
                        "reviewer": record.get("reviewer"),
                        "basis": Value::Object(basis),
                        "recorded_at": record["recorded_at"],
                        "revision": record.get("revision"),
                    })),
                );
            }
            return (
                "pending_exclusion".into(),
                Some(json!({
                    "reason": "exclusion record lacks a verifiable evidence basis; review and record again"
                })),
            );
        }
    }
    if item["definition"]["enabled"].as_bool() == Some(false) {
        return (
            "pending_exclusion".into(),
            Some(json!({
                "reason": "disabled without a reviewed exclusion; it stays inside the agreed denominator"
            })),
        );
    }
    ("applicable".into(), None)
}

/// `_owner_decided`: manual work counts as owner-complete by signoff or non-agent review.
pub fn owner_decided(record: &Value) -> bool {
    if record["signoff"].as_bool() == Some(true) {
        return true;
    }
    record["review"].as_str() == Some("approved")
        && !agent_reviewer().is_match(record["reviewer"].as_str().unwrap_or_default())
}

/// `_measured_urls`: the URL set a fresh automatic measurement covered, and completeness.
pub fn measured_urls(row: &Value) -> (BTreeSet<String>, bool) {
    let measurement = row.get("measurement").cloned().unwrap_or(json!({}));
    if let Some(urls) = measurement.get("urls").and_then(Value::as_array) {
        return (
            urls.iter()
                .filter_map(|u| u.as_str().map(str::to_owned))
                .collect(),
            measurement["urls_enumerated"].as_bool().unwrap_or(true),
        );
    }
    let scope_urls = row["scope"]
        .get("urls")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    if !scope_urls.is_empty() {
        return (
            scope_urls
                .iter()
                .filter_map(|u| u.as_str().map(str::to_owned))
                .collect(),
            true,
        );
    }
    (BTreeSet::new(), measurement.get("population").is_none())
}

/// `_eligibility`: the agreed eligible URL set for a row, or None when unverifiable.
pub fn eligibility(row: &Value, population: Option<&Value>) -> Option<BTreeSet<String>> {
    let population = population?;
    if population["kind"].as_str() == Some("unknown") {
        return None;
    }
    let template = row["scope"]["template"].as_str();
    if let Some(template) = template {
        if let Some(entry) = population["templates"].get(template) {
            let urls = entry["urls"].as_array().cloned().unwrap_or_default();
            return if urls.is_empty() {
                None
            } else {
                Some(
                    urls.iter()
                        .filter_map(|u| u.as_str().map(str::to_owned))
                        .collect(),
                )
            };
        }
    }
    let urls = population["urls"].as_array().cloned().unwrap_or_default();
    if !urls.is_empty() {
        return Some(
            urls.iter()
                .filter_map(|u| u.as_str().map(str::to_owned))
                .collect(),
        );
    }
    None
}

/// `_task_axis`: one task-coverage axis over the given rows.
pub fn task_axis(rows: &[&Value], basis: &str) -> Value {
    let excluded = rows
        .iter()
        .filter(|r| r["applicability"].as_str() == Some("excluded"))
        .count() as i64;
    let pending = rows
        .iter()
        .filter(|r| r["applicability"].as_str() == Some("pending_exclusion"))
        .count() as i64;
    let not_agreed = rows
        .iter()
        .filter(|r| r["applicability"].as_str() == Some("not_agreed"))
        .count() as i64;
    let denominator_rows: Vec<&&Value> = rows
        .iter()
        .filter(|r| {
            matches!(
                r["applicability"].as_str(),
                Some("applicable" | "pending_exclusion")
            )
        })
        .collect();
    let numerator = denominator_rows
        .iter()
        .filter(|r| r["complete"].as_bool() == Some(true))
        .count() as i64;
    let denominator = denominator_rows.len() as i64;
    json!({
        "basis": basis,
        "numerator": numerator,
        "denominator": denominator,
        "excluded": excluded,
        "pending_exclusion": pending,
        "not_agreed": not_agreed,
        "unfinished": denominator - numerator,
        "state": if denominator > 0 { "measured" } else { "unknown" },
        "reason": if denominator > 0 {
            "enumerated checklist task set"
        } else {
            "no agreed applicable tasks remain in this set; the ratio is undefined, not 100%"
        },
    })
}

/// `_url_axis`: eligible-URL coverage over the agreed eligible population.
pub fn url_axis(ordered: &[Value], plan: Option<&Value>) -> Value {
    let population = plan.and_then(|p| p.get("population"));
    let mut measured: BTreeSet<String> = BTreeSet::new();
    let mut counted: BTreeSet<String> = BTreeSet::new();
    let mut unverified = 0i64;
    for row in ordered {
        if row["execution_kind"].as_str() != Some("automatic")
            || row["applicability"].as_str() == Some("excluded")
            || row["state"].as_str() != Some("run")
        {
            continue;
        }
        let (observed, fully_enumerated) = measured_urls(row);
        measured.extend(observed.iter().cloned());
        let eligible = eligibility(row, population);
        if let Some(eligible) = &eligible {
            counted.extend(observed.intersection(eligible).cloned());
        }
        if eligible.is_none() || !fully_enumerated {
            unverified += 1;
        }
    }
    let (state, numerator, denominator, reason) = match population {
        None => (
            "unknown".to_owned(),
            Value::Null,
            Value::Null,
            "no agreed URL population is recorded for this checklist".to_owned(),
        ),
        Some(pop) if pop["kind"].as_str() == Some("unknown") => (
            "unknown".to_owned(),
            Value::Null,
            Value::Null,
            pop["reason"].as_str().unwrap_or_default().to_owned(),
        ),
        Some(pop) => {
            let denominator = pop["size"].clone();
            let numerator = json!(counted.len());
            let state = if unverified == 0 {
                "measured"
            } else {
                "partial"
            };
            let reason = if unverified > 0 {
                format!(
                    "{unverified} measurement(s) cannot be verified against the agreed \
                     population; numerator counts only verified eligible URLs"
                )
            } else if pop["kind"].as_str() == Some("sample") {
                "measured against the named sample, not the full site population".to_owned()
            } else {
                "measured against the agreed site population".to_owned()
            };
            (state.to_owned(), numerator, denominator, reason)
        }
    };
    json!({
        "basis": "eligible URLs covered by fresh check measurements over the agreed eligible \
                  URL population",
        "numerator": numerator,
        "denominator": denominator,
        "measured_urls": measured.len(),
        "unverified_measurements": unverified,
        "population_kind": population.and_then(|p| p.get("kind")).cloned().unwrap_or(Value::Null),
        "population_name": population.and_then(|p| p.get("name")).cloned().unwrap_or(Value::Null),
        "state": state,
        "reason": reason,
    })
}
