//! Ported tests for `reports/audit_pdf.rs` (reference the reference suite):
//! HTML rendering of the technical-audit PDF model — localization, coverage
//! states, escaping, backlog wrapper, and rejection of bad input.

use serde_json::{json, Value};
use spiderhead_reports::audit_pdf::render_audit_pdf_html;

fn finding_row() -> Value {
    json!({
        "source_ref": {
            "collection": "/findings",
            "index": 0,
            "id": "finding-example-1",
        },
        "record": {
            "severity": "critical",
            "url": "https://example.invalid/каталог/",
            "status_code": 404,
            "occurrences_count": 2,
            "fix_hint": "Restore the missing destination.",
            "details": {"anchor": "Каталог <ссылок>"},
            "locations": [{"source_url": "https://example.invalid/", "anchor": "Каталог"}],
            "remediation_status": "open",
        },
        "display": {
            "check_key": "BROKEN_INTERNAL_LINK",
            "title": "Broken internal link",
            "observation": "The saved audit recorded a missing destination.",
            "reproduction": "The synthetic URL returned HTTP 404.",
        },
    })
}

/// `_model(state=..., findings=...)`: the full sentinel audit PDF model.
fn model(state: &str, findings: Option<Vec<Value>>) -> Value {
    let rows = findings.unwrap_or_else(|| vec![finding_row()]);
    let row_count = rows.len() as i64;
    json!({
        "schema": "spiderhead.technical-audit-pdf/1",
        "source": {
            "kind": "site-audit",
            "schema": "spiderhead.site-audit/1",
            "domain": "example.invalid",
            "url": "https://example.invalid/",
            "generated_at": "2026-10-03T12:00:00Z",
            "run_id": "synthetic-run-1",
            "input_diagnostics": [{"kind": "synthetic", "reason": "Fixture metadata"}],
        },
        "run": {
            "state": state,
            "scope": {"urls_crawled": 12, "scope_reason": "Synthetic sample"},
            "reasons": [
                {
                    "source_field": "crawl_finish_reason",
                    "value": "Synthetic stop after the configured page limit.",
                }
            ],
        },
        "summary": {
            "source": {
                "pages_checked": 12,
                "findings_total": row_count,
                "findings_by_severity": {"critical": 1, "warning": 0, "notice": 0},
            },
            "counts": {
                "findings": {
                    "source_count": row_count,
                    "declared_total": {"state": "reported", "value": row_count},
                    "projected_count": row_count,
                },
                "pages": {
                    "source_count": 2,
                    "declared_total": {"state": "reported", "value": 2},
                    "projected_count": 2,
                },
                "checks": {
                    "declared_total": {"state": "reported", "value": 4},
                    "ran": {"state": "reported", "source_count": 2, "projected_count": 2},
                    "silent": {"state": "unavailable", "source_count": null, "projected_count": 0},
                    "failed": {"state": "reported", "source_count": 1, "projected_count": 1},
                    "skipped": {"state": "reported", "source_count": 1, "projected_count": 1},
                    "disabled": {"state": "reported", "source_count": 0, "projected_count": 0},
                    "capabilities": {"state": "reported", "source_count": 1, "projected_count": 1},
                    "page_tools_failed": {
                        "state": "unavailable",
                        "source_count": null,
                        "projected_count": 0,
                    },
                },
                "backlog": {
                    "state": "recorded",
                    "source_count": 1,
                    "declared_count": {"state": "reported", "value": 1},
                    "projected_count": 1,
                },
            },
        },
        "coverage": {
            "state": "reported",
            "source_evidence": {
                "state": "reported",
                "record": {"population": {"state": "partial", "reason": "Only 12 synthetic URLs."}},
            },
            "source_check_coverage": {
                "state": "unavailable",
                "record": null,
            },
            "groups": {
                "ran": {
                    "state": "reported",
                    "source_count": 2,
                    "projected_count": 2,
                    "records": ["CHECK_RUN_A", "CHECK_RUN_B"],
                },
                "failed": {
                    "state": "reported",
                    "source_count": 1,
                    "projected_count": 1,
                    "records": [{"tool": "CHECK_A", "error": "Synthetic timeout"}],
                },
                "skipped": {
                    "state": "reported",
                    "source_count": 1,
                    "projected_count": 1,
                    "records": [{"id": "CHECK_B", "reason": "Synthetic input unavailable"}],
                },
                "disabled": {
                    "state": "reported",
                    "source_count": 0,
                    "projected_count": 0,
                    "records": [],
                },
                "capabilities": {
                    "state": "reported",
                    "source_count": 1,
                    "projected_count": 1,
                    "records": [
                        {
                            "check": "SYNTHETIC_GROUP",
                            "state": "measured",
                            "reason": "Synthetic evidence",
                        }
                    ],
                },
                "silent": {
                    "state": "unavailable",
                    "source_count": null,
                    "projected_count": 0,
                    "records": [],
                },
                "page_tools_failed": {
                    "state": "unavailable",
                    "source_count": null,
                    "projected_count": 0,
                    "records": [],
                },
            },
            "checks": [
                {
                    "source_ref": {
                        "collection": "summary.tools_failed",
                        "index": 0,
                        "pointer": "#/summary/tools_failed/0",
                    },
                    "id": "CHECK_A",
                    "state": "failed",
                    "reason": "Synthetic timeout",
                    "record": {"tool": "CHECK_A", "error": "Synthetic timeout"},
                },
                {
                    "source_ref": {
                        "collection": "summary.tools_run",
                        "index": 0,
                        "pointer": "#/summary/tools_run/0",
                    },
                    "id": "CHECK_RUN_A",
                    "state": "ran",
                    "reason": null,
                    "record": "CHECK_RUN_A",
                },
                {
                    "source_ref": {
                        "collection": "summary.tools_run",
                        "index": 1,
                        "pointer": "#/summary/tools_run/1",
                    },
                    "id": "CHECK_RUN_B",
                    "state": "ran",
                    "reason": null,
                    "record": "CHECK_RUN_B",
                },
                {
                    "source_ref": {
                        "collection": "summary.evidence_contract.capability_rows",
                        "index": 0,
                        "pointer": "#/summary/evidence_contract/capability_rows/0",
                    },
                    "id": "SYNTHETIC_GROUP",
                    "state": "unreported",
                    "reason": "Synthetic evidence",
                    "record": {
                        "id": "SYNTHETIC_GROUP",
                        "state": "measured",
                        "reason": "Synthetic evidence",
                    },
                },
                {
                    "source_ref": {
                        "collection": "summary.tools_failed",
                        "index": 1,
                        "pointer": "#/summary/tools_failed/1",
                    },
                    "id": "CHECK_B",
                    "state": "skipped",
                    "reason": "Synthetic input unavailable",
                    "record": {"id": "CHECK_B", "reason": "Synthetic input unavailable"},
                },
            ],
        },
        "findings": rows,
        "pages": [
            {
                "source_ref": {"collection": "/pages", "index": 0},
                "record": {
                    "url": "https://example.invalid/",
                    "status": 200,
                    "title": "Synthetic home",
                },
            },
            {
                "source_ref": {"collection": "/pages", "index": 1},
                "record": {
                    "url": "https://example.invalid/last/",
                    "status": 200,
                    "title": "Final synthetic page",
                },
            },
        ],
        "backlog": {
            "state": "recorded",
            "source_count": 1,
            "declared_count": {"state": "reported", "value": 1},
            "projected_count": 1,
            "project": "Synthetic project",
            "status": {"state": "recorded", "counts": {"total": 1}},
            "items": [
                {
                    "source_ref": {
                        "collection": "summary.project_coverage.status.items",
                        "index": 0,
                        "pointer": "#/summary/project_coverage/status/items/0",
                        "id": "task-1",
                    },
                    "record": {
                        "id": "task-1",
                        "title": "Review the sample page",
                        "state": "not_run",
                        "verification_status": "not_requested",
                    },
                }
            ],
        },
        "omissions": [],
    })
}

