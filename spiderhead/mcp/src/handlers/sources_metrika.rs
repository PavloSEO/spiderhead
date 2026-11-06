//! Yandex Metrika handlers (`mcp/handlers/traffic.py`): counter listing,
//! counter setup inspection, flat Reporting API records and the traffic
//! document's collect/render/write pipeline.

use std::path::Path;

use serde_json::{json, Map, Value};

use spiderhead_reports::{chromium_pdf, traffic_dashboard};
use spiderhead_sources::metrika::{rows_to_records, MetrikaClient, MetrikaEnv, MetrikaFail};
use spiderhead_sources::metrika_traffic::{self, ReportFuture, TrafficInput, LANGUAGES};
use spiderhead_sources::{gsc, pyjson, pystr};

use crate::error::ToolError;

use super::net::block_on;
use super::sources_net::{environment, fail, present, run, transport};
use super::{kwargs, opt, opt_bool, opt_int, opt_str};

/// Python's `except MissingCredential` / `except MetrikaError` arms; anything
/// else escapes to the CLI boundary.
pub(super) fn report_failure(error: MetrikaFail) -> Result<Value, ToolError> {
    match &error {
        MetrikaFail::MissingCredential(message) => Ok(json!({"ok": false, "error": message})),
        MetrikaFail::Metrika(api) => {
            Ok(json!({"ok": false, "error": api.message, "status": api.status}))
        }
        MetrikaFail::Classed { .. } => Err(fail(error)),
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

/// `metrika-traffic` — the document/report basename.
const TRAFFIC_BASENAME: &str = "metrika-traffic";

/// `Path(value)` for a JSON value: only strings are paths; the message is the
/// `TypeError` CPython's `pathlib` raises for anything else.
fn path_arg(value: &Value) -> Result<std::path::PathBuf, ToolError> {
    match value {
        Value::String(text) => Ok(std::path::PathBuf::from(text)),
        other => Err(fail(format!(
            "argument should be a str or an os.PathLike object where __fspath__ returns a str, \
             not '{}'",
            pystr::type_name(other)
        ))),
    }
}

/// `read_text` + `json.loads` of the `--document` file with CPython's messages.
fn document_json(path: &str) -> Result<Value, ToolError> {
    let source = Path::new(path);
    if !source.is_file() {
        return Err(fail(format!("document file not found: {path}")));
    }
    let bytes = std::fs::read(source).map_err(|error| {
        fail(spiderhead_core::common::python_os_error(
            &error,
            Some(source),
        ))
    })?;
    let text = match String::from_utf8(bytes) {
        Ok(text) => text,
        Err(error) => {
            let utf8 = error.utf8_error();
            let at = utf8.valid_up_to();
            let bad = error.as_bytes()[at];
            let reason = if utf8.error_len().is_none() {
                "unexpected end of data"
            } else if bad >= 0xc2 {
                "invalid continuation byte"
            } else {
                "invalid start byte"
            };
            return Err(fail(format!(
                "document is not valid JSON: 'utf-8' codec can't decode byte 0x{bad:02x} in \
                 position {at}: {reason}"
            )));
        }
    };
    serde_json::from_str(&text).map_err(|error| {
        fail(format!(
            "document is not valid JSON: {}",
            pyjson::decode_error(&text).unwrap_or_else(|| error.to_string())
        ))
    })
}

/// The file writes of the handler are atomic, like `_write_text_atomic`.
fn write_text(path: &Path, text: &str) -> Result<(), ToolError> {
    spiderhead_core::filesystem::atomic_write_bytes(path, text.as_bytes(), None, None, true)
        .map_err(|error| fail(spiderhead_core::common::python_os_error(&error, Some(path))))
}

/// `metrika_traffic_pdf`: collect a traffic document and/or render the
/// dashboard HTML + PDF into `out_dir`. The argument rules are checked in the
/// handler's own order so each `ValueError` text lands exactly once.
pub fn metrika_traffic_pdf(args: &Value) -> Result<Value, ToolError> {
    // Raw map — an explicit `null` reaches Python as `None`, which differs from
    // "absent" for every argument whose default is not `None`.
    let raw = args.as_object().cloned().unwrap_or_default();
    let out_dir = opt(&raw, "out_dir").filter(|v| pystr::truthy(Some(v)));
    let Some(out_dir) = out_dir else {
        return Err(fail(
            "out_dir required: the directory that receives the report files",
        ));
    };
    let document_arg = opt(&raw, "document");
    let counter_id = opt(&raw, "counter_id");
    if document_arg.is_some() && counter_id.is_some() {
        return Err(fail(
            "pass either document (render only) or counter_id (collect), not both",
        ));
    }
    let gsc_rows = opt(&raw, "gsc_rows");
    let gsc_site_url = opt(&raw, "gsc_site_url");
    if gsc_rows.is_some() && gsc_site_url.is_some_and(|v| pystr::truthy(Some(v))) {
        return Err(fail("pass either gsc_rows or gsc_site_url, not both"));
    }
    let lang = opt(&raw, "lang");
    if let Some(lang) = lang {
        let ok = matches!(lang, Value::String(text) if LANGUAGES.contains(&text.as_str()));
        if !ok {
            return Err(fail("lang must be one of ['en', 'ru']"));
        }
    }
    let timeout_value = raw.get("timeout").cloned().unwrap_or(json!(120.0));
    let timeout = match &timeout_value {
        Value::Number(number) => number.as_f64().unwrap_or_default(),
        _ => -1.0,
    };
    if !timeout.is_finite() || timeout <= 0.0 {
        return Err(fail("timeout must be a positive number of seconds"));
    }
    // `load_brand` validates before the document is even looked at.
    traffic_dashboard::load_brand(opt(&raw, "brand")).map_err(fail)?;

    let collected = document_arg.is_none();
    let mut document = match document_arg {
        Some(Value::String(path)) => document_json(path)?,
        Some(other) => other.clone(),
        None => Value::Null,
    };
    if collected {
        let input = TrafficInput {
            counter_id: counter_id.cloned().unwrap_or(Value::Null),
            date1: opt(&raw, "date1").cloned().unwrap_or(Value::Null),
            date2: opt(&raw, "date2").cloned().unwrap_or(Value::Null),
            attribution: raw
                .get("attribution")
                .cloned()
                .unwrap_or(json!("last_significant")),
            traffic: raw.get("traffic").cloned().unwrap_or(json!("organic")),
            lang: lang.cloned().unwrap_or(json!("en")),
            top: raw.get("top").cloned().unwrap_or(json!(15)),
            filters: opt(&raw, "filters").cloned(),
            site_label: opt(&raw, "site_label").cloned(),
            gsc_rows: gsc_rows.cloned(),
            gsc_fetch: None,
            now: None,
        };
        metrika_traffic::validate_traffic_input(&input).map_err(fail)?;
        if let Some(rows) = gsc_rows {
            metrika_traffic::normalize_gsc_rows(rows).map_err(fail)?;
        }
    } else {
        traffic_dashboard::validate_document(&document).map_err(fail)?;
    }

    // `Path(out_dir)` happens only now in Python — after every check above.
    let directory = path_arg(out_dir)?;
    let render = raw.get("render").is_none_or(|v| pystr::truthy(Some(v)));
    let pdf = raw.get("pdf").is_none_or(|v| pystr::truthy(Some(v)));
    let overwrite = raw.get("overwrite").is_some_and(|v| pystr::truthy(Some(v)));
    let doc_path = directory.join(format!("{TRAFFIC_BASENAME}.json"));
    let html_path = directory.join(format!("{TRAFFIC_BASENAME}.html"));
    let pdf_path = directory.join(format!("{TRAFFIC_BASENAME}.pdf"));
    let mut targets: Vec<&Path> = Vec::new();
    if collected {
        targets.push(&doc_path);
    }
    if render {
        targets.push(&html_path);
        if pdf {
            targets.push(&pdf_path);
        }
    }
    let mut existing: Vec<String> = targets
        .iter()
        .filter(|path| path.exists())
        .map(|path| path.to_string_lossy().into_owned())
        .collect();
    existing.sort();
    if !existing.is_empty() && !overwrite {
        return Ok(json!({
            "ok": false,
            "error": "report files already exist; pass overwrite=true to replace them",
            "existing": existing,
        }));
    }

    let (host, root) = environment();
    let transport = transport()?;
    if collected {
        let env = MetrikaEnv::live(&host, &root, &transport);
        let client = match MetrikaClient::new(&env, None) {
            Ok(client) => client,
            Err(error) => return report_failure(error),
        };
        // `client.report(params, limit=..., offset=...)` with pagination off.
        let report_call =
            |params: Map<String, Value>, limit: i64, offset: i64| -> ReportFuture<'_> {
                let client = &client;
                Box::pin(async move {
                    client
                        .report(&Value::Object(params), false, limit, offset)
                        .await
                })
            };
        let gsc_fetch = gsc_site_url.filter(|v| pystr::truthy(Some(v))).map(|site| {
            let site = site.clone();
            let root = root.clone();
            move |start: &str, end: &str| -> Result<Value, String> {
                // `search_analytics_pages` is async while this seam is sync;
                // a worker thread gets its own runtime.
                let host = spiderhead_core::host::SystemHost;
                let root = root.clone();
                let site = site.clone();
                let (start, end) = (start.to_owned(), end.to_owned());
                std::thread::spawn(move || {
                    let transport = super::sources_net::transport().map_err(|e| e.to_string())?;
                    let site = match site.as_str() {
                        Some(site) => site.to_owned(),
                        None => return Err("quote_from_bytes() expected bytes".to_owned()),
                    };
                    let page_args = gsc::PagesArgs {
                        start_date: Some(&start),
                        end_date: Some(&end),
                        dimensions: Some(json!(["query"])),
                        row_limit: 1000,
                        max_rows: gsc::MAX_ANALYTICS_ROWS,
                        token: None,
                        now: None,
                        service_account: None,
                    };
                    block_on(gsc::search_analytics_pages(
                        &host, &root, &site, &page_args, &transport,
                    ))
                    .and_then(|result| result.map_err(|e| e.to_string()))
                })
                .join()
                .unwrap_or_else(|_| Err("Search Console fetch failed".to_owned()))
            }
        });
        type GscFetch<'a> = dyn Fn(&str, &str) -> Result<Value, String> + 'a;
        let gsc_fetch: Option<Box<GscFetch<'_>>> = gsc_fetch.map(|f| Box::new(f) as _);
        let input = TrafficInput {
            counter_id: counter_id.cloned().unwrap_or(Value::Null),
            date1: opt(&raw, "date1").cloned().unwrap_or(Value::Null),
            date2: opt(&raw, "date2").cloned().unwrap_or(Value::Null),
            attribution: raw
                .get("attribution")
                .cloned()
                .unwrap_or(json!("last_significant")),
            traffic: raw.get("traffic").cloned().unwrap_or(json!("organic")),
            lang: lang.cloned().unwrap_or(json!("en")),
            top: raw.get("top").cloned().unwrap_or(json!(15)),
            filters: opt(&raw, "filters").cloned(),
            site_label: opt(&raw, "site_label").cloned(),
            gsc_rows: gsc_rows.cloned(),
            gsc_fetch: gsc_fetch.as_deref(),
            now: None,
        };
        document = match run(metrika_traffic::build_traffic_document(
            &input,
            &report_call,
        ))? {
            Ok(document) => document,
            Err(error) => return report_failure(error),
        };
        if !pystr::truthy(document.get("ok")) {
            let error = document
                .get("error")
                .filter(|v| pystr::truthy(Some(v)))
                .cloned()
                .unwrap_or(json!("traffic summary unavailable"));
            let requests = document
                .get("requests")
                .filter(|v| pystr::truthy(Some(v)))
                .cloned()
                .or_else(|| {
                    document
                        .get("methodology")
                        .and_then(|m| m.get("requests"))
                        .cloned()
                })
                .unwrap_or(Value::Null);
            return Ok(json!({"ok": false, "error": error, "requests": requests}));
        }
    } else if let Some(label) = opt(&raw, "site_label").filter(|v| pystr::truthy(Some(v))) {
        if let Some(object) = document.as_object_mut() {
            object.insert("site_label".to_owned(), label.clone());
        }
    }

    std::fs::create_dir_all(&directory).map_err(|error| {
        fail(spiderhead_core::common::python_os_error(
            &error,
            Some(&directory),
        ))
    })?;
    let mut files = Map::new();
    for key in ["document", "html", "pdf"] {
        files.insert(key.to_owned(), Value::Null);
    }
    if collected {
        let text = spiderhead_sf_import::verification::python_dumps_indent(&document, 2) + "\n";
        write_text(&doc_path, &text)?;
        files.insert(
            "document".to_owned(),
            json!(doc_path.to_string_lossy().into_owned()),
        );
    }
    let mut pdf_result = json!({"status": "skipped", "reason": "rendering was not requested"});
    if render {
        let html = traffic_dashboard::render_html(
            &document,
            opt(&raw, "brand"),
            lang.and_then(Value::as_str),
        )
        .map_err(fail)?;
        write_text(&html_path, &html)?;
        files.insert(
            "html".to_owned(),
            json!(html_path.to_string_lossy().into_owned()),
        );
        if !pdf {
            pdf_result = json!({"status": "skipped", "reason": "pdf=false was requested"});
        } else {
            pdf_result = chromium_pdf::print_to_pdf(&html_path, &pdf_path, None, timeout);
            if pdf_result.get("status") == Some(&json!("ok")) {
                files.insert(
                    "pdf".to_owned(),
                    json!(pdf_path.to_string_lossy().into_owned()),
                );
            }
        }
    }
    let blocks = document.get("blocks").and_then(Value::as_object);
    let block_status = blocks.map(|blocks| {
        blocks
            .iter()
            .filter(|(_, block)| block.is_object())
            .map(|(name, block)| {
                (
                    name.clone(),
                    json!({
                        "status": get(block, "status"),
                        "reason": get(block, "reason"),
                    }),
                )
            })
            .collect::<Map<String, Value>>()
    });
    Ok(json!({
        "ok": true,
        "schema": get(&document, "schema"),
        "mode": if collected && render {
            "collect_and_render"
        } else if collected {
            "collect"
        } else {
            "render"
        },
        "period": get(&document, "period"),
        "attribution": get(&document, "attribution"),
        "traffic": get(&document, "traffic"),
        "files": Value::Object(files),
        "pdf": pdf_result,
        "blocks": block_status.map(Value::Object).unwrap_or_else(|| json!({})),
        "warnings": document
            .get("warnings")
            .filter(|v| pystr::truthy(Some(v)))
            .cloned()
            .unwrap_or_else(|| json!([])),
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
