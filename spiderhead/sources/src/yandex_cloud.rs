//! Yandex Cloud Search API client for Wordstat demand and Yandex Web SERPs.
//!
//! Async-only endpoints at `searchapi.api.cloud.yandex.net`. A *billed* request that loses its response is not
//! resent (no idempotency key exists): it is journaled and raised as `NetworkAmbiguousError`.

use std::cell::Cell;
use std::future::Future;
use std::path::Path;
use std::pin::Pin;

use serde_json::{json, Map, Value};
use spiderhead_core::host::Host;

use crate::credentials::{yandex_cloud_api_key, yandex_cloud_folder_id, CredentialError};
use crate::http::{exponential_pause, retry_slots, Request, Sleeper, Transport, TransportError};
use crate::metrika::{py_eq, SpendSink};
use crate::spend::{self, Record};
use crate::{pyerr, pyjson, pystr};

mod errors;
pub use errors::*;
mod transport;
pub use transport::*;
mod values;
pub use values::*;
mod serp;
pub use serp::*;

/// `HOST` — the Cloud Search API base.
pub const HOST: &str = "https://searchapi.api.cloud.yandex.net";
/// `OPERATIONS` — the long-running-operation polling base.
pub const OPERATIONS: &str = "https://operation.api.cloud.yandex.net/operations";
/// `SOURCE` — the spend-ledger provider name.
pub const SOURCE: &str = "yandex_cloud";

/// Shared base: credential resolution plus the request retry engine. The Wordstat and web search clients
/// add methods only, so one type covers both.
pub struct Cloud<'e, T: Transport> {
    env: &'e CloudEnv<'e, T>,
    /// API key.
    pub key: String,
    /// Cloud folder id.
    pub folder: String,
    /// Minimum spacing between requests, in seconds.
    min_interval: f64,
    /// Monotonic timestamp of the last request start.
    last: Cell<f64>,
    /// Send seam; `None` is the live boundary.
    send: Option<&'e CloudTransport<'e>>,
}

/// Constructor arguments: `api_key`, `folder_id`, `rps`, `transport`.
#[derive(Default)]
pub struct CloudArgs<'a> {
    /// A falsy value resolves the stored key.
    pub api_key: Option<&'a Value>,
    /// A falsy value resolves the stored folder id.
    pub folder_id: Option<&'a Value>,
    /// Requests per second; the spacing is `1.0 / rps`. Absent defaults to `5`; `None` is a type error.
    pub rps: Option<&'a Value>,
    /// `transport=` — `None` uses the live send.
    pub transport: Option<&'a CloudTransport<'a>>,
}

impl<'e, T: Transport> Cloud<'e, T> {
    /// Build the base client; credentials resolve eagerly, at construction.
    pub fn new(env: &'e CloudEnv<'e, T>, args: &CloudArgs<'e>) -> Result<Self, CloudError> {
        let key = if pystr::truthy(args.api_key) {
            str_value(args.api_key.unwrap_or(&Value::Null))
        } else {
            cloud_credential("api key", || match env.api_key {
                Some(probe) => probe(),
                None => yandex_cloud_api_key(env.host, env.root),
            })?
        };
        let folder = if pystr::truthy(args.folder_id) {
            str_value(args.folder_id.unwrap_or(&Value::Null))
        } else {
            cloud_credential("folder id", || match env.folder_id {
                Some(probe) => probe(),
                None => yandex_cloud_folder_id(env.host, env.root),
            })?
        };
        let interval = match args.rps {
            None => 0.2,
            Some(value) => min_interval(value)?,
        };
        Ok(Cloud {
            env,
            key,
            folder,
            min_interval: interval,
            last: Cell::new(0.0),
            send: args.transport,
        })
    }

