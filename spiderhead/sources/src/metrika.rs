//! Yandex Metrica API client: management counters,
//! `stat/v1/data` reports with the complexity-split machinery, and the Logs API.
//!
//! Safeguards: retries honor `Retry-After` on 429, a
//! "Query is too complicated" refusal is retried in calendar-month slices under
//! one global raw-row cap, pagination carries a row ceiling, and `MetrikaError`
//! carries the API's status and message.

use std::fmt::Write as _;
use std::path::Path;

use serde_json::{json, Map, Value};
use spiderhead_core::host::Host;

use crate::credentials::metrika_token;
use crate::credentials::CredentialError;
use crate::http::{Request, Sleeper, Transport, TransportError};
use crate::spend::{self, Record};
use crate::{pyerr, pyjson, pystr, pytime};

mod split;
mod tz;
pub use split::rows_to_records;

mod errors;
pub use errors::*;
mod helpers;
pub(crate) use helpers::*;

/// Provider endpoints.
pub const API_BASE: &str = "https://api-metrika.yandex.net";
/// Management API prefix.
pub const API_MANAGEMENT: &str = "management/v1";
/// Reporting API path.
pub const API_REPORTS: &str = "stat/v1/data";
/// Provider label written to the spend log.
pub const SOURCE: &str = "metrika";

/// Per-request timeout in seconds.
pub const TIMEOUT: f64 = 30.0;
/// HTTP 429/5xx retry count (attempts beyond this re-raise).
pub const RETRIES: i64 = 3;
/// Delay between automatically paginated requests.
pub const PAGE_PAUSE: f64 = 0.15;
/// Row ceiling that prevents unbounded downloads.
pub const ROW_CAP: i64 = 100_000;
/// Backoff ceiling.
pub const MAX_BACKOFF: f64 = 30.0;

/// Attempts per slice before the accuracy is degraded.
pub const COMPLEXITY_ATTEMPTS: i64 = 3;
/// Seconds between complexity attempts; grows toward [`COMPLEXITY_PAUSE_MAX`].
pub const COMPLEXITY_PAUSE: f64 = 5.0;
/// Cap on the complexity retry pause.
pub const COMPLEXITY_PAUSE_MAX: f64 = 15.0;
/// Last-resort sample share for a slice that stays too complex.
pub const SAMPLED_ACCURACY: f64 = 0.1;

/// Metrics provably additive over disjoint calendar periods.
pub const ADDITIVE_METRICS: [&str; 2] = ["ym:s:visits", "ym:s:pageviews"];

const COMPLEXITY_MARKERS: [&str; 2] = ["too complicated", "слишком сложн"];

/// UTC offset in seconds for a zone name at a unix instant.
/// `None` when the zone name is unknown or invalid.
pub type ZoneinfoProbe<'a> = dyn Fn(&str) -> Option<i64> + 'a;

/// Records one spend-log entry; tests inject a replacement so no file is written.
pub type SpendSink<'a> = dyn Fn(&Record) -> std::io::Result<Value> + 'a;

/// The seams every client call runs against.
pub struct MetrikaEnv<'a, T: Transport> {
    /// Credential and spend-ledger host.
    pub host: &'a dyn Host,
    /// Config root holding provider credentials.
    pub root: &'a Path,
    /// HTTP boundary.
    pub transport: &'a T,
    /// Retry/pagination sleeps.
    pub sleeper: &'a Sleeper,
    /// Spend-log seam; `None` writes the real ledger.
    pub spend: Option<&'a SpendSink<'a>>,
    /// Current time as unix seconds (UTC).
    pub now: Option<i64>,
    /// Zone resolution; `None` probes the system tz database.
    pub zoneinfo: Option<&'a ZoneinfoProbe<'a>>,
    /// Raw-row cap; overridable in tests.
    pub row_cap: i64,
}

impl<'a, T: Transport> MetrikaEnv<'a, T> {
    /// The production environment: real transport, real sleeps, the real ledger.
    pub fn live(host: &'a dyn Host, root: &'a Path, transport: &'a T) -> Self {
        MetrikaEnv {
            host,
            root,
            transport,
            sleeper: &Sleeper::Real,
            spend: None,
            now: None,
            zoneinfo: None,
            row_cap: ROW_CAP,
        }
    }

