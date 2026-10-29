//! Split out of `crux`.

use super::*;

/// The keyword arguments of `crux.history`.
#[derive(Debug, Default)]
pub struct History<'a> {
    /// `url` — exactly one of `url`/`origin` must be a non-empty string.
    pub url: Option<&'a str>,
    /// `origin` — exactly one of `url`/`origin` must be a non-empty string.
    pub origin: Option<&'a str>,
    /// `form_factor` — optional form factor filter.
    pub form_factor: Option<&'a str>,
    /// `metrics` — passed through into the payload verbatim when truthy.
    pub metrics: Option<&'a Value>,
    /// `api_key` — falls back to the `crux/api_key` credential when empty.
    pub api_key: Option<&'a str>,
}

/// `crux.history`: reportable CrUX History field trends — a different endpoint and envelope
/// than [`query`], sharing the credential and failure taxonomy.
pub async fn history<T: Transport>(
    host: &dyn Host,
    root: &Path,
    transport: &T,
    args: &History<'_>,
) -> Result<Value, CruxError> {
    if present(args.url) == present(args.origin) {
        return invalid("exactly one of url or origin is required");
    }
    let url_mode = present(args.url);
    let key = match api_token(host, root, args.api_key, |message| {
        Ok(json!({"ok": false, "state": "not_configured", "error": message}))
    }) {
        Ok(token) => token,
        Err(result) => return result,
    };
    let mut payload = Map::new();
    payload.insert(
        if url_mode { "url" } else { "origin" }.to_owned(),
        json!(if url_mode { args.url } else { args.origin }),
    );
    if let Some(ff) = args.form_factor.filter(|f| !f.is_empty()) {
        payload.insert("formFactor".to_owned(), json!(ff));
    }
    if let Some(metrics) = args.metrics.filter(|m| pystr::truthy(Some(m))) {
        payload.insert("metrics".to_owned(), metrics.clone());
    }
    let raw = match post(transport, HISTORY_HOST, &Value::Object(payload), &key).await {
        Ok(raw) => raw,
        Err(FetchFail::TooLarge) => {
            return Ok(json!({"ok": false, "state": "response_too_large", "error": TOO_LARGE}));
        }
        Err(FetchFail::Undecodable) => {
            return Ok(json!({"ok": false, "state": "failed", "error": HISTORY_MALFORMED}));
        }
        Err(FetchFail::Http(response)) => {
            if response.status == 404 {
                return Ok(json!({
                    "ok": true,
                    "state": "no_field_data",
                    "target": if url_mode { args.url } else { args.origin },
                    "records": [],
                }));
            }
            return Ok(json!({
                "ok": false, "state": "failed",
                "error": api_error(&response), "status": response.status,
            }));
        }
        Err(FetchFail::Connect(kind)) => {
            return Ok(json!({
                "ok": false, "state": "failed",
                "error": format!("CrUX request failed: {}", pyerr::url_error(&kind)),
            }));
        }
    };
    if raw.len() > MAX_RESPONSE_BYTES {
        return Ok(json!({"ok": false, "state": "response_too_large", "error": TOO_LARGE}));
    }
    let Some(body) = response_object(&raw) else {
        return Ok(json!({"ok": false, "state": "failed", "error": HISTORY_MALFORMED}));
    };
    let Some(record) = body.get("record").filter(|r| r.is_object()) else {
        return Ok(json!({"ok": false, "state": "failed", "error": HISTORY_MALFORMED}));
    };
    let (Some(Value::Object(key_data)), Some(Value::Object(metric_data))) =
        (record.get("key"), record.get("metrics"))
    else {
        return Ok(json!({"ok": false, "state": "failed", "error": HISTORY_MALFORMED}));
    };
    Ok(json!({
        "ok": true,
        "state": "complete",
        "target": if url_mode { args.url } else { args.origin },
        "target_kind": if url_mode { "url" } else { "origin" },
        "form_factor": key_data.get("formFactor").cloned().unwrap_or(Value::Null),
        "collection_period": record.get("collectionPeriods").cloned().unwrap_or(Value::Null),
        "metric_source": "CrUX History field data",
        "metrics": metric_data,
    }))
}
