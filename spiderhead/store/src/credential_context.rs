//! Redacted credential resume context and a local equality verifier.

use std::collections::BTreeSet;
use std::sync::OnceLock;

use regex::Regex;
use serde_json::{json, Map, Value};
use sha2::{Digest, Sha256};
use spiderhead_core::crawl_settings::redact_sensitive_headers;

use crate::error::StoreError;
use crate::scan_artifact::ScanError;

/// Regex source of an environment reference (full match).
pub const ENV: &str = r"env:([A-Za-z_][A-Za-z0-9_]*)\Z";
/// Placeholder recorded in place of every credential name, value and profile path.
pub const REDACTED: &str = "REDACTED";

fn scan_error(message: &str) -> StoreError {
    ScanError {
        message: message.to_string(),
    }
    .into()
}

fn env_name(reference: &str) -> Option<String> {
    static RE: OnceLock<Regex> = OnceLock::new();
    let re =
        RE.get_or_init(|| Regex::new(r"^env:([A-Za-z_][A-Za-z0-9_]*)$").expect("static pattern"));
    re.captures(reference).map(|c| c[1].to_string())
}

/// Copy effective settings while removing credential names, values, and profile paths.
pub fn redact_config(settings: &Map<String, Value>) -> Result<Map<String, Value>, StoreError> {
    let mut recorded = settings.clone();
    let browser_ok = recorded
        .get("rendering")
        .and_then(|r| r.get("browser"))
        .is_some_and(Value::is_object);
    if !recorded.get("http").is_some_and(Value::is_object) || !browser_ok {
        return Err(scan_error(
            "settings lack the effective HTTP or browser configuration",
        ));
    }
    if !recorded["http"]
        .get("credential_headers")
        .is_some_and(Value::is_array)
    {
        return Err(scan_error("credential headers must be a list"));
    }
    let redacted_headers =
        redact_sensitive_headers(recorded["http"].get("headers").unwrap_or(&Value::Null));
    if let Some(http) = recorded.get_mut("http").and_then(Value::as_object_mut) {
        http.insert("headers".into(), redacted_headers);
        if let Some(Value::Array(entries)) = http.get_mut("credential_headers") {
            for entry in entries.iter_mut() {
                let Some(headers) = entry.get("headers").and_then(Value::as_object) else {
                    return Err(scan_error("credential header entry is malformed"));
                };
                let masked: Map<String, Value> = headers
                    .keys()
                    .map(|n| (n.clone(), json!(REDACTED)))
                    .collect();
                entry["headers"] = Value::Object(masked);
            }
        }
    }
    if let Some(browser) = recorded
        .get_mut("rendering")
        .and_then(|r| r.get_mut("browser"))
        .and_then(Value::as_object_mut)
    {
        if browser
            .get("persistent_profile_dir")
            .is_some_and(crate::ledger::ingest::truthy)
        {
            browser.insert("persistent_profile_dir".into(), json!(REDACTED));
        }
    }
    Ok(recorded)
}

