//! Replay `cloudflare_history_oracle.json` — reference `cloudflare_history`
//! (`save_snapshot`/`list_snapshots`/`list_zones`) driven by scripted ops against a
//! sandbox project directory. Paths normalize to `$BASE`.
//!

mod common;

use spiderhead_core::filesystem::set_permissions_mode;
use std::path::{Path, PathBuf};

use serde_json::{json, Value};

use spiderhead_sources::cloudflare_history as cfh;

use common::load_fixture;

fn expand_base(value: &Value, base: &Path) -> Value {
    let text = serde_json::to_string(value).unwrap_or_default();
    serde_json::from_str(&text.replace("$BASE", &base.to_string_lossy()))
        .unwrap_or_else(|_| value.clone())
}

fn scrub_base(value: &mut Value, base: &str) {
    match value {
        Value::String(text) => *text = text.replace(base, "$BASE"),
        Value::Array(items) => items.iter_mut().for_each(|item| scrub_base(item, base)),
        Value::Object(map) => map.values_mut().for_each(|item| scrub_base(item, base)),
        _ => {}
    }
}

fn sandbox(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("cf-history-oracle-{}-{name}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap_or_default();
    dir
}

fn chmod(path: &Path, mode: u32) {
    set_permissions_mode(path, mode).unwrap_or_default();
}

fn outcome(result: Result<Value, cfh::CfhError>) -> Value {
    match result {
        Ok(value) => json!({"result": value}),
        Err(err) => {
            json!({"error": {"class": err.python_class(), "message": err.to_string()}})
        }
    }
}

fn run_op(op: &Value, base: &Path) -> Value {
    let fn_name = op.get("fn").and_then(Value::as_str).unwrap_or_default();
    let kw = expand_base(op.get("kw").unwrap_or(&Value::Null), base);
    match fn_name {
        "_setup" => {
            let proj = base.join("proj");
            std::fs::create_dir_all(&proj).unwrap_or_default();
            std::fs::write(proj.join("project.json"), "{}").unwrap_or_default();
            chmod(&proj, 0o700);
            for name in op
                .get("sqlite_foreign")
                .and_then(Value::as_array)
                .cloned()
                .unwrap_or_default()
            {
                let con = rusqlite::Connection::open(proj.join(name.as_str().unwrap_or_default()))
                    .unwrap();
                con.execute_batch("CREATE TABLE scan(value TEXT)").unwrap();
            }
            for name in op
                .get("sqlite_bad_marker")
                .and_then(Value::as_array)
                .cloned()
                .unwrap_or_default()
            {
                let con = rusqlite::Connection::open(proj.join(name.as_str().unwrap_or_default()))
                    .unwrap();
                con.execute_batch(
                    "CREATE TABLE cloudflare_meta(format TEXT PRIMARY KEY);
                     INSERT INTO cloudflare_meta VALUES ('other.v9');
                     CREATE TABLE cloudflare_snapshots (id INTEGER PRIMARY KEY)",
                )
                .unwrap();
            }
            for pair in op
                .get("sqlite_open")
                .and_then(Value::as_array)
                .cloned()
                .unwrap_or_default()
            {
                let name = pair[0].as_str().unwrap_or_default();
                let mode = pair[1].as_u64().unwrap_or(0o644) as u32;
                let con = rusqlite::Connection::open(proj.join(name)).unwrap();
                con.execute_batch(
                    "CREATE TABLE cloudflare_meta(format TEXT PRIMARY KEY);
                     CREATE TABLE cloudflare_snapshots (id INTEGER PRIMARY KEY)",
                )
                .unwrap();
                con.execute("INSERT INTO cloudflare_meta VALUES (?)", [cfh::FORMAT])
                    .unwrap();
                chmod(&proj.join(name), mode);
            }
            json!({"result": true})
        }
        "save_snapshot" => outcome(cfh::save_snapshot(
            kw.get("result").unwrap_or(&Value::Null),
            kw.get("project").unwrap_or(&Value::Null),
            kw.get("observed_at"),
        )),
        "list_snapshots" => outcome(cfh::list_snapshots(&kw)),
        "list_zones" => outcome(cfh::list_zones(
            kw.get("project").unwrap_or(&Value::Null),
            kw.get("limit"),
            kw.get("offset"),
        )),
        other => panic!("unknown fn {other}"),
    }
}

#[test]
fn cloudflare_history_oracle_replay() {
    let scenarios = load_fixture("cloudflare_history_oracle.json");
    let mut failures = Vec::new();
    for scenario in &scenarios {
        let name = scenario
            .get("name")
            .and_then(Value::as_str)
            .unwrap_or("?")
            .to_owned();
        let base = sandbox(&name);
        let mut problems = Vec::new();
        for (index, op) in scenario
            .get("ops")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default()
            .iter()
            .enumerate()
        {
            let want = op.get("outcome").cloned().unwrap_or_default();
            let mut got = run_op(op, &base);
            scrub_base(&mut got, &base.to_string_lossy());
            if got != want {
                problems.push(format!(
                    "op {index} outcome differs\nwant: {}\n got: {}",
                    serde_json::to_string_pretty(&want).unwrap_or_default(),
                    serde_json::to_string_pretty(&got).unwrap_or_default()
                ));
            }
        }
        let _ = std::fs::remove_dir_all(&base);
        if !problems.is_empty() {
            failures.push(format!("scenario {name}: {}", problems.join("; ")));
            if failures.len() >= 5 {
                break;
            }
        }
    }
    assert!(failures.is_empty(), "{}", failures.join("\n---\n"));
}
