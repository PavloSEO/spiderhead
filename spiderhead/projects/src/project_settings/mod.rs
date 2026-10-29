//! Revisioned project labels and owner-reviewed agent setting proposals

use std::path::{Path, PathBuf};

use serde_json::{json, Value};
use uuid::Uuid;

use crate::error::{ProjectsError, ProjectsResult};
use crate::pytime;
use crate::runtime::{
    project_policy, project_write_lock, read_document, validate_policy, write_document,
};
use crate::workspace::load as workspace_load;

mod list_proposals;
pub use list_proposals::*;

/// Settings document name.
pub const SETTINGS_FILE: &str = "project-settings.json";
/// Proposals document name.
pub const PROPOSALS_FILE: &str = "settings-proposals.json";
/// Settings format.
pub const SETTINGS_FORMAT: &str = "spiderhead.project-settings.v1";
/// Proposals format.
pub const PROPOSALS_FORMAT: &str = "spiderhead.project-settings-proposals.v1";
const MAX_PROPOSALS: usize = 100;

fn label(value: &Value) -> ProjectsResult<String> {
    let Some(text) = value.as_str() else {
        return Err(crate::error::value(
            "label must be non-empty text of at most 128 characters",
        ));
    };
    if text.is_empty()
        || text.len() > 128
        || text
            .chars()
            .any(|c| ('\u{0}'..='\u{1f}').contains(&c) || c == '\u{7f}')
        || text.trim().is_empty()
    {
        return Err(crate::error::value(
            "label must be non-empty text of at most 128 characters",
        ));
    }
    Ok(text.trim().to_owned())
}

fn hex32(value: &str) -> bool {
    value.len() == 32
        && value
            .chars()
            .all(|c| c.is_ascii_hexdigit() && !c.is_ascii_uppercase())
}

fn utc() -> String {
    pytime::zulu()
}

fn settings(directory: &Path) -> ProjectsResult<(PathBuf, Value, Option<Value>)> {
    let (root, project) = workspace_load(directory)?;
    let mut current = read_document(&root, SETTINGS_FILE)?;
    if let Some(document) = current.as_mut() {
        let keys: std::collections::BTreeSet<_> = document
            .as_object()
            .map(|o| o.keys().map(String::as_str).collect())
            .unwrap_or_default();
        let shape = keys
            == ["format", "version", "project_uuid", "revision", "settings"]
                .into_iter()
                .collect()
            || keys
                == [
                    "format",
                    "version",
                    "project_uuid",
                    "revision",
                    "settings",
                    "last_operation_id",
                ]
                .into_iter()
                .collect();
        let label_ok = document["settings"]
            .as_object()
            .map(|s| s.keys().map(String::as_str).collect::<Vec<_>>() == vec!["label"])
            .unwrap_or(false)
            && document["settings"]["label"].as_str().is_some_and(|l| {
                !l.is_empty()
                    && l.len() <= 128
                    && !l
                        .chars()
                        .any(|c| ('\u{0}'..='\u{1f}').contains(&c) || c == '\u{7f}')
                    && !l.trim().is_empty()
            });
        let operation_ok = match document.get("last_operation_id") {
            None | Some(Value::Null) => true,
            Some(v) => v.as_str().is_some_and(hex32),
        };
        if !shape
            || document["format"].as_str() != Some(SETTINGS_FORMAT)
            || document["version"].as_i64() != Some(1)
            || document["version"].is_boolean()
            || document["project_uuid"] != project["project_uuid"]
            || document["revision"].as_i64().is_none_or(|r| r < 1)
            || document["revision"].is_boolean()
            || !label_ok
            || !operation_ok
        {
            return Err(crate::error::value(
                "project settings have an unsupported shape",
            ));
        }
        if !document
            .as_object()
            .unwrap()
            .contains_key("last_operation_id")
        {
            document
                .as_object_mut()
                .unwrap()
                .insert("last_operation_id".into(), Value::Null);
        }
    }
    Ok((root, project, current))
}

