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

/// Attempts per URL when the server drops a connection under the request.
const CONNECTION_ATTEMPTS: u32 = 3;

/// One GET request with the fixed header set.
/// Transport failures are returned in the result, not as an error. A
/// connection the server closed under the request (keep-alive recycling,
/// HTTP/2 GOAWAY) is not a page failure: the request is repeated on a fresh
/// connection and only a persistent drop is reported.
pub async fn get(client: &reqwest::Client, url: &str, user_agent: &str) -> RawResponse {
    let started = Instant::now();
    let mut attempt = 1;
    loop {
        match get_once(client, url, user_agent).await {
            Ok(mut response) => {
                response.response_time = round3(started.elapsed().as_secs_f64());
                return response;
            }
            Err(e) if is_dropped_connection(&e) && attempt < CONNECTION_ATTEMPTS => {
                attempt += 1;
            }
            Err(e) => return failure(&e, round3(started.elapsed().as_secs_f64())),
        }
    }
}

async fn get_once(
    client: &reqwest::Client,
    url: &str,
    user_agent: &str,
) -> Result<RawResponse, reqwest::Error> {
    let mut request = client.get(url);
    for (name, value) in request_header_template(user_agent, &host_of(url)) {
        if name == "host" {
            continue; // reqwest derives Host from the URL; the pair is only recorded
        }
        request = request.header(name.as_str(), value.as_str());
    }
    let response = request.send().await?;
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
    let body = match response.bytes().await {
        Ok(bytes) => bytes.to_vec(),
        // A body cut short by a dropped connection must not pass as a complete page.
        Err(e) if is_dropped_connection(&e) => return Err(e),
        Err(_) => Vec::new(),
    };
    Ok(RawResponse {
        status: Some(status),
        headers,
        body,
        response_time: 0.0,
        error: String::new(),
        error_kind: None,
    })
}

/// The error with its whole cause chain, so a failed page says why.
fn describe(e: &reqwest::Error) -> String {
    let mut text = e.to_string();
    let mut source = std::error::Error::source(e);
    while let Some(inner) = source {
        text.push_str(": ");
        text.push_str(&inner.to_string());
        source = inner.source();
    }
    text
}

fn failure(e: &reqwest::Error, response_time: f64) -> RawResponse {
    let (message, kind) = if e.is_timeout() {
        (
            format!("timed out: {}", describe(e)),
            FetchErrorKind::Timeout,
        )
    } else {
        (
            format!("connection failed: {}", describe(e)),
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

/// Whether the server closed an established connection under the request, as
/// opposed to a timeout or a connection that was never made.
fn is_dropped_connection(e: &reqwest::Error) -> bool {
    if e.is_timeout() || e.is_connect() {
        return false;
    }
    let mut chain = vec![e.to_string()];
    let mut source = std::error::Error::source(e);
    while let Some(inner) = source {
        chain.push(inner.to_string());
        source = inner.source();
    }
    let text = chain.join(" ").to_ascii_lowercase();
    [
        "connection closed",
        "connection reset",
        "connection aborted",
        "broken pipe",
        "unexpected eof",
        "unexpected end of file",
        "incomplete message",
        "go_away",
        "goaway",
        "go away",
        "channel closed",
        "canceled",
    ]
    .iter()
    .any(|needle| text.contains(needle))
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
