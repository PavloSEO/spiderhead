//! Arsenkin API client for search volume, SERP clustering, and Yandex top results.
//!
//! Task lifecycle: `/set` creates and charges a task (`cost` in the response), `/check`
//! polls status, `/get` fetches the already-paid result, `/info` reports account limits.
//! Two safeguards keep paid results recoverable: every non-terminal status means "keep
//! waiting", and `task_id` is written to the spend log when the task is created, before
//! result parsing — `get`/`refetch` then costs nothing.

use serde_json::{json, Map, Value};
use spiderhead_core::host::Host;
use std::cell::RefCell;
use std::path::Path;

use crate::credentials::{self, CredentialError};
use crate::http::{exponential_pause, retry_slots, Request, Sleeper, Transport, TransportError};
use crate::metrika::SpendSink;
use crate::spend::{self, Record};
use crate::{pyerr as pyerrs, pyjson, pystr};

mod errors;
pub use errors::*;
mod wordstat;
pub use wordstat::*;

/// Base URL of the Arsenkin tools API.
pub const BASE: &str = "https://arsenkin.ru/api/tools";
/// `MAX_RPM` — the API ceiling across all endpoints, in requests per minute.
pub const MAX_RPM: i64 = 30;
/// `MAX_CONCURRENT` — the API ceiling for concurrently running tasks.
pub const MAX_CONCURRENT: usize = 5;
/// `SOURCE` — the spend-ledger source name.
pub const SOURCE: &str = "arsenkin";

/// `WORDSTAT_TOOL` — the provider tool that returns exact `!W`.
pub const WORDSTAT_TOOL: &str = "wordstat";
/// `WORDSTAT_TYPE_FREQUENCY` — the `type` selector inside the wordstat tool.
pub const WORDSTAT_TYPE_FREQUENCY: i64 = 1;
/// `WORDSTAT_WS_TYPES` — `overal` is the provider's field for the `!WS` operator; neither
/// `quoted` (`"WS"`) nor `exact` (`[!WS]`) is the `!W` measurement.
pub const WORDSTAT_WS_TYPES: [&str; 2] = ["base", "overal"];

/// The seams every adapter call runs against.
pub struct ArsEnv<'a, T: Transport> {
    /// Env-var/filesystem host.
    pub host: &'a dyn Host,
    /// Credential config root for the live readers.
    pub root: &'a Path,
    /// HTTP boundary for `post`.
    pub transport: &'a T,
    /// Sleep hook for backoff, the rate limiter and wait intervals.
    pub sleeper: &'a Sleeper,
    /// Spend-ledger sink; None writes to the real ledger.
    pub spend: Option<&'a SpendSink<'a>>,
    /// Token source; None reads the real credential store.
    pub token: Option<&'a dyn Fn() -> Result<String, CredentialError>>,
    /// Monotonic clock for wait bounds and the rate limiter window; None uses the real clock.
    pub clock: Option<&'a dyn Fn() -> f64>,
}

impl<'a, T: Transport> ArsEnv<'a, T> {
    /// The production environment.
    pub fn live(host: &'a dyn Host, root: &'a Path, transport: &'a T) -> Self {
        ArsEnv {
            host,
            root,
            transport,
            sleeper: &Sleeper::Real,
            spend: None,
            token: None,
            clock: None,
        }
    }

    /// The real spend ledger or the injected sink; I/O errors propagate.
    pub(crate) fn record(&self, rec: &Record) -> Result<Value, ArsError> {
        match self.spend {
            Some(sink) => sink(rec),
            None => spend::record(self.host, rec),
        }
        .map_err(|err| pyerr("OSError", err.to_string()))
    }

    pub(crate) fn token(&self) -> Result<String, ArsError> {
        match self.token {
            Some(probe) => probe(),
            None => credentials::arsenkin_token(self.host, self.root),
        }
        .map_err(cred_err)
    }

    /// Current monotonic time from the injected clock or the wall clock.
    pub(crate) fn monotonic(&self) -> f64 {
        match self.clock {
            Some(clock) => clock(),
            None => std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_secs_f64())
                .unwrap_or_default(),
        }
    }
}

/// `RateLimiter` — a sliding window capped at `max_calls` per `period` seconds.
pub struct RateLimiter {
    /// Calls allowed per window, minus a safety margin below the published ceiling (at least 1).
    max_calls: i64,
    /// The window length in seconds.
    period: f64,
    /// Recorded call timestamps.
    calls: RefCell<Vec<f64>>,
}