/// `read_settings`: read project settings; legacy projects start at revision zero.
pub fn read_settings(directory: &Path) -> ProjectsResult<Value> {
    let (_root, project, current) = settings(directory)?;
    Ok(json!({
        "ok": true,
        "revision": current.as_ref().map_or(0, |c| c["revision"].as_i64().unwrap_or(0)),
        "operation_id": current.as_ref().map_or(Value::Null, |c| c["last_operation_id"].clone()),
        "settings": current.map(|c| c["settings"].clone()).unwrap_or_else(|| {
            serde_json::json!({"label": project["site"]["label"].clone()})
        }),
    }))
}

/// `update_label`: update the project label with an optimistic revision check.
pub fn update_label(
    directory: &Path,
    label_value: &str,
    expected_revision: i64,
    client: &str,
    operation_id: Option<&str>,
    log_action: bool,
) -> ProjectsResult<Value> {
    let (root, _project) = workspace_load(directory)?;
    let _guard = project_write_lock(&root, 5.0)?;
    if !crate::action_log::CLIENTS.contains(&client) {
        return Err(crate::error::value("client must be a supported interface"));
    }
    if expected_revision < 0 {
        return Err(crate::error::value(
            "expected_revision must be a nonnegative integer",
        ));
    }
    let label_value = label(&json!(label_value))?;
    let operation_id = match operation_id {
        None => Uuid::new_v4().simple().to_string(),
        Some(op) if hex32(op) => op.to_owned(),
        _ => return Err(crate::error::value("operation_id must be a UUID hex value")),
    };
    let (root, project, current) = settings(directory)?;
    let revision = current
        .as_ref()
        .map_or(0, |c| c["revision"].as_i64().unwrap_or(0));
    if expected_revision != revision {
        return Err(crate::error::value(format!(
            "project settings revision conflict: current revision is {revision}"
        )));
    }
    let pending = log_action
        .then(|| {
            crate::action_log::record_root(
                &root,
                client,
                "project.settings.update",
                "pending",
                "Project label update pending; terminal result is reserved.",
                None,
                Some(&operation_id),
            )
        })
        .transpose()?;
    let pending_sequence = pending
        .as_ref()
        .and_then(|p| p["action"]["sequence"].as_i64());
    let document = json!({
        "format": SETTINGS_FORMAT,
        "version": 1,
        "project_uuid": project["project_uuid"],
        "revision": revision + 1,
        "settings": {"label": label_value},
        "last_operation_id": operation_id,
    });
    if let Err(e) = write_document(&root, SETTINGS_FILE, &document, Some(revision)) {
        if let Some(sequence) = pending_sequence {
            let _ = crate::action_log::resolve_pending_root(
                &root,
                sequence,
                "stale",
                "Project label update did not apply.",
                None,
                Some(&operation_id),
            );
        }
        return Err(e);
    }
    let mut result = read_settings(&root)?;
    if let Some(sequence) = pending_sequence {
        if crate::action_log::resolve_pending_root(
            &root,
            sequence,
            "applied",
            &format!("Updated project label at revision {}.", result["revision"]),
            None,
            Some(&operation_id),
        )
        .is_err()
        {
            result["audit_warning"] = json!("Label was saved; the action row remains pending.");
        }
    }
    Ok(result)
}

fn validate_proposal(value: &Value) -> ProjectsResult<Value> {
    let Some(object) = value.as_object() else {
        return Err(crate::error::value(
            "proposal must change exactly one supported setting",
        ));
    };
    if object.len() != 1 {
        return Err(crate::error::value(
            "proposal must change exactly one supported setting",
        ));
    }
    if let Some(label_value) = object.get("label") {
        return Ok(json!({"label": label(label_value)?}));
    }
    if let Some(policy) = object.get("crawl_policy") {
        return Ok(json!({"crawl_policy": validate_policy(policy)?}));
    }
    Err(crate::error::value(
        "proposal supports label or crawl_policy settings only",
    ))
}

fn proposal_timestamp(stamp: &Value) -> ProjectsResult<()> {
    let Some(text) = stamp.as_str() else {
        return Err(crate::error::value(
            "settings proposal timestamps must be RFC3339 UTC",
        ));
    };
    let parsed = pytime::fromisoformat(&text.replace('Z', "+00:00"))
        .map_err(|_| crate::error::value("settings proposal timestamps must be RFC3339 UTC"))?;
    if !parsed.is_utc() {
        return Err(crate::error::value(
            "settings proposal timestamps must be RFC3339 UTC",
        ));
    }
    Ok(())
}

