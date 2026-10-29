//! Named per-provider credential reads (`arsenkin_token`, `metrika_token`, ...) plus the
//! Google service-account document checks.
//!
//! Each helper binds the canonical `~/.config` path, the overriding environment variable
//! and the user-facing hint.

use super::{available, is_private_mode, mode_of, read, shellexpand_home, CredentialError};
use serde_json::{json, Value};
use spiderhead_core::host::Host;
use std::fs;
use std::path::{Path, PathBuf};

/// Arsenkin API token from its credential file or environment variable.
pub fn arsenkin_token(host: &dyn Host, root: &Path) -> Result<String, CredentialError> {
    read(
        host,
        root,
        "arsenkin/token",
        "ARSENKIN_TOKEN",
        "Create the token in your arsenkin.ru account.",
    )
}

/// Yandex Cloud API key used for Wordstat and search calls.
pub fn yandex_cloud_api_key(host: &dyn Host, root: &Path) -> Result<String, CredentialError> {
    read(
        host,
        root,
        "yandex-wordstat/api_key",
        "YANDEX_CLOUD_API_KEY",
        "Create it in Yandex AI Studio with the search-api.webSearch.user role.",
    )
}

/// Yandex Cloud folder ID that owns the API key.
pub fn yandex_cloud_folder_id(host: &dyn Host, root: &Path) -> Result<String, CredentialError> {
    read(
        host,
        root,
        "yandex-wordstat/folder_id",
        "YANDEX_CLOUD_FOLDER_ID",
        "Use the folder ID shown in the Yandex Cloud console.",
    )
}

/// Yandex Metrica OAuth token from its credential file or environment variable.
pub fn metrika_token(host: &dyn Host, root: &Path) -> Result<String, CredentialError> {
    read(
        host,
        root,
        "yandex-metrika/token",
        "YANDEX_METRIKA_TOKEN",
        "Create a Yandex Metrica OAuth token at oauth.yandex.ru.",
    )
}

/// Miratext API key from its credential file or environment variable.
pub fn miratext_api_key(host: &dyn Host, root: &Path) -> Result<String, CredentialError> {
    read(host, root, "miratext/api_key", "MIRATEXT_API_KEY", "")
}

/// DataForSEO login name from its credential file or environment variable.
pub fn dataforseo_login(host: &dyn Host, root: &Path) -> Result<String, CredentialError> {
    read(host, root, "dataforseo/login", "DATAFORSEO_LOGIN", "")
}

/// DataForSEO password from its credential file or environment variable.
pub fn dataforseo_password(host: &dyn Host, root: &Path) -> Result<String, CredentialError> {
    read(host, root, "dataforseo/password", "DATAFORSEO_PASSWORD", "")
}

/// DataForSEO readiness and per-component status, without exposing secret values.
pub fn dataforseo_ready(host: &dyn Host, root: &Path) -> (bool, Value) {
    let components = json!({
        "login": available(host, root, "dataforseo/login", "DATAFORSEO_LOGIN"),
        "password": available(host, root, "dataforseo/password", "DATAFORSEO_PASSWORD"),
    });
    let ready = components
        .as_object()
        .map(|m| m.values().all(|v| v == &Value::Bool(true)))
        .unwrap_or(false);
    (ready, components)
}

/// `gsc_access_token` — a short-lived OAuth2 bearer token; this client does not refresh one.
pub fn gsc_access_token(host: &dyn Host, root: &Path) -> Result<String, CredentialError> {
    read(
        host,
        root,
        "gsc/access_token",
        "GSC_ACCESS_TOKEN",
        "See docs/SETUP.md for how to obtain a Search Console OAuth token.",
    )
}

/// Token endpoint for the Google service-account JWT exchange.
pub const GOOGLE_TOKEN_URI: &str = "https://oauth2.googleapis.com/token";
/// Largest service-account file accepted, in bytes.
pub const GSC_SERVICE_ACCOUNT_MAX_BYTES: u64 = 65536;
const GSC_SERVICE_ACCOUNT_REQUIRED_FIELDS: [&str; 3] = ["client_email", "private_key", "token_uri"];

fn gsc_service_account_candidate_path(host: &dyn Host, root: &Path) -> PathBuf {
    if let Some(configured) = host
        .var("GSC_SERVICE_ACCOUNT_FILE")
        .filter(|v| !v.is_empty())
    {
        return PathBuf::from(shellexpand_home(host, &configured));
    }
    root.join("gsc/service-account.json")
}