    fn spend(&self, rec: &Record) -> Result<Value, MetrikaFail> {
        match self.spend {
            Some(sink) => sink(rec),
            None => spend::record(self.host, rec),
        }
        .map_err(|err| MetrikaFail::python("OSError", err.to_string()))
    }

    /// Current UTC instant in unix seconds.
    fn now(&self) -> i64 {
        self.now.unwrap_or_else(pytime::now_seconds)
    }

    /// Named zone resolved to its offset at `now`; `None` when the zone is not found.
    fn zone(&self, name: &str) -> Option<Tz> {
        let offset = match self.zoneinfo {
            Some(probe) => probe(name),
            None => tz_offset_at(name, self.now()),
        }?;
        Some(Tz::Named(name.to_owned(), offset))
    }
}

/// Metrika API client; the token is resolved at construction.
pub struct MetrikaClient<'e, T: Transport> {
    env: &'e MetrikaEnv<'e, T>,
    token: String,
}

impl<'e, T: Transport> MetrikaClient<'e, T> {
    /// A falsy token falls through to the stored credential.
    pub fn new(env: &'e MetrikaEnv<'e, T>, token: Option<&str>) -> Result<Self, MetrikaFail> {
        let token = match token {
            Some(token) if !token.is_empty() => token.to_owned(),
            _ => metrika_token(env.host, env.root).map_err(credential_fail)?,
        };
        Ok(MetrikaClient { env, token })
    }

    // --- transport ---------------------------------------------------------

    /// One request, with retries on HTTP 429/5xx and transport failures,
    /// honoring `Retry-After`; other failures return immediately.
    async fn request(&self, method: &str, url: &str, raw: bool) -> Result<Value, MetrikaFail> {
        let mut attempt: i64 = 0;
        loop {
            attempt += 1;
            let request = Request::new(method, url)
                .header("Authorization", format!("OAuth {}", self.token))
                .header("Accept", "application/json")
                .timeout(TIMEOUT);
            match self.env.transport.send(&request).await {
                Ok(response) => {
                    let body = String::from_utf8_lossy(&response.body).into_owned();
                    if raw {
                        return Ok(Value::String(body));
                    }
                    return match serde_json::from_str::<Value>(&body) {
                        Ok(value) => Ok(value),
                        Err(_) => Err(MetrikaFail::python(
                            "JSONDecodeError",
                            pyjson::decode_error(&body)
                                .unwrap_or_else(|| "JSON decode error".to_owned()),
                        )),
                    };
                }
                Err(TransportError::Http(response)) => {
                    let text = String::from_utf8_lossy(&response.body).into_owned();
                    let code = i64::from(response.status);
                    if matches!(code, 429 | 500 | 502 | 503 | 504) && attempt <= RETRIES {
                        self.env
                            .sleeper
                            .pause(backoff(attempt, response.header("Retry-After")))
                            .await;
                        continue;
                    }
                    return Err(MetrikaError {
                        status: code,
                        message: api_message(&text),
                    }
                    .into());
                }
                Err(TransportError::Connect(kind)) => {
                    if attempt <= RETRIES {
                        self.env.sleeper.pause(backoff(attempt, None)).await;
                        continue;
                    }
                    return Err(MetrikaError {
                        status: 0,
                        message: format!("network: {}", pyerr::url_error(&kind)),
                    }
                    .into());
                }
                // A malformed URL or method is an error of its own, not a transport failure, so it is not retried.
                Err(TransportError::InvalidRequest(kind)) => {
                    return Err(MetrikaFail::python("ValueError", kind));
                }
            }
        }
    }

    /// Build the request URL: values are stringified, `None` and empty values are dropped, then form-encoded.
    fn url(path: &str, params: Option<&Map<String, Value>>) -> String {
        let pairs: Vec<(String, String)> = params
            .map(|params| {
                params
                    .iter()
                    .filter(|(_, v)| !v.is_null() && !matches!(v, Value::String(s) if s.is_empty()))
                    .map(|(k, v)| (k.clone(), py_str(v)))
                    .collect()
            })
            .unwrap_or_default();
        let mut url = format!("{API_BASE}/{path}");
        if !pairs.is_empty() {
            let _ = write!(url, "?{}", crate::http::urlencode(&pairs));
        }
        url
    }

