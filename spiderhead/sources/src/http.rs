//! Provider HTTP boundary with explicit redirect control, plus the shared retry mechanics.
//!
//! Every client sends requests through the [`Transport`] seam. Following redirects is opt-in:
//! credential-bearing requests turn any 3xx into an error, so an Authorization header can never
//! leak to a redirect target. Tests replay recorded responses through the seam without touching
//! the network.

use std::future::Future;
use std::sync::{Arc, Mutex};
use std::time::Duration;

/// One fixed request: method, URL, headers, body, timeout and redirect policy.
#[derive(Debug, Clone)]
pub struct Request {
    /// HTTP method (`GET`, `POST`, ...).
    pub method: String,
    /// The absolute request URL.
    pub url: String,
    /// Client-set headers in insertion order (transport-added `Host`/`Content-Length`
    /// are a wire concern and are not recorded).
    pub headers: Vec<(String, String)>,
    /// Request body bytes; empty when the request has none.
    pub body: Vec<u8>,
    /// Whole-request timeout in seconds.
    pub timeout: f64,
    /// Redirect policy for this request.
    pub redirects: Redirects,
}

impl Request {
    /// A `GET` request that refuses redirects, the default for credential-bearing calls.
    pub fn get(url: impl Into<String>) -> Self {
        Self::new("GET", url)
    }

    /// A request that refuses redirects.
    pub fn new(method: &str, url: impl Into<String>) -> Self {
        Request {
            method: method.to_owned(),
            url: url.into(),
            headers: Vec::new(),
            body: Vec::new(),
            timeout: 30.0,
            redirects: Redirects::Refused,
        }
    }

    /// A request that follows redirects (used by keyless public sources).
    pub fn following(method: &str, url: impl Into<String>) -> Self {
        let mut request = Self::new(method, url);
        request.redirects = Redirects::Follow;
        request
    }

    /// Add a client header (insertion order preserved).
    pub fn header(mut self, name: &str, value: impl Into<String>) -> Self {
        self.headers.push((name.to_owned(), value.into()));
        self
    }

    /// Attach a body.
    pub fn body(mut self, body: Vec<u8>) -> Self {
        self.body = body;
        self
    }

    /// Set the timeout in seconds.
    pub fn timeout(mut self, seconds: f64) -> Self {
        self.timeout = seconds;
        self
    }
}

/// Whether a 3xx answer is followed or returned as an error.
/// Following stops after ten hops.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Redirects {
    /// Any 3xx surfaces as [`TransportError::Http`]; nothing is re-sent.
    Refused,
    /// Up to ten redirects are followed by the transport.
    Follow,
}

/// A completed HTTP answer: status line, headers and the full body.
#[derive(Debug, Clone)]
pub struct Response {
    /// Numeric status code.
    pub status: u16,
    /// Response headers; names lowercased for case-insensitive lookup.
    pub headers: Vec<(String, String)>,
    /// Full response body.
    pub body: Vec<u8>,
}

impl Response {
    /// First `limit` body bytes.
    pub fn read(&self, limit: usize) -> &[u8] {
        let end = self.body.len().min(limit);
        &self.body[..end]
    }

    /// First value of a header, matched case-insensitively.
    pub fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|(k, _)| k.eq_ignore_ascii_case(name))
            .map(|(_, v)| v.as_str())
    }
}

/// Request failure: either a received non-2xx answer, or a transport-level failure with no response.
#[derive(Debug)]
pub enum TransportError {
    /// A response was received and carries an error status (including a refused 3xx).
    Http(Response),
    /// Nothing came back: DNS, connect, TLS, timeout, interrupted body.
    Connect(String),
    /// The request itself could not be built.
    InvalidRequest(String),
}

impl TransportError {
    /// HTTP status code, when a response exists.
    pub fn status(&self) -> Option<u16> {
        match self {
            TransportError::Http(response) => Some(response.status),
            _ => None,
        }
    }

    /// The error body; empty for transport failures.
    pub fn body(&self) -> &[u8] {
        match self {
            TransportError::Http(response) => &response.body,
            _ => &[],
        }
    }

    /// Bounded body text for diagnostics; invalid UTF-8 is replaced.
    pub fn body_text(&self, limit: usize) -> String {
        String::from_utf8_lossy(&self.body()[..self.body().len().min(limit)]).into_owned()
    }

    /// Response header lookup, case-insensitive.
    pub fn header(&self, name: &str) -> Option<&str> {
        match self {
            TransportError::Http(response) => response.header(name),
            _ => None,
        }
    }
}

