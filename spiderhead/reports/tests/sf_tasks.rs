//! Ported task-backlog tests from the reference suite.
//!
//! The four tests bound to the `result` fixture (groups_by_check_with_priority,
//! severity_filter, per_issue_grouping, markdown_renders) need the SF analyzer
//! pipeline (`run_audit` + `to_dict`) and stay deferred.

use serde_json::{json, Value};
use spiderhead_reports::sf_tasks::{build_tasks, render_tasks_md};

fn audit_with_occurrences(occurrences_count: i64) -> Value {
    json!({
        "run": {"project": "example.test", "generated_at": "2026-09-05T00:00:00Z"},
        "summary": {"health_score": 80},
        "issues": [{
            "id": "ISSUE-000001",
            "check": "BROKEN_INTERNAL_LINK",
            "severity": "critical",
            "source": "inlinks:Client Error (4xx) Inlinks",
            "message": "Internal link points to a 4xx URL",
            "target_url": "https://example.test/dead",
            "status_code": 404,
            "occurrences_count": occurrences_count,
            "fix_hint": "Replace the shared footer link.",
            "locations": [{
                "source_url": "https://example.test/source-a",
                "anchor": "Old page",
                "link_position": "Footer",
                "link_path": "/html/body/footer/a[1]",
            }],
        }],
    })
}

fn tasks(audit: &Value, config: Option<&Value>) -> Value {
    build_tasks(audit, config).unwrap()
}

#[test]
fn min_occurrences_drops_low_frequency_issue_grouping() {
    let cfg = json!({"tasks_pipeline": {"group_by": "issue", "min_occurrences": 2}});
    let backlog = tasks(&audit_with_occurrences(1), Some(&cfg));
    assert_eq!(backlog["summary"]["tasks_total"], 0);
}

#[test]
fn min_occurrences_keeps_high_frequency_check_grouping() {
    let cfg = json!({"tasks_pipeline": {"group_by": "check", "min_occurrences": 2}});
    let backlog = tasks(&audit_with_occurrences(100), Some(&cfg));
    assert_eq!(backlog["summary"]["tasks_total"], 1);
}

#[test]
fn min_occurrences_same_meaning_both_grouping_modes() {
    for group_by in ["check", "issue"] {
        let cfg = json!({"tasks_pipeline": {"group_by": group_by, "min_occurrences": 5}});
        let dropped = tasks(&audit_with_occurrences(4), Some(&cfg));
        assert_eq!(dropped["summary"]["tasks_total"], 0, "{group_by}");
        let kept = tasks(&audit_with_occurrences(5), Some(&cfg));
        assert_eq!(kept["summary"]["tasks_total"], 1, "{group_by}");
    }
}

fn partial_audit() -> Value {
    json!({
        "run": {
            "project": "example.test",
            "generated_at": "2026-09-05T00:00:00Z",
            "crawl_valid": true,
            "crawl_partial": true,
            "crawl_finish_reason": "url_limit",
        },
        "summary": {
            "health_score": 90,
            "health_score_scope": "1 of 1,000 sitemap URLs crawled — the score describes the crawled subset, not the whole site",
        },
        "issues": [{
            "id": "ISSUE-000001",
            "check": "BROKEN_INTERNAL_LINK",
            "severity": "critical",
            "target_url": "https://example.test/dead",
            "occurrences_count": 1,
            "locations": [],
        }],
    })
}

#[test]
fn partial_crawl_scope_and_reason_carried_into_source() {
    let backlog = tasks(&partial_audit(), None);
    let src = &backlog["source"];
    assert_eq!(src["crawl_partial"], true);
    assert_eq!(src["crawl_finish_reason"], "url_limit");
    assert!(src["health_score_scope"]
        .as_str()
        .unwrap()
        .contains("1 of 1,000"));
}

#[test]
fn partial_crawl_warning_rendered_before_task_list() {
    let md = render_tasks_md(&tasks(&partial_audit(), None));
    assert!(md.contains("Partial crawl"));
    assert!(md.contains("url_limit"));
    assert!(md.contains("1 of 1,000"));
    assert!(md.find("Partial crawl").unwrap() < md.find("## P1").unwrap());
}

#[test]
fn invalid_crawl_warning_stays_separate_from_partial_warning() {
    let mut audit = partial_audit();
    audit["run"]["crawl_valid"] = json!(false);
    audit["run"]["crawl_invalid_reason"] = json!("fetch failed for every URL");
    let md = render_tasks_md(&tasks(&audit, None));
    assert!(md.contains("Crawl failed"));
    assert!(md.contains("Partial crawl"));
    assert!(md.find("Crawl failed").unwrap() < md.find("Partial crawl").unwrap());
}

