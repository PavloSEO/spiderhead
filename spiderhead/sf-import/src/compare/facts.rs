//! Page-fact extraction, measurement gaps and preflight warnings: what a
//! comparison can and cannot honestly claim about the two runs it was given.

use std::collections::BTreeSet;
use std::sync::OnceLock;

use crate::pyvalue::py_repr_value;
use serde_json::{json, Map, Value};
use spiderhead_store::evidence_contract::Obj;

use crate::compare::base::{
    header_obj, run_of, ResolvedCorrespondence, CORRESPONDENCE_SCHEMA, RELEASE_REVIEW_SCHEMA,
};
use crate::pyvalue::{py_eq, truthy};
use crate::verification::AuditSource;

/// One measured-or-missing page attribute, with the row it was read from.
fn fact(value: Value, present: bool, source: &str) -> Obj {
    let state = if present {
        if value.is_null() {
            "absent"
        } else {
            "measured"
        }
    } else {
        "unavailable"
    };
    let mut row = Obj::new();
    row.insert("value".into(), if present { value } else { Value::Null });
    row.insert("state".into(), Value::String(state.into()));
    row.insert("source".into(), Value::String(source.into()));
    row.insert(
        "coverage".into(),
        Value::String(if present { "recorded" } else { "unavailable" }.into()),
    );
    row
}

/// The comparable page facts: status, title, description, h1, canonical and the
/// robots directives, each saying whether the crawl recorded it at all.
pub(crate) fn page_facts(page: &Obj) -> Obj {
    let empty = Map::new();
    let metrics = page
        .get("metrics")
        .and_then(Value::as_object)
        .unwrap_or(&empty);
    let mut facts = Obj::new();
    facts.insert(
        "status".into(),
        Value::Object(fact(
            page.get("status_code").cloned().unwrap_or(Value::Null),
            page.contains_key("status_code"),
            "pages.status_code",
        )),
    );
    facts.insert(
        "title".into(),
        Value::Object(fact(
            metrics.get("title").cloned().unwrap_or(Value::Null),
            metrics.contains_key("title"),
            "pages.metrics.title",
        )),
    );
    facts.insert(
        "description".into(),
        Value::Object(fact(
            metrics
                .get("meta_description")
                .cloned()
                .unwrap_or(Value::Null),
            metrics.contains_key("meta_description"),
            "pages.metrics.meta_description",
        )),
    );
    facts.insert(
        "h1".into(),
        Value::Object(fact(
            metrics.get("h1").cloned().unwrap_or(Value::Null),
            metrics.contains_key("h1"),
            "pages.metrics.h1",
        )),
    );
    facts.insert(
        "canonical".into(),
        Value::Object(fact(
            metrics.get("canonical").cloned().unwrap_or(Value::Null),
            metrics.contains_key("canonical"),
            "pages.metrics.canonical",
        )),
    );
    let robots_present = metrics.contains_key("meta_robots") || metrics.contains_key("x_robots");
    let mut robots = Obj::new();
    robots.insert(
        "meta".into(),
        metrics.get("meta_robots").cloned().unwrap_or(Value::Null),
    );
    robots.insert(
        "x_robots".into(),
        metrics.get("x_robots").cloned().unwrap_or(Value::Null),
    );
    facts.insert(
        "robots".into(),
        Value::Object(fact(
            Value::Object(robots),
            robots_present,
            "pages.metrics.meta_robots,x_robots",
        )),
    );
    facts
}

/// Matched pairs always resolve on both sides by construction; the shared empty
/// page only keeps a porting bug from panicking.
fn empty_page() -> &'static Obj {
    static EMPTY: OnceLock<Obj> = OnceLock::new();
    EMPTY.get_or_init(Obj::new)
}

