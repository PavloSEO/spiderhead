//! Checklist row construction: custom items, built-in reconciliation, v4 promotion

use serde_json::{json, Map, Value};

use super::document::definition;
use super::schema::{identifier, FORMAT_V4};
use crate::error::ProjectsResult;
use crate::pytime;

/// `_set`: upsert one definition and append it to the item's version history.
pub fn set(items: &mut Map<String, Value>, def: Value) {
    let item_id = def["id"].as_str().unwrap_or_default().to_owned();
    if !items.contains_key(&item_id) {
        let mut item = Map::new();
        item.insert("definition".into(), def.clone());
        item.insert("definitions".into(), json!([]));
        item.insert("records".into(), json!([]));
        if def.get("assignee").is_some() {
            item.insert("comments".into(), json!([]));
        }
        items.insert(item_id.clone(), Value::Object(item));
    }
    let item = items.get_mut(&item_id).unwrap();
    let differs = item["definition"] != def;
    let definitions = item["definitions"].as_array_mut().unwrap();
    if definitions.is_empty() || differs {
        definitions.push(json!({"observed_at": pytime::zulu(), "definition": def.clone()}));
        item["definition"] = def;
    }
}

fn custom_base(item_id: &str, site: &str) -> Value {
    json!({
        "id": item_id,
        "title": item_id,
        "kind": "custom",
        "scope": {"site": site, "template": null, "urls": []},
        "dependencies": [],
        "execution_kind": "manual",
        "priority": "P1",
        "priority_origin": "default",
        "enabled": true,
        "order": 0,
        "operation": null,
        "source_hash": null,
        "assignee": null,
        "description": null,
        "acceptance_criterion": null,
        "finding_ref": null,
        "method_ref": null,
        "competitors": [],
    })
}

/// `_custom`: validate one operator-supplied item against its (optional) prior state.
pub fn custom(
    raw: &Value,
    site: &str,
    catalogue: &Map<String, Value>,
    previous: Option<&Value>,
) -> ProjectsResult<Value> {
    let object = raw
        .as_object()
        .ok_or_else(|| crate::error::value("item must be an object"))?;
    let item_id = identifier(&object.get("id").cloned().unwrap_or(Value::Null))?;
    if !item_id.starts_with("custom:") && previous.is_none() {
        return Err(crate::error::value(
            "new operator items require the custom namespace",
        ));
    }
    if object.contains_key("priority_origin") {
        return Err(crate::error::value(
            "priority origin is assigned by the checklist, not input",
        ));
    }
    let base = previous
        .cloned()
        .unwrap_or_else(|| custom_base(&item_id, site));
    if object
        .keys()
        .any(|k| !base.as_object().unwrap().contains_key(k))
    {
        return Err(crate::error::value("unknown item definition fields"));
    }
    if let Some(prev) = previous {
        for k in ["id", "kind", "source_hash"] {
            let supplied = object.get(k).unwrap_or(&prev[k]);
            if supplied != &prev[k] {
                return Err(crate::error::value("item identity/source is immutable"));
            }
        }
    }
    let removed = previous.is_some()
        && previous.unwrap()["kind"].as_str() != Some("custom")
        && !catalogue.contains_key(&item_id);
    if removed
        && object.keys().any(|k| {
            !matches!(
                k.as_str(),
                "id" | "enabled" | "order" | "priority" | "title"
            )
        })
    {
        return Err(crate::error::value(
            "removed built-ins can only be disabled or relabeled",
        ));
    }
    let mut def = base.clone();
    for (k, v) in object {
        def[k] = v.clone();
    }
    if object.contains_key("priority") {
        def["priority_origin"] = json!("operator");
    }
    definition(&def, catalogue, removed)
}

/// `_builtin`: the checklist row built from one catalogue entry.
pub fn builtin(item_id: &str, entry: &Value, order: i64, site: &str) -> Value {
    let kind = entry["kind"].as_str().unwrap_or_default();
    json!({
        "id": item_id,
        "title": entry["title"],
        "kind": kind,
        "scope": {"site": site, "template": null, "urls": []},
        "dependencies": [],
        "execution_kind": if kind == "check" { "automatic" } else { "manual" },
        "priority": "P1",
        "priority_origin": "default",
        "enabled": true,
        "order": order,
        "operation": if kind == "check" { Value::from(item_id) } else { Value::Null },
        "source_hash": entry["definition_hash"],
    })
}

/// `_reconcile_builtins`: add or refresh every built-in row.
pub fn reconcile_builtins(
    items: &mut Map<String, Value>,
    catalogue: &Map<String, Value>,
    site: &str,
) -> ProjectsResult<()> {
    for (order, (item_id, entry)) in catalogue.iter().enumerate() {
        let def = if let Some(item) = items.get(item_id) {
            let mut def = item["definition"].clone();
            def["source_hash"] = entry["definition_hash"].clone();
            def["title"] = entry["title"].clone();
            def
        } else {
            builtin(item_id, entry, order as i64, site)
        };
        set(items, definition(&def, catalogue, false)?);
    }
    Ok(())
}

/// `_promote_v4`: add task metadata/comments while retaining every earlier record.
pub fn promote_v4(document: &mut Value) {
    let defaults: Vec<(&str, Value)> = vec![
        ("assignee", Value::Null),
        ("description", Value::Null),
        ("acceptance_criterion", Value::Null),
        ("finding_ref", Value::Null),
        ("method_ref", Value::Null),
        ("competitors", json!([])),
    ];
    if let Some(items) = document["items"].as_object_mut() {
        for item in items.values_mut() {
            if let Some(def) = item["definition"].as_object_mut() {
                for (key, value) in &defaults {
                    def.entry((*key).to_owned())
                        .or_insert_with(|| value.clone());
                }
            }
            if let Some(definitions) = item["definitions"].as_array_mut() {
                for version in definitions {
                    if let Some(def) = version["definition"].as_object_mut() {
                        for (key, value) in &defaults {
                            def.entry((*key).to_owned())
                                .or_insert_with(|| value.clone());
                        }
                    }
                }
            }
            item.as_object_mut()
                .unwrap()
                .entry("comments".to_owned())
                .or_insert_with(|| json!([]));
        }
    }
    if document.get("format").and_then(Value::as_str) != Some(FORMAT_V4) {
        document["format"] = json!(FORMAT_V4);
        document["version"] = json!(4);
        document
            .as_object_mut()
            .unwrap()
            .entry("priority_policy".to_owned())
            .or_insert_with(|| json!({"applications": []}));
        document
            .as_object_mut()
            .unwrap()
            .entry("plans".to_owned())
            .or_insert_with(|| json!([]));
    }
}
