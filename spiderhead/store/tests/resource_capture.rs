//! Resource-graph fetch pass against a scripted fetcher: budgets, redirects, scope, failures.

use rusqlite::Connection;
use serde_json::{json, Value};
use spiderhead_store::resource_capture::{capture, FetchFailure, FetchedResource, ResourceFetcher};
use spiderhead_store::resource_graph::{invalidate_pages, read, store_document};
use spiderhead_store::schema_sql::{SCAN_V1_SQL, SCAN_V2_SQL};

type Reply = Box<dyn FnMut(&str) -> Result<FetchedResource, FetchFailure>>;

struct Script {
    calls: Vec<String>,
    reply: Reply,
}

impl ResourceFetcher for Script {
    fn fetch(&mut self, url: &str, _max: i64) -> Result<FetchedResource, FetchFailure> {
        self.calls.push(url.to_owned());
        (self.reply)(url)
    }
    fn delivery_json(&self, body: &[u8], kind: &str, _h: &[(String, String)]) -> String {
        format!("{{\"kind\": \"{kind}\", \"bytes\": {}}}", body.len())
    }
}

fn script(reply: impl FnMut(&str) -> Result<FetchedResource, FetchFailure> + 'static) -> Script {
    Script {
        calls: Vec::new(),
        reply: Box::new(reply),
    }
}

fn answer(status: i64, body: &[u8], location: &str, content_type: &str) -> FetchedResource {
    FetchedResource {
        status_code: Some(status),
        redirect_url: location.to_owned(),
        content_type: content_type.to_owned(),
        body: Some(body.to_vec()),
        body_complete: true,
        ..Default::default()
    }
}

fn scan_with(html: &str) -> Connection {
    let con = Connection::open_in_memory().unwrap();
    con.execute_batch(SCAN_V1_SQL).unwrap();
    con.execute_batch(SCAN_V2_SQL).unwrap();
    con.execute_batch("DROP TABLE scan; CREATE TABLE scan(singleton INTEGER,start_url TEXT);")
        .unwrap();
    con.execute("INSERT INTO scan VALUES (1,'https://example.test/')", [])
        .unwrap();
    con.execute_batch("PRAGMA user_version=2").unwrap();
    con.execute("INSERT INTO urls(url) VALUES ('https://example.test/')", [])
        .unwrap();
    con.execute(
        "INSERT INTO documents(url_id,representation,captured_at,decoder_version,decoder_source,\
         decoder_charset,decoder_errors,fidelity,body_state,body_reason,renderer_json) \
         VALUES (1,'static','2026-01-01T00:00:00Z','scan_decoder.v1','not_applicable','',\
         'not_applicable','unavailable','unavailable','none','{}')",
        [],
    )
    .unwrap();
    store_document(&con, 1, 1, "static", Some(html)).unwrap();
    con
}

fn settings(overrides: Value) -> Value {
    let mut graph = json!({
        "max_requests": 100, "max_bytes": 1_000_000, "max_seconds": 0, "max_origins": 10,
        "max_nesting": 2, "max_bytes_per_resource": 100_000, "max_redirects": 5,
    });
    if let Some(extra) = overrides.as_object() {
        for (k, v) in extra {
            graph[k] = v.clone();
        }
    }
    json!({"resources": {"fetch": true, "graph": graph}, "scope": {}})
}

fn totals(map: &indexmap::IndexMap<String, i64>) -> Vec<i64> {
    ["stored", "fetched", "excluded", "failed", "budget"]
        .iter()
        .map(|k| map[*k])
        .collect()
}

fn run(con: &Connection, cfg: &Value, fetcher: &mut Script) -> Vec<i64> {
    totals(&capture(con, cfg, fetcher, &mut || 0.0).unwrap())
}

#[test]
fn redirect_outside_scope_is_excluded_without_following() {
    let con = scan_with("<img src=\"/image.png\">");
    let mut fetcher = script(|_| Ok(answer(302, b"", "https://outside.test/image.png", "")));
    assert_eq!(
        run(&con, &settings(json!({})), &mut fetcher),
        [0, 0, 1, 0, 0]
    );
    assert_eq!(fetcher.calls, ["https://example.test/image.png"]);
    let row: (String, String, String) = con
        .query_row(
            "SELECT state,reason,final_url FROM resource_graph_fetches",
            [],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        )
        .unwrap();
    assert_eq!(
        row,
        (
            "excluded".into(),
            "outside_host".into(),
            "https://example.test/image.png".into()
        )
    );
}

