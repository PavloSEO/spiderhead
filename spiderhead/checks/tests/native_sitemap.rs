//! Retained-sitemap and robots-contradiction tests
//! (the reference suite).
//!
//! The fixture is built through the real `NativeScan`
//! writer (create/enqueue/claim/commit_page/declare_sitemap/write_context),
//! then run `skip_unsupported` + `run_retained_sitemap` over the stored file.
//! The scan file is written by
//! `NativeScanSchemaMixin`, reopened read-only for `ctx.scan`, and the same
//! assertions read `ctx.issues`/`ctx.skipped` plus the returned summary map.

use std::collections::HashSet;
use std::path::Path;

use rusqlite::Connection;
use serde_json::{json, Map, Value};
use spiderhead_checks::audit::{native_check_ids, rules, skip_unsupported_set, AuditContext};
use spiderhead_core::crawl_settings::{load, LoadInput};
use spiderhead_core::host::FakeHost;
use spiderhead_store::native_scan::NativeScan;
use spiderhead_store::scan_lease::{Lease, NativeScanLease, PageCommit};
use spiderhead_store::scan_schema::NativeScanSchemaMixin;
use tempfile::TempDir;

const START: &str = "https://example.test/";
const STATUS_CHECKS: [&str; 5] = [
    "SITEMAP_URL_3XX",
    "SITEMAP_URL_4XX_5XX",
    "SITEMAP_URL_NON_INDEXABLE",
    "SITEMAP_URL_NON_CANONICAL",
    "SITEMAP_URL_BLOCKED_ROBOTS",
];

fn object(value: Value) -> Map<String, Value> {
    value.as_object().cloned().expect("object")
}

fn runtime_versions() -> Map<String, Value> {
    object(json!({
        "python": "test",
        "sqlite": "test",
        "httpx": "test",
        "lxml": "test",
        "beautifulsoup4": "test",
    }))
}

/// `_metadata()`: `load(overrides={"speed.min_delay_seconds": 0})` as the
/// scan's effective configuration.
fn create(path: &Path) -> NativeScan {
    let config = load(
        &LoadInput {
            overrides: object(json!({"speed.min_delay_seconds": 0})),
            ..LoadInput::default()
        },
        &FakeHost::default(),
    )
    .expect("settings should load");
    NativeScanSchemaMixin::create(
        path,
        START,
        &object(config),
        None,
        "3.0.0",
        &"a".repeat(40),
        &runtime_versions(),
        &[],
        None,
        &[],
        "scan.v1",
    )
    .expect("create native scan")
}

/// `_runtime()`.
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

/// `_record(url)`: the commit-record shape taken from the shared store fixture
/// `commit-expected.json`, with the title, type and depth `_record` sets.
fn record(url: &str) -> Map<String, Value> {
    let fixture =
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../store/tests/fixtures/commit-expected.json");
    let expected: Value =
        serde_json::from_str(&std::fs::read_to_string(fixture).expect("fixture")).expect("json");
    let first_commit = expected["a"]["ops"]
        .as_array()
        .expect("ops")
        .iter()
        .find(|op| op["op"] == "commit")
        .expect("commit op");
    let mut base = first_commit["record"].as_object().expect("record").clone();
    base.remove("bogus");
    base.insert("url".into(), json!(url));
    base.insert("title".into(), json!("Home"));
    base.insert("status_code".into(), json!(200));
    base.insert("content_type".into(), json!("text/html"));
    base.insert(
        "crawl_depth".into(),
        json!(if url == START { 0 } else { 1 }),
    );
    base
}

/// `_link(source, destination)`.
fn link(source: &str, destination: &str) -> Map<String, Value> {
    object(json!({
        "source": source,
        "destination": destination,
        "anchor": "",
        "nofollow": false,
        "position": "",
        "rel": [],
        "target": "",
        "raw_href": "",
    }))
}

/// `_commit(scan, url, links, **record_values)`: claim the next lease and
/// commit `_record(url)` plus the authored overrides.
fn commit(scan: &mut NativeScan, url: &str, links: &[Map<String, Value>], values: Value) {
    let lease: Lease = scan.claim(1).expect("claim").remove(0);
    assert_eq!(lease.url, url);
    let mut rec = record(url);
    if let Value::Object(over) = values {
        for (k, v) in over {
            rec.insert(k, v);
        }
    }
    scan.commit_page(&PageCommit {
        lease: Some(lease),
        record: rec,
        links: links.to_vec(),
        runtime: Some(runtime()),
        ..Default::default()
    })
    .expect("commit_page");
}

