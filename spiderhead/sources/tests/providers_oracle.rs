//! Oracle replay for the reference suite + the reference suite —
//! drives the reference suite fixtures through the Rust port, comparing
//! recorded dispatch calls and verbatim outcomes.

use serde_json::{json, Value};
use spiderhead_sources::cloudflare::CfError;
use spiderhead_sources::external_join;
use spiderhead_sources::providers::{self, DispatchError, ProviderEnv};
use spiderhead_sources::providers_lifecycle;
use std::cell::RefCell;
use std::collections::BTreeMap;
use std::path::PathBuf;
use std::rc::Rc;

struct FakeHost {
    env: BTreeMap<String, String>,
}

impl spiderhead_core::host::Host for FakeHost {
    fn var(&self, name: &str) -> Option<String> {
        self.env.get(name).cloned()
    }

    fn resolve(&self, _host: &str, _port: u16) -> Result<Vec<std::net::SocketAddr>, String> {
        Err("offline".to_owned())
    }

    fn zstd_available(&self) -> bool {
        false
    }
}

fn ops() -> Vec<Value> {
    let path =
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/providers_oracle.json");
    serde_json::from_str(&std::fs::read_to_string(path).unwrap()).unwrap()
}

fn scrub(value: &Value, base: &str) -> Value {
    match value {
        Value::String(s) => json!(s.replace("$BASE", base)),
        Value::Array(items) => json!(items.iter().map(|v| scrub(v, base)).collect::<Vec<_>>()),
        Value::Object(map) => {
            let mut out = serde_json::Map::new();
            for (k, v) in map {
                out.insert(k.clone(), scrub(v, base));
            }
            Value::Object(out)
        }
        other => other.clone(),
    }
}

fn expand_args(op: &Value, base: &str) -> Value {
    let args = op.get("args").cloned().unwrap_or(json!({}));
    scrub(&args, base)
}

fn setup_fs(op: &Value, base: &str) {
    for entry in op
        .get("fs")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default()
    {
        let line = entry.as_str().unwrap_or_default().replace("$BASE", base);
        let (kind, rest) = line.split_once(' ').unwrap();
        match kind {
            "mkdir" => std::fs::create_dir_all(rest).unwrap(),
            "write" => {
                let (path, content) = rest.split_once(' ').unwrap();
                let path = PathBuf::from(path);
                std::fs::create_dir_all(path.parent().unwrap()).unwrap();
                std::fs::write(path, content).unwrap();
            }
            "chmod" => {
                let (mode, path) = rest.split_once(' ').unwrap();
                let mode = u32::from_str_radix(mode, 8).unwrap();
                spiderhead_core::filesystem::set_permissions_mode(std::path::Path::new(path), mode)
                    .unwrap();
            }
            _ => panic!("unknown fs op {line}"),
        }
    }
}

