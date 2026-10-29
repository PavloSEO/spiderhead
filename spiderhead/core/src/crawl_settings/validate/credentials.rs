//! Credential headers, HTTP auth and the generic header lane.

use super::*;

/// Credential headers: host-bound, env-referenced, and only with acknowledgement.
pub(super) fn validate_credential_headers(
    config: &Value,
    host: &dyn Host,
) -> Result<(), ConfigError> {
    let entries = at(config, "http.credential_headers")
        .as_array()
        .cloned()
        .unwrap_or_default();
    if entries.is_empty() {
        return Ok(());
    }
    if at(config, "http.credentials_acknowledged").as_bool() != Some(true) {
        return err(
            "http.credential_headers is set but http.credentials_acknowledged is not true; a crawler clicks every link, including ones that log out, publish, or delete, so authenticated crawling needs an explicit acknowledgement",
        );
    }
    for entry in &entries {
        let host_name = entry
            .get("host")
            .and_then(Value::as_str)
            .filter(|h| !h.is_empty());
        let Some(host_name) = host_name else {
            return err(
                "each http.credential_headers entry needs a host binding; an unbound credential is sent on every request and the leak is silent",
            );
        };
        let headers = entry
            .get("headers")
            .and_then(Value::as_object)
            .filter(|h| !h.is_empty());
        let Some(headers) = headers else {
            return err(format!(
                "http.credential_headers entry for {host_name:?} needs headers"
            ));
        };
        for (name, value) in headers {
            let text = value.as_str().unwrap_or_default();
            let Some(var) = env_ref(text) else {
                return err(format!(
                    "http.credential_headers header {name:?} for {host_name:?} must reference an environment variable as 'env:VAR_NAME', never an inline value"
                ));
            };
            if host.var(var).is_none() {
                return err(format!(
                    "http.credential_headers for {host_name:?} references environment variable {var:?}, which is not set"
                ));
            }
        }
    }
    Ok(())
}

/// Basic and digest auth entries: exact keys, lowercase host, env-only credentials.
pub(super) fn validate_http_auth(config: &Value, host: &dyn Host) -> Result<(), ConfigError> {
    let acknowledged = at(config, "http.auth_acknowledged");
    let Some(ack) = acknowledged.as_bool() else {
        return err("http.auth_acknowledged must be boolean");
    };
    let Some(entries) = at(config, "http.auth").as_array() else {
        return err("http.auth must be a list");
    };
    if !entries.is_empty() && !ack {
        return err("http.auth is set but http.auth_acknowledged is not true; authenticated crawls follow links");
    }
    let mut seen_hosts = std::collections::BTreeSet::new();
    let header_hosts: std::collections::BTreeSet<String> = at(config, "http.credential_headers")
        .as_array()
        .into_iter()
        .flatten()
        .map(|e| {
            e.get("host")
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_lowercase()
        })
        .collect();
    for entry in entries {
        let object = entry.as_object();
        let exact = object.is_some_and(|o: &Map<String, Value>| {
            o.len() == 4
                && ["host", "scheme", "username_env", "password_env"]
                    .iter()
                    .all(|k| o.contains_key(*k))
        });
        if !exact {
            return err(
                "each http.auth entry requires exactly host, scheme, username_env and password_env",
            );
        }
        let entry_host = object
            .and_then(|o| o.get("host"))
            .and_then(Value::as_str)
            .unwrap_or("");
        if entry_host.is_empty()
            || entry_host != entry_host.to_lowercase()
            || entry_host.contains('/')
        {
            return err("each http.auth host must be a lowercase hostname");
        }
        if !seen_hosts.insert(entry_host.to_owned()) {
            return err(format!("http.auth has duplicate host {entry_host:?}"));
        }
        if header_hosts.contains(entry_host) {
            return err(format!(
                "http.auth and http.credential_headers cannot both configure {entry_host:?}"
            ));
        }
        let scheme = object
            .and_then(|o| o.get("scheme"))
            .and_then(Value::as_str)
            .unwrap_or("");
        if scheme != "basic" && scheme != "digest" {
            return err(format!(
                "http.auth scheme for {entry_host:?} must be basic or digest"
            ));
        }
        for field in ["username_env", "password_env"] {
            let reference = object
                .and_then(|o| o.get(field))
                .and_then(Value::as_str)
                .unwrap_or("");
            let Some(var) = env_ref(reference) else {
                return err(format!(
                    "http.auth {field} for {entry_host:?} must reference env:VAR"
                ));
            };
            if host.var(var).is_none() {
                return err(format!(
                    "http.auth for {entry_host:?} references environment variable {var:?}, which is not set"
                ));
            }
        }
    }
    Ok(())
}

/// The generic header lane: string-to-string, and never a credential header.
pub(super) fn validate_http_headers(config: &Value) -> Result<(), ConfigError> {
    let Some(headers) = at(config, "http.headers").as_object() else {
        return err("http.headers must be an object mapping header names to string values");
    };
    if headers.values().any(|v| !v.is_string()) {
        return err("http.headers must be an object mapping header names to string values");
    }
    if headers
        .keys()
        .any(|name| SENSITIVE_HEADER_NAMES.contains(&name.to_lowercase().as_str()))
    {
        return err(
            "http.headers cannot carry credentials; use host-bound, environment-sourced http.credential_headers instead",
        );
    }
    Ok(())
}
