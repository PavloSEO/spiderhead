//! Self-consistency scan parity over generated run directories.

mod common;

use serde_json::Value;
use spiderhead_log_analyzer::runscan::{load_run, scan};

use common::{assert_same, load};

#[test]
fn scan_results_match_for_every_case() {
    for case in load("runscan_cases.json").as_array().unwrap() {
        let tmp = tempfile::tempdir().unwrap();
        let run = tmp.path().join("run");
        std::fs::create_dir_all(&run).unwrap();
        let pages = case["pages"].as_array().unwrap();
        let name = case["name"].as_str().unwrap();
        if !pages.is_empty() || name == "no_pages" {
            let mut body: String = pages.iter().map(|p| format!("{p}\n")).collect();
            body.push_str("{truncated");
            std::fs::write(run.join("pages.jsonl"), body).unwrap();
        }
        if !case["audit"].is_null() {
            std::fs::write(run.join("audit.json"), case["audit"].to_string()).unwrap();
        }
        if let Some(decisions) = case.get("decisions").and_then(Value::as_array) {
            let body: String = decisions.iter().map(|d| format!("{d}\n")).collect();
            std::fs::write(run.join("decisions.jsonl"), body).unwrap();
        }
        let mut images = None;
        if let Some(spec) = case.get("images") {
            let dir = tmp.path().join("images");
            std::fs::create_dir_all(&dir).unwrap();
            std::fs::write(dir.join("manifest.json"), spec["manifest"].to_string()).unwrap();
            for (file, size) in spec["files"].as_object().unwrap() {
                std::fs::write(dir.join(file), vec![b'x'; size.as_u64().unwrap() as usize])
                    .unwrap();
            }
            images = Some(dir.to_string_lossy().into_owned());
        }
        let artifacts = load_run(&run.to_string_lossy(), images.as_deref()).unwrap();
        let max = case
            .get("max_per_rule")
            .and_then(Value::as_u64)
            .unwrap_or(20) as usize;
        assert_same(name, &case["expected"], &scan(&artifacts, max), &[]);
    }
}
