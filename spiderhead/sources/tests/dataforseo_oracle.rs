//! Oracle replay for the reference suite's fixture — the DataForSEO client
//! (`geo_guard`, `estimate`, `post`/`balance`, `_run`, the Google wrappers and
//! `backlink_call`) against scripted `responses` queues. Every op replays the
//! recorded requests/pauses/spends and diffs the outcome field-wise.

mod common;

use std::sync::{Arc, Mutex};

use serde_json::{json, Map, Value};
use spiderhead_sources::credentials::CredentialError;
use spiderhead_sources::dataforseo::{
    self, BacklinkArgs, Client, ClientArgs, DfsEnv, DfsError, EstimateArgs, GeoArgs, IdeasArgs,
    SerpArgs,
};
use spiderhead_sources::http::Sleeper;
use spiderhead_sources::metrika::SpendSink;
use spiderhead_sources::spend::Record;

use common::{load_fixture, scenario_host, StubTransport};

/// `spend.record` kwargs as JSON — reference passes ints for `cost`/`items` and omits
/// `items` entirely when the caller never supplied it.
fn spend_json(rec: &Record) -> Value {
    let mut m = Map::new();
    m.insert("source".to_owned(), json!(rec.source));
    m.insert("operation".to_owned(), json!(rec.operation));
    if let Some(cost) = rec.cost {
        // `cost or 0.0` and provider `cost` values arrive as floats — no int coercion.
        m.insert("cost".to_owned(), json!(cost));
    }
    m.insert("unit".to_owned(), json!(rec.unit));
    if let Some(items) = rec.items {
        m.insert("items".to_owned(), json!(items));
    }
    if let Some(extra) = &rec.extra {
        m.insert("extra".to_owned(), Value::Object(extra.clone()));
    }
    Value::Object(m)
}

/// `{result}` or `{error: {class, message}}` — the fixture's outcome shape.
fn outcome<T>(result: Result<T, DfsError>) -> Value
where
    T: serde::Serialize,
{
    match result {
        Ok(value) => json!({"result": value}),
        Err(err) => json!({"error": {
            "class": err.python_class(),
            "message": err.to_string(),
        }}),
    }
}

/// A credential reader from a spec dict (`{token|missing|oserror}`).
fn credential_probe(spec: Value) -> impl Fn() -> Result<String, CredentialError> {
    move || {
        if let Some(token) = spec.get("token").and_then(Value::as_str) {
            return Ok(token.to_owned());
        }
        if let Some(message) = spec.get("missing").and_then(Value::as_str) {
            return Err(CredentialError::Missing(message.to_owned()));
        }
        if let Some(message) = spec.get("oserror").and_then(Value::as_str) {
            return Err(CredentialError::Io(std::io::Error::other(
                message.to_owned(),
            )));
        }
        unreachable!("unknown credential spec {spec}")
    }
}

fn pick<'a>(args: &'a Value, key: &str) -> Option<&'a Value> {
    args.get(key)
}

fn geo_args<'a>(kw: &'a Value) -> GeoArgs<'a> {
    GeoArgs {
        location_code: pick(kw, "location_code"),
        language: pick(kw, "language"),
        country: pick(kw, "country"),
        env: pick(kw, "env"),
        confirm_paid: pick(kw, "confirm_paid"),
        spend_ceiling_usd: pick(kw, "spend_ceiling_usd"),
        prices: pick(kw, "prices"),
    }
}

fn backlink_args<'a>(
    kw: &'a Value,
    client: Option<&'a Client<'a, StubTransport>>,
) -> BacklinkArgs<'a, StubTransport> {
    BacklinkArgs {
        limit: pick(kw, "limit"),
        offset: pick(kw, "offset"),
        enabled: pick(kw, "enabled"),
        account_eligible: pick(kw, "account_eligible"),
        production_approved: pick(kw, "production_approved"),
        cost_approved: pick(kw, "cost_approved"),
        confirm_paid: pick(kw, "confirm_paid"),
        spend_ceiling_usd: pick(kw, "spend_ceiling_usd"),
        cache_key: pick(kw, "cache_key"),
        env: pick(kw, "env"),
        prices: pick(kw, "prices"),
        client,
        date_from: pick(kw, "date_from"),
        date_to: pick(kw, "date_to"),
    }
}