impl RateLimiter {
    /// Default limiter: MAX_RPM calls per minute with a safety margin of 3.
    pub fn new(max_calls: i64, period: f64, safety: i64) -> Self {
        RateLimiter {
            max_calls: (max_calls - safety).max(1),
            period,
            calls: RefCell::new(Vec::new()),
        }
    }

    /// `acquire()` — prune stale timestamps; append `now` when the window has room,
    /// otherwise sleep past the oldest call plus the 0.05s margin and retry.
    pub async fn acquire<T: Transport>(&self, env: &ArsEnv<'_, T>) {
        loop {
            let sleep_for = {
                let now = env.monotonic();
                let mut calls = self.calls.borrow_mut();
                calls.retain(|t| now - *t < self.period);
                if (calls.len() as i64) < self.max_calls {
                    calls.push(now);
                    return;
                }
                (self.period - (now - calls[0]) + 0.05).max(0.1)
            };
            env.sleeper.pause(sleep_for).await;
        }
    }
}

impl Default for RateLimiter {
    fn default() -> Self {
        RateLimiter::new(MAX_RPM, 60.0, 3)
    }
}

/// Client for one Arsenkin account.
pub struct Client<'e, T: Transport> {
    env: &'e ArsEnv<'e, T>,
    token: String,
    limiter: RateLimiter,
}

/// Optional construction arguments for the Arsenkin client.
#[derive(Default)]
pub struct ClientArgs<'a> {
    /// Explicit token; an empty value falls back to the stored credential.
    pub token: Option<&'a Value>,
    /// Rate limiter to use; None builds the default limiter.
    pub limiter: Option<RateLimiter>,
}

impl<'e, T: Transport> Client<'e, T> {
    /// Builds a client, using the explicit token when given and the stored credential otherwise.
    pub fn new(env: &'e ArsEnv<'e, T>, args: ClientArgs<'e>) -> Result<Self, ArsError> {
        let token = match args.token.filter(|v| pystr::truthy(Some(v))) {
            Some(v) => pystr::repr_value(v),
            None => env.token()?,
        };
        Ok(Client {
            env,
            token,
            limiter: args.limiter.unwrap_or_default(),
        })
    }

    /// Bearer token resolved at construction.
    pub fn token(&self) -> &str {
        &self.token
    }

    /// POST one JSON payload, retrying up to `retries` times.
    ///
    /// HTTP 429 means no task was created, so retrying is safe. A transport failure
    /// loses only the response: for a billed endpoint the attempt is journaled as a
    /// charge of unknown size, and the call fails instead of resending a payload that
    /// may already have created and charged a task.
    pub async fn post(
        &self,
        endpoint: &str,
        body: &Value,
        retries: usize,
        billed: bool,
    ) -> Result<Value, ArsError> {
        let url = format!("{BASE}/{endpoint}");
        let data = pyjson::dumps_opts(body, false, false).into_bytes();
        let mut last: Option<ArsError> = None;
        for slot in retry_slots(retries) {
            self.limiter.acquire(self.env).await;
            let request = Request::new("POST", &url)
                .header("Authorization", format!("Bearer {}", self.token))
                .header("Content-Type", "application/json")
                .body(data.clone())
                .timeout(90.0);
            match self.env.transport.send(&request).await {
                Ok(response) => {
                    let raw = pystr::decode_utf8_strict(&response.body)
                        .map_err(|m| pyerr("UnicodeDecodeError", m))?;
                    let payload = match serde_json::from_str::<Value>(&raw) {
                        Ok(payload) => payload,
                        Err(_) => {
                            return Err(pyerr(
                                "JSONDecodeError",
                                pyjson::decode_error(&raw)
                                    .unwrap_or_else(|| "JSON decode error".to_owned()),
                            ));
                        }
                    };
                    if let Some(map) = payload.as_object() {
                        if pystr::repr_value(map.get("code").unwrap_or(&Value::Null)) == "429" {
                            self.env.sleeper.pause(exponential_pause(slot.index)).await;
                            last = Some(provider("429", json!(raw)));
                            continue;
                        }
                        if map.get("status") == Some(&json!("Error")) {
                            let mut err = provider(
                                pystr::repr_value(map.get("code").unwrap_or(&Value::Null)),
                                map.get("error").cloned().unwrap_or_else(|| json!(raw)),
                            );
                            if let ArsError::Provider(p) = &mut err {
                                p.payload = Some(payload);
                            }
                            return Err(err);
                        }
                    }
                    return Ok(payload);
                }
                Err(TransportError::Http(response)) => {
                    let raw = String::from_utf8_lossy(&response.body).into_owned();
                    if response.status == 429 {
                        self.env.sleeper.pause(exponential_pause(slot.index)).await;
                        last = Some(provider("429", json!(raw)));
                        continue;
                    }
                    return Err(provider(response.status.to_string(), json!(raw)));
                }
                Err(TransportError::Connect(kind)) => {
                    // Timeouts are not URL errors, so they escape this handler unchanged.
                    if kind == "timeout" {
                        return Err(pyerr("TimeoutError", "timed out"));
                    }
                    let detail = pyerrs::url_error(&kind);
                    if billed {
                        self.env.record(&Record {
                            source: SOURCE.to_owned(),
                            operation: endpoint.to_owned(),
                            cost: Some(0.0),
                            unit: "limits".to_owned(),
                            task_id: None,
                            items: None,
                            extra: Some(Map::from_iter([
                                ("attempt_failed".to_owned(), json!("network_error")),
                                ("detail".to_owned(), json!(detail)),
                            ])),
                            at: None,
                        })?;
                        return Err(provider(
                            "NETWORK",
                            json!(format!(
                                "{endpoint}: response lost; task may already be billed: {detail}"
                            )),
                        ));
                    }
                    last = Some(provider("NETWORK", json!(detail)));
                    self.env.sleeper.pause(exponential_pause(slot.index)).await;
                    continue;
                }
                Err(TransportError::InvalidRequest(m)) => return Err(value_err(m)),
            }
        }
        Err(last
            .unwrap_or_else(|| provider("RETRY", json!(format!("{endpoint}: retries exhausted")))))
    }

