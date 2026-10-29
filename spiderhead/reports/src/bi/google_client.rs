//! Host-allowlisted Google Sheets and BigQuery REST clients for explicit, bounded BI delivery,
//! plus the exact host target configuration they resolve.
//!
//! The network surface sits behind two injected seams: a `TokenSupplier`
//! closure yields the OAuth bearer for the client's scope, and a [`Fetcher`]
//! performs one request and returns the decoded JSON body.  Tests inject fakes;
//! [`LiveFetcher`] is the production transport: no redirects, 30 s timeout.

use std::collections::HashMap;
use std::sync::{Arc, LazyLock, Mutex};

use serde_json::{json, Map, Value};
use spiderhead_core::envvar::read_env;
use spiderhead_core::host::{Host, SystemHost};

use super::state::{bi_err, uncertain, DestinationError};

/// Environment variable naming the host target configuration.
pub const HOST_CONFIG_ENV: &str = "SEOHEAD_BI_DESTINATIONS_FILE";

/// OAuth scope for Sheets.
pub const SHEETS_SCOPE: &str = "https://www.googleapis.com/auth/spreadsheets";

/// OAuth scope for BigQuery.
pub const BIGQUERY_SCOPE: &str = "https://www.googleapis.com/auth/bigquery";

/// Retry attempts for transient Google failures.
pub const GOOGLE_RETRIES: usize = 3;

/// Rows per Sheets request chunk.
pub const GOOGLE_CHUNK_ROWS: usize = 1_000;

/// Byte limit per request body.
pub const GOOGLE_REQUEST_BYTES: usize = 4 * 1024 * 1024;

/// Source-byte limit per chunk.
pub const GOOGLE_CHUNK_SOURCE_BYTES: usize = GOOGLE_REQUEST_BYTES / 2;

const RETRYABLE_CODES: [i64; 5] = [429, 500, 502, 503, 504];

/// Host-owned, already-authorized clients registered outside CLI/MCP JSON,
/// keyed by `(destination, target)`.
pub type HostClientMap = HashMap<(String, String), Arc<dyn DestinationClient>>;

pub static HOST_CLIENTS: LazyLock<Mutex<HostClientMap>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));

/// Register a host-owned client for one destination and target.
pub fn register_host_client(
    destination: &str,
    target: &str,
    client: Arc<dyn DestinationClient>,
) -> Result<(), DestinationError> {
    HOST_CLIENTS
        .lock()
        .map_err(|_| bi_err("host client registry is poisoned"))?
        .insert((destination.to_owned(), target.to_owned()), client);
    Ok(())
}

pub type TokenSupplier = Arc<dyn Fn(&str) -> Option<String> + Send + Sync>;

/// Sleep and jitter seams: production blocks and uses real randomness; tests
/// record instead of waiting.
pub type Sleeper = Arc<dyn Fn(f64) + Send + Sync>;
/// Random jitter term of the backoff delay.
pub type Jitter = Arc<dyn Fn() -> f64 + Send + Sync>;

/// Failure kinds a fetcher reports: a received non-2xx answer (HTTP error), a read timeout,
/// or a transport failure without any response.
#[derive(Debug)]
pub enum FetchError {
    /// An HTTP error response; `retry_after` mirrors `headers.get("Retry-After")`.
    Http {
        code: i64,
        retry_after: Option<String>,
    },
    /// The response timed out.
    Timeout,
    /// Any other transport-level failure.
    Transport(String),
}

/// One bounded request against the injected transport. The request carries
/// `method`, `url`, `body` (JSON value, or raw bytes as `{"__bytes__": [...]}`),
/// `authorization` and `headers`.
pub trait Fetcher: Send + Sync {
    /// Send `request`; the return value is the decoded JSON body.
    fn fetch(&self, request: &Value) -> Result<Value, FetchError>;
}

/// The production fetcher: a no-redirect HTTP client with a 30 second timeout.
/// Created per request batch; a dedicated current-thread runtime drives it so
/// callers stay synchronous.
pub struct LiveFetcher {
    runtime: tokio::runtime::Runtime,
    client: reqwest::Client,
}

impl LiveFetcher {
    /// Build the no-redirect transport; redirects are refused.
    pub fn new() -> Result<Self, DestinationError> {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .map_err(|e| bi_err(format!("Google transport runtime is unavailable: {e}")))?;
        let client = reqwest::Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .map_err(|e| bi_err(format!("Google transport is unavailable: {e}")))?;
        Ok(LiveFetcher { runtime, client })
    }
}

impl Fetcher for LiveFetcher {
    fn fetch(&self, request: &Value) -> Result<Value, FetchError> {
        let url = request
            .get("url")
            .and_then(Value::as_str)
            .unwrap_or_default();
        let method = request
            .get("method")
            .and_then(Value::as_str)
            .unwrap_or("GET");
        let method = reqwest::Method::from_bytes(method.as_bytes())
            .map_err(|e| FetchError::Transport(e.to_string()))?;
        let body = request.get("body");
        let encoded = match body {
            None | Some(Value::Null) => None,
            Some(Value::Object(m)) if m.contains_key("__bytes__") => {
                m.get("__bytes__").and_then(Value::as_array).map(|a| {
                    a.iter()
                        .filter_map(|b| b.as_u64().map(|n| n as u8))
                        .collect::<Vec<u8>>()
                })
            }
            Some(value) => serde_json::to_string(value).map(|s| s.into_bytes()).ok(),
        };
        if let Some(bytes) = &encoded {
            if bytes.len() > GOOGLE_REQUEST_BYTES {
                return Err(FetchError::Transport(
                    "request exceeds the bounded payload size".to_owned(),
                ));
            }
        }
        let headers = request.get("headers").and_then(Value::as_object);
        let authorization = request
            .get("authorization")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_owned();
        let client = self.client.clone();
        let method_owned = method.clone();
        let url_owned = url.to_owned();
        let encoded = encoded.clone();
        let header_pairs: Vec<(String, String)> = headers
            .map(|h| {
                h.iter()
                    .map(|(k, v)| (k.clone(), v.as_str().unwrap_or_default().to_owned()))
                    .collect()
            })
            .unwrap_or_default();
        let response = self.runtime.block_on(async move {
            let mut builder = client
                .request(method_owned, &url_owned)
                .timeout(std::time::Duration::from_secs(30))
                .header("Authorization", authorization)
                .header("Content-Type", "application/json");
            for (name, value) in header_pairs {
                builder = builder.header(name, value);
            }
            if let Some(bytes) = encoded {
                builder = builder.body(bytes);
            }
            let sent = builder.send().await.map_err(|e| {
                if e.is_timeout() {
                    FetchError::Timeout
                } else {
                    FetchError::Transport(e.to_string())
                }
            })?;
            let status = sent.status().as_u16() as i64;
            let retry_after = sent
                .headers()
                .get("retry-after")
                .and_then(|v| v.to_str().ok())
                .map(|s| s.to_owned());
            let body = sent
                .bytes()
                .await
                .map_err(|e| FetchError::Transport(e.to_string()))?;
            Ok::<_, FetchError>((status, retry_after, body))
        })?;
        let (status, retry_after, body) = response;
        if !(200..300).contains(&status) {
            return Err(FetchError::Http {
                code: status,
                retry_after,
            });
        }
        serde_json::from_slice(&body).map_err(|e| FetchError::Transport(e.to_string()))
    }
}

/// The destination surface `apply_with_client` and friends drive. Optional methods
/// are default trait methods returning `None`, so a client can decline reconciliation
/// support.
pub trait DestinationClient: Send + Sync {
    /// Destination name, or the type name when none is set.
    fn destination_name(&self) -> &str;
    /// Must return true before any write.
    fn authorize_target(&self, target: &str) -> bool;
    /// Non-secret target binding for checkpoints.
    fn checkpoint_identity(&self) -> Option<Value> {
        None
    }
    /// Open the remote transaction.
    fn begin(&self, _args: &BeginArgs<'_>) -> Result<Value, DestinationError> {
        Err(bi_err("destination client cannot begin"))
    }
    /// Recover a lost begin, or `None` when unsupported.
    fn reconcile_begin(
        &self,
        _args: &BeginArgs<'_>,
    ) -> Result<Option<BeginOutcome>, DestinationError> {
        Ok(None)
    }
    /// Write rows of one dataset into the transaction.
    fn write(
        &self,
        _transaction: &mut Value,
        _dataset: &str,
        _rows: &[Vec<String>],
    ) -> Result<(), DestinationError> {
        Err(bi_err("destination client cannot write"))
    }
    /// Pending write description; `None` means a generic pending record.
    fn pending_write(
        &self,
        _transaction: &Value,
        _dataset: &str,
        _rows: &[Vec<String>],
        _chunk: usize,
    ) -> Result<Option<Value>, DestinationError> {
        Ok(None)
    }
    /// Reconcile a pending write; `None` means the client cannot reconcile.
    fn reconcile_pending(
        &self,
        _transaction: &mut Value,
        _pending: &Value,
        _rows: &[Vec<String>],
    ) -> Result<Option<PendingOutcome>, DestinationError> {
        Ok(None)
    }
    /// Reconcile a commit; `None` means the client cannot reconcile.
    fn reconcile_commit(
        &self,
        _transaction: &mut Value,
    ) -> Result<Option<CommitOutcome>, DestinationError> {
        Ok(None)
    }
    /// Publish the transaction and return the publication dict.
    fn commit(&self, _transaction: &mut Value) -> Result<Value, DestinationError> {
        Err(bi_err("destination client cannot commit"))
    }
    /// Clean up an abandoned transaction; `None` means there is no cleanup hook.
    fn abort(&self, _transaction: &mut Value) -> Result<Option<()>, DestinationError> {
        Ok(None)
    }
}

/// Shared keyword arguments of begin and reconcile-begin.
pub struct BeginArgs<'a> {
    /// The configured target name.
    pub target: &'a str,
    /// `replace` or `append`.
    pub operation: &'a str,
    /// Schema version of the BI export.
    pub schema_version: &'a str,
    /// Declared datasets from the verified manifest.
    pub datasets: &'a Map<String, Value>,
    /// SHA-256 of the package manifest bytes.
    pub package_sha256: &'a str,
    /// True for a selected (`bi-filter`) projection package.
    pub selected_projection: bool,
}

/// Verdicts of begin reconciliation: not applied, uncertain, or a recovered transaction.
pub enum BeginOutcome {
    /// The prior begin never landed; a fresh begin may run.
    NotApplied,
    /// The remote state cannot prove either way.
    Uncertain,
    /// The recovered transaction dict.
    Transaction(Value),
}

