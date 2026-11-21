//! `excel` stage golden check against the reference baseline fixture
//! (the reference suite → `tests/fixtures/excel_oracle.json`).
//!
//! The fixture records either `expect.result`+`expect.xlsx` (openpyxl cell
//! dump) or `expect.error` — the reference stage itself raises `sqlite3
//! OperationalError` for `scope != "all"` because the `WHERE` fragments are
//! concatenated without a space; the Rust implementation must hit the same error.

mod golden;

use spiderhead_semantics::stages;

#[test]
fn golden_excel() {
    let fx = golden::fixture("excel");
    for scenario in fx["scenarios"].as_array().unwrap() {
        let name = scenario["name"].as_str().unwrap();
        let (store, dir) = golden::setup(scenario);
        let cfg = golden::cfg_of(scenario, &dir);
        let scope = scenario["args"]["scope"].as_str().unwrap_or("all");
        let outcome = stages::excel::run(&store, &cfg, &dir, scope);
        let mut diffs = Vec::new();
        if let Some(want_err) = scenario["expect"].get("error").and_then(|e| e.as_str()) {
            match outcome {
                Err(e) => {
                    if !e.to_string().contains(want_err) {
                        diffs.push(format!("error: rust={} golden={want_err}", e));
                    }
                }
                Ok(v) => diffs.push(format!("expected error, got {v}")),
            }
        } else {
            let mut result = outcome.unwrap();
            let file = dir.join(result["file"].as_str().unwrap());
            result["file"] =
                serde_json::json!(file.file_name().unwrap().to_string_lossy().to_string());
            golden::check_result(scenario, &result, &mut diffs);
            golden::check_xlsx(scenario, &file, &mut diffs);
        }
        golden::check_tables(&store, scenario, &mut diffs);
        assert!(diffs.is_empty(), "scenario {name}:\n{}", diffs.join("\n"));
    }
}
