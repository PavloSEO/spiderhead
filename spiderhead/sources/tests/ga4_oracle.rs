//! Oracle replay for the reference suite's fixture — `run_report`, `landing_pages`,
//! `page_views` driven with scripted transports and credential seams, matching
//! the reference own monkeypatched tests.

mod common;

use std::cell::{Cell, RefCell};
use std::collections::VecDeque;

use serde_json::{json, Value};
use spiderhead_core::host::FakeHost;
use spiderhead_sources::credentials::CredentialError;
use spiderhead_sources::ga4::{
    self, Ga4Env, Ga4Error, Ga4Fault, Ga4Transport, LandingArgs, ReportArgs,
};
use spiderhead_sources::ga4_content::{self, PageViewsArgs};
use spiderhead_sources::http::Sleeper;
use spiderhead_sources::pyerr::reason_phrase;
use spiderhead_sources::{pyjson, pystr};

use common::{absent_config_root, StubTransport};

const PAGE_DIMENSIONS: [&str; 3] = ["date", "hostName", "pagePathPlusQueryString"];
const PAGE_METRICS: [&str; 1] = ["screenPageViews"];

/// The fixture's `report_body` — headers + tuple/dict rows + rowCount + metadata +
/// propertyQuota, emitted via reference-default `json.dumps` separators.
fn report_body(spec: &Value, offset: Option<usize>, limit: usize, index: usize) -> String {
    if let Some(raw) = spec.get("raw").and_then(Value::as_str) {
        return raw.to_owned();
    }
    let dims = spec
        .get("dims")
        .cloned()
        .unwrap_or_else(|| json!(PAGE_DIMENSIONS));
    let mets = spec
        .get("mets")
        .cloned()
        .unwrap_or_else(|| json!(PAGE_METRICS));
    let dims_len = dims.as_array().map_or(0, Vec::len);
    let all_rows = spec
        .get("rows")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    let rows = match offset {
        Some(start) => all_rows
            .iter()
            .skip(start)
            .take(limit)
            .cloned()
            .collect::<Vec<_>>(),
        None => all_rows,
    };
    let body_rows: Vec<Value> = rows
        .iter()
        .map(|row| {
            if row.is_object() {
                row.clone()
            } else {
                let values = row.as_array().cloned().unwrap_or_default();
                json!({
                    "dimensionValues": values.iter().take(dims_len).map(|v| json!({"value": v})).collect::<Vec<_>>(),
                    "metricValues": values.iter().skip(dims_len).map(|v| json!({"value": pystr::repr_value(v)})).collect::<Vec<_>>(),
                })
            }
        })
        .collect();
    let total = spec
        .get("totals")
        .and_then(Value::as_array)
        .and_then(|t| t.get(index))
        .cloned()
        .or_else(|| spec.get("total").cloned())
        .unwrap_or_else(|| json!(rows.len() + offset.unwrap_or(0)));
    let zone = spec
        .get("zones")
        .and_then(Value::as_array)
        .and_then(|z| z.get(index))
        .cloned()
        .or_else(|| spec.get("zone").cloned())
        .unwrap_or_else(|| json!("UTC"));
    let metadata_extra = spec.get("metadata").cloned().unwrap_or_else(|| json!({}));
    let mut metadata = metadata_extra.as_object().cloned().unwrap_or_default();
    let metadata = if spec.get("zone_meta").and_then(Value::as_bool) != Some(false) {
        let mut with_zone = serde_json::Map::new();
        with_zone.insert("timeZone".to_owned(), json!(zone));
        with_zone.append(&mut metadata);
        Value::Object(with_zone)
    } else {
        Value::Object(metadata)
    };
    let mut body = json!({
        "dimensionHeaders": dims,
        "metricHeaders": mets.as_array().map(|list| list.iter().map(|item| {
            if item.is_object() { item.clone() } else { json!({"name": item, "type": "TYPE_INTEGER"}) }
        }).collect::<Vec<_>>()),
        "rows": body_rows,
        "rowCount": total,
        "metadata": metadata,
        "propertyQuota": spec.get("quota").cloned().unwrap_or_else(|| json!({"tokensPerDay": {"consumed": 1, "remaining": 199999}})),
    });
    // `dims` entries are plain names; headers still need `{"name": …}` objects.
    if let Some(headers) = body
        .get_mut("dimensionHeaders")
        .and_then(Value::as_array_mut)
    {
        for header in headers.iter_mut() {
            if !header.is_object() {
                *header = json!({"name": header.take()});
            }
        }
    }
    if spec.get("bad_headers") == Some(&json!(true)) {
        body["metricHeaders"] = json!([{"name": "notScreenPageViews"}]);
    }
    if let Some(raw) = spec.get("metadata_raw") {
        body["metadata"] = raw.clone();
    }
    if spec.get("no_quota") == Some(&json!(true)) {
        body.as_object_mut().unwrap().remove("propertyQuota");
    }
    if spec.get("null_rows") == Some(&json!(true)) {
        body["rows"] = Value::Null;
    }
    if let Some(raw) = spec.get("rows_raw") {
        body["rows"] = raw.clone();
    }
    if spec.get("no_rowcount") == Some(&json!(true)) {
        body.as_object_mut().unwrap().remove("rowCount");
    }
    if let Some(raw) = spec.get("rowcount_raw") {
        body["rowCount"] = raw.clone();
    }
    if spec.get("non_dict").is_some() {
        return pyjson::dumps(&json!([1, 2]));
    }
    pyjson::dumps(&body)
}