/// Validate recorded redaction without reading the environment, return a safe validation copy.
pub fn validate_recorded_credentials(
    snapshot: &Map<String, Value>,
) -> Result<Map<String, Value>, StoreError> {
    let mut checked = snapshot.clone();
    let browser_ok = checked
        .get("rendering")
        .and_then(|r| r.get("browser"))
        .is_some_and(Value::is_object);
    if !checked.get("http").is_some_and(Value::is_object) || !browser_ok {
        return Err(scan_error(
            "recorded settings lack HTTP or browser configuration",
        ));
    }
    let Some(entries) = checked["http"]
        .get("credential_headers")
        .and_then(Value::as_array)
        .cloned()
    else {
        return Err(scan_error("recorded credential headers must be a list"));
    };
    for entry in &entries {
        let headers = entry.get("headers").and_then(Value::as_object);
        let host_ok = entry
            .get("host")
            .and_then(Value::as_str)
            .is_some_and(|h| !h.is_empty());
        let headers_ok = headers.is_some_and(|h| {
            !h.is_empty()
                && h.iter()
                    .all(|(name, value)| !name.is_empty() && value.as_str() == Some(REDACTED))
        });
        if !entry.is_object() || !host_ok || !headers_ok {
            return Err(scan_error(
                "recorded credential headers are not exactly redacted",
            ));
        }
    }
    let profile_dir = checked["rendering"]["browser"]
        .get("persistent_profile_dir")
        .cloned();
    if !matches!(
        profile_dir.as_ref().and_then(Value::as_str),
        Some("") | Some(REDACTED)
    ) {
        return Err(scan_error(
            "recorded persistent profile path is not redacted",
        ));
    }
    let auth_entries: Vec<Value> = match checked["http"].get("auth") {
        None => Vec::new(),
        Some(Value::Array(a)) => a.clone(),
        Some(_) => return Err(scan_error("recorded HTTP auth must be a list")),
    };
    let acknowledged = match checked["http"].get("auth_acknowledged") {
        None => false,
        Some(Value::Bool(b)) => *b,
        Some(_) => {
            return Err(scan_error(
                "recorded HTTP auth acknowledgement must be boolean",
            ))
        }
    };
    if !auth_entries.is_empty() && !acknowledged {
        return Err(scan_error(
            "recorded HTTP auth lacks its required acknowledgement",
        ));
    }
    let mut seen: BTreeSet<String> = BTreeSet::new();
    let credential_hosts: BTreeSet<String> = entries
        .iter()
        .map(|e| match e.get("host") {
            None => String::new(),
            Some(Value::String(s)) => s.to_lowercase(),
            Some(other) => other.to_string().to_lowercase(),
        })
        .collect();
    let auth_keys = ["host", "scheme", "username_env", "password_env"];
    for entry in &auth_entries {
        let invalid = || scan_error("recorded HTTP auth environment references are invalid");
        let Some(map) = entry.as_object() else {
            return Err(invalid());
        };
        if map.len() != 4 || !auth_keys.iter().all(|k| map.contains_key(*k)) {
            return Err(invalid());
        }
        let host = map["host"]
            .as_str()
            .filter(|h| !h.is_empty())
            .ok_or_else(invalid)?;
        if host != host.to_lowercase()
            || host.contains('/')
            || seen.contains(host)
            || credential_hosts.contains(host)
            || !matches!(map["scheme"].as_str(), Some("basic" | "digest"))
            || ["username_env", "password_env"]
                .iter()
                .any(|n| map[*n].as_str().and_then(env_name).is_none())
        {
            return Err(invalid());
        }
        seen.insert(host.to_string());
    }
    // This copy is only for structural settings validation; it never represents an
    // authenticated or persistent-profile crawl at runtime.
    if let Some(http) = checked.get_mut("http").and_then(Value::as_object_mut) {
        http.insert("credential_headers".into(), json!([]));
        http.insert("credentials_acknowledged".into(), json!(false));
        http.insert("auth".into(), json!([]));
        http.insert("auth_acknowledged".into(), json!(false));
    }
    if let Some(browser) = checked
        .get_mut("rendering")
        .and_then(|r| r.get_mut("browser"))
        .and_then(Value::as_object_mut)
    {
        browser.insert("persistent_profile".into(), json!(false));
        browser.insert("persistent_profile_dir".into(), json!(""));
    }
    Ok(checked)
}

fn hmac_sha256(key: &[u8], data: &[u8]) -> [u8; 32] {
    let mut block = [0u8; 64];
    if key.len() > 64 {
        block[..32].copy_from_slice(&Sha256::digest(key));
    } else {
        block[..key.len()].copy_from_slice(key);
    }
    let mut inner = Sha256::new();
    inner.update(block.map(|b| b ^ 0x36));
    inner.update(data);
    let mut outer = Sha256::new();
    outer.update(block.map(|b| b ^ 0x5c));
    outer.update(inner.finalize());
    outer.finalize().into()
}

