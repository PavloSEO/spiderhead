//! Ported tests for `reports/bi` bounded delivery (reference
//! the reference suite): selected export, index recovery and
//! large-projection boundaries.
//!
//! Not portable: the `low_disk`/`interrupt` filter-failure params (reference
//! patches `shutil.disk_usage` and `PartitionWriter.write_cells`) and
//! `test_xlsx_index_publication_failure_rolls_back_new_workbook` (patches
//! `os.replace`) — no equivalent Rust seam; Rust checks the real filesystem
//! and publishes through `fs::rename`. `test_quadrants_stream...` and
//! `test_inlink_share...` are ported via the public `quadrant_candidates` /
//! `InlinkIndex` APIs.

use std::fs;
use std::path::PathBuf;

use serde_json::{json, Map, Value};
use spiderhead_reports::bi::cohorts::quadrant_candidates;
use spiderhead_reports::bi::planners::{export_bi_xlsx, filter_package};
use spiderhead_reports::bi::project::{export_bi, ExportLimits, Observation};
use spiderhead_reports::bi::schema::{Groups, ProviderInput, RunInput};
use spiderhead_reports::bi_index::{projection_index, GroupIndex, InlinkIndex, QuadrantHit};

fn tmp(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("bi-bounded-{tag}-{}", std::process::id()));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).unwrap();
    dir
}

fn sha256(data: &[u8]) -> String {
    use sha2::Digest;
    format!("{:x}", sha2::Sha256::digest(data))
}

/// `package(tmp_path, title=...)`: the 4-page audit package.
fn package(tag: &str, title: &str) -> PathBuf {
    let audit = json!({
        "schema": "spiderhead.site-audit/1",
        "url": "https://example.test/",
        "site": {},
        "pages": (0..4).map(|i| json!({
            "url": format!("https://example.test/{i}"),
            "status_code": 200,
            "title": title,
        })).collect::<Vec<_>>(),
        "findings": [],
        "summary": {
            "pages_checked": 4,
            "findings_total": 0,
            "tools_run": [],
            "tools_failed": [{"tool": "render", "error": "unavailable"}],
        },
    });
    let root = tmp(tag).join("package");
    export_bi(
        None,
        Some(&audit),
        &[],
        Some(&root),
        ExportLimits::default(),
    )
    .expect("export_bi");
    root
}

#[test]
// (drives `filter_package` directly; `bi_filter` is the mcp handler wrapper)
fn filter_streams_hashes_and_keeps_source_coverage_and_omitted_population() {
    let root = package("filter", "=SUM(1,2)");
    let out = tmp("filter-out").join("selected");
    let where_ = Map::from_iter([(
        "url".to_owned(),
        json!(["https://example.test/0", "https://example.test/3"]),
    )]);
    let result = filter_package(
        &root,
        "pages",
        &out,
        Some(&where_),
        Some(&["url".to_owned(), "title".to_owned()]),
        1,
        1 << 20,
        1 << 24,
    )
    .expect("filter");
    assert_eq!(
        result["conservation"],
        json!({
            "source_rows": 4,
            "selected_rows": 2,
            "omitted_rows": 2,
            "source_states": {"not_declared": 4},
            "selected_states": {"not_declared": 2},
            "omission_reason": "explicit exact-filter selection; not evidence of resolution",
        })
    );
    assert_eq!(result["source"]["crawl_completeness"]["state"], "unknown");
    assert_eq!(result["source_coverage"]["coverage"]["source_rows"], 4);
    let part_rows: Vec<i64> = result["partitions"]
        .as_array()
        .unwrap()
        .iter()
        .map(|p| p["rows"].as_i64().unwrap())
        .collect();
    assert_eq!(part_rows, vec![1, 1]);
    let mut rows = Vec::new();
    for part in result["partitions"].as_array().unwrap() {
        let path = out.join(part["path"].as_str().unwrap());
        assert_eq!(sha256(&fs::read(&path).unwrap()), part["sha256"]);
        rows.extend(
            csv::Reader::from_path(&path)
                .unwrap()
                .deserialize::<Map<String, Value>>()
                .map(|r| r.unwrap()),
        );
    }
    let titles: Vec<&str> = rows.iter().map(|r| r["title"].as_str().unwrap()).collect();
    assert_eq!(titles, vec!["'=SUM(1,2)", "'=SUM(1,2)"]);
}

