//! Browser transport: a local launch or an operator-supplied Playwright endpoint.
//!
//! Config validation and endpoint selection. The endpoint secret is read from an injected environment lookup and is never echoed in an error.
//! Launching or connecting a browser needs the renderer's driver, so that part lives with the render code.

use fancy_regex::Regex;
use serde_json::{Map, Value};
use std::sync::OnceLock;
use thiserror::Error;

/// Default values of the four browser-transport settings, in a fixed order.
pub const DEFAULTS: &[(&str, &str)] = &[
    ("transport", "local"),
    ("remote_protocol", "playwright"),
    ("remote_endpoint_env", ""),
    ("remote_playwright_version", ""),
];

/// A safe, actionable failure. The message never includes an endpoint or a token.
#[derive(Debug, Error, Clone, PartialEq, Eq)]
#[error("{message}")]
pub struct BrowserTransportError {
    /// Stable machine code, for example `remote_version_incompatible`.
    pub code: &'static str,
    /// Human-readable message.
    pub message: String,
}

impl BrowserTransportError {
    fn new(code: &'static str, message: &str) -> Self {
        Self {
            code,
            message: message.to_owned(),
        }
    }
}

/// The browser route a page uses, recorded as renderer provenance.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Route {
    /// A browser launched on this machine.
    Local,
    /// A browser connected to an operator-supplied Playwright server.
    Remote {
        /// Version of the installed Playwright client.
        client_version: String,
        /// Version the operator declared for the remote server.
        expected_remote_version: String,
    },
}

impl Route {
    /// Provenance recorded with the render, reported as `facts`.
    pub fn to_value(&self) -> Value {
        match self {
            Route::Local => serde_json::json!({"mode": "local"}),
            Route::Remote {
                client_version,
                expected_remote_version,
            } => serde_json::json!({
                "mode": "remote",
                "protocol": "playwright",
                "client_version": client_version,
                "expected_remote_version": expected_remote_version,
            }),
        }
    }
}

/// The result of a successful [`prepare`]: the endpoint to connect to, if any, and the route.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Prepared {
    /// The resolved WebSocket endpoint; `None` for a local launch.
    pub endpoint: Option<String>,
    /// Route facts for provenance.
    pub route: Route,
}

/// Compiled once; `None` only if the constant pattern were malformed, which then refuses every value.
static ENV_NAME_RE: OnceLock<Option<Regex>> = OnceLock::new();
static VERSION_RE: OnceLock<Option<Regex>> = OnceLock::new();

fn matches_pattern(cell: &'static OnceLock<Option<Regex>>, pattern: &str, text: &str) -> bool {
    let compiled = cell.get_or_init(|| Regex::new(pattern).ok());
    compiled
        .as_ref()
        .is_some_and(|re| re.is_match(text).unwrap_or(false))
}

fn is_env_name(text: &str) -> bool {
    matches_pattern(&ENV_NAME_RE, r"\A[A-Za-z_][A-Za-z0-9_]*\z", text)
}

fn is_full_version(text: &str) -> bool {
    matches_pattern(&VERSION_RE, VERSION_PATTERN, text)
}

const VERSION_PATTERN: &str = r"\A(\d+)\.(\d+)\.\d+(?:[a-zA-Z0-9.+-]*)?\z";

/// Error text for an invalid selection, as a plain string.
fn validate_fields(config: &Map<String, Value>, embedded: bool) -> Result<(), String> {
    let allowed = |key: &str| DEFAULTS.iter().any(|(name, _)| *name == key);
    if !embedded && config.keys().any(|key| !allowed(key)) {
        return Err("browser transport config has unsupported fields".into());
    }
    let kind = config
        .get("transport")
        .cloned()
        .unwrap_or_else(|| Value::String("local".into()));
    let protocol = config
        .get("remote_protocol")
        .cloned()
        .unwrap_or_else(|| Value::String("playwright".into()));
    let name = config
        .get("remote_endpoint_env")
        .cloned()
        .unwrap_or_else(|| Value::String(String::new()));
    let expected = config
        .get("remote_playwright_version")
        .cloned()
        .unwrap_or_else(|| Value::String(String::new()));
    let kind = match kind.as_str() {
        Some(text @ ("local" | "remote")) => text.to_owned(),
        _ => return Err("browser transport must be local or remote".into()),
    };
    if protocol.as_str() != Some("playwright") {
        return Err("remote browser protocol must be playwright; CDP is unsupported".into());
    }
    let (Some(name), Some(expected)) = (name.as_str(), expected.as_str()) else {
        return Err("remote browser endpoint env and version must be strings".into());
    };
    if kind == "local" {
        if !name.is_empty() || !expected.is_empty() {
            return Err("remote browser settings require transport=remote".into());
        }
        return Ok(());
    }
    if !is_env_name(name) {
        return Err("remote browser requires a valid remote_endpoint_env variable name".into());
    }
    if !is_full_version(expected) {
        return Err("remote browser requires a full remote_playwright_version".into());
    }
    Ok(())
}

