//! Coverage format constants and per-field validators.

use std::collections::BTreeSet;

use regex::Regex;
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::sync::LazyLock;

use crate::error::ProjectsResult;
use crate::workspace::target as workspace_target;

/// v1 document format.
pub const FORMAT: &str = "spiderhead.coverage.v1";
/// v2 document format.
pub const FORMAT_V2: &str = "spiderhead.coverage.v2";
/// v3 document format.
pub const FORMAT_V3: &str = "spiderhead.coverage.v3";
/// v4 document format.
pub const FORMAT_V4: &str = "spiderhead.coverage.v4";
/// `coverage.json` byte ceiling.
pub const MAX_BYTES: usize = 32 * 1024 * 1024;
/// Enumerated-URL ceiling.
pub const URL_ENUMERATION_LIMIT: usize = 10000;

fn id_re() -> &'static Regex {
    static RE: LazyLock<Regex> = LazyLock::new(|| {
        Regex::new(
            r"(?:check:[A-Z][A-Z0-9_]*|skill:(?:workflow|general)/[a-z0-9_-]+|scenario:[a-z0-9_-]+|custom:[a-z][a-z0-9._/-]{0,127})\z",
        )
        .unwrap()
    });
    &RE
}

/// `_hash`: sha256 over the canonical JSON spelling; non-JSON values are rejected.
pub fn hash(value: &Value) -> ProjectsResult<String> {
    // serde_json always serializes its Value; reject non-finite floats like allow_nan=False.
    fn finite(v: &Value) -> bool {
        match v {
            Value::Number(n) => n.as_f64().is_none_or(f64::is_finite),
            Value::Array(items) => items.iter().all(finite),
            Value::Object(map) => map.values().all(finite),
            _ => true,
        }
    }
    if !finite(value) {
        return Err(crate::error::value("coverage value is not canonical JSON"));
    }
    let encoded = crate::pyjson::dumps_compact_ascii(value);
    Ok(format!("{:x}", Sha256::digest(encoded.as_bytes())))
}

/// `_completion_hash`: hash evidence-bearing definition fields, not scheduling choices.
pub fn completion_hash(definition: &Value) -> ProjectsResult<String> {
    let object = definition.as_object().cloned().unwrap_or_default();
    let filtered: serde_json::Map<String, Value> = object
        .into_iter()
        .filter(|(key, value)| {
            !matches!(
                key.as_str(),
                "priority" | "priority_origin" | "order" | "enabled" | "assignee"
            ) && !(matches!(
                key.as_str(),
                "description"
                    | "acceptance_criterion"
                    | "finding_ref"
                    | "method_ref"
                    | "competitors"
            ) && value.is_null())
                && !(key == "competitors" && value.as_array().is_some_and(Vec::is_empty))
        })
        .collect();
    hash(&Value::Object(filtered))
}

/// `_text`: nonempty bounded text.
pub fn text(value: &Value, name: &str, limit: usize) -> ProjectsResult<String> {
    match value.as_str() {
        Some(s) if !s.trim().is_empty() && s.len() <= limit => Ok(s.to_owned()),
        _ => Err(crate::error::value(format!(
            "{name} must be nonempty text of at most {limit} characters"
        ))),
    }
}

/// `_identifier`: one checklist item identifier.
pub fn identifier(value: &Value) -> ProjectsResult<String> {
    match value.as_str() {
        Some(s) if id_re().is_match(s) && !s.contains("..") => Ok(s.to_owned()),
        _ => Err(crate::error::value("invalid checklist item identifier")),
    }
}

/// `_urls`: a bounded list of normalized absolute URLs on the same site identity.
pub fn urls(value: &Value, site: &str, label: &str) -> ProjectsResult<Vec<String>> {
    let list = value
        .as_array()
        .ok_or_else(|| crate::error::value(format!("{label} must be a bounded list")))?;
    if list.len() > URL_ENUMERATION_LIMIT {
        return Err(crate::error::value(format!(
            "{label} must be a bounded list"
        )));
    }
    let site_netloc = crate::urls::urlsplit(site)?.netloc;
    let mut out = Vec::with_capacity(list.len());
    for url in list {
        let s = url.as_str().filter(|s| s.len() <= 2048);
        let Some(s) = s else {
            return Err(crate::error::value(format!(
                "{label} must be normalized absolute URLs"
            )));
        };
        if spiderhead_core::net::normalize_url(s) != s {
            return Err(crate::error::value(format!(
                "{label} must be normalized absolute URLs"
            )));
        }
        if crate::urls::urlsplit(s)?.netloc != site_netloc {
            return Err(crate::error::value(format!(
                "{label} belongs to another site"
            )));
        }
        out.push(s.to_owned());
    }
    if out.iter().collect::<BTreeSet<_>>().len() != out.len() {
        return Err(crate::error::value(format!("duplicate URL in {label}")));
    }
    Ok(out)
}