#[test]
// (`bytes` param only; `low_disk` patches shutil.disk_usage and `interrupt`
// patches PartitionWriter.write_cells — no Rust seam)
fn filter_failure_never_publishes_partial_output_or_changes_source() {
    let root = package("fail", "t");
    let original: Map<String, Value> = fs::read_dir(&root)
        .unwrap()
        .map(|entry| {
            let path = entry.unwrap().path();
            (
                path.file_name().unwrap().to_string_lossy().into_owned(),
                json!(fs::read(&path).unwrap()),
            )
        })
        .collect();
    let out = tmp("fail-out").join("selected");
    assert!(filter_package(&root, "pages", &out, None, None, 10_000, 1024, 1024).is_err());
    assert!(!out.exists());
    // No staging dirs are left beside the refused output path.
    let leftovers: Vec<_> = fs::read_dir(tmp("fail-out"))
        .unwrap()
        .filter_map(|e| e.ok())
        .filter(|e| {
            e.file_name()
                .to_string_lossy()
                .starts_with(".seohead-bi-filter-")
        })
        .collect();
    assert!(leftovers.is_empty(), "staging dirs left: {leftovers:?}");
    let current: Map<String, Value> = fs::read_dir(&root)
        .unwrap()
        .map(|entry| {
            let path = entry.unwrap().path();
            (
                path.file_name().unwrap().to_string_lossy().into_owned(),
                json!(fs::read(&path).unwrap()),
            )
        })
        .collect();
    assert_eq!(original, current);
}

#[test]
fn xlsx_index_maps_every_selected_row_and_is_the_completion_marker() {
    let root = package("index", "t");
    let selected = tmp("index-out").join("selected");
    let projection = filter_package(
        &root,
        "pages",
        &selected,
        None,
        Some(&["url".to_owned(), "title".to_owned()]),
        3,
        1 << 20,
        1 << 24,
    )
    .expect("filter");
    let out = tmp("index-xlsx").join("pages.xlsx");
    let result = export_bi_xlsx(&selected, "pages", &out, 2, 1 << 24).expect("xlsx");
    let index: Value =
        serde_json::from_str(&fs::read_to_string(result["index"].as_str().unwrap()).unwrap())
            .unwrap();
    assert_eq!(index["state"], "complete");
    assert_eq!(index["conservation"], projection["conservation"]);
    assert_eq!(index["rows"], 4);
    let ranges: Vec<(String, i64, i64, String, i64, i64)> = index["ranges"]
        .as_array()
        .unwrap()
        .iter()
        .map(|r| {
            (
                r["partition"].as_str().unwrap().to_owned(),
                r["source_row_start"].as_i64().unwrap(),
                r["source_row_end"].as_i64().unwrap(),
                r["worksheet"].as_str().unwrap().to_owned(),
                r["worksheet_row_start"].as_i64().unwrap(),
                r["worksheet_row_end"].as_i64().unwrap(),
            )
        })
        .collect();
    assert_eq!(
        ranges,
        vec![
            (
                "pages-0001.csv".to_owned(),
                1,
                2,
                "pages-0001".to_owned(),
                2,
                3
            ),
            (
                "pages-0001.csv".to_owned(),
                3,
                3,
                "pages-0002".to_owned(),
                2,
                2
            ),
            (
                "pages-0002.csv".to_owned(),
                1,
                1,
                "pages-0002".to_owned(),
                3,
                3
            ),
        ]
    );
    assert_eq!(
        sha256(&fs::read(result["output"].as_str().unwrap()).unwrap()),
        index["workbook_sha256"].as_str().unwrap()
    );
}

#[test]
fn xlsx_refuses_cell_truncation_and_retains_complete_csv() {
    let root = package("truncate", &"x".repeat(32_768));
    let out = tmp("truncate-out").join("pages.xlsx");
    let err = export_bi_xlsx(&root, "pages", &out, 1_000_000, 1 << 24).unwrap_err();
    assert!(err.to_string().contains("32767-character"));
    assert!(root.join("manifest.json").exists());
    assert!(!out.exists());
    assert!(!out.with_extension("xlsx.index.json").exists());
    assert!(!out.parent().unwrap().join("pages.xlsx.index.json").exists());
}

#[test]
fn group_index_uses_disk_bound_and_cleans_up_after_failure() {
    let dir = tmp("group-bound");
    let index = projection_index(&dir, 32 * 1024).expect("projection index");
    let groups: Vec<Value> = (0..100)
        .map(|i| {
            json!({
                "id": i.to_string(),
                "value": "x".repeat(8192),
                "count": 100,
                "urls": (0..100)
                    .map(|member| format!("https://example.test/{i}/{member}"))
                    .collect::<Vec<_>>(),
            })
        })
        .collect();
    let mut groups = Groups::List(groups);
    let err = match GroupIndex::new(&index.con, &mut groups) {
        Ok(_) => panic!("group index must refuse beyond the disk bound"),
        Err(e) => e,
    };
    assert!(
        err.to_string().contains("disk bound") || err.to_string().contains("database or disk"),
        "{err}"
    );
    drop(index);
    let staging: Vec<_> = fs::read_dir(&dir)
        .unwrap()
        .filter_map(|e| e.ok())
        .filter(|e| {
            e.file_name()
                .to_string_lossy()
                .starts_with(".seohead-bi-index-")
        })
        .collect();
    assert!(staging.is_empty());
}