/// Validates the recorded selection without resolving the endpoint secret. `embedded` allows extra keys from a containing config.
pub fn validate_config(
    config: &Map<String, Value>,
    embedded: bool,
) -> Result<(), BrowserTransportError> {
    validate_fields(config, embedded)
        .map_err(|message| BrowserTransportError::new("invalid_browser_transport", &message))
}

/// Major and minor components of a full `X.Y.Z[...]` version string.
fn major_minor(version: &str) -> Option<(String, String)> {
    let regex = VERSION_RE
        .get_or_init(|| Regex::new(VERSION_PATTERN).ok())
        .as_ref()?;
    let caps = regex.captures(version).ok().flatten()?;
    Some((
        caps.get(1)?.as_str().to_owned(),
        caps.get(2)?.as_str().to_owned(),
    ))
}

/// Resolves an opt-in endpoint before any origin fetch or browser launch.
///
/// `env` looks up an environment variable by name; `local_version` returns the installed Playwright client version, or `None` when it is not installed.
/// Both are injected so the logic stays free of process state.
pub fn prepare(
    config: Option<&Value>,
    embedded: bool,
    env: &dyn Fn(&str) -> Option<String>,
    local_version: &dyn Fn() -> Option<String>,
) -> Result<Prepared, BrowserTransportError> {
    let mut selected: Map<String, Value> = DEFAULTS
        .iter()
        .map(|(key, value)| ((*key).to_owned(), Value::String((*value).to_owned())))
        .collect();
    match config {
        None | Some(Value::Null) => {}
        Some(Value::Object(map)) => selected.extend(map.clone()),
        Some(_) => {
            return Err(BrowserTransportError::new(
                "invalid_browser_transport",
                "Browser transport config must be an object",
            ))
        }
    }
    validate_config(&selected, embedded)?;
    if selected.get("transport").and_then(Value::as_str) == Some("local") {
        return Ok(Prepared {
            endpoint: None,
            route: Route::Local,
        });
    }
    let client_version = local_version().ok_or_else(|| {
        BrowserTransportError::new(
            "playwright_unavailable",
            "Install the optional Playwright package before remote rendering",
        )
    })?;
    let remote_declared = selected
        .get("remote_playwright_version")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_owned();
    let same_line = match (major_minor(&client_version), major_minor(&remote_declared)) {
        (Some(local), Some(remote)) => local == remote,
        _ => false,
    };
    if !same_line {
        return Err(BrowserTransportError::new(
            "remote_version_incompatible",
            "Remote Playwright major/minor version must match the installed client version",
        ));
    }
    let name = selected
        .get("remote_endpoint_env")
        .and_then(Value::as_str)
        .unwrap_or_default();
    let endpoint = env(name).unwrap_or_default();
    validate_endpoint(&endpoint)?;
    Ok(Prepared {
        endpoint: Some(endpoint),
        route: Route::Remote {
            client_version,
            expected_remote_version: remote_declared,
        },
    })
}

/// Check that the endpoint is a WebSocket URL with no userinfo or fragment, and that a
/// plain `ws` endpoint is loopback only. Never includes the endpoint in an error.
fn validate_endpoint(endpoint: &str) -> Result<(), BrowserTransportError> {
    let invalid_url = || {
        BrowserTransportError::new(
            "invalid_remote_endpoint",
            "Remote Playwright endpoint is not a valid WebSocket URL",
        )
    };
    let shape = || {
        BrowserTransportError::new(
            "invalid_remote_endpoint",
            "Remote Playwright endpoint must be a WebSocket URL without userinfo or fragment",
        )
    };
    let parsed = url::Url::parse(endpoint).map_err(|_| invalid_url())?;
    if authority_has_at_sign(endpoint) {
        return Err(shape());
    }
    if !matches!(parsed.scheme(), "ws" | "wss") || parsed.host_str().is_none_or(str::is_empty) {
        return Err(shape());
    }
    if parsed.fragment().is_some_and(|f| !f.is_empty()) || parsed.port() == Some(0) {
        return Err(shape());
    }
    if parsed.scheme() == "ws" && !is_loopback_host(parsed.host_str().unwrap_or_default()) {
        return Err(BrowserTransportError::new(
            "insecure_remote_endpoint",
            "Use wss for a non-loopback Playwright endpoint",
        ));
    }
    Ok(())
}

/// True when the authority carries an `@`. The check is textual, so an empty user name still counts as userinfo.
fn authority_has_at_sign(endpoint: &str) -> bool {
    let after_scheme = endpoint.split_once("//").map_or("", |(_, rest)| rest);
    let authority = after_scheme
        .split(['/', '?', '#'])
        .next()
        .unwrap_or_default();
    authority.contains('@')
}

