//! Coverage item definitions and whole-document validation.

use std::collections::{BTreeMap, BTreeSet, HashMap};

use regex::Regex;
use serde_json::{Map, Value};
use std::sync::LazyLock;

use super::policy::{application_time, priority_receipt};
use super::schema::{
    completion_hash, identifier, scope as check_scope, stored_plan, text, FORMAT, FORMAT_V2,
    FORMAT_V3, FORMAT_V4,
};
use crate::error::ProjectsResult;
use crate::pytime;
use crate::workspace::target as workspace_target;

mod validate_document;
pub use validate_document::*;

fn hash_re() -> &'static Regex {
    static RE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"[a-f0-9]{64}\z").unwrap());
    &RE
}

const REQUIRED: &[&str] = &[
    "id",
    "title",
    "kind",
    "scope",
    "dependencies",
    "execution_kind",
    "priority",
    "priority_origin",
    "enabled",
    "order",
    "operation",
    "source_hash",
];

const TASK_FIELDS: &[&str] = &[
    "assignee",
    "description",
    "acceptance_criterion",
    "finding_ref",
    "method_ref",
    "competitors",
];

/// `_definition`: validate one checklist row; `historical` skips catalogue membership.
pub fn definition(
    value: &Value,
    catalogue: &Map<String, Value>,
    historical: bool,
) -> ProjectsResult<Value> {
    let object = value
        .as_object()
        .ok_or_else(|| crate::error::value("item definition has unsupported fields"))?;
    let keys: BTreeSet<&str> = object.keys().map(String::as_str).collect();
    let required: BTreeSet<&str> = REQUIRED.iter().copied().collect();
    let mut full = required.clone();
    full.extend(TASK_FIELDS.iter().copied());
    if keys != required && keys != full {
        return Err(crate::error::value(
            "item definition has unsupported fields",
        ));
    }
    let mut def = object.clone();
    let item_id = identifier(&def["id"])?;
    text(&def["title"], "title", 512)?;
    let kind = def["kind"].as_str().unwrap_or_default();
    if !matches!(kind, "check" | "skill" | "scenario" | "custom")
        || !item_id.starts_with(&format!("{kind}:"))
    {
        return Err(crate::error::value("item kind does not match identifier"));
    }
    if !historical && kind != "custom" && !catalogue.contains_key(&item_id) {
        return Err(crate::error::value("unknown built-in item"));
    }
    def.insert("scope".into(), check_scope(&def["scope"])?);
    if !matches!(
        def["execution_kind"].as_str(),
        Some("automatic" | "manual" | "deliverable")
    ) {
        return Err(crate::error::value("invalid execution kind"));
    }
    if !matches!(def["priority"].as_str(), Some("P0" | "P1" | "P2")) {
        return Err(crate::error::value("invalid priority"));
    }
    if !matches!(
        def["priority_origin"].as_str(),
        Some("default" | "operator" | "policy")
    ) {
        return Err(crate::error::value("invalid priority origin"));
    }
    let enabled_ok = def["enabled"].is_boolean();
    let order_ok = def["order"]
        .as_i64()
        .is_some_and(|o| (0..=1_000_000).contains(&o))
        && !def["order"].is_boolean();
    if !enabled_ok || !order_ok {
        return Err(crate::error::value("invalid enabled/order value"));
    }
    let dependencies = def["dependencies"].as_array().filter(|d| d.len() <= 1000);
    let Some(dependencies) = dependencies else {
        return Err(crate::error::value("dependencies must be a bounded list"));
    };
    for dep in dependencies {
        identifier(dep)?;
    }
    if dependencies
        .iter()
        .any(|d| d.as_str() == Some(item_id.as_str()))
        || super::schema::duplicated(dependencies)
    {
        return Err(crate::error::value("duplicate or self dependency"));
    }
    let operation = &def["operation"];
    if def["execution_kind"].as_str() == Some("automatic") {
        let op = operation.as_str().unwrap_or_default();
        let bound = !historical
            && catalogue
                .get(op)
                .is_some_and(|e| e["kind"].as_str() == Some("check"));
        if !op.starts_with("check:") || (!historical && !bound) {
            return Err(crate::error::value(
                "automatic items bind to a registered check",
            ));
        }
    } else if !operation.is_null() {
        return Err(crate::error::value(
            "manual/deliverable items do not execute operations",
        ));
    }
    if !def["source_hash"].is_null()
        && !def["source_hash"]
            .as_str()
            .is_some_and(|s| hash_re().is_match(s))
    {
        return Err(crate::error::value("invalid source definition hash"));
    }
    if TASK_FIELDS.iter().all(|f| def.contains_key(*f)) {
        for (field, limit) in [
            ("assignee", 128),
            ("description", 2048),
            ("acceptance_criterion", 2048),
        ] {
            if !def[field].is_null() {
                text(&def[field], field, limit)?;
            }
        }
        if !def["finding_ref"].is_null() {
            let reference = def["finding_ref"].as_object().filter(|r| {
                r.keys().map(String::as_str).collect::<BTreeSet<_>>()
                    == ["scan_id", "finding_id"].into_iter().collect()
            });
            let Some(reference) = reference else {
                return Err(crate::error::value(
                    "finding_ref requires scan_id and finding_id",
                ));
            };
            text(&reference["scan_id"], "finding_ref scan_id", 128)?;
            text(&reference["finding_id"], "finding_ref finding_id", 256)?;
        }
        let method_ref = &def["method_ref"];
        if !method_ref.is_null() {
            let ok = method_ref.as_str().is_some_and(|m| {
                m.len() <= 256 && (m.starts_with("skill:") || m.starts_with("scenario:"))
            });
            if !ok {
                return Err(crate::error::value(
                    "method_ref must identify a registered skill or scenario",
                ));
            }
        }
        let competitors = def["competitors"].as_array().filter(|c| c.len() <= 20);
        let Some(competitors) = competitors else {
            return Err(crate::error::value(
                "competitors must be a bounded list of at most 20 sites",
            ));
        };
        let mut identities = BTreeSet::new();
        let primary_netloc = crate::urls::urlsplit(&workspace_target(
            def["scope"]["site"].as_str().unwrap_or_default(),
        )?)?
        .netloc
        .to_lowercase();
        for competitor in competitors {
            let object = competitor.as_object().filter(|o| {
                o.keys().map(String::as_str).collect::<BTreeSet<_>>()
                    == ["url", "max_urls"].into_iter().collect()
            });
            let Some(object) = object else {
                return Err(crate::error::value(
                    "each competitor requires url and max_urls",
                ));
            };
            let url = text(&object["url"], "competitor URL", 2048)?;
            let normalized = workspace_target(&url)?;
            let parsed = crate::urls::urlsplit(&normalized)?;
            if !matches!(parsed.scheme.as_str(), "http" | "https") || parsed.hostname().is_none() {
                return Err(crate::error::value(
                    "competitor URL must be an HTTP(S) site",
                ));
            }
            if parsed.netloc.to_lowercase() == primary_netloc {
                return Err(crate::error::value(
                    "the task's primary site cannot also be a competitor",
                ));
            }
            if !identities.insert(normalized) {
                return Err(crate::error::value("duplicate competitor URL"));
            }
            match object["max_urls"].as_i64() {
                Some(v) if !object["max_urls"].is_boolean() && (1..=1_000_000).contains(&v) => {}
                _ => {
                    return Err(crate::error::value(
                        "competitor max_urls must be 1..1,000,000",
                    ))
                }
            }
        }
    }
    Ok(Value::Object(def))
}

