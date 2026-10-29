//! Fail-closed target policy for an authenticated, queued crawl job.
//!
//! The service owns this policy. A request may select a saved project and bounded settings,
//! but may not widen the project's private-host allowlist or budgets. The policy is passed
//! explicitly; there is no ambient context that binds it to clients.

use std::collections::{BTreeSet, HashMap};
use std::net::IpAddr;
use std::sync::Mutex;

use serde_json::Value;
use thiserror::Error;

use crate::host::Host;
use crate::net::{allows_private_range, validate_url};

/// Safe, URL-free rejection suitable for an API response or event.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
#[error("{message}")]
pub struct RemoteTargetError {
    /// Stable machine code, e.g. `unsafe_target`.
    pub code: &'static str,
    /// Human-readable message that never echoes the URL.
    pub message: String,
}

impl RemoteTargetError {
    fn new(code: &'static str, message: &str) -> Self {
        Self {
            code,
            message: message.to_owned(),
        }
    }
}

/// Invalid policy construction (a configuration error, not a request error).
#[derive(Debug, Clone, PartialEq, Eq, Error)]
#[error("{0}")]
pub struct PolicyError(pub String);

/// Project-bound limits and exact private staging hosts for one queued job.
#[derive(Debug)]
pub struct RemoteEgressPolicy {
    /// Project this policy belongs to.
    pub project_id: String,
    /// Exact lowercase private hostnames permitted to resolve privately.
    pub allowed_private_hosts: BTreeSet<String>,
    /// Physical HTTP attempts allowed per origin.
    pub max_requests_per_origin: u64,
    /// Physical HTTP attempts allowed in total.
    pub max_total_requests: u64,
    /// Maximum crawl concurrency.
    pub max_concurrency: u64,
    /// Minimum politeness delay in seconds.
    pub min_delay_seconds: f64,
    /// Whether a remote browser backend may be used.
    pub allow_remote_browser: bool,
    requests: Mutex<HashMap<(String, String, u16), u64>>,
}

/// Whether `host` is a dotted, octal, hex or bare-number form that `inet_aton` would accept.
fn is_inet_aton_alias(host: &str) -> bool {
    let parts: Vec<&str> = host.split('.').collect();
    if parts.is_empty() || parts.len() > 4 {
        return false;
    }
    parts.iter().all(|part| {
        if part.is_empty() {
            return false;
        }
        let parsed = if let Some(hex) = part.strip_prefix("0x").or_else(|| part.strip_prefix("0X"))
        {
            u64::from_str_radix(hex, 16).ok()
        } else if part.len() > 1 && part.starts_with('0') {
            u64::from_str_radix(&part[1..], 8).ok()
        } else {
            part.parse::<u64>().ok()
        };
        parsed.is_some()
    })
}

impl RemoteEgressPolicy {
    /// Build a policy, validating every field.
    pub fn new(
        project_id: &str,
        allowed_private_hosts: impl IntoIterator<Item = String>,
        max_requests_per_origin: u64,
        max_total_requests: u64,
        max_concurrency: u64,
        min_delay_seconds: f64,
        allow_remote_browser: bool,
    ) -> Result<Self, PolicyError> {
        if project_id.is_empty() {
            return Err(PolicyError("project_id must be a nonempty string".into()));
        }
        if [max_requests_per_origin, max_total_requests, max_concurrency].contains(&0) {
            return Err(PolicyError(
                "remote request and concurrency limits must be positive integers".into(),
            ));
        }
        if !min_delay_seconds.is_finite() || min_delay_seconds < 0.0 {
            return Err(PolicyError(
                "remote delay floor must be finite and nonnegative".into(),
            ));
        }
        let mut normalized = BTreeSet::new();
        for value in allowed_private_hosts {
            let host = value.trim_end_matches('.').to_lowercase();
            if host.parse::<IpAddr>().is_ok() {
                return Err(PolicyError(
                    "private host allowlist requires DNS hostnames, not IP literals".into(),
                ));
            }
            if is_inet_aton_alias(&host) {
                return Err(PolicyError(
                    "private host allowlist requires DNS hostnames, not IP aliases".into(),
                ));
            }
            if host.is_empty()
                || host != host.trim()
                || host == "localhost"
                || host.ends_with(".localhost")
                || ["://", ":", "/", "@", "*"]
                    .iter()
                    .any(|bad| host.contains(bad))
            {
                return Err(PolicyError(
                    "private host allowlist must contain exact hostnames".into(),
                ));
            }
            normalized.insert(host);
        }
        Ok(Self {
            project_id: project_id.to_owned(),
            allowed_private_hosts: normalized,
            max_requests_per_origin,
            max_total_requests,
            max_concurrency,
            min_delay_seconds,
            allow_remote_browser,
            requests: Mutex::new(HashMap::new()),
        })
    }

