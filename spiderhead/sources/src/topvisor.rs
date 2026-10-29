//! Bounded read-only Topvisor API access using central provider credentials.
//!
//! One `fetch` entry point reads exactly one page from an allowlisted `get` endpoint; it never launches checks.
//! The continuation signal is the presence of `nextOffset`, never a comparison of the result length with the limit.

use crate::credentials::{self, CredentialError};
use crate::http::{Request, Response, Transport, TransportError};
use crate::pystr;
use serde_json::{Map, Value};

/// Provider label written to the spend log.
pub const SOURCE: &str = "topvisor";

const ENDPOINTS: [(&str, &str); 6] = [
    ("projects", "projects_2/projects"),
    ("competitors", "projects_2/competitors"),
    ("groups", "keywords_2/groups"),
    ("keywords", "keywords_2/keywords"),
    ("history", "positions_2/history"),
    ("summary", "positions_2/summary"),
];

/// List operations return arrays; history and summary return objects.
const LIST_OPERATIONS: [&str; 4] = ["projects", "competitors", "groups", "keywords"];

/// Provider envelope keys preserved verbatim when present.
const PAGE_META: [&str; 3] = ["nextOffset", "total", "limitedBy"];

/// Errors surfaced by this module, each tagged with its error class.
#[derive(Debug, thiserror::Error)]
pub enum TopvisorError {
    /// Redacted provider or transport error.
    #[error("{0}")]
    Provider(String),
    /// Invalid input, rejected before any request.
    #[error("{0}")]
    Invalid(String),
    /// `MissingCredential` and friends from the credential store.
    #[error("{0}")]
    Credential(#[from] CredentialError),
}

impl TopvisorError {
    /// Stable error class name reported to callers.
    pub fn python_class(&self) -> &'static str {
        match self {
            TopvisorError::Provider(_) => "TopvisorError",
            TopvisorError::Invalid(_) => "ValueError",
            TopvisorError::Credential(e) => e.python_class(),
        }
    }
}

