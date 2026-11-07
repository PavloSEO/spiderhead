//! Microsoft Clarity Data Export API: per-project live behavioral insights
//! (issue #47). Free, token per Clarity project, no history — every pull is a
//! dated snapshot the caller keeps. Aggregated metrics with optional
//! dimensions (URL, device, country, source, browser, OS); never session
//! recordings or heatmaps.

use crate::http::{Request, Transport, TransportError};
use serde_json::{json, Value};

/// Base URL of the Clarity Data Export API.
pub const HOST: &str = "https://www.clarity.ms/export-data/api/v1/";
/// Per-request timeout in seconds.
pub const TIMEOUT: f64 = 60.0;
/// The API serves roughly the last 1–3 days per call.
pub const MAX_RANGE_DAYS: i64 = 3;

/// Errors `live_insights` reports: invalid input, an unauthorized token, or a
/// transport failure. Class names mirror the provider conventions.
#[derive(Debug)]
pub struct ClarityError {
    /// Error class name reported to callers.
    pub class: &'static str,
    /// Error message.
    pub message: String,
    /// HTTP status when the provider answered with one.
    pub status: Option<i64>,
}

impl std::fmt::Display for ClarityError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for ClarityError {}

impl ClarityError {
    /// Error class name reported to callers.
    pub fn error_class(&self) -> &'static str {
        self.class
    }
}

fn raise<T>(class: &'static str, message: impl Into<String>) -> Result<T, ClarityError> {
    Err(ClarityError {
        class,
        message: message.into(),
        status: None,
    })
}

/// `YYYY-MM-DD` shape check; anything else is refused before a request.
fn valid_date(text: &str) -> bool {
    let bytes = text.as_bytes();
    bytes.len() == 10
        && bytes[4] == b'-'
        && bytes[7] == b'-'
        && bytes
            .iter()
            .enumerate()
            .all(|(index, b)| index == 4 || index == 7 || b.is_ascii_digit())
}

/// Pull one project's live insights for a date range (at most three days
/// back, the window the API keeps). `medium=json` rows come back as recorded;
/// the answer also carries the request identity so snapshots stay dated.
pub async fn live_insights<T: Transport>(
    transport: &T,
    api_key: &str,
    project_id: &str,
    start_date: &str,
    end_date: &str,
) -> Result<Value, ClarityError> {
    if api_key.trim().is_empty() {
        return raise(
            "ValueError",
            "api_key required: generate one in Clarity under Settings → Project Settings → API Key",
        );
    }
    if project_id.trim().is_empty() {
        return raise("ValueError", "project_id required");
    }
    if !valid_date(start_date) || !valid_date(end_date) {
        return raise("ValueError", "start_date and end_date must be YYYY-MM-DD");
    }
    if end_date < start_date {
        return raise("ValueError", "end_date cannot precede start_date");
    }
    let query = crate::http::urlencode(&[
        ("projectId".to_owned(), project_id.to_owned()),
        ("startDate".to_owned(), start_date.to_owned()),
        ("endDate".to_owned(), end_date.to_owned()),
        ("medium".to_owned(), "json".to_owned()),
    ]);
    let request = Request::following("GET", format!("{HOST}project-live-insights?{query}"))
        .header("Authorization", format!("Bearer {api_key}"))
        .timeout(TIMEOUT);
    let response = match transport.send(&request).await {
        Ok(response) => response,
        Err(TransportError::Http(response)) => {
            let class = if response.status == 401 || response.status == 403 {
                "PermissionError"
            } else {
                "ProviderError"
            };
            return Err(ClarityError {
                class,
                message: format!(
                    "clarity export request rejected with HTTP {}",
                    response.status
                ),
                status: Some(i64::from(response.status)),
            });
        }
        Err(other) => {
            return Err(ClarityError {
                class: "ConnectionError",
                message: other.to_string(),
                status: None,
            })
        }
    };
    let raw = response.body;
    let text = match std::str::from_utf8(&raw) {
        Ok(text) => text,
        Err(_) => {
            return raise(
                "UnicodeDecodeError",
                "clarity export response was not valid UTF-8",
            )
        }
    };
    let rows: Value = match serde_json::from_str(text) {
        Ok(value) => value,
        Err(e) => {
            return raise(
                "ValueError",
                format!("clarity export response was not valid JSON: {e}"),
            )
        }
    };
    let count = rows.as_array().map(Vec::len).unwrap_or(0) as i64;
    Ok(json!({
        "ok": true,
        "provider": "clarity",
        "operation": "live_insights",
        "project_id": project_id,
        "start_date": start_date,
        "end_date": end_date,
        "rows": rows,
        "row_count": count,
        "snapshot_note": "the export API keeps no history; store this dated answer to build trends",
    }))
}
