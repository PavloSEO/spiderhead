//! Guarded HTTP client plus DNS-over-HTTPS, RDAP and system-whois helpers.
//!
//! Every hop of a redirect chain is validated
//! (scheme, hostname, credentials) and resolved once through
//! [`resolve_socket_addresses`]; the connection then goes to the addresses the
//! policy just vetted via reqwest's DNS overrides, never to a second
//! unvalidated lookup. Private/non-public targets are refused unless the
//! `SEOHEAD_ALLOW_PRIVATE_NETWORKS` / `SEOHEAD_ALLOW_PRIVATE_HOSTS` opt-ins
//! name them. Proxy routing is not supported: it is a remote-service feature.

use std::collections::{HashMap, HashSet};
use std::io::Read;
use std::net::SocketAddr;
use std::process::Command;
use std::sync::Mutex;
use std::time::{Duration, Instant};

use serde_json::{json, Value as Json};
use spiderhead_core::host::{Host, SystemHost};
use spiderhead_core::net::{
    allowed_private_hosts, is_valid_whois_server, private_networks_enabled,
    resolve_socket_addresses, whois_field, whois_server_for_tld, WHOIS_REFERRAL_KEYS,
};
use spiderhead_core::net::{PRIVATE_HOST_ALLOWLIST_ENV, PRIVATE_NETWORK_ENV};
use spiderhead_core::user_agents::RECON_USER_AGENT;
use url::Url;

/// DoH resolvers queried in order, first non-empty answer wins.
pub const DOH_ENDPOINTS: [&str; 2] = [
    "https://cloudflare-dns.com/dns-query",
    "https://dns.google/resolve",
];
/// RDAP bootstrap server.
pub const RDAP_BOOTSTRAP: &str = "https://rdap.org";

pub use spiderhead_core::net::{
    normalize_domain, normalize_url, registrable_domain, waf_block_reason, BLOCKED_BY_WAF,
};

const MAX_REDIRECTS: usize = 20;

/// A redirect target refused by [`validate_hop`]: the response was real, only
/// the next hop is refused. Carries that response's status and the refused
/// absolute target.
#[derive(Debug)]
pub struct BlockedRedirectError {
    /// The validation message that refused the target.
    pub message: String,
    /// Status of the response whose `Location` was refused.
    pub status_code: u16,
    /// Absolute redirect target that was refused.
    pub location: String,
}

impl std::fmt::Display for BlockedRedirectError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.message)
    }
}
impl std::error::Error for BlockedRedirectError {}

/// Errors the guarded client or network helpers can return.
/// The recon entry points render all of them as result data.
#[derive(Debug, thiserror::Error)]
pub enum FetchError {
    /// The URL itself failed validation.
    #[error("{0}")]
    Invalid(String),
    /// A redirect hop was refused.
    #[error("{0}")]
    Blocked(#[from] BlockedRedirectError),
    /// The base client could not be built.
    #[error("{0}")]
    Unavailable(String),
    /// The request exceeded its timeout.
    #[error("Timeout")]
    Timeout,
    /// A transport-level failure.
    #[error("{0}")]
    Transport(String),
}

impl FetchError {
    fn transport(error: &reqwest::Error) -> Self {
        if error.is_timeout() {
            return FetchError::Timeout;
        }
        let mut source: Option<&dyn std::error::Error> = std::error::Error::source(error);
        while let Some(inner) = source {
            if let Some(blocked) = inner.downcast_ref::<BlockedRedirectError>() {
                return FetchError::Blocked(BlockedRedirectError {
                    message: blocked.message.clone(),
                    status_code: blocked.status_code,
                    location: blocked.location.clone(),
                });
            }
            source = inner.source();
        }
        FetchError::Transport(error.to_string())
    }
}

/// One parsed `Set-Cookie` pair; `value` is `None` for a valueless name, which
/// is still kept as a named cookie.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Cookie {
    /// Cookie name.
    pub name: String,
    /// Cookie value; `None` when the header carried no `name=value` pair.
    pub value: Option<String>,
}

/// Response surface the recon analyzers consume: status, final URL,
/// multi-valued headers, body, cookie jar, protocol version and elapsed time.
#[derive(Debug, Clone)]
pub struct Response {
    /// HTTP status code.
    pub status_code: u16,
    /// Final URL after redirects.
    pub url: String,
    /// Headers as `(lowercased name, value)` pairs in wire order.
    pub headers: Vec<(String, String)>,
    /// Raw body bytes.
    pub content: Vec<u8>,
    /// Cookies collected from `Set-Cookie` headers, in wire order.
    pub cookies: Vec<Cookie>,
    /// `HTTP/1.1`, `HTTP/2`, `HTTP/3`, or `"unmeasurable"` when unknown.
    pub http_version: String,
    /// Time the request took.
    pub elapsed: Duration,
}