/// One oracle op: dispatch `args.fn` with `args.kw`.
async fn run_op(
    scenario: &Value,
    transport: &StubTransport,
    sleeper: &Sleeper,
    spend: &SpendSink<'_>,
) -> Result<Value, DfsError> {
    let (host, root) = scenario_host(scenario);
    let cred = scenario.get("cred").cloned().unwrap_or_else(|| json!({}));
    let login = credential_probe(
        cred.get("login")
            .cloned()
            .unwrap_or_else(|| json!({"missing": "synthetic missing login"})),
    );
    let password = credential_probe(
        cred.get("password")
            .cloned()
            .unwrap_or_else(|| json!({"missing": "synthetic missing password"})),
    );
    let today_days = scenario
        .get("today")
        .and_then(Value::as_str)
        .and_then(spiderhead_sources::pytime::date_fromisoformat)
        .unwrap_or(0);
    let today = move || today_days;
    let env = DfsEnv {
        host: &host,
        root: &root,
        transport,
        sleeper,
        spend: Some(spend),
        login: Some(&login),
        password: Some(&password),
        today: Some(&today),
    };

    let call = scenario.get("args").cloned().unwrap_or_else(|| json!({}));
    let fn_name = call.get("fn").and_then(Value::as_str).unwrap_or_default();
    let kwarg = call.get("kw").cloned().unwrap_or_else(|| json!({}));
    let init = scenario.get("init").cloned().unwrap_or_else(|| json!({}));
    let client_args = ClientArgs {
        login: init.get("login"),
        password: init.get("password"),
        env_name: init.get("env"),
    };

    // An injected `client` for the environment-mismatch check (`client_env`).
    let env_v = scenario.get("client_env").cloned().unwrap_or(Value::Null);
    let injected = match scenario.get("client_env") {
        Some(_) => Some(Client::new(
            &env,
            &ClientArgs {
                login: None,
                password: None,
                env_name: Some(&env_v),
            },
        )?),
        None => None,
    };

    match fn_name {
        "geo_guard" => {
            dataforseo::geo_guard(pick(&kwarg, "country"), pick(&kwarg, "location_code"))
                .map(|v| v.unwrap_or(Value::Null))
        }
        "estimate" => dataforseo::estimate(
            &env,
            kwarg.get("operation").unwrap_or(&Value::Null),
            &EstimateArgs {
                query_count: pick(&kwarg, "query_count"),
                row_limit: pick(&kwarg, "row_limit"),
                cache_hits: pick(&kwarg, "cache_hits"),
                prices: pick(&kwarg, "prices"),
                env: pick(&kwarg, "env"),
                query: pick(&kwarg, "query"),
            },
        ),
        "post" => {
            Client::new(&env, &client_args)?
                .post(
                    kwarg
                        .get("endpoint")
                        .and_then(Value::as_str)
                        .unwrap_or_default(),
                    kwarg.get("payload").unwrap_or(&Value::Null),
                    kwarg.get("operation"),
                )
                .await
        }
        "balance" => Client::new(&env, &client_args)?.balance().await,
        "_run" => dataforseo::run(
            &Client::new(&env, &client_args)?,
            kwarg
                .get("kind")
                .and_then(Value::as_str)
                .unwrap_or_default(),
            kwarg.get("payload").unwrap_or(&Value::Null),
            kwarg
                .get("items_count")
                .and_then(Value::as_i64)
                .unwrap_or(0),
        )
        .await
        .map(|o| json!([o.items, o.errors, o.cost, o.failed])),
        "search_volume" => {
            dataforseo::search_volume(
                &env,
                kwarg.get("keywords").unwrap_or(&Value::Null),
                &geo_args(&kwarg),
            )
            .await
        }
        "keyword_ideas" => {
            dataforseo::keyword_ideas(
                &env,
                kwarg.get("seed").unwrap_or(&Value::Null),
                &IdeasArgs {
                    location_code: pick(&kwarg, "location_code"),
                    language: pick(&kwarg, "language"),
                    limit: pick(&kwarg, "limit"),
                    country: pick(&kwarg, "country"),
                    env: pick(&kwarg, "env"),
                    confirm_paid: pick(&kwarg, "confirm_paid"),
                    spend_ceiling_usd: pick(&kwarg, "spend_ceiling_usd"),
                    prices: pick(&kwarg, "prices"),
                },
            )
            .await
        }
        "keyword_difficulty" => {
            dataforseo::keyword_difficulty(
                &env,
                kwarg.get("keywords").unwrap_or(&Value::Null),
                &geo_args(&kwarg),
            )
            .await
        }
        "serp" => {
            dataforseo::serp(
                &env,
                kwarg.get("query").unwrap_or(&Value::Null),
                &SerpArgs {
                    location_code: pick(&kwarg, "location_code"),
                    language: pick(&kwarg, "language"),
                    depth: pick(&kwarg, "depth"),
                    country: pick(&kwarg, "country"),
                    env: pick(&kwarg, "env"),
                    confirm_paid: pick(&kwarg, "confirm_paid"),
                    spend_ceiling_usd: pick(&kwarg, "spend_ceiling_usd"),
                    prices: pick(&kwarg, "prices"),
                },
            )
            .await
        }
        "backlink_call" => {
            dataforseo::backlink_call(
                &env,
                kwarg.get("operation").unwrap_or(&Value::Null),
                kwarg.get("target").unwrap_or(&Value::Null),
                &backlink_args(&kwarg, injected.as_ref()),
            )
            .await
        }
        "backlinks"
        | "backlinks_summary"
        | "referring_domains"
        | "backlinks_history"
        | "domain_pages_summary" => {
            let target = kwarg.get("target").unwrap_or(&Value::Null);
            let bargs = backlink_args(&kwarg, injected.as_ref());
            match fn_name {
                "backlinks" => dataforseo::backlinks(&env, target, &bargs).await,
                "backlinks_summary" => dataforseo::backlinks_summary(&env, target, &bargs).await,
                "referring_domains" => dataforseo::referring_domains(&env, target, &bargs).await,
                "backlinks_history" => dataforseo::backlinks_history(&env, target, &bargs).await,
                _ => dataforseo::domain_pages_summary(&env, target, &bargs).await,
            }
        }
        "task_items" => {
            dataforseo::task_items(kwarg.get("body").unwrap_or(&Value::Null)).map(Value::Array)
        }
        "task_errors" => {
            dataforseo::task_errors(kwarg.get("body").unwrap_or(&Value::Null)).map(|v| json!(v))
        }
        "tasks_all_failed" => {
            dataforseo::tasks_all_failed(kwarg.get("body").unwrap_or(&Value::Null))
                .map(|b| json!(b))
        }
        other => panic!("unknown fn {other}"),
    }
}