/// One scripted entry → body or the exception the reference script raises.
fn resolve_entry(entry: &Value) -> Result<String, Ga4Fault> {
    if let Some(body) = entry.get("$body").and_then(Value::as_str) {
        return Ok(body.to_owned());
    }
    if let Some(spec) = entry.get("$report") {
        return Ok(report_body(spec, None, usize::MAX, 0));
    }
    if let Some(http) = entry.get("$http") {
        let code = http.get("code").and_then(Value::as_i64).unwrap_or(500);
        let reason = http
            .get("reason")
            .and_then(Value::as_str)
            .map(str::to_owned)
            .unwrap_or_else(|| reason_phrase(code as u16).to_owned());
        return Err(Ga4Fault::Http { code, reason });
    }
    if let Some(reason) = entry.get("$url").and_then(Value::as_str) {
        return Err(Ga4Fault::Url(reason.to_owned()));
    }
    if let Some(message) = entry.get("$timeout").and_then(Value::as_str) {
        return Err(Ga4Fault::Timeout(message.to_owned()));
    }
    if let Some(message) = entry.get("$value").and_then(Value::as_str) {
        return Err(Ga4Fault::Value(message.to_owned()));
    }
    if let Some(other) = entry.get("$other") {
        return Err(Ga4Fault::Other {
            class: other["class"].as_str().unwrap().to_owned(),
            message: other["message"].as_str().unwrap().to_owned(),
        });
    }
    panic!("unknown transport entry {entry}");
}

#[allow(clippy::too_many_arguments)]
fn build_env<'a>(
    op: &'a Value,
    host: &'a FakeHost,
    root: &'a std::path::Path,
    http: &'a StubTransport,
    token_seam: &'a dyn Fn() -> Result<String, CredentialError>,
    avail_seam: &'a dyn Fn() -> bool,
    sa_seam: &'a dyn Fn(&str) -> Result<String, CredentialError>,
    zoneinfo: &'a dyn Fn(&str) -> Result<(), Option<String>>,
    sleeper: &'a Sleeper,
) -> Ga4Env<'a, StubTransport> {
    let env = op.get("env").cloned().unwrap_or_else(|| json!({}));
    Ga4Env {
        host,
        root,
        http,
        access_token: Some(token_seam),
        sa_available: Some(avail_seam),
        sa_token: Some(sa_seam),
        page_rows: env
            .get("page_rows")
            .and_then(Value::as_i64)
            .unwrap_or(ga4::MAX_ROWS),
        sleeper: Some(sleeper),
        zoneinfo: Some(zoneinfo),
        max_requests: env
            .get("max_requests")
            .and_then(Value::as_i64)
            .unwrap_or(ga4_content::MAX_PAGE_REQUESTS),
    }
}

