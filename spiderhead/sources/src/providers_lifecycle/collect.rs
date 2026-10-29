//! Split out of `providers_lifecycle`.

use super::*;

/// `provider_collect(provider, operation, request, *, artifact_dir, event_sink)`.
pub fn provider_collect(
    env: &ProviderEnv,
    provider: &str,
    operation: &str,
    request: &Value,
    artifact_dir: Option<&str>,
    event_sink: Option<providers::EventSink>,
) -> Result<Value, CfError> {
    let contract = operation_contract(provider, operation);
    match contract.get("state").and_then(Value::as_str) {
        Some("dedicated") => {
            return Err(CfError::Value(
                "this provider operation uses its existing dedicated route".to_owned(),
            ))
        }
        Some("dedicated_write") => {
            return Err(CfError::Value(
                "this provider operation is a separately confirmed write action".to_owned(),
            ))
        }
        Some("supported") => {}
        _ => return Err(CfError::Value("unsupported provider operation".to_owned())),
    }
    if !request.is_object() {
        return Err(CfError::Value("request must be an object".to_owned()));
    }
    if let Some(sink) = event_sink {
        sink(
            "provider_enrichment",
            json!({"provider": provider, "operation": operation, "state": "started", "rows": 0}),
        );
    }
    let result: Value = match provider {
        "gsc" => match operation {
            "properties" => dispatch_value(env, "gsc.discover_properties", None)?,
            "search_analytics" => dispatch_value(
                env,
                "gsc.search_analytics_pages",
                Some(&json!({"kwargs": request})),
            )?,
            "inspection" => {
                dispatch_value(env, "gsc.inspect_urls", Some(&json!({"kwargs": request})))?
            }
            "sitemaps" => {
                dispatch_value(env, "gsc.sitemap_status", Some(&json!({"kwargs": request})))?
            }
            _ => provider_verify(env, &json!("gsc"), Some(request))?,
        },
        "crux" => {
            let route = if operation == "history" {
                "crux.history"
            } else {
                "crux.query"
            };
            dispatch_value(env, route, Some(&json!({"kwargs": request})))?
        }
        "pagespeed" => dispatch_value(
            env,
            "pagespeed.sample",
            Some(&json!({"desktop": operation == "desktop_samples", "kwargs": request})),
        )?,
        "ga4" => {
            if operation == "page_views" {
                dispatch_value(
                    env,
                    "ga4_content.page_views",
                    Some(&json!({"kwargs": request})),
                )?
            } else {
                dispatch_value(env, "ga4.landing_pages", Some(&json!({"kwargs": request})))?
            }
        }
        "yandex_webmaster" => dispatch_value(
            env,
            "yandex_webmaster.collect",
            Some(&json!({"operation": operation, "kwargs": request})),
        )?,
        "bing_webmaster" => dispatch_value(
            env,
            "bing_webmaster.collect",
            Some(&json!({"operation": operation, "kwargs": request})),
        )?,
        "dataforseo_backlinks" => dispatch_value(
            env,
            "dataforseo.backlink_call",
            Some(&json!({"operation": operation, "kwargs": request})),
        )?,
        "wayback" => dispatch_value(env, "wayback.history", Some(&json!({"kwargs": request})))?,
        "crtsh" => dispatch_value(env, "crtsh.subdomains", Some(&json!({"kwargs": request})))?,
        "cloudflare" => match operation {
            "traffic" => {
                dispatch_value(env, "cloudflare.traffic", Some(&json!({"kwargs": request})))?
            }
            "logpull" => {
                dispatch_value(env, "cloudflare.logpull", Some(&json!({"kwargs": request})))?
            }
            _ => dispatch_value(
                env,
                "cloudflare_history.list_snapshots",
                Some(&json!({"kwargs": request})),
            )?,
        },
        "metrika" => {
            if !matches!(operation, "counters" | "aggregate_report") {
                return Err(CfError::Value(
                    "Metrika raw Logs API is intentionally unreachable".to_owned(),
                ));
            }
            match metrika_collect(env, operation, request) {
                Ok(value) => value,
                Err(error) if error.missing_credential => {
                    json!({"ok": false, "state": "not_configured", "error": error.message})
                }
                Err(error) if error.class == "MetrikaError" => {
                    let mut out = json!({"ok": false, "state": "failed", "error": error.message});
                    if let Some(status) = error.status {
                        out["status"] = json!(status);
                    }
                    out
                }
                Err(error) => return Err(error.into()),
            }
        }
        _ => return Err(CfError::Value("unsupported provider operation".to_owned())),
    };
    let mut ev = evidence(env, provider, operation, request, &result, None)?;
    let artifact = if let Some(dir) = artifact_dir {
        Some(save_local_artifact(
            dir,
            &json!({"evidence": ev, "result": result}),
        )?)
    } else {
        None
    };
    ev["artifact_reference"] = artifact.clone().map(Value::from).unwrap_or(Value::Null);
    if let Some(sink) = event_sink {
        let rows = ["rows", "samples", "url_facts", "summary"]
            .iter()
            .filter_map(|k| result.get(k))
            .find(|v| pystr::truthy(Some(v)))
            .cloned()
            .unwrap_or(json!([]));
        let row_count = match &rows {
            Value::Array(a) => a.len() as i64,
            Value::Object(o) => o.len() as i64,
            Value::String(t) => t.chars().count() as i64,
            _ => 0,
        };
        let returned = result
            .get("returned")
            .and_then(Value::as_i64)
            .unwrap_or(row_count);
        sink(
            "provider_enrichment",
            json!({
                "provider": provider,
                "operation": operation,
                "state": result.get("state").filter(|v| pystr::truthy(Some(v))).map(|v| pyjson::str_value(v).unwrap_or_default()).unwrap_or_else(|| "unknown".to_owned()),
                "rows": returned,
            }),
        );
    }
    Ok(json!({"evidence": ev, "result": Value::Null}))
}