#[test]
fn complete_crawl_has_no_partial_warning() {
    let audit = json!({
        "run": {"project": "example.test", "generated_at": "2026-09-05T00:00:00Z"},
        "summary": {"health_score": 95},
        "issues": [],
    });
    let backlog = tasks(&audit, None);
    assert_eq!(backlog["source"]["crawl_partial"], false);
    let md = render_tasks_md(&backlog);
    assert!(!md.contains("Partial crawl"));
}

fn broken_link_audit(locations: Value) -> Value {
    let count = locations.as_array().unwrap().len() as i64;
    json!({
        "run": {"project": "example.test", "generated_at": "2026-09-05T00:00:00Z"},
        "summary": {"health_score": 80},
        "issues": [{
            "id": "ISSUE-000001",
            "check": "BROKEN_INTERNAL_LINK",
            "severity": "critical",
            "target_url": "https://example.test/dead",
            "status_code": 404,
            "occurrences_count": count,
            "locations": locations,
        }],
    })
}

fn three_locations() -> Value {
    json!([
        {"source_url": "https://example.test/a", "anchor": "A", "link_position": "Content", "link_path": "/a"},
        {"source_url": "https://example.test/b", "anchor": "B", "link_position": "Footer", "link_path": "/b"},
        {"source_url": "https://example.test/c", "anchor": "C", "link_position": "Footer", "link_path": "/c"},
    ])
}

#[test]
fn target_cap_alone_does_not_truncate_broken_link_locations() {
    let cfg = json!({"tasks_pipeline": {"max_urls_per_task": 1}});
    let backlog = tasks(&broken_link_audit(three_locations()), Some(&cfg));
    let task = &backlog["tasks"][0];
    assert_eq!(task["urls"], json!(["https://example.test/dead"]));
    assert_eq!(task["urls_truncated"], 0);
    let sources: Vec<&str> = task["broken_links"]
        .as_array()
        .unwrap()
        .iter()
        .map(|r| r["source_url"].as_str().unwrap())
        .collect();
    assert_eq!(
        sources,
        [
            "https://example.test/a",
            "https://example.test/b",
            "https://example.test/c"
        ]
    );
    assert_eq!(task["broken_links_total"], 3);
    assert_eq!(task["broken_links_truncated"], 0);
    let md = render_tasks_md(&backlog);
    assert!(md.contains("https://example.test/b"));
    assert!(md.contains("https://example.test/c"));
    assert!(!md.contains("omitted"));
}

#[test]
fn location_cap_reports_totals_and_omissions() {
    let cfg = json!({"tasks_pipeline": {"max_locations_per_task": 1}});
    let backlog = tasks(&broken_link_audit(three_locations()), Some(&cfg));
    let task = &backlog["tasks"][0];
    assert_eq!(
        task["broken_links"][0]["source_url"],
        "https://example.test/a"
    );
    assert_eq!(task["broken_links"].as_array().unwrap().len(), 1);
    assert_eq!(task["broken_links_total"], 3);
    assert_eq!(task["broken_links_truncated"], 2);
    let md = render_tasks_md(&backlog);
    assert!(!md.contains("https://example.test/b"));
    assert!(!md.contains("https://example.test/c"));
    assert!(md.contains("2 more source location(s) omitted"));
}

fn reduced_coverage_audit(partial: bool) -> Value {
    json!({
        "run": {
            "project": "example.test",
            "generated_at": "2026-09-06T00:00:00Z",
            "crawl_valid": true,
            "crawl_partial": partial,
            "crawl_finish_reason": if partial { json!("url_limit") } else { Value::Null },
        },
        "summary": {
            "health_score": 82,
            "health_score_basis": "40 of 96 checks could run; the score is not comparable to a run with full evidence",
            "check_coverage": {
                "checks_total": 96,
                "checks_fired": 40,
                "checks_skipped": 50,
                "checks_disabled": 6,
                "coverage": 0.417,
            },
        },
        "issues": [{
            "check": "MISSING_TITLE",
            "severity": "warning",
            "target_url": "https://example.test/a",
            "occurrences_count": 1,
            "source": "SF",
        }],
    })
}

#[test]
fn reduced_coverage_basis_rendered_for_non_partial_crawl() {
    let md = render_tasks_md(&tasks(&reduced_coverage_audit(false), None));
    assert!(md.contains("40 of 96"));
}

#[test]
fn reduced_coverage_basis_rendered_alongside_partial_warning() {
    let md = render_tasks_md(&tasks(&reduced_coverage_audit(true), None));
    assert!(md.contains("Partial crawl"));
    assert!(md.contains("40 of 96"));
}