#[test]
fn request_budget_counts_a_redirect_hop_before_following_it() {
    let con = scan_with("<script src=\"/redirect.js\"></script>");
    let mut fetcher = script(|_| Ok(answer(302, b"hop", "/final.js", "")));
    let cfg = settings(json!({"max_requests": 1, "max_redirects": 2}));
    assert_eq!(run(&con, &cfg, &mut fetcher), [0, 0, 0, 0, 1]);
    assert_eq!(fetcher.calls, ["https://example.test/redirect.js"]);
    let row: (String, String, i64) = con
        .query_row(
            "SELECT state,reason,bytes_received FROM resource_graph_fetches",
            [],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        )
        .unwrap();
    assert_eq!(
        row,
        (
            "budget".into(),
            "resource graph request budget exhausted".into(),
            3
        )
    );
}

#[test]
fn byte_budget_counts_redirect_entity_before_following_it() {
    let con = scan_with("<script src=\"/redirect.js\"></script>");
    let mut fetcher = script(|_| Ok(answer(302, b"x", "/final.js", "")));
    let cfg = settings(
        json!({"max_requests": 10, "max_bytes": 1, "max_bytes_per_resource": 8, "max_redirects": 2}),
    );
    assert_eq!(run(&con, &cfg, &mut fetcher), [0, 0, 0, 0, 1]);
    assert_eq!(fetcher.calls, ["https://example.test/redirect.js"]);
}

#[test]
fn complete_stylesheet_stores_delivery_and_nested_declarations() {
    let con = scan_with("<link rel=\"stylesheet\" href=\"/s.css\">");
    let mut fetcher = script(|url| {
        Ok(if url.ends_with(".css") {
            answer(
                200,
                b"a{background:url(/bg.png)}",
                "",
                "text/css; charset=utf-8",
            )
        } else {
            answer(200, b"png", "", "image/png")
        })
    });
    assert_eq!(
        run(&con, &settings(json!({})), &mut fetcher),
        [0, 2, 0, 0, 0]
    );
    assert_eq!(
        fetcher.calls,
        ["https://example.test/s.css", "https://example.test/bg.png"]
    );
    let delivery: String = con
        .query_row(
            "SELECT delivery_json FROM resource_graph_fetches WHERE resolved_url='https://example.test/s.css'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(delivery, "{\"kind\": \"stylesheet\", \"bytes\": 26}");
}

#[test]
fn transport_failure_is_recorded_and_request_budget_stops_the_pass() {
    let con = scan_with("<img src=\"/a.png\"><img src=\"/b.png\"><img src=\"/c.png\">");
    let mut n = 0;
    let mut fetcher = script(move |_| {
        n += 1;
        match n {
            1 => Err(FetchFailure::Failed("connection refused".into())),
            _ => Err(FetchFailure::RequestBudgetExhausted),
        }
    });
    assert_eq!(
        run(&con, &settings(json!({})), &mut fetcher),
        [0, 0, 0, 1, 1]
    );
    let reasons: Vec<String> = con
        .prepare("SELECT reason FROM resource_graph_occurrences ORDER BY occurrence_id")
        .unwrap()
        .query_map([], |r| r.get(0))
        .unwrap()
        .map(Result::unwrap)
        .collect();
    assert_eq!(
        reasons,
        [
            "resource fetch failed",
            "total HTTP request budget exhausted",
            "resource fetch is disabled"
        ]
    );
}

#[test]
fn capture_does_nothing_when_fetching_is_off_or_artifact_is_v1() {
    let con = scan_with("<img src=\"/a.png\">");
    let mut fetcher = script(|_| Ok(answer(200, b"", "", "")));
    let mut off = settings(json!({}));
    off["resources"]["fetch"] = json!(false);
    assert_eq!(run(&con, &off, &mut fetcher), [0; 5]);
    con.execute_batch("PRAGMA user_version=1").unwrap();
    assert_eq!(run(&con, &settings(json!({})), &mut fetcher), [0; 5]);
    assert!(fetcher.calls.is_empty());
}

#[test]
fn integrity_is_per_declaration_not_collapsed_by_resource_url() {
    let con = scan_with(
        "<script src=\"/shared.js\" integrity=\"sha384-first\"></script>\
         <script src=\"/shared.js\"></script>\
         <link rel=\"stylesheet\" href=\"/site.css\" integrity=\"\">",
    );
    let rows: Vec<(String, Option<String>, String)> = con
        .prepare("SELECT raw_url,integrity,integrity_state FROM resource_graph_occurrences ORDER BY ordinal")
        .unwrap()
        .query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))
        .unwrap()
        .map(Result::unwrap)
        .collect();
    assert_eq!(
        rows,
        [
            (
                "/shared.js".to_owned(),
                Some("sha384-first".to_owned()),
                "declared".to_owned()
            ),
            ("/shared.js".to_owned(), None, "absent".to_owned()),
            (
                "/site.css".to_owned(),
                Some(String::new()),
                "declared".to_owned()
            ),
        ]
    );
}

