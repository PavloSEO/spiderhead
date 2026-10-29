//! Split out of `cloudflare`.

use super::*;

/// The `info` dict shape returned by `resolve_zone`/`resolve_zone_id` — `row["id"]`
/// and `row["name"]` are hard subscripts (`KeyError` when missing, `TypeError` on a
/// non-dict row); the `(x or {}).get(...)` members raise `AttributeError` on a truthy
/// non-dict `account`/`plan`.
pub(crate) fn zone_info(row: &Value) -> Result<Value, CfError> {
    static EMPTY: std::sync::LazyLock<Value> = std::sync::LazyLock::new(|| json!({}));
    let id = py_subscript(row, "id")?;
    let name = py_subscript(row, "name")?;
    let account = match obj_get(row, "account")? {
        Some(v) if pystr::truthy(Some(v)) => {
            if v.is_object() {
                v.clone()
            } else {
                return Err(attr_get_error(v));
            }
        }
        _ => EMPTY.clone(),
    };
    let plan = match obj_get(row, "plan")? {
        Some(v) if pystr::truthy(Some(v)) => {
            if v.is_object() {
                v.clone()
            } else {
                return Err(attr_get_error(v));
            }
        }
        _ => EMPTY.clone(),
    };
    Ok(json!({
        "id": id.clone(),
        "name": name.clone(),
        "account": account.get("id").cloned().unwrap_or(Value::Null),
        "plan": plan.get("name").cloned().unwrap_or(Value::Null),
        "status": row.get("status").cloned().unwrap_or(Value::Null),
    }))
}

/// `resolve_zone` without the transport half — the payload checks for the async path.
pub(crate) fn zone_lookup(payload: &Value, zone: &str) -> Result<Value, CfError> {
    if !pystr::truthy(obj_get(payload, "success")?) {
        return cf(format!("zone lookup failed: {}", errors_or_attr(payload)?));
    }
    let rows = match obj_get(payload, "result")? {
        Some(v) if pystr::truthy(Some(v)) => v.clone(),
        _ => Value::Array(Vec::new()),
    };
    if !pystr::truthy(Some(&rows)) {
        return cf(format!(
            "zone {} is not visible to this token",
            pystr::repr_str(zone)
        ));
    }
    let row = py_index0(&rows)?;
    zone_info(&row)
}

/// `resolve_zone_id` without the transport half.
pub(crate) fn zone_id_lookup(payload: &Value, zone_id: &str) -> Result<Value, CfError> {
    if !pystr::truthy(obj_get(payload, "success")?) {
        return cf(format!("zone lookup failed: {}", errors_or_attr(payload)?));
    }
    match obj_get(payload, "result")? {
        Some(row) if row.is_object() && row.get("id") == Some(&json!(zone_id)) => zone_info(row),
        _ => cf("bound Cloudflare zone is not visible to this token"),
    }
}

/// `resolve_zone(zone, token, transport)` — zone lookup by name.
pub fn resolve_zone(zone: &str, token: &str, send: &CfTransport) -> Result<Value, CfError> {
    let query = http::urlencode(&[("name".to_owned(), zone.to_owned())]);
    let payload = send("GET", &format!("{API}/zones?{query}"), None, token)?;
    zone_lookup(&payload, zone)
}

/// `resolve_zone_id(zone_id, token, transport)` — the read-only zone endpoint.
pub fn resolve_zone_id(zone_id: &str, token: &str, send: &CfTransport) -> Result<Value, CfError> {
    let quoted = http::quote(zone_id, "");
    let payload = send("GET", &format!("{API}/zones/{quoted}"), None, token)?;
    zone_id_lookup(&payload, zone_id)
}

/// The parsed-payload half of `_zone_setting` (transport errors pre-decoded to
/// `Err`/`Null` by the caller).
pub(super) fn zone_setting_payload(payload: Result<&Value, &CfError>, setting: &str) -> Value {
    let payload = match payload {
        Ok(p) => p,
        Err(_) => return json!({"state": "unavailable", "reason": "zone settings read failed"}),
    };
    let result = payload.get("result");
    if !payload.is_object()
        || !pystr::truthy(payload.get("success"))
        || !result.is_some_and(Value::is_object)
        || result.and_then(|r| r.get("id")) != Some(&json!(setting))
    {
        return json!({"state": "unavailable", "reason": "zone setting is unavailable or not readable"});
    }
    let result = result.cloned().unwrap_or_default();
    let mut value = result.get("value").cloned().unwrap_or(Value::Null);
    if let Some(s) = value.as_str() {
        value = json!(s.chars().take(256).collect::<String>());
    } else if !value.is_null() && !value.is_boolean() && !value.is_number() {
        value = Value::Null;
    }
    json!({
        "state": "measured",
        "value": value,
        "editable": result.get("editable").cloned().unwrap_or(Value::Null),
        "modified_on": result.get("modified_on").cloned().unwrap_or(Value::Null),
    })
}