fn proposal_document(directory: &Path) -> ProjectsResult<(PathBuf, Value, Option<Value>)> {
    let (root, project) = workspace_load(directory)?;
    let mut current = read_document(&root, PROPOSALS_FILE)?;
    if let Some(document) = &mut current {
        let keys: std::collections::BTreeSet<_> = document
            .as_object()
            .map(|o| o.keys().map(String::as_str).collect())
            .unwrap_or_default();
        let proposals = document["proposals"].as_array();
        if keys
            != ["format", "version", "project_uuid", "revision", "proposals"]
                .into_iter()
                .collect()
            || document["format"].as_str() != Some(PROPOSALS_FORMAT)
            || document["version"].as_i64() != Some(1)
            || document["version"].is_boolean()
            || document["project_uuid"] != project["project_uuid"]
            || document["revision"].as_i64().is_none_or(|r| r < 1)
            || document["revision"].is_boolean()
            || proposals.is_none_or(|p| p.len() > MAX_PROPOSALS)
        {
            return Err(crate::error::value(
                "settings proposal store has an unsupported shape",
            ));
        }
        let legacy_keys: std::collections::BTreeSet<&str> = [
            "id",
            "created_at",
            "client",
            "status",
            "kind",
            "base_revision",
            "changes",
            "decided_at",
        ]
        .into_iter()
        .collect();
        let full_keys: std::collections::BTreeSet<&str> = [
            "id",
            "created_at",
            "client",
            "status",
            "kind",
            "base_revision",
            "changes",
            "decided_at",
            "action_sequence",
            "decision",
        ]
        .into_iter()
        .collect();
        let proposals = document["proposals"].as_array_mut().unwrap();
        for proposal in proposals.iter_mut() {
            if let Some(object) = proposal.as_object_mut() {
                let keys: std::collections::BTreeSet<_> =
                    object.keys().map(String::as_str).collect();
                if keys == legacy_keys {
                    let status = object["status"].as_str();
                    let decision = match status {
                        Some("denied") => json!("deny"),
                        Some("applied" | "stale") => json!("approve"),
                        _ => Value::Null,
                    };
                    object.insert("action_sequence".into(), Value::Null);
                    object.insert("decision".into(), decision);
                }
            }
            let object = proposal.as_object().filter(|o| {
                o.keys()
                    .map(String::as_str)
                    .collect::<std::collections::BTreeSet<_>>()
                    == full_keys
            });
            let Some(object) = object else {
                return Err(crate::error::value(
                    "settings proposal has an unsupported shape",
                ));
            };
            let id_ok = object["id"].as_str().is_some_and(hex32);
            let client_ok =
                crate::action_log::CLIENTS.contains(&object["client"].as_str().unwrap_or_default());
            let status = object["status"].as_str().unwrap_or_default();
            let status_ok = ["pending", "applying", "applied", "denied", "stale"].contains(&status);
            let kind = object["kind"].as_str().unwrap_or_default();
            let kind_ok = ["project_settings", "crawl_policy"].contains(&kind);
            let base_ok = object["base_revision"].as_i64().is_some_and(|r| r >= 0)
                && !object["base_revision"].is_boolean();
            let seq_ok = match &object["action_sequence"] {
                Value::Null => true,
                v => v.as_i64().is_some_and(|s| s >= 1) && !v.is_boolean(),
            };
            let decision_ok = match &object["decision"] {
                Value::Null => true,
                v => ["approve", "deny"].contains(&v.as_str().unwrap_or_default()),
            };
            if !(id_ok && client_ok && status_ok && kind_ok && base_ok && seq_ok && decision_ok) {
                return Err(crate::error::value("settings proposal metadata is invalid"));
            }
            if !object["changes"].is_object() {
                return Err(crate::error::value(
                    "settings proposal changes must be an object",
                ));
            }
            let changes = validate_proposal(&object["changes"])?;
            let expected_kind = if changes.get("label").is_some() {
                "project_settings"
            } else {
                "crawl_policy"
            };
            if expected_kind != kind {
                return Err(crate::error::value(
                    "settings proposal kind does not match its changes",
                ));
            }
            proposal_timestamp(&object["created_at"])?;
            if !object["decided_at"].is_null() {
                proposal_timestamp(&object["decided_at"])?;
            }
            let pending_like = matches!(status, "pending" | "applying");
            if pending_like != object["decided_at"].is_null() {
                return Err(crate::error::value(
                    "settings proposal decision time does not match its status",
                ));
            }
            if status == "pending" && !object["decision"].is_null() {
                return Err(crate::error::value(
                    "pending settings proposal cannot have a decision",
                ));
            }
            if status == "applying" && object["decision"].as_str() != Some("approve") {
                return Err(crate::error::value(
                    "applying settings proposal must be approved",
                ));
            }
            if matches!(status, "applied" | "denied" | "stale") && object["decision"].is_null() {
                return Err(crate::error::value(
                    "decided settings proposal must retain its decision",
                ));
            }
        }
    }
    Ok((root, project, current))
}

