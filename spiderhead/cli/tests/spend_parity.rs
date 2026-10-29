//! Integration test: the Rust `spend-report` handler reproduces the oracle's
//! captured outputs — full report, `--since`, and the CSV export.

use std::path::Path;
use std::process::Command;

use regex::Regex;
use serde_json::Value;

/// The oracle's normalisation, without a fixture server: the work path and
/// timestamps become stable tokens.
struct Norm {
    work: String,
    ts_re: Regex,
}

impl Norm {
    fn new(work: &Path) -> Norm {
        Norm {
            work: work.to_string_lossy().into_owned(),
            ts_re: Regex::new(r"\d{4}-\d\d-\d\d[T ]\d\d:\d\d:\d\d(\.\d+)?(Z|[+-]\d\d:?\d\d)?")
                .expect("ts regex"),
        }
    }

    fn s(&self, text: &str) -> String {
        self.ts_re
            .replace_all(&text.replace(&self.work, "<WORK>"), "<TS>")
            .into_owned()
    }
}

#[test]
fn spend_report_matches_the_oracle() {
    let fixture: Value = serde_json::from_str(
        &std::fs::read_to_string(
            Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/spend/report.json"),
        )
        .expect("fixture file"),
    )
    .expect("fixture json");
    let work = std::env::temp_dir().join(format!("sh-spend-parity-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&work);
    std::fs::create_dir_all(&work).expect("work dir");
    let journal = work.join("spend.jsonl");
    std::fs::write(&journal, fixture["journal"].as_str().unwrap_or_default()).expect("journal");
    let norm = Norm::new(&work);
    for (name, case) in fixture["cases"].as_object().expect("cases") {
        let argv: Vec<String> = case["argv"]
            .as_array()
            .expect("argv")
            .iter()
            .map(|part| {
                part.as_str()
                    .unwrap_or_default()
                    .replace("{WORK}", &work.to_string_lossy())
            })
            .collect();
        let output = Command::new(env!("CARGO_BIN_EXE_spiderhead"))
            .args(&argv)
            .current_dir(&work)
            .env("LC_ALL", "C")
            .env("TZ", "UTC")
            .env("SEOHEAD_SPEND_LOG", &journal)
            .output()
            .expect("binary runs");
        let mut problems = String::new();
        let exit = output.status.code().unwrap_or_default();
        let expected_exit = case["exit"].as_i64().unwrap_or_default();
        if i64::from(exit) != expected_exit {
            problems += &format!("  exit: expected {expected_exit}, got {exit}\n");
        }
        let stdout = String::from_utf8_lossy(&output.stdout);
        let expected_stdout = case["stdout"].as_str().unwrap_or_default();
        let actual_stdout = norm.s(&stdout);
        if actual_stdout != expected_stdout {
            problems += &format!(
                "  stdout: expected {} chars, got {}\n",
                expected_stdout.len(),
                actual_stdout.len()
            );
        }
        let stderr = String::from_utf8_lossy(&output.stderr);
        let expected_stderr = case["stderr"].as_str().unwrap_or_default();
        let actual_stderr = norm.s(&stderr);
        if actual_stderr != expected_stderr {
            problems += &format!("  stderr: expected {expected_stderr:?}, got {actual_stderr:?}\n");
        }
        if let Some(expected_file) = case["out_file"].as_str() {
            // the reference read_text translates \r\n (the CSV terminator) to \n.
            let actual_file = std::fs::read_to_string(work.join("spend.csv"))
                .map(|text| norm.s(text.replace('\r', "").as_str()))
                .unwrap_or_default();
            if actual_file != expected_file {
                problems += &format!(
                    "  out_file: expected {} chars, got {}\n",
                    expected_file.len(),
                    actual_file.len()
                );
            }
        }
        assert!(problems.is_empty(), "{name} diverges:\n{problems}");
    }
    let _ = std::fs::remove_dir_all(&work);
}
