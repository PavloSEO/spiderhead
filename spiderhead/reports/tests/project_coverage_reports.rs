//! Ported tests for project-checklist coverage in human reports
//! (reference the reference suite).
//!
//! `build_report(project=...)` binds through an injected `ProjectsHost`;
//! the projects crate is a dev-dependency here (dev-dependency cycles are
//! allowed), so the tests wire the same real workspace reads the handler
//! layer injects and exercise the true `load_snapshot` path.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use calamine::{Data, DataType, Reader};
use serde_json::{json, Map, Value};

use spiderhead_projects::coverage::{
    coverage_status, initialize_coverage, record_execution, update_item,
};
use spiderhead_projects::workspace::{create_project, open_project};
use spiderhead_reports::build_report::build_report;
use spiderhead_reports::document::SITE_AUDIT_SCHEMA;
use spiderhead_reports::project_coverage::{
    load_snapshot, protected_destination, protected_project_paths, ProjectsHost,
};

fn tmp(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("coverage-reports-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

fn audit() -> Value {
    json!({
        "schema": SITE_AUDIT_SCHEMA,
        "domain": "example.test",
        "url": "https://example.test/",
        "generated_at": "2026-09-09T00:00:00Z",
        "findings": [],
        "pages": [],
        "summary": {"pages_checked": 1, "findings_total": 0, "findings_by_severity": {}},
    })
}

/// The `project` fixture: checklist with one signed-off custom item whose
/// title and reason lead with `=` (formula-injection surface).
fn project(dir: &Path) -> PathBuf {
    let root = dir.join("project");
    create_project(&root, "https://example.test/", None, None, None, None).expect("create_project");
    initialize_coverage(&root, None).expect("initialize_coverage");
    update_item(
        &root,
        &json!({"id": "custom:formula-review", "title": "=formula title"}),
        revision(&root),
    )
    .expect("update_item");
    record_execution(
        &root,
        "custom:formula-review",
        &json!({
            "status": "succeeded",
            "reason": "=formula reason",
            "reviewer": "Specialist",
            "signoff": true,
        }),
        revision(&root),
    )
    .expect("record_execution");
    root
}

fn revision(root: &Path) -> i64 {
    coverage_status(root, true).expect("coverage_status")["revision"]
        .as_i64()
        .expect("revision")
}

/// The `ProjectsHost` the handler layer injects — the same real workspace
/// reads, wired to the live projects crate.
fn with_host<T>(f: impl FnOnce(&ProjectsHost) -> T) -> T {
    let open = |directory: &str| -> Result<Value, String> {
        open_project(Path::new(directory), None).map_err(|e| e.to_string())
    };
    let status = |directory: &str| -> Result<Value, String> {
        spiderhead_projects::workspace::project_status(Path::new(directory), None, true)
            .map_err(|e| e.to_string())
    };
    let apply = |directory: &str, view: &str, document: &Value, offset: i64| {
        spiderhead_projects::finding_views::apply_view_details(
            Path::new(directory),
            view,
            document,
            offset,
        )
        .map(|applied| (applied.result, applied.selected_rows))
        .map_err(|e| e.to_string())
    };
    let host = ProjectsHost {
        open_project: &open,
        project_status: &status,
        apply_view_details: &apply,
    };
    f(&host)
}

/// `build_report` bound to a project root — the reference `project=` call.
fn report_project(document: &Value, fmt: &str, target: &Path, root: &Path) -> Value {
    with_host(|host| {
        build_report(
            document,
            fmt,
            Some(target.to_str().expect("target")),
            Some(root.to_str().expect("root")),
            None,
            0,
            "en",
            None,
            Some(host),
        )
    })
}

fn report(document: &Value, fmt: &str, target: &Path) -> Value {
    build_report(
        document,
        fmt,
        Some(target.to_str().expect("target")),
        None,
        None,
        0,
        "en",
        None,
        None,
    )
}

/// `json.dumps(value, ensure_ascii=False, sort_keys=True, separators=(",", ":"))`.
fn dumps(value: &Value) -> String {
    match value {
        Value::Null => "null".to_owned(),
        Value::Bool(flag) => flag.to_string(),
        Value::Number(n) => n.to_string(),
        Value::String(text) => serde_json::to_string(text).expect("string"),
        Value::Array(items) => format!(
            "[{}]",
            items.iter().map(dumps).collect::<Vec<_>>().join(",")
        ),
        Value::Object(map) => {
            let inner: BTreeMap<&String, &Value> = map.iter().collect();
            let inner: Vec<String> = inner
                .iter()
                .map(|(k, v)| format!("{}:{}", serde_json::to_string(k).expect("key"), dumps(v)))
                .collect();
            format!("{{{}}}", inner.join(","))
        }
    }
}

/// `csv.DictReader` over the `;` dialect written by `csvfile` (utf-8-sig).
fn csv_rows(path: &Path) -> Vec<Map<String, Value>> {
    let text = std::fs::read_to_string(path).expect("csv text");
    let text = text.strip_prefix('\u{feff}').unwrap_or(&text);
    let mut reader = csv::ReaderBuilder::new()
        .delimiter(b';')
        .from_reader(text.as_bytes());
    let headers = reader.headers().expect("headers").clone();
    reader
        .records()
        .map(|record| {
            headers
                .iter()
                .zip(record.expect("record").iter())
                .map(|(h, v)| (h.to_owned(), json!(v)))
                .collect()
        })
        .collect()
}

/// `load_workbook(...)[sheet].iter_rows(values_only=True)` via calamine.
fn xlsx_rows(path: &Path, sheet: &str) -> Vec<Vec<Data>> {
    let mut workbook: calamine::Xlsx<_> = calamine::open_workbook(path).expect("open xlsx");
    workbook
        .worksheet_range(sheet)
        .expect("worksheet")
        .rows()
        .map(|row| row.to_vec())
        .collect()
}

/// `Document(path).paragraphs` joined by newlines (same extractor as
/// `reports_docx.rs`): every `<w:t>` run, one paragraph per line.
fn docx_text(path: &Path) -> String {
    let file = std::fs::File::open(path).expect("docx file");
    let mut zip = zip::ZipArchive::new(file).expect("docx zip");
    let mut xml = String::new();
    std::io::Read::read_to_string(
        &mut zip.by_name("word/document.xml").expect("document"),
        &mut xml,
    )
    .expect("read document.xml");
    let mut paragraphs = Vec::new();
    let mut text = String::new();
    let mut rest = xml.as_str();
    while let Some(start) = rest.find("<w:t") {
        let after = &rest[start..];
        let Some(close) = after.find('>') else {
            break;
        };
        let Some(end) = after.find("</w:t>") else {
            break;
        };
        let run = &after[close + 1..end];
        let run = run
            .replace("&quot;", "\"")
            .replace("&apos;", "'")
            .replace("&lt;", "<")
            .replace("&gt;", ">")
            .replace("&amp;", "&");
        text.push_str(&run);
        rest = &after[end + 6..];
        if rest.starts_with("</w:r>") || rest.starts_with("<w:br") {
            paragraphs.push(std::mem::take(&mut text));
        }
    }
    paragraphs.push(text);
    paragraphs.join("\n")
}

/// The target list `build_report` would protect for `fmt`.
fn targets_for(fmt: &str, target: &Path) -> Vec<PathBuf> {
    let mut targets = vec![target.to_path_buf()];
    if fmt == "csv" {
        targets.push(target.with_extension("pages.csv"));
        targets.push(target.with_extension("scope.csv"));
        targets.push(target.with_extension("coverage.csv"));
    }
    if fmt == "xlsx" {
        targets.push(PathBuf::from(format!("{}.index.json", target.display())));
    }
    targets
}

#[test]
fn human_reports_include_the_current_project_coverage_snapshot() {
    for fmt in ["md", "csv", "xlsx", "docx"] {
        let dir = tmp(&format!("snapshot-{fmt}"));
        let root = project(&dir);
        let target = dir.join(format!("audit.{fmt}"));
        let result = report_project(&audit(), fmt, &target, &root);
        assert_eq!(result["ok"], json!(true), "{fmt}: {result}");
        let expected = coverage_status(&root, true).expect("coverage_status");
        match fmt {
            "md" => {
                let text = std::fs::read_to_string(&target).expect("md text");
                assert!(text.contains("## Project checklist coverage"));
                assert!(text.contains(&format!("| {} |", expected["counts"]["total"])));
                assert!(text.contains("=formula reason"));
                assert!(text.contains("specialist review or signoff"));
            }
            "csv" => {
                let coverage = target.with_extension("coverage.csv");
                assert!(coverage.exists());
                assert!(result["outputs"]
                    .as_array()
                    .expect("outputs")
                    .contains(&json!(coverage.display().to_string())));
                let rows = csv_rows(&coverage);
                let formula = rows
                    .iter()
                    .find(|row| row["Item ID"] == json!("custom:formula-review"))
                    .expect("formula row");
                assert_eq!(formula["Counts"], json!(dumps(&expected["counts"])));
                assert_eq!(
                    formula["Measurement"],
                    json!("{\"reason\":\"specialist review or signoff; no automatic measurement is implied\",\"state\":\"not_measured\"}")
                );
                assert_eq!(formula["Reason"], json!("'=formula reason"));
            }
            "xlsx" => {
                let rows = xlsx_rows(&target, "Project Coverage");
                assert!(rows.iter().any(|row| row.len() >= 2
                    && row[0].get_string() == Some("Checklist state")
                    && row[1].get_string() == Some("initialized")));
                let formula = rows
                    .iter()
                    .find(|row| {
                        row.first().and_then(Data::get_string) == Some("custom:formula-review")
                    })
                    .expect("formula row");
                assert_eq!(formula[1].get_string(), Some("'=formula title"));
                assert_eq!(
                    formula.last().and_then(Data::get_string),
                    Some("'=formula reason")
                );
            }
            _ => {
                let text = docx_text(&target);
                assert!(text.contains("Project Checklist Coverage"));
                assert!(text.contains("=formula reason"));
                assert!(text.contains("specialist review or signoff"));
            }
        }
    }
}

#[test]
fn no_project_keeps_existing_human_outputs_and_json_passthrough_unchanged() {
    let dir = tmp("plain");
    let _root = project(&dir);
    let markdown = dir.join("plain.md");
    let plain = report(&audit(), "md", &markdown);
    assert_eq!(plain["ok"], json!(true));
    assert!(!std::fs::read_to_string(&markdown)
        .expect("md")
        .contains("Project checklist coverage"));
    let csv_target = dir.join("plain.csv");
    let csv_result = report(&audit(), "csv", &csv_target);
    assert_eq!(csv_result["ok"], json!(true));
    assert!(!csv_target.with_extension("coverage.csv").exists());
    // With no bound project the json output relays the document verbatim.
    let target = dir.join("audit.json");
    let result = report(&audit(), "json", &target);
    assert_eq!(result["ok"], json!(true));
    let written: Value =
        serde_json::from_str(&std::fs::read_to_string(&target).expect("json")).expect("json parse");
    assert_eq!(written, audit());
}

#[test]
fn project_report_refuses_a_different_audit_site() {
    let dir = tmp("refuse");
    let root = project(&dir);
    let other = json!({
        "schema": SITE_AUDIT_SCHEMA,
        "domain": "other.test",
        "url": "https://other.test/",
        "generated_at": "2026-09-09T00:00:00Z",
        "findings": [],
        "pages": [],
        "summary": {"pages_checked": 1, "findings_total": 0, "findings_by_severity": {}},
    });
    let error = with_host(|host| {
        load_snapshot(root.to_str().expect("root"), &other, "site-audit", host)
            .map_err(|e| e.to_string())
    })
    .expect_err("bind must fail");
    assert!(
        error.contains("does not match audit source identity"),
        "{error}"
    );
}

#[test]
fn project_report_binds_sf_audit_to_its_recorded_crawl_identity() {
    let dir = tmp("sf-identity");
    let root = project(&dir);
    let audit = json!({
        "schema_version": "2.0",
        "run": {"source": "https://example.test/"},
        "summary": {"totals": {}, "by_severity": {}, "by_check": {}},
        "issues": [],
        "pages": [],
        "groups": [],
    });
    let result = report_project(&audit, "md", &dir.join("sf.md"), &root);
    assert_eq!(result["ok"], json!(true), "{result}");
}

#[test]
fn dependency_blocker_is_visible_when_a_completed_parent_becomes_stale() {
    for fmt in ["md", "csv", "xlsx", "docx"] {
        let dir = tmp(&format!("blocked-{fmt}"));
        let root = project(&dir);
        update_item(
            &root,
            &json!({"id": "custom:parent", "title": "Parent"}),
            revision(&root),
        )
        .expect("parent");
        update_item(
            &root,
            &json!({
                "id": "custom:child",
                "title": "Child\nreview",
                "dependencies": ["custom:parent"],
            }),
            revision(&root),
        )
        .expect("child");
        for item_id in ["custom:parent", "custom:child"] {
            record_execution(
                &root,
                item_id,
                &json!({
                    "status": "succeeded",
                    "reason": "Reviewed",
                    "reviewer": "Specialist",
                    "signoff": true,
                }),
                revision(&root),
            )
            .expect("record");
        }
        update_item(
            &root,
            &json!({"id": "custom:parent", "title": "Parent revised"}),
            revision(&root),
        )
        .expect("revise parent");
        let status = coverage_status(&root, true).expect("coverage_status");
        let child = status["items"]
            .as_array()
            .expect("items")
            .iter()
            .find(|item| item["id"] == json!("custom:child"))
            .expect("child");
        assert_eq!(child["state"], json!("run"));
        assert_eq!(child["complete"], json!(false));
        assert_eq!(child["blocked_by"], json!(["custom:parent"]));

        let target = dir.join(format!("blocked.{fmt}"));
        let result = report_project(&audit(), fmt, &target, &root);
        assert_eq!(result["ok"], json!(true), "{fmt}: {result}");
        match fmt {
            "md" => {
                let text = std::fs::read_to_string(&target).expect("md");
                let child_row = text
                    .lines()
                    .find(|line| line.contains("Child review"))
                    .expect("child row");
                assert!(!text.contains("Child\nreview"));
                assert!(child_row.contains("False"));
                assert!(child_row.contains("[\"custom:parent\"]"));
            }
            "csv" => {
                let rows = csv_rows(&target.with_extension("coverage.csv"));
                let child_row = rows
                    .iter()
                    .find(|row| row["Item ID"] == json!("custom:child"))
                    .expect("child row");
                assert_eq!(child_row["Complete"], json!("False"));
                assert_eq!(child_row["Blocked by"], json!("[\"custom:parent\"]"));
            }
            "xlsx" => {
                let rows = xlsx_rows(&target, "Project Coverage");
                let values = rows
                    .iter()
                    .find(|row| row.first().and_then(Data::get_string) == Some("custom:child"))
                    .expect("child row");
                let header = rows
                    .iter()
                    .find(|row| row.first().and_then(Data::get_string) == Some("Item ID"))
                    .expect("header");
                let child_row: BTreeMap<String, &Data> = header
                    .iter()
                    .filter_map(|h| h.get_string().map(str::to_owned))
                    .zip(values.iter())
                    .collect();
                assert_eq!(child_row["Complete"].get_bool(), Some(false));
                assert_eq!(
                    child_row["Blocked by"].get_string(),
                    Some("[\"custom:parent\"]")
                );
            }
            _ => {
                let text = docx_text(&target);
                assert!(text.contains("Complete: False"));
                assert!(text.contains("Blocked by: [\"custom:parent\"]"));
            }
        }
    }
}

#[test]
fn project_report_never_replaces_controls_or_referenced_artifact_sidecars() {
    let dir = tmp("protected");
    let root = project(&dir);
    // `protected_destination` is the boundary `build_report(project=...)`
    // consults before writing; the wrapper itself is the declared gap.
    for target in [root.join("project.json"), root.join("coverage.json")] {
        let error = protected_destination(&root, &targets_for("md", &target));
        assert_eq!(
            error.as_deref(),
            Some("report output must not overwrite project control files or referenced evidence"),
            "{target:?}"
        );
    }
    let evidence = root.join("reports").join("protected.scope.csv");
    std::fs::create_dir_all(evidence.parent().expect("parent")).expect("mkdir");
    std::fs::write(&evidence, "reviewed").expect("artifact");
    update_item(&root, &json!({"id": "custom:protected"}), revision(&root)).expect("item");
    record_execution(
        &root,
        "custom:protected",
        &json!({
            "status": "succeeded",
            "reason": "Reviewed",
            "artifact": "reports/protected.scope.csv",
            "reviewer": "Specialist",
            "review": "approved",
        }),
        revision(&root),
    )
    .expect("record");
    let target = root.join("reports").join("protected.csv");
    let error = protected_destination(&root, &targets_for("csv", &target));
    assert_eq!(
        error.as_deref(),
        Some("report output must not overwrite project control files or referenced evidence")
    );
    // The referenced artifact itself is in the protected set.
    assert!(
        protected_project_paths(&root).contains(&std::fs::canonicalize(&evidence).expect("canon"))
    );
}