    // --- counter configuration (Management API) ----------------------------

    /// `counters()` — all counters visible to the token.
    pub async fn counters(&self) -> Result<Value, MetrikaFail> {
        let body = self
            .request(
                "GET",
                &Self::url(&format!("{API_MANAGEMENT}/counters"), None),
                false,
            )
            .await?;
        let counters = or_get(&body, "counters")?.cloned().unwrap_or(json!([]));
        let items = py_len(&counters)?;
        self.env.spend(&Record {
            source: SOURCE.to_owned(),
            operation: "management.counters".to_owned(),
            cost: Some(1.0),
            unit: "requests".to_owned(),
            task_id: None,
            items: Some(items),
            extra: None,
            at: None,
        })?;
        Ok(counters)
    }

    /// `counter(counter_id)`.
    pub async fn counter(&self, counter_id: &Value) -> Result<Value, MetrikaFail> {
        let body = self
            .request(
                "GET",
                &Self::url(
                    &format!("{API_MANAGEMENT}/counter/{}", py_str(counter_id)),
                    None,
                ),
                false,
            )
            .await?;
        self.env.spend(&Record {
            source: SOURCE.to_owned(),
            operation: "management.counter".to_owned(),
            cost: Some(1.0),
            unit: "requests".to_owned(),
            task_id: None,
            items: Some(1),
            extra: None,
            at: None,
        })?;
        Ok(body)
    }

    /// `goals(counter_id)` — the configured goals list (empty means "no goals").
    pub async fn goals(&self, counter_id: &Value) -> Result<Value, MetrikaFail> {
        let body = self
            .request(
                "GET",
                &Self::url(
                    &format!("{API_MANAGEMENT}/counter/{}/goals", py_str(counter_id)),
                    None,
                ),
                false,
            )
            .await?;
        let goals = or_get(&body, "goals")?.cloned().unwrap_or(json!([]));
        let items = py_len(&goals)?;
        self.env.spend(&Record {
            source: SOURCE.to_owned(),
            operation: "management.goals".to_owned(),
            cost: Some(1.0),
            unit: "requests".to_owned(),
            task_id: None,
            items: Some(items),
            extra: None,
            at: None,
        })?;
        Ok(goals)
    }

    /// `filters(counter_id)` — the raw body (spend counts the `filters` list).
    pub async fn filters(&self, counter_id: &Value) -> Result<Value, MetrikaFail> {
        let body = self
            .request(
                "GET",
                &Self::url(
                    &format!("{API_MANAGEMENT}/counter/{}/filters", py_str(counter_id)),
                    None,
                ),
                false,
            )
            .await?;
        let items = match or_get(&body, "filters")? {
            Some(v) => py_len(v)?,
            None => 0,
        };
        self.env.spend(&Record {
            source: SOURCE.to_owned(),
            operation: "management.filters".to_owned(),
            cost: Some(1.0),
            unit: "requests".to_owned(),
            task_id: None,
            items: Some(items),
            extra: None,
            at: None,
        })?;
        Ok(body)
    }

    /// `operations(counter_id)` — data operations such as URL-parameter removal.
    pub async fn operations(&self, counter_id: &Value) -> Result<Value, MetrikaFail> {
        let body = self
            .request(
                "GET",
                &Self::url(
                    &format!("{API_MANAGEMENT}/counter/{}/operations", py_str(counter_id)),
                    None,
                ),
                false,
            )
            .await?;
        let items = match or_get(&body, "operations")? {
            Some(v) => py_len(v)?,
            None => 0,
        };
        self.env.spend(&Record {
            source: SOURCE.to_owned(),
            operation: "management.operations".to_owned(),
            cost: Some(1.0),
            unit: "requests".to_owned(),
            task_id: None,
            items: Some(items),
            extra: None,
            at: None,
        })?;
        Ok(body)
    }

    // --- reports (Reporting API) --------------------------------------------

