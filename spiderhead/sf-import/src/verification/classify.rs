//! `classify`: the per-finding verdict against one recrawled page observation.
//!
//! Every gap keeps the finding `not_verifiable`; only measured, comparable evidence
//! may produce `resolved`, `persisting`, or `changed`.

use super::*;
use crate::pyvalue::str_or_dumps;
use spiderhead_parse::py_url::urlsplit;

/// The issue signature reference compares for persisting/changed verdicts.
fn issue_signature(issue: &Object) -> String {
    let mut evidence = issue
        .get("evidence")
        .and_then(Value::as_object)
        .cloned()
        .unwrap_or_default();
    evidence.remove("contract");
    let mut payload = Object::new();
    payload.insert(
        "check".into(),
        issue.get("check").cloned().unwrap_or(Value::Null),
    );
    payload.insert(
        "status_code".into(),
        issue.get("status_code").cloned().unwrap_or(Value::Null),
    );
    payload.insert(
        "details".into(),
        issue
            .get("details")
            .filter(|v| truthy(v))
            .cloned()
            .unwrap_or_else(|| Value::Object(Object::new())),
    );
    payload.insert(
        "locations".into(),
        issue
            .get("locations")
            .filter(|v| truthy(v))
            .cloned()
            .unwrap_or_else(|| Value::Array(Vec::new())),
    );
    payload.insert("evidence".into(), Value::Object(evidence));
    digest(&Value::Object(payload))
}
/// `metrics.representation` when it is a non-empty string.
fn representation(page: &Object) -> Option<String> {
    get_mapping(page, "metrics")?
        .get("representation")?
        .as_str()
        .filter(|s| !s.is_empty())
        .map(str::to_owned)
}
/// `_policy_gap`: the before/after crawl-config policy must agree for a fix verdict.
fn policy_gap(before: &Object, after: &Object) -> Option<String> {
    let old = get_mapping(before, "run").and_then(|r| get_mapping(r, "crawl_config"));
    let new = get_mapping(after, "run").and_then(|r| get_mapping(r, "crawl_config"));
    let (Some(old), Some(new)) = (old, new) else {
        return Some("one audit lacks its results-affecting crawl configuration".to_owned());
    };
    let keys: HashSet<&String> = old.keys().chain(new.keys()).collect();
    let mut changed: Vec<&String> = keys
        .into_iter()
        .filter(|k| !TARGET_BOUNDS.contains(&k.as_str()))
        .filter(|k| {
            !py_eq(
                old.get(*k).unwrap_or(&Value::Null),
                new.get(*k).unwrap_or(&Value::Null),
            )
        })
        .collect();
    changed.sort();
    if !changed.is_empty() {
        return Some(format!(
            "results-affecting settings changed: {}",
            changed
                .iter()
                .map(|s| s.as_str())
                .collect::<Vec<_>>()
                .join(", ")
        ));
    }
    let after_run = get_mapping(after, "run");
    if after_run.and_then(|r| r.get("crawl_valid")) == Some(&Value::Bool(false)) {
        return Some("recrawl is marked invalid; no usable measurement is proven".to_owned());
    }
    if after_run
        .and_then(|r| r.get("cache_replay"))
        .is_some_and(truthy)
    {
        return Some(
            "recrawl reused cached responses rather than measuring the page now".to_owned(),
        );
    }
    None
}
/// `_check_unmeasured`: the after run skipped or disabled the check entirely.
fn check_unmeasured(after: &Object, check: &Value) -> Option<String> {
    let run = get_mapping(after, "run")?;
    for label in ["checks_skipped", "checks_disabled"] {
        if let Some(items) = run.get(label).and_then(Value::as_array) {
            for item in items.iter().filter_map(|i| i.as_object()) {
                if py_eq(item.get("id").unwrap_or(&Value::Null), check) {
                    let reason = item
                        .get("reason")
                        .filter(|r| truthy(r))
                        .map(str_or_dumps)
                        .unwrap_or_else(|| "no evidence".to_owned());
                    let word = label.strip_prefix("checks_").unwrap_or(label);
                    return Some(format!("{} was {word}: {reason}", str_or_dumps(check)));
                }
            }
        }
    }
    None
}
/// `_source_gap`: the recrawl source origin must match the affected URL's origin.
fn source_gap(after: &Object, url: &str) -> Result<Option<&'static str>, VerifyError> {
    let source = get_mapping(after, "run").and_then(|r| r.get("source"));
    if let Some(source) = source.and_then(Value::as_str) {
        if source.starts_with("http://") || source.starts_with("https://") {
            let origin = urlsplit(source).map_err(|e| VerifyError(e.to_string()))?;
            let target = urlsplit(url).map_err(|e| VerifyError(e.to_string()))?;
            if (origin.scheme, origin.netloc.to_lowercase())
                != (target.scheme, target.netloc.to_lowercase())
            {
                return Ok(Some("recrawl source origin differs from the affected URL"));
            }
        }
    }
    Ok(None)
}

