//! Import, summary, join and bot-evidence parity against the retained-store fixtures.

mod common;

use std::path::Path;
use std::sync::Arc;

use rusqlite::Connection;
use serde_json::{json, Value};
use spiderhead_log_analyzer::bot_evidence::{bot_verification, BotQuery};
use spiderhead_log_analyzer::import::{import_logs, ImportContext, ImportOptions};
use spiderhead_log_analyzer::join::{
    join_log_scan, JoinQuery, ScanHeader, ScanOpener, ScanPage, ScanSource,
};
use spiderhead_log_analyzer::store::query_rows;
use spiderhead_log_analyzer::summary::{log_summary, Filter, SummaryQuery};

use common::{
    assert_same, fixtures, load, log_path, make_project, verifier, DirLoader, FakeResolver,
};

fn int(q: &Value, k: &str) -> Option<i64> {
    q.get(k).and_then(Value::as_i64)
}

fn text(q: &Value, k: &str) -> String {
    q.get(k).and_then(Value::as_str).unwrap_or("").to_string()
}

fn do_import(root: &Path, path: &str, opts: &Value) -> Value {
    let free = |_: &Path| Ok(u64::MAX);
    let now = || 1_790_000_000.25;
    let ctx = ImportContext {
        loader: &DirLoader,
        free_space: &free,
        now: &now,
    };
    let mut o = ImportOptions {
        keep_requests: false,
        ..ImportOptions::default()
    };
    o.max_lines = int(opts, "max_lines").unwrap_or(o.max_lines);
    o.sample_size = int(opts, "sample_size").unwrap_or(o.sample_size);
    import_logs(&ctx, path, &root.to_string_lossy(), o)
}

fn drop_volatile(mut v: Value) -> Value {
    if let Some(o) = v.as_object_mut() {
        for k in ["import_id", "source", "created_at", "project_uuid"] {
            o.remove(k);
        }
    }
    v
}

fn tables(source: &str) -> Value {
    let con = Connection::open(source).unwrap();
    let rows = |sql: &str| {
        Value::Array(
            query_rows(&con, sql, &[])
                .unwrap()
                .into_iter()
                .map(Value::Object)
                .collect(),
        )
    };
    let mut sources = query_rows(&con, "SELECT * FROM sources ORDER BY sha256", &[]).unwrap();
    for s in &mut sources {
        s.remove("path");
        s.remove("mtime_ns");
    }
    json!({
        "visits": rows("SELECT * FROM visits ORDER BY bot,ip,url,date,status"),
        "bot_ips": rows("SELECT * FROM bot_ips ORDER BY bot,ip"),
        "sources": sources,
    })
}

/// Decompressor failures differ in wording only.
const WORDING: [&str; 2] = ["truncated.gz", "corrupt.gz"];

#[test]
fn imports_store_the_same_aggregates() {
    let cases = load("store_cases.json");
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().join("project");
    make_project(&root, "https://example.test");
    for case in cases["imports"].as_array().unwrap() {
        let name = case["file"].as_str().unwrap();
        let result = do_import(&root, &log_path(name), &case["options"]);
        let label = format!("{name} {}", case["options"]);
        if WORDING.contains(&name) {
            assert_eq!(result["ok"], json!(false), "{label}");
            continue;
        }
        assert_same(&label, &case["result"], &drop_volatile(result.clone()), &[]);
        if let Some(expected) = case.get("tables") {
            assert_same(
                &label,
                expected,
                &tables(result["source"].as_str().unwrap()),
                &[],
            );
        }
    }
    let leftovers: Vec<_> = std::fs::read_dir(root.join("logs")).unwrap().collect();
    let kept = cases["imports"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|c| c["result"]["ok"] == json!(true))
        .count();
    assert_eq!(
        leftovers.len(),
        kept,
        "failed imports leave no artifact behind"
    );
}

