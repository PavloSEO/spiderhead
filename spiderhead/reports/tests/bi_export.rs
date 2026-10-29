//! Ported BI-export tests from the reference suite — the cases whose
//! only dependencies are the reports crate.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

use serde_json::{json, Map, Value};
use spiderhead_reports::bi::cohorts::{cohort_rows, quadrant_candidates};
use spiderhead_reports::bi::project::{export_bi, ExportLimits, Observation};
use spiderhead_reports::bi::schema::{Groups, ProviderInput, RunInput};
use spiderhead_reports::bi_index::{projection_index, PrimarySegmentIndex, QuadrantHit};
use spiderhead_reports::evidence_import::normalize_inline;
use spiderhead_reports::segments::assign_segments;

fn tmp_dir(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("bi-export-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

fn csv_rows(package: &Path, dataset: &str) -> Vec<BTreeMap<String, String>> {
    let manifest: Value = serde_json::from_str(
        &std::fs::read_to_string(package.join("manifest.json")).expect("manifest"),
    )
    .expect("manifest json");
    let mut rows = Vec::new();
    for partition in manifest["datasets"][dataset]["partitions"]
        .as_array()
        .cloned()
        .unwrap_or_default()
    {
        let path = package.join(partition["path"].as_str().unwrap());
        let mut reader = csv::Reader::from_path(&path).expect("csv");
        for row in reader.deserialize() {
            rows.push(row.expect("csv row"));
        }
    }
    rows
}

fn site_audit(extra_pages: &[Value]) -> Value {
    let mut pages = vec![json!({
        "url": "https://example.test/",
        "status_code": 200,
        "title": "=SUM(A1:A2)",
        "description": "Synthetic description",
        "h1": "Heading",
        "word_count": 0,
    })];
    pages.extend(extra_pages.iter().cloned());
    json!({
        "schema": "spiderhead.site-audit/1",
        "url": "https://example.test/",
        "domain": "example.test",
        "generated_at": "2026-01-02T03:04:05Z",
        "site": {},
        "pages": pages,
        "findings": [{
            "source": "synthetic",
            "severity": "notice",
            "url": "https://example.test/",
            "text": "=formula finding",
        }],
        "summary": {
            "pages_checked": pages.len(),
            "findings_total": 1,
            "findings_by_severity": {"critical": 0, "warning": 0, "notice": 1},
            "tools_run": [],
            "tools_failed": [],
        },
    })
}

fn evidence_doc(provider: &str, metric: &str, value: &Value, timezone: &str) -> Value {
    evidence_doc_url("https://example.test/", provider, metric, value, timezone)
}

fn evidence_doc_url(
    url: &str,
    provider: &str,
    metric: &str,
    value: &Value,
    timezone: &str,
) -> Value {
    normalize_inline(
        &json!([{ "url": url, metric: value }]),
        &json!({
            "format": "spiderhead.evidence-mapping.v1",
            "source": {
                "provider": provider,
                "operation": "synthetic",
                "privacy": "supplied",
                "timezone": timezone,
                "reporting_identity": format!("synthetic-{provider}"),
                "attribution": "last-click",
            },
            "url": {"field": "url", "kind": "absolute"},
            "row_shape": "flat",
            "dimensions": [],
            "metrics": [{"name": metric, "type": "number", "unit": "count"}],
            "period": {"start_date": "2026-01-01", "end_date": "2026-01-07"},
            "collection": {
                "state": "complete",
                "sampled": false,
                "thresholded": false,
                "truncated": false,
            },
        }),
    )
    .expect("normalize_inline")
}

fn write_json(path: &Path, doc: &Value) -> String {
    std::fs::write(path, serde_json::to_string(doc).unwrap()).unwrap();
    path.to_string_lossy().into_owned()
}

#[test]
fn million_page_projection_factories_are_lazy_and_reiterable() {
    let produced = Arc::new(AtomicUsize::new(0));
    let counter = produced.clone();
    let mut run = RunInput {
        run_id: "large-synthetic".into(),
        source_kind: "scan".into(),
        source_schema: "scan.v1".into(),
        source_name: Some("large.sqlite".into()),
        source_sha256: "a".repeat(64),
        source_bytes: 1,
        run_metadata: {
            let mut m = Map::new();
            m.insert("crawl_state".into(), json!("complete"));
            m.insert("crawl_reason".into(), Value::Null);
            m.insert(
                "capabilities".into(),
                json!({"links": {"state": "complete"}}),
            );
            m
        },
        pages_factory: Box::new(move || {
            let counter = counter.clone();
            Box::new((0..1_000_000usize).map(move |ordinal| {
                counter.fetch_add(1, Ordering::SeqCst);
                json!({
                    "url": format!("https://example.test/{ordinal}"),
                    "page_ordinal": ordinal,
                    "status_code": 200,
                    "content_type": "text/html",
                    "size_bytes": 1,
                    "response_time": 0.01,
                    "title": "Page",
                    "meta_description": "",
                    "h1": "Page",
                    "canonical": "",
                    "word_count": 1,
                    "crawl_depth": 1,
                    "inlinks": 0,
                    "unique_inlinks": 0,
                    "outlinks": 0,
                    "external_outlinks": 0,
                    "representation": "static",
                    "document_id": 1,
                })
            }))
        }),
        page_count: 1_000_000,
        findings_factory: Box::new(|| Box::new(std::iter::empty())),
        finding_count: 0,
        groups: Groups::List(vec![]),
        links_factory: Box::new(|| Box::new(std::iter::empty())),
        links_source_state: "complete".into(),
        links_source_reason: None,
        coverage_rows: vec![],
        link_count: Some(0),
        audit_sha256: None,
    };
    let first = (run.pages_factory)().next().unwrap();
    assert_eq!(first["url"], json!("https://example.test/0"));
    assert_eq!(produced.load(Ordering::SeqCst), 1);
    let first_cohorts: Vec<Value> = cohort_rows(&mut run, &[], None, None, None)
        .take(5)
        .collect::<Result<_, _>>()
        .expect("cohorts");
    assert_eq!(first_cohorts.len(), 5);
    assert_eq!(produced.load(Ordering::SeqCst), 2);
    assert_eq!(
        (run.pages_factory)().count(),
        1_000_000,
        "the full source remains re-iterable"
    );
    assert_eq!(produced.load(Ordering::SeqCst), 1_000_002);
}

#[test]
fn segment_projection_refuses_absent_fields_but_keeps_explicit_null() {
    let dir = tmp_dir("segments");
    let definitions = json!([{
        "name": "has-title",
        "rules": [{"op": "eq", "field": "title", "value": "Home"}],
    }]);
    let assignment = assign_segments(
        &[json!({"url": "https://example.test/", "title": "Home"})],
        definitions.as_array().unwrap(),
    )
    .expect("assign_segments");
    assert_eq!(
        assignment["primary"]["https://example.test/"],
        json!("has-title")
    );
    {
        let index = projection_index(&dir, 1024 * 1024).expect("index");
        let primary = PrimarySegmentIndex::new(&index.con, definitions.as_array().unwrap())
            .expect("primary index");
        primary
            .build(vec![json!({"url": "https://example.test/"})])
            .expect("build");
        let err = primary.primary("https://example.test/").unwrap_err();
        assert!(
            err.0.contains("retained page fields are missing: title"),
            "unexpected error: {}",
            err.0
        );
    }
    {
        let index = projection_index(&dir, 1024 * 1024).expect("index");
        let primary = PrimarySegmentIndex::new(&index.con, definitions.as_array().unwrap())
            .expect("primary index");
        primary
            .build(vec![
                json!({"url": "https://example.test/", "title": Value::Null}),
            ])
            .expect("build");
        assert_eq!(
            primary.primary("https://example.test/").expect("primary"),
            "default"
        );
    }
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn site_audit_nulls_and_formula_cells_are_explicitly_safe() {
    let dir = tmp_dir("site-audit");
    let package = dir.join("site-audit-bi");
    export_bi(
        None,
        Some(&site_audit(&[])),
        &[],
        Some(&package),
        ExportLimits::default(),
    )
    .expect("export_bi");
    let page = &csv_rows(&package, "pages")[0];
    assert_eq!(page["title"], "'=SUM(A1:A2)");
    assert_eq!(page["title_state"], "measured");
    assert_eq!(page["word_count"], "0");
    assert_eq!(page["word_count_state"], "measured");
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn hard_output_limit_fails_without_publishing_a_partial_package() {
    let dir = tmp_dir("hard-limit");
    let audit = site_audit(
        &(0..50)
            .map(|i| {
                json!({
                    "url": format!("https://example.test/page-{i}"),
                    "status_code": 200,
                    "title": format!("Synthetic page {i} with a reasonably long title"),
                    "description": "Synthetic description",
                    "h1": "Heading",
                    "word_count": 12,
                })
            })
            .collect::<Vec<_>>(),
    );
    let audit_path = dir.join("audit.json");
    std::fs::write(&audit_path, serde_json::to_string(&audit).unwrap()).unwrap();
    let destination = dir.join("too-small");
    let result = export_bi(
        None,
        Some(&json!(audit_path.to_string_lossy().into_owned())),
        &[],
        Some(&destination),
        ExportLimits {
            max_rows_per_file: 10,
            max_bytes_per_file: 1024,
            max_output_bytes: 1024,
            ..ExportLimits::default()
        },
    );
    let message = result.expect_err("export must fail").0;
    assert!(
        message.contains("bound")
            || message.contains("partition byte limit")
            || message.contains("row exceeds"),
        "unexpected error: {message}"
    );
    assert!(!destination.exists(), "no partial package may be published");
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn cohorts_keep_zero_quadrants_separate_from_unconfigured_provider_evidence() {
    let dir = tmp_dir("cohorts-zero");
    let audit = {
        let mut doc = site_audit(&[]);
        let pages = doc["pages"].as_array_mut().unwrap();
        pages[0]
            .as_object_mut()
            .unwrap()
            .insert("crawl_depth".to_string(), json!(3));
        doc
    };
    let gsc = write_json(
        &dir.join("gsc.json"),
        &evidence_doc("gsc", "clicks", &json!(0), "UTC"),
    );
    let ga4 = write_json(
        &dir.join("ga4.json"),
        &evidence_doc("ga4", "sessions", &json!(2), "UTC"),
    );
    let package = dir.join("cohort-bi");
    export_bi(
        None,
        Some(&audit),
        &[gsc.clone(), ga4.clone()],
        Some(&package),
        ExportLimits {
            search_metric: Some("clicks".into()),
            ..ExportLimits::default()
        },
    )
    .expect("export_bi");
    let manifest: Value =
        serde_json::from_str(&std::fs::read_to_string(package.join("manifest.json")).unwrap())
            .unwrap();
    let cohorts = csv_rows(&package, "cohorts");
    let quadrant = cohorts
        .iter()
        .find(|row| row["cohort_id"] == "search_visibility_vs_sessions")
        .expect("quadrant");
    assert_eq!(quadrant["membership"], "member");
    assert_eq!(quadrant["value_label"], "zero_search_positive_sessions");
    assert_eq!(quadrant["search_value"], "0");
    assert_eq!(quadrant["sessions_value"], "2");
    assert_eq!(quadrant["period_start"], "2026-01-01");
    assert_eq!(quadrant["timezone"], "UTC");
    assert_eq!(manifest["datasets"]["cohorts"]["row_count"], json!(5));

    // no providers: the quadrant reports not_configured, inlink share unavailable
    let no_provider = dir.join("cohort-no-provider");
    export_bi(
        None,
        Some(&audit),
        &[],
        Some(&no_provider),
        ExportLimits::default(),
    )
    .expect("export_bi no providers");
    let cohorts = csv_rows(&no_provider, "cohorts");
    let missing = cohorts
        .iter()
        .find(|row| row["cohort_id"] == "search_visibility_vs_sessions")
        .expect("quadrant");
    assert_eq!(missing["membership"], "unclassified");
    assert_eq!(missing["state"], "not_configured");
    let inlinks = cohorts
        .iter()
        .find(|row| row["cohort_id"] == "observed_unique_inlink_share")
        .expect("inlinks");
    assert_eq!(inlinks["state"], "unavailable");

    // timezone-mismatched evidence pairs are incomplete, not quadrants
    let mismatched = write_json(
        &dir.join("ga4-mismatched.json"),
        &evidence_doc("ga4", "sessions", &json!(2), "America/New_York"),
    );
    let incompatible = dir.join("cohort-incompatible");
    export_bi(
        None,
        Some(&audit),
        &[gsc, mismatched],
        Some(&incompatible),
        ExportLimits {
            search_metric: Some("clicks".into()),
            ..ExportLimits::default()
        },
    )
    .expect("export_bi incompatible");
    let cohorts = csv_rows(&incompatible, "cohorts");
    let row = cohorts
        .iter()
        .find(|row| row["cohort_id"] == "search_visibility_vs_sessions")
        .expect("quadrant");
    assert_eq!(row["membership"], "unclassified");
    assert_eq!(row["state"], "incomplete");
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn cohort_quadrants_refuse_normalized_url_key_collisions() {
    let dir = tmp_dir("cohort-collision");
    let audit = {
        let mut doc = site_audit(&[]);
        let pages = doc["pages"].as_array_mut().unwrap();
        pages[0]
            .as_object_mut()
            .unwrap()
            .insert("url".to_string(), json!("https://example.test/a"));
        pages.push(json!({"url": "https://EXAMPLE.test/a", "status_code": 200}));
        doc["summary"]["pages_checked"] = json!(2);
        doc
    };
    let gsc = write_json(
        &dir.join("gsc.json"),
        &evidence_doc_url("https://example.test/a", "gsc", "clicks", &json!(0), "UTC"),
    );
    let ga4 = write_json(
        &dir.join("ga4.json"),
        &evidence_doc_url(
            "https://example.test/a",
            "ga4",
            "sessions",
            &json!(2),
            "UTC",
        ),
    );
    let package = dir.join("collision-bi");
    export_bi(
        None,
        Some(&audit),
        &[gsc, ga4],
        Some(&package),
        ExportLimits {
            search_metric: Some("clicks".into()),
            ..ExportLimits::default()
        },
    )
    .expect("export_bi");
    let cohorts = csv_rows(&package, "cohorts");
    let quadrants: Vec<_> = cohorts
        .iter()
        .filter(|row| row["cohort_id"] == "search_visibility_vs_sessions")
        .collect();
    assert_eq!(quadrants.len(), 2);
    for row in quadrants {
        assert_eq!(row["membership"], "unclassified");
        assert_eq!(row["state"], "incomplete");
        assert!(
            row["reason"].contains("matches multiple retained crawl URLs"),
            "unexpected reason: {}",
            row["reason"]
        );
        assert_eq!(row["value_label"], "");
    }
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn quadrant_pairing_refuses_cursor_backed_multi_page_match() {
    let dir = tmp_dir("quadrant-cursor");
    let observation = |metric: &str, value: &Value| Observation {
        identity: (0, String::new()),
        row: json!({"dimensions": {}}),
        metric: json!({"name": metric, "unit": "count"}),
        entry: json!({"state": "measured", "value": value}),
        dimensions: json!({}),
        population: "matched".to_string(),
        matched_urls: vec![],
        matched_page_count: 2,
        matched_url_provenance: None,
        url: json!({"state": "keyed", "normalized": "https://example.test/a"}),
    };
    let provider = |name: &str| ProviderInput {
        source_id: name.to_string(),
        name: None,
        sha256: String::new(),
        byte_count: 0,
        join: None,
        compatibility: None,
        normalized: None,
        error_state: None,
        error_reason: None,
        reported_rows: None,
    };
    let gsc = provider("gsc");
    let ga4 = provider("ga4");
    let gsc_obs = vec![observation("clicks", &json!(0))];
    let ga4_obs = vec![observation("sessions", &json!(2))];
    let gsc_info = json!({
        "evidence": {
            "provider": "gsc",
            "collection": {"state": "complete"},
            "period": {"start_date": "2026-01-01", "end_date": "2026-01-07"},
            "timezone": "UTC",
        }
    });
    let ga4_info = json!({
        "evidence": {
            "provider": "ga4",
            "collection": {"state": "complete"},
            "period": {"start_date": "2026-01-01", "end_date": "2026-01-07"},
            "timezone": "UTC",
        }
    });
    let sources: Vec<(&ProviderInput, &Vec<Observation>, &Value)> =
        vec![(&gsc, &gsc_obs, &gsc_info), (&ga4, &ga4_obs, &ga4_info)];
    let index = projection_index(&dir, 1024 * 1024).expect("index");
    let (pairs, reason, blocked) =
        quadrant_candidates(&sources, Some("clicks"), &index.con).expect("quadrant_candidates");
    assert!(matches!(
        pairs.get("https://example.test/a").expect("get"),
        QuadrantHit::Miss
    ));
    assert_eq!(reason, None);
    match blocked.get("https://example.test/a").expect("blocked") {
        QuadrantHit::Reason(text) => assert_eq!(
            text,
            "normalized URL key matches multiple retained crawl URLs; \
             provider traffic cannot be attributed to one URL observation"
        ),
        QuadrantHit::Miss => panic!("expected refusal reason, got Miss"),
        QuadrantHit::Pair(_, _) => panic!("expected refusal reason, got Pair"),
    }
    let _ = std::fs::remove_dir_all(&dir);
}

fn provider_manifest() -> Value {
    json!({
        "format": "spiderhead.evidence-mapping.v1",
        "source": {
            "provider": "gsc",
            "operation": "search_analytics",
            "reporting_identity": "synthetic-property",
            "privacy": "supplied",
            "timezone": "America/Los_Angeles",
            "attribution": "last-click",
            "search_engine": "google",
            "search_type": "web",
        },
        "url": {"field": "url", "kind": "absolute"},
        "row_shape": "flat",
        "dimensions": ["query"],
        "metrics": [
            {"name": "clicks", "type": "number", "unit": "count"},
            {"name": "impressions", "type": "number", "unit": "count"},
        ],
        "period": {"start_date": "2026-01-01", "end_date": "2026-01-07"},
        "collection": {"state": "partial", "reason": "synthetic sampled fixture", "sampled": true},
        "duplicate_policy": "mark",
    })
}

fn provider_document() -> Value {
    normalize_inline(
        &json!([
            {"url": "https://example.test/", "query": "=formula query", "clicks": 0, "impressions": 100},
            {"url": "https://example.test/", "query": "=formula query", "clicks": 3, "impressions": 120},
            {"url": "https://outside.example.test/page", "query": "external only", "clicks": 4, "impressions": null},
            {"url": "/relative", "query": "unkeyable", "clicks": 2, "impressions": 3},
        ]),
        &provider_manifest(),
    )
    .expect("normalize_inline")
}

#[test]
fn legacy_untyped_provider_join_is_counted_as_unsupported() {
    let dir = tmp_dir("legacy-join");
    let audit_path = dir.join("audit.json");
    std::fs::write(
        &audit_path,
        serde_json::to_string(&site_audit(&[])).unwrap(),
    )
    .unwrap();
    let legacy = dir.join("legacy-provider-join.json");
    std::fs::write(
        &legacy,
        serde_json::to_string(&json!({
            "format": "spiderhead.provider-join.v1",
            "join": {"summary": {"rows": 7}},
        }))
        .unwrap(),
    )
    .unwrap();
    let package = dir.join("legacy-bi");
    export_bi(
        None,
        Some(&json!(audit_path.to_string_lossy().into_owned())),
        &[legacy.to_string_lossy().into_owned()],
        Some(&package),
        ExportLimits::default(),
    )
    .expect("export_bi");
    let manifest: Value =
        serde_json::from_str(&std::fs::read_to_string(package.join("manifest.json")).unwrap())
            .unwrap();
    let metric_sources: Vec<_> = csv_rows(&package, "coverage")
        .into_iter()
        .filter(|row| row["dataset"] == "metrics" && row["population"] == "provider_source")
        .collect();
    assert_eq!(metric_sources.len(), 1);
    assert_eq!(metric_sources[0]["state"], "unsupported");
    assert_eq!(metric_sources[0]["source_rows"], "7");
    assert!(metric_sources[0]["reason"].contains("does not declare typed metric names"));
    assert_eq!(
        manifest["datasets"]["metrics"]["state"],
        json!("unavailable")
    );
    assert_eq!(manifest["datasets"]["metrics"]["row_count"], json!(0));
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn saved_issue781_join_artifact_projects_without_rejoining_or_summing() {
    let dir = tmp_dir("saved-join");
    let doc = provider_document();
    let audit = json!({
        "schema": "spiderhead.site-audit/1",
        "url": "https://example.test/",
        "domain": "example.test",
        "generated_at": "2026-01-02T03:04:05Z",
        "site": {},
        "pages": [{"url": "https://example.test/", "title": "Synthetic"}],
        "findings": [],
        "summary": {
            "pages_checked": 1,
            "findings_total": 0,
            "findings_by_severity": {"critical": 0, "warning": 0, "notice": 0},
            "tools_run": [],
            "tools_failed": [],
        },
    });
    let join = spiderhead_reports::evidence_join::join_evidence(
        audit["pages"].as_array().unwrap(),
        &doc,
        None,
        Some(&json!({"source": "audit"})),
    )
    .expect("join_evidence");
    let compatibility =
        spiderhead_reports::bi::compatibility::evidence_compatibility(&doc, &doc, &Value::Null)
            .expect("evidence_compatibility");
    let artifact = dir.join("saved-evidence-join.json");
    std::fs::write(
        &artifact,
        serde_json::to_string(&json!({"join": join, "compatibility": compatibility})).unwrap(),
    )
    .unwrap();
    let package = dir.join("saved-join-bi");
    export_bi(
        None,
        Some(&audit),
        &[artifact.to_string_lossy().into_owned()],
        Some(&package),
        ExportLimits::default(),
    )
    .expect("export_bi");
    let manifest: Value =
        serde_json::from_str(&std::fs::read_to_string(package.join("manifest.json")).unwrap())
            .unwrap();
    let metrics = csv_rows(&package, "metrics");
    let populations: Vec<&str> = metrics
        .iter()
        .map(|row| row["population_state"].as_str())
        .collect();
    assert_eq!(metrics.len(), 8);
    assert_eq!(populations.iter().filter(|p| **p == "matched").count(), 4);
    assert_eq!(
        populations
            .iter()
            .filter(|p| **p == "external_only")
            .count(),
        2
    );
    assert_eq!(populations.iter().filter(|p| **p == "unkeyable").count(), 2);
    assert_eq!(
        manifest["provider_sources"][0]["compatibility"]["verdict"],
        json!("compatible")
    );
    let ids: std::collections::BTreeSet<&str> = metrics
        .iter()
        .map(|row| row["metric_observation_id"].as_str())
        .collect();
    assert_eq!(ids.len(), metrics.len());
    let _ = std::fs::remove_dir_all(&dir);
}

// --- the reference suite -----------------------------

fn base_document() -> Value {
    json!({
        "schema": "spiderhead.site-audit/1",
        "domain": "example.com",
        "url": "https://example.com/",
        "generated_at": "2026-09-05T00:00:00Z",
        "findings": [],
        "pages": [],
        "summary": {"pages_checked": 0, "findings_total": 0, "findings_by_severity": {}},
    })
}

fn build_tech_xlsx(dir: &Path, technologies: &Value) -> PathBuf {
    let mut doc = base_document();
    doc.as_object_mut().unwrap().insert(
        "site".into(),
        json!({"site": {}, "tech_detect": {"technologies": technologies}}),
    );
    let out = dir.join("tech.xlsx");
    let result = spiderhead_reports::build_report::build_report(
        &doc,
        "xlsx",
        Some(&out.to_string_lossy()),
        None,
        None,
        0,
        "en",
        None,
        None,
    );
    assert_eq!(result["ok"], json!(true), "{result}");
    out
}

fn sheet_cells(path: &Path, sheet: &str) -> Vec<Vec<String>> {
    use calamine::{Data, Reader, Xlsx};
    let mut wb: Xlsx<std::io::BufReader<std::fs::File>> =
        calamine::open_workbook(path).expect("open xlsx");
    let range = wb.worksheet_range(sheet).expect("sheet");
    range
        .rows()
        .map(|row| {
            row.iter()
                .map(|cell| match cell {
                    Data::String(s) => s.to_string(),
                    Data::Empty => String::new(),
                    other => format!("{other:?}"),
                })
                .collect()
        })
        .collect()
}

#[test]
fn formula_leading_tech_name_is_neutralised() {
    let dir = tmp_dir("xlsx-formula");
    let out = build_tech_xlsx(
        &dir,
        &json!([{
            "category": "runtime",
            "name": "=cmd|' /C calc'!A1",
            "evidence": "x-powered-by: =cmd|' /C calc'!A1",
        }]),
    );
    let cells = sheet_cells(&out, "Technologies");
    let name_cell = &cells[1][1];
    let evidence_cell = &cells[1][2];
    assert_eq!(name_cell.trim_start_matches('\''), "=cmd|' /C calc'!A1");
    assert_eq!(
        evidence_cell.trim_start_matches('\''),
        "x-powered-by: =cmd|' /C calc'!A1"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn ordinary_tech_name_is_unchanged() {
    let dir = tmp_dir("xlsx-ordinary");
    let out = build_tech_xlsx(
        &dir,
        &json!([{
            "category": "framework",
            "name": "Next.js",
            "evidence": "detected via build id",
        }]),
    );
    let cells = sheet_cells(&out, "Technologies");
    assert_eq!(cells[1][1], "Next.js");
    assert_eq!(cells[1][2], "detected via build id");
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn summary_metadata_formula_cells_are_text() {
    let dir = tmp_dir("xlsx-metadata");
    let mut doc = base_document();
    doc["url"] = json!("=1+1");
    let summary = doc["summary"].as_object_mut().unwrap();
    summary.insert(
        "tools_failed".into(),
        json!([{"tool": "TITLE_MISSING", "error": "=2+2"}]),
    );
    summary.insert("severity_note".into(), json!("=3+3"));
    let target = dir.join("metadata.xlsx");
    let result = spiderhead_reports::build_report::build_report(
        &doc,
        "xlsx",
        Some(&target.to_string_lossy()),
        None,
        None,
        0,
        "en",
        None,
        None,
    );
    assert_eq!(result["ok"], json!(true), "{result}");
    let cells = sheet_cells(&target, "Summary");
    let flat: std::collections::BTreeSet<&str> = cells
        .iter()
        .flat_map(|row| row.iter().map(|c| c.as_str()))
        .collect();
    for payload in ["'=1+1", "'=2+2", "'=3+3"] {
        assert!(flat.contains(payload), "missing {payload}");
    }
    let _ = std::fs::remove_dir_all(&dir);
}
