//! DataForSEO client for Google volume, keyword ideas, difficulty, and SERPs.
//!
//! Provider boundaries are strict: DataForSEO serves Google and international markets.
//! Russian or Belarusian requests never reach the network — [`geo_guard`] refuses them up
//! front because the provider cannot serve those locations while still billing the call.
//! Sandbox is the default environment; production requires `env="prod"` (or
//! `DATAFORSEO_ENV=prod`) and the shared spend gate's `confirm_paid` confirmation.

mod ops;

pub use ops::{
    backlink_call, backlinks, backlinks_history, backlinks_summary, domain_pages_summary, estimate,
    keyword_difficulty, keyword_ideas, referring_domains, run, search_volume, serp, BacklinkArgs,
    Called, EstimateArgs, GeoArgs, IdeasArgs, RunOutcome, SerpArgs,
};

use serde_json::{json, Map, Value};
use spiderhead_core::host::Host;
use std::path::Path;

use crate::credentials::{self, CredentialError};
use crate::http::{Request, Sleeper, Transport, TransportError};
use crate::metrika::SpendSink;
use crate::spend::{self, Record};
use crate::{pyjson, pystr};

mod errors;
pub use errors::*;
mod results;
pub use results::*;

/// Production API base URL.
pub const PROD_BASE: &str = "https://api.dataforseo.com/";
/// Sandbox API base URL, used for test calls.
pub const SANDBOX_BASE: &str = "https://sandbox.dataforseo.com/";
/// `SOURCE` — the spend-ledger source name.
pub const SOURCE: &str = "dataforseo";
/// `TIMEOUT` — whole-request timeout seconds.
pub const TIMEOUT: f64 = 120.0;
/// `RETRIES` — HTTP-error retries on 429/5xx (the provider replied, so retrying is safe).
pub const RETRIES: i64 = 3;
/// `MAX_ACCOUNT_METADATA_BYTES` — `appendix/user_data` answers are capped at 2 MiB.
pub const MAX_ACCOUNT_METADATA_BYTES: usize = 2 * 1024 * 1024;

/// `UNSUPPORTED` — the reason text behind each refused ISO code.
fn unsupported_reason(iso: &str) -> Option<&'static str> {
    match iso {
        "RU" => Some("Russia is unavailable in Labs, Google Ads, and SERP"),
        "BY" => Some("Belarus locations are unavailable across DataForSEO services"),
        _ => None,
    }
}

/// `COUNTRY_ALIASES` lookup on the stripped, lowercased country string.
fn country_alias(lowered: &str) -> Option<&'static str> {
    match lowered {
        "россия" | "russia" | "russian federation" | "рф" => Some("RU"),
        "беларусь" | "белоруссия" | "belarus" | "рб" => Some("BY"),
        _ => None,
    }
}

/// `UNSUPPORTED_LOCATION_CODES` — geo-target IDs for the two excluded markets.
fn unsupported_location(code: i64) -> Option<&'static str> {
    match code {
        2643 => Some("RU"),
        2112 => Some("BY"),
        _ => None,
    }
}

fn fallback_tools() -> Value {
    json!([
        "Yandex Wordstat via Yandex Cloud for volume and expansion (keywords-expand)",
        "Arsenkin for exact !W volume and SERP clustering (keywords-exact)",
        "Yandex SERP via serp-fetch",
    ])
}

/// The Google and backlink endpoints used by this client.
pub fn endpoints(kind: &str) -> Option<&'static str> {
    match kind {
        "search_volume" => Some("v3/keywords_data/google_ads/search_volume/live"),
        "keyword_ideas" => Some("v3/dataforseo_labs/google/keyword_ideas/live"),
        "keyword_difficulty" => Some("v3/dataforseo_labs/google/bulk_keyword_difficulty/live"),
        "serp" => Some("v3/serp/google/organic/live/advanced"),
        "backlinks_summary" => Some("v3/backlinks/summary/live"),
        "backlinks" => Some("v3/backlinks/backlinks/live"),
        "referring_domains" => Some("v3/backlinks/referring_domains/live"),
        "backlinks_history" => Some("v3/backlinks/history/live"),
        "domain_pages_summary" => Some("v3/backlinks/domain_pages_summary/live"),
        _ => None,
    }
}

