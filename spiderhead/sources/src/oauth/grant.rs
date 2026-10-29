//! Grant storage: atomic save, validated load and availability checks.

use super::*;

/// `save_grant` — persist a validated read-only grant atomically; never replaces.
pub fn save_grant(root: &Path, provider: &str, grant: &Value) -> Result<(), OAuthError> {
    let required: std::collections::BTreeSet<&str> =
        ["refresh_token", "client_id", "client_secret", "scopes"]
            .into_iter()
            .collect();
    let map = match grant.as_object() {
        Some(m)
            if m.keys()
                .map(String::as_str)
                .collect::<std::collections::BTreeSet<_>>()
                == required =>
        {
            m
        }
        _ => return invalid("OAuth grant has an unsupported shape"),
    };
    for key in ["refresh_token", "client_id", "client_secret"] {
        match map.get(key).and_then(Value::as_str) {
            Some(s) if !s.is_empty() => {}
            _ => return invalid("OAuth grant has an unsupported shape"),
        }
    }
    let scopes_ok = map
        .get("scopes")
        .and_then(Value::as_array)
        .map(|s| s == &[json!(READONLY_SCOPE)])
        .unwrap_or(false);
    if !scopes_ok {
        return invalid("GSC grants must have only the webmasters.readonly scope");
    }
    let target = path(root, provider)?;
    let parent = target.parent().map(Path::to_owned).unwrap_or_default();
    if is_symlink(&parent) || is_symlink(&target) {
        return invalid("OAuth storage must not use symlinks");
    }
    // The 0700 mode applies only to a directory this call creates.
    let missing = !parent.exists();
    std::fs::create_dir_all(&parent).map_err(|e| OAuthError::Io(e.to_string()))?;
    #[cfg(unix)]
    if missing {
        let _ = spiderhead_core::filesystem::set_permissions_mode(&parent, 0o700);
    }
    #[cfg(not(unix))]
    let _ = missing;
    if target.exists() {
        return invalid("OAuth grant already exists; revoke or disconnect it before reconnecting");
    }
    let body = pyjson::dumps_opts(grant, true, true);
    let staged = crate::credentials::stage_bytes(&parent, ".oauth-", body.as_bytes(), 0o600)
        .map_err(|e| OAuthError::Io(e.to_string()))?;
    // Publish without replacing a grant created by another writer.
    let published = std::fs::hard_link(&staged, &target);
    let _ = std::fs::remove_file(&staged);
    published.map_err(|e| OAuthError::Io(e.to_string()))?;
    Ok(())
}

/// Load and validate the stored grant; fails with `MissingCredential` when absent or invalid.
pub(super) fn grant(root: &Path, provider: &str) -> Result<Map<String, Value>, OAuthError> {
    let target = path(root, provider)?;
    let info = target.symlink_metadata().ok();
    let usable = info
        .as_ref()
        .map(|m| {
            m.is_file()
                && !m.file_type().is_symlink()
                && mode_of(&target).map(is_private_mode).unwrap_or(false)
                && m.len() <= GRANT_MAX_BYTES
        })
        .unwrap_or(false);
    if !usable {
        return Err(OAuthError::Missing(
            "durable OAuth grant is not configured".to_owned(),
        ));
    }
    let value: Value = std::fs::read_to_string(&target)
        .ok()
        .and_then(|t| serde_json::from_str(&t).ok())
        .ok_or_else(|| OAuthError::Missing("durable OAuth grant is unreadable".to_owned()))?;
    let valid = value
        .as_object()
        .map(|m| {
            m.len() == 4
                && ["refresh_token", "client_id", "client_secret"]
                    .iter()
                    .all(|k| {
                        m.get(*k)
                            .and_then(Value::as_str)
                            .is_some_and(|s| !s.is_empty())
                    })
                && m.get("scopes").and_then(Value::as_array) == Some(&vec![json!(READONLY_SCOPE)])
        })
        .unwrap_or(false);
    if !valid {
        return Err(OAuthError::Missing(
            "durable OAuth grant is invalid".to_owned(),
        ));
    }
    Ok(value.as_object().cloned().unwrap_or_default())
}

/// `grant_available` — whether a valid durable grant is stored locally.
pub fn grant_available(root: &Path, provider: &str) -> bool {
    grant(root, provider).is_ok()
}

/// Outcome of a form POST to the token endpoint: a parsed JSON body, an HTTP error (the response is
/// kept for its body), a transport failure, or a non-JSON body on a 2xx answer.
pub(super) enum FormOutcome {
    /// An HTTP error answer; `body` is the bounded body text, with invalid UTF-8 replaced.
    Http(crate::http::Response),
    /// A connection or timeout failure with no response.
    Connect(String),
    /// The 2xx body was not valid JSON; carries the raw text for the error message.
    Decode(String),
    /// The 2xx body was not valid UTF-8 and propagates as an error.
    Undecodable(String),
}