    /// Whether `host` is on the exact private-host allowlist.
    pub fn allows_private_host(&self, host: &str) -> bool {
        self.allowed_private_hosts
            .contains(&host.trim_end_matches('.').to_lowercase())
    }

    /// Allow an exact staging hostname only into RFC 1918 or IPv6 ULA space.
    pub fn allows_private_address(&self, host: &str, address: &str) -> bool {
        self.allows_private_host(host) && allows_private_range(address)
    }

    /// Reject an unsafe submitted target or an unbounded crawl before queueing.
    pub fn authorize_submission(
        &self,
        project_id: &str,
        target_url: &str,
        effective_config: &Value,
        host: &dyn Host,
    ) -> Result<(), RemoteTargetError> {
        if project_id != self.project_id {
            return Err(RemoteTargetError::new(
                "foreign_project",
                "target policy belongs to another project",
            ));
        }
        let unsafe_target = || {
            RemoteTargetError::new(
                "unsafe_target",
                "target is not a permitted public or approved staging URL",
            )
        };
        if target_url.contains('#') || has_url_credentials(target_url) {
            return Err(unsafe_target());
        }
        validate_url(target_url, host, Some(self)).map_err(|_| unsafe_target())?;
        if !effective_config.is_object() {
            return Err(RemoteTargetError::new(
                "invalid_config",
                "effective crawl settings are required",
            ));
        }
        crate::crawl_settings::validate(effective_config, host).map_err(|_| {
            RemoteTargetError::new("invalid_config", "effective crawl settings are invalid")
        })?;
        self.check_budgets(effective_config)
    }

    fn check_budgets(&self, config: &Value) -> Result<(), RemoteTargetError> {
        let invalid =
            || RemoteTargetError::new("invalid_config", "effective crawl settings are invalid");
        let number = |path: &str| {
            config
                .pointer(&format!("/{}", path.replace('.', "/")))
                .and_then(Value::as_f64)
                .ok_or_else(invalid)
        };
        let max_requests = number("limits.max_requests")?;
        let max_urls = number("limits.max_urls")?;
        let concurrency = number("speed.concurrency")?;
        let delay = number("speed.min_delay_seconds")?;
        let total = self.max_total_requests as f64;
        if !(1.0..=total).contains(&max_requests)
            || !(1.0..=total).contains(&max_urls)
            || !(1.0..=self.max_concurrency as f64).contains(&concurrency)
            || !delay.is_finite()
            || delay < self.min_delay_seconds
        {
            return Err(RemoteTargetError::new(
                "budget_exceeded",
                "remote crawl request, URL, concurrency or rate budget exceeded",
            ));
        }
        if config.pointer("/http/proxy").is_some_and(py_truthy) {
            return Err(RemoteTargetError::new(
                "unsupported_proxy",
                "remote proxy routing is not enabled for queued jobs",
            ));
        }
        let browser = config.pointer("/rendering/browser").unwrap_or(&Value::Null);
        // Any backend value other than the string "local", null included, selects a remote backend;
        // a missing key means "local".
        let foreign_backend = match browser.get("backend") {
            None => false,
            Some(Value::String(name)) => name != "local",
            Some(_) => true,
        };
        if browser.get("persistent_profile").is_some_and(py_truthy) || foreign_backend {
            return Err(RemoteTargetError::new(
                "unsupported_browser",
                "remote browser backend or profile is unsupported",
            ));
        }
        let transport = browser
            .get("transport")
            .and_then(Value::as_str)
            .unwrap_or("local");
        if transport == "remote" && !self.allow_remote_browser {
            return Err(RemoteTargetError::new(
                "unsupported_browser",
                "remote browser transport is not enabled for this project",
            ));
        }
        Ok(())
    }

    /// Spend one physical HTTP attempt, including redirects and browser resources.
    pub fn reserve_request(
        &self,
        url: &str,
        original_host: Option<&str>,
    ) -> Result<(), RemoteTargetError> {
        let unsafe_origin =
            || RemoteTargetError::new("unsafe_target", "request has an invalid origin");
        let parsed = url::Url::parse(url).map_err(|_| unsafe_origin())?;
        let scheme = parsed.scheme().to_owned();
        let host_name = original_host
            .map(str::to_owned)
            .or_else(|| parsed.host_str().map(str::to_owned))
            .unwrap_or_default()
            .trim_end_matches('.')
            .to_lowercase();
        let default_port = if scheme == "https" { 443 } else { 80 };
        let port = parsed.port().unwrap_or(default_port);
        if !matches!(scheme.as_str(), "http" | "https") || host_name.is_empty() {
            return Err(unsafe_origin());
        }
        let key = (scheme, host_name, port);
        let mut counts = self.requests.lock().map_err(|_| unsafe_origin())?;
        let total: u64 = counts.values().sum();
        if total >= self.max_total_requests {
            return Err(RemoteTargetError::new(
                "request_budget",
                "remote total request budget exhausted",
            ));
        }
        let origin = counts.get(&key).copied().unwrap_or(0);
        if origin >= self.max_requests_per_origin {
            return Err(RemoteTargetError::new(
                "origin_budget",
                "remote per-origin request budget exhausted",
            ));
        }
        counts.insert(key, origin + 1);
        Ok(())
    }
}