/// Field-wise request comparison, mirroring `common::assert_requests_eq` — `body`
/// compares as parsed JSON (member order is not meaningful to the receiving API).
fn request_diffs(want: &[Value], got: &[Value]) -> Vec<String> {
    let mut problems = Vec::new();
    if want.len() != got.len() {
        problems.push(format!(
            "request count differs: want {} got {}",
            want.len(),
            got.len()
        ));
        return problems;
    }
    for (i, (w, g)) in want.iter().zip(got).enumerate() {
        for f in ["method", "url", "headers", "timeout"] {
            if w.get(f) != g.get(f) {
                problems.push(format!(
                    "request {i} field {f} differs: want {:?} got {:?}",
                    w.get(f),
                    g.get(f)
                ));
            }
        }
        let wb = w.get("body").and_then(Value::as_str).unwrap_or_default();
        let gb = g.get("body").and_then(Value::as_str).unwrap_or_default();
        match (
            serde_json::from_str::<Value>(wb),
            serde_json::from_str::<Value>(gb),
        ) {
            (Ok(wv), Ok(gv)) => {
                if wv != gv {
                    problems.push(format!("request {i} body differs\nwant: {wb}\n got: {gb}"));
                }
            }
            _ => {
                if wb != gb {
                    problems.push(format!("request {i} body differs\nwant: {wb}\n got: {gb}"));
                }
            }
        }
    }
    problems
}

#[tokio::test(flavor = "current_thread")]
async fn dataforseo_oracle_replay() {
    let scenarios = load_fixture("dataforseo_oracle.json");
    let mut failures = Vec::new();
    for (index, scenario) in scenarios.iter().enumerate() {
        let name = scenario
            .get("name")
            .and_then(Value::as_str)
            .unwrap_or("?")
            .to_owned();
        let responses = scenario
            .get("responses")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();
        let transport = StubTransport::new(&responses);
        let (sleeper, pauses) = Sleeper::recording();
        let spends = Arc::new(Mutex::new(Vec::<Value>::new()));
        let spend_impl = {
            let spends = spends.clone();
            move |rec: &Record| {
                spends.lock().unwrap().push(spend_json(rec));
                Ok(json!({"ok": true}))
            }
        };
        let spend: &SpendSink = &spend_impl;

        let expect = scenario.get("expect").cloned().unwrap_or_default();
        let want_outcome = if let Some(r) = expect.get("result") {
            json!({"result": r})
        } else {
            json!({"error": expect.get("error").cloned().unwrap_or_default()})
        };
        let want_spends = expect.get("spends").cloned().unwrap_or_default();
        let want_requests = expect.get("requests").cloned().unwrap_or_default();
        let want_pauses: Vec<f64> = expect
            .get("pauses")
            .and_then(Value::as_array)
            .map(|a| a.iter().filter_map(Value::as_f64).collect())
            .unwrap_or_default();

        let got_outcome = outcome(run_op(scenario, &transport, &sleeper, spend).await);
        let mut problems = request_diffs(
            want_requests
                .as_array()
                .map(Vec::as_slice)
                .unwrap_or_default(),
            &transport.recorded(),
        );
        if *pauses.lock().unwrap() != want_pauses[..] {
            problems.push(format!(
                "pauses differ: want {want_pauses:?} got {:?}",
                *pauses.lock().unwrap()
            ));
        }
        let got_spends = Value::Array(spends.lock().unwrap().clone());
        if got_spends != want_spends {
            problems.push(format!(
                "spends differ\nwant: {}\n got: {}",
                serde_json::to_string_pretty(&want_spends).unwrap_or_default(),
                serde_json::to_string_pretty(&got_spends).unwrap_or_default()
            ));
        }
        if got_outcome != want_outcome {
            problems.push(format!(
                "outcome differs\nwant: {}\n got: {}",
                serde_json::to_string_pretty(&want_outcome).unwrap_or_default(),
                serde_json::to_string_pretty(&got_outcome).unwrap_or_default()
            ));
        }
        if !problems.is_empty() {
            failures.push(format!("op {index} ({name}): {}", problems.join("; ")));
            if failures.len() >= 5 {
                break;
            }
        }
    }
    assert!(failures.is_empty(), "{}", failures.join("\n---\n"));
}