/// `propose_settings`: record one agent proposal; never applies or authorizes.
pub fn propose_settings(
    directory: &Path,
    changes: &Value,
    client: &str,
    expected_revision: i64,
) -> ProjectsResult<Value> {
    let (root, _project) = workspace_load(directory)?;
    let _guard = project_write_lock(&root, 5.0)?;
    if !crate::action_log::CLIENTS.contains(&client) {
        return Err(crate::error::value("client must be a supported interface"));
    }
    if expected_revision < 0 {
        return Err(crate::error::value(
            "expected_revision must be a nonnegative integer",
        ));
    }
    let (root, project, current) = proposal_document(directory)?;
    let revision = current
        .as_ref()
        .map_or(0, |c| c["revision"].as_i64().unwrap_or(0));
    if expected_revision != revision {
        return Err(crate::error::value(format!(
            "proposal revision conflict: current revision is {revision}"
        )));
    }
    let mut proposals = current
        .as_ref()
        .map(|c| c["proposals"].as_array().cloned().unwrap_or_default())
        .unwrap_or_default();
    if proposals.len() >= MAX_PROPOSALS {
        return Err(crate::error::value(
            "project settings proposal queue is full",
        ));
    }
    let changes = validate_proposal(changes)?;
    let kind = if changes.get("label").is_some() {
        "project_settings"
    } else {
        "crawl_policy"
    };
    let base_revision = if kind == "project_settings" {
        read_settings(&root)?["revision"].as_i64().unwrap_or(0)
    } else {
        project_policy(&root, None, false, None, None)?["revision"]
            .as_i64()
            .unwrap_or(0)
    };
    let proposal_id = Uuid::new_v4().simple().to_string();
    let action = crate::action_log::record_root(
        &root,
        client,
        "project.settings.propose",
        "pending",
        "Settings proposal pending; terminal review result is reserved.",
        Some(&proposal_id),
        None,
    )?;
    let mut proposal = json!({
        "id": proposal_id,
        "created_at": utc(),
        "client": client,
        "status": "pending",
        "kind": kind,
        "base_revision": base_revision,
        "changes": changes,
        "decided_at": null,
        "decision": null,
        "action_sequence": action["action"]["sequence"],
    });
    proposals.push(proposal.clone());
    proposal["action_sequence"] = action["action"]["sequence"].clone();
    let document = json!({
        "format": PROPOSALS_FORMAT,
        "version": 1,
        "project_uuid": project["project_uuid"],
        "revision": revision + 1,
        "proposals": proposals,
    });
    if let Err(e) = write_document(&root, PROPOSALS_FILE, &document, Some(revision)) {
        let _ = crate::action_log::resolve_pending_root(
            &root,
            action["action"]["sequence"].as_i64().unwrap_or(0),
            "refused",
            "Settings proposal was not persisted.",
            Some(&proposal_id),
            None,
        );
        return Err(e);
    }
    Ok(json!({"ok": true, "revision": document["revision"], "proposal": proposal}))
}