/// Verdicts of pending-write reconciliation.
pub enum PendingOutcome {
    /// The remote range/job already holds these rows.
    Applied,
    /// Nothing landed; the chunk may be replayed.
    NotApplied,
}

/// Verdicts of commit reconciliation.
pub enum CommitOutcome {
    /// The publish already happened.
    Applied,
    /// The publish never landed; retry is safe.
    NotApplied,
}

fn cfg_err(message: &str) -> DestinationError {
    bi_err(message.to_owned())
}

/// Require the dataset mapping to be a JSON object.
pub fn require_dataset_mapping(
    supplied: &Value,
    datasets: &Map<String, Value>,
    label: &str,
    id_name: &str,
    allow_extra: bool,
) -> Result<Map<String, Value>, DestinationError> {
    let supplied_map = supplied.as_object();
    let valid = match supplied_map {
        Some(m) => {
            datasets.keys().all(|k| m.contains_key(k)) && (allow_extra || m.len() == datasets.len())
        }
        None => false,
    };
    if !valid {
        return Err(cfg_err(&format!(
            "{label} mapping must name every BI package dataset exactly once"
        )));
    }
    let supplied_map = supplied_map.cloned().unwrap_or_default();
    let mut mapped = Map::new();
    for name in datasets.keys() {
        let value = supplied_map.get(name).cloned().unwrap_or(Value::Null);
        let ok = value
            .as_object()
            .map(|v| {
                v.get(id_name)
                    .map(|id| id.is_string() || id.is_i64() || id.is_u64())
                    .unwrap_or(false)
            })
            .unwrap_or(false);
        if !ok {
            return Err(cfg_err(&format!("{label} mapping for {name:?} is invalid")));
        }
        mapped.insert(name.clone(), value);
    }
    Ok(mapped)
}

/// Parse the HTTP-date form of `Retry-After` (IMF-fixdate, a GMT timestamp) into Unix seconds.
fn parse_http_date(value: &str) -> Option<f64> {
    let value = value.trim();
    let comma = value.find(',')?;
    let rest = value[comma + 1..].trim();
    let mut parts = rest.split_whitespace();
    let day: i64 = parts.next()?.parse().ok()?;
    let month = match parts.next()? {
        "Jan" => 1,
        "Feb" => 2,
        "Mar" => 3,
        "Apr" => 4,
        "May" => 5,
        "Jun" => 6,
        "Jul" => 7,
        "Aug" => 8,
        "Sep" => 9,
        "Oct" => 10,
        "Nov" => 11,
        "Dec" => 12,
        _ => return None,
    };
    let year: i64 = parts.next()?.parse().ok()?;
    let clock = parts.next()?;
    let mut hms = clock.split(':');
    let hour: i64 = hms.next()?.parse().ok()?;
    let minute: i64 = hms.next()?.parse().ok()?;
    let second: i64 = hms.next()?.parse().ok()?;
    // Days since 1970-01-01, from a civil date.
    let y = if month <= 2 { year - 1 } else { year };
    let era = if y >= 0 { y } else { y - 399 } / 400;
    let yoe = y - era * 400;
    let mp = (month + 9) % 12;
    let doy = (153 * mp + 2) / 5 + day - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    let days = era * 146097 + doe - 719468;
    Some((days * 86400 + hour * 3600 + minute * 60 + second) as f64)
}

/// Exponential backoff with jitter, honouring a `Retry-After` hint.
pub fn google_backoff(
    attempt: usize,
    retry_after: Option<&str>,
    sleeper: &Sleeper,
    jitter: &Jitter,
    now: f64,
) -> Result<(), DestinationError> {
    let mut delay = 2f64.powi(attempt as i32) + jitter();
    if let Some(raw) = retry_after {
        if !raw.is_empty() {
            let seconds = match raw.parse::<f64>() {
                Ok(s) => Some(s),
                Err(_) => parse_http_date(raw).map(|ts| ts - now),
            };
            if let Some(seconds) = seconds.filter(|s| s.is_finite()) {
                if seconds > 60.0 {
                    return Err(uncertain(
                        "Google Retry-After exceeds the 60-second wait budget; reconcile later",
                    ));
                }
                delay = delay.max(seconds);
            }
        }
    }
    sleeper(delay);
    Ok(())
}

fn default_sleeper() -> Sleeper {
    Arc::new(|seconds| {
        if seconds.is_finite() && seconds > 0.0 {
            std::thread::sleep(std::time::Duration::from_secs_f64(seconds));
        }
    })
}

fn default_jitter() -> Jitter {
    Arc::new(|| {
        // Uniform random value in [0, 1).
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.subsec_nanos() as u64 ^ d.as_secs())
            .unwrap_or(0);
        let addr = &nanos as *const u64 as usize as u64;
        let mixed = nanos.wrapping_mul(6364136223846793005).wrapping_add(addr);
        ((mixed >> 11) as f64) / (1u64 << 53) as f64
    })
}

fn now_unix() -> f64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs_f64())
        .unwrap_or(0.0)
}

/// Shared bounded retry and response boundary for Google REST calls.
pub struct GoogleRestClient {
    scope: &'static str,
    /// Token supplier; required, since no default credential path is available here;
    /// a missing supplier fails like an empty token.
    pub token_supplier: Option<TokenSupplier>,
    /// Fetcher; `None` uses the live no-redirect transport.
    pub fetcher: Option<Arc<dyn Fetcher>>,
    /// Sleep seam for backoff and polling.
    pub sleeper: Sleeper,
    /// Random seam for backoff jitter.
    pub jitter: Jitter,
    live: std::sync::OnceLock<Result<LiveFetcher, String>>,
}

impl GoogleRestClient {
    /// Construct with scope, token supplier and optional fetcher.
    pub fn new(
        scope: &'static str,
        token_supplier: Option<TokenSupplier>,
        fetcher: Option<Arc<dyn Fetcher>>,
    ) -> Self {
        GoogleRestClient {
            scope,
            token_supplier,
            fetcher,
            sleeper: default_sleeper(),
            jitter: default_jitter(),
            live: std::sync::OnceLock::new(),
        }
    }

    fn token(&self) -> Result<String, DestinationError> {
        let token = match &self.token_supplier {
            Some(supplier) => supplier(self.scope),
            None => None,
        };
        match token {
            Some(token) if !token.is_empty() => Ok(token),
            _ => Err(bi_err(
                "Google service-account token supplier returned no token",
            )),
        }
    }

    fn fetch(&self, request: &Value) -> Result<Value, FetchError> {
        if let Some(fetcher) = &self.fetcher {
            return fetcher.fetch(request);
        }
        // Body encoding and the request size bound are applied before transport;
        // the live fetcher enforces both itself.
        match self
            .live
            .get_or_init(|| LiveFetcher::new().map_err(|e| e.to_string()))
        {
            Ok(live) => live.fetch(request),
            Err(message) => Err(FetchError::Transport(message.clone())),
        }
    }

    /// Send one request, with bounded retries when `retryable` is set.
    ///
    /// `body` is a JSON value; raw bytes travel as
    /// `{"__bytes__": [u8, ...]}` so the fetcher can rebuild them losslessly.
    pub fn request(
        &self,
        method: &str,
        url: &str,
        body: Option<&Value>,
        headers: Option<&Map<String, Value>>,
        retryable: bool,
    ) -> Result<Value, DestinationError> {
        let mut attempt = 0usize;
        loop {
            let token = self.token()?;
            let request = json!({
                "method": method,
                "url": url,
                "body": body.cloned().unwrap_or(Value::Null),
                "authorization": format!("Bearer {token}"),
                "headers": headers.cloned().unwrap_or_default(),
            });
            let response = match self.fetch(&request) {
                Ok(response) => response,
                Err(FetchError::Http { code, retry_after }) => {
                    if retryable && RETRYABLE_CODES.contains(&code) && attempt < GOOGLE_RETRIES {
                        google_backoff(
                            attempt,
                            retry_after.as_deref(),
                            &self.sleeper,
                            &self.jitter,
                            now_unix(),
                        )?;
                        attempt += 1;
                        continue;
                    }
                    if RETRYABLE_CODES.contains(&code) {
                        return Err(uncertain(format!(
                            "Google API is temporarily unavailable (HTTP {code}); reconcile before retrying"
                        ))
                        );
                    }
                    return Err(bi_err(format!(
                        "Google API rejected the request with HTTP {code}"
                    )));
                }
                Err(FetchError::Timeout) => {
                    return Err(uncertain(
                        "Google response timed out; commit state is unknown",
                    ));
                }
                Err(FetchError::Transport(_)) => {
                    return Err(uncertain(
                        "Google transport failed; commit state is unknown",
                    ));
                }
            };
            let Some(map) = response.as_object() else {
                return Err(bi_err("Google API response is invalid"));
            };
            let error = map.get("error");
            let code = error
                .and_then(|e| e.as_object())
                .and_then(|e| e.get("code"))
                .and_then(Value::as_i64);
            if retryable
                && code.map(|c| RETRYABLE_CODES.contains(&c)).unwrap_or(false)
                && attempt < GOOGLE_RETRIES
            {
                google_backoff(attempt, None, &self.sleeper, &self.jitter, now_unix())?;
                attempt += 1;
                continue;
            }
            if code.map(|c| RETRYABLE_CODES.contains(&c)).unwrap_or(false) {
                return Err(uncertain(format!(
                    "Google API is temporarily unavailable (HTTP {}); reconcile before retrying",
                    code.unwrap_or_default()
                )));
            }
            if error.filter(|e| crate::py::truthy(Some(e))).is_some() {
                return Err(bi_err(format!(
                    "Google API rejected the request: {}",
                    crate::py::repr(error.unwrap_or(&Value::Null))
                )));
            }
            return Ok(response);
        }
    }
}

