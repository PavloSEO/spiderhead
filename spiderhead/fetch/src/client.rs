//! The HTTP client: one reqwest client with redirects disabled. The crawler records each 3xx
//! and enqueues the target itself. Request headers are pinned to a fixed wire shape.

use std::time::{Duration, Instant};

/// Error class of a failed transport attempt.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FetchErrorKind {
    /// The request exceeded the connect or total timeout.
    Timeout,
    /// Any other transport failure (DNS, connect, reset) before a response arrived.
    Connection,
}

impl FetchErrorKind {
    /// The `error_kind` string recorded in the scan.
    pub fn as_str(self) -> &'static str {
        match self {
            FetchErrorKind::Timeout => "timeout",
            FetchErrorKind::Connection => "connection",
        }
    }
}

/// A completed or failed transport attempt, before any parsing.
#[derive(Debug)]
pub struct RawResponse {
    /// Final HTTP status (`None` when the transport failed).
    pub status: Option<u16>,
    /// Response headers in wire order (lowercased names, values as received).
    /// Header redaction to the scan allowlist happens in the store.
    pub headers: Vec<(String, String)>,
    /// Raw body bytes after transfer decoding (no content-decoding applied).
    pub body: Vec<u8>,
    /// Wall-clock seconds, rounded to milliseconds.
    pub response_time: f64,
    /// Transport error message (`""` on success).
    pub error: String,
    /// Transport error class (`None` on success).
    pub error_kind: Option<FetchErrorKind>,
}

/// The exact request header set sent with every request, in a fixed order.
/// The `host` pair is filled per URL.
pub fn request_header_template(user_agent: &str, host: &str) -> Vec<(String, String)> {
    vec![
        ("accept".to_string(), "*/*".to_string()),
        ("user-agent".to_string(), user_agent.to_string()),
        ("host".to_string(), host.to_string()),
        ("accept-encoding".to_string(), "gzip, deflate".to_string()),
    ]
}

/// Build the shared client. Redirects are never followed by the client.
pub fn build_client(timeout_seconds: f64) -> Result<reqwest::Client, String> {
    reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .timeout(Duration::from_secs_f64(timeout_seconds))
        .connect_timeout(Duration::from_secs_f64(timeout_seconds))
        .build()
        .map_err(|e| format!("http client build failed: {e}"))
}

/// One GET request with the fixed header set.
/// Transport failures are returned in the result, not as an error.
pub async fn get(client: &reqwest::Client, url: &str, user_agent: &str) -> RawResponse {
    let started = Instant::now();
    let mut request = client.get(url);
    for (name, value) in request_header_template(user_agent, &host_of(url)) {
        if name == "host" {
            continue; // reqwest derives Host from the URL; the pair is only recorded
        }
        request = request.header(name.as_str(), value.as_str());
    }
    let outcome = request.send().await;
    let response_time = round3(started.elapsed().as_secs_f64());
    match outcome {
        Ok(response) => {
            let status = response.status().as_u16();
            let headers: Vec<(String, String)> = response
                .headers()
                .iter()
                .map(|(name, value)| {
                    (
                        name.as_str().to_ascii_lowercase(),
                        value.to_str().unwrap_or("").to_string(),
                    )
                })
                .collect();
            let body = response.bytes().await.unwrap_or_default().to_vec();
            RawResponse {
                status: Some(status),
                headers,
                body,
                response_time,
                error: String::new(),
                error_kind: None,
            }
        }
        Err(e) => {
            let (message, kind) = if e.is_timeout() {
                (format!("timed out: {e}"), FetchErrorKind::Timeout)
            } else {
                (
                    format!("connection failed: {e}"),
                    FetchErrorKind::Connection,
                )
            };
            RawResponse {
                status: None,
                headers: Vec::new(),
                body: Vec::new(),
                response_time,
                error: message,
                error_kind: Some(kind),
            }
        }
    }
}

/// The `host` header value recorded for a request: `host:port` from the URL.
pub fn host_of(url: &str) -> String {
    match url::Url::parse(url) {
        Ok(parsed) => match parsed.port() {
            Some(port) => format!("{}:{port}", parsed.host_str().unwrap_or("")),
            None => parsed.host_str().unwrap_or("").to_string(),
        },
        Err(_) => String::new(),
    }
}

fn round3(value: f64) -> f64 {
    (value * 1000.0).round() / 1000.0
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn host_header_matches_the_oracle_shape() {
        assert_eq!(host_of("http://127.0.0.1:18701/"), "127.0.0.1:18701");
        assert_eq!(host_of("http://example.com/a"), "example.com");
    }

    #[test]
    fn header_template_is_the_httpx_set() {
        let headers = request_header_template("UA", "127.0.0.1:1");
        assert_eq!(
            headers,
            vec![
                ("accept".to_string(), "*/*".to_string()),
                ("user-agent".to_string(), "UA".to_string()),
                ("host".to_string(), "127.0.0.1:1".to_string()),
                ("accept-encoding".to_string(), "gzip, deflate".to_string()),
            ]
        );
    }
}
