//! Lifecycle routes: `provider_verify`, `provider_collect`, `provider_join`, `provider_replay`.
//! Pure registry and credential-slot logic lives in `providers.rs`.

use serde_json::{json, Value};

use crate::cloudflare::CfError;
use crate::credentials;
use crate::external_join;
use crate::providers::{
    self, credential_details, evidence, operation_contract, permission_failure_state,
    readiness_state, save_local_artifact, DispatchError, ProviderEnv, EVIDENCE_FORMAT,
};
use crate::pyjson;
use crate::pystr;

mod collect;
pub use collect::*;
mod join_replay;
pub use join_replay::*;

fn invalid_verify(provider: &Value, error: &str) -> Value {
    json!({
        "ok": false,
        "provider": provider,
        "state": "invalid",
        "readiness_state": "invalid",
        "permission_state": "invalid",
        "target_access": "not_requested",
        "verified": false,
        "error": error,
    })
}

fn unsupported_verify(provider: &Value) -> Value {
    json!({
        "ok": false,
        "provider": provider.as_str().map(Value::from).unwrap_or(Value::Null),
        "state": "unsupported",
        "readiness_state": "unsupported",
        "permission_state": "unsupported",
        "target_access": "not_requested",
        "verified": false,
        "error": "unsupported provider",
    })
}

fn dispatch(
    env: &ProviderEnv,
    route: &str,
    request: Option<&Value>,
) -> Result<Value, DispatchError> {
    (env.dispatch)(route, request)
}

fn dispatch_value(
    env: &ProviderEnv,
    route: &str,
    request: Option<&Value>,
) -> Result<Value, CfError> {
    dispatch(env, route, request).map_err(CfError::from)
}

