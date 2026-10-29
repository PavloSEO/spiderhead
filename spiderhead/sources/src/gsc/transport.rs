//! HTTP call helpers for Search Console: one request, error mapping and JSON response parsing.

use super::*;

/// Outcome of one request before it is mapped to a response envelope.
pub(crate) enum Fetch {
    /// Decoded response body text.
    Body(String),
    /// A non-2xx answer was received.
    Http(crate::http::Response),
    /// The body exceeded [`MAX_RESPONSE_BYTES`].
    TooLarge,
    /// Nothing came back: connection, DNS, TLS or timeout failure.
    Connect(String),
    /// The body was not valid UTF-8.
    Decode(String),
}

/// `_request` — one authenticated JSON call against the `webmasters/v3` or inspection
/// endpoints; the token travels in the `Authorization` header, never in the URL.
pub(crate) async fn send<T: Transport>(
    transport: &T,
    method: &str,
    url: &str,
    payload: Option<&Value>,
    bearer: &str,
) -> Fetch {
    let mut request = Request::new(method, url)
        .header("Authorization", format!("Bearer {bearer}"))
        .header("Content-Type", "application/json")
        .timeout(TIMEOUT);
    if let Some(payload) = payload {
        request = request.body(pyjson::dumps(payload).into_bytes());
    }
    match transport.send(&request).await {
        Ok(response) => {
            if response.body.len() > MAX_RESPONSE_BYTES {
                return Fetch::TooLarge;
            }
            match pystr::decode_utf8_strict(&response.body) {
                Ok(text) => Fetch::Body(text),
                Err(message) => Fetch::Decode(message),
            }
        }
        Err(TransportError::Http(response)) => Fetch::Http(response),
        Err(TransportError::Connect(kind)) => Fetch::Connect(kind),
        Err(TransportError::InvalidRequest(message)) => Fetch::Connect(message),
    }
}

/// Message text for transport failures that providers put in `error` fields.
pub(crate) fn transport_text(kind: &str) -> String {
    pyerr::url_error(kind)
}

/// Provider error text: `error.message`, or the HTTP reason phrase, with the bearer redacted.
/// A non-object `error` member is reported as an error.
pub(super) fn api_error(
    response: &crate::http::Response,
    secret: &str,
) -> Result<String, GscError> {
    let text = String::from_utf8_lossy(&response.body);
    let parsed: Value = match serde_json::from_str(&text) {
        Ok(v) => v,
        Err(_) => return Ok(pyerr::reason_phrase(response.status).to_owned()),
    };
    // A non-object body or `error` member is a failure, not an empty error.
    let type_name = |v: &Value| match v {
        Value::Null => "NoneType",
        Value::Bool(_) => "bool",
        Value::Number(_) => {
            if v.is_i64() || v.is_u64() {
                "int"
            } else {
                "float"
            }
        }
        Value::String(_) => "str",
        Value::Array(_) => "list",
        Value::Object(_) => "dict",
    };
    if !parsed.is_object() {
        return Err(GscError::Wrapped(
            "AttributeError",
            format!("'{}' object has no attribute 'get'", type_name(&parsed)),
        ));
    }
    let error_member = parsed.get("error").cloned().unwrap_or_else(|| json!({}));
    let message = match &error_member {
        Value::Object(error) => {
            let candidate = error.get("message").filter(|v| py_truthy(v));
            candidate
                .map(pystr::repr_value)
                .unwrap_or_else(|| pyerr::reason_phrase(response.status).to_owned())
        }
        other => {
            return Err(GscError::Wrapped(
                "AttributeError",
                format!("'{}' object has no attribute 'get'", type_name(other)),
            ))
        }
    };
    Ok(if secret.is_empty() {
        message
    } else {
        message.replace(secret, "[redacted]")
    })
}

/// Truthiness of a JSON value: null, false, zero, and empty string, array or object are falsy.
pub(crate) fn py_truthy(v: &Value) -> bool {
    match v {
        Value::Null => false,
        Value::Bool(b) => *b,
        Value::Number(n) => n.as_f64().map(|f| f != 0.0).unwrap_or(false),
        Value::String(s) => !s.is_empty(),
        Value::Array(a) => !a.is_empty(),
        Value::Object(m) => !m.is_empty(),
    }
}

/// `_response_object` — parsed JSON when it is an object, else `None`.
pub(crate) fn response_object(raw: &str) -> Option<Map<String, Value>> {
    serde_json::from_str::<Value>(raw)
        .ok()
        .and_then(|v| v.as_object().cloned())
}