/// reference `int` coercion for JSON: ints and booleans count (`isinstance(x, int)`),
/// floats do not even when integral (`isinstance(200.0, int)` is False).
/// reference `int` coercion for JSON: ints and booleans count (`isinstance(x, int)`),
/// floats do not even when integral (`isinstance(200.0, int)` is False).
fn json_int(value: &Value) -> Option<i64> {
    match value {
        Value::Bool(b) => Some(i64::from(*b)),
        Value::Number(n) => n.as_i64(),
        _ => None,
    }
}

/// reference `type(value).__name__` for the AttributeError messages of `.get` on non-dicts.
/// reference `type(value).__name__` for the AttributeError messages of `.get` on non-dicts.
fn py_type_name(value: &Value) -> &'static str {
    match value {
        Value::Null => "NoneType",
        Value::Bool(_) => "bool",
        Value::Number(n) if n.is_i64() || n.is_u64() => "int",
        Value::Number(_) => "float",
        Value::String(_) => "str",
        Value::Array(_) => "list",
        Value::Object(_) => "dict",
    }
}

/// `check in _STATUS_CHECKS` under reference `in` semantics.
/// `check in _STATUS_CHECKS` under reference `in` semantics.
fn status_check(check: &Value) -> bool {
    check.as_str().is_some_and(|c| STATUS_CHECKS.contains(&c))
}