    /// Resolve the `transport=` or live send into one `CloudCall` object.
    fn call(&self) -> Box<dyn CloudCall + '_> {
        match self.send {
            Some(send) => Box::new(SyncCloudCall(send)),
            None => Box::new(LiveCloudCall(self.env.transport)),
        }
    }

    /// `_request` — one send with the rate limiter, backoff, and billed semantics.
    pub async fn request(
        &self,
        url: &str,
        body: Option<&Value>,
        method: &str,
        retries: usize,
        billed: bool,
        operation: Option<&str>,
    ) -> Result<(i64, Value), CloudError> {
        let mut call = self.call();
        let operation_name = operation.unwrap_or(url);
        let mut last: (i64, Value) = (0, json!("no request was completed"));
        for slot in retry_slots(retries) {
            let wait = self.min_interval - (self.env.monotonic() - self.last.get());
            if wait > 0.0 {
                self.env.sleeper.pause(wait).await;
            }
            self.last.set(self.env.monotonic());
            match call.send(method, url, body, &self.key).await {
                Ok(answer) if answer.strict => {
                    // A decode or JSON error on either step is journaled when billed, then raised.
                    let parsed = pystr::decode_utf8_strict(&answer.body)
                        .map_err(CloudError::Decode)
                        .and_then(|text| {
                            serde_json::from_str(&text).map_err(|_| {
                                CloudError::python(
                                    "JSONDecodeError",
                                    pyjson::decode_error(&text)
                                        .unwrap_or_else(|| "JSON decode error".to_owned()),
                                )
                            })
                        });
                    match parsed {
                        Ok(value) => return Ok((answer.status, value)),
                        Err(err) => {
                            if billed {
                                self.env.spend(&Record {
                                    source: SOURCE.to_owned(),
                                    operation: operation_name.to_owned(),
                                    cost: Some(0.0),
                                    unit: "requests".to_owned(),
                                    items: Some(1),
                                    task_id: None,
                                    extra: Some(Map::from_iter([
                                        ("response_received".to_owned(), json!(true)),
                                        ("attempt_failed".to_owned(), json!("malformed_response")),
                                        ("charge_uncertain".to_owned(), json!(true)),
                                        ("status".to_owned(), json!(answer.status)),
                                    ])),
                                    at: None,
                                })?;
                            }
                            return Err(err);
                        }
                    }
                }
                Ok(answer) => {
                    // An HTTP error body is read as text with invalid UTF-8 replaced.
                    let raw = String::from_utf8_lossy(&answer.body).into_owned();
                    if matches!(answer.status, 429 | 500 | 503) {
                        if slot.has_next {
                            self.env.sleeper.pause(exponential_pause(slot.index)).await;
                        }
                        last = (answer.status, Value::String(raw));
                        continue;
                    }
                    return Ok((answer.status, maybe_json(&raw)));
                }
                Err(err) if err.network_retryable() => {
                    if billed {
                        self.env.spend(&Record {
                            source: SOURCE.to_owned(),
                            operation: operation_name.to_owned(),
                            cost: Some(0.0),
                            unit: "requests".to_owned(),
                            items: None,
                            task_id: None,
                            extra: Some(Map::from_iter([
                                ("attempt_failed".to_owned(), json!("network_error")),
                                ("detail".to_owned(), json!(err.to_string())),
                            ])),
                            at: None,
                        })?;
                        return Err(CloudError::NetworkAmbiguous(format!(
                            "{operation_name}: response lost; request may already be billed: {err}"
                        )));
                    }
                    if slot.has_next {
                        self.env.sleeper.pause(exponential_pause(slot.index)).await;
                    }
                    last = (0, json!(format!("network: {err}")));
                }
                Err(err) => return Err(err),
            }
        }
        Ok(last)
    }

    /// `Wordstat.top` — expand a phrase into the topRequests response.
    pub async fn wordstat_top(
        &self,
        phrase: &Value,
        limit: &Value,
        regions: &Value,
        devices: &Value,
    ) -> Result<Value, CloudError> {
        let num_phrases = match pystr::to_python_int(limit) {
            Ok(value) => value,
            Err((class, message)) => return Err(CloudError::python(class, message)),
        };
        // Region and device lists are converted before the request is sent, so conversion errors land first.
        let regions = py_list(regions)?;
        let devices = py_list(devices)?;
        let url = format!("{HOST}/v2/wordstat/topRequests");
        let body = json!({
            "phrase": phrase,
            "numPhrases": num_phrases,
            "folderId": self.folder,
            "regions": regions,
            "devices": devices,
        });
        let (status, body) = self
            .request(
                &url,
                Some(&body),
                "POST",
                5,
                true,
                Some("wordstat.topRequests"),
            )
            .await?;
        self.env.spend(&Record {
            source: SOURCE.to_owned(),
            operation: "wordstat.topRequests".to_owned(),
            cost: Some(1.0),
            unit: "requests".to_owned(),
            items: Some(1),
            task_id: None,
            extra: Some(Map::from_iter([
                ("phrase".to_owned(), phrase.clone()),
                ("regions".to_owned(), json!(regions)),
            ])),
            at: None,
        })?;
        if status != 200 {
            return Err(CloudError::python(
                "RuntimeError",
                format!("topRequests {status}: {}", str_value(&body)),
            ));
        }
        Ok(body)
    }

    /// `Wordstat.dynamics` — seasonality over an RFC 3339 range.
    pub async fn wordstat_dynamics(
        &self,
        phrase: &Value,
        from_date: &Value,
        to_date: &Value,
        period: &Value,
        regions: &Value,
        devices: &Value,
    ) -> Result<Value, CloudError> {
        let regions = py_list(regions)?;
        let devices = py_list(devices)?;
        let url = format!("{HOST}/v2/wordstat/dynamics");
        let body = json!({
            "phrase": phrase,
            "folderId": self.folder,
            "period": period,
            "fromDate": from_date,
            "toDate": to_date,
            "regions": regions,
            "devices": devices,
        });
        let (status, body) = self
            .request(
                &url,
                Some(&body),
                "POST",
                5,
                true,
                Some("wordstat.dynamics"),
            )
            .await?;
        self.env.spend(&Record {
            source: SOURCE.to_owned(),
            operation: "wordstat.dynamics".to_owned(),
            cost: Some(1.0),
            unit: "requests".to_owned(),
            items: Some(1),
            task_id: None,
            extra: Some(Map::from_iter([
                ("phrase".to_owned(), phrase.clone()),
                ("period".to_owned(), period.clone()),
            ])),
            at: None,
        })?;
        if status != 200 {
            return Err(CloudError::python(
                "RuntimeError",
                format!("dynamics {status}: {}", str_value(&body)),
            ));
        }
        Ok(body)
    }

    /// `Wordstat.expand` — `top` reduced to `[{phrase: base_volume}, metadata]`.
    pub async fn wordstat_expand(
        &self,
        phrase: &Value,
        limit: &Value,
        regions: &Value,
        devices: &Value,
    ) -> Result<Value, CloudError> {
        let body = self.wordstat_top(phrase, limit, regions, devices).await?;
        let get = |key: &str| -> Result<&Value, CloudError> {
            match &body {
                Value::Object(map) => Ok(map.get(key).unwrap_or(&Value::Null)),
                other => Err(CloudError::python(
                    "AttributeError",
                    format!(
                        "'{}' object has no attribute 'get'",
                        pystr::type_name(other)
                    ),
                )),
            }
        };
        let mut pool = Map::new();
        let mut origin = Map::new();
        for tag in ["results", "associations"] {
            let items = get(tag)?.clone();
            let items = match &items {
                // A falsy member yields nothing; a truthy non-list is iterated (text by characters, objects by keys)
                // before the item lookup fails, or is rejected when it is not iterable at all.
                _ if !pystr::truthy(Some(&items)) => Vec::new(),
                Value::Array(items) => items.clone(),
                Value::String(text) => text.chars().map(|c| json!(c.to_string())).collect(),
                Value::Object(map) => map.keys().cloned().map(Value::String).collect(),
                other => {
                    return Err(CloudError::python(
                        "TypeError",
                        format!("'{}' object is not iterable", pystr::type_name(other)),
                    ))
                }
            };
            for item in &items {
                let raw_phrase = match item {
                    Value::Object(map) => map.get("phrase").cloned().unwrap_or(Value::Null),
                    other => {
                        return Err(CloudError::python(
                            "AttributeError",
                            format!(
                                "'{}' object has no attribute 'get'",
                                pystr::type_name(other)
                            ),
                        ))
                    }
                };
                let key = normalize(&raw_phrase)?;
                if key.is_empty() {
                    continue;
                }
                // A missing or unreadable count becomes 0.
                let count = match item {
                    Value::Object(map) => {
                        let raw = map.get("count").cloned().unwrap_or(Value::Null);
                        if pystr::truthy(Some(&raw)) {
                            pystr::to_python_int(&raw).unwrap_or(0)
                        } else {
                            0
                        }
                    }
                    _ => 0,
                };
                let prior = pool.get(&key).and_then(|v| v.as_i64()).unwrap_or(-1);
                if count >= prior {
                    pool.insert(key.clone(), json!(count));
                }
                if !origin.contains_key(&key) {
                    origin.insert(key, json!(tag));
                }
            }
        }
        Ok(json!([
            pool,
            {
                "totalCount": get("totalCount")?,
                "results": py_len(get("results")?)?,
                "associations": py_len(get("associations")?)?,
                "origin": origin,
            }
        ]))
    }

    /// `WebSearch.search` — submit one async SERP request and poll until done.
    #[allow(clippy::too_many_arguments)] // Python signature kept positional
    pub async fn web_search(
        &self,
        query: &Value,
        region: &Value,
        search_type: &Value,
        groups: &Value,
        docs_in_group: &Value,
        family: &Value,
        timeout: &Value,
    ) -> Result<Value, CloudError> {
        let url = format!("{HOST}/v2/web/searchAsync");
        let body = serp_body(
            query,
            region,
            search_type,
            groups,
            docs_in_group,
            family,
            &self.folder,
        );
        let (status, operation) = self
            .request(&url, Some(&body), "POST", 5, true, Some("web.searchAsync"))
            .await?;
        self.env.spend(&Record {
            source: SOURCE.to_owned(),
            operation: "web.searchAsync".to_owned(),
            cost: Some(1.0),
            unit: "requests".to_owned(),
            items: Some(1),
            task_id: None,
            extra: Some(Map::from_iter([
                ("query".to_owned(), query.clone()),
                ("region".to_owned(), region.clone()),
            ])),
            at: None,
        })?;
        if status != 200 {
            return Err(CloudError::python(
                "RuntimeError",
                format!("searchAsync {status}: {}", str_value(&operation)),
            ));
        }
        let operation_id = match &operation {
            Value::Object(map) => map.get("id").cloned().unwrap_or(Value::Null),
            other => {
                return Err(CloudError::python(
                    "AttributeError",
                    format!(
                        "'{}' object has no attribute 'get'",
                        pystr::type_name(other)
                    ),
                ))
            }
        };
        let deadline = self.env.monotonic() + deadline_seconds(timeout)?;
        let op_url = format!("{OPERATIONS}/{}", str_value(&operation_id));
        while self.env.monotonic() < deadline {
            self.env.sleeper.pause(5.0).await;
            let (_, done) = self.request(&op_url, None, "GET", 5, false, None).await?;
            if done.is_object() && pystr::truthy(done.get("done")) {
                if pystr::truthy(done.get("error")) {
                    return Err(CloudError::python(
                        "RuntimeError",
                        format!(
                            "operation returned an error: {}",
                            str_value(done.get("error").unwrap_or(&Value::Null))
                        ),
                    ));
                }
                let raw = raw_data(&done)?;
                let xml = serp_xml(&raw)?;
                return Ok(json!({
                    "operation_id": operation_id,
                    "xml": xml,
                    "docs": parse_serp(&xml),
                }));
            }
        }
        Err(CloudError::python(
            "RuntimeError",
            format!(
                "operation {} did not finish within {}s",
                str_value(&operation_id),
                str_value(timeout)
            ),
        ))
    }

    /// `WebSearch.search_batch` — concurrent submissions keyed by query text.
    #[allow(clippy::too_many_arguments)] // Python signature kept positional
    pub async fn search_batch(
        &self,
        queries: &Value,
        region: &Value,
        search_type: &Value,
        groups: &Value,
        docs_in_group: &Value,
        family: &Value,
        timeout: &Value,
        poll: &Value,
    ) -> Result<Value, CloudError> {
        // Iterate the queries (text by characters, objects by keys), then deduplicate in order; unhashable members are an error.
        let members = match queries {
            Value::Array(items) => items.clone(),
            Value::String(text) => text.chars().map(|c| json!(c.to_string())).collect(),
            Value::Object(map) => map.keys().cloned().map(Value::String).collect(),
            other => {
                return Err(CloudError::python(
                    "TypeError",
                    format!("'{}' object is not iterable", pystr::type_name(other)),
                ))
            }
        };
        let mut unique: Vec<Value> = Vec::new();
        for query in members {
            if matches!(query, Value::Array(_) | Value::Object(_)) {
                // Each member is stored as a key; the error names the failing insertion, not a bare unhashable value.
                return Err(CloudError::python(
                    "TypeError",
                    format!(
                        "cannot use '{}' as a dict key (unhashable type: '{}')",
                        pystr::type_name(&query),
                        pystr::type_name(&query)
                    ),
                ));
            }
            if !unique.iter().any(|seen| py_eq(seen, &query)) {
                unique.push(query);
            }
        }
        let url = format!("{HOST}/v2/web/searchAsync");
        let mut pending: Vec<(Value, Value)> = Vec::new(); // (operation_id, query)
        let mut submitted: Vec<(Value, Value)> = Vec::new(); // (query, operation_id)
        let mut results = Map::new();
        for query in &unique {
            let body = serp_body(
                query,
                region,
                search_type,
                groups,
                docs_in_group,
                family,
                &self.folder,
            );
            let (status, operation) = match self
                .request(&url, Some(&body), "POST", 5, true, Some("web.searchAsync"))
                .await
            {
                Ok(pair) => pair,
                Err(CloudError::NetworkAmbiguous(message)) => {
                    results.insert(
                        dict_key(query),
                        json!({
                            "error": message,
                            "status": "network_ambiguous",
                            "docs": [],
                        }),
                    );
                    continue;
                }
                Err(err) => return Err(err),
            };
            if status == 200
                && matches!(&operation, Value::Object(map) if pystr::truthy(map.get("id")))
            {
                let operation_id = operation.get("id").cloned().unwrap_or(Value::Null);
                pending.push((operation_id.clone(), query.clone()));
                submitted.push((query.clone(), operation_id));
            } else {
                results.insert(
                    dict_key(query),
                    json!({
                        "error": format!("searchAsync rejected: {status}: {}", str_value(&operation)),
                        "status": "rejected",
                        "docs": [],
                    }),
                );
            }
        }
        let operation_ids: Map<String, Value> = submitted
            .iter()
            .map(|(query, operation)| (dict_key(query), operation.clone()))
            .collect();
        self.env.spend(&Record {
            source: SOURCE.to_owned(),
            operation: "web.searchAsync".to_owned(),
            cost: Some(pending.len() as f64),
            unit: "requests".to_owned(),
            items: Some(unique.len() as i64),
            task_id: None,
            extra: Some(Map::from_iter([
                ("region".to_owned(), region.clone()),
                ("batch".to_owned(), json!(true)),
                ("operation_ids".to_owned(), Value::Object(operation_ids)),
            ])),
            at: None,
        })?;

        let deadline = self.env.monotonic() + deadline_seconds(timeout)?;
        while !pending.is_empty() && self.env.monotonic() < deadline {
            // The poll interval is used inside the loop, so a bad interval on an already-empty `pending` is never reached.
            self.env.sleeper.pause(sleep_seconds(poll)?).await;
            let mut still: Vec<(Value, Value)> = Vec::new();
            for (operation_id, query) in pending {
                let op_url = format!("{OPERATIONS}/{}", str_value(&operation_id));
                let (status, done) = self.request(&op_url, None, "GET", 5, false, None).await?;
                if status != 200 {
                    results.insert(
                        dict_key(&query),
                        json!({
                            "error": safe_operation_message(&done),
                            "status": "operation_read_error",
                            "operation_id": operation_id,
                            "http_status": status,
                            "docs": [],
                        }),
                    );
                    continue;
                }
                if !(done.is_object() && pystr::truthy(done.get("done"))) {
                    still.push((operation_id, query));
                    continue;
                }
                if pystr::truthy(done.get("error")) {
                    results.insert(
                        dict_key(&query),
                        json!({
                            "error": done.get("error").cloned().unwrap_or(Value::Null),
                            "status": "operation_error",
                            "operation_id": operation_id,
                            "docs": [],
                        }),
                    );
                    continue;
                }
                let raw = raw_data(&done)?;
                let xml = serp_xml(&raw)?;
                results.insert(
                    dict_key(&query),
                    json!({
                        "operation_id": operation_id,
                        "status": "ok",
                        "xml": xml,
                        "docs": parse_serp(&xml),
                    }),
                );
            }
            pending = still;
        }
        Ok(Value::Object(results))
    }
}
