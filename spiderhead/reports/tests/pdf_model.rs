//! Ported offline tests for the lossless semantic technical-audit PDF model
//! (reference the reference suite).
//!
//! The checklist test exercises the attached `summary.project_coverage`
//! snapshot: the `project=` path binding (`project_coverage::load_snapshot`)
//! stays a declared gap at this layer because projects depends on reports,
//! so live binding belongs to the handler crates that see both.

use std::path::PathBuf;

use serde_json::{json, Value};
use spiderhead_reports::audit_pdf::render_audit_pdf_html;
use spiderhead_reports::document::SITE_AUDIT_SCHEMA;
use spiderhead_reports::pdf_model::{build_pdf_model, PdfModelError, SCHEMA};

fn site_audit(overrides: Value) -> Value {
    let mut document = json!({
        "ok": true,
        "schema": SITE_AUDIT_SCHEMA,
        "domain": "audit.example.test",
        "url": "https://audit.example.test/",
        "generated_at": "2026-10-03T12:00:00Z",
        "findings": [],
        "pages": [],
        "summary": {
            "pages_checked": 0,
            "findings_total": 0,
            "findings_by_severity": {"critical": 0, "warning": 0, "notice": 0},
        },
    });
    if let (Some(base), Some(extra)) = (document.as_object_mut(), overrides.as_object()) {
        for (k, v) in extra {
            base.insert(k.clone(), v.clone());
        }
    }
    document
}

fn sf_audit(overrides: Value) -> Value {
    let mut document = json!({
        "schema_version": "2.0",
        "run": {
            "project": "audit.example.test",
            "source": "https://audit.example.test/",
            "scan_uuid": "scan-fixture-1",
            "generated_at": "2026-10-03T12:00:00Z",
            "crawl_valid": true,
            "crawl_partial": false,
        },
        "summary": {
            "totals": {"urls_crawled": 0, "issues_total": 0},
            "by_severity": {"critical": 0, "warning": 0, "notice": 0},
            "by_check": {},
        },
        "issues": [],
        "pages": [],
        "groups": [],
    });
    if let (Some(base), Some(extra)) = (document.as_object_mut(), overrides.as_object()) {
        for (k, v) in extra {
            base.insert(k.clone(), v.clone());
        }
    }
    document
}

fn build(document: &Value) -> Value {
    build_pdf_model(document, &[], None, None).expect("build_pdf_model")
}

fn err(document: &Value) -> String {
    match build_pdf_model(document, &[], None, None) {
        Err(PdfModelError(message)) => message,
        Ok(_) => panic!("build_pdf_model must refuse the document"),
    }
}

#[test]
fn site_audit_model_preserves_identity_rows_and_declared_counts() {
    let finding = json!({
        "id": "finding-1",
        "source": "render_check",
        "severity": "warning",
        "url": "https://audit.example.test/page",
        "text": "Recorded observation",
        "check": "TITLE_MISSING",
        "details": {"source_value": "retained"},
    });
    let page = json!({"url": "https://audit.example.test/page", "status": 200, "title": "Example"});
    let document = site_audit(json!({
        "findings": [finding.clone()],
        "pages": [page.clone()],
        "summary": {
            "pages_checked": 17,
            "findings_total": 9,
            "findings_by_severity": {"critical": 1, "warning": 2, "notice": 6},
        },
    }));

    let model = build(&document);
    assert_eq!(model["schema"], json!(SCHEMA));
    assert_eq!(
        model["source"],
        json!({
            "kind": "site-audit",
            "schema": SITE_AUDIT_SCHEMA,
            "domain": "audit.example.test",
            "url": "https://audit.example.test/",
            "generated_at": "2026-10-03T12:00:00Z",
            "run_id": null,
            "input_diagnostics": [],
        })
    );
    assert_eq!(
        model["summary"]["counts"]["findings"],
        json!({
            "source_count": 1,
            "declared_total": {"state": "reported", "value": 9},
            "projected_count": 1,
            "declared_total_source_field": "findings_total",
            "declared_by_severity": {
                "state": "reported",
                "value": {"critical": 1, "warning": 2, "notice": 6},
            },
        })
    );
    assert_eq!(
        model["summary"]["counts"]["pages"]["source_count"],
        json!(1)
    );
    assert_eq!(
        model["summary"]["counts"]["pages"]["declared_total"],
        json!({"state": "reported", "value": 17})
    );
    assert_eq!(
        model["findings"][0]["source_ref"],
        json!({
            "collection": "findings",
            "index": 0,
            "pointer": "#/findings/0",
            "id": "finding-1",
        })
    );
    assert_eq!(model["findings"][0]["record"], finding);
    assert_eq!(
        model["findings"][0]["display"]["observation"],
        json!("Recorded observation")
    );
    assert_eq!(model["pages"][0]["record"], page);
    assert_eq!(
        model["pages"][0]["source_ref"]["pointer"],
        json!("#/pages/0")
    );
    // Declared totals may disagree with the source arrays. Keep both values.
    assert_eq!(model["omissions"], json!([]));
}

