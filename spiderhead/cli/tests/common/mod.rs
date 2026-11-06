//! Shared helpers for the parity tests: fixture loading, running the CLI, unified diffs.

#![allow(dead_code)]

use std::path::PathBuf;
use std::sync::Once;

use serde_json::Value;
use spiderhead_cli::{run, Format, Output, Registry, TOOLKIT_VERSION};

static INIT: Once = Once::new();

/// Pin the terminal width the fixtures were captured with (`COLUMNS=100`).
pub fn init() {
    INIT.call_once(|| {
        if std::env::var_os("COLUMNS").is_none() {
            std::env::set_var("COLUMNS", "100");
        }
    });
}

/// Path of a committed fixture (the plain name; a `.zst` sibling may exist).
pub fn fixture_path(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures")
        .join(name)
}

/// Parse a committed fixture, preferring a `.zst` sibling when present.
pub fn fixture(name: &str) -> Value {
    spiderhead_oracle_testkit::fixture_json(&fixture_path(name))
}

/// What one invocation produced, normalised like the fixtures.
pub struct Captured {
    pub code: i64,
    pub stdout: String,
    pub stderr: String,
}

/// Run the Rust CLI on `argv` with no handlers registered.
pub fn invoke(argv: &[String]) -> Captured {
    init();
    let mut out = Output::new(Format::Json);
    let code = run(argv, &Registry::new(), &mut out).map_or(-1, |c| i64::from(c.0));
    Captured {
        code,
        stdout: out.stdout.replace(TOOLKIT_VERSION, "{VERSION}"),
        stderr: out.stderr.replace(TOOLKIT_VERSION, "{VERSION}"),
    }
}

/// argv of a fixture record.
pub fn argv_of(record: &Value) -> Vec<String> {
    record["argv"].as_array().map_or_else(Vec::new, |a| {
        a.iter()
            .filter_map(|v| v.as_str().map(str::to_owned))
            .collect()
    })
}

/// Line-based unified-style diff (LCS), for readable failure output.
pub fn diff(expected: &str, actual: &str) -> String {
    let a: Vec<&str> = expected.split('\n').collect();
    let b: Vec<&str> = actual.split('\n').collect();
    let mut lcs = vec![vec![0usize; b.len() + 1]; a.len() + 1];
    for i in (0..a.len()).rev() {
        for j in (0..b.len()).rev() {
            lcs[i][j] = if a[i] == b[j] {
                lcs[i + 1][j + 1] + 1
            } else {
                lcs[i + 1][j].max(lcs[i][j + 1])
            };
        }
    }
    let (mut i, mut j) = (0, 0);
    let mut out = String::from("--- fixture\n+++ actual\n");
    while i < a.len() || j < b.len() {
        if i < a.len() && j < b.len() && a[i] == b[j] {
            out.push_str(&format!(" {}\n", a[i]));
            i += 1;
            j += 1;
        } else if j < b.len() && (i == a.len() || lcs[i][j + 1] >= lcs[i + 1][j]) {
            out.push_str(&format!("+{}\n", b[j]));
            j += 1;
        } else {
            out.push_str(&format!("-{}\n", a[i]));
            i += 1;
        }
    }
    out
}

/// Compare one record against the Rust result; `Err` carries a diff.
pub fn compare(key: &str, record: &Value) -> Result<(), String> {
    let got = invoke(&argv_of(record));
    let mut problems = String::new();
    for (name, want, have) in [
        (
            "stdout",
            record["stdout"].as_str().unwrap_or_default(),
            got.stdout.as_str(),
        ),
        (
            "stderr",
            record["stderr"].as_str().unwrap_or_default(),
            got.stderr.as_str(),
        ),
    ] {
        if want != have {
            problems.push_str(&format!(
                "[{key}] {name} differs for argv {:?}\n{}\n",
                argv_of(record),
                diff(want, have)
            ));
        }
    }
    if record["code"].as_i64() != Some(got.code) {
        problems.push_str(&format!(
            "[{key}] exit code: fixture {} actual {}\n",
            record["code"], got.code
        ));
    }
    if problems.is_empty() {
        Ok(())
    } else {
        Err(problems)
    }
}

/// Regenerate a fixture: replay every record's argv through the CLI and store
/// the captured `{argv, code, stdout, stderr}` back in record order.
fn bless_fixture(name: &str, records: &serde_json::Map<String, Value>) {
    let mut out = serde_json::Map::new();
    for (key, record) in records {
        let got = invoke(&argv_of(record));
        let mut blessed = record.clone();
        if let Some(obj) = blessed.as_object_mut() {
            obj.insert("code".to_owned(), Value::from(got.code));
            obj.insert("stdout".to_owned(), Value::from(got.stdout));
            obj.insert("stderr".to_owned(), Value::from(got.stderr));
        }
        out.insert(key.clone(), blessed);
    }
    spiderhead_oracle_diff::bless_json(&fixture_path(name), &Value::Object(out));
}

/// Run every record of a fixture and fail with all diffs (capped) when any differ.
pub fn assert_fixture(name: &str) -> usize {
    let data = fixture(name);
    let records = data
        .as_object()
        .unwrap_or_else(|| panic!("{name}: not an object"));
    if spiderhead_oracle_diff::bless_enabled() {
        bless_fixture(name, records);
        return records.len();
    }
    let failures: Vec<String> = records
        .iter()
        .filter_map(|(k, v)| compare(k, v).err())
        .collect();
    assert!(
        failures.is_empty(),
        "{} of {} records differ in {name}:\n{}",
        failures.len(),
        records.len(),
        failures
            .iter()
            .take(8)
            .cloned()
            .collect::<Vec<_>>()
            .join("\n")
    );
    records.len()
}
