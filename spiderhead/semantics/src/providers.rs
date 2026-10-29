//! Provider seams for paid stages (`DemandProvider`, `SerpProvider`,
//! `ArsenkinProvider`, `PageParser`).
//!
//! Paid stages reach their providers through these seams: stages
//! take a `&dyn` trait, so tests inject recorded responses and the real
//! HTTP clients (owned by `spiderhead/sources`) plug in later without changing
//! stage code.

use indexmap::IndexMap;
use serde_json::Value;
use std::collections::VecDeque;
use std::fmt;

/// Provider call failure with the fields the stages record:
/// the message, whether the outcome is ambiguous, and an optional payload
/// (Arsenkin `set_task` attaches the billed
/// receipt so the stage can journal a charged-but-unknown submission).
#[derive(Debug, Clone)]
pub struct ProviderError {
    /// Error message.
    pub message: String,
    /// Network failure whose outcome cannot be reconciled
    /// automatically, so the attempt is recorded `unknown`, not `retryable`.
    pub ambiguous: bool,
    /// Optional provider payload.
    pub payload: Option<Value>,
}

impl ProviderError {
    /// A plain retryable failure.
    pub fn retryable(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
            ambiguous: false,
            payload: None,
        }
    }

    /// A network failure with an unknown outcome.
    pub fn ambiguous(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
            ambiguous: true,
            payload: None,
        }
    }

    /// A failure carrying a provider payload (billed `set_task` receipt).
    pub fn with_payload(message: impl Into<String>, payload: Value) -> Self {
        Self {
            message: message.into(),
            ambiguous: false,
            payload: Some(payload),
        }
    }
}

impl fmt::Display for ProviderError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for ProviderError {}

impl From<ProviderError> for crate::Error {
    fn from(e: ProviderError) -> Self {
        crate::Error::Provider(e.message)
    }
}

/// Output of `Wordstat.expand`: the merged pool plus provenance meta, as
/// returned by the provider: `pool` keys are the provider's
/// normalized phrases in first-seen order, `origin` tags each pool key with
/// `"results"` or `"associations"`.
#[derive(Debug, Clone, Default)]
pub struct ExpandResult {
    /// `{normalized_phrase: base_count}` merged over results + associations.
    pub pool: IndexMap<String, i64>,
    /// `{normalized_phrase: "results"|"associations"}` provenance tags.
    pub origin: IndexMap<String, String>,
    /// Count of main Wordstat rows received.
    pub results: i64,
    /// Count of association rows received.
    pub associations: i64,
}

/// Wordstat seam: paid; counts one request per call.
pub trait DemandProvider {
    /// Expand `phrase` with `limit` and `regions`; returns `(pool, meta)`.
    fn expand(
        &mut self,
        phrase: &str,
        limit: i64,
        regions: &[String],
    ) -> Result<ExpandResult, ProviderError>;
    /// Top phrases for `phrase`: the raw body dict
    /// (the stage reads `body["totalCount"]` itself).
    fn top(&mut self, phrase: &str, limit: i64, regions: &[String])
        -> Result<Value, ProviderError>;
}

/// Batch SERP search seam: SERP documents per query.
pub trait SerpProvider {
    /// Search a batch of queries; returns per-query result
    /// dicts (`{"operation_id", "status", "docs", "error"}` shapes). A query
    /// absent from the returned map stands for an operation that never returned.
    fn search_batch(
        &mut self,
        queries: &[String],
        region: &str,
        groups: i64,
    ) -> Result<IndexMap<String, Value>, ProviderError>;
}

/// Task-queue seam: submit a batch, then wait for the result.
pub trait ArsenkinProvider {
    /// Submit a task; returns a receipt with `task_id` and `cost`.
    /// A billed-but-rejected submission surfaces as `ProviderError` with the
    /// receipt in `payload`.
    fn set_task(&mut self, tool: &str, payload: &Value) -> Result<Value, ProviderError>;
    /// Raw task result document, once the task is done.
    fn wait(&mut self, task_id: i64) -> Result<Value, ProviderError>;
    /// Whether the provider is configured at all (credentials present).
    fn available(&self) -> bool {
        true
    }
}