#[test]
fn missing_summary_count_is_unavailable_not_zero() {
    let model = build(&site_audit(json!({"summary": {}})));
    assert_eq!(
        model["summary"]["counts"]["findings"]["source_count"],
        json!(0)
    );
    assert_eq!(
        model["summary"]["counts"]["findings"]["projected_count"],
        json!(0)
    );
    assert_eq!(
        model["summary"]["counts"]["findings"]["declared_total"],
        json!({"state": "not_reported", "value": null})
    );
}

#[test]
fn zero_findings_and_empty_pages_are_measured_empty_collections() {
    let model = build(&site_audit(json!({})));
    assert_eq!(
        model["summary"]["counts"]["findings"]["source_count"],
        json!(0)
    );
    assert_eq!(
        model["summary"]["counts"]["findings"]["declared_total"]["value"],
        json!(0)
    );
    assert_eq!(
        model["summary"]["counts"]["pages"]["source_count"],
        json!(0)
    );
    assert_eq!(
        model["summary"]["counts"]["pages"]["declared_total"]["value"],
        json!(0)
    );
    assert_eq!(model["findings"], json!([]));
    assert_eq!(model["pages"], json!([]));
}

#[test]
fn sf_model_preserves_full_source_record_and_collection_identity() {
    let issue = json!({
        "id": "issue-1",
        "check": "BROKEN_INTERNAL_LINK",
        "severity": "critical",
        "message": "Internal link points to a 4xx URL",
        "target_url": "https://audit.example.test/missing",
        "status_code": 404,
        "occurrences_count": 2,
        "fix_hint": "Update the source links.",
        "locations": [{"source_url": "https://audit.example.test/", "anchor": "Catalog"}],
        "details": {"complete_values": (0..15).collect::<Vec<i32>>()},
        "evidence": {"source_table": "issues", "observation_id": "observation-1"},
        "custom_source_field": "must survive",
    });
    let page = json!({"url": "https://audit.example.test/", "status_code": 200, "metrics": {"title": "Home"}});
    let document = sf_audit(json!({
        "issues": [issue.clone()],
        "pages": [page.clone()],
        "summary": {
            "totals": {"urls_crawled": 1, "issues_total": 1},
            "by_severity": {"critical": 1, "warning": 0, "notice": 0},
            "by_check": {"BROKEN_INTERNAL_LINK": 1},
        },
    }));
    let before = document.clone();

    let model = build(&document);
    assert_eq!(model["source"]["kind"], json!("sf-audit"));
    assert_eq!(model["source"]["schema"], json!("2.0"));
    assert_eq!(model["source"]["run_id"], json!("scan-fixture-1"));
    assert_eq!(model["run"]["state"], json!("complete"));
    assert_eq!(
        model["findings"][0]["source_ref"]["pointer"],
        json!("#/issues/0")
    );
    assert_eq!(model["findings"][0]["source_ref"]["id"], json!("issue-1"));
    assert_eq!(model["findings"][0]["record"], issue);
    assert_eq!(
        model["findings"][0]["record"]["details"]["complete_values"],
        json!((0..15).collect::<Vec<i32>>())
    );
    assert!(model["findings"][0]["display"]["details"]
        .as_array()
        .expect("details list")
        .iter()
        .any(|item| item
            .as_str()
            .is_some_and(|s| s.contains("5 more values omitted"))));
    assert_eq!(model["pages"][0]["record"], page);
    assert_eq!(
        model["summary"]["counts"]["findings"]["declared_total"],
        json!({"state": "reported", "value": 1})
    );
    assert_eq!(document, before);
}