/// The parsed-payload half of `_ruleset`.
pub(super) fn ruleset_payload(payload: Result<&Value, &CfError>) -> Value {
    let payload = match payload {
        Ok(p) => p,
        Err(_) => {
            return json!({"state": "unavailable", "reason": "ruleset read failed", "rules": []})
        }
    };
    let value = payload.get("result");
    let rules = value.and_then(|v| v.get("rules")).and_then(Value::as_array);
    let valid = payload.is_object()
        && pystr::truthy(payload.get("success"))
        && rules.is_some_and(|r| r.iter().all(Value::is_object));
    if !valid {
        return json!({
            "state": "unavailable",
            "reason": "ruleset is unavailable or not readable",
            "rules": [],
        });
    }
    let rules = rules.cloned().unwrap_or_default();
    let truncated = rules.len() > MAX_RULES;
    let trimmed: Vec<Value> = rules
        .iter()
        .take(MAX_RULES)
        .map(|rule| {
            let mut out = serde_json::Map::new();
            for key in [
                "id",
                "description",
                "action",
                "enabled",
                "expression",
                "ref",
            ] {
                if let Some(v) = rule.get(key) {
                    let clipped = if key == "expression" {
                        v.as_str()
                            .map(|s| json!(s.chars().take(1000).collect::<String>()))
                            .unwrap_or_else(|| v.clone())
                    } else if key == "description" {
                        v.as_str()
                            .map(|s| json!(s.chars().take(200).collect::<String>()))
                            .unwrap_or_else(|| v.clone())
                    } else {
                        v.clone()
                    };
                    out.insert(key.to_owned(), clipped);
                }
            }
            Value::Object(out)
        })
        .collect();
    json!({
        "state": if truncated { "partial" } else { "complete" },
        "rules": trimmed,
        "truncated": truncated,
        "modified_on": value.and_then(|v| v.get("last_updated")).cloned().unwrap_or(Value::Null),
    })
}

/// The parsed-payload half of `_logpush_jobs`.
pub(super) fn logpush_payload(payload: Result<&Value, &CfError>) -> Value {
    let payload = match payload {
        Ok(p) => p,
        Err(_) => {
            return json!({"state": "unavailable", "reason": "Logpush jobs read failed", "jobs": []})
        }
    };
    let rows = payload.get("result").and_then(Value::as_array);
    if !payload.is_object() || !pystr::truthy(payload.get("success")) || rows.is_none() {
        return json!({
            "state": "unavailable",
            "reason": "Logpush jobs are unavailable or not readable",
            "jobs": [],
        });
    }
    let rows = rows.cloned().unwrap_or_default();
    if !rows.iter().all(Value::is_object) {
        return json!({
            "state": "unavailable",
            "reason": "Logpush jobs response is malformed",
            "jobs": [],
        });
    }
    let info = payload.get("result_info").cloned().unwrap_or(Value::Null);
    let total = if info.is_object() {
        info.get("total_count")
            .filter(|v| pystr::is_py_int(v))
            .and_then(Value::as_i64)
    } else {
        None
    };
    let jobs: Vec<Value> = rows
        .iter()
        .take(MAX_CONFIG_ITEMS as usize)
        .map(|row| {
            let mut out = serde_json::Map::new();
            for key in [
                "id",
                "dataset",
                "enabled",
                "kind",
                "frequency",
                "max_upload_bytes",
                "max_upload_records",
            ] {
                if let Some(v) = row.get(key) {
                    out.insert(key.to_owned(), v.clone());
                }
            }
            Value::Object(out)
        })
        .collect();
    json!({
        "state": if rows.len() >= MAX_CONFIG_ITEMS as usize
            || total.is_some_and(|t| t > rows.len() as i64)
        { "partial" } else { "complete" },
        "total": total.map_or(Value::Null, |t| json!(t)),
        "jobs": jobs,
        "destinations_omitted": true,
        "reason": "This read lists configured jobs only; destination data must be read from the customer-owned target.",
    })
}
