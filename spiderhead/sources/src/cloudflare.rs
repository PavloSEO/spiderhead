//! Cloudflare edge analytics as an aggregated stand-in for server access logs.
//!
//! The GraphQL Analytics API exposes `httpRequestsAdaptiveGroups`: request counts
//! grouped by path, status, user agent, verified-bot category and cache status —
//! enough for "which bots hit which URLs with which codes, per day", but **not a raw
//! log** (no IPs, no sub-day timestamps). The token comes from `$CLOUDFLARE_API_TOKEN`
//! or the file named by `$SEOHEAD_CLOUDFLARE_TOKEN_FILE`; it is never printed or put
//! into an error message.

use spiderhead_core::filesystem::{file_identity, link_identity, NoFollow, StatCompat};
use std::path::PathBuf;

use serde_json::{json, Value};
use thiserror::Error;

use spiderhead_core::host::Host;

use crate::credentials::is_private_mode;
use crate::http::{self, Request, Sleeper, Transport};
use crate::{pyjson, pystr};

mod errors;
pub use errors::*;
mod zones_payloads;
pub use zones_payloads::*;

/// `API` — the REST base; GraphQL goes through [`GRAPHQL`].
pub const API: &str = "https://api.cloudflare.com/client/v4";
/// `GRAPHQL` — the Analytics API endpoint.
pub const GRAPHQL: &str = "https://api.cloudflare.com/client/v4/graphql";
/// `TIMEOUT` — whole-request seconds.
pub const TIMEOUT: f64 = 60.0;
/// `MAX_RESPONSE_BYTES` — the 16 MiB response budget.
pub const MAX_RESPONSE_BYTES: usize = 16 * 1024 * 1024;
/// `PAGE_SIZE` — the dataset's maxPageSize on the Free plan.
pub const PAGE_SIZE: i64 = 10000;
/// `MAX_DAYS` — the dataset's maxDuration on the Free plan (32 days).
pub const MAX_DAYS: i64 = 32;
/// `RETRIES` — transport retry budget after the first attempt.
pub const RETRIES: usize = 3;
/// `MAX_TRACKED_PATHS` — per-family path counter cap.
pub const MAX_TRACKED_PATHS: usize = 5000;
/// `MAX_URL_FACTS` — retained url-fact tuple cap.
pub const MAX_URL_FACTS: usize = 50000;
/// `MAX_TOKEN_BYTES` — token file byte ceiling.
pub const MAX_TOKEN_BYTES: u64 = 8192;
/// `MAX_RULES` — ruleset rule cap per phase.
pub const MAX_RULES: usize = 2000;
/// `RULE_PHASES` — the phases `configuration` reads.
pub const RULE_PHASES: [&str; 3] = [
    "http_request_redirect",
    "http_request_dynamic_redirect",
    "http_request_firewall_custom",
];
/// `AI_SETTINGS` — the zone settings `configuration` reads.
pub const AI_SETTINGS: [&str; 2] = ["search_for_agents", "redirects_for_ai_training"];
/// `MAX_CONFIG_ITEMS` — logpush job listing page cap.
pub const MAX_CONFIG_ITEMS: i64 = 100;
/// `SOURCE` — labels the aggregated evidence.
pub const SOURCE: &str = "cloudflare-aggregated";
/// `NOTE` — the caveat that every findings list starts with.
pub const NOTE: &str = "Aggregated Cloudflare edge analytics, not raw server logs: grouped counts per day, no client IPs, so bot authenticity (reverse DNS) cannot be verified; adaptive sampling applies on busy zones.";

/// `QUERY` — the GraphQL traffic query verbatim.
pub const QUERY: &str = "
query($account: String!, $zone: String!, $from: Time!, $to: Time!, $limit: Int!) {
  viewer { accounts(filter: {accountTag: $account}) {
    httpRequestsAdaptiveGroups(limit: $limit, orderBy: [count_DESC],
        filter: {zoneTag: $zone, datetime_geq: $from, datetime_lt: $to}) {
      count
      avg { sampleInterval }
      dimensions {
        date userAgent verifiedBotCategory clientRequestHTTPHost clientRequestPath
        edgeResponseStatus originResponseStatus cacheStatus
      }
    }
  } }
}
";