    /// `report(params, paginate=, limit=, offset=)` — `offset` is 1-based.
    pub async fn report(
        &self,
        params: &Value,
        paginate: bool,
        limit: i64,
        offset: i64,
    ) -> Result<Value, MetrikaFail> {
        if offset < 1 {
            return Err(MetrikaFail::python(
                "ValueError",
                "offset must be >= 1: the Reporting API numbers report rows from 1",
            ));
        }
        let mut base = Map::new();
        base.insert("accuracy".to_owned(), json!("full"));
        match params {
            Value::Object(object) => {
                for (k, v) in object {
                    base.insert(k.clone(), v.clone());
                }
            }
            other => {
                return Err(MetrikaFail::type_error(format!(
                    "'{}' object is not a mapping",
                    pystr::type_name(other)
                )));
            }
        }
        match self
            .fetch_report(&base, paginate, limit, offset, None)
            .await
        {
            Err(MetrikaFail::Metrika(exc)) if is_too_complicated(&exc) => {
                self.split_report(&base, paginate, limit, offset, exc).await
            }
            other => other,
        }
    }

    /// `_fetch_report` — one request, or the bounded offset/limit pagination.
    async fn fetch_report(
        &self,
        base: &Map<String, Value>,
        paginate: bool,
        limit: i64,
        offset: i64,
        row_budget: Option<i64>,
    ) -> Result<Value, MetrikaFail> {
        if !paginate {
            let mut query = base.clone();
            query.insert("limit".to_owned(), json!(limit));
            query.insert("offset".to_owned(), json!(offset));
            let body = match self
                .request("GET", &Self::url(API_REPORTS, Some(&query)), false)
                .await
            {
                Ok(body) => body,
                Err(err) => {
                    if let MetrikaFail::Metrika(_) = err {
                        // A refused request still consumed provider quota.
                        self.env.spend(&Record {
                            source: SOURCE.to_owned(),
                            operation: "report".to_owned(),
                            cost: Some(1.0),
                            unit: "requests".to_owned(),
                            task_id: None,
                            items: Some(0),
                            extra: Some(Map::from_iter([
                                (
                                    "metrics".to_owned(),
                                    base.get("metrics").cloned().unwrap_or(Value::Null),
                                ),
                                ("outcome".to_owned(), json!("failed")),
                            ])),
                            at: None,
                        })?;
                    }
                    return Err(err);
                }
            };
            let data = or_get(&body, "data")?;
            let items = match data {
                Some(data) if pystr::truthy(Some(data)) => py_len(data)?,
                _ => 0,
            };
            self.env.spend(&Record {
                source: SOURCE.to_owned(),
                operation: "report".to_owned(),
                cost: Some(1.0),
                unit: "requests".to_owned(),
                task_id: None,
                items: Some(items),
                extra: Some(Map::from_iter([(
                    "metrics".to_owned(),
                    base.get("metrics").cloned().unwrap_or(Value::Null),
                )])),
                at: None,
            })?;
            return Ok(body);
        }

        // The budget counts downloaded rows: each page asks only for the share
        // still left, and `row_budget` is this call's share of the one global cap.
        let budget = row_budget.unwrap_or(self.env.row_cap).max(1);
        let page_size = limit.clamp(100, 1000).min(budget);
        let mut first: Option<Value> = None;
        let mut rows: Vec<Value> = Vec::new();
        let mut cursor = offset;
        let mut pages = 0i64;
        let mut collected = offset - 1;
        loop {
            let page_limit = page_size.min(budget - collected);
            if page_limit <= 0 {
                break;
            }
            let mut query = base.clone();
            query.insert("limit".to_owned(), json!(page_limit));
            query.insert("offset".to_owned(), json!(cursor));
            let page = match self
                .request("GET", &Self::url(API_REPORTS, Some(&query)), false)
                .await
            {
                Ok(page) => page,
                Err(err) => {
                    if let MetrikaFail::Metrika(_) = err {
                        // The page that raised still consumed a request against quota.
                        self.env.spend(&Record {
                            source: SOURCE.to_owned(),
                            operation: "report.paginated".to_owned(),
                            cost: Some((pages + 1) as f64),
                            unit: "requests".to_owned(),
                            task_id: None,
                            items: Some(rows.len() as i64),
                            extra: Some(Map::from_iter([
                                (
                                    "metrics".to_owned(),
                                    base.get("metrics").cloned().unwrap_or(Value::Null),
                                ),
                                ("pages".to_owned(), json!(pages)),
                                ("outcome".to_owned(), json!("failed")),
                            ])),
                            at: None,
                        })?;
                    }
                    return Err(err);
                }
            };
            pages += 1;
            if first.is_none() {
                first = Some(page.clone());
            }
            // A falsy `data` member yields no rows.
            let data = py_get(&page, "data")?;
            let chunk = match data {
                Some(value) if pystr::truthy(Some(value)) => py_iter(value)?,
                _ => Vec::new(),
            };
            let chunk_len = chunk.len() as i64;
            rows.extend(chunk);
            collected = offset - 1 + rows.len() as i64;
            if chunk_len < page_limit {
                break;
            }
            if collected >= budget {
                break;
            }
            let total = or_get(first.as_ref().unwrap_or(&Value::Null), "total_rows")?;
            if pystr::truthy(total) && py_ge_i64(collected, total.unwrap_or(&Value::Null))? {
                break;
            }
            cursor += page_limit;
            self.env.sleeper.pause(PAGE_PAUSE).await;
        }

        self.env.spend(&Record {
            source: SOURCE.to_owned(),
            operation: "report.paginated".to_owned(),
            cost: Some(pages as f64),
            unit: "requests".to_owned(),
            task_id: None,
            items: Some(rows.len() as i64),
            extra: Some(Map::from_iter([
                (
                    "metrics".to_owned(),
                    base.get("metrics").cloned().unwrap_or(Value::Null),
                ),
                ("pages".to_owned(), json!(pages)),
            ])),
            at: None,
        })?;
        // The first page must be an object; anything else is a failure.
        let first = first.unwrap_or(Value::Null);
        let mut result = py_dict_from(first.clone())?;
        result.insert("data".to_owned(), Value::Array(rows));
        let query_src = or_get(&first, "query")?;
        let mut query_echo = match query_src {
            Some(value) => py_dict_from_pairs(value.clone())?,
            None => Map::new(),
        };
        query_echo.insert("limit".to_owned(), json!(limit));
        query_echo.insert("offset".to_owned(), json!(offset));
        result.insert("query".to_owned(), Value::Object(query_echo));
        let total = or_get(&first, "total_rows")?;
        let capped = collected >= budget
            && match total {
                Some(total) if is_number(total) => !py_ge_i64(collected, total)?,
                _ => true,
            };
        result.insert("capped".to_owned(), json!(capped));
        Ok(Value::Object(result))
    }