fn render(model: &Value, lang: &str) -> String {
    render_audit_pdf_html(model, lang, None).expect("render")
}

#[test]
fn renders_localized_audit_report_with_neutral_branding() {
    for (lang, expected) in [
        ("en", "Technical SEO audit"),
        ("ru", "Технический SEO-аудит"),
    ] {
        let html = render_audit_pdf_html(
            &model("partial", None),
            lang,
            Some(&json!({"accent": "#1565C0", "name": "Demo label"})),
        )
        .expect("render");

        assert!(html.contains(&format!("<html lang=\"{lang}\">")));
        assert!(html.contains(&format!("<h1>{expected}</h1>")));
        assert!(html.contains("Demo label"));
        assert!(html.contains("13.333in 7.5in"));
        assert!(html.contains("counter(page)"));
        assert!(html.contains("<svg"));
        if lang == "ru" {
            assert!(html.contains("Аудит сайта"));
            assert!(html.contains("Критические"));
            assert!(html.contains("Число повторений"));
            assert!(html.contains("Проверка исправления"));
            assert!(html.contains("Покрытие проверок по источнику"));
            assert!(html.contains("Не запускалось"));
            assert!(!html.contains("not_run"));
        }
    }
}

#[test]
fn unrequested_backlog_kpi_is_not_presented_as_zero() {
    for (lang, label, status) in [
        ("en", "Project checklist items", "Not requested"),
        ("ru", "Задачи проекта", "Не запрашивалось"),
    ] {
        let mut model = model("partial", None);
        model["backlog"]["state"] = json!("not_requested");
        model["summary"]["counts"]["backlog"] = json!({
            "state": "not_requested",
            "source_count": null,
            "declared_count": {"state": "not_requested", "value": null},
            "projected_count": 0,
        });

        let html = render(&model, lang);

        assert!(html.contains(&format!("<span>{label}</span><strong>{status}</strong>")));
        assert!(!html.contains(&format!("<span>{label}</span><strong>0</strong>")));
    }
}

