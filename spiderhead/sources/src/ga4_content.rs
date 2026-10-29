//! Read-only visited-page views, distinct from GA4 session
//! landing pages. Reuses the GA4 transport, credential helpers and the paginated
//! `runReport` reader. Nothing is collected while importing or normalizing saved
//! evidence.

use std::io::Read;
use std::path::PathBuf;

use serde_json::{json, Map, Value};
use spiderhead_parse::py_url;

use crate::ga4::{self, BoxBody, Ga4Env, Ga4Error, Ga4Fault, Ga4Transport, PageCall};
use crate::http::{Sleeper, Transport};
use crate::{pyjson, pystr, pytime};

mod host_helpers;
pub use host_helpers::*;

/// Dimensions of the source grain that the reader must preserve.
pub const DIMENSIONS: [&str; 3] = ["date", "hostName", "pagePathPlusQueryString"];
/// Metrics requested by the page-views query.
pub const METRICS: [&str; 1] = ["screenPageViews"];
/// Maximum number of paged report requests per call.
pub const MAX_PAGE_REQUESTS: i64 = 32;
/// Largest accepted size of one response body, in bytes.
pub const MAX_RESPONSE_BYTES: usize = 16 * 1024 * 1024;
/// Largest accepted total response size across all pages, in bytes.
pub const MAX_TOTAL_RESPONSE_BYTES: usize = 32 * 1024 * 1024;
/// Serialized size limit for one page-metadata record.
const MAX_FACT_BYTES: usize = 64 * 1024;

/// `page_views`'s keyword arguments.
pub struct PageViewsArgs<'a> {
    /// Row limit; None means 100,000. The value must be an integer, and a boolean is rejected.
    pub max_rows: Option<&'a Value>,
    /// Explicit bearer token; an empty value falls through to the stored credentials.
    pub token: Option<&'a str>,
    /// Transport override; None uses the live transport.
    pub transport: Option<&'a Ga4Transport<'a>>,
}

/// `ga4_content.collect`: each `runReport` call is wrapped with retries, request and byte budgets,
/// header and timezone checks, and fact bookkeeping. The struct owns the state carried between pages.
struct Collector<'x, T: Transport> {
    /// Transport for the actual request; None uses the live transport.
    transport: Option<&'x Ga4Transport<'x>>,
    /// Live transport for the default path.
    http: &'x T,
    /// Retry pause; tests record it instead of sleeping.
    sleeper: Option<&'x Sleeper>,
    /// Time-zone lookup; None uses the system zone database.
    zoneinfo: Option<&'x ga4::ZoneinfoProbe<'x>>,
    /// Page request cap; tests can override it.
    max_requests: i64,
    /// `attempts` — every transport try counts, including retries.
    attempts: i64,
    /// `response_bytes` — cumulative decoded-body bytes.
    response_bytes: usize,
    /// `failure_status` — the last `HTTPError.code`, reset after a good page.
    failure_status: Option<i64>,
    /// `timezone` — last seen `metadata.timeZone` (`null` allowed).
    timezone: Value,
    /// `facts` — per-page `page_metadata` records.
    facts: Vec<Value>,
}

impl<T: Transport> Collector<'_, T> {
    /// Pause between retries: 0.1 s times the attempt number.
    async fn pause(&self, seconds: f64) {
        match self.sleeper {
            Some(sleeper) => sleeper.pause(seconds).await,
            None => Sleeper::Real.pause(seconds).await,
        }
    }

    /// Calls the transport with the URL, payload and access token.
    async fn send(&self, url: &str, payload: &Value, token: &str) -> Result<String, Ga4Fault> {
        match self.transport {
            Some(transport) => transport(url, payload, token),
            None => {
                crate::ga4::DefaultCall(self.http)
                    .call(url, payload, token)
                    .await
            }
        }
    }

    /// The time-zone lookup for the current timezone.
    fn resolve_timezone(&self, name: &str) -> Result<(), Option<String>> {
        match self.zoneinfo {
            Some(seam) => seam(name),
            None => zoneinfo_probe(name),
        }
    }
}