/// Verify route: `provider_verify(provider, request)` over the injected transport.
pub fn provider_verify(
    env: &ProviderEnv,
    provider: &Value,
    request: Option<&Value>,
) -> Result<Value, CfError> {
    if let Some(req) = request {
        if !req.is_null() && !req.is_object() {
            return Ok(invalid_verify(provider, "request must be an object"));
        }
    }
    let request = request
        .filter(|r| !r.is_null())
        .cloned()
        .unwrap_or(json!({}));
    if !provider.is_string()
        || providers::registry_entry(provider.as_str().unwrap_or_default()).is_none()
    {
        return Ok(unsupported_verify(provider));
    }
    let name = provider.as_str().unwrap_or_default();
    let (components, sources) = credential_details(env, name)?;
    let readiness = readiness_state(name, &components, &sources);
    if components.is_empty() {
        return Ok(json!({
            "ok": false,
            "provider": name,
            "state": "not_required",
            "readiness_state": "not_required",
            "permission_state": "not_required",
            "target_access": "not_requested",
            "verified": false,
            "credential_components": components,
            "credential_sources": sources,
            "note": "this public source has no authenticated-access contract to verify",
        }));
    }
    let ready = if matches!(name, "gsc" | "ga4") {
        components.values().any(|v| v == &json!(true))
    } else {
        components.values().all(|v| v == &json!(true))
    };
    if !ready {
        let missing = readiness == "missing";
        let mut result = json!({
            "ok": false,
            "provider": name,
            "state": "not_configured",
            "readiness_state": readiness,
            "permission_state": if missing { "missing" } else { "invalid" },
            "target_access": "not_requested",
            "verified": false,
            "credential_components": components,
            "credential_sources": sources,
        });
        result["error"] = json!(if missing {
            "required credential components are missing"
        } else {
            "configured credential source is invalid"
        });
        if matches!(name, "gsc" | "ga4") {
            result["service_account_status"] = json!((env.service_account_status)());
        }
        return Ok(result);
    }
    let started = (env.monotonic)();
    let result: Value = match name {
        "gsc" => dispatch_value(env, "gsc.discover_properties", None)?,
        "yandex_webmaster" => dispatch_value(
            env,
            "yandex_webmaster.collect",
            Some(
                &json!({"operation": "hosts", "kwargs": {"user_id": request.get("user_id").cloned().unwrap_or(Value::Null)}}),
            ),
        )?,
        "bing_webmaster" => dispatch_value(
            env,
            "bing_webmaster.collect",
            Some(
                &json!({"operation": "sites", "kwargs": {"site_url": request.get("site_url").cloned().unwrap_or(json!(""))}}),
            ),
        )?,
        "arsenkin" => match dispatch(env, "arsenkin.limits", None) {
            Ok(balance) => json!({"ok": true, "balance": balance}),
            Err(error) => {
                let code = error
                    .code
                    .clone()
                    .unwrap_or_else(|| "invalid_response".to_owned());
                json!({
                    "ok": false,
                    "status": code.parse::<i64>().ok().map(Value::from).unwrap_or(Value::Null),
                    "error": code,
                })
            }
        },
        "cloudflare" => match dispatch(env, "cloudflare.verify_token", None) {
            Err(_) => {
                json!({"ok": false, "status": Value::Null, "error": "Cloudflare token verification failed"})
            }
            Ok(token) => {
                let token_status = token.get("token_status").cloned().unwrap_or(Value::Null);
                let active = token_status == json!("active");
                json!({
                    "ok": token.get("ok") == Some(&json!(true)) && active,
                    "status": token.get("status").cloned().unwrap_or(Value::Null),
                    "error": token.get("error").cloned().filter(|e| !e.is_null()).unwrap_or_else(|| {
                        if !active { json!("Cloudflare API token is not active") } else { Value::Null }
                    }),
                    "token_status": token_status,
                    "state": if matches!(token_status.as_str(), Some("disabled" | "expired")) { json!("invalid") } else { Value::Null },
                    "verified_capabilities": if active { json!(["api_token.active"]) } else { json!([]) },
                    "unverified_capabilities": ["zone and account resource access"],
                })
            }
        },
        "yandex_cloud" => {
            let client = dispatch_value(env, "yandex_cloud.client", None)?;
            let folder = client.get("folder_id").cloned().unwrap_or(Value::Null);
            match dispatch(
                env,
                "yandex_cloud.request",
                Some(&json!({
                    "url": "https://searchapi.api.cloud.yandex.net/v2/wordstat/getRegionsTree",
                    "body": {"folderId": folder},
                })),
            ) {
                Err(_) => {
                    json!({"ok": false, "status": Value::Null, "error": "provider connection failed"})
                }
                Ok(reply) => {
                    let status = reply
                        .get("status")
                        .and_then(Value::as_i64)
                        .unwrap_or_default();
                    let body = reply.get("body").cloned().unwrap_or(Value::Null);
                    let valid = body
                        .get("regions")
                        .and_then(Value::as_array)
                        .map(|r| !r.is_empty())
                        .unwrap_or(false);
                    json!({
                        "ok": status == 200 && valid,
                        "status": status,
                        "error": if status == 200 && valid { Value::Null } else { json!("unbilled getRegionsTree capability check failed") },
                        "quota_state": "not_reported",
                        "verified_capabilities": if status == 200 && valid { json!(["wordstat.getRegionsTree"]) } else { json!([]) },
                        "unverified_capabilities": ["wordstat.topRequests", "wordstat.dynamics", "web_search"],
                    })
                }
            }
        }
        "dataforseo_backlinks" => {
            match dispatch(env, "dataforseo.balance", Some(&json!({"env": "prod"}))) {
                Err(error) if error.class == "DataForSEOError" => json!({
                    "ok": false,
                    "status": error.status.map(Value::from).unwrap_or(Value::Null),
                    "error": "DataForSEO account metadata check failed",
                }),
                Err(_) => {
                    json!({"ok": false, "status": Value::Null, "error": "provider connection failed"})
                }
                Ok(account) => {
                    let valid = account.is_object()
                        && account.get("env") == Some(&json!("prod"))
                        && account.get("limits").is_some_and(Value::is_object)
                        && account.get("prices").is_some_and(Value::is_object);
                    json!({
                        "ok": valid,
                        "error": if valid { Value::Null } else { json!("account metadata response was incomplete") },
                        "quota_state": if valid { "reported" } else { "unknown" },
                        "account_data_available": valid,
                        "environment": account.get("env").cloned().unwrap_or(Value::Null),
                        "verified_capabilities": if valid { json!(["appendix.user_data"]) } else { json!([]) },
                        "unverified_capabilities": ["backlinks subscription and collection access"],
                    })
                }
            }
        }
        "ga4" | "metrika" | "pagespeed" | "crux" => {
            let operation = match name {
                "ga4" => "landing_pages",
                "metrika" => "counters",
                "pagespeed" => "mobile_samples",
                _ => "current",
            };
            let required: &[&str] = match name {
                "ga4" => &["property_id", "start_date", "end_date"],
                "pagespeed" => &["urls"],
                _ => &[],
            };
            let missing = required.iter().any(|key| !pystr::truthy(request.get(key)))
                || (name == "crux"
                    && pystr::truthy(request.get("url")) == pystr::truthy(request.get("origin")));
            if missing {
                return Ok(json!({
                    "ok": false,
                    "provider": name,
                    "state": "verification_input_required",
                    "readiness_state": readiness,
                    "permission_state": "unknown",
                    "authenticated_account": false,
                    "target_access": "not_requested",
                    "verified": false,
                    "required_fields": if required.is_empty() {
                        if name == "crux" { json!(["url or origin"]) } else { json!([]) }
                    } else {
                        json!(required)
                    },
                    "credential_components": components,
                    "credential_sources": sources,
                    "quota_state": "unknown",
                }));
            }
            provider_collect(env, name, operation, &request, None, None)?
        }
        _ => {
            return Ok(json!({
                "ok": false,
                "provider": name,
                "state": "credential_present",
                "readiness_state": readiness,
                "permission_state": "unsupported",
                "target_access": "not_requested",
                "verified": false,
                "credential_components": components,
                "credential_sources": sources,
                "operation_status": "unsupported",
                "note": "this provider has no supported bounded verification route",
            }))
        }
    };
    let authenticated = pystr::truthy(result.get("ok"));
    let target = request
        .get("site_url")
        .or_else(|| request.get("host_id"))
        .cloned()
        .unwrap_or(Value::Null);
    let mut target_access = "not_requested";
    if pystr::truthy(Some(&target)) {
        let candidates: Vec<Value> = match name {
            "gsc" => result
                .get("properties")
                .and_then(Value::as_array)
                .map(|rows| {
                    rows.iter()
                        .filter(|r| r.is_object())
                        .filter_map(|r| r.get("site_url").cloned())
                        .collect()
                })
                .unwrap_or_default(),
            "yandex_webmaster" => result
                .get("data")
                .and_then(Value::as_object)
                .and_then(|d| d.get("hosts"))
                .and_then(Value::as_array)
                .map(|rows| {
                    rows.iter()
                        .filter(|r| r.is_object())
                        .filter_map(|r| r.get("host_id").cloned())
                        .collect()
                })
                .unwrap_or_default(),
            "bing_webmaster" => result
                .get("data")
                .and_then(Value::as_array)
                .map(|rows| {
                    rows.iter()
                        .filter(|r| r.is_object())
                        .filter_map(|r| r.get("Url").cloned())
                        .collect()
                })
                .unwrap_or_default(),
            _ => Vec::new(),
        };
        target_access = if candidates.contains(&target) {
            "verified"
        } else if candidates.is_empty() {
            "unknown"
        } else {
            "not_granted"
        };
    }
    let permission_state = if authenticated {
        match target_access {
            "verified" => "verified",
            "not_granted" => "not_granted",
            _ => "authenticated_account",
        }
    } else {
        permission_failure_state(&result)
    };
    let mut out = json!({
        "ok": authenticated,
        "provider": name,
        "state": if authenticated { json!("authenticated") } else { result.get("state").cloned().filter(|s| !s.is_null()).unwrap_or(json!("verification_failed")) },
        "readiness_state": readiness,
        "permission_state": permission_state,
        "authenticated_account": authenticated,
        "target_access": target_access,
        "verified": target_access == "verified",
        "credential_components": components,
        "credential_sources": sources,
        "operations": providers::operation_catalog(name),
        "selected_reference": Value::Null,
        "granted_scopes": result.get("scopes").cloned().filter(|s| pystr::truthy(Some(s))).unwrap_or(json!("unknown")),
        "quota_mode": providers::registry_entry(name).map(|e| e.3).unwrap_or("unknown"),
        "quota_state": result.get("quota_state").cloned().unwrap_or(json!("unknown")),
        "response_time_ms": (((env.monotonic)() - started) * 1000.0 * 10.0).round() / 10.0,
        "contract_compatible": authenticated && matches!(target_access, "verified" | "not_requested"),
        "error": result.get("error").cloned().unwrap_or(Value::Null),
    });
    if name == "dataforseo_backlinks" {
        if let Some(env_value) = result.get("environment") {
            out["environment"] = env_value.clone();
        }
    }
    if let Some(token_status) = result.get("token_status") {
        out["token_status"] = token_status.clone();
    }
    if result.get("verified_capabilities").is_some() {
        out["verified_capabilities"] = result["verified_capabilities"].clone();
        out["unverified_capabilities"] = result["unverified_capabilities"].clone();
    }
    Ok(out)
}