#[test]
fn directory_import_sorts_files_and_reads_gzip_by_content() {
    let cases = load("store_cases.json");
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path().join("dir");
    std::fs::create_dir_all(dir.join("sub")).unwrap();
    std::fs::copy(log_path("combined.log"), dir.join("access.1")).unwrap();
    std::fs::copy(log_path("renamed.archive"), dir.join("renamed.archive")).unwrap();
    std::fs::copy(log_path("nginx.log"), dir.join("sub/nginx.log")).unwrap();
    let root = tmp.path().join("project");
    make_project(&root, "https://example.test");
    let result = do_import(&root, &dir.to_string_lossy(), &json!({}));
    assert_same(
        "directory",
        &cases["directory"]["result"],
        &drop_volatile(result.clone()),
        &[],
    );
    assert_same(
        "directory",
        &cases["directory"]["tables"],
        &tables(result["source"].as_str().unwrap()),
        &[],
    );
}

fn summary_query(q: &Value) -> SummaryQuery {
    let mut s = SummaryQuery::default();
    if let Some(v) = q.get("view").and_then(Value::as_str) {
        s.view = v.into();
    }
    s.limit = int(q, "limit").unwrap_or(s.limit);
    s.offset = int(q, "offset").unwrap_or(s.offset);
    s.filter = Filter {
        bot: text(q, "bot"),
        family: text(q, "family"),
        ip: text(q, "ip"),
        date_from: text(q, "date_from"),
        date_to: text(q, "date_to"),
        status: int(q, "status"),
        section: text(q, "section"),
    };
    s
}

fn check_summaries(source: &str, extra_ignore: &[&str]) {
    for case in load("store_cases.json")["summaries"].as_array().unwrap() {
        let q = &case["query"];
        let actual = log_summary(source, &summary_query(q));
        let mut ignore = vec!["/source"];
        ignore.extend_from_slice(extra_ignore);
        if case["expected"]["ok"] == json!(false) {
            assert_eq!(actual["ok"], json!(false), "{q}");
            if q.get("date_from").is_none() {
                assert_eq!(actual["error"], case["expected"]["error"], "{q}");
            }
        } else {
            assert_same(&q.to_string(), &case["expected"], &actual, &ignore);
        }
    }
}

#[test]
fn summaries_over_a_foreign_artifact_match() {
    let tmp = tempfile::tempdir().unwrap();
    let copy = tmp.path().join("combined_store.sqlite");
    std::fs::copy(fixtures().join("combined_store.sqlite"), &copy).unwrap();
    check_summaries(&copy.to_string_lossy(), &[]);
}

#[test]
fn summaries_over_a_native_import_match() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().join("project");
    make_project(&root, "https://example.test");
    let result = do_import(&root, &log_path("combined.log"), &json!({}));
    check_summaries(result["source"].as_str().unwrap(), &["/import"]);
}

struct JsonScan {
    kwargs: Value,
    pages: Value,
}

impl ScanSource for JsonScan {
    fn header(&self) -> Result<ScanHeader, String> {
        let k = &self.kwargs;
        let state = k
            .get("pages_state")
            .and_then(Value::as_str)
            .unwrap_or("complete");
        Ok(ScanHeader {
            source_kind: k
                .get("kind")
                .and_then(Value::as_str)
                .unwrap_or("native")
                .into(),
            lifecycle: k
                .get("lifecycle")
                .and_then(Value::as_str)
                .unwrap_or("finished")
                .into(),
            crawl_partial: k.get("partial").and_then(Value::as_bool).unwrap_or(false),
            capabilities: json!({"pages": {"state": state}}),
            start_url: k
                .get("start")
                .and_then(Value::as_str)
                .unwrap_or("https://example.test/")
                .into(),
            scan_uuid: json!("uuid-1"),
            evidence_revision: json!(7),
        })
    }

    fn pages(&self, each: &mut dyn FnMut(ScanPage)) -> Result<(), String> {
        for p in self.pages.as_array().unwrap() {
            let s = |i: usize| p[i].as_str().map(String::from);
            each(ScanPage {
                url: s(0).unwrap(),
                status_code: p[1].as_i64(),
                meta_robots: s(2),
                x_robots: s(3),
            });
        }
        Ok(())
    }
}

