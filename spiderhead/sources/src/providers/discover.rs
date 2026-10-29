//! Split out of `providers`.

use super::*;

/// `provider_discover(provider, *, domain)` — lists visible resources only.
pub fn provider_discover(
    env: &ProviderEnv,
    provider: &str,
    domain: Option<&Value>,
) -> Result<Value, CfError> {
    if !matches!(provider, "google" | "yandex" | "topvisor") {
        return Err(CfError::Value(
            "provider must be google, yandex, or topvisor".to_owned(),
        ));
    }
    let domain_text = match domain {
        None | Some(Value::Null) => None,
        Some(Value::String(s)) if !s.trim().is_empty() && s.len() <= 253 => {
            Some(s.trim().to_owned())
        }
        Some(_) => {
            return Err(CfError::Value(
                "domain must be a bounded hostname".to_owned(),
            ))
        }
    };
    let (result, mut resources): (Value, Vec<Value>) = match provider {
        "google" => {
            let result = (env.dispatch)("gsc.discover_properties", None)?;
            let rows = if result.get("ok") == Some(&json!(true)) {
                result
                    .get("properties")
                    .and_then(Value::as_array)
                    .cloned()
                    .unwrap_or_default()
            } else {
                Vec::new()
            };
            let items = rows
                .iter()
                .filter(|row| row.is_object())
                .filter_map(|row| row.get("site_url").and_then(Value::as_str))
                .map(|url| json!({"service": "gsc", "resource": url, "label": url}))
                .collect();
            (result, items)
        }
        "yandex" => {
            let result = (env.dispatch)(
                "yandex_webmaster.collect",
                Some(&json!({"operation": "hosts", "kwargs": {}})),
            )?;
            let hosts = if result.get("ok") == Some(&json!(true)) {
                result
                    .get("data")
                    .and_then(Value::as_object)
                    .and_then(|d| d.get("hosts"))
                    .and_then(Value::as_array)
                    .cloned()
                    .unwrap_or_default()
            } else {
                Vec::new()
            };
            let items = hosts
                .iter()
                .filter(|row| row.is_object())
                .filter_map(|row| {
                    row.get("host_id").and_then(Value::as_str).map(|id| {
                        json!({
                            "service": "webmaster",
                            "resource": id,
                            "label": row.get("ascii_host_url"),
                        })
                    })
                })
                .collect();
            (result, items)
        }
        _ => {
            let result = (env.dispatch)(
                "topvisor.fetch",
                Some(
                    &json!({"operation": "projects", "params": {"limit": 100, "offset": 0}, "kwargs": {}}),
                ),
            )?;
            let rows = if result.get("ok") == Some(&json!(true)) {
                result
                    .get("result")
                    .and_then(Value::as_array)
                    .cloned()
                    .unwrap_or_default()
            } else {
                Vec::new()
            };
            let items = rows
                .iter()
                .filter(|row| row.is_object())
                .filter(|row| !row.get("id").is_none_or(Value::is_null))
                .map(|row| {
                    json!({
                        "service": "topvisor",
                        "resource": pyjson::str_value(row.get("id").unwrap_or(&Value::Null))
                            .unwrap_or_default(),
                        "label": row.get("name"),
                    })
                })
                .collect();
            (result, items)
        }
    };
    if let Some(raw_needle) = domain_text {
        let mut needle = raw_needle.to_lowercase();
        if needle.contains("://") {
            let host = split_host(&needle);
            if !host.is_empty() {
                needle = host;
            }
        }
        if let Some(stripped) = needle.strip_prefix("www.") {
            needle = stripped.to_owned();
        }
        for item in &mut resources {
            let raw = item
                .get("label")
                .or_else(|| item.get("resource"))
                .and_then(Value::as_str)
                .unwrap_or("");
            let host = if let Some(rest) = raw.strip_prefix("sc-domain:") {
                rest.to_lowercase()
            } else if raw.starts_with("sc-domain:") {
                String::new()
            } else {
                split_host(raw)
            };
            if host.is_empty() {
                item["domain_match"] = Value::Null;
            } else {
                let host = host.strip_prefix("www.").unwrap_or(&host).to_owned();
                item["domain_match"] = json!(host == needle);
            }
        }
    }
    Ok(json!({
        "ok": result.get("ok") == Some(&json!(true)),
        "provider": provider,
        "resources": resources,
        "count": resources.len(),
        "truncated": pystr::truthy(result.get("nextOffset")),
        "selected": false,
        "error": result.get("error").cloned().unwrap_or(Value::Null),
        "note": "discovery lists visible resources only; it does not bind or select one",
    }))
}