/// The reviewed-release attachment: per-page fact diffs across a declared
/// correspondence, plus the provenance a reviewer needs to judge the release.
pub(crate) fn release_review(
    before: &AuditSource<'_>,
    after: &AuditSource<'_>,
    resolved: &ResolvedCorrespondence,
    result: &Obj,
    force: bool,
) -> Value {
    let mut facts: Vec<Value> = Vec::new();
    for pair in &resolved.pairs {
        if pair.get("state").and_then(Value::as_str) != Some("matched") {
            continue;
        }
        let before_url = pair
            .get("before_url")
            .and_then(Value::as_str)
            .unwrap_or_default();
        let after_url = pair
            .get("after_url")
            .and_then(Value::as_str)
            .unwrap_or_default();
        let before_facts = page_facts(
            resolved
                .before_pages
                .get(before_url)
                .unwrap_or(empty_page()),
        );
        let after_facts = page_facts(resolved.after_pages.get(after_url).unwrap_or(empty_page()));
        let mut changed: Vec<String> = before_facts
            .keys()
            .filter(|name| {
                let b = &before_facts[name.as_str()];
                let a = &after_facts[name.as_str()];
                b.get("state") != a.get("state")
                    || !py_eq(
                        b.get("value").unwrap_or(&Value::Null),
                        a.get("value").unwrap_or(&Value::Null),
                    )
            })
            .cloned()
            .collect();
        changed.sort();
        let mut row = Obj::new();
        row.insert("before_url".into(), pair["before_url"].clone());
        row.insert("after_url".into(), pair["after_url"].clone());
        row.insert(
            "correspondence_key".into(),
            pair["correspondence_key"].clone(),
        );
        row.insert("kind".into(), pair["kind"].clone());
        row.insert("host_changed".into(), pair["host_changed"].clone());
        row.insert("before".into(), Value::Object(before_facts));
        row.insert("after".into(), Value::Object(after_facts));
        row.insert(
            "changed".into(),
            Value::Array(changed.into_iter().map(Value::String).collect()),
        );
        facts.push(Value::Object(row));
    }
    let empty = Vec::new();
    json!({
        "schema_version": RELEASE_REVIEW_SCHEMA,
        "provenance": {
            "before_generated_at": run_of(header_obj(before)).get("generated_at").cloned().unwrap_or(Value::Null),
            "after_generated_at": run_of(header_obj(after)).get("generated_at").cloned().unwrap_or(Value::Null),
            "force": force,
            "warnings": result.get("warnings").cloned().unwrap_or(Value::Null),
            "compatibility": result.get("compatibility").cloned().unwrap_or(Value::Null),
        },
        "correspondence": {
            "schema_version": CORRESPONDENCE_SCHEMA,
            "origin_map": resolved.origin_map,
            "pairs": resolved.pairs,
        },
        "facts": facts,
        "findings": {
            "entered": result.get("entered").cloned().unwrap_or(Value::Array(empty.clone())),
            "left": result.get("left").cloned().unwrap_or(Value::Array(empty.clone())),
            "appeared": result.get("appeared").cloned().unwrap_or(Value::Array(empty.clone())),
            "disappeared": result.get("disappeared").cloned().unwrap_or(Value::Array(empty)),
        },
        "summary": result.get("summary").cloned().unwrap_or(Value::Null),
    })
}

/// Pages crawled in only one side. Partial crawls make absence unproven.
pub(crate) fn url_set_lists(
    before_urls: &BTreeSet<String>,
    after_urls: &BTreeSet<String>,
    unproven: bool,
) -> Value {
    let only_before: Vec<Value> = before_urls
        .difference(after_urls)
        .map(|u| Value::String(u.clone()))
        .collect();
    let only_after: Vec<Value> = after_urls
        .difference(before_urls)
        .map(|u| Value::String(u.clone()))
        .collect();
    json!({
        "only_in_before": only_before,
        "only_in_after": only_after,
        "counts": {
            "only_in_before": only_before.len(),
            "only_in_after": only_after.len(),
        },
        "unproven": unproven,
    })
}

/// Keep missing check execution distinct from a measured finding delta.
pub(crate) fn measurement_gaps(before: &AuditSource<'_>, after: &AuditSource<'_>) -> Vec<Obj> {
    let mut gaps = Vec::new();
    for (side, source) in [("before", before), ("after", after)] {
        for field in ["checks_skipped", "checks_disabled"] {
            let Some(items) = run_of(header_obj(source))
                .get(field)
                .and_then(Value::as_array)
            else {
                continue;
            };
            for item in items {
                let Some(item) = item.as_object() else {
                    continue;
                };
                let Some(id) = item.get("id").and_then(Value::as_str) else {
                    continue;
                };
                let mut gap = Obj::new();
                gap.insert("side".into(), Value::String(side.into()));
                gap.insert("check".into(), Value::String(id.into()));
                gap.insert(
                    "state".into(),
                    Value::String(field.strip_prefix("checks_").unwrap_or(field).into()),
                );
                gap.insert(
                    "reason".into(),
                    item.get("reason")
                        .filter(|r| truthy(r))
                        .cloned()
                        .unwrap_or_else(|| Value::String("no measurement available".into())),
                );
                gaps.push(gap);
            }
        }
    }
    gaps
}