struct JsonScans(Value);

impl ScanOpener for JsonScans {
    fn open(&self, path: &str) -> Result<Box<dyn ScanSource>, String> {
        let name = path.strip_prefix("scan:").unwrap_or(path);
        match self.0["scans"].get(name) {
            Some(kwargs) => Ok(Box::new(JsonScan {
                kwargs: kwargs.clone(),
                pages: self.0["pages"].clone(),
            })),
            None => Err(format!("scan not found: {path}")),
        }
    }
}

#[test]
fn joins_match_for_every_view_and_scan_state() {
    let cases = load("join_cases.json");
    let opener = JsonScans(cases.clone());
    for case in cases["cases"].as_array().unwrap() {
        let store = fixtures().join(format!("join_{}.sqlite", case["store"].as_str().unwrap()));
        let extra = &case["extra"];
        let q = JoinQuery {
            view: case["view"].as_str().unwrap().into(),
            base_url: text(extra, "base_url"),
            bot: text(extra, "bot"),
            family: text(extra, "family"),
            date_from: text(extra, "date_from"),
            date_to: text(extra, "date_to"),
            limit: int(extra, "limit").unwrap_or(100),
            offset: int(extra, "offset").unwrap_or(0),
        };
        let scan = format!("scan:{}", case["scan"].as_str().unwrap());
        let actual = join_log_scan(&store.to_string_lossy(), &scan, &opener, &q);
        let label = format!(
            "{} {} {} {extra}",
            case["store"], case["scan"], case["view"]
        );
        assert_same(&label, &case["expected"], &actual, &["/source", "/scan"]);
    }
}

#[test]
fn bot_evidence_steps_match_including_the_persisted_cache() {
    let cases = load("bots_cases.json");
    let tmp = tempfile::tempdir().unwrap();
    let work = tmp.path().join("work.sqlite");
    std::fs::copy(fixtures().join("bots_initial.sqlite"), &work).unwrap();
    let source = work.to_string_lossy().into_owned();
    for step in cases["steps"].as_array().unwrap() {
        let q = &step["query"];
        let resolver = Arc::new(FakeResolver::standard());
        let v = verifier(Arc::clone(&resolver));
        let query = BotQuery {
            verify_bots: q
                .get("verify_bots")
                .and_then(Value::as_bool)
                .unwrap_or(false),
            limit: int(q, "limit").unwrap_or(100),
            offset: int(q, "offset").unwrap_or(0),
            bot: text(q, "bot"),
            family: text(q, "family"),
            ip: text(q, "ip"),
            verdict: text(q, "verdict"),
            max_dns: int(q, "max_dns").unwrap_or(20),
        };
        let now = common::QuickClock;
        let now = spiderhead_log_analyzer::dns::Clock::wall(&now);
        let label = step["label"].to_string();
        match bot_verification(&source, &query, &v, now) {
            Ok(actual) => {
                let want = &step["expected"]["value"];
                assert_same(&label, want, &actual, &["/source", "/rows/*/checked_at"]);
            }
            Err(e) => assert_eq!(step["expected"]["message"], json!(e.to_string()), "{label}"),
        }
        if step["lookups"].is_array() {
            assert_eq!(json!(resolver.lookups()), step["lookups"], "{label}");
        }
    }
    let con = Connection::open(&source).unwrap();
    let mut rows = query_rows(
        &con,
        "SELECT ip,bot,payload FROM dns_cache ORDER BY bot,ip",
        &[],
    )
    .unwrap();
    for r in &mut rows {
        let mut payload: Value = serde_json::from_str(r["payload"].as_str().unwrap()).unwrap();
        payload.as_object_mut().unwrap().remove("checked_at");
        r.insert("payload".into(), payload);
    }
    let actual: Vec<Value> = rows.into_iter().map(Value::Object).collect();
    assert_same("cache rows", &cases["cache_rows"], &json!(actual), &[]);
}