/// A failed call; the message never contains the token.
#[derive(Debug, Error)]
pub enum CfError {
    /// Expected failure with a readable message.
    #[error("{0}")]
    Cloudflare(String),
    /// Invalid argument.
    #[error("{0}")]
    Value(String),
    /// Filesystem or transport error.
    #[error("{0}")]
    Os(String),
    /// Any other failure kind; its class name is kept with the error.
    #[error("{1}")]
    Python(&'static str, String),
}

impl CfError {
    /// Error class name this variant reports.
    pub fn python_class(&self) -> &'static str {
        match self {
            CfError::Cloudflare(_) => "CloudflareError",
            CfError::Value(_) => "ValueError",
            CfError::Os(_) => "OSError",
            CfError::Python(class, _) => class,
        }
    }
}

fn cf<T>(message: impl Into<String>) -> Result<T, CfError> {
    Err(CfError::Cloudflare(message.into()))
}

/// Response headers with lower-cased names, as recorded in the receipt.
pub fn lower_headers(headers: &[(String, String)]) -> Vec<(String, String)> {
    headers
        .iter()
        .map(|(k, v)| (k.to_lowercase(), v.clone()))
        .collect()
}

/// The Cloudflare `Transport` callable shape: `(method, url, body_or_none, token)`
/// → the parsed JSON object.
pub type CfTransport<'a> =
    dyn Fn(&str, &str, Option<&Value>, &str) -> Result<Value, CfError> + Send + Sync + 'a;

/// Raw transport result: bytes, or text (encoded as UTF-8 before parsing).
pub enum RawReply {
    /// `bytes` payload.
    Bytes(Vec<u8>),
    /// `str` payload — encoded to UTF-8 before parsing.
    Text(String),
    /// Any other type → `Cloudflare Logpull returned an invalid response`.
    Other,
}

/// The `raw_transport` callable for Logpull: `(url, token)` → the raw payload.
pub type RawTransport<'a> = dyn Fn(&str, &str) -> Result<RawReply, CfError> + Send + Sync + 'a;

/// `read_token()` — `$CLOUDFLARE_API_TOKEN` first, then the private token file.
pub fn read_token(host: &dyn Host, home: &dyn Fn(&str) -> PathBuf) -> Result<String, CfError> {
    let env = host
        .var("CLOUDFLARE_API_TOKEN")
        .map(|v| v.trim().to_owned())
        .unwrap_or_default();
    if !env.is_empty() {
        if env.len() > MAX_TOKEN_BYTES as usize || env.chars().any(|c| (c as u32) < 32) {
            return cf("Cloudflare API token environment value is invalid");
        }
        return Ok(env);
    }
    let path = host
        .var("SEOHEAD_CLOUDFLARE_TOKEN_FILE")
        .filter(|v| !v.is_empty())
        .map(PathBuf::from)
        .unwrap_or_else(|| home("~/.config/cloudflare/api-token"));
    let meta = match path.symlink_metadata() {
        Ok(m) => m,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            return cf(format!(
                "Cloudflare token not found: set $CLOUDFLARE_API_TOKEN or store it in {} \
                 (or name the file in $SEOHEAD_CLOUDFLARE_TOKEN_FILE)",
                path.display()
            ))
        }
        Err(_) => return cf("Cloudflare token file is unreadable or invalid"),
    };
    if meta.file_type().is_symlink() || !meta.is_file() {
        return cf("Cloudflare token file must be a regular file, not a link");
    }
    let file = std::fs::OpenOptions::new()
        .read(true)
        .no_follow()
        .open(&path)
        .map_err(|_| {
            CfError::Cloudflare("Cloudflare token file is unreadable or invalid".into())
        })?;
    let opened = file.metadata().map_err(|_| {
        CfError::Cloudflare("Cloudflare token file is unreadable or invalid".into())
    })?;
    let opened_id = file_identity(&file).map_err(|_| {
        CfError::Cloudflare("Cloudflare token file is unreadable or invalid".into())
    })?;
    let meta_id = link_identity(&path).map_err(|_| {
        CfError::Cloudflare("Cloudflare token file is unreadable or invalid".into())
    })?;
    let post_open = path.symlink_metadata().map_err(|_| {
        CfError::Cloudflare("Cloudflare token file is unreadable or invalid".into())
    })?;
    let post_open_id = link_identity(&path).map_err(|_| {
        CfError::Cloudflare("Cloudflare token file is unreadable or invalid".into())
    })?;
    if !opened.is_file()
        || opened_id.2 != 1
        || opened.len() > MAX_TOKEN_BYTES
        || !is_private_mode(opened.mode())
        || post_open.file_type().is_symlink()
        || (meta_id.1 != 0 && opened_id.1 != meta_id.1)
        || (meta_id.0 != 0 && opened_id.0 != meta_id.0)
        || (opened_id.1 != 0 && opened_id.1 != post_open_id.1)
        || (opened_id.0 != 0 && opened_id.0 != post_open_id.0)
    {
        return cf("Cloudflare token file must be a private bounded regular file");
    }
    use std::io::Read;
    let mut raw = Vec::new();
    file.take(MAX_TOKEN_BYTES + 1)
        .read_to_end(&mut raw)
        .map_err(|_| {
            CfError::Cloudflare("Cloudflare token file is unreadable or invalid".into())
        })?;
    if raw.len() as u64 > MAX_TOKEN_BYTES {
        return cf("Cloudflare token file exceeds the byte limit");
    }
    let value = String::from_utf8(raw)
        .map_err(|_| CfError::Cloudflare("Cloudflare token file is unreadable or invalid".into()))?
        .trim()
        .to_owned();
    if value.len() > MAX_TOKEN_BYTES as usize || value.chars().any(|c| (c as u32) < 32) {
        return cf("Cloudflare token file is invalid");
    }
    if value.is_empty() {
        return cf(format!(
            "Cloudflare token not found: set $CLOUDFLARE_API_TOKEN or store it in {} \
             (or name the file in $SEOHEAD_CLOUDFLARE_TOKEN_FILE)",
            path.display()
        ));
    }
    Ok(value)
}