/// Coarse filesystem state of the candidate path; contents are never inspected here.
fn gsc_service_account_file_state(path: &Path) -> &'static str {
    if path
        .symlink_metadata()
        .map(|m| m.file_type().is_symlink())
        .unwrap_or(false)
    {
        return "unsafe_file";
    }
    let info = match path.metadata() {
        Ok(m) => m,
        Err(_) => return "missing",
    };
    if !info.is_file() || !is_private_mode(mode_of(path).unwrap_or(0o777)) {
        return "unsafe_file";
    }
    "present"
}

/// Validate the configured service-account JSON and return `(status, document)`.
/// The document is populated only for `"configured_unverified"`; diagnostics never carry key material.
pub fn gsc_service_account_document(host: &dyn Host, root: &Path) -> (String, Option<Value>) {
    let path = gsc_service_account_candidate_path(host, root);
    if gsc_service_account_file_state(&path) != "present" {
        return (gsc_service_account_file_state(&path).to_owned(), None);
    }
    let size = match path.metadata() {
        Ok(m) => m.len(),
        Err(_) => return ("missing".to_owned(), None),
    };
    if size > GSC_SERVICE_ACCOUNT_MAX_BYTES {
        return ("too_large".to_owned(), None);
    }
    let document: Value = match fs::read_to_string(&path) {
        Ok(text) => match serde_json::from_str(&text) {
            Ok(v) => v,
            Err(_) => return ("malformed_json".to_owned(), None),
        },
        Err(_) => return ("unreadable".to_owned(), None),
    };
    let Some(map) = document.as_object() else {
        return ("unsupported_shape".to_owned(), None);
    };
    if map.get("type").and_then(Value::as_str) != Some("service_account")
        || map.get("token_uri").and_then(Value::as_str) != Some(GOOGLE_TOKEN_URI)
        || GSC_SERVICE_ACCOUNT_REQUIRED_FIELDS.iter().any(|field| {
            map.get(*field)
                .and_then(Value::as_str)
                .unwrap_or("")
                .is_empty()
        })
    {
        return ("unsupported_shape".to_owned(), None);
    }
    ("configured_unverified".to_owned(), Some(document))
}

/// Coarse credential state for diagnostics.
pub fn gsc_service_account_status(host: &dyn Host, root: &Path) -> String {
    gsc_service_account_document(host, root).0
}

/// True when a service-account file is present and well-formed; the key is not checked against Google.
pub fn gsc_service_account_available(host: &dyn Host, root: &Path) -> bool {
    gsc_service_account_status(host, root) == "configured_unverified"
}

/// GA4 access token from its credential file or environment variable; expected to be read-only.
pub fn ga4_access_token(host: &dyn Host, root: &Path) -> Result<String, CredentialError> {
    read(
        host,
        root,
        "ga4/access_token",
        "GA4_ACCESS_TOKEN",
        "Authorize only analytics.readonly for the selected GA4 property.",
    )
}

/// Yandex Webmaster OAuth token from its credential file or environment variable.
pub fn yandex_webmaster_token(host: &dyn Host, root: &Path) -> Result<String, CredentialError> {
    read(
        host,
        root,
        "yandex-webmaster/access_token",
        "YANDEX_WEBMASTER_TOKEN",
        "Authorize a read-only Yandex Webmaster application before collecting evidence.",
    )
}

/// Bing Webmaster API key from its credential file or environment variable.
pub fn bing_webmaster_key(host: &dyn Host, root: &Path) -> Result<String, CredentialError> {
    read(
        host,
        root,
        "bing-webmaster/api_key",
        "BING_WEBMASTER_API_KEY",
        "Create a Bing Webmaster API key for the verified site.",
    )
}

/// Chrome UX Report API key from its credential file or environment variable.
pub fn crux_api_key(host: &dyn Host, root: &Path) -> Result<String, CredentialError> {
    read(
        host,
        root,
        "crux/api_key",
        "CRUX_API_KEY",
        "Create an API key in Google Cloud Console and enable the Chrome UX Report API.",
    )
}

/// `indexnow_key` — a self-generated key, not a provider-issued secret.
pub fn indexnow_key(host: &dyn Host, root: &Path) -> Result<String, CredentialError> {
    read(
        host,
        root,
        "indexnow/key",
        "INDEXNOW_KEY",
        "Generate one yourself and publish it at https://<host>/<key>.txt first.",
    )
}

/// `topvisor` token/user-id pair used by that client.
pub fn topvisor_token(host: &dyn Host, root: &Path) -> Result<String, CredentialError> {
    read(host, root, "topvisor/access_token", "TOPVISOR_TOKEN", "")
}

/// `topvisor` user id.
pub fn topvisor_user_id(host: &dyn Host, root: &Path) -> Result<String, CredentialError> {
    read(host, root, "topvisor/user_id", "TOPVISOR_USER_ID", "")
}