/// `_scope`: validate `{site, template, urls}`.
pub fn scope(value: &Value) -> ProjectsResult<Value> {
    let object = value.as_object().filter(|o| {
        o.keys().map(String::as_str).collect::<BTreeSet<_>>()
            == ["site", "template", "urls"].into_iter().collect()
    });
    let Some(object) = object else {
        return Err(crate::error::value(
            "scope requires site, template and urls",
        ));
    };
    let site = workspace_target(object["site"].as_str().unwrap_or_default())?;
    let template = &object["template"];
    if !template.is_null() {
        text(template, "template", 128)?;
    }
    Ok(json!({
        "site": site,
        "template": template,
        "urls": urls(&object["urls"], &site, "scope urls")?,
    }))
}

fn population_defaults() -> serde_json::Map<String, Value> {
    [
        ("size", Value::Null),
        ("urls", json!([])),
        ("name", Value::Null),
        ("reason", Value::Null),
        ("templates", Value::Null),
    ]
    .into_iter()
    .map(|(k, v)| (k.to_owned(), v))
    .collect()
}

/// `_population`: validate one agreed URL population against the site identity.
pub fn population(value: &Value, site: &str, nested: bool) -> ProjectsResult<Value> {
    let object = value
        .as_object()
        .ok_or_else(|| crate::error::value("population must be an object"))?;
    let mut allowed: BTreeSet<&str> = ["kind", "size", "urls", "name", "source", "reason"]
        .into_iter()
        .collect();
    if !nested {
        allowed.insert("templates");
    }
    let missing: Vec<&str> = ["kind", "source"]
        .into_iter()
        .filter(|key| !object.contains_key(*key))
        .collect();
    if !missing.is_empty() {
        return Err(crate::error::value(format!(
            "population is missing required key(s): {}",
            missing.join(", ")
        )));
    }
    let unknown: Vec<&str> = object
        .keys()
        .map(String::as_str)
        .filter(|key| !allowed.contains(key))
        .collect();
    if !unknown.is_empty() {
        let mut sorted_unknown = unknown.clone();
        sorted_unknown.sort_unstable();
        let mut sorted_allowed: Vec<&str> = allowed.into_iter().collect();
        sorted_allowed.sort_unstable();
        return Err(crate::error::value(format!(
            "population has unsupported key(s): {}; allowed: {}",
            sorted_unknown.join(", "),
            sorted_allowed.join(", ")
        )));
    }
    let mut merged = population_defaults();
    for (k, v) in object {
        merged.insert(k.clone(), v.clone());
    }
    let kind = merged["kind"].as_str().unwrap_or_default();
    let kinds: &[&str] = if nested {
        &["complete_set", "sample"]
    } else {
        &["complete_set", "sample", "unknown"]
    };
    if !kinds.contains(&kind) {
        return Err(crate::error::value(format!(
            "population kind must be complete_set, sample{}",
            if nested { "" } else { " or unknown" }
        )));
    }
    let urls = urls(&merged["urls"], site, "population urls")?;
    let mut size = merged["size"].clone();
    let name = merged["name"].clone();
    if kind == "unknown" {
        if !size.is_null() || !urls.is_empty() || !name.is_null() {
            return Err(crate::error::value(
                "an unknown population has no size, urls or name",
            ));
        }
        text(&merged["reason"], "unknown population reason", 512)?;
    } else {
        if !urls.is_empty() {
            if !size.is_null() && size.as_i64() != Some(urls.len() as i64) {
                return Err(crate::error::value(
                    "population size disagrees with its enumerated urls",
                ));
            }
            size = json!(urls.len());
        } else if size.is_null() {
            return Err(crate::error::value(
                "population is missing required key(s): size (or urls)",
            ));
        }
        match size.as_i64() {
            Some(v) if !size.is_boolean() && (1..=1_000_000_000).contains(&v) => {}
            _ => {
                return Err(crate::error::value(
                    "population size must be a positive bounded integer",
                ))
            }
        }
        if kind == "sample" {
            text(&name, "sample name", 128)?;
        } else if !name.is_null() {
            return Err(crate::error::value("only a sample population is named"));
        }
        if !merged["reason"].is_null() {
            text(&merged["reason"], "population reason", 512)?;
        }
    }
    text(&merged["source"], "population source", 512)?;
    let mut result = json!({
        "kind": kind,
        "size": size,
        "urls": urls,
        "name": name,
        "source": merged["source"],
        "reason": merged["reason"],
    });
    if !nested {
        let templates = match &merged["templates"] {
            Value::Null => serde_json::Map::new(),
            Value::Object(t) if t.len() <= 100 => t.clone(),
            _ => {
                return Err(crate::error::value(
                    "population templates must be a bounded object",
                ))
            }
        };
        let mut checked = serde_json::Map::new();
        for (key, entry) in &templates {
            let name = text(&json!(key), "template population name", 128)?;
            checked.insert(name, population(entry, site, true)?);
        }
        if let Some(size) = result["size"].as_i64() {
            let mut members: BTreeSet<String> = urls.iter().cloned().collect();
            for (template_name, entry) in &checked {
                let entry_urls: Vec<String> = entry["urls"]
                    .as_array()
                    .unwrap_or(&Vec::new())
                    .iter()
                    .filter_map(|u| u.as_str().map(str::to_owned))
                    .collect();
                if !urls.is_empty() && !entry_urls.iter().all(|u| members.contains(u)) {
                    return Err(crate::error::value(format!(
                        "template population {template_name:?} declares URLs outside the \
                         enumerated site population"
                    )));
                }
                if entry["size"].as_i64().unwrap_or(0) > size {
                    return Err(crate::error::value(format!(
                        "template population {template_name:?} is larger than the agreed site \
                         population"
                    )));
                }
                members.extend(entry_urls);
            }
            if members.len() as i64 > size {
                return Err(crate::error::value(
                    "template populations declare more URLs than the agreed site population",
                ));
            }
        }
        result["templates"] = Value::Object(checked);
    }
    Ok(result)
}