/// `_declaration(scan, sitemap_url, members, complete=?)`.
fn declaration(
    scan: &NativeScan,
    sitemap_url: &str,
    members: &[(i64, String)],
    complete: bool,
) -> i64 {
    let sid = scan
        .declare_sitemap(sitemap_url, "explicit", 0)
        .expect("declare_sitemap");
    let entries: Vec<Value> = members
        .iter()
        .map(|(ordinal, url)| json!([ordinal, url]))
        .collect();
    scan.write_sitemap_members(sid, &entries)
        .expect("write_sitemap_members");
    scan.finish_sitemap(
        sid,
        complete,
        if complete { "" } else { "fixture incomplete" },
    )
    .expect("finish_sitemap");
    sid
}

/// `_save_robots(scan, text)`: a fetched `robots_summary` context row whose
/// `parsed` is `parse_robots(text)`.
fn save_robots(scan: &NativeScan, text: &str, token: &str) {
    let (groups, sitemaps) = spiderhead_fetch::robots::parse_robots(text);
    let parsed = json!({
        "groups": groups.iter().map(|g| g.to_value()).collect::<Vec<Value>>(),
        "sitemaps": sitemaps,
    });
    scan.write_context(&[json!({
        "kind": "robots_summary",
        "item_key": "run",
        "payload_version": "scan_context.v1",
        "payload_json": serde_json::to_string(&json!({
            "policy": "report_only",
            "token": token,
            "fetch_state": "fetched",
            "final_response_id": Value::Null,
            "note": "retained fixture",
            "parsed": parsed,
        }))
        .expect("payload"),
        "completeness": "complete",
        "reason": "",
    })])
    .expect("write_context");
}

/// `_robots_blocked(scan, url)`.
fn robots_blocked(scan: &NativeScan, url: &str) {
    let url_id: i64 = scan
        .con()
        .query_row("SELECT url_id FROM urls WHERE url=?", [url], |r| r.get(0))
        .expect("url_id");
    scan.write_context(&[json!({
        "kind": "robots_blocked_url",
        "item_key": format!("url:{url_id}"),
        "payload_version": "scan_context.v1",
        "payload_json": json!({
            "policy": "report_only",
            "token": "SEOHEAD-Tools",
            "url_id": url_id,
        })
        .to_string(),
        "completeness": "complete",
        "reason": "fixture",
    })])
    .expect("write_context");
}

/// `_run(scan)`: the stored file reopened read-only for the audit context.
fn run(path: &Path) -> (AuditContext, Map<String, Value>) {
    let mut ctx = AuditContext::default();
    ctx.scan = Some(Connection::open(path).expect("open stored scan"));
    let native = native_check_ids(&ctx);
    skip_unsupported_set(&mut ctx, &HashSet::new(), &native);
    let result = rules::sitemap::run_retained_sitemap(&mut ctx);
    (ctx, result)
}

fn findings(ctx: &AuditContext) -> HashSet<(String, String)> {
    ctx.issues
        .iter()
        .map(|i| (i.check.clone(), i.target_url.clone().unwrap_or_default()))
        .collect()
}