impl<T: Transport> PageCall for Collector<'_, T> {
    fn call<'s>(&'s mut self, url: &'s str, payload: &'s Value, token: &'s str) -> BoxBody<'s> {
        Box::pin(async move {
            let mut payload = payload.clone();
            // Adds `returnPropertyQuota` and `orderBys` after the request fields.
            payload["returnPropertyQuota"] = json!(true);
            payload["orderBys"] = json!(DIMENSIONS
                .iter()
                .map(|name| json!({"dimension": {"dimensionName": name}}))
                .collect::<Vec<_>>());
            let mut raw = None;
            for attempt in 0..3i64 {
                if self.attempts >= self.max_requests {
                    return Err(Ga4Fault::Value(
                        "GA4 page-view request budget exhausted".to_owned(),
                    ));
                }
                self.attempts += 1;
                match self.send(url, &payload, token).await {
                    Ok(body) => {
                        raw = Some(body);
                        break;
                    }
                    Err(Ga4Fault::Http { code, .. }) => {
                        self.failure_status = Some(code);
                        if (code == 429 || (500..=599).contains(&code)) && attempt < 2 {
                            self.pause(0.1 * (attempt + 1) as f64).await;
                            continue;
                        }
                        return Err(Ga4Fault::Url(format!(
                            "GA4 page-view request failed (HTTP {code})"
                        )));
                    }
                    Err(Ga4Fault::Url(_)) | Err(Ga4Fault::Timeout(_)) => {
                        return Err(Ga4Fault::Url(
                            "GA4 page-view transport unavailable".to_owned(),
                        ));
                    }
                    Err(fault) => return Err(fault),
                }
            }
            let Some(raw) = raw else {
                // Unreachable: the loop always returns a body, an error, or retries.
                return Err(Ga4Fault::Url(
                    "GA4 page-view transport unavailable".to_owned(),
                ));
            };
            let raw_len = raw.len();
            if raw_len > MAX_RESPONSE_BYTES {
                return Err(Ga4Fault::Value(
                    "GA4 response exceeds the bounded page size".to_owned(),
                ));
            }
            self.response_bytes += raw_len;
            if self.response_bytes > MAX_TOTAL_RESPONSE_BYTES {
                return Err(Ga4Fault::Value(
                    "GA4 page-view response byte budget exhausted".to_owned(),
                ));
            }
            let body: Value = match serde_json::from_str(&raw) {
                Ok(body) => body,
                Err(_) => {
                    return Err(Ga4Fault::Value(
                        pyjson::decode_error(&raw)
                            .unwrap_or_else(|| "JSON decode error".to_owned()),
                    ))
                }
            };
            if !body.is_object() {
                return Err(Ga4Fault::Value("GA4 response is not an object".to_owned()));
            }
            for (field, expected) in [
                ("dimensionHeaders", DIMENSIONS.as_slice()),
                ("metricHeaders", METRICS.as_slice()),
            ] {
                let headers = body.get(field);
                let names: Vec<Value> = headers
                    .and_then(Value::as_array)
                    .map(|list| {
                        list.iter()
                            .map(|item| {
                                if item.is_object() {
                                    item.get("name").cloned().unwrap_or(Value::Null)
                                } else {
                                    Value::Null
                                }
                            })
                            .collect()
                    })
                    .unwrap_or_default();
                let headers_ok = headers.is_some_and(Value::is_array)
                    && headers
                        .and_then(Value::as_array)
                        .map(|list| list.iter().all(Value::is_object))
                        .unwrap_or(false)
                    && names == expected.iter().map(|name| json!(name)).collect::<Vec<_>>();
                if !headers_ok {
                    return Err(Ga4Fault::Value(
                        "GA4 returned different page-view dimensions or metrics".to_owned(),
                    ));
                }
            }
            let metadata = body
                .get("metadata")
                .filter(|v| pystr::truthy(Some(v)))
                .cloned()
                .unwrap_or_else(|| json!({}));
            if !metadata.is_object() {
                return Err(Ga4Fault::Value("GA4 metadata is invalid".to_owned()));
            }
            let current_timezone = metadata.get("timeZone").cloned().unwrap_or(Value::Null);
            if !current_timezone.is_null() {
                let Some(name) = current_timezone.as_str() else {
                    return Err(Ga4Fault::Value("GA4 timezone is invalid".to_owned()));
                };
                match self.resolve_timezone(name) {
                    Ok(()) => {}
                    Err(None) => {
                        return Err(Ga4Fault::Value("GA4 timezone is unavailable".to_owned()))
                    }
                    Err(Some(message)) => return Err(Ga4Fault::Value(message)),
                }
            }
            if !self.facts.is_empty() && current_timezone != self.timezone {
                return Err(Ga4Fault::Value(
                    "GA4 timezone changed during pagination".to_owned(),
                ));
            }
            self.timezone = current_timezone.clone();
            let fact = json!({
                "offset": payload["offset"],
                "limit": payload["limit"],
                "row_count": body.get("rowCount"),
                "timezone": current_timezone,
                "sampled": pystr::truthy(metadata.get("samplingMetadatas")),
                "sampling_metadata": metadata.get("samplingMetadatas"),
                "thresholded": pystr::truthy(metadata.get("subjectToThresholding")),
                "data_loss": pystr::truthy(metadata.get("dataLossFromOtherRow")),
                "property_quota": body.get("propertyQuota"),
            });
            if pyjson::dumps(&fact).len() > MAX_FACT_BYTES {
                return Err(Ga4Fault::Value(
                    "GA4 page metadata exceeds its bound".to_owned(),
                ));
            }
            self.facts.push(fact);
            self.failure_status = None;
            Ok(raw)
        })
    }
}