#[test]
fn partial_scope_and_each_coverage_state_are_visible() {
    let html = render(&model("partial", None), "en");

    for expected in [
        "This audit is partial",
        "Synthetic stop after the configured page limit.",
        "Synthetic timeout",
        "Synthetic input unavailable",
        "Only 12 synthetic URLs.",
        "Source check coverage",
        "SYNTHETIC_GROUP",
    ] {
        assert!(html.contains(expected), "missing {expected}");
    }
}

#[test]
fn coverage_reported_does_not_mean_every_check_passed() {
    let mut model = model("partial", None);
    model["coverage"]["state"] = json!("reported");
    let html = render(&model, "en");

    assert!(
        html.contains("Coverage metadata recorded; individual checks may still be unavailable.")
    );
    assert!(html.contains("Synthetic timeout"));
    assert!(html.contains("Synthetic input unavailable"));
}

#[test]
fn nested_count_groups_and_unavailable_coverage_remain_distinct() {
    let mut model = model("partial", None);
    model["summary"]["counts"]["findings"]["declared_total"]["value"] = json!(9);
    let html = render(&model, "en");

    for expected in [
        "Coverage records",
        "Declared: 9",
        "Source evidence",
        ">Unavailable</td>",
        "Source check coverage",
        "Recorded",
        "Page checks failed",
        "<td>task-1</td><td>Review the sample page</td>",
    ] {
        assert!(html.contains(expected), "missing {expected}");
    }
    assert!(!html.contains("CHECKS_A"));
}