/// The seams every adapter call runs against.
pub struct DfsEnv<'a, T: Transport> {
    /// Env-var/filesystem host.
    pub host: &'a dyn Host,
    /// Credential config root for the live readers.
    pub root: &'a Path,
    /// HTTP boundary for `post`/`balance`.
    pub transport: &'a T,
    /// Sleep hook for the 429 and 5xx backoff in `post`.
    pub sleeper: &'a Sleeper,
    /// Spend-ledger sink; None writes to the real ledger.
    pub spend: Option<&'a SpendSink<'a>>,
    /// Login source; None reads the real credential store.
    pub login: Option<&'a dyn Fn() -> Result<String, CredentialError>>,
    /// Password source; None reads the real credential store.
    pub password: Option<&'a dyn Fn() -> Result<String, CredentialError>>,
    /// Today's date as days since the epoch for `backlinks_history` bounds; None uses the local date.
    pub today: Option<&'a dyn Fn() -> i64>,
}

impl<'a, T: Transport> DfsEnv<'a, T> {
    /// The production environment.
    pub fn live(host: &'a dyn Host, root: &'a Path, transport: &'a T) -> Self {
        DfsEnv {
            host,
            root,
            transport,
            sleeper: &Sleeper::Real,
            spend: None,
            login: None,
            password: None,
            today: None,
        }
    }

    /// The real spend ledger or the injected sink; I/O errors propagate.
    pub(crate) fn record(&self, rec: &Record) -> Result<Value, DfsError> {
        match self.spend {
            Some(sink) => sink(rec),
            None => spend::record(self.host, rec),
        }
        .map_err(|err| pyerr("OSError", err.to_string()))
    }

    pub(crate) fn login(&self) -> Result<String, DfsError> {
        match self.login {
            Some(probe) => probe(),
            None => credentials::dataforseo_login(self.host, self.root),
        }
        .map_err(cred_err)
    }

    pub(crate) fn password(&self) -> Result<String, DfsError> {
        match self.password {
            Some(probe) => probe(),
            None => credentials::dataforseo_password(self.host, self.root),
        }
        .map_err(cred_err)
    }

    /// Today's date as days since the epoch.
    pub(crate) fn today(&self) -> i64 {
        match self.today {
            Some(today) => today(),
            None => {
                crate::pytime::date_fromisoformat(&spend::local_stamp()[..10]).unwrap_or_default()
            }
        }
    }
}

/// Environment name from the argument, then `DATAFORSEO_ENV`, then `sandbox`; it must be a known
/// name. A non-text value is an error.
pub(crate) fn resolve_env(env: Option<&Value>, host: &dyn Host) -> Result<String, DfsError> {
    let raw = match env.filter(|v| pystr::truthy(Some(v))) {
        Some(v) => match v.as_str() {
            Some(text) => text.to_owned(),
            None => return Err(attr_err(v, "lower")),
        },
        None => match host.var("DATAFORSEO_ENV").filter(|v| !v.is_empty()) {
            Some(var) => var,
            None => "sandbox".to_owned(),
        },
    };
    let lowered = raw.to_lowercase();
    if lowered != "sandbox" && lowered != "prod" {
        return Err(value_err("env must be sandbox or prod"));
    }
    Ok(lowered)
}

fn rejection(iso: &str) -> Value {
    let reason = unsupported_reason(iso).unwrap_or_default();
    json!({
        "ok": false,
        "unsupported_geo": iso,
        "error": format!("DataForSEO does not cover {iso}: {reason}"),
        "use_instead": fallback_tools(),
    })
}

/// `geo_guard` — return a rejection for unsupported geographies, `None` when allowed.
///
/// `location_code` is checked first because it is the field actually billed; `country` is
/// advisory text.
pub fn geo_guard(
    country: Option<&Value>,
    location_code: Option<&Value>,
) -> Result<Option<Value>, DfsError> {
    if let Some(code_value) = location_code.filter(|v| !v.is_null()) {
        if let Ok(code) = pystr::to_python_int(code_value) {
            if let Some(iso) = unsupported_location(code) {
                return Ok(Some(rejection(iso)));
            }
        }
    }
    if !pystr::truthy(country) {
        return Ok(None);
    }
    let country = country.unwrap_or(&Value::Null);
    let Some(text) = country.as_str() else {
        return Err(attr_err(country, "strip"));
    };
    let stripped = pystr::strip(text);
    let iso = country_alias(&stripped.to_lowercase())
        .unwrap_or(&stripped.to_uppercase())
        .to_owned();
    if unsupported_reason(&iso).is_none() {
        return Ok(None);
    }
    Ok(Some(rejection(&iso)))
}

