//! Ported tests from the reference suite — the context-level cases that do
//! not need `aggregate`, `NativeScan` or `sys.getsizeof` cache accounting.

use serde_json::{json, Value};
use spiderhead_formats::models::Page;
use spiderhead_sf_import::context::disk::DiskPages;
use spiderhead_sf_import::context::{AddOpts, AuditContext};
use spiderhead_sf_import::loader::LoadedExports;
use spiderhead_sf_import::normalize::Object;
use spiderhead_sf_import::table::Table;

/// A `LoadedExports` holding one `internal_all` frame built in memory — the
/// same path `load_exports` takes after `read_table`.
fn exports_for(rows: Vec<Vec<Value>>) -> LoadedExports {
    let columns = [
        "Address",
        "Content Type",
        "Status Code",
        "Status",
        "Indexability",
    ]
    .iter()
    .map(|s| s.to_string())
    .collect();
    let table = Table::new(columns, rows);
    let mut e = LoadedExports::default();
    e.frames.insert("internal_all".into(), table);
    e
}

fn s(v: &str) -> Value {
    Value::String(v.to_owned())
}

fn page_row(url: &str, ctype: &str, code: i64, status: &str, index: &str) -> Vec<Value> {
    vec![s(url), s(ctype), Value::from(code), s(status), s(index)]
}

fn new_ctx(exports: LoadedExports, disk: bool) -> AuditContext {
    AuditContext::new(exports, json!({}), None, disk).unwrap()
}

#[test]
fn duplicate_urls_collapse() {
    let e = exports_for(vec![
        page_row("https://example.com/", "text/html", 200, "OK", "Indexable"),
        page_row("https://example.com/", "text/html", 200, "OK", "Indexable"),
        page_row("https://example.com/x", "text/html", 200, "OK", "Indexable"),
    ]);
    let mut ctx = new_ctx(e, false);
    let urls: Vec<String> = ctx.pages_iter().iter().map(|h| h.url()).collect();
    assert_eq!(urls, ["https://example.com/", "https://example.com/x"]);
    assert_eq!(ctx.url_count(), ctx.pages_len());
}

#[test]
fn html_pages_excludes_redirect_and_error_stubs() {
    let e = exports_for(vec![
        page_row(
            "https://example.com/live",
            "text/html",
            200,
            "OK",
            "Indexable",
        ),
        page_row(
            "https://example.com/old-page",
            "text/html",
            301,
            "Moved",
            "Non-Indexable",
        ),
        page_row(
            "https://example.com/gone",
            "text/html; charset=UTF-8",
            404,
            "Not Found",
            "Non-Indexable",
        ),
    ]);
    let mut ctx = new_ctx(e, false);
    let urls: Vec<String> = ctx.html_pages().iter().map(|h| h.url()).collect();
    assert_eq!(urls, ["https://example.com/live"]);
}

#[test]
fn disk_backed_pages_preserve_metrics_and_normalized_lookup() {
    let e = exports_for(vec![
        page_row(
            "https://example.com/old",
            "text/html",
            301,
            "Moved",
            "Non-Indexable",
        ),
        page_row(
            "https://example.com/live/",
            "text/html",
            200,
            "OK",
            "Indexable",
        ),
    ]);
    let mut ctx = new_ctx(e, true);
    let store_path = ctx.disk_pages().unwrap().path().clone();
    assert!(store_path.exists());
    assert_eq!(ctx.pages_len(), 2);
    let live = ctx.page_by_norm("https://example.com/live").unwrap();
    assert_eq!(live.url(), "https://example.com/live/");
    ctx.mutate_page("https://example.com/live/", |p| {
        p.metrics.insert("bytes_per_word".into(), json!(3.5));
        p.issue_ids.push("ISSUE-000001".into());
    });
    let reopened = ctx.page_by_url("https://example.com/live/").unwrap();
    reopened.map(|p| {
        assert_eq!(p.metrics.get("bytes_per_word"), Some(&json!(3.5)));
        assert_eq!(p.issue_ids, ["ISSUE-000001"]);
    });
    ctx.add(
        "TITLE_MISSING",
        AddOpts {
            target_url: Some("https://example.com/live/".into()),
            ..AddOpts::default()
        },
    )
    .unwrap();
    assert_eq!(
        ctx.issues_iter()
            .iter()
            .map(|i| i.check.clone())
            .collect::<Vec<_>>(),
        ["TITLE_MISSING"]
    );
    ctx.retract("TITLE_MISSING", "fixture withdrawal").unwrap();
    assert!(ctx.issues_iter().is_empty());
    let group = ctx
        .add_group(
            "TITLE_DUPLICATE",
            Some("same title".into()),
            vec!["https://example.com/live/".into()],
        )
        .unwrap()
        .unwrap();
    assert_eq!(group.group_id, "GRP-TITLE-0001");
    let urls: Vec<String> = ctx.indexable_html_pages().iter().map(|h| h.url()).collect();
    assert_eq!(urls, ["https://example.com/live/"]);
    ctx.close();
    assert!(!store_path.exists());
}

