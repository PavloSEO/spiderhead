//! Parity for the targeted verification handler and the remediation recheck
//! that composes it: selections, the offline evidence contract, the live
//! policy gate, and one guarded measured recheck over the retained oracle
//! fixture — against the behaviours pinned by the oracle's own tests.

use std::path::{Path, PathBuf};

use serde_json::{json, Value};

use spiderhead_core::crawl_settings::{load, manifest, LoadInput};
use spiderhead_core::host::FakeHost;
use spiderhead_mcp::handlers::dispatch;
use spiderhead_store::audit_v2::AuditV2Reader;
use spiderhead_store::ledger::ingest::ingest_scan;
use spiderhead_store::ledger::lifecycle::{transition_occurrence, Transition};
use spiderhead_store::ledger::queries::{
    ledger_summary, read_cases, remediation_summary, CaseFilter,
};

const A: &str = "https://example.test/a";
const BEFORE_TS: &str = "2026-10-01T00:00:00Z";
const AFTER_TS: &str = "2026-10-02T00:00:00Z";

/// The raw scan fixture of the oracle's own suite; its audit.v2 companion
/// sits next to it.
fn oracle_scan() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../oracle/raw-scans/basic/scan.sqlite")
}

/// An empty remediation ledger whose site binding matches the oracle fixture.
fn ledger_fixture() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../store/tests/fixtures/ledger-pre.sqlite")
}