#[test]
fn run_scope_states_are_explicit_and_partial_failed_reasons_survive() {
    let partial = sf_audit(json!({
        "run": {
            "source": "https://audit.example.test/",
            "crawl_valid": true,
            "crawl_partial": true,
            "crawl_finish_reason": "url_limit",
            "crawl_scope_note": "2 of 20 declared URLs",
        }
    }));
    let model = build(&partial);
    assert_eq!(model["run"]["state"], json!("partial"));
    assert_eq!(
        model["run"]["partial"],
        json!({"state": "reported", "value": true})
    );
    let reasons: std::collections::BTreeSet<&str> = model["run"]["reasons"]
        .as_array()
        .expect("reasons")
        .iter()
        .filter_map(|item| item["value"].as_str())
        .collect();
    assert_eq!(reasons, ["url_limit"].into_iter().collect());
    assert_eq!(
        model["run"]["scope"]["crawl_scope_note"],
        json!("2 of 20 declared URLs")
    );

    let failed = sf_audit(json!({
        "run": {
            "source": "https://audit.example.test/",
            "crawl_valid": false,
            "crawl_invalid_reason": "no response",
        }
    }));
    let failed_model = build(&failed);
    assert_eq!(failed_model["run"]["state"], json!("failed"));
    let reasons: std::collections::BTreeSet<&str> = failed_model["run"]["reasons"]
        .as_array()
        .expect("reasons")
        .iter()
        .filter_map(|item| item["value"].as_str())
        .collect();
    assert_eq!(reasons, ["no response"].into_iter().collect());

    let unknown = build(&sf_audit(json!({
        "run": {"source": "https://audit.example.test/"}
    })));
    assert_eq!(unknown["run"]["state"], json!("unknown"));
    assert_eq!(
        unknown["run"]["validity"],
        json!({"state": "not_reported", "value": null})
    );
}

#[test]
fn check_coverage_failures_disabled_and_capabilities_remain_distinct() {
    let document = sf_audit(json!({
        "run": {
            "source": "https://audit.example.test/",
            "crawl_valid": true,
            "crawl_partial": false,
            "checks_skipped": [{"id": "SCHEMA_VALIDATE", "reason": "missing dependency"}],
            "checks_disabled": [{"id": "TITLE_LENGTH", "reason": "operator setting"}],
        },
        "summary": {
            "totals": {"urls_crawled": 2, "issues_total": 0},
            "by_severity": {"critical": 0, "warning": 0, "notice": 0},
            "by_check": {},
            "check_coverage": {
                "checks_total": 4,
                "checks_fired": 0,
                "checks_skipped": 1,
                "checks_disabled": 1,
                "checks_silent": 2,
                "checks_silent_ids": ["CANONICAL_MISSING", "H1_MISSING"],
            },
        },
    }));
    let model = build(&document);
    let checks = model["coverage"]["checks"].as_array().expect("checks");
    let states: std::collections::BTreeSet<(&str, &str)> = checks
        .iter()
        .filter_map(|item| Some((item["id"].as_str()?, item["state"].as_str()?)))
        .collect();
    assert!(states.contains(&("SCHEMA_VALIDATE", "skipped")));
    assert!(states.contains(&("TITLE_LENGTH", "disabled")));
    assert!(states.contains(&("CANONICAL_MISSING", "ran_no_findings")));
    assert!(states.contains(&("H1_MISSING", "ran_no_findings")));
    assert_eq!(
        model["coverage"]["source_check_coverage"]["record"]["checks_total"],
        json!(4)
    );
    assert_eq!(
        model["summary"]["counts"]["checks"]["declared_total"],
        json!({"state": "reported", "value": 4})
    );
}

#[test]
fn page_tool_failures_are_normalized_as_failed_coverage_rows() {
    let model = build(&site_audit(json!({
        "summary": {
            "pages_checked": 3,
            "findings_total": 0,
            "page_tools_failed": [{"tool": "HTML_TOOL", "failed_pages": 3, "pages_checked": 3}],
        }
    })));

    let failed = model["coverage"]["checks"].as_array().expect("checks");
    assert_eq!(failed.len(), 1);
    assert_eq!(failed[0]["id"], json!("HTML_TOOL"));
    assert_eq!(failed[0]["state"], json!("failed"));
    assert_eq!(
        failed[0]["record"],
        json!({"tool": "HTML_TOOL", "failed_pages": 3, "pages_checked": 3})
    );
    assert_eq!(
        failed[0]["source_ref"]["pointer"],
        json!("#/summary/page_tools_failed/0")
    );
    assert_eq!(
        model["summary"]["counts"]["checks"]["page_tools_failed"]["source_count"],
        json!(1)
    );
}