/// Build the Sheets destination mapping.
pub fn sheets_mapping(
    supplied: &Value,
    datasets: &Map<String, Value>,
    allow_extra: bool,
) -> Result<Map<String, Value>, DestinationError> {
    let mapping = require_dataset_mapping(
        supplied,
        datasets,
        "Google Sheets worksheet",
        "worksheet_id",
        allow_extra,
    )?;
    for (name, value) in &mapping {
        let worksheet_id = value.get("worksheet_id");
        let title = value.get("worksheet_title");
        let ok = worksheet_id
            .map(|v| v.as_i64().map(|i| i >= 0).unwrap_or(false) && !v.is_boolean())
            .unwrap_or(false)
            && title
                .and_then(Value::as_str)
                .map(|t| !t.is_empty())
                .unwrap_or(false);
        if !ok {
            return Err(cfg_err(&format!(
                "Google Sheets worksheet mapping for {name:?} is invalid"
            )));
        }
    }
    for key in ["worksheet_id", "worksheet_title"] {
        let distinct: std::collections::BTreeSet<String> = mapping
            .values()
            .map(|v| v.get(key).cloned().unwrap_or(Value::Null).to_string())
            .collect();
        if distinct.len() != mapping.len() {
            return Err(cfg_err(
                "Google Sheets datasets must map to distinct worksheet IDs/titles",
            ));
        }
    }
    Ok(mapping)
}

/// Decode a formula-safe CSV cell into its declared type.
pub fn bq_value(value: &str, field_type: &str) -> Result<Value, DestinationError> {
    if value.is_empty() {
        return Ok(Value::Null);
    }
    let invalid = || {
        cfg_err(&format!(
            "BI CSV value is invalid for declared {field_type:?} field"
        ))
    };
    match field_type {
        "boolean" => match value {
            "true" => Ok(Value::Bool(true)),
            "false" => Ok(Value::Bool(false)),
            _ => Err(invalid()),
        },
        "integer" => value.parse::<i64>().map(Value::from).map_err(|_| invalid()),
        "number" => value
            .parse::<f64>()
            // serde_json has no non-finite number. Non-finite text can only come from
            // hand-edited CSVs, and the Sheets finite check rejects it.
            .map(|f| serde_json::Number::from_f64(f).map_or(Value::Null, Value::Number))
            .map_err(|_| invalid()),
        "json" => serde_json::from_str(value).map_err(|_| invalid()),
        _ => Ok(Value::String(value.to_owned())),
    }
}

/// BigQuery type for a declared BI field.
pub fn bq_type(field_type: &str) -> &'static str {
    match field_type {
        "boolean" => "BOOL",
        "integer" => "INT64",
        "number" => "FLOAT64",
        "date" => "DATE",
        "json" => "JSON",
        "string" => "STRING",
        _ => "",
    }
}

/// Convert a zero-based column index to an A1-style column name.
pub fn a1_column(width: i64) -> Result<String, DestinationError> {
    if width < 1 {
        return Err(cfg_err("Google Sheets requires at least one column"));
    }
    let mut width = width;
    let mut letters = String::new();
    while width > 0 {
        width -= 1;
        let remainder = width % 26;
        letters.insert(0, (b'A' + remainder as u8) as char);
        width /= 26;
    }
    Ok(letters)
}

/// Sheets cell values: literal text plus decoded numeric and boolean cells.
pub fn sheets_values(
    transaction: &Value,
    dataset: &str,
    rows: &[Vec<String>],
) -> Result<Vec<Vec<Value>>, DestinationError> {
    let fields = transaction
        .get("fields")
        .and_then(|f| f.get(dataset))
        .and_then(Value::as_array)
        .filter(|f| !f.is_empty())
        .cloned()
        .ok_or_else(|| cfg_err("Sheets checkpoint has no typed fields; review its staged data"))?;
    if rows.iter().any(|row| row.len() != fields.len()) {
        return Err(cfg_err(
            "Google Sheets row width differs from the declared header",
        ));
    }
    let written = transaction
        .get("rows")
        .and_then(|r| r.get(dataset))
        .and_then(Value::as_i64)
        .unwrap_or(0);
    if written == 0 {
        let header: Vec<String> = fields
            .iter()
            .map(|f| {
                f.get("name")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_owned()
            })
            .collect();
        if rows.iter().map(|r| r.as_slice()).collect::<Vec<_>>() != vec![header.as_slice()] {
            return Err(cfg_err(
                "Google Sheets header differs from the declared BI schema",
            ));
        }
        return Ok(rows
            .iter()
            .map(|r| r.iter().map(|v| Value::String(v.clone())).collect())
            .collect());
    }
    let mut values = Vec::with_capacity(rows.len());
    for row in rows {
        let mut decoded = Vec::with_capacity(row.len());
        for (value, field) in row.iter().zip(fields.iter()) {
            let kind = field
                .get("type")
                .and_then(Value::as_str)
                .unwrap_or_default();
            let cell: Value =
                if !value.is_empty() && matches!(kind, "boolean" | "integer" | "number") {
                    bq_value(value, kind)?
                } else {
                    Value::String(value.clone())
                };
            if kind == "integer" && !cell.is_null() {
                if let Some(i) = cell.as_i64() {
                    if i.unsigned_abs() > (1u64 << 53) - 1 {
                        return Err(cfg_err("BI integer exceeds exact Sheets numeric precision"));
                    }
                }
            }
            // Non-empty text that is not a finite number stays a string; serde_json
            // cannot hold a non-finite float, so the check re-reads the source string.
            if kind == "number"
                && !value.is_empty()
                && value
                    .parse::<f64>()
                    .map(|f| !f.is_finite())
                    .unwrap_or(false)
            {
                return Err(cfg_err("BI number must be finite for Google Sheets"));
            }
            decoded.push(cell);
        }
        values.push(decoded);
    }
    Ok(values)
}

/// Read-back check: Google omits empty trailing cells and rows, so they are ignored.
pub fn sheets_readback_matches(actual: &Value, expected: &[Vec<Value>]) -> bool {
    let Some(actual_rows) = actual.as_array() else {
        return false;
    };
    if actual_rows.len() > expected.len() {
        return false;
    }
    for (index, row) in expected.iter().enumerate() {
        let observed = actual_rows.get(index).and_then(Value::as_array);
        let empty: Vec<Value> = Vec::new();
        let observed = observed.unwrap_or(&empty);
        if observed.len() > row.len() {
            return false;
        }
        for (column, value) in row.iter().enumerate() {
            let cell = observed
                .get(column)
                .cloned()
                .unwrap_or(Value::String(String::new()));
            if cell != *value || cell.is_boolean() != value.is_boolean() {
                return false;
            }
        }
    }
    true
}

fn stage_title(name: &str, nonce: &str) -> String {
    format!("__seohead_stage_{name}_{nonce}")
}

fn sheets_stage_url(spreadsheet_id: &str) -> String {
    format!("https://sheets.googleapis.com/v4/spreadsheets/{spreadsheet_id}")
}

fn sheets_properties_url(spreadsheet_id: &str, fields: &str) -> String {
    format!(
        "{base}?fields={fields}",
        base = sheets_stage_url(spreadsheet_id)
    )
}

/// Host-owned Sheets client: replaces a worksheet while keeping each configured worksheet ID,
/// through a staged copy and paste publication.
pub struct GoogleSheetsClient {
    rest: GoogleRestClient,
    /// Destination name, always `sheets`.
    pub destination: &'static str,
    /// Configured target name.
    pub target: String,
    /// Spreadsheet ID from the host configuration.
    pub spreadsheet_id: String,
    /// Dataset name -> `{"worksheet_id": int, "worksheet_title": str}`.
    pub worksheets: Map<String, Value>,
}

impl GoogleSheetsClient {
    /// Construct with target, spreadsheet ID, worksheets and options.
    pub fn new(
        target: impl Into<String>,
        spreadsheet_id: impl Into<String>,
        worksheets: Map<String, Value>,
        token_supplier: Option<TokenSupplier>,
        fetcher: Option<Arc<dyn Fetcher>>,
    ) -> Self {
        GoogleSheetsClient {
            rest: GoogleRestClient::new(SHEETS_SCOPE, token_supplier, fetcher),
            destination: "sheets",
            target: target.into(),
            spreadsheet_id: spreadsheet_id.into(),
            worksheets,
        }
    }

    /// Mutable access to the REST boundary, for backoff and polling test seams.
    pub fn rest_mut(&mut self) -> &mut GoogleRestClient {
        &mut self.rest
    }

    fn request(
        &self,
        method: &str,
        url: &str,
        body: Option<&Value>,
        headers: Option<&Map<String, Value>>,
        retryable: bool,
    ) -> Result<Value, DestinationError> {
        self.rest.request(method, url, body, headers, retryable)
    }
}