fn scratch(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("sh-verify-{}-{tag}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("scratch dir");
    dir
}

fn cleanup(dir: &Path) {
    let _ = std::fs::remove_dir_all(dir);
}

/// The default results-affecting manifest, as the oracle's `_audit` helper
/// records it.
fn default_crawl_config() -> Value {
    let host = FakeHost::default();
    let settings = load(&LoadInput::default(), &host).expect("default settings");
    Value::Object(
        manifest(&settings, &host)
            .expect("manifest")
            .into_iter()
            .collect(),
    )
}

/// The synthetic baseline audit the oracle's tests build.
fn synth_audit(
    urls: &[&str],
    issues: &[Value],
    scan_uuid: Option<&str>,
    generated_at: Option<&str>,
) -> Value {
    let mut run = json!({
        "generated_at": generated_at.map(Value::from).unwrap_or(Value::Null),
        "source": "https://example.test/",
        "crawl_config": default_crawl_config(),
        "checks_skipped": [],
        "checks_disabled": [],
        "crawl_partial": false,
    });
    if generated_at.is_none() {
        run.as_object_mut().unwrap().remove("generated_at");
    }
    let mut summary = json!({
        "check_coverage": {
            "checks_silent_ids": ["TITLE_MISSING", "DESC_MISSING", "CANONICAL_MISSING"]
        },
    });
    if let Some(id) = scan_uuid {
        summary["evidence_contract"] = json!({"scan_uuid": id});
    }
    json!({
        "schema_version": "2.0",
        "run": run,
        "summary": summary,
        "pages": urls
            .iter()
            .map(|url| {
                json!({
                    "url": url,
                    "status_code": 200,
                    "content_type": "text/html",
                    "metrics": {"representation": "static"},
                })
            })
            .collect::<Vec<_>>(),
        "issues": issues,
    })
}

fn issue(id: &str, check: &str, url: &str) -> Value {
    json!({"id": id, "check": check, "target_url": url, "status_code": 200})
}

#[test]
fn graph_only_selection_never_launches_a_recrawl() {
    let work = scratch("graph-only");
    let graph = issue("ISSUE-000004", "LOW_LINK_SCORE", A);
    let baseline = synth_audit(&[A], std::slice::from_ref(&graph), None, Some(BEFORE_TS));
    let result = dispatch(
        "verify_fixes",
        &json!({
            "baseline": baseline,
            "finding_ids": [graph["id"]],
            "out_dir": work.join("verify").to_string_lossy(),
        }),
    )
    .expect("graph-only run");
    assert_eq!(result["selection"]["urls"], json!([]));
    assert_eq!(result["findings"][0]["status"], json!("not_verifiable"));
    cleanup(&work);
}

#[test]
fn offline_handler_writes_immutable_linked_json_and_focused_report() {
    let work = scratch("offline");
    let baseline = synth_audit(
        &[A],
        &[issue("ISSUE-000001", "TITLE_MISSING", A)],
        Some("before-scan"),
        Some(BEFORE_TS),
    );
    let after = synth_audit(&[A], &[], Some("after-scan"), Some(AFTER_TS));
    let out = work.join("verification");
    let result = dispatch(
        "verify_fixes",
        &json!({
            "baseline": baseline,
            "after": after,
            "finding_ids": ["ISSUE-000001"],
            "out_dir": out.to_string_lossy(),
        }),
    )
    .expect("offline run");
    assert_eq!(result["summary"]["resolved"], json!(1), "{result}");
    let saved: Value = serde_json::from_str(
        &std::fs::read_to_string(out.join("verification.json")).expect("saved json"),
    )
    .expect("saved json parses");
    assert_eq!(
        saved["baseline"]["audit_sha256"],
        result["baseline"]["audit_sha256"]
    );
    assert_eq!(saved["findings"][0]["before"]["id"], json!("ISSUE-000001"));
    let report = std::fs::read_to_string(out.join("verification.md")).expect("saved md");
    assert!(report.contains(A), "report names the verified URL");

    let repeat = dispatch(
        "verify_fixes",
        &json!({
            "baseline": baseline,
            "after": after,
            "urls": [A],
            "out_dir": out.to_string_lossy(),
        }),
    )
    .expect_err("same destination refused");
    assert!(repeat.to_string().contains("already exists"), "{repeat}");

    let view = work.join("view.json");
    std::fs::write(
        &view,
        json!({"schema_version": "verification_view.v1", "finding_ids": ["ISSUE-000001"]})
            .to_string(),
    )
    .unwrap();
    let from_view = dispatch(
        "verify_fixes",
        &json!({
            "baseline": baseline,
            "after": after,
            "view": view.to_string_lossy(),
            "out_dir": work.join("from-view").to_string_lossy(),
        }),
    )
    .expect("view run");
    assert_eq!(
        from_view["selection"]["finding_ids"],
        json!(["ISSUE-000001"])
    );
    cleanup(&work);
}

/// One gap case: baseline UUID, after UUID, baseline stamp, after stamp, and
/// the reason fragment both records must carry.
type GapCase = (
    Option<&'static str>,
    Option<&'static str>,
    Option<&'static str>,
    Option<&'static str>,
    &'static str,
);

#[test]
fn offline_after_requires_distinct_later_observation() {
    let cases: [GapCase; 6] = [
        (
            Some("same-scan"),
            Some("same-scan"),
            Some(BEFORE_TS),
            Some(AFTER_TS),
            "same scan UUID",
        ),
        (
            Some("before-scan"),
            Some("after-scan"),
            Some(AFTER_TS),
            Some(BEFORE_TS),
            "not later",
        ),
        (
            Some("before-scan"),
            Some("after-scan"),
            Some(BEFORE_TS),
            Some(BEFORE_TS),
            "not later",
        ),
        (
            None,
            Some("after-scan"),
            Some(BEFORE_TS),
            Some(AFTER_TS),
            "lacks a recorded scan UUID",
        ),
        (
            Some("before-scan"),
            None,
            Some(BEFORE_TS),
            Some(AFTER_TS),
            "lacks a recorded scan UUID",
        ),
        (
            Some("before-scan"),
            Some("after-scan"),
            Some("2026-10-01T00:00:00"),
            Some(AFTER_TS),
            "lacks a timezone-aware",
        ),
    ];
    let work = scratch("gap");
    for (index, (before_id, after_id, before_ts, after_ts, reason)) in cases.into_iter().enumerate()
    {
        let baseline = synth_audit(
            &[A],
            &[issue("ISSUE-000001", "TITLE_MISSING", A)],
            before_id,
            before_ts,
        );
        let after = synth_audit(&[A], &[], after_id, after_ts);
        let out = work.join(format!("verify-{index}"));
        let result = dispatch(
            "verify_fixes",
            &json!({
                "baseline": baseline,
                "after": after,
                "finding_ids": ["ISSUE-000001"],
                "out_dir": out.to_string_lossy(),
            }),
        )
        .unwrap_or_else(|error| panic!("case {index}: {error}"));
        assert_eq!(
            result["summary"],
            json!({"resolved": 0, "persisting": 0, "changed": 0, "not_verifiable": 1}),
            "case {index} summary"
        );
        assert_eq!(result["collection"]["state"], json!("not_verifiable"));
        assert!(
            result["collection"]["reason"]
                .as_str()
                .unwrap_or_default()
                .contains(reason),
            "case {index} collection reason"
        );
        assert!(
            result["findings"][0]["reason"]
                .as_str()
                .unwrap_or_default()
                .contains(reason),
            "case {index} finding reason"
        );
        let saved: Value = serde_json::from_str(
            &std::fs::read_to_string(out.join("verification.json")).expect("saved json"),
        )
        .unwrap();
        assert_eq!(saved["summary"]["resolved"], json!(0));
    }
    cleanup(&work);
}

#[test]
fn verification_artifact_does_not_change_the_remediation_ledger() {
    let work = scratch("ledger-unchanged");
    let ledger = work.join("ledger.sqlite");
    std::fs::copy(ledger_fixture(), &ledger).expect("ledger copy");
    ingest_scan(Path::new(&ledger).into(), &oracle_scan()).expect("ingest");
    let bytes_before = std::fs::read(&ledger).expect("ledger bytes");
    let summary_before = ledger_summary(Path::new(&ledger).into()).expect("summary");

    let baseline = synth_audit(
        &[A],
        &[issue("ISSUE-000001", "TITLE_MISSING", A)],
        Some("before-scan"),
        Some(BEFORE_TS),
    );
    let after = synth_audit(&[A], &[], Some("after-scan"), Some(AFTER_TS));
    let result = dispatch(
        "verify_fixes",
        &json!({
            "baseline": baseline,
            "after": after,
            "finding_ids": ["ISSUE-000001"],
            "out_dir": work.join("verification").to_string_lossy(),
        }),
    )
    .expect("offline run");
    assert_eq!(result["summary"]["resolved"], json!(1));
    assert!(work
        .join("verification")
        .join("verification.json")
        .is_file());
    assert_eq!(
        std::fs::read(&ledger).expect("ledger bytes"),
        bytes_before,
        "verify_fixes must not write to the ledger"
    );
    let summary_after = ledger_summary(Path::new(&ledger).into()).expect("summary");
    assert_eq!(json!(summary_after), json!(summary_before));
    cleanup(&work);
}

#[test]
fn policy_mismatch_stops_before_network_and_records_not_verifiable() {
    let work = scratch("policy");
    let mut baseline = synth_audit(
        &[A],
        &[issue("ISSUE-000001", "TITLE_MISSING", A)],
        None,
        Some(BEFORE_TS),
    );
    baseline["run"]["crawl_config"]["robots.policy"] = json!("ignore");
    let config = work.join("config.json");
    std::fs::write(&config, "{}").unwrap();
    let result = dispatch(
        "verify_fixes",
        &json!({
            "baseline": baseline,
            "urls": [A],
            "config": config.to_string_lossy(),
            "out_dir": work.join("verify").to_string_lossy(),
        }),
    )
    .expect("policy-gated run");
    assert_eq!(result["collection"]["state"], json!("not_run"));
    assert_eq!(result["summary"]["not_verifiable"], json!(1));
    assert!(
        result["findings"][0]["reason"]
            .as_str()
            .unwrap_or_default()
            .contains("robots.policy"),
        "{result}"
    );
    cleanup(&work);
}

/// Copy of the oracle fixture wired into the empty ledger fixture, with one
/// TITLE_TOO_SHORT case moved to `recheck_pending` through three decisions.
fn pending_case(dir: &Path, tag: &str) -> (PathBuf, String, i64) {
    let ledger = dir.join(format!("ledger-{tag}.sqlite"));
    std::fs::copy(ledger_fixture(), &ledger).expect("ledger copy");
    ingest_scan(Path::new(&ledger).into(), &oracle_scan()).expect("ingest");
    let cases = read_cases(
        Path::new(&ledger).into(),
        &CaseFilter {
            check: Some("TITLE_TOO_SHORT"),
            url: None,
            finding_key: None,
        },
        Some(50),
        0,
    )
    .expect("cases");
    let occurrence = &cases["findings"][0]["occurrences"][0];
    let key = occurrence["occurrence_key"]
        .as_str()
        .expect("key")
        .to_owned();
    let mut revision = ledger_summary(Path::new(&ledger).into()).expect("summary")
        ["ledger_revision"]
        .as_i64()
        .expect("revision");
    for state in ["verified", "fix_reported", "recheck_pending"] {
        transition_occurrence(
            Path::new(&ledger).into(),
            &Transition {
                occurrence_key: &key,
                state,
                actor: "reviewer",
                reason: &format!("move to {state}"),
                expected_revision: revision,
                observation_id: None,
                decided_at: None,
            },
        )
        .unwrap_or_else(|error| panic!("transition to {state}: {error}"));
        revision += 1;
    }
    (ledger, key, revision)
}

/// The after document of the e2e recheck: the same scan refrozen later, with
/// the verified check silent and the issue gone.
fn after_document(dir: &Path) -> PathBuf {
    let reader = AuditV2Reader::new(&oracle_scan(), true, false, None).expect("audit.v2 reader");
    let raw = reader.document_chunks(None).expect("document").concat();
    let mut after: Value = serde_json::from_str(&raw).expect("document parses");
    after["issues"] = json!([]);
    // The recorded contract UUID wins over `run.scan_uuid`, so both move.
    after["run"]["scan_uuid"] = json!("later-synthetic-scan");
    after["summary"]["evidence_contract"]["scan_uuid"] = json!("later-synthetic-scan");
    // The oracle fixture was generated on 2026-10-27; the recheck evidence
    // must be a strictly later observation.
    after["run"]["generated_at"] = json!("2027-01-02T00:00:00Z");
    after["summary"]["check_coverage"] = json!({"checks_silent_ids": ["TITLE_TOO_SHORT"]});
    let path = dir.join("after.json");
    std::fs::write(&path, after.to_string()).expect("after document");
    path
}

#[test]
fn ledger_selected_synthetic_recheck_records_measured_artifact_and_task_coverage() {
    let work = scratch("recheck-e2e");
    let (ledger, key, revision) = pending_case(&work, "e2e");
    let after = after_document(&work);
    let result = dispatch(
        "remediation_recheck",
        &json!({
            "ledger": ledger.to_string_lossy(),
            "baseline": oracle_scan().to_string_lossy(),
            "occurrence_keys": [key],
            "actor": "loopback-recheck",
            "expected_revision": revision,
            "task_id": "title-fix",
            "after": after.to_string_lossy(),
            "out_dir": work.join("verification").to_string_lossy(),
        }),
    )
    .expect("guarded recheck");
    assert_eq!(
        result["verification"]["summary"]["resolved"],
        json!(1),
        "{}",
        result["verification"]
    );

    let cases = read_cases(
        Path::new(&ledger).into(),
        &CaseFilter {
            check: Some("TITLE_TOO_SHORT"),
            url: None,
            finding_key: None,
        },
        Some(50),
        0,
    )
    .expect("cases after recheck");
    let occurrence = &cases["findings"][0]["occurrences"][0];
    assert_eq!(occurrence["current_state"], json!("resolved"));
    let binding = &occurrence["decisions"]
        .as_array()
        .expect("decisions")
        .last()
        .expect("a decision")["verification"];
    assert_eq!(binding["task_id"], json!("title-fix"));
    assert_eq!(binding["measured"], json!(true));
    let artifact = Path::new(binding["artifact_path"].as_str().expect("artifact path"));
    assert!(artifact.is_file(), "measured artifact retained");

    let summary = remediation_summary(Path::new(&ledger).into()).expect("ledger summary");
    assert_eq!(
        summary["ledger_revision"],
        result["recording"]["ledger_revision"]
    );
    assert_eq!(summary["counts"]["resolved"], json!(1));
    // The retained oracle fixture carries grouping evidence, so the scope is
    // partial: the represented share is measured, the whole-scope share is
    // withheld (the oracle's `_membership_scope` behaves identically).
    assert_eq!(summary["scope"]["state"], json!("partial"));
    let task = &summary["tasks"]["title-fix"];
    assert_eq!(
        task["represented_resolved_percent"].as_f64(),
        Some(100.0),
        "task: {task}"
    );
    assert!(task["resolved_percent"].is_null());
    cleanup(&work);
}

// malformed selection, a stale revision and a non-pending case.
#[test]
fn remediation_recheck_validates_selection_revision_and_state() {
    let work = scratch("recheck-guard");
    let missing = dispatch(
        "remediation_recheck",
        &json!({"ledger": "/unused.sqlite", "actor": "a", "out_dir": "/unused"}),
    )
    .expect_err("empty selection refused");
    assert_eq!(
        missing.to_string(),
        "occurrence_keys must be a nonempty list of exact ledger case keys"
    );
    let duplicated = dispatch(
        "remediation_recheck",
        &json!({
            "ledger": "/unused.sqlite",
            "occurrence_keys": ["k", "k"],
            "actor": "a",
            "out_dir": "/unused"
        }),
    )
    .expect_err("duplicate keys refused");
    assert_eq!(
        duplicated.to_string(),
        "occurrence_keys must be a distinct bounded case selection"
    );

    let (ledger, key, revision) = pending_case(&work, "guard");
    let stale = dispatch(
        "remediation_recheck",
        &json!({
            "ledger": ledger.to_string_lossy(),
            "baseline": oracle_scan().to_string_lossy(),
            "occurrence_keys": [key],
            "actor": "loopback-recheck",
            "expected_revision": revision + 100,
            "out_dir": work.join("v").to_string_lossy(),
        }),
    )
    .expect_err("stale revision refused");
    assert_eq!(
        stale.to_string(),
        "ledger revision changed; reread cases before starting a recheck"
    );

    // A fresh ledger copy whose case is still `detected` cannot be rechecked.
    let ledger = work.join("ledger-detected.sqlite");
    std::fs::copy(ledger_fixture(), &ledger).expect("ledger copy");
    ingest_scan(Path::new(&ledger).into(), &oracle_scan()).expect("ingest");
    let cases = read_cases(
        Path::new(&ledger).into(),
        &CaseFilter {
            check: Some("TITLE_TOO_SHORT"),
            url: None,
            finding_key: None,
        },
        Some(50),
        0,
    )
    .expect("cases");
    let detected = cases["findings"][0]["occurrences"][0]["occurrence_key"]
        .as_str()
        .expect("key")
        .to_owned();
    let revision = ledger_summary(Path::new(&ledger).into()).expect("summary")["ledger_revision"]
        .as_i64()
        .expect("revision");
    let not_pending = dispatch(
        "remediation_recheck",
        &json!({
            "ledger": ledger.to_string_lossy(),
            "baseline": oracle_scan().to_string_lossy(),
            "occurrence_keys": [detected],
            "actor": "loopback-recheck",
            "expected_revision": revision,
            "out_dir": work.join("v2").to_string_lossy(),
        }),
    )
    .expect_err("non-pending case refused");
    assert_eq!(
        not_pending.to_string(),
        "selected case must be recheck_pending before execution"
    );
    cleanup(&work);
}