impl Response {
    /// First value of a header, case-insensitive on the name.
    pub fn header(&self, name: &str) -> Option<&str> {
        let want = name.to_ascii_lowercase();
        self.headers
            .iter()
            .find(|(k, _)| k == &want)
            .map(|(_, v)| v.as_str())
    }
    /// All values of a header in wire order.
    pub fn header_list(&self, name: &str) -> Vec<&str> {
        let want = name.to_ascii_lowercase();
        self.headers
            .iter()
            .filter(|(k, _)| k == &want)
            .map(|(_, v)| v.as_str())
            .collect()
    }
    /// Body decoded as UTF-8 (lossy), like `response.text` for the HTML pages
    /// recon consumes.
    pub fn text(&self) -> String {
        String::from_utf8_lossy(&self.content).into_owned()
    }
    /// httpx `is_redirect`: a 3xx status that carries a `Location` header.
    pub fn is_redirect(&self) -> bool {
        matches!(self.status_code, 301 | 302 | 303 | 307 | 308) && self.header("location").is_some()
    }
    /// Parse the body as JSON (`response.json()`).
    pub fn json(&self) -> Result<Json, serde_json::Error> {
        serde_json::from_slice(&self.content)
    }
}

/// Parse `Set-Cookie` headers the way `httpx.Cookies` exposes them: the first
/// `;`-segment is `name=value`; a segment without `=` still yields the name.
fn parse_cookies(headers: &[(String, String)]) -> Vec<Cookie> {
    let mut out = Vec::new();
    for (name, value) in headers {
        if name != "set-cookie" {
            continue;
        }
        let pair = value.split(';').next().unwrap_or("").trim();
        match pair.split_once('=') {
            Some((n, v)) => out.push(Cookie {
                name: n.trim().to_string(),
                value: Some(v.trim().to_string()),
            }),
            None if !pair.is_empty() => out.push(Cookie {
                name: pair.to_string(),
                value: None,
            }),
            _ => {}
        }
    }
    out
}

/// The seam every recon entry point uses. Test doubles answer from a URL map;
/// the real implementation is [`ReqwestReconClient`].
pub trait ReconClient {
    /// GET `url`, following guarded redirects; `params` become the query string.
    fn get(
        &self,
        url: &str,
        headers: &[(String, String)],
        params: &[(String, String)],
    ) -> Result<Response, FetchError>;

    /// Request `url` with an explicit method; only GET and HEAD are required.
    /// The default answers a GET regardless of the requested method.
    fn request(
        &self,
        method: &str,
        url: &str,
        headers: &[(String, String)],
        params: &[(String, String)],
    ) -> Result<Response, FetchError> {
        let _ = method;
        self.get(url, headers, params)
    }

    /// One request with no redirect following — a 3xx response comes back as
    /// is, the way hop-by-hop checkers need it. The default reuses
    /// [`ReconClient::request`], whose canned implementations are already
    /// single-shot.
    fn request_single(
        &self,
        method: &str,
        url: &str,
        headers: &[(String, String)],
    ) -> Result<Response, FetchError> {
        self.request(method, url, headers, &[])
    }
}

/// Test/CLI fake answering canned responses by exact URL; unknown URLs fail
/// like an unreachable host. A key of the form `"METHOD|url"` answers only
/// that method; a plain URL key answers every method. URLs listed in
/// `timeouts` fail with a timeout instead of answering.
#[derive(Default)]
pub struct FakeClient {
    /// URL → canned response.
    pub responses: HashMap<String, Response>,
    /// URLs that time out instead of answering (a `"METHOD|url"` entry wins,
    /// then the plain URL entry).
    pub timeouts: HashSet<String>,
}

impl FakeClient {
    /// The canned response for `method` on `url`: a `"METHOD|url"` entry wins,
    /// then the plain URL entry.
    fn lookup(&self, method: &str, url: &str) -> Option<&Response> {
        self.responses
            .get(&format!("{method}|{url}"))
            .or_else(|| self.responses.get(url))
    }

    /// Answer `method` on `url`: a listed timeout first, then the canned map,
    /// then the unreachable-host failure.
    fn call(&self, method: &str, url: &str) -> Result<Response, FetchError> {
        if self.timeouts.contains(&format!("{method}|{url}")) || self.timeouts.contains(url) {
            return Err(FetchError::Timeout);
        }
        match self.lookup(method, url) {
            Some(response) => Ok(response.clone()),
            None => Err(FetchError::Transport(format!("no route to {url}"))),
        }
    }
}