#[test]
fn all_retained_contradictions_and_fifteen_clean_showcase_pages() {
    let scratch = TempDir::new().expect("scratch dir");
    let path = scratch.path().join("scan.sqlite");
    {
        let mut scan = create(&path);
        let clean: Vec<String> = (0..15).map(|n| format!("{START}showcase-{n}")).collect();
        let bad = [
            ("redirect", 301),
            ("missing", 404),
            ("server", 503),
            ("noindex", 200),
            ("canonicalized", 200),
            ("blocked", 200),
        ];
        let mut urls = vec![START.to_owned()];
        urls.extend(clean.iter().cloned());
        urls.extend(bad.iter().map(|(name, _)| format!("{START}{name}")));
        urls.push(format!("{START}extra"));
        urls.push(format!("{START}unlinked-extra"));
        let entries: Vec<(String, i64)> = urls
            .iter()
            .map(|u| (u.clone(), if u == START { 0 } else { 1 }))
            .collect();
        scan.enqueue(&entries, "enqueue").expect("enqueue");
        let mut declared = vec![START.to_owned()];
        declared.extend(clean.iter().cloned());
        declared.extend(bad.iter().map(|(name, _)| format!("{START}{name}")));
        declared.push(format!("{START}unfetched"));
        declaration(
            &scan,
            &format!("{START}sitemap.xml"),
            &declared
                .iter()
                .enumerate()
                .map(|(i, u)| (i as i64, u.clone()))
                .collect::<Vec<_>>(),
            true,
        );
        save_robots(
            &scan,
            &format!(
                "User-agent: *\nDisallow: /blocked\nDisallow: /assets/\nAllow: /assets/public.js\nSitemap: {START}sitemap.xml"
            ),
            "SEOHEAD-Tools",
        );
        let mut linked: Vec<String> = clean.clone();
        linked.extend(bad.iter().map(|(name, _)| format!("{START}{name}")));
        linked.push(format!("{START}extra"));
        let root_links: Vec<Map<String, Value>> =
            linked.iter().map(|target| link(START, target)).collect();
        commit(&mut scan, START, &root_links, json!({}));
        for target in &clean {
            commit(&mut scan, target, &[], json!({"canonical": target}));
        }
        for (name, status) in bad {
            let mut values = object(json!({"status_code": status}));
            match name {
                "redirect" => {
                    values.insert("redirect_url".into(), json!(format!("{START}showcase-0")));
                }
                "noindex" => {
                    values.insert("x_robots".into(), json!("noindex"));
                }
                "canonicalized" => {
                    values.insert("canonical".into(), json!(format!("{START}showcase-0")));
                }
                _ => {}
            }
            commit(
                &mut scan,
                &format!("{START}{name}"),
                &[],
                Value::Object(values),
            );
        }
        commit(&mut scan, &format!("{START}extra"), &[], json!({}));
        commit(&mut scan, &format!("{START}unlinked-extra"), &[], json!({}));
        robots_blocked(&scan, &format!("{START}blocked"));
        for (ordinal, (target, kind)) in [
            (format!("{START}assets/private.js"), "script"),
            (format!("{START}assets/private.css"), "stylesheet"),
            (format!("{START}assets/public.js"), "script"),
            ("https://cdn.test/assets/private.js".to_owned(), "script"),
        ]
        .iter()
        .enumerate()
        {
            let page_id: i64 = scan
                .con()
                .query_row("SELECT url_id FROM urls WHERE url=?", [START], |r| r.get(0))
                .expect("page url_id");
            scan.con()
                .execute("INSERT OR IGNORE INTO urls(url) VALUES(?)", [target])
                .expect("intern resource url");
            let target_id: i64 = scan
                .con()
                .query_row("SELECT url_id FROM urls WHERE url=?", [target], |r| {
                    r.get(0)
                })
                .expect("resource url_id");
            scan.con()
                .execute(
                    "INSERT INTO resource_refs(page_url_id,ordinal,resource_url_id,kind,representation,raw_url,capture_state,reason) VALUES(?,?,?,?,?,?,?,?)",
                    rusqlite::params![page_id, ordinal as i64, target_id, kind, "static", target, "not_fetched", "fixture"],
                )
                .expect("insert resource ref");
        }
        scan.close();
    }
    let (ctx, result) = run(&path);
    let found = findings(&ctx);
    let expected: HashSet<(String, String)> = [
        ("SITEMAP_URL_3XX", format!("{START}redirect")),
        ("SITEMAP_URL_4XX_5XX", format!("{START}missing")),
        ("SITEMAP_URL_4XX_5XX", format!("{START}server")),
        ("SITEMAP_URL_NON_INDEXABLE", format!("{START}noindex")),
        ("SITEMAP_URL_NON_CANONICAL", format!("{START}canonicalized")),
        ("SITEMAP_URL_BLOCKED_ROBOTS", format!("{START}blocked")),
        (
            "INTERNALLY_LINKED_ROBOTS_BLOCKED",
            format!("{START}blocked"),
        ),
        ("URL_NOT_IN_SITEMAP", format!("{START}extra")),
        ("URL_NOT_IN_SITEMAP", format!("{START}unlinked-extra")),
        ("ROBOTS_BLOCKS_RESOURCES", START.to_owned()),
        ("SITEMAP_ORPHAN", format!("{START}unfetched")),
    ]
    .iter()
    .map(|(c, u)| (c.to_string(), u.clone()))
    .collect();
    assert!(
        expected.is_subset(&found),
        "missing: {:?}",
        &expected - &found
    );
    let clean_set: HashSet<String> = (0..15).map(|n| format!("{START}showcase-{n}")).collect();
    for status_check in &STATUS_CHECKS[..4] {
        for issue in ctx
            .issues
            .iter()
            .filter(|i| i.check == *status_check)
            .filter_map(|i| i.target_url.as_deref())
        {
            assert!(
                !clean_set.contains(issue),
                "{status_check} fired on clean page {issue}"
            );
        }
    }
    assert_eq!(
        result["canonical_pages_not_in_sitemap_count"].as_i64(),
        Some(2)
    );
    assert_eq!(result["sitemap_status_unmeasured"].as_i64(), Some(1));
    let resources: Vec<&str> = ctx
        .issues
        .iter()
        .filter(|i| i.check == "ROBOTS_BLOCKS_RESOURCES")
        .filter_map(|i| i.details.get("target_url").and_then(Value::as_str))
        .collect();
    assert_eq!(
        resources,
        [
            format!("{START}assets/private.js"),
            format!("{START}assets/private.css"),
        ]
    );
    assert!(ctx
        .issues
        .iter()
        .all(|i| i.details.get("evidence_source") == Some(&json!("retained_scan"))));
    assert!(ctx
        .issues
        .iter()
        .filter(|i| i.check.starts_with("SITEMAP_URL_"))
        .all(|i| i.details.get("source_url").is_some()));
}

