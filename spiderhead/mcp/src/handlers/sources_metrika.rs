//! Yandex Metrika handlers (the reference suite): counter listing,
//! counter setup inspection and flat Reporting API records.

use std::path::Path;

use serde_json::{json, Map, Value};

use spiderhead_reports::chromium_pdf::print_to_pdf;
use spiderhead_reports::traffic_dashboard;
use spiderhead_sources::gsc::{search_analytics_pages, PagesArgs, MAX_ANALYTICS_ROWS};
use spiderhead_sources::metrika::{rows_to_records, MetrikaClient, MetrikaEnv, MetrikaFail};
use spiderhead_sources::metrika_traffic::{
    build_traffic_document, normalize_gsc_rows, validate_traffic_input, GscFetch, ReportCall,
    ReportFuture, TrafficInput, LANGUAGES,
};
use spiderhead_sources::pystr;

use crate::error::ToolError;

use super::sources_net::{environment, fail, present, run, transport};
use super::{kwargs, opt, opt_bool, opt_int, opt_str};

/// the reference `except MissingCredential` / `except MetrikaError` arms; anything
/// else escapes to the CLI boundary.
pub(super) fn report_failure(error: MetrikaFail) -> Result<Value, ToolError> {
    match &error {
        MetrikaFail::MissingCredential(message) => Ok(json!({"ok": false, "error": message})),
        MetrikaFail::Metrika(api) => {
            Ok(json!({"ok": false, "error": api.message, "status": api.status}))
        }
        MetrikaFail::Python { .. } => Err(fail(error)),
    }
}

fn get(value: &Value, key: &str) -> Value {
    value.get(key).cloned().unwrap_or(Value::Null)
}

/// `metrika_counters`: counters visible to the token with their report ids.
pub fn metrika_counters(_args: &Value) -> Result<Value, ToolError> {
    let (host, root) = environment();
    let transport = transport()?;
    let env = MetrikaEnv::live(&host, &root, &transport);
    let counters = match MetrikaClient::new(&env, None) {
        Ok(client) => run(client.counters())?,
        Err(error) => Err(error),
    };
    let counters = match counters {
        Ok(counters) => counters,
        Err(error) => return report_failure(error),
    };
    let rows = counters.as_array().cloned().unwrap_or_default();
    Ok(json!({
        "ok": true,
        "count": rows.len(),
        "counters": rows.iter().map(|c| json!({
            "id": get(c, "id"), "name": get(c, "name"),
            "site": get(c, "site"), "status": get(c, "status"),
        })).collect::<Vec<_>>(),
    }))
}

/// `metrika_setup`: goals, filters and operations of one counter.
pub fn metrika_setup(args: &Value) -> Result<Value, ToolError> {
    let args = kwargs(args);
    let Some(counter_id) = opt(&args, "counter_id").filter(|v| pystr::truthy(Some(v))) else {
        return Err(fail("counter_id required"));
    };
    let (host, root) = environment();
    let transport = transport()?;
    let env = MetrikaEnv::live(&host, &root, &transport);
    let collected = run(async {
        let client = MetrikaClient::new(&env, None)?;
        let goals = client.goals(counter_id).await?;
        let filters = client.filters(counter_id).await?;
        let operations = client.operations(counter_id).await?;
        Ok::<_, MetrikaFail>((goals, filters, operations))
    })?;
    let (goals, filters, operations) = match collected {
        Ok(parts) => parts,
        Err(error) => return report_failure(error),
    };
    let goal_rows = goals.as_array().cloned().unwrap_or_default();
    let listed = |body: &Value, key: &str| {
        if pystr::truthy(Some(body)) {
            body.get(key).cloned().unwrap_or(json!([]))
        } else {
            json!([])
        }
    };
    Ok(json!({
        "ok": true,
        "counter_id": counter_id,
        "goals": goal_rows.iter().map(|g| json!({
            "id": get(g, "id"), "name": get(g, "name"), "type": get(g, "type"),
        })).collect::<Vec<_>>(),
        "goals_count": goal_rows.len(),
        "filters": listed(&filters, "filters"),
        "operations": listed(&operations, "operations"),
        "note": if goal_rows.is_empty() {
            json!("no goals are configured, so conversions cannot appear in the data; a zero conversion rate would reflect instrumentation, not observed behavior")
        } else {
            Value::Null
        },
    }))
}