/// Stage one hidden sheet per dataset after a capacity preflight; the returned map is the durable
/// transaction.
fn sheets_begin(
    client: &GoogleSheetsClient,
    args: &BeginArgs<'_>,
) -> Result<Value, DestinationError> {
    if args.operation != "replace" {
        return Err(cfg_err(
            "Google Sheets append is unavailable: use replace so every package is reconciled",
        ));
    }
    let mapping = sheets_mapping(
        &Value::Object(client.worksheets.clone()),
        args.datasets,
        args.selected_projection,
    )?;
    let metadata = client.request(
        "GET",
        &sheets_properties_url(
            &client.spreadsheet_id,
            "sheets.properties(sheetId,title,gridProperties(rowCount,columnCount))",
        ),
        None,
        None,
        true,
    )?;
    let properties: Vec<Value> = metadata
        .get("sheets")
        .and_then(Value::as_array)
        .map(|items| {
            items
                .iter()
                .filter_map(|item| item.get("properties").cloned())
                .collect()
        })
        .unwrap_or_default();
    let mut existing: HashMap<(i64, String), Value> = HashMap::new();
    for item in &properties {
        if let (Some(id), Some(title)) = (
            item.get("sheetId").and_then(Value::as_i64),
            item.get("title").and_then(Value::as_str),
        ) {
            existing.insert(
                (id, title.to_owned()),
                item.get("gridProperties").cloned().unwrap_or(Value::Null),
            );
        }
    }
    let missing = mapping.values().any(|value| {
        let id = value
            .get("worksheet_id")
            .and_then(Value::as_i64)
            .unwrap_or(-1);
        let title = value
            .get("worksheet_title")
            .and_then(Value::as_str)
            .unwrap_or_default();
        !existing.contains_key(&(id, title.to_owned()))
    });
    if missing {
        return Err(cfg_err(
            "Google Sheets target does not contain every configured worksheet ID/title",
        ));
    }
    let mut existing_cells: i64 = 0;
    for grid in existing.values() {
        let (rows, cols) = (
            grid.get("rowCount").and_then(Value::as_i64),
            grid.get("columnCount").and_then(Value::as_i64),
        );
        match (rows, cols) {
            (Some(r), Some(c)) => existing_cells += r * c,
            _ => return Err(cfg_err("Google Sheets target capacity is unavailable")),
        }
    }
    let stage_cells: i64 = args
        .datasets
        .values()
        .map(|d| {
            (d.get("row_count").and_then(Value::as_i64).unwrap_or(0) + 1)
                * d.get("fields")
                    .and_then(Value::as_array)
                    .map(|f| f.len() as i64)
                    .unwrap_or(0)
        })
        .sum();
    let mut target_grids = Map::new();
    let mut growth_cells: i64 = 0;
    for (name, value) in &mapping {
        let id = value
            .get("worksheet_id")
            .and_then(Value::as_i64)
            .unwrap_or(-1);
        let title = value
            .get("worksheet_title")
            .and_then(Value::as_str)
            .unwrap_or_default();
        let grid = existing
            .get(&(id, title.to_owned()))
            .cloned()
            .unwrap_or(Value::Null);
        let (rows, cols) = (
            grid.get("rowCount").and_then(Value::as_i64).unwrap_or(0),
            grid.get("columnCount").and_then(Value::as_i64).unwrap_or(0),
        );
        let dataset = &args.datasets[name];
        let need_rows = dataset
            .get("row_count")
            .and_then(Value::as_i64)
            .unwrap_or(0)
            + 1;
        let need_cols = dataset
            .get("fields")
            .and_then(Value::as_array)
            .map(|f| f.len() as i64)
            .unwrap_or(0);
        let grown = json!({
            "rowCount": rows.max(need_rows),
            "columnCount": cols.max(need_cols),
        });
        growth_cells += grown["rowCount"].as_i64().unwrap_or(0)
            * grown["columnCount"].as_i64().unwrap_or(0)
            - rows * cols;
        target_grids.insert(name.clone(), grown);
    }
    if existing_cells + stage_cells + growth_cells > crate::bi_destinations::sheets_max_cells() {
        return Err(cfg_err(
            "Google Sheets target cannot hold the required staging cells",
        ));
    }
    let nonce = &args.package_sha256[..args.package_sha256.len().min(12)];
    let requests: Vec<Value> = args
        .datasets
        .iter()
        .map(|(name, dataset)| {
            json!({
                "addSheet": {
                    "properties": {
                        "title": stage_title(name, nonce),
                        "gridProperties": {
                            "rowCount": dataset.get("row_count").and_then(Value::as_i64).unwrap_or(0) + 1,
                            "columnCount": dataset.get("fields").and_then(Value::as_array).map(|f| f.len()).unwrap_or(0),
                        },
                    }
                }
            })
        })
        .collect();
    let response = client.request(
        "POST",
        &format!("{}:batchUpdate", sheets_stage_url(&client.spreadsheet_id)),
        Some(&json!({"requests": requests})),
        None,
        false,
    )?;
    let replies = response.get("replies").and_then(Value::as_array);
    let mut stages = Map::new();
    for (index, name) in args.datasets.keys().enumerate() {
        let stage = replies
            .and_then(|r| r.get(index))
            .and_then(|r| r.get("addSheet"))
            .and_then(|r| r.get("properties"))
            .cloned()
            .unwrap_or(Value::Null);
        let valid = stage
            .get("sheetId")
            .map(|v| v.is_i64() || v.is_u64())
            .unwrap_or(false)
            && stage.get("title").map(Value::is_string).unwrap_or(false);
        if !valid {
            return Err(cfg_err("Google Sheets staging response is invalid"));
        }
        stages.insert(name.clone(), stage);
    }
    Ok(json!({
        "target": args.target,
        "schema_version": args.schema_version,
        "package_sha256": args.package_sha256,
        "stages": Value::Object(stages),
        "worksheets": Value::Object(mapping),
        "fields": args.datasets.iter().map(|(n, d)| (n.clone(), d.get("fields").cloned().unwrap_or(Value::Null))).collect::<Map<_, _>>(),
        "target_grids": Value::Object(target_grids),
        "headers": {},
        "rows": args.datasets.keys().map(|n| (n.clone(), json!(0))).collect::<Map<_, _>>(),
    }))
}

/// Recover deterministic stage sheets after a lost begin.
fn sheets_reconcile_begin(
    client: &GoogleSheetsClient,
    args: &BeginArgs<'_>,
) -> Result<BeginOutcome, DestinationError> {
    if args.operation != "replace" {
        return Err(cfg_err("Google Sheets append is unavailable: use replace"));
    }
    let mapping = sheets_mapping(
        &Value::Object(client.worksheets.clone()),
        args.datasets,
        args.selected_projection,
    )?;
    let metadata = client.request(
        "GET",
        &sheets_properties_url(
            &client.spreadsheet_id,
            "sheets.properties(sheetId,title,gridProperties(rowCount,columnCount))",
        ),
        None,
        None,
        true,
    )?;
    let properties: Vec<Value> = metadata
        .get("sheets")
        .and_then(Value::as_array)
        .map(|items| {
            items
                .iter()
                .filter_map(|item| item.get("properties").cloned())
                .collect()
        })
        .unwrap_or_default();
    let nonce = &args.package_sha256[..args.package_sha256.len().min(12)];
    let mut stages = Map::new();
    for name in args.datasets.keys() {
        let title = stage_title(name, nonce);
        let found = properties
            .iter()
            .find(|item| item.get("title").and_then(Value::as_str) == Some(title.as_str()))
            .cloned()
            .unwrap_or(Value::Null);
        stages.insert(name.clone(), found);
    }
    let present: Vec<&Value> = stages.values().filter(|s| !s.is_null()).collect();
    if present.is_empty() {
        return Ok(BeginOutcome::NotApplied);
    }
    if present.len() != args.datasets.len()
        || present.iter().any(|stage| {
            !stage
                .get("sheetId")
                .map(|v| v.is_i64() || v.is_u64())
                .unwrap_or(false)
                || !stage.get("title").map(Value::is_string).unwrap_or(false)
        })
    {
        return Ok(BeginOutcome::Uncertain);
    }
    let targets: HashMap<(i64, String), &Value> = properties
        .iter()
        .filter_map(|item| {
            match (
                item.get("sheetId").and_then(Value::as_i64),
                item.get("title").and_then(Value::as_str),
            ) {
                (Some(id), Some(title)) => Some(((id, title.to_owned()), item)),
                _ => None,
            }
        })
        .collect();
    let mut target_grids = Map::new();
    for (name, value) in &mapping {
        let id = value
            .get("worksheet_id")
            .and_then(Value::as_i64)
            .unwrap_or(-1);
        let title = value
            .get("worksheet_title")
            .and_then(Value::as_str)
            .unwrap_or_default();
        let grid = targets
            .get(&(id, title.to_owned()))
            .and_then(|item| item.get("gridProperties"))
            .cloned()
            .unwrap_or(Value::Null);
        let (rows, cols) = (
            grid.get("rowCount").and_then(Value::as_i64),
            grid.get("columnCount").and_then(Value::as_i64),
        );
        let (Some(rows), Some(cols)) = (rows, cols) else {
            return Ok(BeginOutcome::Uncertain);
        };
        let dataset = &args.datasets[name];
        target_grids.insert(
            name.clone(),
            json!({
                "rowCount": rows.max(dataset.get("row_count").and_then(Value::as_i64).unwrap_or(0) + 1),
                "columnCount": cols.max(dataset.get("fields").and_then(Value::as_array).map(|f| f.len() as i64).unwrap_or(0)),
            }),
        );
    }
    Ok(BeginOutcome::Transaction(json!({
        "target": args.target,
        "schema_version": args.schema_version,
        "package_sha256": args.package_sha256,
        "stages": Value::Object(stages),
        "worksheets": Value::Object(mapping),
        "fields": args.datasets.iter().map(|(n, d)| (n.clone(), d.get("fields").cloned().unwrap_or(Value::Null))).collect::<Map<_, _>>(),
        "target_grids": Value::Object(target_grids),
        "headers": {},
        "rows": args.datasets.keys().map(|n| (n.clone(), json!(0))).collect::<Map<_, _>>(),
    })))
}

impl DestinationClient for GoogleSheetsClient {
    fn destination_name(&self) -> &str {
        self.destination
    }

    fn authorize_target(&self, target: &str) -> bool {
        target == self.target
    }

    fn checkpoint_identity(&self) -> Option<Value> {
        Some(json!({
            "spreadsheet_id": self.spreadsheet_id,
            "worksheets": Value::Object(self.worksheets.clone()),
        }))
    }

    fn begin(&self, args: &BeginArgs<'_>) -> Result<Value, DestinationError> {
        sheets_begin(self, args)
    }

    fn reconcile_begin(
        &self,
        args: &BeginArgs<'_>,
    ) -> Result<Option<BeginOutcome>, DestinationError> {
        sheets_reconcile_begin(self, args).map(Some)
    }

    fn write(
        &self,
        transaction: &mut Value,
        dataset: &str,
        rows: &[Vec<String>],
    ) -> Result<(), DestinationError> {
        if transaction
            .get("stages")
            .and_then(|s| s.get(dataset))
            .is_none()
        {
            return Err(cfg_err("Google Sheets write named an undeclared dataset"));
        }
        if rows.is_empty() {
            return Ok(());
        }
        let values = sheets_values(transaction, dataset, rows)?;
        let header = transaction
            .get("headers")
            .and_then(|h| h.get(dataset))
            .cloned()
            .filter(|h| !h.is_null());
        let header = match header {
            None => {
                let header =
                    Value::Array(rows[0].iter().map(|r| Value::String(r.clone())).collect());
                if let Some(headers) = transaction
                    .get_mut("headers")
                    .and_then(Value::as_object_mut)
                {
                    headers.insert(dataset.to_owned(), header.clone());
                }
                rows[0].clone()
            }
            Some(_) => rows[0].clone(),
        };
        if transaction
            .get("headers")
            .and_then(|h| h.get(dataset))
            .and_then(Value::as_array)
            .is_some()
            && rows[0].len() != header.len()
        {
            // Redundant with the length check below; kept for clarity.
        }
        if rows.iter().any(|row| row.len() != header.len()) {
            return Err(cfg_err(
                "Google Sheets row width differs from the declared header",
            ));
        }
        let stage = transaction["stages"][dataset].clone();
        let title = stage
            .get("title")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .replace('\'', "''");
        let prior = transaction
            .get("rows")
            .and_then(|r| r.get(dataset))
            .and_then(Value::as_i64)
            .unwrap_or(0);
        let start = prior + 1;
        let end = prior + rows.len() as i64;
        let width = a1_column(header.len() as i64)?;
        let body = json!({
            "valueInputOption": "RAW",
            "data": [{
                "range": format!("'{title}'!A{start}:{width}{end}"),
                "majorDimension": "ROWS",
                "values": values,
            }],
        });
        let response = self.request(
            "POST",
            &format!(
                "https://sheets.googleapis.com/v4/spreadsheets/{}/values:batchUpdate",
                self.spreadsheet_id
            ),
            Some(&body),
            None,
            true,
        )?;
        if response.get("totalUpdatedRows").and_then(Value::as_i64) != Some(rows.len() as i64) {
            return Err(cfg_err(
                "Google Sheets did not acknowledge every staged row",
            ));
        }
        if let Some(rows_map) = transaction.get_mut("rows").and_then(Value::as_object_mut) {
            rows_map.insert(dataset.to_owned(), json!(prior + rows.len() as i64));
        }
        let readback = self.request(
            "GET",
            &format!(
                "https://sheets.googleapis.com/v4/spreadsheets/{}/values/'{}'!A{}:{}{}?valueRenderOption=UNFORMATTED_VALUE",
                self.spreadsheet_id, title, start, width, end
            ),
            None,
            None,
            true,
        )?;
        let actual = readback.get("values").cloned().unwrap_or(json!([]));
        if !sheets_readback_matches(&actual, &values) {
            return Err(cfg_err(
                "Google Sheets staged-row readback does not conserve values",
            ));
        }
        Ok(())
    }

