//!
//! SQL frontier identities and query spending match legacy queue semantics.

use std::fs;
use std::path::{Path, PathBuf};

use serde_json::{json, Map, Value};
use spiderhead_core::host::SystemHost;
use spiderhead_core::settings_data;
use spiderhead_store::native_scan::NativeScan;
use spiderhead_store::scan_lease::{NativeScanLease, PageCommit};
use spiderhead_store::scan_schema::NativeScanSchemaMixin;

/// reference `PageRecord(url="")` as `dataclasses.asdict` renders it; `url` is set per page.
const PAGE_RECORD_DEFAULTS: &str = r##"{"url":"","status_code":null,"content_type":"","size_bytes":0,"response_time":null,"redirect_url":"","title":"","meta_description":"","h1":"","h1_2":"","h2":"","canonical":"","meta_robots":"","x_robots":"","og_title":"","og_description":"","og_image":"","og_url":"","word_count":0,"text_ratio":null,"content_frames":0,"content_frames_same_origin":0,"meta_description_count":0,"h1_alt_text":"","lorem_ipsum_count":0,"images_total":0,"images_missing_alt_attr":0,"images_max_alt_length":0,"plugin_elements":0,"mobile_alternate_broken":0,"meta_fragment":"","ajax_scheme_outlinks":0,"crawl_depth":0,"content_encoding":"","render_blocking":null,"meta_refresh":"","http_refresh":"","charset":"","doctype":"","viewport":"","title_outside_head":null,"meta_description_outside_head":null,"canonical_outside_head":null,"directives_outside_head":null,"hreflang_outside_head":null,"hreflang":[],"amphtml":"","heading_outline":[],"invalid_link_targets":null,"transport_upgraded":false,"sentence_count":null,"avg_words_per_sentence":null,"flesch":null,"readability":"","link_placement":{"in_heading":[],"in_heading_total":0,"image_no_text":[],"image_no_text_total":0},"trust_signals":null,"duplicate_ids":null,"head_count":0,"body_count":0,"head_not_first":false,"invalid_head_elements":"","outlinks":0,"external_outlinks":0,"jsonld_blocks_found":0,"jsonld_blocks_parsed":0,"error":"","error_kind":"","attempts":null,"cache_status":"","body_unavailable":"","representation":"static","redirect_chain":[],"final_url":"","canonical_chain":[],"final_canonical":""}"##;

/// Scratch directory removed on drop (reference `tmp_path`).
struct Scratch(PathBuf);