/// `page_views` — collect bounded date/host/path view counts using an explicit URL origin.
pub async fn page_views<T: Transport>(
    property_id: &Value,
    start_date: &Value,
    end_date: &Value,
    site_origin: &Value,
    args: &PageViewsArgs<'_>,
    env: &Ga4Env<'_, T>,
) -> Result<Value, Ga4Error> {
    // Date and URL parse errors collapse into the first validation message.
    // Unhashable values are caught as type errors; other scalars are attribute errors that propagate.
    // A null or bytes value parses to a byte-based result that the semantic check rejects.
    let start_days = match start_date {
        Value::String(text) => pytime::date_fromisoformat_err(text).map_err(|_| first_error())?,
        _ => return Err(first_error()),
    };
    let end_days = match end_date {
        Value::String(text) => pytime::date_fromisoformat_err(text).map_err(|_| first_error())?,
        _ => return Err(first_error()),
    };
    let (origin, origin_port, origin_text) = match site_origin {
        Value::String(text) => {
            let split = py_url::urlsplit(text).map_err(|_| first_error())?;
            let port = port(&split.netloc).map_err(|_| first_error())?;
            (split, port, text.as_str())
        }
        Value::Null => {
            let split = py_url::urlsplit("").map_err(|_| first_error())?;
            (split, None, "")
        }
        Value::Array(_) | Value::Object(_) => return Err(first_error()),
        other => {
            return Err(Ga4Error::Python {
                class: "AttributeError".to_owned(),
                message: format!(
                    "'{}' object has no attribute 'decode'",
                    pystr::type_name(other)
                ),
            })
        }
    };
    let (origin_user, origin_password) = userinfo(&origin.netloc);
    let origin_host = hostname(&origin.netloc);
    let max_rows_ok = args
        .max_rows
        .map(|v| pystr::is_py_int(v) && (1..=100_000).contains(&v.as_i64().unwrap_or(0)))
        .unwrap_or(true);
    // The property id must be a text of decimal digits and at least 1.
    let property_ok = match property_id {
        Value::String(text) => pystr::is_decimal(text) && !decimal_lt_one(text)?,
        _ => false,
    };
    let origin_ok = (origin.scheme == "http" || origin.scheme == "https")
        && origin_host
            .as_deref()
            .map(|h| !h.is_empty())
            .unwrap_or(false)
        && !origin_user.map(|u| !u.is_empty()).unwrap_or(false)
        && !origin_password.map(|p| !p.is_empty()).unwrap_or(false)
        && origin.query.is_empty()
        && origin.fragment.is_empty()
        && (origin.path.is_empty() || origin.path == "/")
        && origin_port
            .map(|p| (1..=65535).contains(&p))
            .unwrap_or(true);
    if !property_ok || start_days > end_days || !max_rows_ok || !origin_ok {
        return Err(Ga4Error::Invalid(
            "page_views requires a numeric property, ordered dates, bounded rows and a bare site origin"
                .to_owned(),
        ));
    }
    let max_rows = args.max_rows.and_then(Value::as_i64).unwrap_or(100_000);
    let property = property_id.as_str().unwrap_or_default();
    let start_text = start_date.as_str().unwrap_or_default();
    let end_text = end_date.as_str().unwrap_or_default();

    // Missing credentials give page-view-specific messages, unlike `run_report`.
    let bearer = match ga4::bearer_token(env, args.token).await? {
        Ok(token) => token,
        Err(_missing) => {
            let available = env.sa_available.map(|seam| seam()).unwrap_or_else(|| {
                crate::credentials::gsc_service_account_available(env.host, env.root)
            });
            if !available {
                return Ok(json!({
                    "ok": false,
                    "state": "not_configured",
                    "error": "GA4 read-only credentials are not configured",
                }));
            }
            let sa = match env.sa_token {
                Some(seam) => seam(ga4::READONLY_SCOPE),
                None => {
                    crate::gsc::service_account_access_token(
                        env.host,
                        env.root,
                        ga4::READONLY_SCOPE,
                        env.http,
                    )
                    .await
                }
            };
            match sa {
                Ok(token) => token,
                Err(crate::credentials::CredentialError::Missing(_)) => {
                    return Ok(json!({
                        "ok": false,
                        "state": "not_configured",
                        "error": "GA4 read-only credentials are unavailable",
                    }));
                }
                Err(other) => return Err(other.into()),
            }
        }
    };

    let mut collector = Collector {
        transport: args.transport,
        http: env.http,
        sleeper: env.sleeper,
        zoneinfo: env.zoneinfo,
        max_requests: env.max_requests,
        attempts: 0,
        response_bytes: 0,
        failure_status: None,
        timezone: Value::Null,
        facts: Vec::new(),
    };
    let query = ga4::ReportQuery {
        property_id: property,
        start_date: start_text,
        end_date: end_text,
        dimensions: &DIMENSIONS,
        metrics: &METRICS,
        max_rows: ga4::PyNum::Int(max_rows),
    };
    let result =
        ga4::run_report_with_call(&query, Some(bearer.as_str()), &mut collector, env).await?;
    if result.get("ok").and_then(Value::as_bool) != Some(true) {
        let kind = match collector.failure_status {
            Some(401) | Some(403) => "not_granted",
            Some(429) => "quota_exhausted",
            _ => "collection_failed",
        };
        return Ok(json!({
            "ok": false,
            "state": "failed",
            "failure_kind": kind,
            "status": collector.failure_status,
            "error": "GA4 page-view evidence could not be collected",
            "completed_pages": collector.facts.len(),
            "request_attempts": collector.attempts,
        }));
    }
    // Any missing key or wrong type in a row lands on the same envelope.
    let rows_result = bind_rows(
        result.get("rows").cloned().unwrap_or(Value::Null),
        start_days,
        end_days,
        &origin,
        &origin_host.clone().unwrap_or_default(),
    );
    let rows = match rows_result {
        Ok(rows) => rows,
        Err(()) => {
            return Ok(json!({
                "ok": false,
                "state": "failed",
                "error": "GA4 page-view row contract is invalid",
            }))
        }
    };
    let mut merged = match result {
        Value::Object(map) => map,
        _ => Map::new(),
    };
    let result_state = if rows.is_empty() { "empty" } else { "observed" };
    let extra = json!({
        "rows": rows,
        "operation": "page_views",
        "reporting_identity": format!("properties/{property}"),
        "read_only": true,
        "period": {"start_date": start_text, "end_date": end_text},
        "dimensions": DIMENSIONS,
        "metrics": METRICS,
        "timezone": collector.timezone,
        "attribution": "page_view event count; not session landing-page attribution",
        "grain": "date x hostName x pagePathPlusQueryString",
        "site_origin": origin_text,
        "result_state": result_state,
        "property_access": "verified_by_report",
        "page_metadata": collector.facts,
        "request_attempts": collector.attempts,
        "resource_bounds": {
            "max_rows": max_rows,
            "max_requests": env.max_requests,
            "max_response_bytes": MAX_RESPONSE_BYTES,
            "max_total_response_bytes": MAX_TOTAL_RESPONSE_BYTES,
        },
        "coverage_note": "absent rows are not measured zero; screenPageViews are not sessions or Search Console clicks",
    });
    if let Value::Object(extra) = extra {
        merged.extend(extra);
    }
    Ok(Value::Object(merged))
}