    fn pending_write(
        &self,
        transaction: &Value,
        dataset: &str,
        rows: &[Vec<String>],
        chunk: usize,
    ) -> Result<Option<Value>, DestinationError> {
        if transaction
            .get("stages")
            .and_then(|s| s.get(dataset))
            .is_none()
            || rows.is_empty()
        {
            return Err(cfg_err("Google Sheets pending write is invalid"));
        }
        sheets_values(transaction, dataset, rows)?;
        let header = transaction
            .get("headers")
            .and_then(|h| h.get(dataset))
            .and_then(Value::as_array)
            .map(|a| a.len())
            .unwrap_or_else(|| rows[0].len());
        if rows.iter().any(|row| row.len() != header) {
            return Err(cfg_err(
                "Google Sheets row width differs from the declared header",
            ));
        }
        let stage = transaction["stages"][dataset].clone();
        let start = transaction
            .get("rows")
            .and_then(|r| r.get(dataset))
            .and_then(Value::as_i64)
            .unwrap_or(0)
            + 1;
        let end = start + rows.len() as i64 - 1;
        let digest = sha2::Sha256::new();
        use sha2::Digest;
        let mut digest = digest;
        digest.update(
            serde_json::to_string(&json!(rows))
                .unwrap_or_default()
                .as_bytes(),
        );
        Ok(Some(json!({
            "kind": "sheets_write",
            "dataset": dataset,
            "chunk": chunk,
            "start": start,
            "end": end,
            "width": a1_column(header as i64)?,
            "stage_title": stage.get("title").cloned().unwrap_or(Value::Null),
            "rows_sha256": format!("{:x}", digest.finalize()),
        })))
    }

    fn reconcile_pending(
        &self,
        transaction: &mut Value,
        pending: &Value,
        rows: &[Vec<String>],
    ) -> Result<Option<PendingOutcome>, DestinationError> {
        if pending.get("kind").and_then(Value::as_str) != Some("sheets_write") {
            return Err(uncertain(
                "Sheets pending operation requires operator review",
            ));
        }
        let dataset = pending.get("dataset").and_then(Value::as_str);
        let Some(dataset) = dataset else {
            return Err(cfg_err(
                "Sheets pending write does not match retained transaction state",
            ));
        };
        if transaction
            .get("stages")
            .and_then(|s| s.get(dataset))
            .is_none()
            || rows.is_empty()
        {
            return Err(cfg_err(
                "Sheets pending write does not match retained transaction state",
            ));
        }
        use sha2::Digest;
        let mut digest = sha2::Sha256::new();
        digest.update(
            serde_json::to_string(&json!(rows))
                .unwrap_or_default()
                .as_bytes(),
        );
        if format!("{:x}", digest.finalize())
            != pending
                .get("rows_sha256")
                .and_then(Value::as_str)
                .unwrap_or_default()
        {
            return Err(cfg_err(
                "Sheets pending write does not match the local package",
            ));
        }
        let title = pending
            .get("stage_title")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .replace('\'', "''");
        let (start, end, width) = (
            pending.get("start").and_then(Value::as_i64),
            pending.get("end").and_then(Value::as_i64),
            pending.get("width").and_then(Value::as_str),
        );
        let (Some(start), Some(end), Some(width)) = (start, end, width) else {
            return Err(cfg_err("Sheets pending write has an invalid range"));
        };
        let readback = self.request(
            "GET",
            &format!(
                "https://sheets.googleapis.com/v4/spreadsheets/{}/values/'{}'!A{}:{}{}?valueRenderOption=UNFORMATTED_VALUE",
                self.spreadsheet_id, title, start, width, end
            ),
            None,
            None,
            true,
        )?;
        let actual = readback.get("values").cloned().unwrap_or(json!([]));
        let expected = sheets_values(transaction, dataset, rows)?;
        if !sheets_readback_matches(&actual, &expected) {
            return Ok(Some(PendingOutcome::NotApplied));
        }
        if transaction
            .get("headers")
            .and_then(|h| h.get(dataset))
            .map(Value::is_null)
            .unwrap_or(true)
        {
            if let Some(headers) = transaction
                .get_mut("headers")
                .and_then(Value::as_object_mut)
            {
                headers.insert(
                    dataset.to_owned(),
                    Value::Array(rows[0].iter().map(|r| Value::String(r.clone())).collect()),
                );
            }
        }
        let prior = transaction
            .get("rows")
            .and_then(|r| r.get(dataset))
            .and_then(Value::as_i64)
            .unwrap_or(0);
        if let Some(rows_map) = transaction.get_mut("rows").and_then(Value::as_object_mut) {
            rows_map.insert(dataset.to_owned(), json!(prior + rows.len() as i64));
        }
        Ok(Some(PendingOutcome::Applied))
    }

    fn reconcile_commit(
        &self,
        transaction: &mut Value,
    ) -> Result<Option<CommitOutcome>, DestinationError> {
        let metadata = self.request(
            "GET",
            &sheets_properties_url(&self.spreadsheet_id, "sheets.properties(sheetId,title)"),
            None,
            None,
            true,
        )?;
        let present: std::collections::BTreeSet<i64> = metadata
            .get("sheets")
            .and_then(Value::as_array)
            .map(|items| {
                items
                    .iter()
                    .filter_map(|item| {
                        item.get("properties")
                            .and_then(|p| p.get("sheetId"))
                            .and_then(Value::as_i64)
                    })
                    .collect()
            })
            .unwrap_or_default();
        let expected: std::collections::BTreeSet<i64> = transaction
            .get("stages")
            .and_then(Value::as_object)
            .map(|stages| {
                stages
                    .values()
                    .filter_map(|s| s.get("sheetId").and_then(Value::as_i64))
                    .collect()
            })
            .unwrap_or_default();
        if !expected.is_empty() && expected.is_subset(&present) {
            return Ok(Some(CommitOutcome::NotApplied));
        }
        Err(uncertain(
            "Sheets final switch cannot be reconciled from retained staging sheets; operator review is required",
        ))
    }

    fn commit(&self, transaction: &mut Value) -> Result<Value, DestinationError> {
        let mut requests: Vec<Value> = Vec::new();
        let stages = transaction
            .get("stages")
            .and_then(Value::as_object)
            .cloned()
            .unwrap_or_default();
        for (name, stage) in &stages {
            let header = transaction
                .get("headers")
                .and_then(|h| h.get(name))
                .and_then(Value::as_array)
                .cloned();
            let Some(header) = header else {
                return Err(cfg_err(&format!(
                    "Google Sheets dataset {name:?} has no header"
                )));
            };
            let original = transaction["worksheets"][name].clone();
            let worksheet_id = original.get("worksheet_id").cloned().unwrap_or(Value::Null);
            let written = transaction
                .get("rows")
                .and_then(|r| r.get(name))
                .and_then(Value::as_i64)
                .unwrap_or(0);
            requests.extend([
                json!({
                    "updateSheetProperties": {
                        "properties": {
                            "sheetId": worksheet_id,
                            "gridProperties": transaction["target_grids"][name].clone(),
                        },
                        "fields": "gridProperties.rowCount,gridProperties.columnCount",
                    }
                }),
                json!({
                    "updateCells": {
                        "range": {"sheetId": worksheet_id},
                        "fields": "userEnteredValue",
                    }
                }),
                json!({
                    "copyPaste": {
                        "source": {
                            "sheetId": stage.get("sheetId").cloned().unwrap_or(Value::Null),
                            "startRowIndex": 0,
                            "endRowIndex": written,
                            "startColumnIndex": 0,
                            "endColumnIndex": header.len(),
                        },
                        "destination": {
                            "sheetId": worksheet_id,
                            "startRowIndex": 0,
                            "startColumnIndex": 0,
                        },
                        "pasteType": "PASTE_VALUES",
                        "pasteOrientation": "NORMAL",
                    }
                }),
            ]);
        }
        for stage in stages.values() {
            requests.push(json!({
                "deleteSheet": {"sheetId": stage.get("sheetId").cloned().unwrap_or(Value::Null)}
            }));
        }
        self.request(
            "POST",
            &format!("{}:batchUpdate", sheets_stage_url(&self.spreadsheet_id)),
            Some(&json!({"requests": requests})),
            None,
            false,
        )?;
        Ok(json!({
            "publication": "atomic_spreadsheet_batch_update",
            "remote_verification": "each bounded staged range was read back before publication",
        }))
    }

    fn abort(&self, transaction: &mut Value) -> Result<Option<()>, DestinationError> {
        let stages = transaction
            .get("stages")
            .and_then(Value::as_object)
            .cloned()
            .unwrap_or_default();
        if stages.is_empty() {
            return Ok(Some(()));
        }
        let requests: Vec<Value> = stages
            .values()
            .map(|stage| {
                json!({"deleteSheet": {"sheetId": stage.get("sheetId").cloned().unwrap_or(Value::Null)}})
            })
            .collect();
        self.request(
            "POST",
            &format!("{}:batchUpdate", sheets_stage_url(&self.spreadsheet_id)),
            Some(&json!({"requests": requests})),
            None,
            false,
        )?;
        Ok(Some(()))
    }
}