impl Scratch {
    fn new(name: &str) -> Self {
        let dir = std::env::temp_dir().join(format!(
            "spiderhead-tests-47-frontier-{}-{name}",
            std::process::id()
        ));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).expect("scratch directory");
        Self(dir)
    }

    fn path(&self) -> &Path {
        &self.0
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn object(value: Value) -> Map<String, Value> {
    value.as_object().cloned().expect("JSON object")
}

/// reference `_metadata(**overrides)`: resolved defaults with no politeness delay.
fn metadata(overrides: &[(&str, Value)]) -> (Map<String, Value>, Option<String>) {
    let mut config = settings_data::defaults().clone();
    config["speed"]["min_delay_seconds"] = json!(0);
    for (dotted, value) in overrides {
        let mut parts = dotted.split('.').peekable();
        let mut node = &mut config;
        while let Some(part) = parts.next() {
            if parts.peek().is_none() {
                node[part] = value.clone();
            } else {
                node = &mut node[part];
            }
        }
    }
    let fingerprint = spiderhead_core::crawl_settings::fingerprint(&config, &SystemHost)
        .expect("config fingerprint");
    (object(config), Some(fingerprint))
}

/// reference `_metadata` plus `NativeScan.create` for one fresh scan file.
fn create(path: &Path, overrides: &[(&str, Value)]) -> NativeScan {
    let (config, fingerprint) = metadata(overrides);
    let runtime_versions = object(json!({
        "python": "test",
        "sqlite": "test",
        "httpx": "test",
        "lxml": "test",
        "beautifulsoup4": "test",
    }));
    NativeScanSchemaMixin::create(
        path,
        "https://example.test/",
        &config,
        fingerprint.as_deref(),
        "3.0.0",
        &"a".repeat(40),
        &runtime_versions,
        &[],
        None,
        &[],
        "scan.v1",
    )
    .expect("create native scan")
}

/// reference `_runtime()`.
fn runtime() -> Map<String, Value> {
    object(json!({
        "max_depth_reached": 1,
        "elapsed_seconds": 0.0,
        "circuit_timeout_streak": 0,
        "circuit_server_error_streak": 0,
        "crawl_delay_applied": null,
        "throttle": {"delay_seconds": 0.0, "concurrency": 1, "consecutive_ok": 0},
    }))
}

/// reference `seed(url, *, first=False, reason="")`.
fn seed(url: &str, first: bool, reason: &str) -> Map<String, Value> {
    object(json!({
        "requested_url": url,
        "frontier_url": url,
        "depth": 0,
        "reason": reason,
        "source": if first { "start" } else { "sitemap" },
        "reserve_query": !first,
        "seed": !first,
    }))
}

/// reference `dataclasses.asdict(PageRecord(url=url))`.
fn page_record(url: &str) -> Map<String, Value> {
    let mut record = object(serde_json::from_str(PAGE_RECORD_DEFAULTS).expect("record defaults"));
    record.insert("url".into(), json!(url));
    record
}

fn query_rows(scan: &NativeScan, sql: &str) -> Vec<(String, String)> {
    let mut stmt = scan.con().prepare(sql).expect("prepare");
    stmt.query_map([], |row| Ok((row.get(0)?, row.get(1)?)))
        .expect("query")
        .map(|row| row.expect("row"))
        .collect()
}

#[test]
fn seed_identity_keeps_request_text_and_skips_canonical_aliases() {
    let dir = Scratch::new("seed-identity");
    let path = dir.path().join("scan.sqlite");
    let scan = create(&path, &[]);
    assert_eq!(
        scan.seed_frontier(&[seed("https://example.test/", true, "")])
            .unwrap()["queued"],
        json!(1)
    );
    let result = scan
        .seed_frontier(&[
            seed("https://example.test", false, ""),
            seed("HTTPS://example.test/#anchor", false, ""),
            seed("https://example.test/a#original", false, ""),
            seed("https://example.test/a#other", false, ""),
        ])
        .unwrap();
    assert_eq!(
        Value::Object(result),
        json!({"queued": 1, "excluded": 0, "already_seen": 3})
    );
    let mut stmt = scan
        .con()
        .prepare("SELECT u.url FROM frontier f JOIN urls u USING(url_id) ORDER BY queue_ordinal")
        .unwrap();
    let urls: Vec<String> = stmt
        .query_map([], |row| row.get(0))
        .unwrap()
        .map(|row| row.unwrap())
        .collect();
    assert_eq!(
        urls,
        vec!["https://example.test/", "https://example.test/a#original"]
    );
    drop(stmt);
    drop(scan);
    let counts = NativeScanSchemaMixin::inspect(&path, None).unwrap();
    assert_eq!(counts["counts"]["frontier"], json!(2));
}

#[test]
fn rejected_seed_is_seen_without_spending_a_query_slot() {
    let dir = Scratch::new("rejected-seed");
    let path = dir.path().join("scan.sqlite");
    let scan = create(&path, &[("limits.max_query_variants_per_path", json!(1))]);
    scan.seed_frontier(&[seed("https://example.test/", true, "")])
        .unwrap();
    let rejected = seed("https://example.test/a?one", false, "outside_host");
    assert_eq!(
        scan.seed_frontier(std::slice::from_ref(&rejected)).unwrap()["excluded"],
        json!(1)
    );
    assert_eq!(
        scan.seed_frontier(std::slice::from_ref(&rejected)).unwrap()["already_seen"],
        json!(1)
    );
    assert_eq!(
        scan.seed_frontier(&[seed("https://example.test/a?two", false, "")])
            .unwrap()["queued"],
        json!(1)
    );
    let decisions: i64 = scan
        .con()
        .query_row("SELECT COUNT(*) FROM decisions", [], |row| row.get(0))
        .unwrap();
    assert_eq!(decisions, 1);
    assert_eq!(
        query_rows(&scan, "SELECT * FROM query_variants"),
        vec![("/a".to_owned(), "two".to_owned())]
    );
}

#[test]
fn candidate_query_order_preserves_empty_query_and_exact_rejection() {
    let dir = Scratch::new("candidate-order");
    let path = dir.path().join("scan.sqlite");
    let mut scan = create(&path, &[("limits.max_query_variants_per_path", json!(1))]);
    scan.seed_frontier(&[seed("https://example.test/", true, "")])
        .unwrap();
    let lease = scan.claim(1).unwrap().remove(0);
    let candidates = vec![
        object(json!({
            "path_key": "/a",
            "query_key": "",
            "requested_url": "https://example.test/a#first",
            "frontier_url": "https://example.test/a",
            "depth": 1,
        })),
        object(json!({
            "path_key": "/a",
            "query_key": "q=1",
            "requested_url": "https://example.test/a?q=1#original",
            "frontier_url": "https://example.test/a?q=1",
            "depth": 1,
        })),
    ];
    scan.commit_page(&PageCommit {
        lease: Some(lease.clone()),
        record: page_record(&lease.url),
        runtime: Some(runtime()),
        candidates,
        ..Default::default()
    })
    .unwrap();
    assert_eq!(
        query_rows(&scan, "SELECT * FROM query_variants"),
        vec![("/a".to_owned(), String::new())]
    );
    assert_eq!(
        query_rows(&scan, "SELECT url,reason FROM decisions"),
        vec![(
            "https://example.test/a?q=1#original".to_owned(),
            "query_variants_limit".to_owned()
        )]
    );
    assert_eq!(scan.claim(1).unwrap()[0].url, "https://example.test/a");
}

#[test]
fn partial_observation_state_commits_with_page() {
    let dir = Scratch::new("partial-observation");
    let path = dir.path().join("scan.sqlite");
    let mut scan = create(&path, &[]);
    scan.seed_frontier(&[seed("https://example.test/", true, "")])
        .unwrap();
    let lease = scan.claim(1).unwrap().remove(0);
    scan.commit_page(&PageCommit {
        lease: Some(lease.clone()),
        record: page_record(&lease.url),
        runtime: Some(runtime()),
        partial_reasons: vec!["link_observations_omitted".to_owned()],
        ..Default::default()
    })
    .unwrap();
    let snapshot = scan.resume_snapshot(false).unwrap();
    assert_eq!(snapshot["scan"]["crawl_partial"], json!(1));
    assert!(snapshot["scan"]["limitations_json"]
        .as_str()
        .is_some_and(|text| text.contains("link_observations_omitted")));
}

#[test]
fn rejected_non_http_seed_keeps_the_original_decision_once() {
    let dir = Scratch::new("non-http-seed");
    let path = dir.path().join("scan.sqlite");
    let scan = create(&path, &[]);
    scan.seed_frontier(&[seed("https://example.test/", true, "")])
        .unwrap();
    let first = seed("MAILTO:hello@example.test#first", false, "outside_host");
    let alias = seed("mailto:hello@example.test#again", false, "outside_host");
    assert_eq!(
        scan.seed_frontier(std::slice::from_ref(&first)).unwrap()["excluded"],
        json!(1)
    );
    assert_eq!(
        scan.seed_frontier(std::slice::from_ref(&alias)).unwrap()["already_seen"],
        json!(1)
    );
    assert_eq!(
        query_rows(&scan, "SELECT url,reason FROM decisions"),
        vec![(
            "MAILTO:hello@example.test#first".to_owned(),
            "outside_host".to_owned()
        )]
    );
    assert_eq!(scan.claim(1).unwrap()[0].url, "https://example.test/");
}