/// `_dependencies`: guard the dependency graph — no cycles, depth ≤ 100.
pub fn dependencies(items: &Map<String, Value>) -> ProjectsResult<()> {
    let mut done = BTreeSet::new();
    let mut active = BTreeSet::new();
    fn visit(
        item_id: &str,
        items: &Map<String, Value>,
        done: &mut BTreeSet<String>,
        active: &mut BTreeSet<String>,
    ) -> ProjectsResult<()> {
        if active.contains(item_id) {
            return Err(crate::error::value("cyclic checklist dependency"));
        }
        if done.contains(item_id) {
            return Ok(());
        }
        let Some(item) = items.get(item_id) else {
            return Err(crate::error::value(format!("unknown dependency {item_id}")));
        };
        if active.len() >= 100 {
            return Err(crate::error::value(
                "dependency chain exceeds supported depth",
            ));
        }
        active.insert(item_id.to_owned());
        let deps = item["definition"]["dependencies"]
            .as_array()
            .cloned()
            .unwrap_or_default();
        for dep in deps {
            visit(dep.as_str().unwrap_or_default(), items, done, active)?;
        }
        active.remove(item_id);
        done.insert(item_id.to_owned());
        Ok(())
    }
    for item_id in items.keys() {
        visit(item_id, items, &mut done, &mut active)?;
    }
    Ok(())
}