/// `metrika_report`: one Reporting API query as flat JSON records.
pub fn metrika_report(args: &Value) -> Result<Value, ToolError> {
    let args = kwargs(args);
    let counter_id = opt(&args, "counter_id").filter(|v| pystr::truthy(Some(v)));
    let metrics = present(&args, "metrics");
    let (Some(counter_id), Some(metrics)) = (counter_id, metrics) else {
        return Err(fail("counter_id and metrics required"));
    };
    let date1 = opt_str(&args, "date1").unwrap_or("30daysAgo");
    let date2 = opt_str(&args, "date2").unwrap_or("today");
    let mut params = Map::new();
    params.insert("ids".to_owned(), counter_id.clone());
    params.insert("metrics".to_owned(), json!(metrics));
    params.insert("date1".to_owned(), json!(date1));
    params.insert("date2".to_owned(), json!(date2));
    for key in ["dimensions", "filters", "sort"] {
        if let Some(value) = present(&args, key) {
            params.insert(key.to_owned(), json!(value));
        }
    }
    let (host, root) = environment();
    let transport = transport()?;
    let env = MetrikaEnv::live(&host, &root, &transport);
    let paginate = opt_bool(&args, "paginate").unwrap_or(false);
    let limit = opt_int(&args, "limit").unwrap_or(100);
    let body = match MetrikaClient::new(&env, None) {
        Ok(client) => run(client.report(&Value::Object(params), paginate, limit, 1))?,
        Err(error) => Err(error),
    };
    let body = match body {
        Ok(body) => body,
        Err(error) => return report_failure(error),
    };
    let rows = rows_to_records(&body).map_err(fail)?;
    let truthy = |key: &str| body.get(key).filter(|v| pystr::truthy(Some(v))).cloned();
    let accuracy = truthy("accuracy_used").or_else(|| {
        body.get("query")
            .filter(|v| pystr::truthy(Some(v)))
            .and_then(|q| q.get("accuracy").cloned())
    });
    Ok(json!({
        "ok": true,
        "counter_id": counter_id,
        "period": format!("{date1}..{date2}"),
        "total_rows": get(&body, "total_rows"),
        "returned": body.get("data").and_then(Value::as_array).map_or(0, Vec::len),
        "capped": body.get("capped").cloned().unwrap_or(json!(false)),
        "incomplete": body.get("incomplete").cloned().unwrap_or(json!(false)),
        "sampled": get(&body, "sampled"),
        "sample_share": get(&body, "sample_share"),
        "accuracy": accuracy.unwrap_or(Value::Null),
        "split": get(&body, "split"),
        "totals": get(&body, "totals"),
        "rows": rows,
    }))
}