impl ReconClient for FakeClient {
    fn get(
        &self,
        url: &str,
        _headers: &[(String, String)],
        _params: &[(String, String)],
    ) -> Result<Response, FetchError> {
        self.call("GET", url)
    }

    fn request(
        &self,
        method: &str,
        url: &str,
        _headers: &[(String, String)],
        _params: &[(String, String)],
    ) -> Result<Response, FetchError> {
        self.call(method, url)
    }
}

/// Repr-style quoting for a short identifier:
/// single quotes preferred, double quotes when the text already holds `'`.
pub fn py_repr(value: &str) -> String {
    let quote = if value.contains('\'') && !value.contains('"') {
        '"'
    } else {
        '\''
    };
    let mut out = String::with_capacity(value.len() + 2);
    out.push(quote);
    for c in value.chars() {
        match c {
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if c == quote => {
                out.push('\\');
                out.push(c);
            }
            c => out.push(c),
        }
    }
    out.push(quote);
    out
}

/// Validate a hop syntactically and resolve it once; the returned addresses are
/// the only ones the request may connect to (single-resolution pinning).
fn pin(url: &str, env: &dyn Host) -> Result<(String, Vec<SocketAddr>), String> {
    let parsed = Url::parse(url.trim()).map_err(|error| format!("invalid URL: {error}"))?;
    if !matches!(parsed.scheme(), "http" | "https") {
        return Err("only http:// and https:// URLs are supported".into());
    }
    let Some(host_name) = parsed.host_str().filter(|h| !h.is_empty()) else {
        return Err("URL must include a hostname".into());
    };
    if !parsed.username().is_empty() || parsed.password().is_some() {
        return Err("embedded URL credentials are not supported".into());
    }
    if private_networks_enabled(env) {
        let port = parsed
            .port()
            .unwrap_or(if parsed.scheme() == "https" { 443 } else { 80 });
        return resolve_socket_addresses(host_name, port, env, None)
            .map(|addrs| (host_name.to_string(), addrs));
    }
    let host_name = host_name.trim_end_matches('.').to_lowercase();
    if host_name == "localhost" || host_name.ends_with(".localhost") {
        let allowed = allowed_private_hosts(env);
        if !allowed.contains(&host_name) {
            return Err(format!(
                "private-network target blocked; set {PRIVATE_NETWORK_ENV}=1 to authorize every \
                 private target, or add {} to {PRIVATE_HOST_ALLOWLIST_ENV} to \
                 authorize only this one",
                py_repr(&host_name)
            ));
        }
    }
    let port = parsed
        .port()
        .unwrap_or(if parsed.scheme() == "https" { 443 } else { 80 });
    resolve_socket_addresses(&host_name, port, env, None).map(|addrs| (host_name, addrs))
}

/// The real client: validates every hop and connects to the addresses the
/// policy resolved for that exact hop (issue #142's pinning contract).
pub struct ReqwestReconClient {
    timeout: Duration,
    http2: bool,
    clients: Mutex<HashMap<String, reqwest::blocking::Client>>,
}

impl ReqwestReconClient {
    fn client_for(
        &self,
        host: &str,
        addrs: &[SocketAddr],
    ) -> Result<reqwest::blocking::Client, FetchError> {
        let key = host.to_string();
        if let Some(client) = self.clients.lock().ok().and_then(|m| m.get(&key).cloned()) {
            return Ok(client);
        }
        let builder = reqwest::blocking::Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .cookie_store(true)
            .timeout(self.timeout)
            .user_agent(RECON_USER_AGENT)
            .resolve_to_addrs(host, addrs);
        let builder = if self.http2 {
            builder
        } else {
            builder.http1_only()
        };
        let client = builder
            .build()
            .map_err(|e| FetchError::Unavailable(e.to_string()))?;
        if let Ok(mut map) = self.clients.lock() {
            map.insert(key, client.clone());
        }
        Ok(client)
    }
}

impl ReconClient for ReqwestReconClient {
    fn get(
        &self,
        url: &str,
        headers: &[(String, String)],
        params: &[(String, String)],
    ) -> Result<Response, FetchError> {
        self.send_following("GET", url, headers, params)
    }

    fn request(
        &self,
        method: &str,
        url: &str,
        headers: &[(String, String)],
        params: &[(String, String)],
    ) -> Result<Response, FetchError> {
        self.send_following(method, url, headers, params)
    }

    fn request_single(
        &self,
        method: &str,
        url: &str,
        headers: &[(String, String)],
    ) -> Result<Response, FetchError> {
        self.send_once(method, url, headers, &[])
    }
}