/// On-page signals mined from one competitor page (subset:
/// title, meta description, H1s and H2s).
#[derive(Debug, Clone, Default)]
pub struct PageParts {
    /// Page title.
    pub title: String,
    /// Meta description.
    pub meta: String,
    /// H1 headings.
    pub h1: Vec<String>,
    /// H2 headings.
    pub h2: Vec<String>,
}

/// Public web page fetch for `mine` (non-paid, still behind a seam).
pub trait PageParser {
    /// Fetch `url` into page parts; `Ok(None)` when unfetchable
    /// (the error is logged and contributes no text).
    fn fetch(&mut self, url: &str) -> Result<Option<PageParts>, ProviderError>;
}

// ---------------- recorded fakes (test path) ----------------

fn region_key(regions: &[String]) -> String {
    regions.join(",")
}

/// `expand`/`top` fake keyed by `(phrase, joined_regions)`; calls not in the
/// map error loudly so a run can never silently spend more than the
/// recorded run did.
#[derive(Debug, Default)]
pub struct FakeDemand {
    /// Recorded expansions: `(phrase, regions)` → queue of results (FIFO).
    pub expansions: IndexMap<(String, String), VecDeque<Result<ExpandResult, ProviderError>>>,
    /// Recorded `top` answers: `(phrase, regions)` → raw body dict.
    pub tops: IndexMap<(String, String), Result<Value, ProviderError>>,
    /// Log of `(method, phrase, regions)` calls for request-count parity.
    pub calls: Vec<(String, String, String)>,
}

impl FakeDemand {
    /// Fake serving the same expansion for every call of a phrase/region pair.
    pub fn with_expand(mut self, phrase: &str, regions: &str, r: ExpandResult) -> Self {
        self.expansions
            .entry((phrase.into(), regions.into()))
            .or_default()
            .push_back(Ok(r));
        self
    }

    /// Queue one expansion failure for a phrase/region pair.
    pub fn fail_expand(mut self, phrase: &str, regions: &str, e: ProviderError) -> Self {
        self.expansions
            .entry((phrase.into(), regions.into()))
            .or_default()
            .push_back(Err(e));
        self
    }

    /// Record a `top` answer for a phrase/region pair.
    pub fn with_top(mut self, phrase: &str, regions: &str, body: Value) -> Self {
        self.tops.insert((phrase.into(), regions.into()), Ok(body));
        self
    }

    /// Record a `top` failure for a phrase/region pair.
    pub fn fail_top(mut self, phrase: &str, regions: &str, e: ProviderError) -> Self {
        self.tops.insert((phrase.into(), regions.into()), Err(e));
        self
    }
}

impl DemandProvider for FakeDemand {
    fn expand(
        &mut self,
        phrase: &str,
        _limit: i64,
        regions: &[String],
    ) -> Result<ExpandResult, ProviderError> {
        let key = (phrase.to_string(), region_key(regions));
        self.calls
            .push(("expand".into(), key.0.clone(), key.1.clone()));
        match self.expansions.get_mut(&key) {
            Some(q) if q.len() > 1 => q.pop_front().unwrap_or_else(|| unreachable!()),
            Some(q) => match q.front() {
                Some(Ok(r)) => Ok(r.clone()),
                Some(Err(e)) => Err(e.clone()),
                None => unreachable!(),
            },
            None => Err(ProviderError::retryable(format!(
                "unrecorded wordstat call: {phrase}@{}",
                key.1
            ))),
        }
    }

    fn top(
        &mut self,
        phrase: &str,
        _limit: i64,
        regions: &[String],
    ) -> Result<Value, ProviderError> {
        let key = (phrase.to_string(), region_key(regions));
        self.calls
            .push(("top".into(), key.0.clone(), key.1.clone()));
        match self.tops.get(&key) {
            Some(Ok(v)) => Ok(v.clone()),
            Some(Err(e)) => Err(e.clone()),
            None => Err(ProviderError::retryable(format!(
                "unrecorded wordstat.top call: {phrase}@{}",
                key.1
            ))),
        }
    }
}