/// Constructor options.
#[derive(Debug, Default)]
pub struct ClientArgs<'a> {
    /// Login; an empty value falls back to the stored credential.
    pub login: Option<&'a Value>,
    /// Password; an empty value falls back to the stored credential.
    pub password: Option<&'a Value>,
    /// Environment, `sandbox` or `prod`; an empty value resolves `DATAFORSEO_ENV`, then sandbox.
    pub env_name: Option<&'a Value>,
}

/// Client for the DataForSEO v3 API.
pub struct Client<'e, T: Transport> {
    /// The seams (spend, credentials, transport) the client runs against.
    pub(crate) env: &'e DfsEnv<'e, T>,
    env_name: String,
    base: String,
    login: Option<Value>,
    password: Option<Value>,
}

impl<'e, T: Transport> Client<'e, T> {
    /// Builds a client for the resolved environment: prod uses the production base, anything else the sandbox.
    pub fn new(env: &'e DfsEnv<'e, T>, args: &ClientArgs<'_>) -> Result<Self, DfsError> {
        let env_name = resolve_env(args.env_name, env.host)?;
        let base = if env_name == "prod" {
            PROD_BASE
        } else {
            SANDBOX_BASE
        };
        Ok(Client {
            env,
            env_name,
            base: base.to_owned(),
            login: args.login.cloned(),
            password: args.password.cloned(),
        })
    }

    /// Resolved environment label.
    pub fn env_name(&self) -> &str {
        &self.env_name
    }

    /// Resolved API base URL.
    pub fn base(&self) -> &str {
        &self.base
    }

    /// Credentials for basic auth; empty values fall back to the stored credentials.
    fn auth(&self) -> Result<String, DfsError> {
        let login = match self.login.as_ref().filter(|v| pystr::truthy(Some(v))) {
            Some(v) => pystr::repr_value(v),
            None => self.env.login()?,
        };
        let password = match self.password.as_ref().filter(|v| pystr::truthy(Some(v))) {
            Some(v) => pystr::repr_value(v),
            None => self.env.password()?,
        };
        Ok(format!(
            "Basic {}",
            base64_encode(format!("{login}:{password}").as_bytes())
        ))
    }