#[test]
fn summary_card_separates_coverage_rows_from_declared_check_inventory() {
    let html = render(&model("partial", None), "en");

    assert!(html.contains("Coverage records"));
    assert!(html.contains(">5</strong>"));
    assert!(html.contains("Check inventory: 4"));
}

#[test]
fn capability_state_uses_measurement_state_from_source_record() {
    let mut model = model("partial", None);
    model["coverage"]["checks"] = json!([
        {
            "source_ref": {
                "collection": "summary.evidence_contract.capability_rows",
                "index": 0,
                "pointer": "#/summary/evidence_contract/capability_rows/0",
            },
            "id": "TITLE_MISSING",
            "state": "unreported",
            "reason": "source evidence was not available",
            "record": {
                "check": "TITLE_MISSING",
                "state": "unavailable",
                "reason": "source evidence was not available",
            },
        }
    ]);
    let html = render(&model, "en");

    assert!(html.contains("TITLE_MISSING"));
    assert!(html.contains(">Unavailable</td>"));
    assert!(html.contains("source evidence was not available"));
}

#[test]
fn backlog_wrapper_keeps_source_reference_and_record_fields() {
    let html = render(&model("partial", None), "en");

    assert!(html.contains("<td>task-1</td><td>Review the sample page</td>"));
    assert!(html.contains("<td>Not run</td><td>Not requested</td>"));
}

#[test]
fn page_tool_failures_are_rendered_from_model_coverage_rows() {
    let mut model = model("partial", None);
    model["coverage"]["groups"]["page_tools_failed"] = json!({
        "state": "reported",
        "source_count": 1,
        "projected_count": 1,
        "records": [{"tool": "HTML_TOOL", "failed_pages": 3, "pages_checked": 3}],
    });
    model["summary"]["counts"]["checks"]["page_tools_failed"] = json!({
        "state": "reported",
        "source_count": 1,
        "projected_count": 1,
    });
    model["coverage"]["checks"]
        .as_array_mut()
        .unwrap()
        .push(json!({
            "source_ref": {
                "collection": "summary.page_tools_failed",
                "index": 0,
                "pointer": "#/summary/page_tools_failed/0",
            },
            "id": "HTML_TOOL",
            "state": "failed",
            "reason": "All 3 synthetic pages failed",
            "record": {"tool": "HTML_TOOL", "failed_pages": 3, "pages_checked": 3},
        }));
    let html = render(&model, "en");

    assert!(html.contains("Page checks failed"));
    assert!(html.contains("HTML_TOOL"));
    assert!(html.contains("All 3 synthetic pages failed; Errors on 3 of 3 pages"));
}

