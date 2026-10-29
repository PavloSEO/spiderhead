//! CLI surface parity for the verification domain: the `verify-fixes`
//! command resolves its baseline, runs the offline comparison and prints the
//! document the reference CLI prints — one saved-run shape against the oracle.

use std::path::{Path, PathBuf};

use serde_json::{json, Value};

use spiderhead_cli::{registry, run, Output};

const A: &str = "https://example.test/a";
const BEFORE_TS: &str = "2026-10-01T00:00:00Z";
const AFTER_TS: &str = "2026-10-02T00:00:00Z";

fn scratch(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("sh-cli-verify-{}-{tag}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("scratch dir");
    dir
}

/// The synthetic audit the oracle's verification tests build.
fn synth_audit(issues: &[Value], scan_uuid: &str, generated_at: &str) -> Value {
    json!({
        "schema_version": "2.0",
        "run": {
            "generated_at": generated_at,
            "source": "https://example.test/",
            "crawl_config": {},
            "checks_skipped": [],
            "checks_disabled": [],
            "crawl_partial": false,
        },
        "summary": {
            "check_coverage": {
                "checks_silent_ids": ["TITLE_MISSING", "DESC_MISSING", "CANONICAL_MISSING"]
            },
            "evidence_contract": {"scan_uuid": scan_uuid},
        },
        "pages": [{
            "url": A,
            "status_code": 200,
            "content_type": "text/html",
            "metrics": {"representation": "static"},
        }],
        "issues": issues,
    })
}

#[test]
fn cli_offline_run_reports_one_resolution() {
    let work = scratch("cli");
    let baseline_path = work.join("before.json");
    let after_path = work.join("after.json");
    std::fs::write(
        &baseline_path,
        synth_audit(
            &[json!({"id": "ISSUE-000001", "check": "TITLE_MISSING", "target_url": A, "status_code": 200})],
            "before-scan",
            BEFORE_TS,
        )
        .to_string(),
    )
    .unwrap();
    std::fs::write(
        &after_path,
        synth_audit(&[], "after-scan", AFTER_TS).to_string(),
    )
    .unwrap();
    let out_dir = work.join("verification");

    let mut out = Output::default();
    let exit = run(
        &[
            "verify-fixes".to_owned(),
            "--baseline".to_owned(),
            baseline_path.to_string_lossy().into_owned(),
            "--after".to_owned(),
            after_path.to_string_lossy().into_owned(),
            "--finding-ids".to_owned(),
            "ISSUE-000001".to_owned(),
            "--out-dir".to_owned(),
            out_dir.to_string_lossy().into_owned(),
        ],
        &registry(),
        &mut out,
    )
    .expect("verify-fixes runs");
    assert_eq!(exit, spiderhead_cli::ExitCode::SUCCESS, "{out:?}");
    let printed: Value = serde_json::from_str(&out.stdout).expect("stdout parses as one document");
    assert_eq!(printed["summary"]["resolved"], json!(1));
    assert_eq!(printed["findings"][0]["status"], json!("resolved"));
    let saved: Value = serde_json::from_str(
        &std::fs::read_to_string(out_dir.join("verification.json")).expect("saved json"),
    )
    .expect("saved json parses");
    assert_eq!(saved["summary"]["resolved"], json!(1));
    let _ = std::fs::remove_dir_all(&work);
}

// failure to a usage exit with the reference message.
#[test]
fn cli_refuses_an_existing_destination() {
    let work = scratch("cli-exists");
    let baseline_path = work.join("before.json");
    let after_path = work.join("after.json");
    std::fs::write(
        &baseline_path,
        synth_audit(
            &[json!({"id": "ISSUE-000001", "check": "TITLE_MISSING", "target_url": A, "status_code": 200})],
            "before-scan",
            BEFORE_TS,
        )
        .to_string(),
    )
    .unwrap();
    std::fs::write(
        &after_path,
        synth_audit(&[], "after-scan", AFTER_TS).to_string(),
    )
    .unwrap();
    let out_dir = work.join("verification");
    std::fs::create_dir_all(&out_dir).unwrap();

    let mut out = Output::default();
    let exit = run(
        &[
            "verify-fixes".to_owned(),
            "--baseline".to_owned(),
            baseline_path.to_string_lossy().into_owned(),
            "--after".to_owned(),
            after_path.to_string_lossy().into_owned(),
            "--finding-ids".to_owned(),
            "ISSUE-000001".to_owned(),
            "--out-dir".to_owned(),
            out_dir.to_string_lossy().into_owned(),
        ],
        &registry(),
        &mut out,
    )
    .expect("verify-fixes runs");
    assert_ne!(exit, spiderhead_cli::ExitCode::SUCCESS);
    assert!(
        out.stderr.contains("already exists"),
        "destination failure reaches stderr: {out:?}"
    );
    let _ = std::fs::remove_dir_all(&work);
}

// failure before anything runs.
#[test]
fn cli_requires_out_dir() {
    let mut out = Output::default();
    let exit = run(
        &[
            "verify-fixes".to_owned(),
            "--baseline".to_owned(),
            "{}".to_owned(),
        ],
        &registry(),
        &mut out,
    )
    .expect("verify-fixes runs");
    assert_ne!(exit, spiderhead_cli::ExitCode::SUCCESS);
    assert!(out.stderr.contains("out_dir required"), "{out:?}");
    let work = Path::new(&std::env::temp_dir())
        .join(format!("sh-cli-verify-{}-unused", std::process::id()));
    let _ = std::fs::remove_dir_all(&work);
}
