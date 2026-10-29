//! Portable checklist definitions, append-only history and offline completion views
//! Submodules: `schema`, `policy`, `document`, `store`, `items` and `views`.
//!
//! All mutations run inside one `.coverage.lock` transaction: a fresh validated read, an
//! optimistic revision check, whole-document validation and an atomic publish.

use spiderhead_core::filesystem::PrivateMode;
use std::fs;
use std::path::Path;

use serde_json::{json, Map, Value};

use crate::error::{ProjectsError, ProjectsResult};
use crate::workspace::load as workspace_load;
use spiderhead_core::filesystem::atomic_write_bytes;
use spiderhead_core::formats::matches;

pub mod document;
pub mod items;
pub mod policy;
pub mod schema;
pub mod store;
pub mod views;

pub use schema::{
    completion_hash, hash, identifier, plan as check_plan, population, scope as check_scope,
    stored_plan, tasks as check_tasks, text, urls as check_urls, FORMAT, FORMAT_V2, FORMAT_V3,
    FORMAT_V4, MAX_BYTES, URL_ENUMERATION_LIMIT,
};

mod status;
pub use status::*;

/// `core.common.utc_iso_z` — the UTC stamp written into documents.
pub fn now() -> String {
    crate::pytime::zulu()
}

/// `_transaction`: one locked read-modify-validate-publish pass over `coverage.json`.
///
/// `body` receives `(root, project, document)`; if the document is unchanged the pass is a
/// no-op (no revision bump, no publish).
pub fn transaction<T>(
    directory: &Path,
    expected_revision: Option<i64>,
    body: impl FnOnce(&Path, &Value, &mut Value) -> ProjectsResult<T>,
) -> ProjectsResult<T> {
    let (root, project) = workspace_load(directory)?;
    let lock = root.join(".coverage.lock");
    match std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .private_mode(0o600)
        .open(&lock)
    {
        Ok(mut file) => {
            use std::io::Write;
            let _ = file.write_all(format!("{}\n", std::process::id()).as_bytes());
        }
        Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {
            return Err(crate::error::value(store::busy_reason(&root, &lock)));
        }
        Err(e) => return Err(ProjectsError::Io(e)),
    }
    let result = (|| -> ProjectsResult<T> {
        let document = store::read(&root, &project)?;
        let revision = document
            .as_ref()
            .and_then(|d| d["revision"].as_i64())
            .unwrap_or(0);
        if let Some(expected) = expected_revision {
            if expected != revision {
                return Err(crate::error::value(format!(
                    "coverage revision conflict: current revision is {revision}"
                )));
            }
        }
        let mut document = document.unwrap_or_else(|| {
            json!({
                "format": FORMAT,
                "version": 1,
                "project_uuid": project["project_uuid"],
                "revision": 0,
                "items": {},
            })
        });
        let original = document.clone();
        let outcome = body(&root, &project, &mut document)?;
        if document == original {
            return Ok(outcome);
        }
        document["revision"] = json!(document["revision"].as_i64().unwrap_or(0) + 1);
        document::validate_in_place(&mut document, &project)?;
        let content = crate::pyjson::dumps_indent2(&document) + "\n";
        if content.len() > MAX_BYTES {
            return Err(crate::error::value("coverage exceeds its byte limit"));
        }
        atomic_write_bytes(
            &root.join("coverage.json"),
            content.as_bytes(),
            None,
            None,
            true,
        )
        .map_err(ProjectsError::Io)?;
        Ok(outcome)
    })();
    let _ = fs::remove_file(&lock);
    result
}

/// `_materialize_builtin`: goal-only checklists gain catalogue rows only when touched.
fn materialize_builtin(document: &mut Value, item_id: &str, project: &Value) -> ProjectsResult<()> {
    let catalogue = crate::catalogue::catalogue()?.clone();
    let is_v4 = document.get("format").and_then(Value::as_str) == Some(FORMAT_V4);
    let items = document["items"].as_object_mut().unwrap();
    if !items.contains_key(item_id) && catalogue.contains_key(item_id) {
        let site = project["site"]["target"].as_str().unwrap_or_default();
        if is_v4 {
            let mut def = items::builtin(item_id, &catalogue[item_id], items.len() as i64, site);
            for (key, value) in [
                ("assignee", Value::Null),
                ("description", Value::Null),
                ("acceptance_criterion", Value::Null),
                ("finding_ref", Value::Null),
                ("method_ref", Value::Null),
                ("competitors", json!([])),
            ] {
                def[key] = value;
            }
            items::set(items, def);
        } else {
            items::reconcile_builtins(items, &catalogue, site)?;
        }
    }
    Ok(())
}

/// `initialize_coverage`: reconcile built-ins, apply a template, record the agreed scope.
pub fn initialize_coverage(directory: &Path, template: Option<&Value>) -> ProjectsResult<Value> {
    initialize_coverage_full(directory, template, None, None)
}