/// `_task_comment_shape`: one comment's shape.
pub fn task_comment_shape(value: &Value) -> ProjectsResult<()> {
    let object = value.as_object().filter(|o| {
        let keys: BTreeSet<&str> = o.keys().map(String::as_str).collect();
        keys == ["actor", "text", "created_at"].into_iter().collect()
            || keys
                == ["actor", "text", "created_at", "reference"]
                    .into_iter()
                    .collect()
    });
    let Some(object) = object else {
        return Err(crate::error::value("invalid task comment"));
    };
    text(&object["actor"], "comment actor", 128)?;
    text(&object["text"], "comment text", 8000)?;
    text(&object["created_at"], "comment timestamp", 128)?;
    if let Some(reference) = object.get("reference") {
        let object = reference.as_object().filter(|r| {
            matches!(
                r.get("kind").and_then(Value::as_str),
                Some("scan" | "finding")
            )
        });
        let Some(object) = object else {
            return Err(crate::error::value(
                "comment reference must identify a scan or finding",
            ));
        };
        let kind = object["kind"].as_str().unwrap_or_default();
        let expected: BTreeSet<&str> = if kind == "scan" {
            ["kind", "id"].into_iter().collect()
        } else {
            ["kind", "scan_id", "id"].into_iter().collect()
        };
        if object.keys().map(String::as_str).collect::<BTreeSet<_>>() != expected {
            return Err(crate::error::value(
                "comment reference has an invalid shape",
            ));
        }
        text(&object["id"], "comment reference id", 256)?;
        if kind == "finding" {
            text(&object["scan_id"], "comment reference scan_id", 128)?;
        }
    }
    Ok(())
}

const RECORD_KEYS: &[&str] = &[
    "status",
    "reason",
    "measurement",
    "reviewer",
    "signoff",
    "artifact",
    "sha256",
    "review",
    "scope",
    "site",
    "operation",
    "operation_hash",
    "source",
    "observed_at",
    "definition_hash",
    "recorded_at",
    "evidence",
    "revision",
    "artifact_verification",
    "actor",
];