impl std::fmt::Display for TransportError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            TransportError::Http(r) => write!(f, "HTTP {}", r.status),
            TransportError::Connect(msg) => write!(f, "{msg}"),
            TransportError::InvalidRequest(msg) => write!(f, "{msg}"),
        }
    }
}

impl std::error::Error for TransportError {}

/// The send boundary every provider client talks to. Implementations: [`ReqwestTransport`]
/// for real use, the test/stub transports under `tests/`.
pub trait Transport: Send + Sync {
    /// Send one request; the future resolves to the response or the transport error.
    fn send<'a>(
        &'a self,
        request: &'a Request,
    ) -> impl Future<Output = Result<Response, TransportError>> + Send + 'a;
}

/// The production transport: `reqwest` with rustls, per-request timeout, and the two
/// redirect behaviours.
pub struct ReqwestTransport {
    strict: reqwest::Client,
    follow: reqwest::Client,
}

impl ReqwestTransport {
    /// Build the default transport with the redirect policies described above.
    pub fn new() -> Result<Self, TransportError> {
        let strict = reqwest::Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .map_err(|e| TransportError::InvalidRequest(e.to_string()))?;
        let follow = reqwest::Client::builder()
            .redirect(reqwest::redirect::Policy::limited(10))
            .build()
            .map_err(|e| TransportError::InvalidRequest(e.to_string()))?;
        Ok(ReqwestTransport { strict, follow })
    }
}

impl Transport for ReqwestTransport {
    async fn send(&self, request: &Request) -> Result<Response, TransportError> {
        let client = match request.redirects {
            Redirects::Refused => &self.strict,
            Redirects::Follow => &self.follow,
        };
        let method = reqwest::Method::from_bytes(request.method.as_bytes())
            .map_err(|e| TransportError::InvalidRequest(e.to_string()))?;
        let mut builder = client
            .request(method, &request.url)
            .timeout(Duration::from_secs_f64(request.timeout.max(0.0)));
        for (name, value) in &request.headers {
            let name = reqwest::header::HeaderName::from_bytes(name.as_bytes())
                .map_err(|e| TransportError::InvalidRequest(e.to_string()))?;
            let value = reqwest::header::HeaderValue::from_str(value)
                .map_err(|e| TransportError::InvalidRequest(e.to_string()))?;
            builder = builder.header(name, value);
        }
        if !request.body.is_empty() {
            builder = builder.body(request.body.clone());
        }
        let response = builder
            .send()
            .await
            .map_err(|e| TransportError::Connect(e.to_string()))?;
        let status = response.status().as_u16();
        let headers = response
            .headers()
            .iter()
            .map(|(k, v)| {
                (
                    k.as_str().to_owned(),
                    v.to_str().unwrap_or_default().to_owned(),
                )
            })
            .collect();
        let body = response
            .bytes()
            .await
            .map_err(|e| TransportError::Connect(e.to_string()))?
            .to_vec();
        let answer = Response {
            status,
            headers,
            body,
        };
        if (200..300).contains(&status) {
            Ok(answer)
        } else {
            Err(TransportError::Http(answer))
        }
    }
}

// --------------------------------------------------------------------------
// shared retry and backoff mechanics
// --------------------------------------------------------------------------

/// One attempt of a retry loop: its 0-based index and whether another follows.
#[derive(Debug, Clone, Copy)]
pub struct RetrySlot {
    /// 0-based attempt index.
    pub index: usize,
    /// Whether another attempt follows this one.
    pub has_next: bool,
}

/// Enumerate `count` attempts of a retry loop (`retry_slots`).
pub fn retry_slots(count: usize) -> impl Iterator<Item = RetrySlot> {
    (0..count).map(move |index| RetrySlot {
        index,
        has_next: index + 1 < count,
    })
}

/// The shared backoff seconds after a 0-based attempt: `2**attempt + 1`.
pub fn exponential_pause(attempt: usize) -> f64 {
    2f64.powi(attempt as i32) + 1.0
}

/// Backoff seconds that grow by `step` per attempt: `step * (attempt + 1)`.
pub fn linear_pause(attempt: usize, step: f64) -> f64 {
    step * (attempt as f64 + 1.0)
}

/// Seconds to wait after a 429/5xx: the server's `Retry-After` when it exceeds the local
/// `2**attempt` backoff, capped at 30 seconds.
pub fn retry_after_pause(headers: &[(String, String)], attempt: usize) -> f64 {
    let raw = headers
        .iter()
        .find(|(k, _)| k.eq_ignore_ascii_case("retry-after"))
        .map(|(_, v)| v.as_str())
        .filter(|v| !v.is_empty());
    let wait = raw.and_then(crate::pyjson::float_parse).unwrap_or(0.0);
    wait.max(2f64.powi(attempt as i32)).min(30.0)
}