/// Lifecycle check of the token only.
pub async fn verify_token<T: Transport>(
    host: &dyn Host,
    home: &dyn Fn(&str) -> PathBuf,
    transport: &T,
    sleeper: &Sleeper,
    token: Option<&Value>,
    send: Option<&CfTransport<'_>>,
) -> Value {
    let secret = match token
        .filter(|v| pystr::truthy(Some(v)))
        .and_then(|v| v.as_str().map(str::to_owned))
    {
        Some(t) => t,
        None => match read_token(host, home) {
            Ok(t) => t,
            Err(_) => {
                return json!({
                    "ok": false,
                    "token_status": null,
                    "error": "Cloudflare API token is not configured",
                })
            }
        },
    };
    let send = send.map(|s| s as &CfTransport);
    let live = LiveSend { transport, sleeper };
    let payload = match send {
        Some(s) => s("GET", &format!("{API}/user/tokens/verify"), None, &secret),
        None => {
            live.send("GET", &format!("{API}/user/tokens/verify"), None, &secret)
                .await
        }
    };
    let payload = match payload {
        Ok(p) => p,
        Err(_) => {
            return json!({
                "ok": false,
                "token_status": null,
                "error": "Cloudflare token verification request failed",
            })
        }
    };
    if !pystr::truthy(payload.get("success"))
        || !payload.get("result").is_some_and(Value::is_object)
    {
        return json!({
            "ok": false,
            "token_status": null,
            "error": "Cloudflare token verification returned an invalid response",
        });
    }
    let result = payload.get("result").cloned().unwrap_or_default();
    let state = result.get("status").cloned().unwrap_or(Value::Null);
    if !matches!(state.as_str(), Some("active" | "disabled" | "expired")) {
        return json!({
            "ok": false,
            "token_status": null,
            "error": "Cloudflare token verification returned an unknown status",
        });
    }
    json!({"ok": state == "active", "token_status": state})
}

/// Live JSON transport with the documented retry loop.
pub struct LiveSend<'a, T: Transport> {
    /// HTTP boundary; the live implementation does not follow redirects.
    pub transport: &'a T,
    /// Retry pause sink.
    pub sleeper: &'a Sleeper,
}

impl<'a, T: Transport> LiveSend<'a, T> {
    /// Same contract as the live transport: (method, url, body, token) to parsed JSON.
    pub async fn send(
        &self,
        method: &str,
        url: &str,
        body: Option<&Value>,
        token: &str,
    ) -> Result<Value, CfError> {
        let data = body.map(|b| pyjson::dumps(b).into_bytes());
        for slot in http::retry_slots(RETRIES + 1) {
            let mut request = Request::new(method, url)
                .timeout(TIMEOUT)
                .header("Authorization", format!("Bearer {token}"))
                .header("Content-Type", "application/json");
            if let Some(d) = &data {
                request = request.body(d.clone());
            }
            match self.transport.send(&request).await {
                Ok(response) => {
                    let status = response.status;
                    if (200..300).contains(&status) {
                        if response.body.len() > MAX_RESPONSE_BYTES {
                            return cf("Cloudflare response exceeded the size budget");
                        }
                        return serde_json::from_slice::<Value>(&response.body).map_err(|_| {
                            CfError::Cloudflare("Cloudflare returned a non-JSON response".into())
                        });
                    }
                    let retryable = status == 429 || status >= 500;
                    if retryable && slot.has_next {
                        self.sleeper
                            .pause(http::retry_after_pause(&response.headers, slot.index))
                            .await;
                        continue;
                    }
                    return cf(format!("Cloudflare API returned HTTP {status}"));
                }
                Err(err) => {
                    if slot.has_next {
                        self.sleeper.pause(2f64.powi(slot.index as i32)).await;
                        continue;
                    }
                    return cf(format!("Cloudflare API request failed: {err}"));
                }
            }
        }
        cf("Cloudflare API retries exhausted")
    }
}

