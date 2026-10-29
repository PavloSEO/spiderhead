//! Credential selection for Search Console calls: explicit token, then stored grant, then service account.

use super::*;

/// Maps service-account failure statuses to user-facing text.
pub(super) const SERVICE_ACCOUNT_ERRORS: [(&str, &str); 6] = [
    (
        "missing",
        "GSC service-account JSON file is not configured or readable",
    ),
    (
        "unsafe_file",
        "GSC service-account JSON must be a private regular file (mode 0600)",
    ),
    (
        "too_large",
        "GSC service-account JSON exceeds the maximum supported size",
    ),
    (
        "unreadable",
        "GSC service-account JSON exists but cannot be read",
    ),
    (
        "malformed_json",
        "GSC service-account JSON is not valid JSON",
    ),
    (
        "unsupported_shape",
        "GSC service-account JSON has an unsupported type or token URI",
    ),
];

pub(super) fn service_account_error(status: &str) -> &'static str {
    SERVICE_ACCOUNT_ERRORS
        .iter()
        .find(|(key, _)| *key == status)
        .map(|(_, message)| *message)
        .unwrap_or("GSC service-account JSON file is not configured or readable")
}

/// Injected replacement for `service_account_access_token`, used by tests.
/// `None` runs the real JWT exchange over `transport`.
pub type ServiceAccount<'a> = Option<&'a dyn Fn(&str) -> Result<String, CredentialError>>;

/// `_acquire_token` — an explicit bearer first, then a durable grant refresh, then a
/// service account. Non-`MissingCredential` credential errors propagate unchanged.
///
/// `explicit` is a JSON value; falsy members (`""`, `0`, `[]`) fall through to the
/// credential lookup.
pub(crate) async fn acquire_token<T: Transport>(
    host: &dyn Host,
    root: &Path,
    explicit: Option<&Value>,
    transport: &T,
    service_account: ServiceAccount<'_>,
) -> Result<(Option<String>, Option<String>), GscError> {
    let bearer_error = match explicit.filter(|v| py_truthy(v)) {
        Some(value) => return Ok((Some(pystr::repr_value(value)), None)),
        None => match credentials::gsc_access_token(host, root) {
            Ok(token) => return Ok((Some(token), None)),
            Err(CredentialError::Missing(message)) => message,
            Err(other) => return Err(other.into()),
        },
    };
    if oauth::grant_available(root, "gsc") {
        match oauth::refresh_access_token(root, "gsc", transport).await {
            Ok(refreshed) => {
                let token = refreshed
                    .get("access_token")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_owned();
                return Ok((Some(token), None));
            }
            Err(OAuthError::Missing(message)) => {
                if message.to_lowercase().contains("invalid_grant") {
                    return Ok((None, Some("invalid_grant".to_owned())));
                }
                return Ok((
                    None,
                    Some(
                        "stored OAuth grant refresh failed; reconnect or check the grant"
                            .to_owned(),
                    ),
                ));
            }
            // Transport, decode and refresh failures all fold into the same error.
            Err(OAuthError::Invalid(_)) | Err(OAuthError::Io(_)) | Err(OAuthError::Connect(..)) => {
                return Ok((
                    None,
                    Some(
                        "stored OAuth grant refresh failed; reconnect or check the grant"
                            .to_owned(),
                    ),
                ));
            }
            Err(other) => return Err(other.into()),
        }
    }
    let status = credentials::gsc_service_account_status(host, root);
    if status == "missing" {
        return Ok((None, Some(bearer_error)));
    }
    if status != "configured_unverified" {
        return Ok((
            None,
            Some(format!(
                "OAuth bearer unavailable; {}",
                service_account_error(&status)
            )),
        ));
    }
    let result = match service_account {
        Some(source) => source(READONLY_SCOPE),
        None => {
            service_account::service_account_access_token(host, root, READONLY_SCOPE, transport)
                .await
                .map_err(|e| CredentialError::Missing(e.to_string()))
        }
    };
    match result {
        Ok(token) => Ok((Some(token), None)),
        Err(CredentialError::Missing(service_error)) => Ok((
            None,
            Some(format!(
                "OAuth bearer unavailable; service account unavailable: {service_error}"
            )),
        )),
        Err(other) => Err(other.into()),
    }
}

/// `durable_oauth_token` — refresh the stored `webmasters.readonly` grant for a bounded
/// operation; wraps [`oauth::refresh_access_token`].
pub async fn durable_oauth_token<T: Transport>(
    root: &Path,
    transport: &T,
) -> Result<Value, OAuthError> {
    oauth::refresh_access_token(root, "gsc", transport).await
}