    /// POST a task list to one endpoint.
    ///
    /// HTTP 429/5xx means the provider replied, so `2**attempt` backoff retries apply. A
    /// transport failure loses the response — with no idempotency key the task may already
    /// be billed, so it is logged (charge unknown) and never retried.
    pub async fn post(
        &self,
        endpoint: &str,
        payload: &Value,
        operation: Option<&Value>,
    ) -> Result<Value, DfsError> {
        let url = format!("{}{}", self.base, endpoint.trim_start_matches('/'));
        let data = pyjson::dumps_opts(payload, false, false).into_bytes();
        let label = match operation.filter(|v| pystr::truthy(Some(v))) {
            // The label is passed through as-is; non-text labels only reach the spend record.
            Some(v) => pystr::repr_value(v),
            None => endpoint.to_owned(),
        };
        let mut attempt = 0i64;
        loop {
            attempt += 1;
            let request = Request::new("POST", &url)
                .header("Authorization", self.auth()?)
                .header("Content-Type", "application/json")
                .body(data.clone())
                .timeout(TIMEOUT);
            match self.env.transport.send(&request).await {
                Ok(response) => {
                    let raw = pystr::decode_utf8_strict(&response.body)
                        .map_err(|m| pyerr("UnicodeDecodeError", m))?;
                    return match serde_json::from_str::<Value>(&raw) {
                        Ok(value) => Ok(value),
                        Err(_) => {
                            // A receipt must exist even though the response cannot be parsed.
                            let mut extra = Map::new();
                            extra.insert("response_received".to_owned(), json!(true));
                            extra.insert("response_malformed".to_owned(), json!(true));
                            extra.insert("charge_status".to_owned(), json!("unknown"));
                            extra.insert("cost_unknown".to_owned(), json!(true));
                            extra.insert("status".to_owned(), json!(response.status));
                            self.env.record(&Record {
                                source: SOURCE.to_owned(),
                                operation: label.clone(),
                                cost: Some(0.0),
                                unit: "usd".to_owned(),
                                task_id: None,
                                items: None,
                                extra: Some(extra),
                                at: None,
                            })?;
                            Err(pyerr(
                                "JSONDecodeError",
                                pyjson::decode_error(&raw)
                                    .unwrap_or_else(|| "JSON decode error".to_owned()),
                            ))
                        }
                    };
                }
                Err(TransportError::Http(response)) => {
                    let text = String::from_utf8_lossy(&response.body).into_owned();
                    if matches!(response.status, 429 | 500 | 502 | 503) && attempt <= RETRIES {
                        self.env.sleeper.pause(2f64.powi(attempt as i32)).await;
                        continue;
                    }
                    return Err(DfsError::Provider {
                        status: i64::from(response.status),
                        message: message(&text),
                    });
                }
                Err(TransportError::Connect(kind)) => {
                    // Network and timeout errors: the response was lost and billing is unknown.
                    let detail = transport_text(&kind);
                    let mut extra = Map::new();
                    extra.insert("attempt_failed".to_owned(), json!("network_error"));
                    extra.insert("detail".to_owned(), json!(detail));
                    extra.insert("charge_status".to_owned(), json!("unknown"));
                    extra.insert("cost_unknown".to_owned(), json!(true));
                    self.env.record(&Record {
                        source: SOURCE.to_owned(),
                        operation: label.clone(),
                        cost: Some(0.0),
                        unit: "usd".to_owned(),
                        task_id: None,
                        items: None,
                        extra: Some(extra),
                        at: None,
                    })?;
                    return Err(DfsError::Provider {
                        status: 0,
                        message: format!(
                            "network error, response lost; task may already be billed: {detail}"
                        ),
                    });
                }
                Err(TransportError::InvalidRequest(m)) => return Err(value_err(&m)),
            }
        }
    }

    /// `client.balance()` — `GET v3/appendix/user_data`, envelope-validated.
    pub async fn balance(&self) -> Result<Value, DfsError> {
        let url = format!("{}v3/appendix/user_data", self.base);
        let request = Request::get(url)
            .header("Authorization", self.auth()?)
            .timeout(TIMEOUT);
        let response = match self.env.transport.send(&request).await {
            Ok(response) => response,
            Err(TransportError::Http(response)) => {
                return Err(DfsError::Provider {
                    status: i64::from(response.status),
                    message: "account metadata request was rejected".to_owned(),
                });
            }
            Err(TransportError::Connect(_)) => {
                return Err(DfsError::Provider {
                    status: 0,
                    message: "account metadata request failed".to_owned(),
                });
            }
            Err(TransportError::InvalidRequest(m)) => return Err(value_err(&m)),
        };
        let raw = response.read(MAX_ACCOUNT_METADATA_BYTES + 1);
        if raw.len() > MAX_ACCOUNT_METADATA_BYTES {
            return Err(DfsError::Provider {
                status: 0,
                message: "account metadata response exceeded 2 MiB".to_owned(),
            });
        }
        let body = pystr::decode_utf8_strict(raw)
            .ok()
            .and_then(|text| serde_json::from_str::<Value>(&text).ok())
            .ok_or_else(|| DfsError::Provider {
                status: 0,
                message: "account metadata response was invalid JSON".to_owned(),
            })?;
        let info = account_metadata_result(&body)?;
        let balance = match info.get("money") {
            Some(v) if pystr::truthy(Some(v)) => match v.as_object() {
                Some(m) => m.get("balance").cloned().unwrap_or(Value::Null),
                None => return Err(attr_err(v, "get")),
            },
            _ => Value::Null,
        };
        Ok(json!({
            "env": self.env_name,
            "balance": balance,
            "limits": info.get("rates").cloned().unwrap_or(Value::Null),
            "prices": info.get("price").cloned().unwrap_or(Value::Null),
        }))
    }
}