/// The declared crawl configuration, or none when the run recorded no usable one.
fn cfg_of<'a>(source: &'a AuditSource<'_>) -> Option<&'a Value> {
    run_of(header_obj(source))
        .get("crawl_config")
        .filter(|v| !v.is_null())
}

/// Reasons a comparison would mislead, without refusing outright. A caller
/// decides whether to proceed; this only says what to distrust.
pub(crate) fn preflight(before: &AuditSource<'_>, after: &AuditSource<'_>) -> Vec<String> {
    let mut warnings: Vec<String> = Vec::new();
    for (label, source) in [("before", before), ("after", after)] {
        if matches!(
            run_of(header_obj(source)).get("crawl_valid"),
            Some(Value::Bool(false))
        ) {
            warnings.push(format!(
                "{label} crawl is marked invalid — it measured nothing usable"
            ));
        }
    }
    // A partial baseline and a partial current crawl poison opposite buckets, not
    // the same one: a page the before crawl never reached looks brand new to it,
    // and a page the after crawl never reached looks gone to it.
    if run_of(header_obj(before))
        .get("crawl_partial")
        .is_some_and(truthy)
    {
        warnings.push(
            "before crawl is partial — an 'appeared' finding may only mean the before \
             crawl did not reach that URL, not that the URL is new"
                .into(),
        );
    }
    if run_of(header_obj(after))
        .get("crawl_partial")
        .is_some_and(truthy)
    {
        warnings.push(
            "after crawl is partial — a 'disappeared' finding is not rechecked (the after \
             crawl did not fetch its URL), so it is not counted as fixed"
                .into(),
        );
    }
    let before_cfg = cfg_of(before);
    let after_cfg = cfg_of(after);
    match (before_cfg, after_cfg) {
        (None, _) | (_, None) => {
            let missing: Vec<&str> = [("before", before_cfg), ("after", after_cfg)]
                .iter()
                .filter(|(_, cfg)| cfg.is_none())
                .map(|(label, _)| *label)
                .collect();
            warnings.push(format!(
                "{} recorded no crawl configuration — comparability is unknown, not equal, \
                 so some of the difference may be the configuration rather than the site",
                missing.join(" and ")
            ));
        }
        (Some(a), Some(b)) => {
            if !py_eq(a, b) {
                let (Some(a), Some(b)) = (a.as_object(), b.as_object()) else {
                    // Non-object manifests always compare unequal; the key list
                    // below is only defined for objects.
                    warnings.push(
                        "results-affecting settings differ between the two runs, so some of the \
                         difference may be the configuration rather than the site: "
                            .into(),
                    );
                    return warnings;
                };
                let changed: Vec<String> = a
                    .keys()
                    .chain(b.keys())
                    .collect::<BTreeSet<_>>()
                    .into_iter()
                    .filter(|key| {
                        let x = a.get(key.as_str());
                        let y = b.get(key.as_str());
                        !match (x, y) {
                            (Some(x), Some(y)) => py_eq(x, y),
                            (None, None) => true,
                            _ => false,
                        }
                    })
                    .cloned()
                    .collect();
                warnings.push(format!(
                    "results-affecting settings differ between the two runs, so some of the \
                     difference may be the configuration rather than the site: {}",
                    changed.join(", ")
                ));
            }
        }
    }
    // crawl_config only exists for a native crawl; an SF-derived audit's nearest
    // equivalent is its export profile, which changes which checks had evidence.
    let before_profile = run_of(header_obj(before)).get("profile");
    let after_profile = run_of(header_obj(after)).get("profile");
    if let (Some(a), Some(b)) = (before_profile, after_profile) {
        if !py_eq(a, b) {
            warnings.push(format!(
                "SF export profile differs between the two runs ({} vs {}), so some of the \
                 difference may be the profile's check coverage rather than the site",
                py_repr_value(a),
                py_repr_value(b)
            ));
        }
    }
    for gap in measurement_gaps(before, after) {
        warnings.push(format!(
            "{} {} was {}: {}; finding differences are unverified where this check was not measured",
            gap["side"].as_str().unwrap_or_default(),
            gap["check"].as_str().unwrap_or_default(),
            gap["state"].as_str().unwrap_or_default(),
            gap["reason"].as_str().unwrap_or_default(),
        ));
    }
    warnings
}
