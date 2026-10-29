//! Bounded Miratext SEO analysis adapter; paid modes require an explicit opt-in.
//!
//! A form POST starts or resumes an analysis; `draft` and `working` answers resume by hash.
//! Provider result shapes are reduced without inventing units or recommendations. Paid calls write
//! a spend entry.

use std::path::Path;

use serde_json::{json, Map, Value};
use spiderhead_core::host::Host;
use thiserror::Error;

use crate::credentials::{self, CredentialError};
use crate::http::{Request, Transport, TransportError};
use crate::pystr;
use crate::spend::{self, Record};

/// Miratext analysis endpoint.
pub const ENDPOINT: &str = "https://miratext.com/api2/call/article/seoAnalizText";
/// `MAX_RESPONSE_BYTES` — the documented bounded response budget.
pub const MAX_RESPONSE_BYTES: usize = 20 * 1024 * 1024;

/// Errors raised while building or reading the call (invalid input or an oversized response).
#[derive(Debug, Error)]
#[error("{message}")]
pub struct MiratextError {
    class: &'static str,
    message: String,
}

impl MiratextError {
    /// Error class name of the matching failure.
    pub fn python_class(&self) -> &'static str {
        self.class
    }
}

fn raise<T>(class: &'static str, message: impl Into<String>) -> Result<T, MiratextError> {
    Err(MiratextError {
        class,
        message: message.into(),
    })
}

/// One call's arguments.
#[derive(Debug)]
pub struct Call<'a> {
    /// `urls` — pages to analyze.
    pub urls: Option<&'a [Value]>,
    /// `texts` — raw texts to analyze.
    pub texts: Option<&'a [Value]>,
    /// `my` — the caller's own text/URL being benchmarked.
    pub my: Option<&'a str>,
    /// `hash` — provider task identifier for resume.
    pub hash: Option<&'a Value>,
    /// `check_type` — `url` or `content`.
    pub check_type: &'a str,
    /// `keywords` — keyword analysis (paid).
    pub keywords: Option<&'a str>,
    /// `paid` — request a paid analysis mode.
    pub paid: bool,
    /// `confirm_paid` — the explicit opt-in required for paid/keyword modes.
    pub confirm_paid: bool,
    /// `timeout` — seconds the call may poll for (`type is int`, 1..=600; default 120).
    pub timeout: Option<&'a Value>,
    /// `top` — rows kept per table (`type is int`, 1..=500; default 100).
    pub top: Option<&'a Value>,
    /// `api_key` — overrides the stored credential.
    pub api_key: Option<&'a str>,
}

impl Default for Call<'_> {
    fn default() -> Self {
        Call {
            urls: None,
            texts: None,
            my: None,
            hash: None,
            check_type: "url",
            keywords: None,
            paid: false,
            confirm_paid: false,
            timeout: None,
            top: None,
            api_key: None,
        }
    }
}

fn table(value: &Value) -> Vec<&Map<String, Value>> {
    let rows: Option<&Vec<Value>> = match value {
        Value::Array(rows) => Some(rows),
        Value::Object(m) => ["items", "rows", "data"]
            .iter()
            .find_map(|k| m.get(*k).filter(|v| pystr::truthy(Some(v))))
            .and_then(Value::as_array),
        _ => None,
    };
    match rows {
        Some(rows) if rows.iter().all(Value::is_object) => {
            rows.iter().filter_map(Value::as_object).collect()
        }
        _ => Vec::new(),
    }
}

fn first(row: &Map<String, Value>, names: &[&str]) -> Value {
    names
        .iter()
        .find_map(|n| row.get(*n))
        .cloned()
        .unwrap_or(Value::Null)
}