/// Alias kept so older imports still compile.
pub type GoogleSheetsAppendClient = GoogleSheetsClient;

fn bytes_value(bytes: &[u8]) -> Value {
    Value::Object(Map::from_iter([(
        "__bytes__".to_owned(),
        Value::Array(bytes.iter().map(|b| json!(*b)).collect()),
    )]))
}

/// Host-owned BigQuery client: explicit REST calls with per-table staging and deterministic job
/// reconciliation. Publication is atomic per table, not across all tables.
pub struct GoogleBigQueryClient {
    rest: GoogleRestClient,
    /// Destination name, always `bigquery`.
    pub destination: &'static str,
    /// Configured target name.
    pub target: String,
    /// Billing project ID.
    pub project_id: String,
    /// Destination dataset ID.
    pub dataset_id: String,
    /// Dataset name -> `{"table_id": str}` mapping.
    pub tables: Map<String, Value>,
    /// Optional job location constraint.
    pub location: Option<String>,
    /// Host-owned cost acknowledgement.
    pub cost_authorized: bool,
}

impl GoogleBigQueryClient {
    /// Construct with target, project, dataset, table map and options.
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        target: impl Into<String>,
        project_id: impl Into<String>,
        dataset_id: impl Into<String>,
        tables: Map<String, Value>,
        location: Option<String>,
        cost_authorized: bool,
        token_supplier: Option<TokenSupplier>,
        fetcher: Option<Arc<dyn Fetcher>>,
    ) -> Self {
        GoogleBigQueryClient {
            rest: GoogleRestClient::new(BIGQUERY_SCOPE, token_supplier, fetcher),
            destination: "bigquery",
            target: target.into(),
            project_id: project_id.into(),
            dataset_id: dataset_id.into(),
            tables,
            location,
            cost_authorized,
        }
    }

    fn url(&self, suffix: &str) -> String {
        format!(
            "https://bigquery.googleapis.com/bigquery/v2/projects/{}/{suffix}",
            self.project_id
        )
    }

    fn job_url(&self, job_id: &str) -> String {
        match &self.location {
            Some(location) => self.url(&format!("jobs/{job_id}?location={location}")),
            None => self.url(&format!("jobs/{job_id}")),
        }
    }

    fn stage_load_job_id(token: &str, dataset: &str, part: i64) -> String {
        format!("seohead_{token}_{dataset}_{part}")
    }

    /// Insert a job with a deterministic ID: idempotent, so an existing job is observed
    /// rather than loaded a second time.
    fn job(
        &self,
        job_id: &str,
        configuration: &Value,
        media: Option<&[u8]>,
    ) -> Result<Value, DestinationError> {
        let mut reference = Map::from_iter([
            ("projectId".to_owned(), json!(self.project_id)),
            ("jobId".to_owned(), json!(job_id)),
        ]);
        if let Some(location) = &self.location {
            reference.insert("location".to_owned(), json!(location));
        }
        let payload = json!({
            "jobReference": Value::Object(reference),
            "configuration": configuration,
        });
        let result = match media {
            None => self
                .rest
                .request("POST", &self.url("jobs"), Some(&payload), None, true),
            Some(media) => {
                let boundary = format!("seohead_{}", uuid::Uuid::new_v4().simple());
                let mut multipart: Vec<u8> = Vec::new();
                multipart.extend_from_slice(
                    format!(
                        "--{boundary}\r\nContent-Type: application/json; charset=UTF-8\r\n\r\n"
                    )
                    .as_bytes(),
                );
                multipart.extend_from_slice(
                    serde_json::to_string(&payload)
                        .unwrap_or_default()
                        .as_bytes(),
                );
                multipart.extend_from_slice(
                    format!("\r\n--{boundary}\r\nContent-Type: application/octet-stream\r\n\r\n")
                        .as_bytes(),
                );
                multipart.extend_from_slice(media);
                multipart.extend_from_slice(format!("\r\n--{boundary}--\r\n").as_bytes());
                let mut headers = Map::new();
                headers.insert(
                    "Content-Type".to_owned(),
                    json!(format!("multipart/related; boundary={boundary}")),
                );
                self.rest.request(
                    "POST",
                    &format!(
                        "https://bigquery.googleapis.com/upload/bigquery/v2/projects/{}/jobs?uploadType=multipart",
                        self.project_id
                    ),
                    Some(&bytes_value(&multipart)),
                    Some(&headers),
                    true,
                )
            }
        };
        let result = match result {
            Ok(result) => result,
            Err(exc) => {
                let text = exc.to_string().to_lowercase();
                let duplicate = [
                    "alreadyexists",
                    "already exists",
                    "duplicate",
                    "http 409",
                    "'code': 409",
                ]
                .iter()
                .any(|marker| text.contains(marker));
                if !duplicate {
                    return Err(exc);
                }
                json!({"jobReference": payload["jobReference"].clone()})
            }
        };
        let returned = result
            .get("jobReference")
            .and_then(|r| r.get("jobId"))
            .and_then(Value::as_str)
            .unwrap_or(job_id);
        if returned != job_id {
            return Err(cfg_err(
                "BigQuery response did not preserve the deterministic job identity",
            ));
        }
        self.wait_job(job_id)
    }

    /// Bounded poll for a job; no DONE verdict means the outcome is uncertain.
    fn wait_job(&self, job_id: &str) -> Result<Value, DestinationError> {
        for attempt in 0..=GOOGLE_RETRIES {
            let result = self
                .rest
                .request("GET", &self.job_url(job_id), None, None, true)?;
            let status = result.get("status");
            if status.and_then(|s| s.get("state")).and_then(Value::as_str) == Some("DONE") {
                let failed = status
                    .map(|s| {
                        let error_result = s.get("errorResult");
                        let errors = s.get("errors");
                        crate::py::truthy(error_result) || crate::py::truthy(errors)
                    })
                    .unwrap_or(false);
                if failed {
                    return Err(cfg_err(&format!(
                        "BigQuery job {job_id:?} failed: {}",
                        crate::py::repr(
                            status
                                .and_then(|s| s.get("errorResult"))
                                .unwrap_or(&Value::Null)
                        )
                    )));
                }
                return Ok(result);
            }
            if attempt < GOOGLE_RETRIES {
                (self.rest.sleeper)(0.1 * (attempt as f64 + 1.0));
            }
        }
        Err(uncertain(format!(
            "BigQuery job {job_id:?} did not reach DONE within the bounded poll window"
        )))
    }

    /// Observed remote row count of a table, for reconciliation.
    fn table_rows(&self, table_id: &str) -> Result<i64, DestinationError> {
        let result = self.rest.request(
            "GET",
            &self.url(&format!("datasets/{}/tables/{table_id}", self.dataset_id)),
            None,
            None,
            true,
        )?;
        result
            .get("numRows")
            .and_then(Value::as_str)
            .and_then(|s| s.parse::<i64>().ok())
            .or_else(|| result.get("numRows").and_then(Value::as_i64))
            .ok_or_else(|| cfg_err("BigQuery table verification returned no row count"))
    }
}

fn bq_fields(declared: &[Value]) -> Result<Vec<Value>, DestinationError> {
    let mut fields = Vec::with_capacity(declared.len());
    for field in declared {
        let kind = bq_type(
            field
                .get("type")
                .and_then(Value::as_str)
                .unwrap_or_default(),
        );
        if kind.is_empty() {
            return Err(cfg_err(
                "BI package contains an unsupported BigQuery field type",
            ));
        }
        fields.push(json!({
            "name": field.get("name").cloned().unwrap_or(Value::Null),
            "type": kind,
            "mode": "NULLABLE",
        }));
    }
    Ok(fields)
}

fn bq_load_configuration(
    project_id: &str,
    dataset_id: &str,
    stage: &str,
    disposition: &str,
    dataset: &str,
    fields: &[Value],
) -> Value {
    json!({
        "load": {
            "destinationTable": {
                "projectId": project_id,
                "datasetId": dataset_id,
                "tableId": stage,
            },
            "sourceFormat": "NEWLINE_DELIMITED_JSON",
            "writeDisposition": disposition,
            "createDisposition": "CREATE_IF_NEEDED",
            "schema": {"fields": fields},
        },
        "labels": {"seohead": "bi", "dataset": &dataset[..dataset.len().min(63)]},
    })
}

impl DestinationClient for GoogleBigQueryClient {
    fn destination_name(&self) -> &str {
        self.destination
    }

    fn authorize_target(&self, target: &str) -> bool {
        target == self.target
    }

    fn begin(&self, args: &BeginArgs<'_>) -> Result<Value, DestinationError> {
        if !matches!(args.operation, "replace" | "append") {
            return Err(cfg_err("BigQuery operation must be replace or append"));
        }
        if !self.cost_authorized {
            return Err(cfg_err(
                "BigQuery apply requires a host-owned cost_authorized=true for this project and dataset",
            ));
        }
        let mapping = require_dataset_mapping(
            &Value::Object(self.tables.clone()),
            args.datasets,
            "BigQuery table",
            "table_id",
            args.selected_projection,
        )?;
        for (name, table) in &mapping {
            let ok = table
                .get("table_id")
                .and_then(Value::as_str)
                .map(|t| !t.is_empty())
                .unwrap_or(false);
            if !ok {
                return Err(cfg_err(&format!(
                    "BigQuery table mapping for {name:?} is invalid"
                )));
            }
        }
        let metadata = self.rest.request(
            "GET",
            &self.url(&format!("datasets/{}", self.dataset_id)),
            None,
            None,
            true,
        )?;
        let reference = metadata.get("datasetReference");
        let confirmed = reference
            .and_then(|r| r.get("datasetId"))
            .and_then(Value::as_str)
            == Some(self.dataset_id.as_str());
        if !confirmed {
            return Err(cfg_err(
                "BigQuery target dataset permission check did not confirm the target",
            ));
        }
        let token = &args.package_sha256[..args.package_sha256.len().min(16)];
        Ok(json!({
            "target": args.target,
            "schema_version": args.schema_version,
            "package_sha256": args.package_sha256,
            "tables": Value::Object(mapping),
            "datasets": Value::Object(args.datasets.clone()),
            "token": token,
            "headers": {},
            "rows": args.datasets.keys().map(|n| (n.clone(), json!(0))).collect::<Map<_, _>>(),
            "load_parts": args.datasets.keys().map(|n| (n.clone(), json!(0))).collect::<Map<_, _>>(),
            "stage_ids": args.datasets.keys().map(|n| (n.clone(), json!(format!("_seohead_stage_{n}_{token}")))).collect::<Map<_, _>>(),
            "operation": args.operation,
        }))
    }