/// `metrika_traffic_pdf`: collect a Metrika traffic document and render it as a
/// dashboard-style HTML/PDF report into `out_dir`.
///
/// Two stages that run separately: collection (`counter_id`, `date1`, `date2`)
/// reads the Reporting API and writes `metrika-traffic.json`; rendering formats
/// a document — just collected, or one passed as `document` — into
/// `metrika-traffic.html` and, when a Chromium-family executable is found,
/// `metrika-traffic.pdf`. Rendering alone makes no network request.
#[allow(clippy::too_many_lines)]
pub fn metrika_traffic_pdf(args: &Value) -> Result<Value, ToolError> {
    let args = kwargs(args);
    let Some(out_dir) = present(&args, "out_dir") else {
        return Err(fail(
            "out_dir required: the directory that receives the report files",
        ));
    };
    let document_arg = opt(&args, "document");
    let counter_id = opt(&args, "counter_id");
    if document_arg.is_some() && counter_id.is_some() {
        return Err(fail(
            "pass either document (render only) or counter_id (collect), not both",
        ));
    }
    let gsc_rows_arg = opt(&args, "gsc_rows");
    let gsc_site_url = opt_str(&args, "gsc_site_url").filter(|v| !v.is_empty());
    if gsc_rows_arg.is_some() && gsc_site_url.is_some() {
        return Err(fail("pass either gsc_rows or gsc_site_url, not both"));
    }
    let lang = opt_str(&args, "lang");
    if let Some(lang) = lang {
        if !LANGUAGES.contains(&lang) {
            let list = LANGUAGES
                .iter()
                .map(|l| format!("'{l}'"))
                .collect::<Vec<_>>()
                .join(", ");
            return Err(fail(format!("lang must be one of [{list}]")));
        }
    }
    let timeout = match opt(&args, "timeout") {
        None => 120.0,
        Some(value @ Value::Number(_)) => value.as_f64().unwrap_or(0.0),
        Some(_) => -1.0,
    };
    if timeout <= 0.0 {
        return Err(fail("timeout must be a positive number of seconds"));
    }
    let render = opt_bool(&args, "render").unwrap_or(true);
    let pdf = opt_bool(&args, "pdf").unwrap_or(true);
    let overwrite = opt_bool(&args, "overwrite").unwrap_or(false);
    // `load_brand` runs before any collection so a bad brand fails fast.
    traffic_dashboard::load_brand(opt(&args, "brand")).map_err(fail)?;

    let collected = document_arg.is_none();
    let mut document = match document_arg {
        Some(Value::String(path)) => {
            let source = Path::new(path);
            if !source.is_file() {
                return Err(fail(format!("document file not found: {path}")));
            }
            let body = match std::fs::read_to_string(source) {
                Ok(body) => body,
                Err(error) if error.kind() == std::io::ErrorKind::InvalidData => {
                    return Err(fail(format!("document is not valid JSON: {error}")));
                }
                Err(error) => return Err(fail(error)),
            };
            let document: Value = serde_json::from_str(&body)
                .map_err(|error| fail(format!("document is not valid JSON: {error}")))?;
            traffic_dashboard::validate_document(&document).map_err(fail)?;
            document
        }
        Some(other) => {
            traffic_dashboard::validate_document(other).map_err(fail)?;
            other.clone()
        }
        None => Value::Null,
    };

    let mut gsc_rows: Option<Value> = None;
    if collected {
        let site_label = opt(&args, "site_label").cloned();
        let mut input = TrafficInput {
            counter_id: counter_id.cloned().unwrap_or(Value::Null),
            date1: opt(&args, "date1").cloned().unwrap_or(Value::Null),
            date2: opt(&args, "date2").cloned().unwrap_or(Value::Null),
            attribution: opt(&args, "attribution")
                .cloned()
                .unwrap_or_else(|| json!("last_significant")),
            traffic: opt(&args, "traffic")
                .cloned()
                .unwrap_or_else(|| json!("organic")),
            lang: opt(&args, "lang").cloned().unwrap_or_else(|| json!("en")),
            top: opt(&args, "top").cloned().unwrap_or_else(|| json!(15)),
            filters: opt(&args, "filters").cloned(),
            site_label: site_label.clone(),
            gsc_rows: gsc_rows_arg.cloned(),
            gsc_fetch: None,
            now: None,
        };
        validate_traffic_input(&input).map_err(fail)?;
        if let Some(rows) = gsc_rows_arg {
            gsc_rows = Some(json!(normalize_gsc_rows(rows).map_err(fail)?));
            input.gsc_rows = gsc_rows.clone();
        }
    }

    let directory = Path::new(out_dir);
    let basename = "metrika-traffic";
    let document_target = collected.then(|| directory.join(format!("{basename}.json")));
    let html_target = render.then(|| directory.join(format!("{basename}.html")));
    let pdf_target = (render && pdf).then(|| directory.join(format!("{basename}.pdf")));
    let mut existing: Vec<String> = [&document_target, &html_target, &pdf_target]
        .into_iter()
        .flatten()
        .filter(|p| p.exists())
        .map(|p| p.to_string_lossy().to_string())
        .collect();
    existing.sort();
    if !existing.is_empty() && !overwrite {
        return Ok(json!({
            "ok": false,
            "error": "report files already exist; pass overwrite=true to replace them",
            "existing": existing,
        }));
    }

    if collected {
        let (host, root) = environment();
        let transport = transport()?;
        let env = MetrikaEnv::live(&host, &root, &transport);
        let client = match MetrikaClient::new(&env, None) {
            Ok(client) => client,
            Err(MetrikaFail::MissingCredential(message)) => {
                return Ok(json!({"ok": false, "error": message}));
            }
            Err(error) => return Err(fail(error)),
        };
        // The synchronous `gsc_fetch` seam runs inside the collection future, so
        // a nested runtime is not an option: the Search Console call gets its own
        // thread and transport, exactly like the reference path that uses the gsc
        // module's own default transport rather than the Metrika client's.
        let gsc_site = gsc_site_url.map(str::to_owned);
        let gsc_fetch = move |start: &str, end: &str| -> Result<Value, String> {
            let site = gsc_site.clone().unwrap_or_default();
            let (start, end) = (start.to_owned(), end.to_owned());
            std::thread::spawn(move || {
                let (host, root) = environment();
                let transport = super::net::ProviderTransport::new().map_err(|e| e.to_string())?;
                super::net::block_on(search_analytics_pages(
                    &host,
                    &root,
                    &site,
                    &PagesArgs {
                        start_date: Some(&start),
                        end_date: Some(&end),
                        dimensions: Some(json!(["query"])),
                        row_limit: 1_000,
                        max_rows: MAX_ANALYTICS_ROWS,
                        token: None,
                        now: None,
                        service_account: None,
                    },
                    &transport,
                ))?
                .map_err(|error| error.to_string())
            })
            .join()
            .map_err(|_| "Search Console worker panicked".to_owned())?
        };
        let gsc_fetch_ref: &GscFetch = &gsc_fetch;
        let input = TrafficInput {
            counter_id: counter_id.cloned().unwrap_or(Value::Null),
            date1: opt(&args, "date1").cloned().unwrap_or(Value::Null),
            date2: opt(&args, "date2").cloned().unwrap_or(Value::Null),
            attribution: opt(&args, "attribution")
                .cloned()
                .unwrap_or_else(|| json!("last_significant")),
            traffic: opt(&args, "traffic")
                .cloned()
                .unwrap_or_else(|| json!("organic")),
            lang: opt(&args, "lang").cloned().unwrap_or_else(|| json!("en")),
            top: opt(&args, "top").cloned().unwrap_or_else(|| json!(15)),
            filters: opt(&args, "filters").cloned(),
            site_label: opt(&args, "site_label").cloned(),
            gsc_rows,
            gsc_fetch: gsc_site_url.map(|_| gsc_fetch_ref),
            now: None,
        };
        let report_impl = |params: Map<String, Value>, limit: i64, offset: i64| -> ReportFuture {
            let client = &client;
            Box::pin(async move {
                client
                    .report(&Value::Object(params), false, limit, offset)
                    .await
            })
        };
        let report_call: &ReportCall = &report_impl;
        document = run(build_traffic_document(&input, report_call))?
            .map_err(|error| fail(error.to_string()))?;
        if document.get("ok") != Some(&json!(true)) {
            let requests = document
                .get("requests")
                .or_else(|| document.get("methodology").and_then(|m| m.get("requests")))
                .cloned()
                .unwrap_or(Value::Null);
            return Ok(json!({
                "ok": false,
                "error": document
                    .get("error")
                    .filter(|e| pystr::truthy(Some(e)))
                    .cloned()
                    .unwrap_or_else(|| json!("traffic summary unavailable")),
                "requests": requests,
            }));
        }
    } else if let Some(site_label) = opt(&args, "site_label").filter(|v| pystr::truthy(Some(v))) {
        document["site_label"] = site_label.clone();
    }

    std::fs::create_dir_all(directory).map_err(|error| fail(error.to_string()))?;
    let mut files = json!({"document": null, "html": null, "pdf": null});
    if let Some(target) = &document_target {
        // `json.dumps(document, ensure_ascii=False, indent=2) + "\n"` verbatim.
        let text = format!(
            "{}\n",
            spiderhead_projects::pyjson::dumps_indent2_ordered(&document)
        );
        write_text_atomic(target, &text)?;
        files["document"] = json!(target.to_string_lossy());
    }
    let mut pdf_result = json!({"status": "skipped", "reason": "rendering was not requested"});
    if let Some(html_target) = &html_target {
        let html =
            traffic_dashboard::render_html(&document, opt(&args, "brand"), lang).map_err(fail)?;
        write_text_atomic(html_target, &html)?;
        files["html"] = json!(html_target.to_string_lossy());
        match &pdf_target {
            None => {
                pdf_result = json!({"status": "skipped", "reason": "pdf=false was requested"});
            }
            Some(pdf_target) => {
                pdf_result = print_to_pdf(html_target, pdf_target, None, timeout);
                if pdf_result.get("status") == Some(&json!("ok")) {
                    files["pdf"] = json!(pdf_target.to_string_lossy());
                }
            }
        }
    }
    let blocks = document
        .get("blocks")
        .and_then(Value::as_object)
        .map(|blocks| {
            blocks
                .iter()
                .filter(|(_, block)| block.is_object())
                .map(|(name, block)| {
                    (
                        name.clone(),
                        json!({
                            "status": block.get("status").cloned().unwrap_or(Value::Null),
                            "reason": block.get("reason").cloned().unwrap_or(Value::Null),
                        }),
                    )
                })
                .collect::<Map<String, Value>>()
        })
        .unwrap_or_default();
    let mode = match (collected, render) {
        (true, true) => "collect_and_render",
        (true, false) => "collect",
        (false, _) => "render",
    };
    Ok(json!({
        "ok": true,
        "schema": document.get("schema").cloned().unwrap_or(Value::Null),
        "mode": mode,
        "period": document.get("period").cloned().unwrap_or(Value::Null),
        "attribution": document.get("attribution").cloned().unwrap_or(Value::Null),
        "traffic": document.get("traffic").cloned().unwrap_or(Value::Null),
        "files": files,
        "pdf": pdf_result,
        "blocks": Value::Object(blocks),
        "warnings": document.get("warnings").cloned().unwrap_or_else(|| json!([])),
        "requests": if collected {
            document
                .get("methodology")
                .and_then(|m| m.get("requests"))
                .cloned()
                .unwrap_or(Value::Null)
        } else {
            json!(0)
        },
    }))
}

/// `atomic_write_bytes` for text: one file appears fully written or not at all.
fn write_text_atomic(path: &Path, text: &str) -> Result<(), ToolError> {
    spiderhead_core::filesystem::atomic_write_bytes(path, text.as_bytes(), None, None, true)
        .map_err(|error| fail(error.to_string()))
}