fn author_tables(data: &Value, top: usize) -> Value {
    if !data.is_object() {
        return json!({
            "state": "unavailable",
            "reason": "missing_result_data",
            "words": [],
            "density_deviation": [],
        });
    };
    let tz = data.get("tz").filter(|v| v.is_object()).unwrap_or(data);
    let words_src = tz.get("keywordsAll").cloned().unwrap_or(Value::Null);
    let dev_src = tz
        .get("densityDeviation")
        .filter(|v| pystr::truthy(Some(v)))
        .or_else(|| tz.get("density_deviation"))
        .cloned()
        .unwrap_or(Value::Null);
    let word_rows = table(&words_src);
    let deviation_rows = table(&dev_src);
    let word_of = |row: &Map<String, Value>| {
        ["word", "keyword", "text", "name"]
            .iter()
            .find_map(|n| row.get(*n))
            .and_then(Value::as_str)
            .and_then(|w| {
                let w = w.to_owned();
                if pystr::strip(&w).is_empty() {
                    None
                } else {
                    Some(w)
                }
            })
    };
    let words: Vec<Value> = word_rows
        .iter()
        .filter_map(|row| {
            word_of(row).map(|word| {
                json!({
                    "word": word,
                    "sites": first(row, &["sites", "count_sites", "competitors"]),
                    "median_density": first(row, &["median_density", "density", "median"]),
                    "mine": first(row, &["mine", "my_count", "count_my"]),
                    "recommended": first(row, &["recommended", "recommend", "recommended_count"]),
                    "unit": "provider_reported",
                })
            })
        })
        .collect();
    let deviations: Vec<Value> = deviation_rows
        .iter()
        .filter_map(|row| {
            word_of(row).map(|word| {
                json!({
                    "word": word,
                    "mine": first(row, &["mine", "my_density", "count_my"]),
                    "median_density": first(row, &["median_density", "density", "median"]),
                    "deviation": first(row, &["deviation", "delta", "difference"]),
                    "unit": "provider_reported",
                })
            })
        })
        .collect();
    let stopwords = tz
        .get("stopwords")
        .filter(|v| pystr::truthy(Some(v)))
        .or_else(|| tz.get("stop_words").filter(|v| pystr::truthy(Some(v))))
        .cloned()
        .unwrap_or_else(|| json!("provider_not_reported"));
    let filters = tz
        .get("filters")
        .filter(|v| pystr::truthy(Some(v)))
        .cloned()
        .unwrap_or_else(|| json!("provider_not_reported"));
    json!({
        "state": if words.is_empty() && deviations.is_empty() { "unavailable" } else { "complete" },
        "reason": if words.is_empty() && deviations.is_empty() { Value::from("provider_result_has_no_recognized_author_tables") } else { Value::Null },
        "words": words[..words.len().min(top)].to_vec(),
        "density_deviation": deviations[..deviations.len().min(top)].to_vec(),
        "stopwords": stopwords,
        "filters": filters,
    })
}