impl ReqwestReconClient {
    fn send_once(
        &self,
        method: &str,
        url: &str,
        headers: &[(String, String)],
        params: &[(String, String)],
    ) -> Result<Response, FetchError> {
        let env = SystemHost;
        let (host, addrs) = pin(url, &env).map_err(FetchError::Invalid)?;
        let client = self.client_for(&host, &addrs)?;
        let mut request = client.request(
            reqwest::Method::from_bytes(method.as_bytes()).unwrap_or(reqwest::Method::GET),
            url,
        );
        for (k, v) in headers {
            request = request.header(k.as_str(), v.as_str());
        }
        if !params.is_empty() {
            request = request.query(params);
        }
        let started = Instant::now();
        let response = request.send().map_err(|e| FetchError::transport(&e))?;
        let elapsed = started.elapsed();
        let status = response.status().as_u16();
        let final_url = response.url().to_string();
        let http_version = match response.version() {
            reqwest::Version::HTTP_11 => "HTTP/1.1",
            reqwest::Version::HTTP_2 => "HTTP/2",
            reqwest::Version::HTTP_3 => "HTTP/3",
            _ => "unmeasurable",
        }
        .to_string();
        let mut resp_headers: Vec<(String, String)> = Vec::new();
        for (name, value) in response.headers() {
            let text = value.to_str().unwrap_or_default().to_string();
            resp_headers.push((name.as_str().to_ascii_lowercase(), text));
        }
        let cookies = parse_cookies(&resp_headers);
        let mut content = Vec::new();
        let mut body = response;
        body.read_to_end(&mut content)
            .map_err(|e| FetchError::Transport(e.to_string()))?;
        Ok(Response {
            status_code: status,
            url: final_url,
            headers: resp_headers,
            content,
            cookies,
            http_version,
            elapsed,
        })
    }

    fn send_following(
        &self,
        method: &str,
        url: &str,
        headers: &[(String, String)],
        params: &[(String, String)],
    ) -> Result<Response, FetchError> {
        let mut current = url.to_string();
        let mut previous_status: Option<u16> = None;
        for _ in 0..=MAX_REDIRECTS {
            let resp = match self.send_once(method, &current, headers, params) {
                Ok(resp) => resp,
                // A refusal past the first hop is about the redirect target,
                // not the original request: keep the referring status.
                Err(FetchError::Invalid(message)) => {
                    return Err(match previous_status {
                        Some(status_code) => FetchError::Blocked(BlockedRedirectError {
                            message,
                            status_code,
                            location: current.clone(),
                        }),
                        None => FetchError::Invalid(message),
                    });
                }
                Err(other) => return Err(other),
            };
            if !(matches!(resp.status_code, 301 | 302 | 303 | 307 | 308)
                && resp.header("location").is_some())
            {
                return Ok(resp);
            }
            let location = resp.header("location").unwrap_or_default();
            let next = Url::parse(&resp.url)
                .ok()
                .and_then(|base| base.join(location).ok())
                .map(|u| u.to_string())
                .unwrap_or_else(|| location.to_string());
            previous_status = Some(resp.status_code);
            current = next;
        }
        Err(FetchError::Transport(format!(
            "too many redirects (>{MAX_REDIRECTS})"
        )))
    }
}

/// Build the guarded client; the bool reports HTTP/2 capability to callers
pub fn http_client(timeout: f64) -> Result<(ReqwestReconClient, bool), FetchError> {
    Ok((
        ReqwestReconClient {
            timeout: Duration::from_secs_f64(timeout.max(0.1)),
            http2: true,
            clients: Mutex::new(HashMap::new()),
        },
        true,
    ))
}

// ── DoH / RDAP / WHOIS ─────────────────────────────────────────────────────

/// Query DNS over HTTPS; failures return an empty list.
pub fn doh(client: &dyn ReconClient, name: &str, rtype: &str) -> Vec<String> {
    if name.is_empty() {
        return Vec::new();
    }
    for endpoint in DOH_ENDPOINTS {
        let headers = [("Accept".to_string(), "application/dns-json".to_string())];
        let params = [
            ("name".to_string(), name.to_string()),
            ("type".to_string(), rtype.to_string()),
        ];
        let Ok(response) = client.get(endpoint, &headers, &params) else {
            continue;
        };
        if response.status_code != 200 {
            continue;
        }
        let answers = response
            .json()
            .ok()
            .and_then(|j| j.get("Answer").cloned())
            .and_then(|a| a.as_array().cloned())
            .unwrap_or_default();
        let records: Vec<String> = answers
            .iter()
            .filter_map(|item| item.get("data").and_then(|d| d.as_str()))
            .map(|d| d.trim().trim_matches('"').trim_end_matches('.').to_string())
            .filter(|d| !d.is_empty())
            .collect();
        if !records.is_empty() {
            return records;
        }
    }
    Vec::new()
}