#[test]
fn disk_page_cache_evicts_and_batched_findings_keep_persisted_state() {
    let records = vec![
        {
            let mut r = Object::new();
            r.insert("url".into(), s("https://example.test/a"));
            r.insert("status_code".into(), json!(301));
            r
        },
        {
            let mut r = Object::new();
            r.insert("url".into(), s("https://example.test/a/"));
            r.insert("status_code".into(), json!(200));
            r
        },
        {
            let mut r = Object::new();
            r.insert("url".into(), s("https://example.test/b"));
            r.insert("status_code".into(), json!(200));
            r
        },
    ];
    let mut store = DiskPages::new(records, |rec| {
        let mut p = Page::new(rec.get("url").and_then(Value::as_str).unwrap_or(""));
        p.status_code = rec.get("status_code").and_then(Value::as_i64);
        Ok(p)
    })
    .unwrap();
    store.set_cache_limit(1);
    let page = store.representative("https://example.test/a").unwrap();
    assert_eq!(page.url, "https://example.test/a/");
    // The page cache keeps the just-loaded page; the write-through path must
    // survive an eviction of everything else.
    store.mutate(&page.url, |p| {
        p.metrics.insert("observed".into(), json!(42));
    });
    let _ = store.get("https://example.test/b");
    assert_eq!(store.cache_len(), 1);
    let reloaded = store.get(&page.url).unwrap();
    assert_eq!(reloaded.metrics.get("observed"), Some(&json!(42)));
    store.attach_issue(&page.url, "TITLE_MISSING", "ISSUE-1", false);
    store.attach_issue(&page.url, "TITLE_MISSING", "ISSUE-2", false);
    store.attach_issue(&page.url, "H1_MISSING", "ISSUE-3", true);
    let reloaded = store.get(&page.url).unwrap();
    assert_eq!(reloaded.metrics.get("observed"), Some(&json!(42)));
    assert_eq!(reloaded.issues, ["TITLE_MISSING"]);
    assert_eq!(reloaded.issue_ids, ["ISSUE-1", "ISSUE-2"]);
    assert_eq!(reloaded.suppressed_issue_ids, ["ISSUE-3"]);
    store.close();
}

#[test]
fn html_page_keys_stream_unique_normalized_fetched_html() {
    for disk in [false, true] {
        let columns = ["Address", "Content Type", "Status Code"]
            .iter()
            .map(|s| s.to_string())
            .collect();
        let table = Table::new(
            columns,
            vec![
                vec![s("https://example.test/a"), s("text/html"), json!(200)],
                vec![
                    s("https://example.test/a/"),
                    s("application/xhtml+xml"),
                    json!(200),
                ],
                vec![s("https://example.test/gone"), s("text/html"), json!(404)],
                vec![s("https://example.test/image"), s("image/png"), json!(200)],
            ],
        );
        let mut e = LoadedExports::default();
        e.frames.insert("internal_all".into(), table);
        let mut ctx = new_ctx(e, disk);
        let keys = ctx.html_page_keys();
        assert_eq!(keys, ["https://example.test/a"]);
        ctx.close();
    }
}