async fn run<T: Transport>(
    transport: &T,
    host: &dyn Host,
    root: &Path,
    call: &Call<'_>,
) -> Result<Value, MiratextError> {
    let timeout = match call.timeout {
        None => 120,
        Some(v) if pystr::is_py_int(v) => v.as_i64().unwrap_or(0),
        Some(_) => -1,
    };
    let top = match call.top {
        None => 100,
        Some(v) if pystr::is_py_int(v) => v.as_i64().unwrap_or(0),
        Some(_) => -1,
    };
    if !(1..=600).contains(&timeout) || !(1..=500).contains(&top) {
        return raise(
            "ValueError",
            "timeout must be 1..600 and top must be 1..500",
        );
    }
    if (call.paid || call.keywords.is_some_and(|k| !k.is_empty())) && !call.confirm_paid {
        return Ok(json!({
            "ok": false,
            "state": "confirmation_required",
            "error": "paid or keyword analysis requires confirm_paid=true",
        }));
    }
    let urls = call.urls.unwrap_or(&[]);
    let texts = call.texts.unwrap_or(&[]);
    let has_source =
        (!urls.is_empty() || !texts.is_empty()) && call.my.is_some_and(|m| !m.is_empty());
    if call.hash.is_none() && !has_source {
        return raise("ValueError", "pass hash to resume, or URLs/texts and my");
    }
    let hash = match call.hash {
        None => None,
        Some(Value::String(h))
            if !h.is_empty()
                && h.len() <= 128
                && h.chars()
                    .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-') =>
        {
            Some(h.clone())
        }
        Some(_) => {
            return raise(
                "ValueError",
                "hash must be a 1..128 character provider identifier",
            )
        }
    };
    if call.check_type != "url" && call.check_type != "content" {
        return raise("ValueError", "check_type must be url or content");
    }
    if !urls
        .iter()
        .chain(texts.iter())
        .all(|v| v.as_str().is_some_and(|s| !s.is_empty()))
    {
        return raise(
            "ValueError",
            "urls and texts must contain non-empty strings",
        );
    }
    if (!urls.is_empty() && !texts.is_empty()) || urls.len() > 10 || texts.len() > 10 {
        return raise(
            "ValueError",
            "pass one of urls or texts with at most 10 items",
        );
    }
    let key = match call.api_key.filter(|k| !k.is_empty()).map(str::to_owned) {
        Some(k) => k,
        None => match credentials::miratext_api_key(host, root) {
            Ok(k) => k,
            Err(e @ CredentialError::Missing(_)) => {
                return Ok(json!({"ok": false, "state": "not_configured", "error": e.to_string()}))
            }
            Err(e) => {
                return raise("MiratextError", e.to_string());
            }
        },
    };
    let mut fields: Vec<(String, String)> = vec![
        ("api_key".to_owned(), key.clone()),
        ("check_type".to_owned(), call.check_type.to_owned()),
    ];
    if let Some(h) = &hash {
        fields.push(("hash".to_owned(), h.clone()));
    } else {
        fields.extend(
            urls.iter()
                .filter_map(Value::as_str)
                .map(|v| ("url[]".to_owned(), v.to_owned())),
        );
        fields.extend(
            texts
                .iter()
                .filter_map(Value::as_str)
                .map(|v| ("content[]".to_owned(), v.to_owned())),
        );
        fields.push(("my".to_owned(), call.my.unwrap_or_default().to_owned()));
        fields.push((
            "shop_type".to_owned(),
            if call.keywords.is_some_and(|k| !k.is_empty()) {
                "keywords_search".to_owned()
            } else {
                "normal".to_owned()
            },
        ));
        fields.push((
            "options[paid_type]".to_owned(),
            if call.paid { "paid" } else { "free" }.to_owned(),
        ));
        if let Some(kw) = call.keywords.filter(|k| !k.is_empty()) {
            fields.push(("keywords_search[keywords]".to_owned(), kw.to_owned()));
        }
    }
    let encoded = crate::http::urlencode(&fields);
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(timeout as u64);

    let request = async |payload: String| -> Result<Option<Value>, MiratextError> {
        let req = Request::new("POST", ENDPOINT)
            .header("Content-Type", "application/x-www-form-urlencoded")
            .body(payload.into_bytes())
            .timeout(30.0);
        match transport.send(&req).await {
            Ok(response) => {
                let body = response
                    .body
                    .get(..MAX_RESPONSE_BYTES + 1)
                    .unwrap_or(&response.body);
                if body.len() > MAX_RESPONSE_BYTES {
                    return raise("ResponseTooLarge", "Miratext response exceeds 20 MiB");
                }
                match pystr::decode_utf8_strict(body) {
                    Ok(text) => match serde_json::from_str::<Value>(&text) {
                        Ok(v)
                            if v.as_object().is_some_and(|m| {
                                m.get("result").is_some_and(|v| v.is_string())
                            }) =>
                        {
                            Ok(Some(v))
                        }
                        _ => Ok(None),
                    },
                    Err(_) => Ok(None),
                }
            }
            Err(TransportError::Http(_)) | Err(TransportError::Connect(_)) => Ok(None),
            Err(TransportError::InvalidRequest(_)) => Ok(None),
        }
    };

    let mut body = match request(encoded.clone()).await {
        Ok(b) => b,
        Err(e) if e.class == "ResponseTooLarge" => {
            return Ok(json!({
                "ok": false, "state": "unavailable", "reason": "response_too_large",
            }));
        }
        Err(e) => return Err(e),
    };
    let Some(first) = body else {
        return Ok(json!({"ok": false, "state": "failed", "error": "Miratext request failed"}));
    };
    body = Some(first);
    if body
        .as_ref()
        .and_then(|b| b.get("result"))
        .and_then(Value::as_str)
        != Some("ok")
    {
        return Ok(
            json!({"ok": false, "state": "failed", "error": "Miratext rejected the request"}),
        );
    }
    let mut state = body
        .as_ref()
        .and_then(|b| b.get("status"))
        .cloned()
        .unwrap_or_else(|| json!("accepted"));
    if !state.is_string() {
        return Ok(json!({"ok": false, "state": "failed", "error": "malformed Miratext response"}));
    }
    let mut polls = 0u32;
    while matches!(state.as_str(), Some("draft" | "working"))
        && body
            .as_ref()
            .and_then(|b| b.get("hash"))
            .is_some_and(|v| v.is_string())
    {
        if std::time::Instant::now() >= deadline || polls > 0 {
            break;
        }
        // Injected transports never sleep — the real path is handled by `analyze`.
        let resume = crate::http::urlencode(&[
            ("api_key".to_owned(), key.clone()),
            ("check_type".to_owned(), call.check_type.to_owned()),
            (
                "hash".to_owned(),
                body.as_ref()
                    .and_then(|b| b.get("hash"))
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_owned(),
            ),
        ]);
        let resumed = match request(resume).await {
            Ok(r) => r,
            Err(e) if e.class == "ResponseTooLarge" => {
                return Ok(json!({
                    "ok": false, "state": "unavailable", "reason": "response_too_large",
                }));
            }
            Err(e) => return Err(e),
        };
        if resumed
            .as_ref()
            .and_then(|r| r.get("result"))
            .and_then(Value::as_str)
            != Some("ok")
        {
            break;
        }
        let Some(r) = resumed else { break };
        state = r
            .get("status")
            .cloned()
            .unwrap_or_else(|| json!("accepted"));
        if !state.is_string() {
            return Ok(
                json!({"ok": false, "state": "failed", "error": "malformed Miratext response"}),
            );
        }
        body = Some(r);
        polls += 1;
    }
    let working = matches!(state.as_str(), Some("draft" | "working"));
    let mut result = json!({
        "ok": true,
        "state": state,
        "hash": body.as_ref().and_then(|b| b.get("hash")).cloned().unwrap_or(Value::Null),
        "paid": call.paid,
        "resumable": working,
        "wait_exhausted": working,
    });
    if state.as_str() == Some("accepted") {
        let tables = author_tables(
            body.as_ref()
                .and_then(|b| b.get("data"))
                .unwrap_or(&Value::Null),
            top as usize,
        );
        result["author_tables"] = tables;
    }
    if call.paid && hash.is_none() {
        let entry = spend::record(
            host,
            &Record {
                source: "miratext".to_owned(),
                operation: "seo_analysis".to_owned(),
                cost: Some(0.0),
                unit: "unknown".to_owned(),
                task_id: None,
                items: Some(0),
                extra: Some(Map::from_iter([(
                    "cost_unknown".to_owned(),
                    Value::Bool(true),
                )])),
                at: None,
            },
        )
        .map_err(|e| MiratextError {
            class: "OSError",
            message: e.to_string(),
        })?;
        result["spend"] = entry;
    }
    Ok(result)
}

/// `analyze(...)` — start or resume a Miratext analysis through an injected transport.
pub async fn analyze<T: Transport>(
    transport: &T,
    host: &dyn Host,
    root: &Path,
    call: &Call<'_>,
) -> Result<Value, MiratextError> {
    run(transport, host, root, call).await
}
