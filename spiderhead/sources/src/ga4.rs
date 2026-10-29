//! Read-only GA4 Data API landing-page aggregates for evidence prioritization.

use std::future::Future;
use std::path::Path;
use std::pin::Pin;

use serde_json::{json, Map, Value};
use spiderhead_core::host::Host;

use crate::credentials::{self, CredentialError};
use crate::gsc;
use crate::http::{Request, Transport, TransportError};
use crate::{pyerr, pyjson, pystr};

mod errors;
pub use errors::*;
mod numbers;
pub use numbers::*;

/// GA4 Data API v1beta root.
pub const HOST: &str = "https://analyticsdata.googleapis.com/v1beta";
/// The only scope `landing_pages`/`page_views` are allowed to acquire.
pub const READONLY_SCOPE: &str = "https://www.googleapis.com/auth/analytics.readonly";
/// Per-request row cap; tests can override it through `Ga4Env::page_rows`.
pub const MAX_ROWS: i64 = 25_000;
const TIMEOUT: f64 = 30.0;

/// A future that resolves to a `runReport` response body.
pub type BoxBody<'a> = Pin<Box<dyn Future<Output = Result<String, Ga4Fault>> + 'a>>;

/// Transport callable that tests inject: (url, payload, token) to response body.
pub type Ga4Transport<'a> = dyn Fn(&str, &Value, &str) -> Result<String, Ga4Fault> + 'a;

/// The `(url, payload, bearer) -> body` call seam as a mutable object — `ga4_content`'s
/// `collect` carries retry/budget/timezone state between pages, so a plain `Fn` cannot
/// model it. Implemented by [`SyncCall`] (injected transport), [`DefaultCall`]
/// (the default live transport), and `ga4_content`'s page collector.
pub(crate) trait PageCall {
    /// Perform one `runReport` request; resolves to the raw UTF-8 body.
    fn call<'s>(&'s mut self, url: &'s str, payload: &'s Value, token: &'s str) -> BoxBody<'s>;
}

/// Adapts an injected transport callable to the paging seam.
pub(crate) struct SyncCall<'x>(pub &'x Ga4Transport<'x>);

impl PageCall for SyncCall<'_> {
    fn call<'s>(&'s mut self, url: &'s str, payload: &'s Value, token: &'s str) -> BoxBody<'s> {
        let result = self.0(url, payload, token);
        Box::pin(std::future::ready(result))
    }
}

/// Adapts the live transport to the paging seam.
pub(crate) struct DefaultCall<'x, T: Transport>(pub &'x T);

impl<T: Transport> PageCall for DefaultCall<'_, T> {
    fn call<'s>(&'s mut self, url: &'s str, payload: &'s Value, token: &'s str) -> BoxBody<'s> {
        Box::pin(async move { default_transport(self.0, url, payload, token).await })
    }
}

/// The `{ok: false, state: failed, error}` envelope that `run_report` returns on failure.
fn failed(error: &str) -> Value {
    json!({"ok": false, "state": "failed", "error": error})
}

/// Live transport: POST the report payload and decode the body as UTF-8 text.
async fn default_transport<T: Transport>(
    http: &T,
    url: &str,
    payload: &Value,
    token: &str,
) -> Result<String, Ga4Fault> {
    let request = Request::new("POST", url)
        .header("Authorization", format!("Bearer {token}"))
        .header("Content-Type", "application/json")
        .body(pyjson::dumps(payload).into_bytes())
        .timeout(TIMEOUT);
    let response = http.send(&request).await.map_err(|e| Ga4Fault::from(&e))?;
    utf8_body(&response.body)
}

/// Decode failures are invalid-value errors.
pub(crate) fn utf8_body(bytes: &[u8]) -> Result<String, Ga4Fault> {
    String::from_utf8(bytes.to_vec()).map_err(|err| {
        let valid = err.utf8_error().valid_up_to();
        let byte = err.as_bytes()[valid];
        let reason = match err.utf8_error().error_len() {
            None => "unexpected end of data",
            Some(_) => "invalid start byte",
        };
        Ga4Fault::Value(format!(
            "'utf-8' codec can't decode byte 0x{byte:02x} in position {valid}: {reason}"
        ))
    })
}

/// An empty explicit token falls through to the stored credentials.
/// `MissingCredential` yields `Err(message)`; other credential errors propagate.
pub(crate) async fn bearer_token<T: Transport>(
    env: &Ga4Env<'_, T>,
    explicit: Option<&str>,
) -> Result<Result<String, String>, Ga4Error> {
    if let Some(token) = explicit.filter(|t| !t.is_empty()) {
        return Ok(Ok(token.to_owned()));
    }
    let result = match env.access_token {
        Some(seam) => seam(),
        None => credentials::ga4_access_token(env.host, env.root),
    };
    match result {
        Ok(token) => Ok(Ok(token)),
        Err(CredentialError::Missing(message)) => Ok(Err(message)),
        Err(other) => Err(other.into()),
    }
}