#[test]
fn site_audit_evidence_capabilities_preserve_unavailable_and_not_requested() {
    let contract = json!({
        "scan_identity_state": "recorded",
        "scan_uuid": "scan-evidence-1",
        "population": {"urls_crawled": 3, "crawl_partial": true, "scope_reason": "url cap"},
        "capability_rows": [
            {"check": "TITLE_MISSING", "state": "measured", "reason": "captured"},
            {"check": "CRUX_ORIGIN", "state": "not_requested", "reason": "no origin query"},
            {"check": "GSC_URL_INSPECTION", "state": "unavailable", "reason": "not configured"},
        ],
    });
    let document = site_audit(json!({
        "pages": [{"url": "https://audit.example.test/", "status": 200}],
        "summary": {"pages_checked": 1, "findings_total": 0, "evidence_contract": contract},
    }));
    let model = build(&document);
    assert_eq!(model["source"]["run_id"], json!("scan-evidence-1"));
    assert_eq!(model["coverage"]["source_evidence"]["record"], contract);
    let capabilities = model["coverage"]["groups"]["capabilities"]["records"]
        .as_array()
        .expect("capabilities");
    let states: Vec<&str> = capabilities
        .iter()
        .filter_map(|item| item["state"].as_str())
        .collect();
    assert_eq!(states, ["measured", "not_requested", "unavailable"]);
    let check_states: Vec<&str> = model["coverage"]["checks"]
        .as_array()
        .expect("checks")
        .iter()
        .filter_map(|item| item["state"].as_str())
        .collect();
    assert_eq!(check_states, ["measured", "not_requested", "unavailable"]);
    assert_eq!(
        model["summary"]["counts"]["checks"]["capabilities"]["source_count"],
        json!(3)
    );
}

#[test]
fn invalid_site_declared_totals_are_rejected() {
    for (field, value) in [
        ("findings_total", json!(-2)),
        ("pages_checked", json!(-1)),
        ("findings_total", json!("3")),
    ] {
        let document = site_audit(json!({"summary": {field: value}}));
        assert!(err(&document).contains("non-negative integer"));
    }
}

#[test]
fn invalid_sf_by_check_counts_are_rejected() {
    for count in [json!(0), json!(-1), json!("many"), json!(true)] {
        let document = sf_audit(json!({
            "summary": {
                "totals": {"urls_crawled": 1, "issues_total": 0},
                "by_severity": {"critical": 0},
                "by_check": {"TITLE_MISSING": count},
                "check_coverage": {
                    "checks_total": 1,
                    "checks_silent_ids": ["TITLE_MISSING"],
                },
            },
        }));
        assert!(err(&document).contains("positive integer"));
    }
}

#[test]
fn invalid_sf_declared_totals_are_rejected() {
    for count in [json!(-1), json!("1"), json!(true)] {
        let document = sf_audit(json!({
            "summary": {
                "totals": {"urls_crawled": 0, "issues_total": count},
                "by_severity": {},
                "by_check": {},
            },
        }));
        assert!(err(&document).contains("non-negative integer"));
    }
}

#[test]
fn check_cannot_be_both_fired_and_silent() {
    let document = sf_audit(json!({
        "summary": {
            "totals": {"urls_crawled": 1, "issues_total": 3},
            "by_severity": {"critical": 3},
            "by_check": {"H2_DUPLICATE": 3},
            "check_coverage": {
                "checks_total": 1,
                "checks_fired": 1,
                "checks_skipped": 0,
                "checks_disabled": 0,
                "checks_silent": 1,
                "checks_silent_ids": ["H2_DUPLICATE"],
            },
        },
    }));
    assert!(err(&document).contains("by_check and checks_silent_ids"));
}

#[test]
fn check_coverage_counts_are_typed_and_non_negative() {
    for (field, value) in [
        ("checks_total", json!("many")),
        ("checks_fired", json!(-1)),
        ("checks_silent", json!(true)),
    ] {
        let document = sf_audit(json!({
            "summary": {
                "totals": {"urls_crawled": 0, "issues_total": 0},
                "by_severity": {"critical": 0},
                "by_check": {},
                "check_coverage": {field: value},
            },
        }));
        assert!(err(&document).contains("non-negative integer"), "{field}");
    }
}