#[test]
fn nested_summary_and_coverage_evidence_are_readable_without_raw_json() {
    let mut model = model("partial", None);
    let source = model["summary"]["source"].as_object_mut().unwrap();
    source.insert("tools_run".to_owned(), json!(["parse", "robots"]));
    source.insert(
        "tools_failed".to_owned(),
        json!([{"tool": "robots_check", "error": "Synthetic unavailable response"}]),
    );
    source.insert(
        "findings_by_severity".to_owned(),
        json!({"critical": 1, "warning": 0}),
    );
    model["coverage"]["source_evidence"]["record"] = json!({
        "scan_identity_state": "partial",
        "scan_uuid": "synthetic-scan-1",
        "evidence_contract": {
            "capability_rows": [
                {"check": "TITLE_MISSING", "state": "measured", "reason": "Synthetic fixture"}
            ]
        },
    });
    model["coverage"]["source_evidence"]["source_ref"] =
        json!({"pointer": "#/summary/evidence_contract"});
    model["coverage"]["checks"] = json!([
        {
            "source_ref": {
                "collection": "summary.tools_run",
                "index": 0,
                "pointer": "#/summary/tools_run/0",
            },
            "id": "parse",
            "state": "ran",
            "record": "parse",
        },
        {
            "source_ref": {
                "collection": "summary.tools_failed",
                "index": 0,
                "pointer": "#/summary/tools_failed/0",
            },
            "id": "robots_check",
            "state": "failed",
            "reason": "Synthetic unavailable response",
            "record": {"tool": "robots_check", "error": "Synthetic unavailable response"},
        },
        {
            "source_ref": {
                "collection": "summary.evidence_contract.capability_rows",
                "index": 0,
                "pointer": "#/summary/evidence_contract/capability_rows/0",
            },
            "id": "TITLE_MISSING",
            "state": "measured",
            "reason": "Synthetic fixture",
            "record": {
                "check": "TITLE_MISSING",
                "state": "measured",
                "reason": "Synthetic fixture",
            },
        },
    ]);

    let html = render(&model, "en");

    for expected in [
        "Scan identity status: partial",
        "Scan ID: synthetic-scan-1",
        "parse",
        "robots_check",
        "Synthetic unavailable response",
        "1 Capability records listed below",
        "TITLE_MISSING",
        "Synthetic fixture",
        "#/summary/evidence_contract",
        "#/summary/tools_failed/0",
        "Source reference",
    ] {
        assert!(html.contains(expected), "missing {expected}");
    }
    for absent in [
        "{\"critical\":1,\"warning\":0}",
        "{\"capability_rows\"",
        "{\"tool\":\"robots_check\"",
    ] {
        assert!(!html.contains(absent), "unexpected raw JSON {absent}");
    }
}

#[test]
fn generated_finding_fallbacks_are_localized_in_russian() {
    for (check, title, reason, expected_title, expected_reason) in [
        (
            "TITLE_MISSING",
            "Title element is missing",
            "no stable saved-evidence reference is present in this audit",
            "Отсутствует заголовок страницы",
            "В этом аудите нет стабильной ссылки на сохранённое свидетельство",
        ),
        (
            "SCHEMA_MISSING",
            "Audit finding",
            "saved evidence is unavailable",
            "Проблема аудита",
            "Сохранённое свидетельство недоступно",
        ),
    ] {
        let mut model = model("partial", None);
        let display = model["findings"][0]["display"].as_object_mut().unwrap();
        display.insert("check_key".to_owned(), json!(check));
        display.insert("title".to_owned(), json!(title));
        display.insert(
            "evidence_reference".to_owned(),
            json!({"state": "unavailable", "reason": reason}),
        );

        let html = render(&model, "ru");

        assert!(html.contains(expected_title), "missing {expected_title}");
        assert!(html.contains(expected_reason), "missing {expected_reason}");
        assert!(!html.contains(title), "leaked {title}");
        assert!(!html.contains(reason), "leaked {reason}");
    }
}

#[test]
fn complete_run_status_does_not_claim_full_site_coverage() {
    for (lang, expected, op_label) in [
        (
            "en",
            "Completed within the recorded scope; this does not establish exhaustive site coverage.",
            "Bounded site audit",
        ),
        (
            "ru",
            "Завершён в пределах записанного объёма; это не подтверждает полный обход сайта.",
            "Ограниченный аудит сайта",
        ),
    ] {
        let mut model = model("complete", None);
        model["run"]["scope"]["operation"] = json!("bounded_site_audit");
        model["run"]["reasons"] = json!([]);
        let html = render(&model, lang);

        assert!(html.contains(expected), "missing {expected}");
        assert!(html.contains(op_label), "missing {op_label}");
    }
}

#[test]
fn unknown_run_and_absent_counts_remain_unknown() {
    let mut model = model("unknown", None);
    model["summary"]["counts"]["pages"]["projected_count"] = Value::Null;
    for name in ["ran", "failed", "skipped", "disabled"] {
        model["summary"]["counts"]["checks"][name] = json!({
            "state": "unavailable",
            "source_count": null,
            "projected_count": 0,
        });
    }
    model["coverage"]["state"] = json!("unavailable");
    model["coverage"]["checks"] = json!([]);
    let html = render(&model, "en");

    for expected in [
        "data-state=\"unknown\"",
        "The source does not establish whether this audit completed.",
        "Not reported",
        "Check-state counts were not supplied by the source audit.",
    ] {
        assert!(html.contains(expected), "missing {expected}");
    }
}

