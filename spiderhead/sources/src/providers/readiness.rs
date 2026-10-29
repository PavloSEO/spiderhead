//! Readiness checks: credential state and evidence envelopes for each provider operation.

use super::*;

/// Readiness of one provider operation: whether it can run, and why not.
pub fn provider_readiness(
    env: &ProviderEnv,
    provider: Option<&Value>,
    operation: Option<&Value>,
) -> Value {
    if let Some(op) = operation {
        if !op.is_null() && op.as_str().map(|s| s.trim().is_empty()).unwrap_or(true) {
            return json!({"ok": false, "state": "invalid", "error": "operation must be a non-empty string"});
        }
    }
    if operation.filter(|o| !o.is_null()).is_some() && provider.filter(|p| !p.is_null()).is_none() {
        return json!({"ok": false, "state": "invalid", "error": "provider is required when selecting an operation"});
    }
    if let Some(p) = provider {
        if p.as_str().map(|s| s.trim().is_empty()).unwrap_or(true) {
            return json!({"ok": false, "state": "invalid", "error": "provider must be a non-empty string"});
        }
    }
    let doctor = sources_doctor(env);
    let statuses = doctor["providers"].clone();
    let provider = provider.filter(|p| !p.is_null());
    let operation = operation.filter(|o| !o.is_null());
    let Some(provider_value) = provider else {
        return json!({
            "ok": true,
            "format": "spiderhead.provider-readiness.v1",
            "verification_performed": false,
            "providers": statuses,
            "note": "configured credentials are not verified account or target access",
        });
    };
    let name = provider_value
        .as_str()
        .unwrap_or_default()
        .trim()
        .to_owned();
    if registry_entry(&name).is_none() {
        return json!({"ok": false, "state": "unsupported", "error": "unsupported provider"});
    }
    let status = &statuses[&name];
    let (_, access, _, quota, privacy) = registry_entry(&name).copied().unwrap();
    let mut result = json!({
        "ok": true,
        "format": "spiderhead.provider-readiness.v1",
        "provider": name,
        "state": status["readiness_state"],
        "readiness_state": status["readiness_state"],
        "verified": false,
        "permission_state": "not_verified",
        "target_access": "not_requested",
        "credential_components": status["credential_components"],
        "credential_sources": status["credential_sources"],
        "access": access,
        "quota_mode": quota,
        "privacy_class": privacy,
        "operations": status["operations"],
        "verification_surface": if matches!(name.as_str(), "gsc" | "yandex_webmaster" | "bing_webmaster") {
            json!("provider-verify")
        } else {
            Value::Null
        },
        "unavailable_reason": status.get("unavailable_reason").cloned().unwrap_or(Value::Null),
    });
    if let Some(op_value) = operation {
        let op_name = op_value.as_str().unwrap_or_default().trim().to_owned();
        let ops = status["operations"].as_array().cloned().unwrap_or_default();
        let selected = ops
            .iter()
            .find(|item| item.get("name") == Some(&json!(op_name)))
            .cloned();
        let Some(selected) = selected else {
            let mut out = result.clone();
            out["ok"] = json!(false);
            out["state"] = json!("unsupported");
            out["readiness_state"] = status["readiness_state"].clone();
            out["operation"] = json!({"name": op_name, "state": "unsupported"});
            out["error"] = json!("unsupported provider operation");
            return out;
        };
        result["operations"] = json!([selected.clone()]);
        result["operation"] = selected.clone();
        if selected.get("state") == Some(&json!("unsupported")) {
            result["ok"] = json!(false);
            result["state"] = json!("unsupported");
            result["readiness_state"] = status["readiness_state"].clone();
            result["error"] = json!("declared operation has no supported route");
        }
    }
    result
}

/// Length of a sized value (array, string, object); other values are a type error.
pub(super) fn py_len(value: &Value) -> Result<i64, CfError> {
    match value {
        Value::Array(a) => Ok(a.len() as i64),
        Value::Object(o) => Ok(o.len() as i64),
        Value::String(t) => Ok(t.chars().count() as i64),
        other => Err(CfError::Python(
            "TypeError",
            format!("object of type '{}' has no len()", pystr::type_name(other)),
        )),
    }
}

/// Filter summary with values redacted.
pub fn redacted_filters(value: Option<&Value>) -> Value {
    match value {
        Some(Value::Object(map)) if !map.is_empty() => {
            let mut keys: Vec<String> = map.keys().cloned().collect();
            keys.sort();
            json!({"state": "values_redacted", "keys": keys})
        }
        _ => json!({"state": "not_supplied", "keys": []}),
    }
}

