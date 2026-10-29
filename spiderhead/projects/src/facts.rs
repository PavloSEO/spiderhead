//! Record project stack facts, keeping operator decisions above detected evidence

use std::path::Path;

use serde_json::{json, Value};

use crate::error::{value, ProjectsResult};
use crate::workspace::{facts as workspace_facts, load as workspace_load};

/// The provenance prefix a detection stamps; supplied facts may not claim it.
pub const DETECTED_PREFIX: &str = "detected by tech-detect";
/// One fact per tech-detect category the packaged priority policy consults.
pub const DETECTED_CATEGORIES: &[&str] = &["cms", "framework"];
const GENERATOR_EVIDENCE: &str = "meta name=generator";
const MAX_PROVENANCE: usize = 512;
const MAX_VALUE: usize = 2048;

/// Truthiness of a JSON value: null, false, zero and empty values are falsy.
fn falsy(value: &Value) -> bool {
    match value {
        Value::Null => true,
        Value::Bool(b) => !*b,
        Value::Number(n) => n.as_f64() == Some(0.0),
        Value::String(s) => s.is_empty(),
        Value::Array(a) => a.is_empty(),
        Value::Object(o) => o.is_empty(),
    }
}

/// Truncate to `max` characters, not bytes.
fn truncate_chars(text: &mut String, max: usize) {
    if text.chars().count() > max {
        let byte = text
            .char_indices()
            .nth(max)
            .map(|(i, _)| i)
            .unwrap_or(text.len());
        text.truncate(byte);
    }
}

/// `robots.match_path`: the URL part robots.txt patterns match against.
pub fn match_path(url: &str) -> String {
    spiderhead_checks::audit::robots::match_path(url)
}

fn is_detected(fact: &Value) -> bool {
    fact["provenance"]
        .as_str()
        .unwrap_or_default()
        .starts_with(DETECTED_PREFIX)
}

fn supplied(document: &Value) -> ProjectsResult<Vec<Value>> {
    let facts = workspace_facts(Some(document))?;
    let mut claimed: Vec<String> = facts
        .iter()
        .filter(|f| is_detected(f))
        .filter_map(|f| f["name"].as_str().map(str::to_owned))
        .collect();
    claimed.sort();
    claimed.dedup();
    if !claimed.is_empty() {
        return Err(value(format!(
            "supplied facts must not claim detection provenance: {}",
            claimed.join(", ")
        )));
    }
    let names: Vec<&str> = facts.iter().filter_map(|f| f["name"].as_str()).collect();
    if names
        .iter()
        .collect::<std::collections::BTreeSet<_>>()
        .len()
        != names.len()
    {
        return Err(value("supplied facts must not repeat a name"));
    }
    Ok(facts)
}

/// Injected detection tools.
pub trait DetectTools {
    /// `tools["robots_check"](url=..., paths=[match_path(url)])`.
    fn robots_check(&self, url: &str, paths: &[String]) -> Value;
    /// `tools["tech_detect"](url=...)`.
    fn tech_detect(&self, url: &str) -> Value;
}

fn run_detection(tools: &dyn DetectTools, target: &str) -> Value {
    let mut report = json!({
        "state": "unavailable",
        "url": target,
        "observed_at": crate::coverage::now(),
        "request_budget": 2,
        "facts": [],
        "unavailable": [],
    });
    let robots = tools.robots_check(target, &[match_path(target)]);
    if !robots.is_object() || falsy(&robots["ok"]) {
        let error = if robots.is_object() {
            robots.get("error").map(|e| {
                e.as_str()
                    .map(str::to_owned)
                    .unwrap_or_else(|| e.to_string())
            })
        } else {
            None
        }
        .unwrap_or_else(|| "invalid robots result".to_owned());
        report["reason"] = json!(format!(
            "robots.txt could not be read, so nothing was fetched: {error}"
        ));
        return report;
    }
    if robots["path_checks"]
        .as_array()
        .cloned()
        .unwrap_or_default()
        .iter()
        .any(|row| falsy(&row["allowed"]))
    {
        report["reason"] = json!(format!("robots.txt disallows fetching {target}"));
        return report;
    }
    let detected = tools.tech_detect(target);
    if !detected.is_object() || falsy(&detected["ok"]) {
        let error = if detected.is_object() {
            detected.get("error").map(|e| {
                e.as_str()
                    .map(str::to_owned)
                    .unwrap_or_else(|| e.to_string())
            })
        } else {
            None
        }
        .unwrap_or_else(|| "invalid detection result".to_owned());
        report["reason"] = json!(format!("tech-detect was unavailable: {error}"));
        return report;
    }
    let by_category = detected.get("by_category").cloned().unwrap_or(json!({}));
    for name in DETECTED_CATEGORIES {
        let rows: Vec<Value> = by_category[name]
            .as_array()
            .cloned()
            .unwrap_or_default()
            .into_iter()
            .filter(|row| row.is_object() && row["name"].as_str().is_some_and(|n| !n.is_empty()))
            .collect();
        let candidates: Vec<Value> = {
            let filtered: Vec<Value> = rows
                .iter()
                .filter(|row| {
                    row.get("evidence").and_then(Value::as_str) != Some(GENERATOR_EVIDENCE)
                })
                .cloned()
                .collect();
            if filtered.is_empty() {
                rows
            } else {
                filtered
            }
        };
        if candidates.is_empty() {
            report["unavailable"].as_array_mut().unwrap().push(json!({
                "name": name,
                "reason": format!("no {name} signature matched the fetched page"),
            }));
        } else if candidates.len() > 1 {
            let mut named: Vec<String> = candidates
                .iter()
                .filter_map(|r| r["name"].as_str().map(str::to_owned))
                .collect();
            named.sort();
            report["unavailable"].as_array_mut().unwrap().push(json!({
                "name": name,
                "reason": format!(
                    "{} {name} candidates matched ({}); not guessed",
                    candidates.len(),
                    named.join(", ")
                ),
            }));
        } else {
            let candidate = &candidates[0];
            let evidence = candidate
                .get("evidence")
                .and_then(Value::as_str)
                .filter(|e| !e.is_empty())
                .unwrap_or("signature match");
            let mut value_text = candidate["name"].as_str().unwrap_or_default().to_owned();
            truncate_chars(&mut value_text, MAX_VALUE);
            let mut provenance = format!("{DETECTED_PREFIX} at {target}: {evidence}");
            truncate_chars(&mut provenance, MAX_PROVENANCE);
            let observed_at = report["observed_at"].clone();
            report["facts"].as_array_mut().unwrap().push(json!({
                "name": name,
                "value": value_text,
                "provenance": provenance,
                "observed_at": observed_at,
            }));
        }
    }
    report["state"] = json!(if report["unavailable"].as_array().unwrap().is_empty() {
        "run"
    } else {
        "partial"
    });
    report["reason"] = json!("one page fetched after reading robots.txt");
    report
}