#[test]
fn full_coverage_run_has_no_basis_text() {
    let mut audit = reduced_coverage_audit(false);
    audit["summary"]["health_score_basis"] = Value::Null;
    audit["summary"]["check_coverage"] = json!({
        "checks_total": 96,
        "checks_fired": 96,
        "checks_skipped": 0,
        "checks_disabled": 0,
        "coverage": 1.0,
    });
    let md = render_tasks_md(&tasks(&audit, None));
    assert!(!md.contains("not comparable"));
    assert!(!md.contains("checks could run"));
}

fn same_url_repeat_audit(count: usize) -> Value {
    let issues: Vec<Value> = (0..count)
        .map(|_| {
            json!({
                "check": "MISSING_TITLE",
                "severity": "warning",
                "target_url": "https://example.test/a",
                "occurrences_count": 1,
                "source": "SF",
            })
        })
        .collect();
    json!({
        "run": {"project": "example.test", "generated_at": "2026-09-06T00:00:00Z"},
        "summary": {"health_score": 90},
        "issues": issues,
    })
}

#[test]
fn grouped_title_counts_unique_pages_not_records() {
    let backlog = tasks(&same_url_repeat_audit(3), None);
    let task = &backlog["tasks"][0];
    assert_eq!(task["affected_count"], 1);
    assert_eq!(task["urls"], json!(["https://example.test/a"]));
    let title = task["title"].as_str().unwrap();
    assert!(title.contains("1 page"));
    assert!(!title.contains("3 pages"));
}

#[test]
fn grouped_title_still_counts_distinct_urls_correctly() {
    let issues: Vec<Value> = (0..3)
        .map(|i| {
            json!({
                "check": "MISSING_TITLE",
                "severity": "warning",
                "target_url": format!("https://example.test/{i}"),
                "occurrences_count": 1,
                "source": "SF",
            })
        })
        .collect();
    let audit = json!({
        "run": {"project": "example.test", "generated_at": "2026-09-06T00:00:00Z"},
        "summary": {"health_score": 90},
        "issues": issues,
    });
    let backlog = tasks(&audit, None);
    let task = &backlog["tasks"][0];
    assert_eq!(task["affected_count"], 3);
    assert!(task["title"].as_str().unwrap().contains("3 pages"));
}

fn minimal_audit(run: Value) -> Value {
    json!({
        "run": run,
        "summary": {"health_score": 80},
        "issues": [{
            "check": "MISSING_TITLE",
            "severity": "warning",
            "target_url": "https://example.test/a",
            "occurrences_count": 1,
            "source": "SF",
        }],
    })
}

#[test]
fn heading_names_site_from_source_when_project_missing() {
    let audit = minimal_audit(json!({
        "project": null,
        "source": "https://www.profiz.ru/",
        "generated_at": "2026-09-05T00:00:00Z",
    }));
    let md = render_tasks_md(&tasks(&audit, None));
    assert!(md.contains("# Audit Tasks — profiz.ru"));
    assert!(!md.contains("None"));
}

#[test]
fn heading_names_site_from_start_url_when_project_missing() {
    let audit = minimal_audit(json!({
        "project": null,
        "start_url": "https://shop.example.test/",
        "generated_at": "2026-09-05T00:00:00Z",
    }));
    let md = render_tasks_md(&tasks(&audit, None));
    assert!(md.contains("# Audit Tasks — shop.example.test"));
    assert!(!md.contains("None"));
}

#[test]
fn heading_still_uses_project_when_present() {
    let audit =
        minimal_audit(json!({"project": "example.test", "generated_at": "2026-09-05T00:00:00Z"}));
    let md = render_tasks_md(&tasks(&audit, None));
    assert!(md.contains("# Audit Tasks — example.test"));
}

#[test]
fn heading_falls_back_to_neutral_word_when_nothing_names_the_site() {
    let audit = minimal_audit(json!({"generated_at": "2026-09-05T00:00:00Z"}));
    let md = render_tasks_md(&tasks(&audit, None));
    assert!(md.contains("# Audit Tasks — site"));
    assert!(!md.contains("None"));
}