/// `_credential_details(provider)` → `(components, details)`.
pub fn credential_details(env: &ProviderEnv, provider: &str) -> Result<CredentialDetails, CfError> {
    if registry_entry(provider).is_none() {
        return Err(CfError::Value("unknown provider".to_owned()));
    }
    let mut components = Map::new();
    let mut details = Map::new();
    for (name, _, _) in credential_sources(provider) {
        let state = credential_source_status(env, provider, name);
        components.insert(
            name.to_string(),
            json!(state.get("state") == Some(&json!("configured_unverified"))),
        );
        details.insert(name.to_string(), state);
    }
    if matches!(provider, "gsc" | "ga4") {
        components.insert(
            "service_account".to_owned(),
            json!((env.service_account_available)()),
        );
        let status = (env.service_account_status)();
        let reference = if env.host.var("GSC_SERVICE_ACCOUNT_FILE").is_some() {
            "env:GSC_SERVICE_ACCOUNT_FILE"
        } else {
            "config:gsc/service-account.json"
        };
        let mut detail = json!({
            "state": match status.as_str() {
                "configured_unverified" => "configured_unverified",
                "missing" => "missing",
                _ => "invalid",
            },
            "source_reference": if status == "missing" { Value::Null } else { json!(reference) },
            "accepted_source_references": [
                "env:GSC_SERVICE_ACCOUNT_FILE",
                "config:gsc/service-account.json",
            ],
        });
        if !matches!(status.as_str(), "missing" | "configured_unverified") {
            detail["reason"] = json!(status);
        }
        details.insert("service_account".to_owned(), detail);
    }
    if provider == "gsc" {
        let grant = (env.grant_available)("gsc");
        components.insert("durable_oauth".to_owned(), json!(grant));
        details.insert(
            "durable_oauth".to_owned(),
            json!({
                "state": if grant { "configured_unverified" } else { "missing" },
                "source_reference": if grant { json!("local-grant:gsc") } else { Value::Null },
                "accepted_source_references": ["local-grant:gsc"],
            }),
        );
    }
    Ok((components, details))
}

/// `_readiness_state`.
pub fn readiness_state(
    provider: &str,
    components: &Map<String, Value>,
    details: &Map<String, Value>,
) -> &'static str {
    if components.is_empty() {
        return "not_required";
    }
    let ready = if matches!(provider, "gsc" | "ga4") {
        components.values().any(|v| v == &json!(true))
    } else {
        components.values().all(|v| v == &json!(true))
    };
    if ready {
        return "configured_unverified";
    }
    if details
        .values()
        .any(|d| d.get("state") == Some(&json!("invalid")))
    {
        return "invalid";
    }
    "missing"
}

pub(super) fn input_schema(provider: &str, operation: &str) -> Value {
    json!({
        "type": "object",
        "required": ["provider", "operation", "request"],
        "properties": {
            "provider": {"type": "string", "const": provider},
            "operation": {"type": "string", "const": operation},
            "request": {
                "type": "object",
                "additionalProperties": true,
                "description": "Provider-specific fields are validated by the selected operation adapter.",
            },
        },
    })
}

/// `_operation_contract(provider, operation)`.
pub fn operation_contract(provider: &str, operation: &str) -> Value {
    let Some((_, _, operations, _, _)) = registry_entry(provider) else {
        return json!({"name": operation, "state": "unsupported", "surface": Value::Null});
    };
    if !operations.contains(&operation) {
        return json!({"name": operation, "state": "unsupported", "surface": Value::Null});
    }
    let (state, surface) = if let Some(route) = dedicated(provider, operation) {
        (
            if provider == "indexnow" {
                "dedicated_write"
            } else {
                "dedicated"
            },
            Value::String(route.to_owned()),
        )
    } else if collectable(provider, operation) {
        ("supported", Value::String("provider-collect".to_owned()))
    } else {
        ("unsupported", Value::Null)
    };
    let mut contract = json!({"name": operation, "state": state, "surface": surface});
    if state == "supported" {
        contract["input_schema"] = input_schema(provider, operation);
    }
    contract
}

pub(crate) fn operation_catalog(provider: &str) -> Vec<Value> {
    registry_entry(provider)
        .map(|(_, _, ops, _, _)| {
            ops.iter()
                .map(|op| operation_contract(provider, op))
                .collect()
        })
        .unwrap_or_default()
}

/// `sources_doctor()` — configuration inspection, no provider requests.
pub fn sources_doctor(env: &ProviderEnv) -> Value {
    let mut providers = Map::new();
    for (name, (_, _, _, quota, privacy)) in REGISTRY {
        let (components, sources) = match credential_details(env, name) {
            Ok(pair) => pair,
            Err(_) => continue,
        };
        let available = if matches!(*name, "gsc" | "ga4") {
            components.values().any(|v| v == &json!(true))
        } else {
            components.values().all(|v| v == &json!(true))
        };
        let readiness = readiness_state(name, &components, &sources);
        let mut entry = json!({
            "state": if components.is_empty() {
                "not_required"
            } else if available {
                "credential_present"
            } else {
                "not_configured"
            },
            "credential_components": components,
            "credential_sources": sources,
            "readiness_state": readiness,
            "permission_state": "not_verified",
            "target_access": "not_requested",
            "verified": false,
            "note": "run explicit provider-verify; configured credentials are not verified access",
            "operations": operation_catalog(name),
            "quota_mode": quota,
            "privacy_class": privacy,
        });
        if matches!(*name, "gsc" | "ga4") {
            entry["service_account_status"] = json!((env.service_account_status)());
        }
        if matches!(readiness, "missing" | "invalid") {
            entry["unavailable_reason"] = json!(if readiness == "missing" {
                "one or more required credential components are missing"
            } else {
                "one or more configured credential sources are invalid"
            });
        }
        providers.insert(name.to_string(), entry);
    }
    json!({"format": "spiderhead.provider-doctor.v1", "providers": providers})
}
