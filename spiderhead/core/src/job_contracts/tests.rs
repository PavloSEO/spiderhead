use super::*;
use crate::host::FakeHost;
use serde_json::json;

fn options_from(value: Value) -> Result<ScanOptions, serde_json::Error> {
    serde_json::from_value(value)
}

// Covers the scan-options half; remote project limits are tested with the remote backend.
#[test]
fn scan_options_default_to_the_documented_limits() {
    let defaults = ScanOptions::default();
    assert_eq!(defaults.max_crawl_seconds, 3_600);
    assert_eq!(defaults.max_urls, 200);
    assert_eq!(defaults.max_requests, 20_000);
    assert!(defaults.clone().validated().is_ok());
}

#[test]
fn duration_ceiling_and_types_are_enforced() {
    for bad in [
        json!(0),
        json!(true),
        json!(MAX_REMOTE_CRAWL_SECONDS + 1),
        json!("60"),
    ] {
        let refused = options_from(json!({"max_crawl_seconds": bad}))
            .map_err(|error| error.to_string())
            .and_then(|options| options.validated().map_err(|error| error.to_string()));
        assert!(refused.is_err(), "accepted {bad}");
    }
    let ceiling = options_from(json!({"max_crawl_seconds": MAX_REMOTE_CRAWL_SECONDS}))
        .unwrap()
        .validated();
    assert!(ceiling.is_ok());
}

// Also covers the render cap: the js renderer caps escalation at 300 seconds.
#[test]
fn js_rendering_caps_escalation_at_300_seconds() {
    let host = FakeHost::default();
    let long = ScanOptions {
        max_crawl_seconds: 7 * 24 * 60 * 60,
        rendering_mode: RenderingMode::Js,
        ..ScanOptions::default()
    };
    let rendered = long.effective_config(&host).unwrap();
    assert_eq!(
        rendered["rendering"]["escalation"]["max_render_seconds"],
        json!(300)
    );
    assert_eq!(
        rendered["limits"]["max_crawl_seconds"],
        json!(7 * 24 * 60 * 60)
    );
    let short = ScanOptions {
        max_crawl_seconds: 120,
        rendering_mode: RenderingMode::Js,
        ..ScanOptions::default()
    };
    let capped = short.effective_config(&host).unwrap();
    assert_eq!(
        capped["rendering"]["escalation"]["max_render_seconds"],
        json!(120)
    );
}

#[test]
fn unknown_option_fields_are_refused() {
    assert!(options_from(json!({"path": "/etc"})).is_err());
}

#[test]
fn submission_accepts_a_plain_https_target() {
    let submission = ScanSubmission {
        target_url: "https://example.com/path?q=1".into(),
        options: ScanOptions::default(),
    };
    assert!(submission.validated().is_ok());
}

#[test]
fn submission_refuses_unsafe_or_malformed_targets() {
    let refused = [
        "ftp://example.com/",
        "https://user:pw@example.com/",
        "https://user@example.com/",
        "https://example.com/#fragment",
        "https://example.com:0/",
        "https://example.com:99999/",
        " https://example.com/",
        "https://exa mple.com/",
        "https:///nohost",
        "short",
    ];
    for target in refused {
        let submission = ScanSubmission {
            target_url: target.into(),
            options: ScanOptions::default(),
        };
        assert!(submission.validated().is_err(), "accepted {target:?}");
    }
}

#[test]
fn submission_fingerprint_is_stable_and_order_free() {
    let first: ScanSubmission = serde_json::from_value(json!({
        "target_url": "https://example.com/",
        "options": {"max_urls": 5, "max_depth": 2}
    }))
    .unwrap();
    let again: ScanSubmission = serde_json::from_value(json!({
        "options": {"max_depth": 2, "max_urls": 5},
        "target_url": "https://example.com/"
    }))
    .unwrap();
    assert_eq!(first.fingerprint(), again.fingerprint());
    assert_eq!(first.fingerprint().len(), 64);
}

#[test]
fn job_status_bounds_are_checked() {
    let status = JobStatus {
        job_id: String::new(),
        project_id: "p".into(),
        state: JobState::Queued,
        created_at: "2026-10-27T00:00:00Z".into(),
        started_at: None,
        finished_at: None,
        finish_reason: None,
        progress: JobProgress {
            queued: 0,
            inflight: 0,
            done: 0,
            excluded: 0,
            pages: 0,
        },
    };
    assert!(status.clone().validated().is_err());
    let fixed = JobStatus {
        job_id: "job-1".into(),
        ..status
    };
    assert!(fixed.validated().is_ok());
}

#[test]
fn complete_result_needs_evidence_and_an_audit() {
    let job: JobStatus = serde_json::from_value(json!({
        "job_id": "job-1",
        "project_id": "alpha",
        "state": "finished",
        "created_at": "2026-10-27T00:00:00Z",
        "progress": {"queued": 0, "inflight": 0, "done": 3, "excluded": 0, "pages": 3}
    }))
    .unwrap();
    let no_evidence = JobResult {
        job: job.clone(),
        coverage: Coverage::Complete,
        evidence: None,
        artifacts: vec![],
        audit_available: true,
        audit_reason: String::new(),
    };
    assert!(no_evidence.validated().is_err());
    let no_reason = JobResult {
        job,
        coverage: Coverage::Partial,
        evidence: None,
        artifacts: vec![],
        audit_available: false,
        audit_reason: String::new(),
    };
    assert!(no_reason.validated().is_err());
}

#[test]
fn principal_allows_only_granted_permissions_per_project() {
    let principal = Principal {
        subject: "svc".into(),
        grants: BTreeMap::from([(
            "alpha".to_owned(),
            BTreeSet::from([Permission::Submit, Permission::Read]),
        )]),
    };
    assert!(principal.allows("alpha", Permission::Submit));
    assert!(!principal.allows("alpha", Permission::Cancel));
    assert!(!principal.allows("beta", Permission::Submit));
}

#[test]
fn permission_and_state_strings_match_the_wire_format() {
    assert_eq!(
        serde_json::to_value(Permission::Result).unwrap(),
        json!("scan:result")
    );
    assert_eq!(
        serde_json::to_value(JobState::CancelRequested).unwrap(),
        json!("cancel_requested")
    );
    assert_eq!(
        serde_json::to_value(RenderingMode::Js).unwrap(),
        json!("js")
    );
}