    fn write(
        &self,
        transaction: &mut Value,
        dataset: &str,
        rows: &[Vec<String>],
    ) -> Result<(), DestinationError> {
        if transaction
            .get("datasets")
            .and_then(|d| d.get(dataset))
            .is_none()
        {
            return Err(cfg_err("BigQuery write named an undeclared dataset"));
        }
        if rows.is_empty() {
            return Ok(());
        }
        let prior_header = transaction
            .get("headers")
            .and_then(|h| h.get(dataset))
            .cloned()
            .filter(|h| !h.is_null());
        let header: Vec<String> = match prior_header {
            None => {
                let header = rows[0].clone();
                if let Some(headers) = transaction
                    .get_mut("headers")
                    .and_then(Value::as_object_mut)
                {
                    headers.insert(
                        dataset.to_owned(),
                        Value::Array(header.iter().map(|r| json!(r)).collect()),
                    );
                }
                return Ok(());
            }
            Some(value) => value
                .as_array()
                .map(|a| {
                    a.iter()
                        .map(|v| v.as_str().unwrap_or_default().to_owned())
                        .collect()
                })
                .unwrap_or_default(),
        };
        if rows.iter().any(|row| row.len() != header.len()) {
            return Err(cfg_err(
                "BigQuery row width differs from the declared header",
            ));
        }
        let declared = transaction["datasets"][dataset]
            .get("fields")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();
        let declared_names: Vec<&str> = declared
            .iter()
            .map(|f| f.get("name").and_then(Value::as_str).unwrap_or_default())
            .collect();
        if declared_names != header.iter().map(String::as_str).collect::<Vec<_>>() {
            return Err(cfg_err(
                "BigQuery CSV header differs from the package manifest",
            ));
        }
        let fields = bq_fields(&declared)?;
        let mut objects = Vec::with_capacity(rows.len());
        for row in rows {
            let mut object = Map::new();
            for (field, value) in declared.iter().zip(row.iter()) {
                object.insert(
                    field
                        .get("name")
                        .and_then(Value::as_str)
                        .unwrap_or_default()
                        .to_owned(),
                    bq_value(
                        value,
                        field
                            .get("type")
                            .and_then(Value::as_str)
                            .unwrap_or_default(),
                    )?,
                );
            }
            objects.push(Value::Object(object));
        }
        let part = transaction
            .get("load_parts")
            .and_then(|p| p.get(dataset))
            .and_then(Value::as_i64)
            .unwrap_or(0);
        let stage = transaction["stage_ids"][dataset]
            .as_str()
            .unwrap_or_default()
            .to_owned();
        let disposition = if part == 0 {
            "WRITE_TRUNCATE"
        } else {
            "WRITE_APPEND"
        };
        let token = transaction["token"].as_str().unwrap_or_default();
        let job_id = Self::stage_load_job_id(token, dataset, part);
        let mut payload: Vec<u8> = Vec::new();
        for object in &objects {
            let line = format!("{}\n", serde_json::to_string(object).unwrap_or_default());
            if payload.len() + line.len() > GOOGLE_REQUEST_BYTES {
                return Err(cfg_err(
                    "BigQuery load chunk exceeds the bounded payload size before transport",
                ));
            }
            payload.extend_from_slice(line.as_bytes());
        }
        let configuration = bq_load_configuration(
            &self.project_id,
            &self.dataset_id,
            &stage,
            disposition,
            dataset,
            &fields,
        );
        let result = self.job(&job_id, &configuration, Some(&payload))?;
        let output = result
            .get("statistics")
            .and_then(|s| s.get("load"))
            .and_then(|l| l.get("outputRows"))
            .cloned()
            .unwrap_or(Value::Null);
        if output.to_string().trim_matches('"') != rows.len().to_string() {
            return Err(cfg_err(
                "BigQuery load acknowledgement does not conserve staged rows",
            ));
        }
        let prior_rows = transaction
            .get("rows")
            .and_then(|r| r.get(dataset))
            .and_then(Value::as_i64)
            .unwrap_or(0);
        if let Some(m) = transaction.get_mut("rows").and_then(Value::as_object_mut) {
            m.insert(dataset.to_owned(), json!(prior_rows + rows.len() as i64));
        }
        if let Some(m) = transaction
            .get_mut("load_parts")
            .and_then(Value::as_object_mut)
        {
            m.insert(dataset.to_owned(), json!(part + 1));
        }
        Ok(())
    }

    fn pending_write(
        &self,
        transaction: &Value,
        dataset: &str,
        rows: &[Vec<String>],
        chunk: usize,
    ) -> Result<Option<Value>, DestinationError> {
        if transaction
            .get("datasets")
            .and_then(|d| d.get(dataset))
            .is_none()
            || rows.is_empty()
        {
            return Err(cfg_err("BigQuery pending write is invalid"));
        }
        if transaction
            .get("headers")
            .and_then(|h| h.get(dataset))
            .map(Value::is_null)
            .unwrap_or(true)
        {
            return Ok(Some(json!({
                "kind": "bigquery_header",
                "dataset": dataset,
                "chunk": chunk,
            })));
        }
        let part = transaction
            .get("load_parts")
            .and_then(|p| p.get(dataset))
            .and_then(Value::as_i64)
            .unwrap_or(0);
        let token = transaction["token"].as_str().unwrap_or_default();
        use sha2::Digest;
        let mut digest = sha2::Sha256::new();
        digest.update(
            serde_json::to_string(&json!(rows))
                .unwrap_or_default()
                .as_bytes(),
        );
        Ok(Some(json!({
            "kind": "bigquery_load",
            "dataset": dataset,
            "chunk": chunk,
            "job_id": Self::stage_load_job_id(token, dataset, part),
            "rows": rows.len(),
            "rows_sha256": format!("{:x}", digest.finalize()),
        })))
    }

    fn reconcile_pending(
        &self,
        transaction: &mut Value,
        pending: &Value,
        rows: &[Vec<String>],
    ) -> Result<Option<PendingOutcome>, DestinationError> {
        let kind = pending.get("kind").and_then(Value::as_str);
        if kind == Some("bigquery_header") {
            return Ok(Some(PendingOutcome::NotApplied));
        }
        let dataset = pending.get("dataset").and_then(Value::as_str);
        if kind != Some("bigquery_load")
            || dataset
                .map(|d| transaction.get("datasets").and_then(|m| m.get(d)).is_none())
                .unwrap_or(true)
        {
            return Err(uncertain(
                "BigQuery pending operation requires operator review",
            ));
        }
        use sha2::Digest;
        let mut digest = sha2::Sha256::new();
        digest.update(
            serde_json::to_string(&json!(rows))
                .unwrap_or_default()
                .as_bytes(),
        );
        if format!("{:x}", digest.finalize())
            != pending
                .get("rows_sha256")
                .and_then(Value::as_str)
                .unwrap_or_default()
        {
            return Err(cfg_err(
                "BigQuery pending load does not match the local package",
            ));
        }
        let job_id = pending.get("job_id").and_then(Value::as_str);
        let Some(job_id) = job_id.filter(|j| !j.is_empty()) else {
            return Err(cfg_err("BigQuery pending load has no job identity"));
        };
        let result = self.wait_job(job_id)?;
        let output = result
            .get("statistics")
            .and_then(|s| s.get("load"))
            .and_then(|l| l.get("outputRows"))
            .cloned()
            .unwrap_or(Value::Null);
        let expected = pending.get("rows").cloned().unwrap_or(Value::Null);
        if output.to_string().trim_matches('"') != expected.to_string().trim_matches('"') {
            return Err(cfg_err(
                "BigQuery reconciled load does not conserve staged rows",
            ));
        }
        let dataset = dataset.unwrap_or_default();
        let prior_rows = transaction
            .get("rows")
            .and_then(|r| r.get(dataset))
            .and_then(Value::as_i64)
            .unwrap_or(0);
        let prior_parts = transaction
            .get("load_parts")
            .and_then(|p| p.get(dataset))
            .and_then(Value::as_i64)
            .unwrap_or(0);
        if let Some(m) = transaction.get_mut("rows").and_then(Value::as_object_mut) {
            m.insert(dataset.to_owned(), json!(prior_rows + rows.len() as i64));
        }
        if let Some(m) = transaction
            .get_mut("load_parts")
            .and_then(Value::as_object_mut)
        {
            m.insert(dataset.to_owned(), json!(prior_parts + 1));
        }
        Ok(Some(PendingOutcome::Applied))
    }

    fn reconcile_commit(
        &self,
        transaction: &mut Value,
    ) -> Result<Option<CommitOutcome>, DestinationError> {
        let mut absent = false;
        let stage_ids = transaction
            .get("stage_ids")
            .and_then(Value::as_object)
            .cloned()
            .unwrap_or_default();
        for name in stage_ids.keys() {
            let token = transaction["token"].as_str().unwrap_or_default();
            let version = transaction["schema_version"]
                .as_str()
                .unwrap_or_default()
                .replace('.', "_");
            let job_id = format!("seohead_{token}_{name}_publish_{version}");
            match self.wait_job(&job_id) {
                Ok(_) => {}
                Err(exc) if exc.to_string().contains("HTTP 404") => absent = true,
                Err(exc) => return Err(exc),
            }
        }
        Ok(Some(if absent {
            CommitOutcome::NotApplied
        } else {
            CommitOutcome::Applied
        }))
    }