#[test]
fn check_assignment_groups_template_work_without_losing_counts_or_caps() {
    let article_urls: Vec<String> = (0..35)
        .map(|i| format!("https://example.test/articles/{i}"))
        .collect();
    let product_urls: Vec<String> = (0..2)
        .map(|i| format!("https://example.test/products/{i}"))
        .collect();
    let mut issues: Vec<Value> = article_urls
        .iter()
        .enumerate()
        .map(|(i, url)| {
            json!({
                "id": format!("ISSUE-ARTICLE-{i}"),
                "check": "H1_MISSING",
                "severity": "warning",
                "target_url": url,
                "occurrences_count": 1,
                "source": "synthetic audit",
            })
        })
        .collect();
    issues.extend(product_urls.iter().enumerate().map(|(i, url)| {
        json!({
            "id": format!("ISSUE-PRODUCT-{i}"),
            "check": "H1_MISSING",
            "severity": "warning",
            "target_url": url,
            "occurrences_count": 1,
            "source": "synthetic audit",
        })
    }));
    issues.extend((0..2).map(|i| {
        json!({
            "id": format!("ISSUE-UNKNOWN-{i}"),
            "check": "H1_MISSING",
            "severity": "warning",
            "target_url": "https://example.test/contact",
            "occurrences_count": 2,
            "source": "synthetic audit",
        })
    }));
    let audit = json!({
        "run": {"crawl_partial": true, "crawl_finish_reason": "url_limit"},
        "summary": {"health_score": 70, "health_score_scope": "Synthetic partial scope"},
        "issues": issues,
    });
    let config = json!({
        "tasks_pipeline": {
            "group_by": "check_assignment",
            "max_urls_per_task": 3,
            "assignments": [
                {
                    "name": "Article template",
                    "kind": "template",
                    "urls": article_urls,
                    "rationale": "Operator mapped the article render path.",
                },
                {
                    "name": "Product template",
                    "kind": "template",
                    "urls": product_urls,
                    "rationale": "Operator mapped the product render path.",
                    "state": "confirmed",
                    "confirmed_by": "Lead developer",
                    "confirmation": "The same H1 component is rendered on these pages.",
                },
            ],
        }
    });

    let backlog = tasks(&audit, Some(&config));
    assert_eq!(backlog["source"]["crawl_partial"], true);
    assert_eq!(
        backlog["grouping"],
        json!({
            "mode": "operator_assignments",
            "assignment_count": 2,
            "unassigned_findings": 2,
        })
    );
    let all = backlog["tasks"].as_array().unwrap();
    assert_eq!(all.len(), 3);
    let by_name = |name: &str| {
        all.iter()
            .find(|t| t["assignment"]["name"] == name)
            .unwrap()
    };
    let article = by_name("Article template");
    let product = by_name("Product template");
    let unknown = all
        .iter()
        .find(|t| t["assignment"]["state"] == "unassigned")
        .unwrap();
    assert_eq!(article["affected_count"], 35);
    assert_eq!(article["membership"]["affected_urls_total"], 35);
    assert_eq!(article["occurrences"], 35);
    assert_eq!(article["urls"].as_array().unwrap().len(), 3);
    assert_eq!(article["urls_truncated"], 32);
    assert_eq!(article["assignment"]["state"], "declared");
    assert_eq!(
        article["assignment"]["provenance"]["kind"],
        "operator_assignment"
    );
    assert_eq!(product["affected_count"], 2);
    assert_eq!(product["assignment"]["state"], "confirmed");
    assert_eq!(unknown["affected_count"], 1);
    assert_eq!(unknown["occurrences"], 4);
    assert_eq!(unknown["membership"]["findings_total"], 2);
    let md = render_tasks_md(&backlog);
    assert!(md.contains("Partial crawl"));
    assert!(md.contains("Article template (template, declared)"));
}

#[test]
fn check_assignment_uses_validated_segments_as_candidates_not_confirmed_templates() {
    let audit = json!({
        "run": {
            "crawl_config": {"scope.segments": [{"name": "blog", "prefix": "/blog/"}]},
        },
        "summary": {"health_score": 80},
        "pages": [{"url": "https://example.test/blog/one"}],
        "issues": [
            {
                "check": "H1_MISSING",
                "severity": "warning",
                "target_url": "https://example.test/blog/one",
                "occurrences_count": 1,
            },
            {
                "check": "H1_MISSING",
                "severity": "warning",
                "target_url": "https://example.test/other",
                "occurrences_count": 1,
            },
        ],
    });

    let cfg = json!({"tasks_pipeline": {"group_by": "check_assignment"}});
    let backlog = tasks(&audit, Some(&cfg));
    let all = backlog["tasks"].as_array().unwrap();
    let candidate = all
        .iter()
        .find(|t| t["assignment"]["name"] == "blog")
        .unwrap();
    all.iter()
        .find(|t| t["assignment"]["state"] == "unassigned")
        .unwrap();
    assert_eq!(candidate["assignment"]["state"], "candidate");
    assert_eq!(
        candidate["assignment"]["provenance"]["kind"],
        "audit_segment_rules"
    );
    assert_eq!(
        candidate["assignment"]["provenance"]["rules"],
        json!([{"op": "prefix", "field": "path", "value": "/blog/"}])
    );
}