fn run_op(op: &Value) -> Value {
    static COUNTER: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
    let base_dir = std::env::temp_dir().join(format!(
        "prov-{}-{}",
        std::process::id(),
        COUNTER.fetch_add(1, std::sync::atomic::Ordering::SeqCst)
    ));
    std::fs::create_dir_all(&base_dir).unwrap();
    let base = base_dir.to_str().unwrap().to_owned();
    setup_fs(op, &base);
    let env: BTreeMap<String, String> = op
        .get("env")
        .and_then(Value::as_object)
        .map(|m| {
            m.iter()
                .map(|(k, v)| {
                    (
                        k.clone(),
                        v.as_str().unwrap_or_default().replace("$BASE", &base),
                    )
                })
                .collect()
        })
        .unwrap_or_default();
    let host = FakeHost { env };
    let calls: Rc<RefCell<Vec<Value>>> = Rc::new(RefCell::new(Vec::new()));
    let dispatch_routes = op.get("dispatch").cloned().unwrap_or(json!({}));
    let calls_dispatch = calls.clone();
    let base_disp = base.clone();
    let dispatch = move |route: &str, request: Option<&Value>| {
        let request = request.cloned().unwrap_or(json!({}));
        calls_dispatch
            .borrow_mut()
            .push(json!({"route": route, "kwargs": scrub(&request, &base_disp)}));
        let spec = dispatch_routes.get(route).cloned().ok_or_else(|| {
            DispatchError::plain("AssertionError", &format!("unexpected dispatch: {route}"))
        })?;
        if let Some(err) = spec.get("raise") {
            return Err(DispatchError {
                class: err["class"].as_str().unwrap_or("Exception").to_owned(),
                message: err
                    .get("message")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_owned(),
                code: err.get("code").and_then(Value::as_str).map(str::to_owned),
                status: err.get("status").and_then(Value::as_i64),
                missing_credential: err["class"] == "MissingCredential",
            });
        }
        if let Some(tuple) = spec.get("tuple") {
            let arr = tuple.as_array().cloned().unwrap_or_default();
            return Ok(json!({
                "status": arr.first().cloned().unwrap_or(Value::Null),
                "body": arr.get(1).cloned().unwrap_or(Value::Null),
            }));
        }
        Ok(spec.get("json").cloned().unwrap_or(Value::Null))
    };
    let sa = op.get("service_account").cloned().unwrap_or(json!({}));
    let sa_avail = move || {
        sa.get("available")
            .and_then(Value::as_bool)
            .unwrap_or(false)
    };
    let sa_status_value = op
        .get("service_account")
        .and_then(|v| v.get("status"))
        .and_then(Value::as_str)
        .unwrap_or("missing")
        .to_owned();
    let sa_status = move || sa_status_value.clone();
    let grants = op.get("grants").cloned().unwrap_or(json!({}));
    let grant_available = move |provider: &str| {
        grants
            .get(provider)
            .and_then(Value::as_bool)
            .unwrap_or(false)
    };
    let now = || "2026-03-01T12:00:00+00:00".to_owned();
    let ticks = Rc::new(RefCell::new(
        op.get("monotonic")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default()
            .into_iter()
            .chain([json!(0.0), json!(0.0)])
            .map(|v| v.as_f64().unwrap_or_default())
            .collect::<Vec<f64>>()
            .into_iter(),
    ));
    let monotonic = {
        let ticks = ticks.clone();
        move || ticks.borrow_mut().next().unwrap_or(0.0)
    };
    let calls_scan = calls.clone();
    let scan_spec = op.get("open_scan").cloned();
    let open_scan = move |path: &str| -> Result<(i64, Vec<Value>, String), CfError> {
        calls_scan.borrow_mut().push(json!({
            "route": "storage.open_scan",
            "kwargs": {"input_path": "$BASE/scan.db", "require_audit": false},
        }));
        let _ = path;
        let spec = scan_spec.clone().ok_or_else(|| {
            CfError::Python("AssertionError", "open_scan not scripted".to_owned())
        })?;
        if let Some(err) = spec.get("raise") {
            return Err(CfError::Python(
                "Exception",
                err.get("message")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_owned(),
            ));
        }
        calls_scan.borrow_mut().push(json!({
            "route": "scan.execute",
            "kwargs": {"sql": "SELECT COUNT(*) FROM pages"},
        }));
        let page_count = spec["page_count"].as_i64().unwrap_or_default();
        if page_count <= 100_000 {
            for sql in [
                "SELECT u.url,p.status_code FROM pages p JOIN urls u USING(url_id) ORDER BY p.url_id",
                "SELECT scan_uuid FROM scan",
            ] {
                calls_scan.borrow_mut().push(json!({
                    "route": "scan.execute",
                    "kwargs": {"sql": sql},
                }));
            }
        }
        Ok((
            page_count,
            spec["pages"].as_array().cloned().unwrap_or_default(),
            spec["scan_uuid"].as_str().unwrap_or_default().to_owned(),
        ))
    };
    let env_struct = ProviderEnv {
        host: &host,
        root: std::path::Path::new(&base),
        service_account_available: &sa_avail,
        service_account_status: &sa_status,
        grant_available: &grant_available,
        now: &now,
        monotonic: &monotonic,
        dispatch: &dispatch,
        open_scan: &open_scan,
    };
    let args = expand_args(op, &base);
    let outcome = match op["fn"].as_str().unwrap() {
        "registry" => Ok(providers::provider_registry()),
        "credential_slot" => providers::credential_slot(
            &env_struct,
            args["provider"].as_str().unwrap_or_default(),
            args["component"].as_str().unwrap_or_default(),
        ),
        "manage_credential" => providers::manage_credential(
            &env_struct,
            args["provider"].as_str().unwrap_or_default(),
            args["component"].as_str().unwrap_or_default(),
            args["action"].as_str().unwrap_or("status"),
            args.get("value"),
            args.get("replace")
                .and_then(Value::as_bool)
                .unwrap_or(false),
            args.get("confirm")
                .and_then(Value::as_bool)
                .unwrap_or(false),
        ),
        "discover" => providers::provider_discover(
            &env_struct,
            args["provider"].as_str().unwrap_or_default(),
            args.get("domain"),
        ),
        "readiness" => Ok(providers::provider_readiness(
            &env_struct,
            args.get("provider"),
            args.get("operation"),
        )),
        "doctor" => Ok(providers::sources_doctor(&env_struct)),
        "verify" => providers_lifecycle::provider_verify(
            &env_struct,
            &args["provider"],
            args.get("request"),
        ),
        "collect" => providers_lifecycle::provider_collect(
            &env_struct,
            args["provider"].as_str().unwrap_or_default(),
            args["operation"].as_str().unwrap_or_default(),
            &args["request"],
            args.get("artifact_dir").and_then(Value::as_str),
            None,
        ),
        "join" => providers_lifecycle::provider_join(
            args["crawl_pages"]
                .as_array()
                .cloned()
                .unwrap_or_default()
                .as_slice(),
            args["evidence_rows"]
                .as_array()
                .cloned()
                .unwrap_or_default()
                .as_slice(),
            args["url_field"].as_str().unwrap_or("url"),
            args["url_column"].as_str().unwrap_or("url"),
            args.get("review_external_only")
                .and_then(Value::as_bool)
                .unwrap_or(false),
            args.get("adjustments")
                .and_then(Value::as_array)
                .map(Vec::as_slice),
        ),
        "replay" => providers_lifecycle::provider_replay(
            &env_struct,
            args["input_path"].as_str().unwrap_or_default(),
            args["evidence_file"].as_str().unwrap_or_default(),
            args["out_dir"].as_str().unwrap_or_default(),
            args["url_column"].as_str().unwrap_or("url"),
            args.get("review_external_only")
                .and_then(Value::as_bool)
                .unwrap_or(false),
        ),
        "normalize_join_key" => match external_join::normalize_join_key(
            args.get("url"),
            args.get("ignore_query")
                .and_then(Value::as_bool)
                .unwrap_or(false),
            args.get("ignore_scheme")
                .and_then(Value::as_bool)
                .unwrap_or(false),
            args.get("casefold_path")
                .and_then(Value::as_bool)
                .unwrap_or(false),
        ) {
            Ok(value) => Ok(value.map(Value::from).unwrap_or(Value::Null)),
            Err(e) => Err(e),
        },
        "join_external" => external_join::join_external_data(
            args["pages"]
                .as_array()
                .cloned()
                .unwrap_or_default()
                .as_slice(),
            args["rows"]
                .as_array()
                .cloned()
                .unwrap_or_default()
                .as_slice(),
            args["url_field"].as_str().unwrap_or("url"),
            args["url_column"].as_str().unwrap_or("url"),
            &external_join::default_key_fn,
        ),
        "orphan_urls" => match external_join::orphan_urls(
            &args["join_result"],
            args["url_column"].as_str().unwrap_or("url"),
            None,
        ) {
            Ok(urls) => Ok(json!(urls)),
            Err(e) => Err(e),
        },
        other => panic!("unknown fn {other}"),
    };
    fn normalize_updated_at(value: &Value) -> Value {
        match value {
            Value::Object(map) => {
                let mut out = serde_json::Map::new();
                for (k, v) in map {
                    out.insert(
                        k.clone(),
                        if k == "updated_at" && !v.is_null() {
                            json!("<mtime>")
                        } else {
                            normalize_updated_at(v)
                        },
                    );
                }
                Value::Object(out)
            }
            Value::Array(items) => {
                json!(items.iter().map(normalize_updated_at).collect::<Vec<_>>())
            }
            other => other.clone(),
        }
    }
    let mut out = json!({});
    match outcome {
        Ok(value) => out["result"] = normalize_updated_at(&scrub(&value, &base)),
        Err(error) => {
            out["error"] = json!({
                "class": error.python_class(),
                "message": error.to_string().replace(&base, "$BASE"),
            })
        }
    }
    out["calls"] = json!(calls
        .borrow()
        .iter()
        .map(|c| {
            let s = serde_json::to_string(c).unwrap().replace(&base, "$BASE");
            serde_json::from_str::<Value>(&s).unwrap()
        })
        .collect::<Vec<_>>());
    out
}

#[test]
fn providers_oracle() {
    let mut mismatches = Vec::new();
    for op in ops() {
        let got = run_op(&op);
        let want_outcome = op["outcome"].clone();
        let got_outcome = {
            let mut o = got.clone();
            o.as_object_mut().unwrap().remove("calls");
            o
        };
        if got_outcome != want_outcome {
            mismatches.push(format!(
                "{}: outcome\n want {}\n got  {}",
                op["name"],
                serde_json::to_string(&want_outcome).unwrap(),
                serde_json::to_string(&got_outcome).unwrap(),
            ));
            continue;
        }
        let want_calls = op.get("calls").cloned().unwrap_or(json!([]));
        let got_calls = got["calls"].clone();
        if got_calls != want_calls {
            mismatches.push(format!(
                "{}: calls\n want {}\n got  {}",
                op["name"],
                serde_json::to_string(&want_calls).unwrap(),
                serde_json::to_string(&got_calls).unwrap(),
            ));
        }
    }
    assert!(mismatches.is_empty(), "{}", mismatches.join("\n\n"));
}