#[test]
fn requeue_invalidation_discards_old_page_graph() {
    let con = scan_with("<img src=\"/shared.png\">");
    con.execute(
        "INSERT INTO urls(url) VALUES ('https://example.test/other')",
        [],
    )
    .unwrap();
    con.execute(
        "INSERT INTO documents(url_id,representation,captured_at,decoder_version,decoder_source,\
         decoder_charset,decoder_errors,fidelity,body_state,body_reason,renderer_json) \
         VALUES (2,'static','2026-01-01T00:00:00Z','scan_decoder.v1','not_applicable','',\
         'not_applicable','unavailable','unavailable','none','{}')",
        [],
    )
    .unwrap();
    store_document(&con, 2, 2, "static", Some("<img src=\"/shared.png\">")).unwrap();
    let mut fetcher = script(|_| Ok(answer(200, b"image", "", "image/png")));
    assert_eq!(
        run(&con, &settings(json!({})), &mut fetcher),
        [0, 1, 0, 0, 0]
    );
    invalidate_pages(&con, &[1]).unwrap();
    let remaining: (String, String) = con
        .query_row(
            "SELECT state,reason FROM resource_graph_occurrences WHERE page_url_id=2",
            [],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .unwrap();
    assert_eq!(
        remaining,
        (
            "disabled".into(),
            "resource fetch invalidated by requeue".into()
        )
    );
    let fetches: i64 = con
        .query_row("SELECT COUNT(*) FROM resource_graph_fetches", [], |r| {
            r.get(0)
        })
        .unwrap();
    assert_eq!(fetches, 0);
    let old_context: i64 = con
        .query_row(
            "SELECT COUNT(*) FROM context_items WHERE kind='resource_graph_coverage' AND item_key='document:1'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(old_context, 0);
}

#[test]
fn v1_and_empty_v2_reads_do_not_create_the_graph() {
    let v1 = Connection::open_in_memory().unwrap();
    v1.execute_batch(SCAN_V1_SQL).unwrap();
    v1.execute_batch("PRAGMA user_version=1").unwrap();
    let got = read(&v1, 1000, 0).unwrap();
    assert_eq!(got["state"], "unavailable");
    assert_eq!(got["reason"], "resource graph was not stored in this scan");
    let v2 = Connection::open_in_memory().unwrap();
    v2.execute_batch("PRAGMA user_version=2").unwrap();
    let got = read(&v2, 1000, 0).unwrap();
    assert_eq!(got["reason"], "resource graph extension is absent");
    for con in [&v1, &v2] {
        let n: i64 = con
            .query_row(
                "SELECT COUNT(*) FROM sqlite_master WHERE name LIKE 'resource_graph_%'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(n, 0);
    }
}

#[test]
fn read_exposes_per_kind_declaration_and_weight_aggregates() {
    let con = scan_with("<img src=\"/image.png\">");
    let mut fetcher = script(|_| Ok(answer(200, b"resource", "", "image/png")));
    run(&con, &settings(json!({})), &mut fetcher);
    let got = read(&con, 1000, 0).unwrap();
    let aggregates = &got["aggregates"];
    assert_eq!(aggregates["occurrences_by_kind"], json!({"image": 1}));
    assert_eq!(aggregates["blocks_by_state"], json!({"complete": 1}));
    assert_eq!(
        aggregates["unique_resource_weight_by_kind"]["image"],
        json!({"unique_urls": 1, "complete_urls": 1, "unmeasured_urls": 0, "bytes_received": 8, "bytes_state": "exact"})
    );
}