    /// `by_time(params, limit=, offset=)` — `stat/v1/data/bytime`.
    pub async fn by_time(
        &self,
        params: &Value,
        limit: i64,
        offset: i64,
    ) -> Result<Value, MetrikaFail> {
        if offset < 1 {
            return Err(MetrikaFail::python(
                "ValueError",
                "offset must be >= 1: the Reporting API numbers report rows from 1",
            ));
        }
        let mut query = py_dict_from_pairs(params.clone())?;
        query.insert("accuracy".to_owned(), json!("full"));
        query.insert("limit".to_owned(), json!(limit));
        query.insert("offset".to_owned(), json!(offset));
        let body = self
            .request(
                "GET",
                &Self::url(&format!("{API_REPORTS}/bytime"), Some(&query)),
                false,
            )
            .await?;
        let data = or_get(&body, "data")?;
        let items = match data {
            Some(data) if pystr::truthy(Some(data)) => py_len(data)?,
            _ => 0,
        };
        self.env.spend(&Record {
            source: SOURCE.to_owned(),
            operation: "report.bytime".to_owned(),
            cost: Some(1.0),
            unit: "requests".to_owned(),
            task_id: None,
            items: Some(items),
            extra: None,
            at: None,
        })?;
        Ok(body)
    }

    // --- raw logs (Logs API) -------------------------------------------------