/// Service-account fallback, used when a service account is configured, with the read-only scope.
/// `Err("")` means the account is absent entirely; the upstream missing-credential message wins.
async fn service_account_token<T: Transport>(
    env: &Ga4Env<'_, T>,
) -> Result<Result<String, String>, Ga4Error> {
    let available = match env.sa_available {
        Some(seam) => seam(),
        None => credentials::gsc_service_account_available(env.host, env.root),
    };
    if !available {
        return Ok(Err(String::new()));
    }
    let result = match env.sa_token {
        Some(seam) => seam(READONLY_SCOPE),
        None => {
            gsc::service_account_access_token(env.host, env.root, READONLY_SCOPE, env.http).await
        }
    };
    match result {
        Ok(token) => Ok(Ok(token)),
        Err(CredentialError::Missing(message)) => Ok(Err(message)),
        Err(other) => Err(other.into()),
    }
}

/// `run_report` — page through one `runReport` query into flat `{name: value}` records.
pub async fn run_report<T: Transport>(
    property_id: &str,
    start_date: &str,
    end_date: &str,
    dimensions: &[&str],
    metrics: &[&str],
    args: &ReportArgs<'_>,
    env: &Ga4Env<'_, T>,
) -> Result<Value, Ga4Error> {
    if property_id.is_empty()
        || start_date.is_empty()
        || end_date.is_empty()
        || start_date > end_date
    {
        return Err(Ga4Error::Invalid(
            "property_id and an ordered date range are required".to_owned(),
        ));
    }
    let max_rows = args.max_rows.cloned().unwrap_or_else(|| json!(100_000));
    let query = ReportQuery {
        property_id,
        start_date,
        end_date,
        dimensions,
        metrics,
        max_rows: resolve_max_rows(&max_rows)?,
    };
    let bearer = match bearer_token(env, args.token).await? {
        Ok(token) => token,
        Err(missing) => {
            return Ok(json!({"ok": false, "state": "not_configured", "error": missing}))
        }
    };
    let url = format!("{HOST}/properties/{property_id}:runReport");
    match args.transport {
        Some(transport) => {
            let mut call = SyncCall(transport);
            run_report_paged(&url, &mut call, env, &query, &bearer).await
        }
        None => {
            let mut call = DefaultCall(env.http);
            run_report_paged(&url, &mut call, env, &query, &bearer).await
        }
    }
}

/// The `runReport` query values the paged loop needs as one bundle.
pub(crate) struct ReportQuery<'a> {
    /// GA4 property ID used in the report path.
    pub property_id: &'a str,
    /// First day of the report range, as an ISO date.
    pub start_date: &'a str,
    /// Last day of the report range, as an ISO date.
    pub end_date: &'a str,
    /// Dimension names.
    pub dimensions: &'a [&'a str],
    /// Metric names.
    pub metrics: &'a [&'a str],
    /// Resolved `max_rows`, kept with its numeric type.
    pub max_rows: PyNum,
}

/// `run_report` with an explicit page-call object — `ga4_content::page_views` drives
/// the same validation/credential/loop sequence with its own `collect` state machine.
/// `token` is `run_report`'s argument: `""`/absent falls through to credentials and a
/// `MissingCredential` becomes the `not_configured` envelope.
pub(crate) async fn run_report_with_call<T: Transport>(
    query: &ReportQuery<'_>,
    token: Option<&str>,
    call: &mut dyn PageCall,
    env: &Ga4Env<'_, T>,
) -> Result<Value, Ga4Error> {
    if query.property_id.is_empty()
        || query.start_date.is_empty()
        || query.end_date.is_empty()
        || query.start_date > query.end_date
    {
        return Err(Ga4Error::Invalid(
            "property_id and an ordered date range are required".to_owned(),
        ));
    }
    let bearer = match bearer_token(env, token).await? {
        Ok(token) => token,
        Err(missing) => {
            return Ok(json!({"ok": false, "state": "not_configured", "error": missing}))
        }
    };
    let url = format!("{HOST}/properties/{}:runReport", query.property_id);
    run_report_paged(&url, call, env, query, &bearer).await
}