/// Query RDAP; the result reports whether the registry is supported.
pub fn rdap(client: &dyn ReconClient, path: &str) -> Json {
    let headers = [("Accept".to_string(), "application/rdap+json".to_string())];
    let url = format!("{RDAP_BOOTSTRAP}/{path}");
    let response = match client.get(&url, &headers, &[]) {
        Ok(r) => r,
        Err(error) => return json!({"supported": false, "error": error.to_string()}),
    };
    if response.status_code == 404 {
        return json!({"supported": false, "error": "not found in RDAP"});
    }
    if response.status_code >= 400 {
        return json!({"supported": false, "error": format!("RDAP HTTP {}", response.status_code)});
    }
    match response.json() {
        Ok(data) => json!({"supported": true, "data": data}),
        Err(_) => json!({"supported": false, "error": "RDAP returned non-JSON"}),
    }
}

/// Run the system `whois` binary; `server` may come from an untrusted registry
/// response and is passed only when it is a syntactically valid hostname
/// `which` and `run` are injectable for tests.
pub fn whois_text_with(
    domain: &str,
    timeout: f64,
    server: Option<&str>,
    which: &dyn Fn(&str) -> Option<String>,
    run: &dyn Fn(&[String], f64) -> Option<String>,
) -> Option<String> {
    let binary = which("whois")?;
    if domain.is_empty() {
        return None;
    }
    let mut argv = vec![binary];
    if let Some(server) = server {
        if !is_valid_whois_server(&server.to_lowercase()) {
            return None;
        }
        argv.push("-h".to_string());
        argv.push(server.to_string());
    }
    argv.push(domain.to_string());
    run(&argv, timeout)
}

/// [`whois_text_with`] backed by the real `whois` binary, invoked via argv
/// (never through a shell).
pub fn whois_text(domain: &str, timeout: f64, server: Option<&str>) -> Option<String> {
    let which = |name: &str| -> Option<String> {
        std::env::var_os("PATH").and_then(|paths| {
            std::env::split_paths(&paths).find_map(|dir| {
                let candidate = dir.join(name);
                candidate
                    .is_file()
                    .then(|| candidate.to_string_lossy().into_owned())
            })
        })
    };
    let run = |argv: &[String], timeout: f64| -> Option<String> {
        let child = Command::new(&argv[0])
            .args(&argv[1..])
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .spawn()
            .ok()?;
        let (tx, rx) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            let _ = tx.send(child.wait_with_output());
        });
        let output = rx
            .recv_timeout(Duration::from_secs_f64(timeout.max(0.1)))
            .ok()?
            .ok()?;
        let text = String::from_utf8_lossy(&output.stdout).into_owned();
        (!text.is_empty()).then_some(text)
    };
    whois_text_with(domain, timeout, server, &which, &run)
}

/// Query whois, following one registry referral; returns `(text, server)`
pub fn whois_lookup(domain: &str, timeout: f64) -> (Option<String>, Option<String>) {
    whois_lookup_with(domain, timeout, whois_text)
}

/// Injectable seam used by tests.
pub fn whois_lookup_with(
    domain: &str,
    timeout: f64,
    whois_text: impl Fn(&str, f64, Option<&str>) -> Option<String>,
) -> (Option<String>, Option<String>) {
    if domain.is_empty() {
        return (None, None);
    }
    let tld = domain.rsplit('.').next().unwrap_or("").to_lowercase();
    let mapped = whois_server_for_tld(&tld).map(str::to_string);
    if let Some(mapped) = &mapped {
        if let Some(text) = whois_text(domain, timeout, Some(mapped)) {
            return (Some(text), Some(mapped.clone()));
        }
    }
    let Some(text) = whois_text(domain, timeout, None) else {
        return (None, None);
    };
    let referral = whois_field(&text, WHOIS_REFERRAL_KEYS)
        .map(|r| {
            r.split("//")
                .last()
                .unwrap_or("")
                .split('/')
                .next()
                .unwrap_or("")
                .trim()
                .to_lowercase()
        })
        .unwrap_or_default();
    if !referral.is_empty() && Some(&referral) != mapped.as_ref() {
        if let Some(deeper) = whois_text(domain, timeout, Some(&referral)) {
            return (Some(deeper), Some(referral));
        }
    }
    (Some(text), None)
}