/// Fetch exactly one page from an allowlisted get endpoint (`topvisor.fetch`).
///
/// `params` is the request body object; `limit`/`offset` default to 100/0.
/// The returned page echoes the requested limit/offset separately from provider metadata:
/// `nextOffset` (its presence is the continuation signal), `total` and `limitedBy` pass
/// through when Topvisor sends them.
pub async fn fetch<T: Transport>(
    host: &dyn spiderhead_core::host::Host,
    root: &std::path::Path,
    transport: &T,
    operation: &str,
    params: Option<&Map<String, Value>>,
) -> Result<Value, TopvisorError> {
    let Some(endpoint) = ENDPOINTS
        .iter()
        .find(|(name, _)| *name == operation)
        .map(|(_, path)| *path)
    else {
        return Err(TopvisorError::Invalid(
            "Unsupported read operation".to_owned(),
        ));
    };
    let mut body = params.cloned().unwrap_or_default();
    body.entry("limit".to_owned())
        .or_insert_with(|| json_int(100));
    body.entry("offset".to_owned())
        .or_insert_with(|| json_int(0));
    for (key, low, high) in [("limit", 1i64, 1000i64), ("offset", 0i64, 1_000_000i64)] {
        let value = body.get(key);
        let ok = pystr::as_strict_i64(value)
            .map(|v| (low..=high).contains(&v))
            .unwrap_or(false);
        if !ok {
            return Err(TopvisorError::Invalid(format!(
                "{key} must be an integer between {low} and {high}"
            )));
        }
    }
    if operation != "projects" && !pystr::truthy(body.get("project_id")) {
        return Err(TopvisorError::Invalid("project_id is required".to_owned()));
    }
    let token = credentials::topvisor_token(host, root)?;
    let user_id = credentials::topvisor_user_id(host, root)?;
    if !pystr::is_ascii_decimal(&user_id) {
        return Err(TopvisorError::Invalid(
            "Topvisor user_id must contain only ASCII digits".to_owned(),
        ));
    }
    if token.chars().any(pystr::isspace) {
        return Err(TopvisorError::Invalid(
            "Topvisor token contains whitespace".to_owned(),
        ));
    }
    let request = Request::new(
        "POST",
        format!("https://api.topvisor.com/v2/json/get/{endpoint}"),
    )
    .header("Content-Type", "application/json")
    .header("User-Id", user_id.clone())
    .header("Authorization", format!("bearer {token}"))
    .body(crate::pyjson::dumps(&Value::Object(body.clone())).into_bytes())
    .timeout(30.0);
    let data: Value = match send_and_parse(transport, &request).await {
        Ok(data) => data,
        Err(e) => return Err(e),
    };
    let Some(map) = data.as_object() else {
        return Err(TopvisorError::Provider(
            "Topvisor returned an unexpected envelope".to_owned(),
        ));
    };
    if pystr::truthy(map.get("errors")) {
        let errors = map.get("errors").cloned().unwrap_or(Value::Null);
        // Keep diagnostics useful without exposing echoed credentials.
        let list: Vec<Value> = match errors {
            Value::Array(items) => items,
            single => vec![single],
        };
        let mut details = Vec::new();
        for error in &list {
            if let Value::Object(e) = error {
                let parts: Vec<String> = ["code", "message", "string"]
                    .iter()
                    .filter_map(|k| e.get(*k))
                    .map(pystr::repr_value)
                    .collect();
                let detail = parts
                    .join(": ")
                    .replace(&token, "[redacted]")
                    .replace(&user_id, "[redacted]");
                details.push(detail);
            }
        }
        let joined = details.join("; ");
        let joined: String = joined.chars().take(1000).collect();
        let message = if joined.is_empty() {
            "unspecified".to_owned()
        } else {
            joined
        };
        return Err(TopvisorError::Provider(format!(
            "Topvisor API errors: {message}"
        )));
    }
    if !map.contains_key("result") {
        return Err(TopvisorError::Provider(
            "Topvisor response has no result".to_owned(),
        ));
    }
    let result = map.get("result").cloned().unwrap_or(Value::Null);
    if result.is_null() {
        return Err(TopvisorError::Provider(
            "Topvisor returned a null result".to_owned(),
        ));
    }
    let is_list_op = LIST_OPERATIONS.contains(&operation);
    if is_list_op && !result.is_array() || !is_list_op && !result.is_object() {
        return Err(TopvisorError::Provider(format!(
            "Topvisor {operation} returned an unexpected result type"
        )));
    }
    let limit = body.get("limit").cloned().unwrap_or(json_int(100));
    let offset = body.get("offset").cloned().unwrap_or(json_int(0));
    let mut page = Map::new();
    page.insert("ok".to_owned(), Value::Bool(true));
    page.insert("operation".to_owned(), Value::String(operation.to_owned()));
    page.insert("limit".to_owned(), limit);
    page.insert("offset".to_owned(), offset);
    page.insert("page_only".to_owned(), Value::Bool(true));
    page.insert("result".to_owned(), result);
    for key in PAGE_META {
        if let Some(v) = map.get(key) {
            page.insert(key.to_owned(), v.clone());
        }
    }
    Ok(Value::Object(page))
}

fn json_int(v: i64) -> Value {
    Value::Number(serde_json::Number::from(v))
}

/// Send one request and parse the JSON body, mapping every failure to a provider error.
async fn send_and_parse<T: Transport>(
    transport: &T,
    request: &Request,
) -> Result<Value, TopvisorError> {
    let response: Response = match transport.send(request).await {
        Ok(r) => r,
        Err(TransportError::Http(r)) => {
            return Err(TopvisorError::Provider(format!(
                "Topvisor HTTP {}; response body omitted",
                r.status
            )));
        }
        Err(TransportError::Connect(_)) | Err(TransportError::InvalidRequest(_)) => {
            return Err(TopvisorError::Provider(
                "Topvisor connection failed".to_owned(),
            ));
        }
    };
    let raw = response.read(10_000_001);
    if raw.len() > 10_000_000 {
        return Err(TopvisorError::Provider(
            "Response exceeded 10 MB; request fewer rows or fields".to_owned(),
        ));
    }
    let data: Value = match serde_json::from_slice(raw) {
        Ok(v) => v,
        Err(_) => {
            return Err(TopvisorError::Provider(
                "Topvisor returned invalid JSON".to_owned(),
            ));
        }
    };
    Ok(data)
}