#[test]
fn sf_severity_and_nested_total_counts_are_typed_and_non_negative() {
    let bad_severity = sf_audit(json!({
        "summary": {
            "totals": {"urls_crawled": 0, "issues_total": 0},
            "by_severity": {"critical": -3},
            "by_check": {},
        },
    }));
    assert!(err(&bad_severity).contains("summary.by_severity.critical"));

    let bad_representation = sf_audit(json!({
        "summary": {
            "totals": {
                "urls_crawled": 0,
                "issues_total": 0,
                "pages_by_representation": {"static": "many"},
            },
            "by_severity": {},
            "by_check": {},
        },
    }));
    assert!(err(&bad_representation).contains("pages_by_representation['static']"));
}

#[test]
fn large_source_arrays_are_conserved_without_caps() {
    let size = 1205usize;
    let document = site_audit(json!({
        "findings": (0..size).map(|i| json!({
            "id": format!("finding-{i}"),
            "severity": "notice",
            "source": "fixture",
            "text": format!("Finding {i}"),
        })).collect::<Vec<_>>(),
        "pages": (0..size).map(|i| json!({
            "url": format!("https://audit.example.test/page/{i}"),
            "status": 200,
        })).collect::<Vec<_>>(),
        "summary": {
            "pages_checked": size,
            "findings_total": size,
            "findings_by_severity": {"critical": 0, "warning": 0, "notice": size},
        },
    }));
    let model = build(&document);
    assert_eq!(
        model["summary"]["counts"]["findings"]["source_count"],
        json!(size)
    );
    assert_eq!(
        model["summary"]["counts"]["findings"]["projected_count"],
        json!(size)
    );
    assert_eq!(
        model["summary"]["counts"]["pages"]["source_count"],
        json!(size)
    );
    assert_eq!(
        model["summary"]["counts"]["pages"]["projected_count"],
        json!(size)
    );
    let findings = model["findings"].as_array().expect("findings");
    let pages = model["pages"].as_array().expect("pages");
    assert_eq!(
        findings[size - 1]["source_ref"]["pointer"],
        json!(format!("#/findings/{}", size - 1))
    );
    assert_eq!(
        pages[size - 1]["source_ref"]["pointer"],
        json!(format!("#/pages/{}", size - 1))
    );
}

#[test]
fn unsupported_or_malformed_documents_are_rejected() {
    let cases: Vec<(Value, &str)> = vec![
        (
            json!({"schema": "spiderhead.site-audit/999", "findings": [], "pages": [], "summary": {}}),
            "schema",
        ),
        (
            json!({"schema": SITE_AUDIT_SCHEMA, "findings": ["not an object"], "pages": [], "summary": {}}),
            "finding objects",
        ),
        (
            json!({"schema": SITE_AUDIT_SCHEMA, "findings": [], "pages": [null], "summary": {}}),
            "page objects",
        ),
    ];
    for (document, message) in cases {
        assert!(err(&document).contains(message), "{message}");
    }
}

#[test]
fn malformed_optional_coverage_containers_are_rejected() {
    let cases: Vec<(Value, &str)> = vec![
        (
            sf_audit(json!({"summary": {"totals": "wrong type"}})),
            "summary.totals",
        ),
        (
            site_audit(json!({
                "summary": {
                    "findings_total": 0,
                    "evidence_contract": {"capability_rows": ["wrong type"]},
                }
            })),
            "capability_rows",
        ),
        (
            site_audit(json!({"summary": {"page_tools_failed": ["wrong type"]}})),
            "page_tools_failed",
        ),
        (
            sf_audit(json!({"run": {"checks_disabled": "wrong type"}})),
            "run.checks_disabled",
        ),
    ];
    for (document, message) in cases {
        assert!(err(&document).contains(message), "{message}");
    }
}