async fn run_op(op: &Value) -> (Value, Vec<Value>, Vec<f64>, Vec<Value>) {
    let host = FakeHost::default();
    let root = absent_config_root();
    let http = StubTransport::new(&[]);
    let (sleeper, pauses) = Sleeper::recording();
    let env_spec = op.get("env").cloned().unwrap_or_else(|| json!({}));

    // Credential seams — mirror `apply_env` defaults exactly.
    let ga4_token = env_spec
        .get("ga4_token")
        .cloned()
        .unwrap_or_else(|| json!({"missing": "synthetic missing credentials"}));
    let access_token = move || -> Result<String, CredentialError> {
        match &ga4_token {
            Value::Object(spec) if spec.get("missing").is_some() => Err(CredentialError::Missing(
                spec["missing"].as_str().unwrap().to_owned(),
            )),
            Value::Object(spec) if spec.get("oserror").is_some() => Err(CredentialError::Io(
                std::io::Error::other(spec["oserror"].as_str().unwrap().to_owned()),
            )),
            Value::Object(spec) => Ok(spec["token"].as_str().unwrap().to_owned()),
            _ => unreachable!(),
        }
    };
    let sa_available_flag = env_spec
        .get("sa_available")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    let sa_available = move || sa_available_flag;
    let sa_spec = env_spec
        .get("sa_token")
        .cloned()
        .unwrap_or_else(|| json!({"missing": "synthetic missing service account"}));
    let sa_calls = RefCell::new(Vec::new());
    let sa_token = |scope: &str| -> Result<String, CredentialError> {
        sa_calls.borrow_mut().push(json!({"scope": scope}));
        match &sa_spec {
            Value::Object(spec) if spec.get("missing").is_some() => Err(CredentialError::Missing(
                spec["missing"].as_str().unwrap().to_owned(),
            )),
            Value::Object(spec) if spec.get("oserror").is_some() => Err(CredentialError::Io(
                std::io::Error::other(spec["oserror"].as_str().unwrap().to_owned()),
            )),
            Value::Object(spec) => Ok(spec["token"].as_str().unwrap().to_owned()),
            _ => unreachable!(),
        }
    };
    let zoneinfo = |name: &str| -> Result<(), Option<String>> {
        if let Some(message) = ga4_content::zoneinfo_key_check(name) {
            return Err(Some(message));
        }
        match name {
            "UTC" | "Europe/Minsk" => Ok(()),
            _ => Err(None),
        }
    };
    let env = build_env(
        op,
        &host,
        &root,
        &http,
        &access_token,
        &sa_available,
        &sa_token,
        &zoneinfo,
        &sleeper,
    );

    // The scripted `transport(url, payload, token)` — absent means `_default_transport`.
    let entries = op
        .get("transport")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    let calls = RefCell::new(Vec::new());
    let queue = RefCell::new(entries.iter().cloned().collect::<VecDeque<_>>());
    let paged = RefCell::new(None::<Value>);
    let page_index = Cell::new(0usize);
    let transport = |url: &str, payload: &Value, token: &str| -> Result<String, Ga4Fault> {
        calls
            .borrow_mut()
            .push(json!({"url": url, "payload": payload, "token": token}));
        if paged.borrow().is_none() {
            let entry = queue.borrow_mut().pop_front().unwrap_or_else(
                || json!({"$other": {"class": "IndexError", "message": "pop from empty list"}}),
            );
            match entry.get("$paged") {
                Some(spec) => *paged.borrow_mut() = Some(spec.clone()),
                None => return resolve_entry(&entry),
            }
        }
        let spec = paged.borrow().clone().unwrap();
        let offset: usize = payload["offset"].as_str().unwrap_or("0").parse().unwrap();
        // The scripted reference `send` does `int(payload["limit"])` — a float
        // `max_rows` makes this `ValueError` surface inside `run_report`'s `try`.
        let limit_text = payload["limit"].as_str().unwrap_or("0");
        let Ok(limit) = limit_text.parse::<usize>() else {
            return Err(Ga4Fault::Value(format!(
                "invalid literal for int() with base 10: {}",
                spiderhead_sources::pystr::repr_str(limit_text)
            )));
        };
        let index = page_index.get();
        page_index.set(index + 1);
        Ok(report_body(&spec, Some(offset), limit, index))
    };
    let has_transport = op.get("transport").is_some();
    let transport_ref: Option<&Ga4Transport> = if has_transport {
        Some(&transport)
    } else {
        None
    };

    let name = op.get("op").and_then(Value::as_str).unwrap_or("");
    let token = op.get("token").and_then(Value::as_str);
    let result: Result<Value, Ga4Error> = match name {
        "run_report" => {
            let dims: Vec<&str> = op["dimensions"]
                .as_array()
                .map(|list| list.iter().filter_map(Value::as_str).collect())
                .unwrap_or_default();
            let mets: Vec<&str> = op["metrics"]
                .as_array()
                .map(|list| list.iter().filter_map(Value::as_str).collect())
                .unwrap_or_default();
            let args = ReportArgs {
                max_rows: op.get("max_rows"),
                token,
                transport: transport_ref,
            };
            ga4::run_report(
                op["property_id"].as_str().unwrap_or_default(),
                op["start_date"].as_str().unwrap_or_default(),
                op["end_date"].as_str().unwrap_or_default(),
                &dims,
                &mets,
                &args,
                &env,
            )
            .await
        }
        "landing_pages" => {
            let args = LandingArgs {
                include_conversions: op
                    .get("include_conversions")
                    .and_then(Value::as_bool)
                    .unwrap_or(false),
                include_revenue: op
                    .get("include_revenue")
                    .and_then(Value::as_bool)
                    .unwrap_or(false),
                reporting_identity: op.get("reporting_identity").and_then(Value::as_str),
                token,
                transport: transport_ref,
            };
            ga4::landing_pages(
                op["property_id"].as_str().unwrap_or_default(),
                op["start_date"].as_str().unwrap_or_default(),
                op["end_date"].as_str().unwrap_or_default(),
                &args,
                &env,
            )
            .await
        }
        "page_views" => {
            let args = PageViewsArgs {
                max_rows: op.get("max_rows"),
                token,
                transport: transport_ref,
            };
            ga4_content::page_views(
                &op["property_id"],
                &op["start_date"],
                &op["end_date"],
                &op["site_origin"],
                &args,
                &env,
            )
            .await
        }
        other => panic!("unknown op {other}"),
    };
    let out = match result {
        Ok(value) => json!({"result": value}),
        Err(error) => {
            json!({"error": {"class": error.python_class(), "message": error.to_string()}})
        }
    };
    let recorded_calls = calls.borrow().clone();
    let recorded_sleeps = pauses.lock().unwrap().clone();
    let recorded_sa = sa_calls.borrow().clone();
    (out, recorded_calls, recorded_sleeps, recorded_sa)
}