/// The shared pagination loop over one `runReport` query. Returns `Err` only for
/// the `int(payload["limit"])` failure a float/bool `max_rows` produces mid-loop.
pub(crate) async fn run_report_paged<T: Transport>(
    url: &str,
    call: &mut dyn PageCall,
    env: &Ga4Env<'_, T>,
    query: &ReportQuery<'_>,
    bearer: &str,
) -> Result<Value, Ga4Error> {
    let mut records: Vec<Value> = Vec::new();
    let mut offset = 0i64;
    let mut declared_total: Option<i64> = None;
    let (mut data_loss, mut sampled, mut thresholded) = (false, false, false);
    let max_rows = query.max_rows;
    let total = loop {
        // The page limit is the smaller of `MAX_ROWS` and the rows left; on a tie the first value
        // is kept, so an equal float still renders as `25000`.
        let rest = max_rows.minus(offset);
        let limit_num = if rest.lt_int(env.page_rows) {
            rest
        } else {
            PyNum::Int(env.page_rows)
        };
        let limit_text = limit_num.to_py_str();
        let payload = json!({
            "dateRanges": [{"startDate": query.start_date, "endDate": query.end_date}],
            "dimensions": query.dimensions.iter().map(|name| json!({"name": name})).collect::<Vec<_>>(),
            "metrics": query.metrics.iter().map(|name| json!({"name": name})).collect::<Vec<_>>(),
            "limit": limit_text,
            "offset": offset.to_string(),
        });
        let raw = call.call(url, &payload, bearer).await;
        let raw = match raw {
            Ok(raw) => raw,
            Err(Ga4Fault::Other { class, message }) => {
                return Err(Ga4Error::Python { class, message })
            }
            Err(fault) => return Ok(failed(&fault.to_string())),
        };
        let body: Value = match serde_json::from_str(&raw) {
            Ok(body) => body,
            Err(_) => {
                let message =
                    pyjson::decode_error(&raw).unwrap_or_else(|| "JSON decode error".to_owned());
                return Ok(failed(&message));
            }
        };
        // A non-object body or an explicit null rows value takes the malformed branch.
        let rows = if body.is_object() {
            body.get("rows").cloned().unwrap_or_else(|| json!([]))
        } else {
            Value::Null
        };
        let total = if body.is_object() {
            body.get("rowCount").cloned()
        } else {
            None
        };
        let rows_len = rows.as_array().map_or(0, Vec::len) as i64;
        // The checks run lazily: the limit is parsed only after the rows list, the row count
        // and the total-vs-count check pass, so a float `max_rows` fails mid-loop with an
        // invalid-value error that escapes the handler.
        if !rows.is_array()
            || total.as_ref().map(pystr::is_py_int) != Some(true)
            || total.as_ref().and_then(Value::as_i64).unwrap_or_default() < offset + rows_len
            || rows_len > py_int(&limit_text)?
            || !rows
                .as_array()
                .map(|list| list.iter().all(Value::is_object))
                .unwrap_or(false)
        {
            return Ok(failed("malformed GA4 Data API response"));
        }
        let total_i = total.as_ref().and_then(Value::as_i64).unwrap_or_default();
        match declared_total {
            None => declared_total = Some(total_i),
            Some(declared) if declared != total_i => {
                return Ok(failed("GA4 rowCount changed during paging"));
            }
            _ => {}
        }
        // Missing or falsy metadata counts as an empty object.
        let metadata = body
            .get("metadata")
            .filter(|v| pystr::truthy(Some(v)))
            .cloned()
            .unwrap_or_else(|| json!({}));
        if !metadata.is_object() {
            return Ok(failed("malformed GA4 metadata"));
        }
        data_loss |= pystr::truthy(metadata.get("dataLossFromOtherRow"));
        sampled |= pystr::truthy(metadata.get("samplingMetadatas"));
        thresholded |= pystr::truthy(metadata.get("subjectToThresholding"));
        for row in rows.as_array().cloned().unwrap_or_default() {
            let dimension_values = row.get("dimensionValues");
            let metric_values = row.get("metricValues");
            let dims = dimension_values.and_then(Value::as_array);
            let mets = metric_values.and_then(Value::as_array);
            let shape_ok = dims.map(Vec::len) == Some(query.dimensions.len())
                && mets.map(Vec::len) == Some(query.metrics.len())
                && dims
                    .into_iter()
                    .flatten()
                    .chain(mets.into_iter().flatten())
                    .all(|item| item.is_object() && item.get("value").is_some());
            if !shape_ok {
                return Ok(failed("malformed GA4 row values"));
            }
            let values = dims
                .into_iter()
                .flatten()
                .chain(mets.into_iter().flatten())
                .map(|item| item.get("value").cloned().unwrap_or_default());
            let record: Map<String, Value> = query
                .dimensions
                .iter()
                .chain(query.metrics.iter())
                .map(|name| name.to_string())
                .zip(values)
                .collect();
            records.push(Value::Object(record));
        }
        offset += rows_len;
        if offset >= total_i || max_rows.le_offset(offset) {
            break total_i;
        }
        if rows_len == 0 {
            return Ok(failed("GA4 ended before declared rowCount"));
        }
    };
    let truncated = offset < total;
    Ok(json!({
        "ok": true,
        "state": if truncated || data_loss || sampled || thresholded { "partial" } else { "complete" },
        "rows": records,
        "returned": records.len(),
        "truncated": truncated,
        "data_loss": data_loss,
        "sampled": sampled,
        "thresholded": thresholded,
    }))
}