#[test]
fn partial_sitemap_keeps_positive_status_and_suppresses_missing_coverage() {
    let scratch = TempDir::new().expect("scratch dir");
    let path = scratch.path().join("scan.sqlite");
    {
        let mut scan = create(&path);
        scan.enqueue(
            &[(START.to_owned(), 0), (format!("{START}broken"), 1)],
            "enqueue",
        )
        .expect("enqueue");
        declaration(
            &scan,
            &format!("{START}sitemap.xml"),
            &[(0, format!("{START}broken"))],
            false,
        );
        commit(
            &mut scan,
            START,
            &[link(START, &format!("{START}broken"))],
            json!({}),
        );
        commit(
            &mut scan,
            &format!("{START}broken"),
            &[],
            json!({"status_code": 404}),
        );
        scan.close();
    }
    let (ctx, result) = run(&path);
    assert_eq!(
        findings(&ctx),
        [
            ("SITEMAP_URL_4XX_5XX", format!("{START}broken")),
            ("SITEMAP_FETCH_INCOMPLETE", START.to_owned()),
        ]
        .iter()
        .map(|(c, u)| (c.to_string(), u.clone()))
        .collect()
    );
    assert_eq!(result["reconciliation_available"].as_bool(), Some(false));
    let skipped: HashSet<&str> = ctx.skipped.iter().map(|s| s.id.as_str()).collect();
    for check in ["URL_NOT_IN_SITEMAP", "SITEMAP_ORPHAN", "SITEMAP_DESYNC"] {
        assert!(skipped.contains(check), "{check} should be skipped");
    }
}

#[test]
fn partial_crawl_suppresses_orphan_but_keeps_missing_fetched_canonical() {
    let scratch = TempDir::new().expect("scratch dir");
    let path = scratch.path().join("scan.sqlite");
    {
        let mut scan = create(&path);
        scan.enqueue(&[(START.to_owned(), 0)], "enqueue")
            .expect("enqueue");
        declaration(
            &scan,
            &format!("{START}sitemap.xml"),
            &[(0, format!("{START}unvisited"))],
            true,
        );
        commit(&mut scan, START, &[], json!({}));
        scan.con()
            .execute("UPDATE scan SET crawl_partial=1", [])
            .expect("mark partial");
        scan.close();
    }
    let (ctx, result) = run(&path);
    assert_eq!(
        ctx.issues
            .iter()
            .map(|i| i.check.as_str())
            .collect::<Vec<_>>(),
        ["URL_NOT_IN_SITEMAP"]
    );
    assert_eq!(result["crawl_partial"].as_bool(), Some(true));
    let skipped: HashSet<&str> = ctx.skipped.iter().map(|s| s.id.as_str()).collect();
    for check in ["SITEMAP_ORPHAN", "SITEMAP_DESYNC"] {
        assert!(skipped.contains(check), "{check} should be skipped");
    }
}