pub(super) fn metrika_collect(
    env: &ProviderEnv,
    operation: &str,
    request: &Value,
) -> Result<Value, DispatchError> {
    dispatch(env, "metrika.client", None)?;
    if operation == "counters" {
        let counters = dispatch(env, "metrika.counters", None)?;
        let rows = counters.as_array().cloned().unwrap_or_default();
        return Ok(json!({
            "ok": true,
            "state": "complete",
            "returned": rows.len(),
            "rows": rows,
        }));
    }
    for key in ["counter_id", "metrics", "date1", "date2"] {
        if !request.as_object().is_some_and(|m| m.contains_key(key)) {
            return Err(DispatchError::plain(
                "ValueError",
                "aggregate_report requires counter_id, metrics, date1, and date2",
            ));
        }
    }
    let limit = pystr::to_python_int(request.get("limit").unwrap_or(&json!(100)))
        .map_err(|(class, msg)| DispatchError::plain(class, &msg))?;
    let mut report_body = json!({
        "ids": request["counter_id"],
        "metrics": request["metrics"],
        "date1": request["date1"],
        "date2": request["date2"],
    });
    if let Some(dimensions) = request.get("dimensions").filter(|v| pystr::truthy(Some(v))) {
        report_body["dimensions"] = dimensions.clone();
    }
    let body = dispatch(
        env,
        "metrika.report",
        Some(&json!({
            "body": report_body,
            "paginate": pystr::truthy(request.get("paginate")),
            "limit": limit,
        })),
    )?;
    let rows = dispatch(env, "metrika.rows_to_records", Some(&json!({"body": body})))?;
    Ok(json!({
        "ok": true,
        "state": if pystr::truthy(body.get("capped")) || pystr::truthy(body.get("incomplete")) { "partial" } else { "complete" },
        "period": {"start_date": request["date1"], "end_date": request["date2"]},
        "rows": rows,
        "returned": body.get("data").and_then(Value::as_array).map(|d| d.len()).unwrap_or(0),
        "truncated": pystr::truthy(body.get("capped")),
        "incomplete": pystr::truthy(body.get("incomplete")),
        "sampled": body.get("sampled").cloned().unwrap_or(Value::Null),
        "sample_share": body.get("sample_share").cloned().unwrap_or(Value::Null),
        "accuracy": body.get("accuracy_used").filter(|v| pystr::truthy(Some(v))).cloned().or_else(|| body.get("query").and_then(|q| q.get("accuracy")).cloned()).unwrap_or(Value::Null),
        "split": body.get("split").cloned().unwrap_or(Value::Null),
    }))
}