/// Read one zone setting through the synchronous transport.
pub fn zone_setting(zone_id: &str, setting: &str, token: &str, send: &CfTransport) -> Value {
    let url = format!(
        "{API}/zones/{}/settings/{setting}",
        http::quote(zone_id, "")
    );
    zone_setting_payload(send("GET", &url, None, token).as_ref(), setting)
}

/// Read one ruleset phase entrypoint through the synchronous transport.
pub fn ruleset(phase: &str, zone_id: &str, token: &str, send: &CfTransport) -> Value {
    let url = format!(
        "{API}/zones/{}/rulesets/phases/{}/entrypoint",
        http::quote(zone_id, ""),
        http::quote(phase, "")
    );
    ruleset_payload(send("GET", &url, None, token).as_ref())
}

/// List configured Logpush jobs through the synchronous transport.
pub fn logpush_jobs(zone_id: &str, token: &str, send: &CfTransport) -> Value {
    let url = format!(
        "{API}/zones/{}/logpush/jobs?per_page={MAX_CONFIG_ITEMS}&page=1",
        http::quote(zone_id, "")
    );
    logpush_payload(send("GET", &url, None, token).as_ref())
}

/// Bounded reads of zone configuration (settings, rulesets, Logpush jobs).
pub async fn configuration<T: Transport>(
    host: &dyn Host,
    home: &dyn Fn(&str) -> PathBuf,
    transport: &T,
    sleeper: &Sleeper,
    zone_id: &str,
    token: Option<&Value>,
    send: Option<&CfTransport<'_>>,
) -> Result<Value, CfError> {
    let secret = match token
        .filter(|v| pystr::truthy(Some(v)))
        .and_then(|v| v.as_str().map(str::to_owned))
    {
        Some(t) => t,
        None => read_token(host, home)?,
    };
    let live = LiveSend { transport, sleeper };
    let call = async |method: &str, url: &str, body: Option<&Value>| match send {
        Some(s) => s(method, url, body, &secret),
        None => live.send(method, url, body, &secret).await,
    };
    let mut settings = serde_json::Map::new();
    for name in AI_SETTINGS {
        let url = format!("{API}/zones/{}/settings/{name}", http::quote(zone_id, ""));
        let payload = call("GET", &url, None).await;
        settings.insert(
            name.to_owned(),
            zone_setting_payload(payload.as_ref(), name),
        );
    }
    let mut rulesets = serde_json::Map::new();
    for phase in RULE_PHASES {
        let url = format!(
            "{API}/zones/{}/rulesets/phases/{}/entrypoint",
            http::quote(zone_id, ""),
            http::quote(phase, "")
        );
        let payload = call("GET", &url, None).await;
        rulesets.insert(phase.to_owned(), ruleset_payload(payload.as_ref()));
    }
    let url = format!(
        "{API}/zones/{}/logpush/jobs?per_page={MAX_CONFIG_ITEMS}&page=1",
        http::quote(zone_id, "")
    );
    let payload = call("GET", &url, None).await;
    let logpush = logpush_payload(payload.as_ref());
    let mut states = Vec::new();
    for item in settings.values().chain(rulesets.values()).chain([&logpush]) {
        states.push(
            item.get("state")
                .and_then(Value::as_str)
                .unwrap_or_default(),
        );
    }
    Ok(json!({
        "state": if states.iter().all(|s| *s == "measured" || *s == "complete") { "complete" } else { "partial" },
        "source": "Cloudflare zone settings and Rulesets API; read-only",
        "zone_settings": settings,
        "rulesets": rulesets,
        "logpush": logpush,
        "ai_crawler_policy_note": "AI Crawl Control allow/block actions are represented by Cloudflare custom WAF rules. These are configured rules, not proof that a rule matched a URL or request.",
    }))
}