/// Truthiness of a JSON value: null, `false`, `0`, `""` and empty containers are falsy;
/// everything else is truthy. Proxy and profile flags are checked this way, not by type.
fn py_truthy(value: &Value) -> bool {
    match value {
        Value::Null => false,
        Value::Bool(flag) => *flag,
        Value::Number(number) => number.as_f64().is_some_and(|x| x != 0.0),
        Value::String(text) => !text.is_empty(),
        Value::Array(items) => !items.is_empty(),
        Value::Object(map) => !map.is_empty(),
    }
}

/// Whether a URL carries userinfo (`user@` or `user:pass@`).
fn has_url_credentials(url: &str) -> bool {
    url::Url::parse(url)
        .map(|parsed| !parsed.username().is_empty() || parsed.password().is_some())
        .unwrap_or(false)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::host::FakeHost;

    fn policy(hosts: &[&str]) -> RemoteEgressPolicy {
        RemoteEgressPolicy::new(
            "p1",
            hosts.iter().map(|h| (*h).to_owned()),
            3,
            5,
            4,
            0.5,
            false,
        )
        .unwrap()
    }

    // Behaviour of the policy constructor: IP literals, aliases and bad names refused.
    #[test]
    fn allowlist_must_hold_exact_dns_hostnames() {
        for bad in [
            "10.0.0.1",
            "2130706433",
            "127.1",
            "localhost",
            "app.localhost",
            "a/b",
            "*.example.test",
        ] {
            assert!(
                RemoteEgressPolicy::new("p", [bad.to_owned()], 1, 1, 1, 0.0, false).is_err(),
                "{bad}"
            );
        }
        assert!(RemoteEgressPolicy::new(
            "p",
            ["Staging.Example.Test.".to_owned()],
            1,
            1,
            1,
            0.0,
            false
        )
        .unwrap()
        .allows_private_host("staging.example.test"));
    }

    // Behaviour of the policy constructor: non-positive limits and bad delay floors are refused.
    #[test]
    fn limits_must_be_positive_and_delay_finite() {
        assert!(RemoteEgressPolicy::new("", [], 1, 1, 1, 0.0, false).is_err());
        assert!(RemoteEgressPolicy::new("p", [], 0, 1, 1, 0.0, false).is_err());
        assert!(RemoteEgressPolicy::new("p", [], 1, 1, 1, f64::NAN, false).is_err());
        assert!(RemoteEgressPolicy::new("p", [], 1, 1, 1, -1.0, false).is_err());
    }

    // Behaviour of allows_private_address: only RFC 1918 or ULA space for an allowlisted host.
    #[test]
    fn private_address_needs_allowlisted_host_and_staging_range() {
        let p = policy(&["stage.example.test"]);
        assert!(p.allows_private_address("stage.example.test", "10.2.3.4"));
        assert!(!p.allows_private_address("stage.example.test", "127.0.0.1"));
        assert!(!p.allows_private_address("other.example.test", "10.2.3.4"));
    }

    // Behaviour of reserve_request: total and per-origin budgets are enforced.
    #[test]
    fn request_budgets_are_enforced_per_origin_and_total() {
        let p = policy(&[]);
        for _ in 0..3 {
            p.reserve_request("https://a.example.test/x", None).unwrap();
        }
        let origin = p
            .reserve_request("https://a.example.test/y", None)
            .unwrap_err();
        assert_eq!(origin.code, "origin_budget");
        p.reserve_request("https://b.example.test/", None).unwrap();
        p.reserve_request("https://c.example.test/", None).unwrap();
        let total = p
            .reserve_request("https://d.example.test/", None)
            .unwrap_err();
        assert_eq!(total.code, "request_budget");
    }

    // Behaviour of authorize_submission: a foreign project and URL credentials are refused.
    #[test]
    fn submission_refuses_foreign_project_and_credentials() {
        let p = policy(&[]);
        let host = FakeHost::default();
        let cfg = Value::Object(Default::default());
        let foreign = p
            .authorize_submission("other", "https://a.example.test/", &cfg, &host)
            .unwrap_err();
        assert_eq!(foreign.code, "foreign_project");
        let creds = p
            .authorize_submission("p1", "https://u:p@a.example.test/", &cfg, &host)
            .unwrap_err();
        assert_eq!(creds.code, "unsafe_target");
    }
}
