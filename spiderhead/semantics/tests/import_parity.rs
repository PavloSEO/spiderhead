//! `import` stage vs the reference baseline (`tests/fixtures/import_oracle.json`).

mod oracle;

use serde_json::Value;
use spiderhead_semantics::stages;
use std::path::Path;

fn b64decode(s: &str) -> Vec<u8> {
    fn val(b: u8) -> u8 {
        match b {
            b'A'..=b'Z' => b - b'A',
            b'a'..=b'z' => b - b'a' + 26,
            b'0'..=b'9' => b - b'0' + 52,
            b'+' => 62,
            b'/' => 63,
            _ => 0,
        }
    }
    let bytes: Vec<u8> = s.bytes().filter(|b| *b != b'=').collect();
    let mut out = Vec::with_capacity(bytes.len() * 3 / 4);
    for chunk in bytes.chunks(4) {
        let n =
            chunk.iter().fold(0u32, |acc, &b| acc << 6 | val(b) as u32) << (6 * (4 - chunk.len()));
        out.push((n >> 16) as u8);
        if chunk.len() > 2 {
            out.push((n >> 8) as u8);
        }
        if chunk.len() > 3 {
            out.push(n as u8);
        }
    }
    out
}

fn write_inputs(scenario: &Value, dir: &Path) {
    for spec in scenario["input_files"]
        .as_array()
        .cloned()
        .unwrap_or_default()
    {
        let bytes = b64decode(spec["b64"].as_str().unwrap_or_default());
        std::fs::write(dir.join(spec["name"].as_str().unwrap()), bytes).unwrap();
    }
}

/// An expected error line matches when both its stable head (before the first
/// `:` path separator) and its basename tail appear in the Rust message — the
/// oracle records absolute tmp paths that differ per run.
fn error_matches(got: &str, want: &str) -> bool {
    if got.contains(want) {
        return true;
    }
    let head = want.split(':').next().unwrap_or(want);
    let tail = want.rsplit('/').next().unwrap_or(want);
    got.contains(head.trim()) && got.contains(tail.trim())
}

#[test]
fn import_matches_reference_oracle() {
    let fixture = oracle::fixture("import");
    let mut diffs = Vec::new();
    for scenario in fixture["scenarios"].as_array().unwrap() {
        let name = scenario["name"].as_str().unwrap();
        let (store, dir) = oracle::setup(scenario);
        let cfg = oracle::cfg_of(scenario, &dir);
        write_inputs(scenario, &dir);
        let args = &scenario["args"];
        let file = dir.join(args["file"].as_str().unwrap_or_default());
        let sheet = args.get("sheet").and_then(Value::as_str);
        let mapping = args.get("mapping").and_then(Value::as_object);
        let merge = args["merge"].as_str().unwrap_or("add");
        let result = if args["mode"].as_str() == Some("plan") {
            stages::import::plan(&store, &cfg, &file, sheet, mapping, merge)
        } else {
            stages::import::run(&store, &cfg, &file, sheet, mapping, merge)
        };
        if let Some(want) = scenario["expect"].get("error") {
            match result {
                Err(e) if error_matches(&e.to_string(), want.as_str().unwrap()) => {}
                Err(e) => diffs.push(format!(
                    "{name}: error mismatch rust=`{e}` oracle=`{}`",
                    want.as_str().unwrap()
                )),
                Ok(v) => diffs.push(format!("{name}: expected error, got {v}")),
            }
        } else {
            match result {
                Ok(v) => oracle::check_result(scenario, &v, &mut diffs),
                Err(e) => diffs.push(format!("{name}: unexpected error {e}")),
            }
        }
        let mut scoped = Vec::new();
        oracle::check_tables(&store, scenario, &mut scoped);
        diffs.extend(scoped.into_iter().map(|d| format!("{name}: {d}")));
    }
    assert!(diffs.is_empty(), "{}", diffs.join("\n"));
}