#[test]
fn render_escapes_source_text_and_preserves_raw_evidence_fields() {
    let mut model = model("partial", None);
    model["findings"][0]["record"]["details"] = json!({"payload": "<script>alert(1)</script>"});
    let html = render(&model, "en");

    assert!(!html.contains("<script>alert(1)</script>"));
    assert!(html.contains("&lt;script&gt;alert(1)&lt;/script&gt;"));
    assert!(!html.contains("occurrences_count"));
    assert!(html.contains("Occurrences count"));
    assert!(html.contains("https://example.invalid/каталог/"));
    assert!(html.contains("https://example.invalid/last/"));
}

#[test]
fn evidence_reference_is_human_readable_and_preserves_state() {
    for (lang, state, expected_status, labels) in [
        (
            "en",
            "unavailable",
            "Status: Unavailable",
            [
                "Evidence ID: evidence-1",
                "Source table: page_observations",
                "Observation ID: observation-1",
                "Role: finding",
            ],
        ),
        (
            "ru",
            "partial",
            "Статус: Частичный",
            [
                "Идентификатор свидетельства: evidence-1",
                "Таблица источника: page_observations",
                "Идентификатор наблюдения: observation-1",
                "Роль: finding",
            ],
        ),
        (
            "en",
            "future_state",
            "Status: future_state",
            [
                "Evidence ID: evidence-1",
                "Source table: page_observations",
                "Observation ID: observation-1",
                "Role: finding",
            ],
        ),
    ] {
        let mut model = model("partial", None);
        model["findings"][0]["display"]["evidence_reference"] = json!({
            "state": state,
            "reason": "no stable saved-evidence reference is present",
            "id": "evidence-1",
            "source_table": "page_observations",
            "observation_id": "observation-1",
            "role": "finding",
        });

        let html = render(&model, lang);

        assert!(html.contains(expected_status), "missing {expected_status}");
        assert!(html.contains("no stable saved-evidence reference is present"));
        for label in labels {
            assert!(html.contains(label), "missing {label}");
        }
        assert!(!html.contains("{\"id\":\"evidence-1\""));
    }
}

#[test]
fn large_synthetic_findings_keep_first_and_last_records_without_caps() {
    let findings: Vec<Value> = (0..150)
        .map(|index| {
            json!({
                "source_ref": {
                    "collection": "/findings",
                    "index": index,
                    "id_if_present": format!("sample-{index:04}"),
                },
                "record": {
                    "severity": "warning",
                    "url": format!("https://example.invalid/sample-{index:04}/"),
                },
                "display": {"title": format!("Synthetic finding {index:04}")},
            })
        })
        .collect();
    let html = render(&model("partial", Some(findings)), "en");

    assert_eq!(
        html.matches("class=\"finding-card severity-warning\"")
            .count(),
        150
    );
    assert!(html.contains("Synthetic finding 0000"));
    assert!(html.contains("Synthetic finding 0149"));
    assert!(html.contains("sample-0149"));
}

#[test]
fn rejects_unrecognized_model_schema() {
    for model in [
        json!({}),
        json!({"schema": "spiderhead.technical-audit-pdf/2"}),
    ] {
        let err = render_audit_pdf_html(&model, "en", None).expect_err("rejected");
        assert!(
            err.contains("spiderhead.technical-audit-pdf/1"),
            "{err} must name the supported schema"
        );
    }
}

#[test]
fn rejects_unsupported_language() {
    let err = render_audit_pdf_html(&model("partial", None), "fr", None).expect_err("rejected");
    assert!(err.contains("unsupported report language"), "{err}");
}
