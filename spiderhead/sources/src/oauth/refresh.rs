//! Token refresh and form POST helpers for the OAuth grant.

use super::*;

/// POST `fields` urlencoded to `url`, returning the decoded body taxonomy.
pub(super) async fn post_form<T: Transport>(
    transport: &T,
    url: &str,
    fields: &[(String, String)],
) -> Result<Value, FormOutcome> {
    let request = Request::new("POST", url)
        .header("Content-Type", "application/x-www-form-urlencoded")
        .body(urlencode(fields).into_bytes())
        .timeout(30.0);
    match transport.send(&request).await {
        Ok(response) => {
            let text = match pystr::decode_utf8_strict(&response.body) {
                Ok(text) => text,
                Err(message) => return Err(FormOutcome::Undecodable(message)),
            };
            match serde_json::from_str(&text) {
                Ok(body) => Ok(body),
                Err(_) => Err(FormOutcome::Decode(text)),
            }
        }
        Err(TransportError::Http(response)) => Err(FormOutcome::Http(response)),
        Err(TransportError::Connect(kind)) => Err(FormOutcome::Connect(kind)),
        Err(TransportError::InvalidRequest(message)) => Err(FormOutcome::Connect(message)),
    }
}

/// Connection or timeout failure from a form POST, passed through with its message.
pub(super) fn connect_error(kind: &str) -> OAuthError {
    if kind == "timeout" {
        OAuthError::Connect("TimeoutError", "timed out".to_owned())
    } else {
        OAuthError::Connect("URLError", crate::pyerr::url_error(kind))
    }
}

/// `refresh_access_token`: exchange the stored refresh token for a short-lived bearer.
///
/// `invalid_grant` marks the stored flow record invalid; any other rejection keeps it.
/// A transport failure is not swallowed: it propagates to the caller unchanged.
pub async fn refresh_access_token<T: Transport>(
    root: &Path,
    provider: &str,
    transport: &T,
) -> Result<Value, OAuthError> {
    let stored = grant(root, provider)?;
    let fields = [
        ("grant_type".to_owned(), "refresh_token".to_owned()),
        (
            "refresh_token".to_owned(),
            stored
                .get("refresh_token")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_owned(),
        ),
        (
            "client_id".to_owned(),
            stored
                .get("client_id")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_owned(),
        ),
        (
            "client_secret".to_owned(),
            stored
                .get("client_secret")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_owned(),
        ),
    ];
    let body = match post_form(transport, TOKEN_HOST, &fields).await {
        Ok(body) => body,
        Err(FormOutcome::Http(response)) => {
            let text = String::from_utf8_lossy(response.read(8193)).into_owned();
            let parsed: Value = serde_json::from_str(&text).unwrap_or(Value::Null);
            let rejected = parsed
                .as_object()
                .and_then(|m| m.get("error"))
                .and_then(Value::as_str)
                == Some("invalid_grant");
            if rejected {
                mark_invalid_grant(root, provider)?;
                return Err(OAuthError::Missing("invalid_grant".to_owned()));
            }
            return Err(OAuthError::Missing(
                "OAuth refresh failed; provider rejected the grant".to_owned(),
            ));
        }
        Err(FormOutcome::Connect(kind)) => return Err(connect_error(&kind)),
        // A 2xx body that is not valid JSON propagates unchanged.
        Err(FormOutcome::Decode(text)) => {
            return Err(OAuthError::Invalid(
                pyjson::decode_error(&text)
                    .unwrap_or_else(|| "Expecting value: line 1 column 1 (char 0)".to_owned()),
            ))
        }
        // A body that is not valid UTF-8 propagates unchanged.
        Err(FormOutcome::Undecodable(message)) => {
            return Err(OAuthError::Connect("UnicodeDecodeError", message))
        }
    };
    let token = body.get("access_token").and_then(Value::as_str);
    match token {
        Some(token) if !token.is_empty() => Ok(json!({
            "access_token": token,
            "scopes": stored.get("scopes").cloned().unwrap_or(Value::Null),
            "expires_in": body.get("expires_in").cloned().unwrap_or(Value::Null),
        })),
        _ => {
            let rejected = body
                .as_object()
                .and_then(|m| m.get("error"))
                .and_then(Value::as_str)
                == Some("invalid_grant");
            if rejected {
                mark_invalid_grant(root, provider)?;
                return Err(OAuthError::Missing("invalid_grant".to_owned()));
            }
            Err(OAuthError::Missing(
                "OAuth refresh did not return an access token".to_owned(),
            ))
        }
    }
}

pub(super) fn mark_invalid_grant(root: &Path, provider: &str) -> Result<(), OAuthError> {
    oauth_flow::mark_invalid(&flow_path(root, provider)?)?;
    Ok(())
}

/// The keyword arguments of `manage_grant`.
#[derive(Debug, Default)]
pub struct Manage<'a> {
    /// `action`: status, start, callback, connect, refresh, cancel, disconnect, revoke.
    pub action: &'a str,
    /// `grant_file` for `connect`.
    pub grant_file: Option<&'a str>,
    /// `confirm` for disconnect/revoke.
    pub confirm: bool,
    /// `state`/`code` for `callback`.
    pub state: Option<&'a str>,
    /// `code` for `callback`.
    pub code: Option<&'a str>,
    /// Fixed clock as epoch microseconds; `None` reads the wall clock.
    pub now: Option<i64>,
}

pub(super) fn now_or(now: Option<i64>) -> i64 {
    now.unwrap_or_else(pytime::now_micros)
}

pub(super) fn flow_public(flow: &Map<String, Value>, now: i64) -> Result<Value, OAuthError> {
    Ok(json!({
        "flow_id": flow.get("flow_id").cloned().unwrap_or(Value::Null),
        "status": oauth_flow::effective_status(flow, now)?,
        "expires_at": flow.get("expires_at").cloned().unwrap_or(Value::Null),
        "connected_at": flow.get("connected_at").cloned().unwrap_or(Value::Null),
        "token_expires_at": flow.get("token_expires_at").cloned().unwrap_or(Value::Null),
    }))
}