    fn commit(&self, transaction: &mut Value) -> Result<Value, DestinationError> {
        let mut copied: Vec<Value> = Vec::new();
        let stage_ids = transaction
            .get("stage_ids")
            .and_then(Value::as_object)
            .cloned()
            .unwrap_or_default();
        for (name, stage_id) in &stage_ids {
            let header = transaction
                .get("headers")
                .and_then(|h| h.get(name))
                .cloned()
                .filter(|h| !h.is_null());
            if header.is_none() {
                return Err(cfg_err(&format!("BigQuery dataset {name:?} has no header")));
            }
            let stage = stage_id.as_str().unwrap_or_default();
            if transaction
                .get("load_parts")
                .and_then(|p| p.get(name))
                .and_then(Value::as_i64)
                .unwrap_or(0)
                == 0
            {
                let declared = transaction["datasets"][name]
                    .get("fields")
                    .and_then(Value::as_array)
                    .cloned()
                    .unwrap_or_default();
                let fields = bq_fields(&declared)?;
                let token = transaction["token"].as_str().unwrap_or_default();
                let job_id = format!("seohead_{token}_{name}_empty");
                let configuration = bq_load_configuration(
                    &self.project_id,
                    &self.dataset_id,
                    stage,
                    "WRITE_TRUNCATE",
                    name,
                    &fields,
                );
                let result = self.job(&job_id, &configuration, Some(&[]))?;
                let output = result
                    .get("statistics")
                    .and_then(|s| s.get("load"))
                    .and_then(|l| l.get("outputRows"))
                    .cloned()
                    .unwrap_or(Value::Null);
                if output.to_string().trim_matches('"') != "0" {
                    return Err(cfg_err("BigQuery empty-stage acknowledgement is not empty"));
                }
            }
            let expected = transaction
                .get("rows")
                .and_then(|r| r.get(name))
                .and_then(Value::as_i64)
                .unwrap_or(0);
            if self.table_rows(stage)? != expected {
                return Err(cfg_err(
                    "BigQuery staging table does not reconcile with local rows",
                ));
            }
            let destination = transaction["tables"][name]
                .get("table_id")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_owned();
            let token = transaction["token"].as_str().unwrap_or_default();
            let version = transaction["schema_version"]
                .as_str()
                .unwrap_or_default()
                .replace('.', "_");
            let job_id = format!("seohead_{token}_{name}_publish_{version}");
            let disposition = if transaction
                .get("operation")
                .and_then(Value::as_str)
                .unwrap_or("replace")
                == "replace"
            {
                "WRITE_TRUNCATE"
            } else {
                "WRITE_APPEND"
            };
            self.job(
                &job_id,
                &json!({
                    "copy": {
                        "sourceTable": {
                            "projectId": self.project_id,
                            "datasetId": self.dataset_id,
                            "tableId": stage,
                        },
                        "destinationTable": {
                            "projectId": self.project_id,
                            "datasetId": self.dataset_id,
                            "tableId": destination,
                        },
                        "createDisposition": "CREATE_IF_NEEDED",
                        "writeDisposition": disposition,
                    }
                }),
                None,
            )?;
            copied.push(json!(name));
        }
        Ok(json!({
            "publication": "atomic_per_bigquery_table_not_across_datasets",
            "published_datasets": copied,
            "remote_verification": "each staging table numRows matched its acknowledged local rows",
        }))
    }

    fn abort(&self, transaction: &mut Value) -> Result<Option<()>, DestinationError> {
        let stage_ids = transaction
            .get("stage_ids")
            .and_then(Value::as_object)
            .cloned()
            .unwrap_or_default();
        for table_id in stage_ids.values() {
            let id = table_id.as_str().unwrap_or_default();
            match self.rest.request(
                "DELETE",
                &self.url(&format!("datasets/{}/tables/{id}", self.dataset_id)),
                None,
                None,
                false,
            ) {
                Ok(_) => {}
                // Cleanup is not claimed when the remote outcome is unknown.
                Err(exc) if exc.is_uncertain() => return Err(exc),
                // A failed or absent staging table never permits touching a destination.
                Err(_) => continue,
            }
        }
        Ok(Some(()))
    }
}

fn config_root(host: &dyn Host) -> std::path::PathBuf {
    // Config root: the user's `~/.config` directory.
    if let Some(home) = host.var("HOME").filter(|v| !v.is_empty()) {
        return std::path::PathBuf::from(home).join(".config");
    }
    std::env::home_dir()
        .map(|h| h.join(".config"))
        .unwrap_or_else(|| std::path::PathBuf::from("~/.config"))
}

/// Load one exact enabled host target; no auth client is built.
pub fn host_target_config(
    host: &dyn Host,
    destination: &str,
    target: &str,
) -> Result<Map<String, Value>, DestinationError> {
    let path = read_env(host, HOST_CONFIG_ENV)
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|| config_root(host).join("bi-destinations.json"));
    let text = match std::fs::read_to_string(&path) {
        Ok(text) => text,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            return Err(cfg_err("BI destination host configuration is unavailable"));
        }
        Err(_) => return Err(cfg_err("BI destination host configuration is invalid")),
    };
    let config: Value = match serde_json::from_str(&text) {
        Ok(value) => value,
        Err(_) => return Err(cfg_err("BI destination host configuration is invalid")),
    };
    let allowed = config
        .get(destination)
        .and_then(|d| d.get("targets"))
        .and_then(Value::as_object)
        .cloned()
        .unwrap_or_default();
    let target_config = allowed.get(target);
    let enabled = target_config
        .map(|t| {
            t.as_object()
                .map(|o| o.get("enabled").and_then(Value::as_bool).unwrap_or(false))
                .unwrap_or(false)
        })
        .unwrap_or(false);
    if !enabled {
        return Err(cfg_err("destination target is not host-allowlisted"));
    }
    Ok(target_config
        .and_then(Value::as_object)
        .cloned()
        .unwrap_or_default())
}

/// Non-secret target mapping shown before apply.
pub fn resolved_target_mapping(
    host: &dyn Host,
    destination: &str,
    target: &str,
    datasets: &Map<String, Value>,
    selected_projection: bool,
) -> Result<Value, DestinationError> {
    let target_config = host_target_config(host, destination, target)?;
    let kind = target_config.get("kind").and_then(Value::as_str);
    if destination == "sheets" && kind == Some("google_sheets_service_account") {
        let worksheets = sheets_mapping(
            target_config.get("worksheets").unwrap_or(&Value::Null),
            datasets,
            selected_projection,
        )?;
        let spreadsheet_id = target_config.get("spreadsheet_id").and_then(Value::as_str);
        let Some(spreadsheet_id) = spreadsheet_id.filter(|s| !s.is_empty()) else {
            return Err(cfg_err("Google Sheets target has no valid spreadsheet ID"));
        };
        return Ok(json!({
            "kind": "google_sheets_service_account",
            "spreadsheet_id": spreadsheet_id,
            "worksheets": worksheets.iter().map(|(n, v)| (n.clone(), json!({
                "worksheet_id": v.get("worksheet_id").cloned().unwrap_or(Value::Null),
                "worksheet_title": v.get("worksheet_title").cloned().unwrap_or(Value::Null),
            }))).collect::<Map<_, _>>(),
        }));
    }
    if destination == "bigquery" && kind == Some("google_bigquery_service_account") {
        let tables = require_dataset_mapping(
            target_config.get("tables").unwrap_or(&Value::Null),
            datasets,
            "BigQuery table",
            "table_id",
            selected_projection,
        )?;
        let project_id = target_config.get("project_id").and_then(Value::as_str);
        let dataset_id = target_config.get("dataset_id").and_then(Value::as_str);
        let location = target_config
            .get("location")
            .cloned()
            .unwrap_or(Value::Null);
        let ids_ok = matches!(project_id, Some(p) if !p.is_empty())
            && matches!(dataset_id, Some(d) if !d.is_empty());
        if !ids_ok {
            return Err(cfg_err(
                "BigQuery target has no valid project or dataset ID",
            ));
        }
        if !location.is_null() && !location.is_string() {
            return Err(cfg_err("BigQuery target location is invalid"));
        }
        if tables.values().any(|v| {
            v.get("table_id")
                .and_then(Value::as_str)
                .map(|t| t.is_empty())
                .unwrap_or(true)
        }) {
            return Err(cfg_err("BigQuery target has an invalid table mapping"));
        }
        return Ok(json!({
            "kind": "google_bigquery_service_account",
            "project_id": project_id,
            "dataset_id": dataset_id,
            "location": location,
            "cost_authorized": target_config.get("cost_authorized").and_then(Value::as_bool).unwrap_or(false),
            "tables": tables.iter().map(|(n, v)| (n.clone(), json!({
                "table_id": v.get("table_id").cloned().unwrap_or(Value::Null),
            }))).collect::<Map<_, _>>(),
        }));
    }
    Err(cfg_err(
        "host has no authorized client for the allowlisted target",
    ))
}

/// Return the allowlisted client or a registered host client; credentials are never read from input.
pub fn resolve_host_client(
    host: &dyn Host,
    destination: &str,
    target: &str,
) -> Result<Arc<dyn DestinationClient>, DestinationError> {
    let target_config = host_target_config(host, destination, target)?;
    if let Ok(registry) = HOST_CLIENTS.lock() {
        if let Some(client) = registry.get(&(destination.to_owned(), target.to_owned())) {
            return Ok(client.clone());
        }
    }
    let kind = target_config.get("kind").and_then(Value::as_str);
    if destination == "sheets" && kind == Some("google_sheets_service_account") {
        let spreadsheet_id = target_config.get("spreadsheet_id").and_then(Value::as_str);
        let worksheets = target_config.get("worksheets").and_then(Value::as_object);
        if let (Some(spreadsheet_id), Some(worksheets)) = (spreadsheet_id, worksheets) {
            return Ok(Arc::new(GoogleSheetsClient::new(
                target,
                spreadsheet_id,
                worksheets.clone(),
                None,
                None,
            )));
        }
    }
    if destination == "bigquery" && kind == Some("google_bigquery_service_account") {
        let project_id = target_config.get("project_id").and_then(Value::as_str);
        let dataset_id = target_config.get("dataset_id").and_then(Value::as_str);
        let tables = target_config.get("tables").and_then(Value::as_object);
        // JSON null and a missing key both count as absent.
        let location = target_config
            .get("location")
            .cloned()
            .filter(|l| !l.is_null());
        let location_ok = location.as_ref().map(Value::is_string).unwrap_or(true);
        if let (Some(project_id), Some(dataset_id), Some(tables)) = (
            project_id,
            dataset_id,
            if location_ok { tables } else { None },
        ) {
            if target_config
                .get("cost_authorized")
                .and_then(Value::as_bool)
                != Some(true)
            {
                return Err(cfg_err(
                    "BigQuery target is missing host-owned cost_authorized=true",
                ));
            }
            return Ok(Arc::new(GoogleBigQueryClient::new(
                target,
                project_id,
                dataset_id,
                tables.clone(),
                location.and_then(|l| l.as_str().map(str::to_owned)),
                true,
                None,
                None,
            )));
        }
    }
    Err(cfg_err(
        "host has no authorized client for the allowlisted target",
    ))
}

/// Convenience entry using the process environment.
pub fn resolve_host_client_env(
    destination: &str,
    target: &str,
) -> Result<Arc<dyn DestinationClient>, DestinationError> {
    resolve_host_client(&SystemHost, destination, target)
}
