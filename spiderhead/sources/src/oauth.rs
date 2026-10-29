//! Restricted local storage and explicit refresh for read-only OAuth grants.
//!
//! Grants live under the credential root as `gsc/oauth.json` (mode 0600, no symlinks); the
//! pending-flow ledger is `gsc/oauth-flow.json`, handled by [`crate::oauth_flow`]. No secret value
//! ever appears in an error message or a result envelope.

use serde_json::{json, Map, Value};
use spiderhead_core::host::Host;
use std::path::{Path, PathBuf};

use crate::credentials::is_private_mode;
use crate::http::{urlencode, Request, Transport, TransportError};
use crate::{oauth_flow, pyjson, pystr, pytime};

mod grant;
pub use grant::*;
mod refresh;
pub use refresh::*;
mod paths;
pub use paths::*;

/// `TOKEN_HOST` — the OAuth2 token endpoint for refresh and code exchange.
pub const TOKEN_HOST: &str = "https://oauth2.googleapis.com/token";
/// `AUTH_HOST` — the consent-screen endpoint the authorization URL points at.
pub const AUTH_HOST: &str = "https://accounts.google.com/o/oauth2/v2/auth";
/// `REVOKE_HOST` — the revocation endpoint `manage_grant("revoke")` posts to.
pub const REVOKE_HOST: &str = "https://oauth2.googleapis.com/revoke";
/// The only scope a stored grant may carry.
pub const READONLY_SCOPE: &str = "https://www.googleapis.com/auth/webmasters.readonly";
/// Largest accepted grant file, in bytes.
const GRANT_MAX_BYTES: u64 = 65536;
/// Largest accepted flow-adjacent file (grant_file import).
const GRANT_FILE_MAX_BYTES: u64 = 65536;

/// Errors that `manage_grant` and related calls return.
#[derive(Debug, thiserror::Error)]
pub enum OAuthError {
    /// Invalid input or a refused state.
    #[error("{0}")]
    Invalid(String),
    /// No usable credential.
    #[error("{0}")]
    Missing(String),
    /// A naive and an aware timestamp were compared during flow handling.
    #[error("{0}")]
    TypeErr(String),
    /// Filesystem failure.
    #[error("{0}")]
    Io(String),
    /// A transport failure, passed through unchanged (timeout or connection error).
    #[error("{1}")]
    Connect(&'static str, String),
    /// The home directory of a `~user` path could not be resolved.
    #[error("{0}")]
    Runtime(String),
}

impl OAuthError {
    /// Stable error class name reported to callers.
    pub fn python_class(&self) -> &'static str {
        match self {
            OAuthError::Invalid(_) => "ValueError",
            OAuthError::Missing(_) => "MissingCredential",
            OAuthError::TypeErr(_) => "TypeError",
            OAuthError::Io(_) => "OSError",
            OAuthError::Connect(class, _) => class,
            OAuthError::Runtime(_) => "RuntimeError",
        }
    }
}

impl From<oauth_flow::FlowError> for OAuthError {
    fn from(e: oauth_flow::FlowError) -> Self {
        match e {
            oauth_flow::FlowError::Invalid(m) => OAuthError::Invalid(m),
            oauth_flow::FlowError::TypeErr(m) => OAuthError::TypeErr(m),
            oauth_flow::FlowError::Io(e) => OAuthError::Io(e.to_string()),
        }
    }
}

fn invalid<T>(message: &str) -> Result<T, OAuthError> {
    Err(OAuthError::Invalid(message.to_owned()))
}

/// Grant file for a provider; only `gsc` is supported.
pub fn path(root: &Path, provider: &str) -> Result<PathBuf, OAuthError> {
    if provider != "gsc" {
        return invalid("unsupported OAuth provider");
    }
    Ok(root.join(provider).join("oauth.json"))
}

/// Pending-flow ledger, stored next to the grant file.
pub fn flow_path(root: &Path, provider: &str) -> Result<PathBuf, OAuthError> {
    Ok(path(root, provider)?.with_file_name("oauth-flow.json"))
}

fn is_symlink(path: &Path) -> bool {
    path.symlink_metadata()
        .map(|m| m.file_type().is_symlink())
        .unwrap_or(false)
}