/// Whether a host is a loopback address or the `localhost` name.
fn is_loopback_host(host: &str) -> bool {
    let bare = host.trim_start_matches('[').trim_end_matches(']');
    match bare.parse::<std::net::IpAddr>() {
        Ok(address) => address.is_loopback(),
        Err(_) => bare == "localhost",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const ENDPOINT_VAR: &str = "SYNTHETIC_PLAYWRIGHT_WS";

    fn remote_config(endpoint_var: &str, version: &str) -> Value {
        serde_json::json!({
            "transport": "remote",
            "remote_protocol": "playwright",
            "remote_endpoint_env": endpoint_var,
            "remote_playwright_version": version,
        })
    }

    fn env_with(endpoint: &'static str) -> impl Fn(&str) -> Option<String> {
        move |name: &str| (name == ENDPOINT_VAR).then(|| endpoint.to_owned())
    }

    fn client() -> Option<String> {
        Some("1.55.1".to_owned())
    }

    #[test]
    fn local_default_does_not_resolve_an_endpoint() {
        let prepared = prepare(None, false, &|_| None, &client).expect("local default");
        assert_eq!(prepared.endpoint, None);
        assert_eq!(
            prepared.route.to_value(),
            serde_json::json!({"mode": "local"})
        );
    }

    #[test]
    fn unsupported_protocol_or_version_refuses_before_connection() {
        let cases = [
            (
                serde_json::json!({"remote_protocol": "cdp"}),
                "invalid_browser_transport",
            ),
            (
                serde_json::json!({"remote_playwright_version": "1.54.9"}),
                "remote_version_incompatible",
            ),
        ];
        for (change, code) in cases {
            let mut config = remote_config(ENDPOINT_VAR, "1.55.0");
            if let (Some(base), Some(extra)) = (config.as_object_mut(), change.as_object()) {
                base.extend(extra.clone());
            }
            let error = prepare(
                Some(&config),
                false,
                &env_with("wss://browser.example.test/p"),
                &client,
            )
            .expect_err("refused");
            assert_eq!(error.code, code);
        }
    }

    #[test]
    fn endpoint_validation_never_echoes_the_value() {
        let cases = [
            (
                "https://browser.example.test/playwright",
                "invalid_remote_endpoint",
            ),
            (
                "ws://browser.example.test/playwright",
                "insecure_remote_endpoint",
            ),
            (
                "wss://user:secret@browser.example.test/playwright",
                "invalid_remote_endpoint",
            ),
        ];
        for (endpoint, code) in cases {
            let config = remote_config(ENDPOINT_VAR, "1.55.0");
            let env = move |name: &str| (name == ENDPOINT_VAR).then(|| endpoint.to_owned());
            let error = prepare(Some(&config), false, &env, &client).expect_err("refused");
            assert_eq!(error.code, code);
            assert!(!error.message.contains(endpoint));
            assert!(!error.message.contains("secret"));
        }
    }

    #[test]
    fn valid_remote_endpoint_is_returned_with_route_facts() {
        let config = remote_config(ENDPOINT_VAR, "1.55.0");
        let prepared = prepare(
            Some(&config),
            false,
            &env_with("wss://browser.example.test/playwright?token=synthetic"),
            &client,
        )
        .expect("accepted");
        assert_eq!(
            prepared.endpoint.as_deref(),
            Some("wss://browser.example.test/playwright?token=synthetic")
        );
        assert_eq!(
            prepared.route.to_value(),
            serde_json::json!({
                "mode": "remote",
                "protocol": "playwright",
                "client_version": "1.55.1",
                "expected_remote_version": "1.55.0",
            })
        );
    }

    #[test]
    fn loopback_ws_is_accepted_and_missing_client_is_actionable() {
        let config = remote_config(ENDPOINT_VAR, "1.55.0");
        assert!(prepare(
            Some(&config),
            false,
            &env_with("ws://127.0.0.1:9222/p"),
            &client
        )
        .is_ok());
        let missing = prepare(Some(&config), false, &env_with("wss://h.test/p"), &|| None)
            .expect_err("no client");
        assert_eq!(missing.code, "playwright_unavailable");
    }

    #[test]
    fn local_config_refuses_remote_fields_and_unknown_keys() {
        let stray = serde_json::json!({"remote_endpoint_env": "X"});
        let error =
            validate_config(stray.as_object().expect("object"), false).expect_err("refused");
        assert_eq!(
            error.message,
            "remote browser settings require transport=remote"
        );
        let unknown = serde_json::json!({"headless": true});
        let error =
            validate_config(unknown.as_object().expect("object"), false).expect_err("refused");
        assert_eq!(
            error.message,
            "browser transport config has unsupported fields"
        );
        assert!(validate_config(unknown.as_object().expect("object"), true).is_ok());
    }
}
