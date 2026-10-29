//! Replay `provider_artifacts_oracle.json` — reference `read_pagespeed_artifact` driven by
//! scripted ops against a sandbox artifact directory. `_setup` materializes files with
//! the same modes the gen writes; `artifact_ref` stores a payload and returns its
//! `local-artifact:` reference. Sandbox paths normalize to `$BASE`.
//!

mod common;

use spiderhead_core::filesystem::set_permissions_mode;
use std::path::{Path, PathBuf};

use serde_json::{json, Value};
use sha2::{Digest, Sha256};

use spiderhead_sources::provider_artifacts as pa;
use spiderhead_sources::pyjson;

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
    let dir = std::env::temp_dir().join(format!(
        "provider-artifacts-oracle-{}-{name}",
        std::process::id()
    ));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap_or_default();
    set_permissions_mode(&dir, 0o700).unwrap_or_default();
    dir
}

fn chmod(path: &Path, mode: u32) {
    set_permissions_mode(path, mode).unwrap_or_default();
}

/// `write_artifact` — store the payload and return its `local-artifact:` reference.
fn write_artifact(base: &Path, payload: &Value) -> String {
    let digest = {
        let text = pyjson::dumps_opts(payload, true, true);
        let mut hasher = Sha256::new();
        hasher.update(text.as_bytes());
        format!("{:x}", hasher.finalize())
    };
    let path = base.join(format!("provider-{}.json", &digest[..16]));
    std::fs::write(&path, pyjson::dumps_opts(payload, true, false)).unwrap_or_default();
    chmod(&path, 0o600);
    format!("local-artifact:{digest}")
}

fn outcome(result: Result<Value, pa::PaError>) -> Value {
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
            for dir in op
                .get("mkdir")
                .and_then(Value::as_array)
                .cloned()
                .unwrap_or_default()
            {
                let target = base.join(dir.as_str().unwrap_or_default());
                std::fs::create_dir_all(&target).unwrap_or_default();
                chmod(&target, 0o700);
            }
            for dir in op
                .get("mkdir_open")
                .and_then(Value::as_array)
                .cloned()
                .unwrap_or_default()
            {
                let target = base.join(dir.as_str().unwrap_or_default());
                std::fs::create_dir_all(&target).unwrap_or_default();
                chmod(&target, 0o755);
            }
            for payload in op
                .get("artifacts")
                .and_then(Value::as_object)
                .map(|m| m.values().cloned().collect::<Vec<_>>())
                .unwrap_or_default()
            {
                write_artifact(base, &payload);
            }
            if let Some(files) = op.get("files").and_then(Value::as_object) {
                for (name, content) in files {
                    let target = base.join(name);
                    if let Some(parent) = target.parent() {
                        std::fs::create_dir_all(parent).unwrap_or_default();
                    }
                    std::fs::write(&target, content.as_str().unwrap_or_default())
                        .unwrap_or_default();
                    chmod(&target, 0o600);
                }
            }
            for payload in op
                .get("tamper")
                .and_then(Value::as_object)
                .map(|m| m.values().cloned().collect::<Vec<_>>())
                .unwrap_or_default()
            {
                if let Ok(entries) = std::fs::read_dir(base) {
                    for entry in entries.flatten() {
                        let name = entry.file_name().to_string_lossy().into_owned();
                        if name.starts_with("provider-") && name.ends_with(".json") {
                            std::fs::write(entry.path(), pyjson::dumps(&payload))
                                .unwrap_or_default();
                            chmod(&entry.path(), 0o600);
                        }
                    }
                }
            }
            json!({"result": true})
        }
        "artifact_ref" => {
            let payload = kw.get("payload").cloned().unwrap_or_default();
            json!({"result": write_artifact(base, &payload)})
        }
        "read" => outcome(pa::read_pagespeed_artifact(
            kw.get("artifact_ref").unwrap_or(&Value::Null),
            kw.get("artifact_dir").unwrap_or(&Value::Null),
            kw.get("offset"),
            kw.get("limit"),
        )),
        other => panic!("unknown fn {other}"),
    }
}

#[test]
fn provider_artifacts_oracle_replay() {
    let scenarios = load_fixture("provider_artifacts_oracle.json");
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