fn replace(facts: &mut Vec<Value>, fact: Value) {
    let name = fact["name"].as_str().unwrap_or_default();
    let positions: Vec<usize> = facts
        .iter()
        .enumerate()
        .filter(|(_, row)| row["name"].as_str() == Some(name))
        .map(|(i, _)| i)
        .collect();
    if positions.is_empty() {
        facts.push(fact);
        return;
    }
    facts[positions[0]] = fact;
    for index in positions[1..].iter().rev() {
        facts.remove(*index);
    }
}

fn merge(current: &[Value], supplied: &[Value], detected: &[Value]) -> (Vec<Value>, Vec<Value>) {
    let mut result: Vec<Value> = current.to_vec();
    let mut changes = Vec::new();
    for fact in supplied {
        let name = fact["name"].as_str().unwrap_or_default();
        let previous = result
            .iter()
            .find(|row| row["name"].as_str() == Some(name))
            .cloned();
        replace(&mut result, fact.clone());
        changes.push(json!({
            "name": name,
            "action": if previous.is_none() { "recorded" } else { "updated" },
            "value": fact["value"],
            "provenance": fact["provenance"],
            "reason": "operator-entered decision",
        }));
    }
    for fact in detected {
        let name = fact["name"].as_str().unwrap_or_default();
        let previous = result
            .iter()
            .find(|row| row["name"].as_str() == Some(name))
            .cloned();
        if let Some(previous) = &previous {
            if !is_detected(previous) {
                changes.push(json!({
                    "name": name,
                    "action": "kept_operator",
                    "value": previous["value"],
                    "provenance": previous["provenance"],
                    "detected_value": fact["value"],
                    "reason": "an operator-entered fact is a decision; detection is evidence",
                }));
                continue;
            }
        }
        let action = match &previous {
            None => "recorded",
            Some(previous) if previous["value"] == fact["value"] => "confirmed",
            _ => "updated",
        };
        replace(&mut result, fact.clone());
        changes.push(json!({
            "name": name,
            "action": action,
            "value": fact["value"],
            "provenance": fact["provenance"],
            "reason": "detected evidence",
        }));
    }
    (result, changes)
}

/// `project_facts`: preview or explicitly record project facts, optionally detected.
///
/// `detect` is the only thing that makes a request — and only through the injected tools.
pub fn project_facts(
    directory: &Path,
    facts: Option<&Value>,
    detect: bool,
    apply: bool,
    tools: Option<&dyn DetectTools>,
) -> ProjectsResult<Value> {
    let (root, project) = workspace_load(directory)?;
    let supplied = match facts {
        Some(f) => supplied(f)?,
        None => Vec::new(),
    };
    if supplied.is_empty() && !detect {
        let mut saved = project["facts"].as_array().cloned().unwrap_or_default();
        saved.sort_by(|a, b| {
            a["name"]
                .as_str()
                .unwrap_or_default()
                .cmp(b["name"].as_str().unwrap_or_default())
        });
        return Ok(json!({
            "ok": true,
            "state": "saved",
            "facts": saved,
            "count": saved.len(),
        }));
    }
    let detection = if detect {
        let Some(tools) = tools else {
            return Err(value(format!(
                "detection requires an injected {} tool",
                "robots_check"
            )));
        };
        run_detection(
            tools,
            project["site"]["target"].as_str().unwrap_or_default(),
        )
    } else {
        json!({
            "state": "not_run",
            "reason": "detection was not requested; no request was made",
            "facts": [],
            "unavailable": [],
        })
    };
    let (merged, changes) = merge(
        &project["facts"].as_array().cloned().unwrap_or_default(),
        &supplied,
        detection["facts"].as_array().unwrap_or(&Vec::new()),
    );
    let validated = workspace_facts(Some(&json!(merged)))?;
    let mut result = json!({
        "ok": true,
        "applied": false,
        "path": root.to_string_lossy(),
        "facts": validated,
        "changes": changes,
        "detection": detection,
        "reason": "preview only; pass apply=true to record these facts",
    });
    if Value::Array(validated.clone()) == project["facts"] {
        result["reason"] = json!("saved facts already match; nothing to record");
        return Ok(result);
    }
    if !apply {
        return Ok(result);
    }
    let mut document = project.clone();
    document["facts"] = json!(validated);
    crate::runtime::write_document(&root, "project.json", &document, None)?;
    result["applied"] = json!(true);
    result["reason"] = json!("facts recorded; apply project priorities to act on them");
    Ok(result)
}