/// Form-encode key/value pairs: space becomes `+`, unreserved
/// `A-Za-z0-9_.-~` pass through, everything else percent-encodes its UTF-8 bytes uppercase.
pub fn urlencode(pairs: &[(String, String)]) -> String {
    let mut out = String::new();
    for (index, (key, value)) in pairs.iter().enumerate() {
        if index > 0 {
            out.push('&');
        }
        quote_plus_into(key, &mut out);
        out.push('=');
        quote_plus_into(value, &mut out);
    }
    out
}

fn quote_plus_into(text: &str, out: &mut String) {
    for byte in text.bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'_' | b'.' | b'-' | b'~' => {
                out.push(byte as char)
            }
            b' ' => out.push('+'),
            other => out.push_str(&format!("%{other:02X}")),
        }
    }
}

/// Form-encode one value, with `quote_plus` rules.
pub fn quote_plus(text: &str) -> String {
    let mut out = String::new();
    quote_plus_into(text, &mut out);
    out
}

/// Percent-encode one path segment: like [`quote_plus`], but space becomes `%20` and `/` is encoded.
pub fn quote(text: &str, safe: &str) -> String {
    let mut out = String::new();
    let safe = safe.as_bytes();
    for byte in text.bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'_' | b'.' | b'-' | b'~' => {
                out.push(byte as char)
            }
            b if safe.contains(&b) => out.push(b as char),
            other => out.push_str(&format!("%{other:02X}")),
        }
    }
    out
}

/// Injectable sleep: real `tokio::time::sleep` in production, a recorder in tests so retry
/// pauses can be checked without waiting.
#[derive(Debug, Clone, Default)]
pub enum Sleeper {
    /// `tokio::time::sleep`.
    #[default]
    Real,
    /// Records requested seconds without sleeping.
    Recorded(Arc<Mutex<Vec<f64>>>),
}

impl Sleeper {
    /// A recording sleeper plus access to the recorded pauses.
    pub fn recording() -> (Self, Arc<Mutex<Vec<f64>>>) {
        let pauses = Arc::new(Mutex::new(Vec::new()));
        (Sleeper::Recorded(pauses.clone()), pauses)
    }

    /// Sleep for the given seconds, or record them instead when `Recorded`.
    pub async fn pause(&self, seconds: f64) {
        match self {
            Sleeper::Real => tokio::time::sleep(Duration::from_secs_f64(seconds.max(0.0))).await,
            Sleeper::Recorded(store) => store
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .push(seconds),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn retry_slots_carry_index_and_has_next() {
        let slots: Vec<(usize, bool)> = retry_slots(3).map(|s| (s.index, s.has_next)).collect();
        assert_eq!(slots, vec![(0, true), (1, true), (2, false)]);
    }

    #[test]
    fn retry_slots_of_one_has_no_next() {
        let slots: Vec<(usize, bool)> = retry_slots(1).map(|s| (s.index, s.has_next)).collect();
        assert_eq!(slots, vec![(0, false)]);
    }

    #[test]
    fn exponential_pause_is_two_power_plus_one() {
        let pauses: Vec<f64> = (0..4).map(exponential_pause).collect();
        assert_eq!(pauses, vec![2.0, 3.0, 5.0, 9.0]);
    }

    #[test]
    fn linear_pause_grows_by_step() {
        let pauses: Vec<f64> = (0..3).map(|i| linear_pause(i, 2.0)).collect();
        assert_eq!(pauses, vec![2.0, 4.0, 6.0]);
    }

    #[test]
    fn retry_after_pause_table() {
        let cases: [(&str, usize, f64); 6] = [
            ("", 0, 1.0),
            ("5", 0, 5.0),
            ("0.5", 2, 4.0),
            ("120", 1, 30.0),
            ("not-a-number", 2, 4.0),
            ("Mon, 01 Jan 2024 00:00:00 GMT", 0, 1.0),
        ];
        for (retry_after, attempt, expected) in cases {
            let headers = if retry_after.is_empty() {
                Vec::new()
            } else {
                vec![("Retry-After".to_string(), retry_after.to_string())]
            };
            assert_eq!(retry_after_pause(&headers, attempt), expected);
        }
    }

    #[test]
    fn urlencode_matches_reference() {
        assert_eq!(
            urlencode(&[
                ("a b".into(), "x+y".into()),
                ("k".into(), "тест".into()),
                ("tilde".into(), "~ok".into()),
            ]),
            "a+b=x%2By&k=%D1%82%D0%B5%D1%81%D1%82&tilde=~ok"
        );
        assert_eq!(urlencode(&[]), "");
    }
}