/// PBKDF2-HMAC-SHA256 with a 32-byte output (a single block).
pub fn pbkdf2_hmac_sha256(password: &[u8], salt: &[u8], iterations: u32) -> [u8; 32] {
    let mut seed = salt.to_vec();
    seed.extend_from_slice(&1u32.to_be_bytes());
    let mut u = hmac_sha256(password, &seed);
    let mut out = u;
    for _ in 1..iterations {
        u = hmac_sha256(password, &u);
        for (o, x) in out.iter_mut().zip(u) {
            *o ^= x;
        }
    }
    out
}

fn auth_env(name: &str) -> Option<String> {
    std::env::var(name).ok()
}

/// Return a salted local equality verifier for explicitly configured headers.
pub fn credential_verifier(
    live_settings: &Map<String, Value>,
    scan_uuid: &str,
) -> Result<Option<String>, StoreError> {
    let salt = uuid::Uuid::parse_str(scan_uuid)
        .map_err(|_| scan_error("scan UUID is invalid for credential verification"))?;
    let http = live_settings.get("http").filter(|h| h.is_object());
    let Some(entries) = http
        .and_then(|h| h.get("credential_headers"))
        .and_then(Value::as_array)
    else {
        return Err(scan_error("live credential headers must be a list"));
    };
    let auth_entries: Vec<Value> = match http.and_then(|h| h.get("auth")) {
        None => Vec::new(),
        Some(Value::Array(a)) => a.clone(),
        Some(_) => return Err(scan_error("live HTTP auth must be a list")),
    };
    if entries.is_empty() && auth_entries.is_empty() {
        return Ok(None);
    }
    let mut canonical: Vec<(String, Vec<(String, String)>)> = Vec::new();
    for entry in entries {
        let headers = entry.get("headers").and_then(Value::as_object);
        let host = entry.get("host").and_then(Value::as_str);
        let (Some(host), Some(headers)) = (host, headers) else {
            return Err(scan_error("live credential header entry is malformed"));
        };
        let mut values: Vec<(String, String)> = Vec::new();
        for (name, reference) in headers {
            let Some(reference) = reference.as_str() else {
                return Err(scan_error(
                    "credential header names and references must be strings",
                ));
            };
            let value = env_name(reference).and_then(|n| auth_env(&n));
            let Some(value) = value else {
                return Err(scan_error(
                    "credential verifier requires each configured environment value",
                ));
            };
            values.push((name.to_lowercase(), value));
        }
        values.sort();
        canonical.push((host.to_lowercase(), values));
    }
    canonical.sort();
    let value = if auth_entries.is_empty() {
        // Preserve the verifier format for existing credential-header artifacts.
        serde_json::to_value(&canonical)?
    } else {
        let mut auth_canonical: Vec<Vec<String>> = Vec::new();
        for entry in &auth_entries {
            let malformed = || scan_error("live HTTP auth entry is malformed");
            let map = entry.as_object().ok_or_else(malformed)?;
            let keys = ["host", "scheme", "username_env", "password_env"];
            if map.len() != 4
                || !keys.iter().all(|k| map.contains_key(*k))
                || map.values().any(|v| !v.is_string())
            {
                return Err(malformed());
            }
            let text = |k: &str| map[k].as_str().unwrap_or("").to_string();
            let mut values = Vec::new();
            for name in ["username_env", "password_env"] {
                let resolved = env_name(&text(name)).and_then(|n| auth_env(&n));
                let Some(resolved) = resolved else {
                    return Err(scan_error(
                        "HTTP auth verifier requires configured environment values",
                    ));
                };
                values.push(resolved);
            }
            auth_canonical.push(vec![
                text("host").to_lowercase(),
                text("scheme"),
                text("username_env"),
                text("password_env"),
                values[0].clone(),
                values[1].clone(),
            ]);
        }
        auth_canonical.sort();
        json!({"credential_headers": canonical, "http_auth": auth_canonical})
    };
    let payload = serde_json::to_string(&value)?;
    let digest = pbkdf2_hmac_sha256(payload.as_bytes(), salt.as_bytes(), 100_000);
    Ok(Some(digest.iter().map(|b| format!("{b:02x}")).collect()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pbkdf2_matches_the_rfc_vectors() {
        let hex = |b: [u8; 32]| b.iter().map(|x| format!("{x:02x}")).collect::<String>();
        assert_eq!(
            hex(pbkdf2_hmac_sha256(b"passwd", b"salt", 1)),
            "55ac046e56e3089fec1691c22544b605f94185216dde0465e68b9d57c20dacbc"
        );
        assert_eq!(
            hex(pbkdf2_hmac_sha256(b"Password", b"NaCl", 80000)),
            "4ddcd8f60b98be21830cee5ef22701f9641a4418d04c0414aeff08876b34ab56"
        );
    }

    fn settings() -> Map<String, Value> {
        json!({
            "http": {
                "headers": {"Authorization": "Bearer x", "Accept": "text/html"},
                "credential_headers": [{"host": "Example.test", "headers": {"X-Token": "env:SH_TEST_TOKEN"}}],
                "auth": [],
            },
            "rendering": {"browser": {"persistent_profile": true, "persistent_profile_dir": "/home/me/profile"}},
        })
        .as_object()
        .cloned()
        .unwrap()
    }

    #[test]
    fn redaction_removes_names_values_and_paths() {
        let recorded = redact_config(&settings()).unwrap();
        assert_eq!(recorded["http"]["headers"]["Authorization"], "REDACTED");
        assert_eq!(recorded["http"]["headers"]["Accept"], "text/html");
        assert_eq!(
            recorded["http"]["credential_headers"][0]["headers"]["X-Token"],
            "REDACTED"
        );
        assert_eq!(
            recorded["rendering"]["browser"]["persistent_profile_dir"],
            "REDACTED"
        );
        let checked = validate_recorded_credentials(&recorded).unwrap();
        assert_eq!(checked["http"]["credential_headers"], json!([]));
        assert_eq!(
            checked["rendering"]["browser"]["persistent_profile_dir"],
            ""
        );
        let mut bad = recorded.clone();
        bad["http"]["credential_headers"][0]["headers"]["X-Token"] = json!("secret");
        assert!(validate_recorded_credentials(&bad).is_err());
        assert!(redact_config(&Map::new()).is_err());
    }

    #[test]
    fn verifier_matches_the_reference_oracle() {
        std::env::set_var("SH_TEST_TOKEN", "abc");
        std::env::set_var("SH_USER", "u");
        std::env::set_var("SH_PASS", "p\u{e4}");
        let id = "0d6f2c1e-8c1a-4a0c-9d6e-2b7f6a1f9f10";
        let mut live = json!({
            "http": {
                "credential_headers": [{"host": "Example.test", "headers": {"X-Token": "env:SH_TEST_TOKEN", "A": "env:SH_USER"}}],
                "auth": [],
            }
        })
        .as_object()
        .cloned()
        .unwrap();
        assert_eq!(
            credential_verifier(&live, id).unwrap().as_deref(),
            Some("27d08f9aa26737a8ab379645e97b818d003195e26bd15039ad61617d9da493de")
        );
        live["http"]["auth"] = json!([{"host": "b.test", "scheme": "basic", "username_env": "env:SH_USER", "password_env": "env:SH_PASS"}]);
        assert_eq!(
            credential_verifier(&live, id).unwrap().as_deref(),
            Some("c130f53e3529019cc7d5e16bbfa4be22230ba1012dfc5f3dc23b2383983df653")
        );
        live["http"]["auth"][0]["password_env"] = json!("env:SH_MISSING_VALUE");
        assert!(credential_verifier(&live, id).is_err());
    }

    #[test]
    fn verifier_is_salted_and_needs_the_environment() {
        let id = "0d6f2c1e-8c1a-4a0c-9d6e-2b7f6a1f9f10";
        assert!(credential_verifier(&settings(), "nope").is_err());
        let mut absent = settings();
        absent["http"]["credential_headers"][0]["headers"]["X-Token"] =
            json!("env:SH_ABSENT_TOKEN_X");
        assert!(credential_verifier(&absent, id).is_err());
        let mut none = settings();
        none["http"]["credential_headers"] = json!([]);
        assert_eq!(credential_verifier(&none, id).unwrap(), None);
    }
}