/// `initialize_coverage` with a revision guard and optional plan arguments.
pub fn initialize_coverage_full(
    directory: &Path,
    template: Option<&Value>,
    expected_revision: Option<i64>,
    plan: Option<&Value>,
) -> ProjectsResult<Value> {
    let catalogue = crate::catalogue::catalogue()?.clone();
    if let Some(t) = template {
        let ok = t.as_object().is_some_and(|o| {
            o.keys()
                .map(String::as_str)
                .collect::<std::collections::BTreeSet<_>>()
                == ["format", "items"].into_iter().collect()
                && matches(o["format"].as_str(), "spiderhead.checklist-template.v1")
                && o["items"].is_array()
        });
        if !ok {
            return Err(crate::error::value("unsupported checklist template"));
        }
    }
    transaction(directory, expected_revision, |_root, project, document| {
        let site = project["site"]["target"]
            .as_str()
            .unwrap_or_default()
            .to_owned();
        let items = document["items"].as_object_mut().unwrap().clone();
        let mut items = items;
        items::reconcile_builtins(&mut items, &catalogue, &site)?;
        document["items"] = Value::Object(items);
        if document.get("format").and_then(Value::as_str) == Some(FORMAT_V4) || template.is_some() {
            items::promote_v4(document);
        }
        let mut seen = std::collections::BTreeSet::new();
        for value in template
            .and_then(|t| t["items"].as_array())
            .cloned()
            .unwrap_or_default()
        {
            if !value.is_object() {
                return Err(crate::error::value("invalid template item"));
            }
            let id = identifier(&value["id"])?;
            if !seen.insert(id.clone()) {
                return Err(crate::error::value("invalid or duplicate template item"));
            }
            let previous = document["items"]
                .get(&id)
                .and_then(|i| i.get("definition"))
                .cloned();
            let def = items::custom(&value, &site, &catalogue, previous.as_ref())?;
            items::set(document["items"].as_object_mut().unwrap(), def);
        }
        if let Some(plan) = plan {
            let stored = check_plan(plan, &site)?;
            if stored["tasks"]["kind"].as_str() == Some("selection") {
                let ids = stored["tasks"]["ids"]
                    .as_array()
                    .cloned()
                    .unwrap_or_default();
                if ids.iter().any(|id| {
                    !document["items"]
                        .as_object()
                        .unwrap()
                        .contains_key(id.as_str().unwrap_or_default())
                }) {
                    return Err(crate::error::value(
                        "agreed task ids must exist in the reconciled checklist",
                    ));
                }
            }
            if document.get("format").and_then(Value::as_str) != Some(FORMAT_V4) {
                document["format"] = json!(FORMAT_V3);
                document["version"] = json!(3);
            }
            document
                .as_object_mut()
                .unwrap()
                .entry("priority_policy".to_owned())
                .or_insert_with(|| json!({"applications": []}));
            let agreement = {
                let mut a = stored.clone();
                a["site"] = project["site"]["target"].clone();
                a
            };
            let next_revision = document["revision"].as_i64().unwrap_or(0) + 1;
            let plans = document
                .as_object_mut()
                .unwrap()
                .entry("plans".to_owned())
                .or_insert_with(|| json!([]));
            let current = plans.as_array().and_then(|p| p.last().cloned());
            let differs = current.as_ref().is_none_or(|current| {
                agreement
                    .as_object()
                    .unwrap()
                    .iter()
                    .any(|(key, value)| current.get(key) != Some(value))
            });
            if current.is_none() || differs {
                let revision = next_revision;
                let mut entry = stored.clone();
                entry["recorded_at"] = json!(now());
                entry["revision"] = json!(revision);
                entry["site"] = project["site"]["target"].clone();
                plans.as_array_mut().unwrap().push(entry);
            }
        }
        Ok(())
    })?;
    coverage_status(directory, true)
}

/// `ensure_goal_task`: create one manual custom task for an accepted goal.
pub fn ensure_goal_task(
    directory: &Path,
    item_id: &str,
    title: &str,
    description: Option<&str>,
) -> ProjectsResult<()> {
    let catalogue = crate::catalogue::catalogue()?.clone();
    transaction(directory, None, |_root, project, document| {
        items::promote_v4(document);
        if !document["items"].as_object().unwrap().contains_key(item_id) {
            let site = project["site"]["target"]
                .as_str()
                .unwrap_or_default()
                .to_owned();
            let def = items::custom(
                &json!({"id": item_id, "title": title, "description": description}),
                &site,
                &catalogue,
                None,
            )?;
            items::set(document["items"].as_object_mut().unwrap(), def);
        }
        Ok(())
    })
}