/// The validation failure for missing ISO dates or an explicit HTTP(S) site origin.
fn first_error() -> Ga4Error {
    Ga4Error::Invalid(
        "page_views requires ISO dates and an explicit HTTP(S) site origin".to_owned(),
    )
}

/// Row binding for `page_views`: any row error maps to the "GA4 page-view row contract is invalid" envelope.
fn bind_rows(
    result_rows: Value,
    start_days: i64,
    end_days: i64,
    origin: &py_url::SplitUrl,
    origin_host: &str,
) -> Result<Vec<Value>, ()> {
    // Iterating a non-list rows value is a type error.
    let items = result_rows.as_array().ok_or(())?;
    let mut rows = Vec::new();
    for item in items {
        let day = item.get("date").ok_or(())?;
        // A non-text day, or one whose length is not 8, is an invalid GA4 date.
        let day_str = day.as_str().filter(|d| d.chars().count() == 8).ok_or(())?;
        // Slicing works on characters, not bytes, so multibyte input must not panic.
        let chars: Vec<char> = day_str.chars().collect();
        let iso = format!(
            "{}-{}-{}",
            chars[..4].iter().collect::<String>(),
            chars[4..6].iter().collect::<String>(),
            chars[6..].iter().collect::<String>()
        );
        let observed = pytime::date_fromisoformat_err(&iso).map_err(|_| ())?;
        if !(start_days..=end_days).contains(&observed) {
            return Err(());
        }
        let count = item.get("screenPageViews").ok_or(())?;
        let count_str = count.as_str().filter(|c| pystr::is_decimal(c)).ok_or(())?;
        // Digits in any Unicode Nd script count; values past i64 take the invalid-contract branch.
        let count_int = pystr::nd_int(count_str).ok_or(())?;
        let host = item.get("hostName").ok_or(())?;
        let path = item.get("pagePathPlusQueryString").ok_or(())?;
        // The checks run in order: the URL path is parsed only after the host and both prefix checks pass.
        // A parse error there lands on the contract-invalid envelope, not the unkeyable branch.
        let host_ok = host
            .as_str()
            .map(|h| folded_host(h) == folded_host(origin_host))
            .unwrap_or(false);
        let eligible = host_ok
            && match path.as_str() {
                Some(p) if p.starts_with('/') && !p.starts_with("//") => {
                    py_url::urlsplit(p).map_err(|_| ())?.fragment.is_empty()
                }
                _ => false,
            };
        let mut row = item.as_object().cloned().ok_or(())?;
        let url = if eligible {
            Value::String(format!(
                "{}://{}{}",
                origin.scheme,
                origin.netloc,
                path.as_str().unwrap_or_default()
            ))
        } else {
            Value::Null
        };
        row.insert("date".to_owned(), {
            let (y, m, d) = pytime::civil_from_days(observed);
            json!(format!("{y:04}-{m:02}-{d:02}"))
        });
        row.insert("source_date".to_owned(), json!(day_str));
        row.insert("screenPageViews".to_owned(), json!(count_int));
        row.insert("url".to_owned(), url);
        row.insert(
            "url_binding_state".to_owned(),
            json!(if eligible {
                "declared_origin"
            } else {
                "unkeyable"
            }),
        );
        row.insert(
            "url_binding_reason".to_owned(),
            if eligible {
                Value::Null
            } else {
                json!("host/path cannot be bound to the explicitly selected site origin")
            },
        );
        rows.push(Value::Object(row));
    }
    Ok(rows)
}