fn mode_of(path: &Path) -> Option<u32> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        path.symlink_metadata().ok().map(|m| m.mode())
    }
    #[cfg(not(unix))]
    {
        path.symlink_metadata().ok().map(|_| 0o600)
    }
}

/// `manage_grant` — the grant lifecycle verb dispatcher. Side effects are explicit:
/// `connect` imports a grant file, `callback` exchanges a code after state verification,
/// `disconnect`/`revoke` require `confirm`, and `refresh` performs a live token call.
pub async fn manage_grant<T: Transport>(
    host: &dyn Host,
    root: &Path,
    provider: &str,
    args: &Manage<'_>,
    transport: &T,
) -> Result<Value, OAuthError> {
    manage_grant_with(
        host,
        root,
        provider,
        args,
        transport,
        &mut oauth_flow::SystemTokens,
        None,
    )
    .await
}

/// A test hook called at a [`HookPoint`] with the flow-ledger path.
pub type CommitHook<'a> = Option<&'a mut dyn FnMut(HookPoint, &Path)>;

/// [`manage_grant`] with the flow token source and commit hook injected, for tests.
pub async fn manage_grant_with<T: Transport>(
    host: &dyn Host,
    root: &Path,
    provider: &str,
    args: &Manage<'_>,
    transport: &T,
    tokens: &mut dyn oauth_flow::Tokens,
    mut hook: CommitHook<'_>,
) -> Result<Value, OAuthError> {
    let grant_file_path = path(root, provider)?;
    match args.action {
        "status" => {
            let mut result = json!({
                "ok": true,
                "configured": grant_available(root, provider),
                "access_verified": false,
            });
            if let Some(flow) = oauth_flow::load_flow(&flow_path(root, provider)?)? {
                let public = flow_public(&flow, now_or(args.now))?;
                result
                    .as_object_mut()
                    .map(|m| m.insert("flow".to_owned(), public));
            }
            Ok(result)
        }
        "start" => {
            let client_id = env(host, "GSC_OAUTH_CLIENT_ID");
            let redirect_uri = env(host, "GSC_OAUTH_REDIRECT_URI");
            let parsed = spiderhead_parse::py_url::urlsplit(&redirect_uri)
                .map_err(|e| OAuthError::Invalid(e.to_string()))?;
            let (username, password, hostname) = netloc_parts(&parsed.netloc);
            if client_id.is_empty()
                || !matches!(parsed.scheme.as_str(), "https" | "http")
                || parsed.netloc.is_empty()
                || username.is_some()
                || password.is_some()
            {
                return invalid("GSC OAuth client ID and redirect URI must be configured");
            }
            if parsed.scheme == "http"
                && !matches!(hostname.as_str(), "localhost" | "127.0.0.1" | "::1")
            {
                return invalid("OAuth redirect URI must use HTTPS outside loopback");
            }
            let now = now_or(args.now);
            let flow_path = flow_path(root, provider)?;
            let (flow, raw_state) = {
                let _guard = oauth_flow::FlowLock::acquire(&flow_path)?;
                if grant_available(root, provider) {
                    return invalid(
                        "a durable OAuth grant already exists; disconnect it before reconnecting",
                    );
                }
                oauth_flow::create_flow_unlocked_with(&flow_path, now, tokens)?
            };
            let query = urlencode(&[
                ("client_id".to_owned(), client_id),
                ("redirect_uri".to_owned(), redirect_uri),
                ("response_type".to_owned(), "code".to_owned()),
                ("scope".to_owned(), READONLY_SCOPE.to_owned()),
                ("access_type".to_owned(), "offline".to_owned()),
                ("prompt".to_owned(), "consent".to_owned()),
                ("state".to_owned(), raw_state),
            ]);
            Ok(json!({
                "ok": true,
                "flow": {
                    "flow_id": flow.get("flow_id").cloned().unwrap_or(Value::Null),
                    "status": "waiting",
                    "expires_at": flow.get("expires_at").cloned().unwrap_or(Value::Null),
                },
                "authorization_url": format!("{AUTH_HOST}?{query}"),
            }))
        }
        "cancel" => {
            let Some(flow) =
                oauth_flow::cancel_flow(&flow_path(root, provider)?, now_or(args.now))?
            else {
                return Ok(json!({"ok": false, "error": "no_active_flow"}));
            };
            let status = flow.get("status").and_then(Value::as_str).unwrap_or_default();
            if matches!(status, "connected" | "revoked" | "invalid") {
                return Ok(json!({"ok": false, "error": "flow_not_waiting"}));
            }
            Ok(json!({
                "ok": true,
                "flow": {
                    "flow_id": flow.get("flow_id").cloned().unwrap_or(Value::Null),
                    "status": status,
                },
            }))
        }
        "connect" => {
            let Some(file) = args.grant_file else {
                return invalid("connect requires a private grant_file");
            };
            let source = PathBuf::from(expand_home(host, file)?);
            let info = source.symlink_metadata().ok();
            let usable = info
                .as_ref()
                .map(|m| {
                    m.is_file()
                        && !m.file_type().is_symlink()
                        && mode_of(&source).map(is_private_mode).unwrap_or(false)
                        && m.len() <= GRANT_FILE_MAX_BYTES
                })
                .unwrap_or(false);
            if !usable {
                return invalid("grant_file must be a private bounded regular JSON file");
            }
            let grant: Value = std::fs::read_to_string(&source)
                .ok()
                .and_then(|t| serde_json::from_str(&t).ok())
                .ok_or_else(|| {
                    OAuthError::Invalid("grant_file is unreadable or invalid JSON".to_owned())
                })?;
            save_grant(root, provider, &grant)?;
            Ok(json!({"ok": true, "configured": true, "access_verified": false}))
        }
        "callback" => {
            let valid_input = args.state.is_some()
                && matches!(args.code, Some(c) if !c.is_empty()
                    && c.chars().count() <= 4096
                    && !c.chars().any(|ch| (ch as u32) < 32));
            if !valid_input {
                return invalid("OAuth callback requires state and authorization code");
            }
            let now = now_or(args.now);
            let flow_path = flow_path(root, provider)?;
            let flow = oauth_flow::verify_callback_state(&flow_path, args.state, now)?;
            let client_id = env(host, "GSC_OAUTH_CLIENT_ID");
            let client_secret = host
                .var("GSC_OAUTH_CLIENT_SECRET")
                .unwrap_or_default();
            let redirect_uri = env(host, "GSC_OAUTH_REDIRECT_URI");
            if client_id.is_empty() || client_secret.is_empty() || redirect_uri.is_empty() {
                return invalid(
                    "GSC OAuth client credentials and redirect URI must be configured",
                );
            }
            let fields = [
                ("grant_type".to_owned(), "authorization_code".to_owned()),
                ("code".to_owned(), args.code.unwrap_or_default().to_owned()),
                ("client_id".to_owned(), client_id.clone()),
                ("client_secret".to_owned(), client_secret.clone()),
                ("redirect_uri".to_owned(), redirect_uri),
            ];
            let token_response = match post_form(transport, TOKEN_HOST, &fields).await {
                Ok(body) => body,
                Err(_) => {
                    return invalid(
                        "Google OAuth code exchange failed; flow remains unconnected",
                    )
                }
            };
            // Tests interpose here through the hook to replay a cancel during the exchange.
            if let Some(hook) = hook.as_mut() {
                hook(HookPoint::AfterExchange, &flow_path);
            }
            if !token_response.is_object() {
                return invalid("Google OAuth returned an invalid token response");
            }
            let scopes: Vec<String> = token_response
                .get("scope")
                .and_then(Value::as_str)
                .map(|s| s.split_whitespace().map(str::to_owned).collect())
                .unwrap_or_default();
            let refresh_token = token_response
                .get("refresh_token")
                .and_then(Value::as_str)
                .filter(|s| !s.is_empty());
            let access_token = token_response
                .get("access_token")
                .and_then(Value::as_str)
                .filter(|s| !s.is_empty());
            let expires_in = match token_response.get("expires_in") {
                Some(value) if pystr::is_py_int(value) => {
                    pystr::as_strict_i64(Some(value)).filter(|v| (1..=86400).contains(v))
                }
                _ => None,
            };
            let (Some(refresh_token), Some(_access_token), Some(expires_in)) =
                (refresh_token, access_token, expires_in)
            else {
                return invalid(
                    "Google OAuth response did not grant the required read-only scope",
                );
            };
            if scopes != [READONLY_SCOPE.to_owned()] {
                return invalid(
                    "Google OAuth response did not grant the required read-only scope",
                );
            }
            let connected_at = now_or(args.now);
            let completed = {
                let _guard = oauth_flow::FlowLock::acquire(&flow_path)?;
                // The commit lock is held: a concurrent `start` blocks on it.
                if let Some(hook) = hook.as_mut() {
                    hook(HookPoint::InCommit, &flow_path);
                }
                oauth_flow::verify_callback_state(&flow_path, args.state, connected_at)?;
                if grant_available(root, provider) {
                    return invalid(
                        "a durable OAuth grant appeared during consent; disconnect it first",
                    );
                }
                save_grant(
                    root,
                    provider,
                    &json!({
                        "refresh_token": refresh_token,
                        "client_id": client_id,
                        "client_secret": client_secret,
                        "scopes": [READONLY_SCOPE],
                    }),
                )?;
                oauth_flow::complete_flow(&flow_path, &flow, connected_at, expires_in)?
            };
            Ok(json!({
                "ok": true,
                "configured": true,
                "access_verified": false,
                "flow": {
                    "flow_id": completed.get("flow_id").cloned().unwrap_or(Value::Null),
                    "status": completed.get("status").cloned().unwrap_or(Value::Null),
                    "connected_at": completed.get("connected_at").cloned().unwrap_or(Value::Null),
                    "token_expires_at": completed.get("token_expires_at").cloned().unwrap_or(Value::Null),
                },
            }))
        }
        "refresh" => match refresh_access_token(root, provider, transport).await {
            Ok(refreshed) => Ok(json!({
                "ok": true,
                "refreshed": true,
                "scopes": refreshed.get("scopes").cloned().unwrap_or(Value::Null),
                "expires_in": refreshed.get("expires_in").cloned().unwrap_or(Value::Null),
                "property_access_verified": false,
            })),
            Err(OAuthError::Missing(message)) if message.to_lowercase().contains("invalid_grant") => {
                Ok(json!({
                    "ok": false,
                    "refreshed": false,
                    "state": "invalid_grant",
                    "error": "stored OAuth grant was rejected; reconnect through consent",
                }))
            }
            Err(OAuthError::Missing(_)) => Ok(json!({
                "ok": false,
                "refreshed": false,
                "state": "verification_failed",
                "error": "stored OAuth grant could not be refreshed",
            })),
            Err(other) => Err(other),
        },
        "disconnect" | "revoke" => {
            if !args.confirm {
                return invalid("disconnect/revoke requires confirm=true");
            }
            let stored = grant(root, provider)?;
            if args.action == "revoke" {
                let fields = [(
                    "token".to_owned(),
                    stored
                        .get("refresh_token")
                        .and_then(Value::as_str)
                        .unwrap_or_default()
                        .to_owned(),
                )];
                let request = Request::new("POST", REVOKE_HOST)
                    .header("Content-Type", "application/x-www-form-urlencoded")
                    .body(urlencode(&fields).into_bytes())
                    .timeout(30.0);
                match transport.send(&request).await {
                    Ok(response) if response.status == 200 => {}
                    Ok(_) => return invalid("Google did not confirm revocation"),
                    // Any non-2xx or transport failure preserves the local grant.
                    Err(_) => {
                        return invalid("Google revocation failed; local grant preserved")
                    }
                }
            }
            std::fs::remove_file(&grant_file_path)
                .map_err(|e| OAuthError::Io(e.to_string()))?;
            oauth_flow::mark_revoked(&flow_path(root, provider)?)?;
            Ok(json!({
                "ok": true,
                "configured": false,
                "remote_revoked": args.action == "revoke",
                "scope": "stored OAuth grant only; environment bearers and service accounts are unchanged",
            }))
        }
        _ => invalid(
            "action must be status, start, callback, connect, refresh, cancel, disconnect, or revoke",
        ),
    }
}