/// `classify` each selected baseline finding with its URL's after-audit observation.
pub fn classify(
    baseline: &Object,
    selected: &[Object],
    observations: &Map<String, Value>,
    missing_reason: &str,
) -> Result<Vec<Object>, VerifyError> {
    unique_rows(&selected.iter().collect::<Vec<_>>(), "finding")?;
    let selected_urls: Vec<Value> = selected
        .iter()
        .map(|i| i.get("target_url").cloned().unwrap_or(Value::Null))
        .collect();
    let baseline_pages = baseline
        .get("pages")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    let in_selected: Vec<&Object> = baseline_pages
        .iter()
        .filter_map(|p| p.as_object())
        .filter(|p| py_contains(&selected_urls, p.get("url").unwrap_or(&Value::Null)))
        .collect();
    unique_rows(&in_selected, "page")?;
    let mut old_pages: Vec<(Value, Object)> = Vec::new();
    for page in baseline_pages.iter().filter_map(|p| p.as_object()) {
        if let Some(url) = page.get("url").filter(|v| truthy(v)) {
            old_pages.push((url.clone(), page.clone()));
        }
    }
    let old_page_of = |url: &Value| -> Option<&Object> {
        old_pages
            .iter()
            .rev()
            .find(|(u, _)| py_eq(u, url))
            .map(|(_, p)| p)
    };
    let mut results = Vec::new();
    for issue in selected {
        let url_value = issue.get("target_url").cloned().unwrap_or(Value::Null);
        let url = url_value.as_str();
        let check = issue.get("check").cloned().unwrap_or(Value::Null);
        let mut status = "not_verifiable".to_owned();
        let mut reason = String::new();
        let mut after_issue: Option<Object> = None;
        let mut after_page: Option<Object> = None;
        let old_page = old_page_of(&url_value);
        let after_raw = url.and_then(|u| observations.get(u));

        let computed = (|| -> Result<(), VerifyError> {
            let (Some(url), Some(old_page)) = (url, old_page) else {
                reason = "finding has no affected baseline page to recrawl".to_owned();
                return Ok(());
            };
            if !local_issue(issue) {
                reason = "finding needs site-wide or relation evidence; a selected-URL recrawl cannot clear it".to_owned();
                return Ok(());
            }
            let Some(after_raw) = after_raw else {
                reason = missing_reason.to_owned();
                return Ok(());
            };
            let Some(after) = after_raw.as_object() else {
                return fail(format!(
                    "'{}' object has no attribute 'get'",
                    py_type_name(after_raw)
                ));
            };
            if after.get("schema_version") != Some(&Value::from("2.0")) {
                reason = "recrawl has no complete audit pages and issues document".to_owned();
                return Ok(());
            }
            let (Some(after_pages), Some(after_issues)) = (
                after.get("pages").and_then(Value::as_array),
                after.get("issues").and_then(Value::as_array),
            ) else {
                reason = "recrawl has no complete audit pages and issues document".to_owned();
                return Ok(());
            };
            if let Some(gap) = policy_gap(baseline, after) {
                reason = gap;
                return Ok(());
            }
            if let Some(gap) = source_gap(after, url)? {
                reason = gap.to_owned();
                return Ok(());
            }
            let same_url: Vec<&Object> = after_pages
                .iter()
                .filter_map(|p| p.as_object())
                .filter(|p| p.get("url").is_some_and(|v| py_eq(v, &Value::from(url))))
                .collect();
            unique_rows(&same_url, "page")?;
            let matching: Vec<&Object> = after_issues
                .iter()
                .filter_map(|i| i.as_object())
                .filter(|i| {
                    i.get("target_url")
                        .is_some_and(|v| py_eq(v, &Value::from(url)))
                        && i.get("check").is_some_and(|v| py_eq(v, &check))
                })
                .collect();
            unique_rows(&matching, "finding")?;
            let ap = after_pages
                .iter()
                .filter_map(|p| p.as_object())
                .find(|p| p.get("url").is_some_and(|v| py_eq(v, &Value::from(url))))
                .cloned();
            let Some(ap) = ap else {
                reason = "selected URL has no page observation in the recrawl".to_owned();
                return Ok(());
            };
            after_page = Some(ap.clone());
            if let Some(gap) = check_unmeasured(after, &check) {
                reason = gap;
                return Ok(());
            }
            let old_repr = representation(old_page);
            if old_repr != representation(&ap) || (!status_check(&check) && old_repr.is_none()) {
                reason = "page representation changed or was not retained in both runs".to_owned();
                return Ok(());
            }
            let found = after_issues.iter().filter_map(|i| i.as_object()).find(|i| {
                i.get("check").is_some_and(|v| py_eq(v, &check))
                    && i.get("target_url")
                        .is_some_and(|v| py_eq(v, &Value::from(url)))
            });
            if let Some(found) = found {
                after_issue = Some(found.clone());
                status = if issue_signature(issue) == issue_signature(found) {
                    "persisting".to_owned()
                } else {
                    "changed".to_owned()
                };
                return Ok(());
            }
            let silent = get_mapping(after, "summary")
                .and_then(|s| s.get("check_coverage"))
                .and_then(Value::as_object)
                .and_then(|c| {
                    c.get("checks_silent_ids")
                        .and_then(Value::as_array)
                        .cloned()
                })
                .unwrap_or_default();
            if !silent.iter().any(|v| py_eq(v, &check)) {
                reason = "recrawl has no measured clean verdict for this check".to_owned();
                return Ok(());
            }
            let requires_rendering = get_mapping(after, "run")
                .and_then(|r| r.get("requires_rendering"))
                .is_some_and(truthy);
            if requires_rendering && representation(&ap).as_deref() == Some("static") {
                reason =
                    "recrawl requires rendering before a clean verdict can be trusted".to_owned();
                return Ok(());
            }
            let code = json_int(ap.get("status_code").unwrap_or(&Value::Null));
            let Some(code) = code.filter(|c| *c >= 100) else {
                reason = "recrawl has no measured HTTP response for the affected URL".to_owned();
                return Ok(());
            };
            if !status_check(&check)
                && (!(200..300).contains(&code)
                    || !ap
                        .get("content_type")
                        .filter(|v| truthy(v))
                        .map(|v| str_or_dumps(v).to_lowercase())
                        .unwrap_or_default()
                        .contains("html"))
            {
                status = "changed".to_owned();
                reason = "response is no longer a successful HTML page; the original page check was not retested".to_owned();
                return Ok(());
            }
            if status_check(&check)
                && !((200..300).contains(&code)
                    || (check.as_str() == Some("BAD_REDIRECT_TYPE")
                        && (code == 301 || code == 308)))
            {
                status = "changed".to_owned();
                reason = "HTTP response changed but is not a verified clean response".to_owned();
                return Ok(());
            }
            let old_code = old_page.get("status_code").cloned().unwrap_or(Value::Null);
            if !py_eq(&old_code, &Value::from(code)) && !status_check(&check) {
                status = "changed".to_owned();
                reason = "HTTP status changed; the original page condition was not retested on the same response".to_owned();
                return Ok(());
            }
            status = "resolved".to_owned();
            Ok(())
        })();
        computed?;
        let mut row = Object::new();
        row.insert(
            "finding_id".into(),
            issue.get("id").cloned().unwrap_or(Value::Null),
        );
        row.insert("check".into(), check.clone());
        row.insert("url".into(), url_value.clone());
        row.insert("status".into(), Value::from(status));
        row.insert("reason".into(), Value::from(reason));
        row.insert("before".into(), Value::Object(issue.clone()));
        row.insert(
            "after".into(),
            after_issue.map_or(Value::Null, Value::Object),
        );
        row.insert(
            "before_page".into(),
            old_page.map_or(Value::Null, |p| Value::Object(p.clone())),
        );
        row.insert(
            "after_page".into(),
            after_page.map_or(Value::Null, Value::Object),
        );
        results.push(row);
    }
    Ok(results)
}