fn assert_op(label: &str, op: &Value, actual: &(Value, Vec<Value>, Vec<f64>, Vec<Value>)) {
    let record = &op["record"];
    let (out, calls, sleeps, sa_calls) = actual;
    assert_eq!(
        out, &record["out"],
        "{label}: outcome differs\nwant: {}\n got: {out}",
        record["out"]
    );
    assert_eq!(
        calls,
        &record["calls"].as_array().cloned().unwrap_or_default(),
        "{label}: transport calls differ\nwant: {}\n got: {calls:?}",
        record["calls"]
    );
    let want_sleeps: Vec<f64> = record["sleeps"]
        .as_array()
        .map(|list| list.iter().filter_map(Value::as_f64).collect())
        .unwrap_or_default();
    assert_eq!(sleeps, &want_sleeps, "{label}: sleeps differ");
    assert_eq!(
        sa_calls,
        &record["sa_calls"].as_array().cloned().unwrap_or_default(),
        "{label}: service-account calls differ"
    );
}

#[tokio::test]
async fn ga4_oracle_replay() {
    let path = common::fixtures_dir().join("ga4_oracle.json");
    let fixture: Value = serde_json::from_str(&std::fs::read_to_string(path).unwrap()).unwrap();
    for scenario in fixture["scenarios"].as_array().unwrap() {
        let name = scenario["name"].as_str().unwrap_or("?");
        for (index, op) in scenario["ops"].as_array().unwrap().iter().enumerate() {
            let label = format!("{name}[{index}].{}", op["op"].as_str().unwrap_or("?"));
            let actual = run_op(op).await;
            assert_op(&label, op, &actual);
        }
    }
}