/// `observations(metric, count)`: repeated measured rows keyed to one URL.
fn observations(metric: &str, count: usize) -> Vec<Observation> {
    (0..count)
        .map(|i| Observation {
            identity: (i as i64, String::new()),
            row: json!({"dimensions": {}, "row_index": i}),
            metric: json!({"name": metric, "unit": "count"}),
            entry: json!({"state": "measured", "value": 0}),
            dimensions: json!({}),
            population: "matched".to_owned(),
            matched_urls: vec!["https://example.test/a".to_owned()],
            matched_page_count: 1,
            matched_url_provenance: None,
            url: json!({"state": "keyed", "normalized": "https://example.test/a"}),
        })
        .collect()
}

fn provider(source_id: &str) -> ProviderInput {
    ProviderInput {
        source_id: source_id.to_owned(),
        name: None,
        sha256: String::new(),
        byte_count: 0,
        join: None,
        compatibility: None,
        normalized: None,
        error_state: None,
        error_reason: None,
        reported_rows: None,
    }
}

fn evidence_info(provider: &str) -> Value {
    json!({
        "evidence": {
            "provider": provider,
            "collection": {
                "state": "complete",
                "sampled": false,
                "thresholded": false,
                "truncated": false,
            },
            "field_origins": Map::from_iter(
                ["collection_state", "sampled", "thresholded", "truncated"]
                    .iter()
                    .map(|name| (name.to_string(), json!("declared")))
            ),
            "timezone": "UTC",
            "period": {"start_date": "2026-01-01", "end_date": "2026-01-07"},
        }
    })
}

#[test]
fn quadrants_stream_repeated_observations_and_keep_ambiguity() {
    let dir = tmp("quadrant");
    let index = projection_index(&dir, 1024 * 1024).expect("projection index");
    let gsc = provider("gsc");
    let ga4 = provider("ga4");
    let gsc_obs = observations("clicks", 20_000);
    let ga4_obs = observations("sessions", 1);
    let gsc_info = evidence_info("gsc");
    let ga4_info = evidence_info("ga4");
    let sources = [(&gsc, &gsc_obs, &gsc_info), (&ga4, &ga4_obs, &ga4_info)];
    let (pairs, _reason, blocked) =
        quadrant_candidates(&sources, Some("clicks"), &index.con).expect("quadrants");
    let count: i64 = index
        .con
        .query_row("SELECT COUNT(*) FROM candidates", [], |r| r.get(0))
        .unwrap();
    assert_eq!(count, 1);
    assert!(matches!(
        pairs.get("https://example.test/a").unwrap(),
        QuadrantHit::Miss
    ));
    match blocked.get("https://example.test/a").unwrap() {
        QuadrantHit::Reason(reason) => {
            assert!(reason.contains("more than one complete"), "{reason}")
        }
        _ => panic!("expected a blocked reason for the ambiguous URL"),
    }
}

#[test]
fn inlink_share_excludes_unmeasured_sources_and_counts_repeated_links_once() {
    let dir = tmp("inlink");
    let index = projection_index(&dir, 1024 * 1024).expect("projection index");
    let pages = vec![
        json!({"url": "https://example.test/a", "document_id": 1, "content_type": "text/html"}),
        json!({"url": "https://example.test/b", "document_id": 2, "content_type": "text/html"}),
        json!({"url": "https://example.test/c", "document_id": null, "content_type": "text/html"}),
    ];
    let links: Vec<Value> = ["a", "a", "c"]
        .iter()
        .map(|s| {
            json!({
                "source_url": format!("https://example.test/{s}"),
                "destination_url": "https://example.test/b",
            })
        })
        .collect();
    let mut run = RunInput {
        run_id: "r".to_owned(),
        source_kind: "site-audit".to_owned(),
        source_schema: "spiderhead.site-audit/1".to_owned(),
        source_name: None,
        source_sha256: String::new(),
        source_bytes: 0,
        run_metadata: Map::new(),
        pages_factory: Box::new(move || {
            Box::new(pages.clone().into_iter()) as Box<dyn Iterator<Item = Value> + Send>
        }),
        page_count: 3,
        findings_factory: Box::new(|| {
            Box::new(std::iter::empty()) as Box<dyn Iterator<Item = Value> + Send>
        }),
        finding_count: 0,
        groups: Groups::List(vec![]),
        links_factory: Box::new(move || {
            Box::new(links.clone().into_iter()) as Box<dyn Iterator<Item = Value> + Send>
        }),
        links_source_state: "complete".to_owned(),
        links_source_reason: None,
        coverage_rows: vec![],
        link_count: Some(3),
        audit_sha256: None,
    };
    let inlinks = InlinkIndex::new(&index.con, &mut run).expect("inlink index");
    assert_eq!(inlinks.denominator, 2);
    assert_eq!(inlinks.numerator("https://example.test/b").unwrap(), 1);
    assert_eq!(inlinks.numerator("https://example.test/c").unwrap(), 0);
}