/// `search_batch` fake: `(phrase, region)` → result dict (`Value::Null` marks
/// a billed operation that never made it into the returned mapping).
#[derive(Debug, Default)]
pub struct FakeSerp {
    /// Recorded results by `(phrase, region)`; `Null` = absent from the map.
    pub responses: IndexMap<(String, String), Value>,
    /// Call log `(queries_joined_by_|, region, groups)`.
    pub calls: Vec<(Vec<String>, String, i64)>,
}

impl SerpProvider for FakeSerp {
    fn search_batch(
        &mut self,
        queries: &[String],
        region: &str,
        groups: i64,
    ) -> Result<IndexMap<String, Value>, ProviderError> {
        self.calls
            .push((queries.to_vec(), region.to_string(), groups));
        let mut out = IndexMap::new();
        for q in queries {
            match self.responses.get(&(q.clone(), region.to_string())) {
                Some(Value::Null) => {}
                Some(v) => {
                    out.insert(q.clone(), v.clone());
                }
                None => {
                    return Err(ProviderError::retryable(format!(
                        "unrecorded serp call: {q}@{region}"
                    )))
                }
            }
        }
        Ok(out)
    }
}

/// Arsenkin fake: `set_task` pops a response queue; `wait` pops per-task
/// queues (`"*"` fallback answers every task forever).
#[derive(Debug, Default)]
pub struct FakeArsenkin {
    /// Queued `set_task` outcomes (receipt `Value` or error).
    pub submits: VecDeque<Result<Value, ProviderError>>,
    /// `task_id` (or `"*"`) → queued `wait` answers.
    pub waits: IndexMap<String, VecDeque<Result<Value, ProviderError>>>,
    /// Submitted `(tool, payload)` calls.
    pub submitted: Vec<(String, Value)>,
    /// `wait` call log, one entry per call.
    pub calls: Vec<(String, i64)>,
    /// Whether `available()` reports configured.
    pub configured: bool,
}

impl FakeArsenkin {
    /// Fake answering every `set_task` with `receipt` and every `wait` with `raw`.
    pub fn ready(receipt: Value, raw: Value) -> Self {
        let mut f = Self {
            configured: true,
            ..Self::default()
        };
        f.submits.push_back(Ok(receipt));
        f.waits.insert("*".into(), VecDeque::from([Ok(raw)]));
        f
    }
}

impl ArsenkinProvider for FakeArsenkin {
    fn set_task(&mut self, tool: &str, payload: &Value) -> Result<Value, ProviderError> {
        self.submitted.push((tool.to_string(), payload.clone()));
        match self.submits.pop_front() {
            Some(Ok(v)) => Ok(v),
            Some(Err(e)) => Err(e),
            None => Err(ProviderError::retryable("unrecorded set_task call")),
        }
    }

    fn wait(&mut self, task_id: i64) -> Result<Value, ProviderError> {
        self.calls.push(("wait".to_string(), task_id));
        fn take(
            q: &mut VecDeque<Result<Value, ProviderError>>,
        ) -> Option<Result<Value, ProviderError>> {
            if q.len() > 1 {
                q.pop_front()
            } else {
                q.front().cloned()
            }
        }
        if let Some(q) = self.waits.get_mut(&task_id.to_string()) {
            if let Some(ans) = take(q) {
                return ans;
            }
        }
        if let Some(q) = self.waits.get_mut("*") {
            if let Some(ans) = take(q) {
                return ans;
            }
        }
        Err(ProviderError::retryable(format!(
            "unrecorded wait call: {task_id}"
        )))
    }

    fn available(&self) -> bool {
        self.configured
    }
}

/// Page-parser fake: `url` → parts or `None`.
#[derive(Debug, Default)]
pub struct FakePages {
    /// Recorded pages by URL.
    pub pages: IndexMap<String, Option<PageParts>>,
    /// Call log.
    pub calls: Vec<String>,
}

impl PageParser for FakePages {
    fn fetch(&mut self, url: &str) -> Result<Option<PageParts>, ProviderError> {
        self.calls.push(url.to_string());
        Ok(self.pages.get(url).cloned().flatten())
    }
}