    /// `limits()` — the remaining account credits (`/info {"query": "limits"}`).
    pub async fn limits(&self) -> Result<Value, ArsError> {
        let payload = self
            .post("info", &json!({"query": "limits"}), 5, false)
            .await?;
        attr_get(&payload, "limits_total")
    }

    /// `status()` — `/info {"query": "status"}`.
    pub async fn status(&self) -> Result<Value, ArsError> {
        self.post("info", &json!({"query": "status"}), 5, false)
            .await
    }

    /// `set_task(tools_name, data)` — create a task and immediately log its charge
    /// together with `task_id`. A `SET_TASK_OK` response is only useful for recovery if
    /// its `task_id` is a positive integer; a missing/unusable ID is journaled as a
    /// temporary entry and raises `INVALID_TASK_ID` instead.
    pub async fn set_task(&self, tools_name: &Value, data: &Value) -> Result<Value, ArsError> {
        let result = self
            .post(
                "set",
                &json!({"tools_name": tools_name, "data": data}),
                5,
                true,
            )
            .await?;
        let raw_task_id = attr_get(&result, "task_id")?;
        let cost = attr_get(&result, "cost")?;
        // Evaluate `cost` before `items`; the error precedence depends on it.
        let cost_f = py_float(&or_zero(&cost))?;
        let items = count_items(data)?;
        let Some(task_id) = valid_task_id(&raw_task_id) else {
            let mut extra = Map::new();
            extra.insert("temporary".to_owned(), json!(true));
            extra.insert("reason".to_owned(), json!("set_task_ok_missing_task_id"));
            extra.insert(
                "received_task_id".to_owned(),
                sanitize_for_journal(&raw_task_id),
            );
            self.env.record(&Record {
                source: SOURCE.to_owned(),
                operation: pystr::repr_value(tools_name),
                cost: Some(cost_f),
                unit: "limits".to_owned(),
                task_id: None,
                items: Some(items),
                extra: Some(extra),
                at: None,
            })?;
            return Err(ArsError::Provider(Box::new(ProviderError {
                code: "INVALID_TASK_ID".to_owned(),
                message: json!(format!(
                    "{}: SET_TASK_OK response had no usable task_id (received {}); cost {} was billed and journaled",
                    pystr::repr_value(tools_name),
                    pystr::repr(&raw_task_id),
                    pystr::repr(&cost),
                )),
                payload: Some(result),
            })));
        };
        self.env.record(&Record {
            source: SOURCE.to_owned(),
            operation: pystr::repr_value(tools_name),
            cost: Some(cost_f),
            unit: "limits".to_owned(),
            task_id: Some(json!(task_id)),
            items: Some(items),
            extra: None,
            at: None,
        })?;
        Ok(json!({"task_id": task_id, "cost": cost, "raw": result}))
    }