#[test]
fn robots_origin_and_specific_user_agent_allow_rules_are_respected() {
    let scratch = TempDir::new().expect("scratch dir");
    let path = scratch.path().join("scan.sqlite");
    {
        let mut scan = create(&path);
        scan.enqueue(&[(START.to_owned(), 0)], "enqueue")
            .expect("enqueue");
        commit(
            &mut scan,
            START,
            &[
                link(START, &format!("{START}private/allowed")),
                link(START, &format!("{START}private/blocked?x=1")),
                link(START, "http://example.test/private/blocked"),
                link(START, "https://other.test/private/blocked"),
            ],
            json!({}),
        );
        save_robots(
            &scan,
            "User-agent: *\nDisallow: /\nUser-agent: SEOHEAD\nDisallow: /private/\nAllow: /private/allowed",
            "SEOHEAD-Tools",
        );
        scan.close();
    }
    let (ctx, result) = run(&path);
    assert_eq!(
        findings(&ctx),
        [
            (
                "INTERNALLY_LINKED_ROBOTS_BLOCKED",
                format!("{START}private/blocked?x=1")
            ),
            ("SITEMAP_NOT_IN_ROBOTS", START.to_owned()),
        ]
        .iter()
        .map(|(c, u)| (c.to_string(), u.clone()))
        .collect()
    );
    assert_eq!(result["reconciliation_available"].as_bool(), Some(false));
}

#[test]
fn summary_keeps_bounded_examples_and_complete_findings() {
    let scratch = TempDir::new().expect("scratch dir");
    let path = scratch.path().join("scan.sqlite");
    {
        let mut scan = create(&path);
        scan.enqueue(&[(START.to_owned(), 0)], "enqueue")
            .expect("enqueue");
        let members: Vec<(i64, String)> = (0..101)
            .map(|n| (n as i64, format!("{START}unvisited-{n}")))
            .collect();
        declaration(&scan, &format!("{START}sitemap.xml"), &members, true);
        commit(&mut scan, START, &[], json!({}));
        scan.close();
    }
    let (ctx, result) = run(&path);
    assert_eq!(result["in_sitemap_not_linked_count"].as_i64(), Some(101));
    assert_eq!(
        result["in_sitemap_not_linked"].as_array().map(Vec::len),
        Some(20)
    );
    assert_eq!(
        ctx.issues
            .iter()
            .filter(|i| i.check == "SITEMAP_ORPHAN")
            .count(),
        101
    );
}

#[test]
fn entrypoint_needs_no_inbound_link_and_does_not_create_desync() {
    let scratch = TempDir::new().expect("scratch dir");
    let path = scratch.path().join("scan.sqlite");
    {
        let mut scan = create(&path);
        scan.enqueue(&[(START.to_owned(), 0)], "enqueue")
            .expect("enqueue");
        declaration(
            &scan,
            &format!("{START}sitemap.xml"),
            &[(0, START.to_owned())],
            true,
        );
        commit(&mut scan, START, &[], json!({}));
        scan.close();
    }
    let (ctx, result) = run(&path);
    assert!(ctx.issues.is_empty(), "issues: {:?}", ctx.issues);
    let skipped: HashSet<&str> = ctx.skipped.iter().map(|s| s.id.as_str()).collect();
    for check in &STATUS_CHECKS[..4] {
        assert!(!skipped.contains(check), "{check} must not be skipped");
    }
    let native = native_check_ids(&ctx);
    for check in &STATUS_CHECKS[..3] {
        assert!(native.contains(*check), "{check} must be native-resolved");
    }
    assert_eq!(result["sitemap_orphan_findings_count"].as_i64(), Some(0));
}