/// Options for `landing_pages`.
#[derive(Default)]
pub struct LandingArgs<'a> {
    /// Append `keyEvents` to the metrics list.
    pub include_conversions: bool,
    /// Append `totalRevenue` to the metrics list.
    pub include_revenue: bool,
    /// Operator-provided identity; falsy becomes "provider default".
    pub reporting_identity: Option<&'a str>,
    /// Explicit bearer token; an empty value falls through to the stored credentials.
    pub token: Option<&'a str>,
    /// Transport override; None uses the live transport.
    pub transport: Option<&'a Ga4Transport<'a>>,
}

/// `landing_pages` — GA4 landing-page aggregates; sessions are not Search Console clicks.
pub async fn landing_pages<T: Transport>(
    property_id: &str,
    start_date: &str,
    end_date: &str,
    args: &LandingArgs<'_>,
    env: &Ga4Env<'_, T>,
) -> Result<Value, Ga4Error> {
    if property_id.is_empty()
        || start_date.is_empty()
        || end_date.is_empty()
        || start_date > end_date
    {
        return Err(Ga4Error::Invalid(
            "property_id and an ordered date range are required".to_owned(),
        ));
    }
    let bearer = match bearer_token(env, args.token).await? {
        Ok(token) => token,
        Err(original) => match service_account_token(env).await? {
            Ok(token) => token,
            Err(sa_missing) => {
                return Ok(json!({
                    "ok": false,
                    "state": "not_configured",
                    "verified": false,
                    "error": if sa_missing.is_empty() { original } else { sa_missing },
                }));
            }
        },
    };
    let mut metrics = vec!["sessions", "engagedSessions"];
    if args.include_conversions {
        metrics.push("keyEvents");
    }
    if args.include_revenue {
        metrics.push("totalRevenue");
    }
    let payload = json!({
        "dateRanges": [{"startDate": start_date, "endDate": end_date}],
        "dimensions": [{"name": "landingPagePlusQueryString"}],
        "metrics": metrics.iter().map(|name| json!({"name": name})).collect::<Vec<_>>(),
        "limit": env.page_rows.to_string(),
        "keepEmptyRows": false,
    });
    let url = format!("{HOST}/properties/{property_id}:runReport");
    let raw = match args.transport {
        Some(transport) => transport(&url, &payload, &bearer),
        None => default_transport(env.http, &url, &payload, &bearer).await,
    };
    let raw = match raw {
        Ok(raw) => raw,
        Err(Ga4Fault::Other { class, message }) => return Err(Ga4Error::Python { class, message }),
        Err(fault) => return Ok(failed(&fault.to_string())),
    };
    let body: Value = match serde_json::from_str(&raw) {
        Ok(body) => body,
        Err(_) => {
            let message =
                pyjson::decode_error(&raw).unwrap_or_else(|| "JSON decode error".to_owned());
            return Ok(failed(&message));
        }
    };
    // An empty report comes back without "rows": zero rows, not a malformed response.
    let rows = if body.is_object() {
        body.get("rows").cloned().unwrap_or_else(|| json!([]))
    } else {
        Value::Null
    };
    let malformed = !rows.is_array()
        || !rows
            .as_array()
            .map(|list| list.iter().all(Value::is_object))
            .unwrap_or(false);
    if malformed {
        return Ok(failed("malformed GA4 Data API response"));
    }
    let sampling = match body.get("metadata") {
        Some(Value::Object(metadata)) => metadata.get("dataLossFromOtherRow").cloned(),
        _ => None,
    };
    let rows_len = rows.as_array().map_or(0, Vec::len) as i64;
    Ok(json!({
        "ok": true,
        "state": if rows_len >= env.page_rows || pystr::truthy(sampling.as_ref()) { "partial" } else { "complete" },
        "property_reference": "redacted-local-artifact",
        "reporting_identity": args.reporting_identity.filter(|v| !v.is_empty()).unwrap_or("provider default"),
        "period": {"start_date": start_date, "end_date": end_date},
        "dimensions": ["landingPagePlusQueryString"],
        "metrics": metrics,
        "rows": rows,
        "returned": rows_len,
        "truncated": rows_len >= env.page_rows,
        "sampling_or_thresholding": pystr::truthy(sampling.as_ref()),
        "note": "GA4 sessions are analytics visits and are not Google Search Console clicks.",
    }))
}