    /// `check(task_id)` — `(status, raw result)` for one `/check` call.
    pub async fn check(&self, task_id: &Value) -> Result<(Value, Value), ArsError> {
        let id = pystr::to_python_int(task_id).map_err(|(c, m)| pyerr(c, m))?;
        let result = self
            .post("check", &json!({"task_id": id}), 5, false)
            .await?;
        Ok((attr_get(&result, "status")?, result))
    }

    /// `get(task_id)` — fetch a result without another charge (paid at creation time).
    pub async fn get(&self, task_id: &Value) -> Result<Value, ArsError> {
        let id = pystr::to_python_int(task_id).map_err(|(c, m)| pyerr(c, m))?;
        self.post("get", &json!({"task_id": id}), 5, false).await
    }

    /// `delete(task_id)` — `/tasks {"action": "delete"}`.
    pub async fn delete(&self, task_id: &Value) -> Result<Value, ArsError> {
        let id = pystr::to_python_int(task_id).map_err(|(c, m)| pyerr(c, m))?;
        self.post(
            "tasks",
            &json!({"action": "delete", "task_id": id}),
            5,
            false,
        )
        .await
    }

    /// `restart(task_id)` — `/tasks {"action": "restart"}`.
    pub async fn restart(&self, task_id: &Value) -> Result<Value, ArsError> {
        let id = pystr::to_python_int(task_id).map_err(|(c, m)| pyerr(c, m))?;
        self.post(
            "tasks",
            &json!({"action": "restart", "task_id": id}),
            5,
            false,
        )
        .await
    }

    /// `wait(task_id, timeout=900, interval=8)` — poll until `finish`, a terminal
    /// failure, or the timeout. Every other state — `process`, `waiting`, `queue`, an
    /// empty status — means "keep waiting" so an already-paid result stays recoverable.
    pub async fn wait(
        &self,
        task_id: &Value,
        timeout: Option<&Value>,
        interval: Option<&Value>,
    ) -> Result<Value, ArsError> {
        let default_timeout = json!(900);
        let default_interval = json!(8);
        let timeout = timeout.unwrap_or(&default_timeout);
        let interval = interval.unwrap_or(&default_interval);
        let started = self.env.monotonic();
        loop {
            let now = self.env.monotonic();
            let Some(limit) = py_f64(timeout) else {
                return Err(type_err(format!(
                    "'<' not supported between instances of 'float' and '{}'",
                    pystr::type_name(timeout)
                )));
            };
            if now - started >= limit {
                break;
            }
            let (status, _result) = self.check(task_id).await?;
            if status.as_str() == Some("finish") {
                return self.get(task_id).await;
            }
            if matches!(
                status.as_str(),
                Some("error" | "failed" | "cancelled" | "canceled")
            ) {
                return Err(provider(
                    "STATUS",
                    json!(format!(
                        "task {}: status {}",
                        pystr::repr_value(task_id),
                        pystr::repr_value(&status)
                    )),
                ));
            }
            let Some(seconds) = py_f64(interval) else {
                return Err(type_err(format!(
                    "'{}' object cannot be interpreted as an integer or float",
                    pystr::type_name(interval)
                )));
            };
            self.env.sleeper.pause(seconds).await;
        }
        Err(provider(
            "TIMEOUT",
            json!(format!(
                "task {} did not finish within {}s; the result is already paid for, \
                 so retrieve it later with get({})",
                pystr::repr_value(task_id),
                pystr::repr_value(timeout),
                pystr::repr_value(task_id)
            )),
        ))
    }

    /// Create a task, wait for it, then fetch the result; returns the result and its cost.
    pub async fn run(
        &self,
        tools_name: &Value,
        data: &Value,
        timeout: Option<&Value>,
        interval: Option<&Value>,
    ) -> Result<(Value, Value), ArsError> {
        let task = self.set_task(tools_name, data).await?;
        let result = self
            .wait(&getitem(&task, "task_id")?, timeout, interval)
            .await?;
        Ok((result, getitem(&task, "cost")?))
    }
}

/// `refetch = get` — explicit alias for retrieving an already-paid result again.
impl<'e, T: Transport> Client<'e, T> {
    /// Fetches an already-paid task result again without starting a new task.
    pub async fn refetch(&self, task_id: &Value) -> Result<Value, ArsError> {
        self.get(task_id).await
    }
}