/// `update_item`: add or edit one item, preserving definitions and result history.
pub fn update_item(
    directory: &Path,
    item: &Value,
    expected_revision: i64,
) -> ProjectsResult<Value> {
    if !item.is_object() {
        return Err(crate::error::value("item must be an object"));
    }
    identifier(&item["id"])?;
    let catalogue = crate::catalogue::catalogue()?.clone();
    transaction(
        directory,
        Some(expected_revision),
        |_root, project, document| {
            if document["revision"].as_i64() == Some(0) {
                return Err(crate::error::value(
                    "initialize the checklist first: run project-checklist-init",
                ));
            }
            items::promote_v4(document);
            materialize_builtin(document, item["id"].as_str().unwrap_or_default(), project)?;
            let previous = document["items"]
                .get(item["id"].as_str().unwrap_or_default())
                .and_then(|i| i.get("definition"))
                .cloned();
            let site = project["site"]["target"]
                .as_str()
                .unwrap_or_default()
                .to_owned();
            let def = items::custom(item, &site, &catalogue, previous.as_ref())?;
            items::set(document["items"].as_object_mut().unwrap(), def);
            Ok(())
        },
    )?;
    coverage_status(directory, true)
}

/// `append_task_comment`: an attributed owner/agent note with an optional typed reference.
pub fn append_task_comment(
    directory: &Path,
    item_id: &str,
    text_value: &str,
    actor: &str,
    expected_revision: i64,
    reference: Option<&Value>,
) -> ProjectsResult<Value> {
    identifier(&json!(item_id))?;
    text(&json!(actor), "comment actor", 128)?;
    text(&json!(text_value), "comment text", 8000)?;
    let mut entry = json!({"actor": actor, "text": text_value, "created_at": now()});
    if let Some(reference) = reference {
        entry["reference"] = reference.clone();
    }
    document::task_comment_shape(&entry)?;
    transaction(
        directory,
        Some(expected_revision),
        |_root, _project, document| {
            if document["revision"].as_i64() == Some(0) {
                return Err(crate::error::value(
                    "initialize the checklist first: run project-checklist-init",
                ));
            }
            items::promote_v4(document);
            let items = document["items"].as_object_mut().unwrap();
            let item = items
                .get_mut(item_id)
                .filter(|i| i["definition"]["kind"].as_str() == Some("custom"));
            let Some(item) = item else {
                return Err(crate::error::value(
                    "comments apply to current custom tasks",
                ));
            };
            let comments = item
                .as_object_mut()
                .unwrap()
                .entry("comments".to_owned())
                .or_insert_with(|| json!([]));
            comments.as_array_mut().unwrap().push(entry.clone());
            if comments.as_array().unwrap().len() > 10000 {
                return Err(crate::error::value(
                    "task comment history exceeds its bound",
                ));
            }
            Ok(())
        },
    )?;
    coverage_status(directory, true)
}

/// `record_execution`: record an attempt or applicability review; never executes anything.
pub fn record_execution(
    directory: &Path,
    item_id: &str,
    record: &Value,
    expected_revision: i64,
) -> ProjectsResult<Value> {
    identifier(&json!(item_id))?;
    if !record.is_object() {
        return Err(crate::error::value("record must be an object"));
    }
    transaction(
        directory,
        Some(expected_revision),
        |root, project, document| {
            if record.get("actor").is_some() {
                items::promote_v4(document);
            }
            materialize_builtin(document, item_id, project)?;
            if !document["items"].as_object().unwrap().contains_key(item_id) {
                return Err(crate::error::value("unknown checklist item"));
            }
            let catalogue = crate::catalogue::catalogue()?.clone();
            let view = status(root, document.clone(), &catalogue, Some(project), true)?;
            let current = view["items"]
                .as_array()
                .and_then(|rows| rows.iter().find(|r| r["id"].as_str() == Some(item_id)))
                .cloned()
                .ok_or_else(|| crate::error::value("unknown checklist item"))?;
            if record["status"].as_str() == Some("succeeded")
                && !current["blocked_by"]
                    .as_array()
                    .unwrap_or(&Vec::new())
                    .is_empty()
            {
                return Err(crate::error::value("dependencies are not complete"));
            }
            let item = document["items"][item_id].clone();
            if item["definition"]["enabled"].as_bool() == Some(false)
                && record["status"].as_str() != Some("not_applicable")
            {
                return Err(crate::error::value("item is disabled"));
            }
            if record["status"].as_str() == Some("succeeded") {
                document::definition(&item["definition"], &catalogue, false)?;
                if item["definition"]["kind"].as_str() != Some("custom")
                    && item["definition"]["source_hash"].as_str()
                        != catalogue
                            .get(item_id)
                            .and_then(|e| e["definition_hash"].as_str())
                {
                    return Err(crate::error::value(
                        "source definition changed; reconcile checklist first",
                    ));
                }
            }
            let validated = crate::evidence::validate_record(root, &item["definition"], record)?;
            let records = document["items"][item_id]["records"]
                .as_array()
                .cloned()
                .unwrap_or_default();
            let mut records = records;
            let mut entry = validated;
            entry["definition_hash"] = json!(completion_hash(&item["definition"])?);
            entry["recorded_at"] = json!(now());
            entry["revision"] = json!(document["revision"].as_i64().unwrap_or(0) + 1);
            records.push(entry);
            document["items"][item_id]["records"] = Value::Array(records);
            Ok(())
        },
    )?;
    crate::journal::note(
        directory,
        "agent",
        "agent",
        &format!(
            "checklist {item_id}: {}",
            record["status"].as_str().unwrap_or_default()
        ),
    );
    coverage_status(directory, true)
}