    /// `create_log_request` — request a `visits`/`hits` raw-log export.
    pub async fn create_log_request(
        &self,
        counter_id: &Value,
        source: &str,
        date1: &str,
        date2: &str,
        fields: &Value,
    ) -> Result<Value, MetrikaFail> {
        let n_fields = py_len(fields)?;
        self.env.spend(&Record {
            source: SOURCE.to_owned(),
            operation: "logs.create".to_owned(),
            cost: Some(1.0),
            unit: "requests".to_owned(),
            task_id: None,
            items: Some(n_fields),
            extra: Some(Map::from_iter([
                ("source".to_owned(), json!(source)),
                ("period".to_owned(), json!(format!("{date1}..{date2}"))),
            ])),
            at: None,
        })?;
        // Every field name must be a string.
        let mut joined = Vec::new();
        for (index, field) in py_iter(fields)?.into_iter().enumerate() {
            match field {
                Value::String(text) => joined.push(text),
                other => {
                    return Err(MetrikaFail::type_error(format!(
                        "sequence item {index}: expected str instance, {} found",
                        pystr::type_name(&other)
                    )));
                }
            }
        }
        let params = Map::from_iter([
            ("source".to_owned(), json!(source)),
            ("date1".to_owned(), json!(date1)),
            ("date2".to_owned(), json!(date2)),
            ("fields".to_owned(), json!(joined.join(","))),
        ]);
        let body = self
            .request(
                "POST",
                &Self::url(
                    &format!(
                        "{API_MANAGEMENT}/counter/{}/logrequests",
                        py_str(counter_id)
                    ),
                    Some(&params),
                ),
                false,
            )
            .await?;
        Ok(or_get(&body, "log_request")?.cloned().unwrap_or(body))
    }

    /// `log_requests(counter_id)` — the `requests` list.
    pub async fn log_requests(&self, counter_id: &Value) -> Result<Value, MetrikaFail> {
        let body = self
            .request(
                "GET",
                &Self::url(
                    &format!(
                        "{API_MANAGEMENT}/counter/{}/logrequests",
                        py_str(counter_id)
                    ),
                    None,
                ),
                false,
            )
            .await?;
        Ok(or_get(&body, "requests")?.cloned().unwrap_or(json!([])))
    }

    /// `log_request` — find one request by ID; the API has no single-request endpoint.
    pub async fn log_request(
        &self,
        counter_id: &Value,
        request_id: &Value,
    ) -> Result<Value, MetrikaFail> {
        let items = self.log_requests(counter_id).await?;
        for item in py_iter(&items)? {
            let got = py_get(&item, "request_id")?;
            if py_eq(got.unwrap_or(&Value::Null), request_id) {
                return Ok(item);
            }
        }
        Err(MetrikaError {
            status: 404,
            message: format!("log request {} not found", py_str(request_id)),
        }
        .into())
    }

    /// `download_log_part` — one completed export part as raw TSV text.
    pub async fn download_log_part(
        &self,
        counter_id: &Value,
        request_id: &Value,
        part: &Value,
    ) -> Result<String, MetrikaFail> {
        let body = self
            .request(
                "GET",
                &Self::url(
                    &format!(
                        "{API_MANAGEMENT}/counter/{}/logrequest/{}/part/{}/download",
                        py_str(counter_id),
                        py_str(request_id),
                        py_str(part)
                    ),
                    None,
                ),
                true,
            )
            .await?;
        Ok(body.as_str().unwrap_or_default().to_owned())
    }

    /// `cancel_log_request` — release one of the limited Logs API request slots.
    pub async fn cancel_log_request(
        &self,
        counter_id: &Value,
        request_id: &Value,
    ) -> Result<Value, MetrikaFail> {
        let params = Map::from_iter([("request_id".to_owned(), request_id.clone())]);
        let body = self
            .request(
                "POST",
                &Self::url(
                    &format!(
                        "{API_MANAGEMENT}/counter/{}/logrequest/{}/cancel",
                        py_str(counter_id),
                        py_str(request_id)
                    ),
                    Some(&params),
                ),
                false,
            )
            .await?;
        Ok(or_get(&body, "log_request")?.cloned().unwrap_or(body))
    }
}