/// Save a result under its SHA-256 and return the `local-artifact:<sha256>` reference.
pub fn save_local_artifact(directory: &str, value: &Value) -> Result<String, CfError> {
    use spiderhead_core::filesystem::{set_permissions_mode, StatCompat};
    let root = PathBuf::from(directory);
    if root
        .symlink_metadata()
        .map(|m| m.file_type().is_symlink())
        .unwrap_or(false)
    {
        return Err(CfError::Value(
            "provider artifact directory must not be a symlink".to_owned(),
        ));
    }
    let existed = root.is_dir();
    std::fs::create_dir_all(&root)
        .map_err(|e| CfError::Python("OSError", format!("[Errno 13] {e}")))?;
    if !existed {
        set_permissions_mode(&root, 0o700)
            .map_err(|e| CfError::Python("OSError", format!("[Errno 13] {e}")))?;
    }
    let info = root
        .symlink_metadata()
        .map_err(|e| CfError::Python("OSError", format!("[Errno 2] {e}")))?;
    if !info.is_dir() || !credentials::is_private_mode(info.mode()) {
        return Err(CfError::Value(
            "provider artifact directory must be a private directory".to_owned(),
        ));
    }
    let digest_text = pyjson::dumps_opts(value, true, true);
    use sha2::Digest;
    let digest = format!("{:x}", sha2::Sha256::digest(digest_text.as_bytes()));
    let destination = root.join(format!("provider-{}.json", &digest[..16]));
    let body = pyjson::dumps_opts(value, true, false);
    let tmp = destination.with_extension("json.tmp");
    std::fs::write(&tmp, body.as_bytes())
        .map_err(|e| CfError::Python("OSError", format!("[Errno 13] {e}")))?;
    set_permissions_mode(&tmp, 0o600)
        .map_err(|e| CfError::Python("OSError", format!("[Errno 13] {e}")))?;
    std::fs::rename(&tmp, &destination)
        .map_err(|e| CfError::Python("OSError", format!("[Errno 13] {e}")))?;
    Ok(format!("local-artifact:{digest}"))
}

pub(super) fn first_nonempty<'a>(result: &'a Value, keys: &[&str]) -> Option<&'a Value> {
    keys.iter()
        .filter_map(|k| result.get(k))
        .find(|v| pystr::truthy(Some(v)))
}

/// Build the evidence envelope for one provider call.
pub fn evidence(
    env: &ProviderEnv,
    provider: &str,
    operation: &str,
    request: &Value,
    result: &Value,
    artifact: Option<&str>,
) -> Result<Value, CfError> {
    let raw_state = result.get("state").and_then(Value::as_str);
    let state = match raw_state {
        Some(s)
            if matches!(
                s,
                "complete" | "partial" | "failed" | "skipped" | "no_field_data" | "not_configured"
            ) =>
        {
            s.to_owned()
        }
        _ => {
            if pystr::truthy(result.get("ok")) {
                "complete".to_owned()
            } else {
                "failed".to_owned()
            }
        }
    };
    let rows = first_nonempty(result, &["rows", "samples", "url_facts", "summary"])
        .cloned()
        .unwrap_or(json!([]));
    let row_count = py_len(&rows)?;
    let returned = result
        .get("returned")
        .cloned()
        .unwrap_or_else(|| json!(row_count));
    let quota = result
        .get("quota_mode")
        .filter(|v| pystr::truthy(Some(v)))
        .cloned()
        .unwrap_or_else(|| json!(registry_entry(provider).map(|e| e.3).unwrap_or("unknown")));
    Ok(json!({
        "format": EVIDENCE_FORMAT,
        "provider": provider,
        "operation": operation,
        "retrieved_at": (env.now)(),
        "period": request.get("period").filter(|v| pystr::truthy(Some(v))).or_else(|| result.get("period").filter(|v| pystr::truthy(Some(v)))).or_else(|| result.get("collection_period")).cloned().unwrap_or(Value::Null),
        "dimensions": request.get("dimensions").or_else(|| result.get("dimensions")).filter(|v| pystr::truthy(Some(v))).cloned().unwrap_or(json!([])),
        "filters": redacted_filters(request.get("filters")),
        "target_reference": Value::Null,
        "pagination": {"returned": returned, "truncated": pystr::truthy(result.get("truncated"))},
        "row_counts": {"returned": returned},
        "sampling": result.get("sampled").cloned().unwrap_or_else(|| result.get("sampling_or_thresholding").cloned().unwrap_or_else(|| result.get("sampling").cloned().unwrap_or(json!("unknown")))),
        "privacy_thresholds": result.get("thresholded").cloned().unwrap_or_else(|| result.get("privacy_thresholds").cloned().unwrap_or(json!("unknown"))),
        "timezone": result.get("timezone").cloned().unwrap_or(Value::Null),
        "attribution": result.get("attribution").cloned().unwrap_or(Value::Null),
        "quota_state": quota,
        "status": state,
        "complete": state == "complete",
        "artifact_reference": artifact.map(Value::from).unwrap_or(Value::Null),
        "redaction": "target identifiers, filter values, and raw provider rows are restricted local artifacts by default",
        "error": result.get("error").cloned().unwrap_or(Value::Null),
        "failure_kind": result.get("failure_kind").cloned().unwrap_or(Value::Null),
    }))
}

/// Classify a permission failure in a result, for the readiness state.
pub fn permission_failure_state(result: &Value) -> &'static str {
    let error = result
        .get("error")
        .map(|v| pyjson::str_value(v).unwrap_or_default())
        .unwrap_or_default()
        .to_lowercase()
        .replace('_', " ");
    if error.contains("insufficient scope")
        || (error.contains("insufficient") && error.contains("scope"))
    {
        return "insufficient_scope";
    }
    if result.get("status") == Some(&json!(401))
        || error.contains("invalid grant")
        || error.contains("invalid token")
    {
        return "invalid";
    }
    if matches!(
        result.get("token_status").and_then(Value::as_str),
        Some("disabled" | "expired")
    ) {
        return "invalid";
    }
    if result.get("status") == Some(&json!(403)) {
        return "not_granted";
    }
    "verification_failed"
}