/// `_record_shape`: validate one execution record against its item's definition hashes.
pub fn record_shape(
    record: &Value,
    definition_hashes: &BTreeMap<String, Value>,
) -> ProjectsResult<()> {
    let object = record.as_object().filter(|o| {
        o.keys().all(|k| RECORD_KEYS.contains(&k.as_str()))
            && [
                "status",
                "reason",
                "measurement",
                "definition_hash",
                "recorded_at",
            ]
            .iter()
            .all(|k| o.contains_key(*k))
    });
    let Some(object) = object else {
        return Err(crate::error::value("invalid execution history entry"));
    };
    if !matches!(
        object["status"].as_str(),
        Some("running" | "failed" | "unavailable" | "succeeded" | "not_applicable")
    ) {
        return Err(crate::error::value("invalid execution history status"));
    }
    text(&object["reason"], "record reason", 2048)?;
    text(&object["recorded_at"], "record timestamp", 128)?;
    if object.contains_key("actor") {
        text(&object["actor"], "record actor", 128)?;
    }
    if object.contains_key("evidence") {
        text(&object["evidence"], "exclusion evidence", 512)?;
    }
    if let Some(revision) = object.get("revision") {
        match revision.as_i64() {
            Some(v) if !revision.is_boolean() && v >= 1 => {}
            _ => return Err(crate::error::value("invalid record revision")),
        }
    }
    let def_hash = object["definition_hash"].as_str().unwrap_or_default();
    if !definition_hashes.contains_key(def_hash) {
        return Err(crate::error::value(
            "execution refers to an unknown definition",
        ));
    }
    if !object["measurement"].is_null() {
        let ok = object["measurement"]
            .as_object()
            .and_then(|m| m.get("state"))
            .and_then(Value::as_str)
            .is_some_and(|s| matches!(s, "limited" | "measured" | "not_measured"));
        if !ok {
            return Err(crate::error::value("invalid measurement record"));
        }
    }
    if object.contains_key("artifact") {
        text(&object["artifact"], "artifact", 1024)?;
        if !object["sha256"]
            .as_str()
            .is_some_and(|s| hash_re().is_match(s))
        {
            return Err(crate::error::value("invalid evidence digest"));
        }
    }
    if let Some(receipt) = object.get("artifact_verification") {
        let ok = receipt.as_object().is_some_and(|r| {
            object.contains_key("artifact")
                && r.keys().map(String::as_str).collect::<BTreeSet<_>>()
                    == ["sha256", "identity", "verified_at"].into_iter().collect()
                && r["sha256"].as_str().is_some_and(|s| hash_re().is_match(s))
                && r["identity"]
                    .as_array()
                    .is_some_and(|i| i.len() == 5 && i.iter().all(|v| v.is_i64() || v.is_u64()))
        });
        if !ok {
            return Err(crate::error::value("invalid artifact verification receipt"));
        }
        text(
            &receipt["verified_at"],
            "artifact verification timestamp",
            128,
        )?;
        let aware = pytime::fromisoformat(
            &receipt["verified_at"]
                .as_str()
                .unwrap_or_default()
                .replace('Z', "+00:00"),
        )
        .map(|p| p.is_aware())
        .unwrap_or(false);
        if !aware {
            return Err(crate::error::value(
                "artifact verification timestamp must include timezone",
            ));
        }
    }
    if object.contains_key("operation") {
        identifier(&object["operation"])?;
        if !object["operation_hash"]
            .as_str()
            .is_some_and(|s| hash_re().is_match(s))
        {
            return Err(crate::error::value("invalid operation digest"));
        }
    }
    let status = object["status"].as_str().unwrap_or_default();
    if matches!(status, "not_applicable" | "succeeded") && !object.contains_key("operation") {
        text(
            object.get("reviewer").unwrap_or(&Value::Null),
            "reviewer",
            128,
        )?;
    }
    if status == "succeeded"
        && !(object.contains_key("artifact")
            || object.get("signoff").and_then(Value::as_bool) == Some(true))
    {
        return Err(crate::error::value(
            "successful history requires evidence or signoff",
        ));
    }
    if status == "succeeded" {
        let definition = &definition_hashes[def_hash];
        if definition["execution_kind"].as_str() == Some("automatic") {
            let ok = object["operation"].as_str() == definition["operation"].as_str()
                && object.contains_key("artifact")
                && !object["measurement"].is_null();
            if !ok {
                return Err(crate::error::value(
                    "automatic history is missing bound measurement evidence",
                ));
            }
        } else {
            let ok = (definition["execution_kind"].as_str() == Some("manual")
                && object.get("signoff").and_then(Value::as_bool) == Some(true))
                || object["review"].as_str() == Some("approved");
            if !ok {
                return Err(crate::error::value("review history is missing approval"));
            }
        }
    }
    Ok(())
}