/// `_tasks`: validate the agreed task set — the whole checklist or a sourced selection.
pub fn tasks(value: &Value) -> ProjectsResult<Value> {
    if value.is_null() || *value == json!({"kind": "all_agreed"}) {
        return Ok(json!({"kind": "all_agreed"}));
    }
    let object = value.as_object().filter(|o| {
        o.keys().map(String::as_str).collect::<BTreeSet<_>>()
            == ["kind", "ids", "source"].into_iter().collect()
    });
    let Some(object) = object else {
        return Err(crate::error::value(
            "task agreement is all_agreed or a sourced selection of item ids",
        ));
    };
    if object["kind"].as_str() != Some("selection") {
        return Err(crate::error::value(
            "task agreement kind must be all_agreed or selection",
        ));
    }
    let ids = object["ids"]
        .as_array()
        .filter(|ids| !ids.is_empty() && ids.len() <= 10000);
    let Some(ids) = ids else {
        return Err(crate::error::value(
            "task selection must be a nonempty bounded list",
        ));
    };
    let mut strings = Vec::with_capacity(ids.len());
    for id in ids {
        strings.push(identifier(id)?);
    }
    if strings.iter().collect::<BTreeSet<_>>().len() != strings.len() {
        return Err(crate::error::value("duplicate agreed task id"));
    }
    strings.sort();
    Ok(json!({
        "kind": "selection",
        "ids": strings,
        "source": text(&object["source"], "task selection source", 512)?,
    }))
}

/// `_plan`: validate the agreed audit scope recorded for a checklist.
pub fn plan(value: &Value, site: &str) -> ProjectsResult<Value> {
    let object = value.as_object().filter(|o| {
        o.contains_key("reviewer")
            && o.contains_key("population")
            && o.keys()
                .all(|k| matches!(k.as_str(), "reviewer" | "population" | "tasks"))
    });
    let Some(object) = object else {
        return Err(crate::error::value("plan requires reviewer and population"));
    };
    Ok(json!({
        "reviewer": text(&object["reviewer"], "plan reviewer", 128)?,
        "population": population(&object["population"], site, false)?,
        "tasks": tasks(object.get("tasks").unwrap_or(&Value::Null))?,
    }))
}

/// `_stored_plan`: validate one plan already written into the history.
pub fn stored_plan(value: &Value, site: &str) -> ProjectsResult<Value> {
    let object = value.as_object().filter(|o| {
        ["recorded_at", "revision", "reviewer", "site", "population"]
            .iter()
            .all(|k| o.contains_key(*k))
            && o.keys().all(|k| {
                matches!(
                    k.as_str(),
                    "recorded_at" | "revision" | "reviewer" | "site" | "population" | "tasks"
                )
            })
    });
    let Some(object) = object else {
        return Err(crate::error::value("invalid audit scope plan"));
    };
    text(&object["recorded_at"], "plan recording time", 128)?;
    match object["revision"].as_i64() {
        Some(v) if !object["revision"].is_boolean() && v >= 1 => {}
        _ => return Err(crate::error::value("invalid plan revision")),
    }
    text(&object["reviewer"], "plan reviewer", 128)?;
    if object["site"].as_str() != Some(site) {
        return Err(crate::error::value(
            "plan site identity does not match the project",
        ));
    }
    population(&object["population"], site, false)?;
    let mut out = object.clone();
    out.insert(
        "tasks".into(),
        tasks(object.get("tasks").unwrap_or(&Value::Null))?,
    );
    Ok(Value::Object(out))
}

/// `set(...) != len(...)` duplicate check without requiring `Ord`/`Hash` on `Value`.
pub fn duplicated(values: &[Value]) -> bool {
    values
        .iter()
        .enumerate()
        .any(|(i, a)| values[i + 1..].contains(a))
}

/// `len(set(values))` without requiring `Ord`/`Hash` on `Value`.
pub fn unique_count(values: &[Value]) -> usize {
    let mut seen: Vec<&Value> = Vec::new();
    for v in values {
        if !seen.contains(&v) {
            seen.push(v);
        }
    }
    seen.len()
}