#[test]
fn pdf_branding_uses_collector_provenance_not_shared_schema() {
    let cases: Vec<(Value, &str, &str)> = vec![
        (
            json!({"collector": "spiderhead.crawl", "input_mode": "crawl"}),
            "SEOHEAD native crawl audit",
            "Screaming Frog audit",
        ),
        (
            json!({"collector": "spiderhead.crawl", "input_mode": "reanalysis"}),
            "SEOHEAD native crawl audit",
            "Screaming Frog audit",
        ),
        (
            json!({"input_mode": "exports"}),
            "Screaming Frog audit",
            "SEOHEAD native crawl audit",
        ),
        (
            json!({"collector": "screaming-frog", "input_mode": "crawl"}),
            "Screaming Frog audit",
            "SEOHEAD native crawl audit",
        ),
        (
            json!({"input_mode": "crawl"}),
            "Crawl audit · collector unknown",
            "Screaming Frog audit",
        ),
        (
            json!({"collector": "other", "input_mode": "exports"}),
            "Crawl audit · collector unknown",
            "Screaming Frog audit",
        ),
        (
            json!({}),
            "Crawl audit · collector unknown",
            "Screaming Frog audit",
        ),
    ];
    for (run, expected, unexpected) in cases {
        let mut document = sf_audit(json!({}));
        if let (Some(base), Some(extra)) = (document["run"].as_object_mut(), run.as_object()) {
            for (k, v) in extra {
                base.insert(k.clone(), v.clone());
            }
        }
        let model = build(&document);
        assert_eq!(model["source"]["kind"], json!("sf-audit"));
        assert_eq!(model["source"]["collector"], run["collector"]);
        let rendered = render_audit_pdf_html(&model, "en", None).expect("render");
        assert!(rendered.contains(expected), "{expected} missing");
        assert!(!rendered.contains(unexpected), "{unexpected} present");
    }
}

fn tmp(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("pdf-model-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

#[test]
fn project_checklist_and_verification_status_are_included_only_when_present() {
    let plain = build(&site_audit(json!({})));
    assert_eq!(plain["backlog"]["state"], json!("not_requested"));
    assert_eq!(plain["backlog"]["source_count"], Value::Null);
    assert_eq!(
        plain["summary"]["counts"]["backlog"]["source_count"],
        Value::Null
    );

    let root = tmp("checklist").join("project");
    spiderhead_projects::workspace::create_project(
        &root,
        "https://audit.example.test/",
        None,
        None,
        None,
        None,
    )
    .expect("create_project");
    spiderhead_projects::coverage::initialize_coverage(&root, None).expect("initialize_coverage");
    let revision = spiderhead_projects::coverage::coverage_status(&root, true)
        .expect("coverage_status")["revision"]
        .as_i64()
        .expect("revision");
    spiderhead_projects::coverage::update_item(
        &root,
        &json!({"id": "custom:verify", "title": "Verify saved finding"}),
        revision,
    )
    .expect("update_item");
    let revision = spiderhead_projects::coverage::coverage_status(&root, true)
        .expect("coverage_status")["revision"]
        .as_i64()
        .expect("revision");
    spiderhead_projects::coverage::record_execution(
        &root,
        "custom:verify",
        &json!({
            "status": "succeeded",
            "reason": "Reviewed synthetic evidence",
            "reviewer": "Fixture reviewer",
            "signoff": true,
        }),
        revision,
    )
    .expect("record_execution");

    let expected_items = spiderhead_projects::coverage::coverage_status(&root, true)
        .expect("coverage_status")["items"]
        .as_array()
        .expect("items")
        .len();
    // `load_snapshot` (`project=` binding) cannot live in this crate — projects
    // depends on reports — so the snapshot is attached exactly as the reference
    // `attach_snapshot` does before the model build.
    let opened = spiderhead_projects::workspace::open_project(&root, None).expect("open_project");
    let mut document = site_audit(json!({}));
    document["summary"]
        .as_object_mut()
        .expect("summary")
        .insert(
            "project_coverage".into(),
            json!({
                "project": {
                    "uuid": opened["project"]["project_uuid"],
                    "site": opened["project"]["site"]["target"],
                },
                "status": opened["checklist"],
            }),
        );

    let model = build(&document);
    assert_eq!(model["backlog"]["state"], json!("recorded"));
    assert_eq!(model["backlog"]["source_count"], json!(expected_items));
    assert_eq!(model["backlog"]["projected_count"], json!(expected_items));
    let item = model["backlog"]["items"]
        .as_array()
        .expect("backlog items")
        .iter()
        .map(|entry| &entry["record"])
        .find(|record| record["id"].as_str() == Some("custom:verify"))
        .expect("custom:verify record");
    assert_eq!(item["attempt_status"], json!("succeeded"));
    assert_eq!(item["complete"], json!(true));
    assert_eq!(item["reason"], json!("Reviewed synthetic evidence"));
}
