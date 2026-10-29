//! Oracle parity for the retained-scan checks (sf-core-pack-03). Each
//! `tests/fixtures/native_checks/<name>/` dir holds a real reference-built
//! `NativeScan` database, the serialized `prepare_sitemap_reconciliation`
//! snapshot and the oracle's observable result. Regenerate with
//! the reference suite.

use std::collections::HashSet;
use std::path::{Path, PathBuf};

use serde_json::{json, Map, Value};
use spiderhead_sf_import::context::AuditContext;
use spiderhead_sf_import::loader::LoadedExports;
use spiderhead_sf_import::markup_rules::check_document_markup;
use spiderhead_sf_import::mobile_checks::check_mobile_render_layout;
use spiderhead_sf_import::native_sitemap::{run_retained_sitemap, SitemapReconciliation};
use spiderhead_sf_import::table::Table;

const START: &str = "https://example.test/";

fn fixture_dir(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/native_checks")
        .join(name)
}

struct FixtureReconciliation {
    data: Value,
}

impl FixtureReconciliation {
    fn objects(&self, key: &str) -> Vec<Map<String, Value>> {
        self.data
            .get(key)
            .and_then(Value::as_array)
            .map(|rows| rows.iter().filter_map(|r| r.as_object().cloned()).collect())
            .unwrap_or_default()
    }
}

impl SitemapReconciliation for FixtureReconciliation {
    fn has_names(&self) -> bool {
        self.data
            .get("has_names")
            .and_then(Value::as_bool)
            .unwrap_or(false)
    }
    fn available(&self) -> bool {
        self.data
            .get("available")
            .and_then(Value::as_bool)
            .unwrap_or(false)
    }
    fn reason(&self) -> String {
        self.data
            .get("reason")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_owned()
    }
    fn crawl_partial(&self) -> bool {
        self.data
            .get("crawl_partial")
            .and_then(Value::as_bool)
            .unwrap_or(false)
    }
    fn counts(&self) -> &Map<String, Value> {
        static EMPTY: std::sync::LazyLock<Map<String, Value>> = std::sync::LazyLock::new(Map::new);
        self.data
            .get("counts")
            .and_then(Value::as_object)
            .unwrap_or(&EMPTY)
    }
    fn summary(&self) -> Map<String, Value> {
        self.data
            .get("summary")
            .and_then(Value::as_object)
            .cloned()
            .unwrap_or_default()
    }
    fn iter_declared_facts(&self) -> Vec<Map<String, Value>> {
        self.objects("declared_facts")
    }
    fn iter_bucket(&self, name: &str) -> Vec<Value> {
        self.data
            .get("buckets")
            .and_then(|b| b.get(name))
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default()
    }
    fn count_bucket(&self, name: &str) -> i64 {
        self.data
            .get("bucket_counts")
            .and_then(|b| b.get(name))
            .and_then(Value::as_i64)
            .unwrap_or(0)
    }
    fn iter_missing_canonical_pages(&self) -> Vec<Map<String, Value>> {
        self.objects("missing_canonical")
    }
    fn iter_orphan_facts(&self) -> Vec<Map<String, Value>> {
        self.objects("orphan_facts")
    }
}

fn exports_from(expect: &Value) -> LoadedExports {
    let mut loaded = LoadedExports::default();
    if let Some(frames) = expect.get("exports").and_then(Value::as_object) {
        for (name, frame) in frames {
            let columns: Vec<String> = frame
                .get("columns")
                .and_then(Value::as_array)
                .map(|c| {
                    c.iter()
                        .map(|v| v.as_str().unwrap_or("").to_owned())
                        .collect()
                })
                .unwrap_or_default();
            let rows = frame
                .get("rows")
                .and_then(Value::as_array)
                .cloned()
                .unwrap_or_default()
                .into_iter()
                .map(|r| r.as_array().cloned().unwrap_or_default())
                .collect();
            loaded
                .frames
                .insert(name.clone(), Table::new(columns, rows));
        }
    }
    loaded
}

fn issue_json(i: &spiderhead_formats::models::Issue) -> Value {
    json!({
        "check": i.check,
        "target_url": i.target_url,
        "status_code": i.status_code,
        "details": i.details,
    })
}

fn snapshot(ctx: &AuditContext) -> Value {
    json!({
        "issues": ctx.issues_iter().iter().map(issue_json).collect::<Vec<_>>(),
        "skipped": ctx
            .skipped
            .iter()
            .map(|s| json!({"id": s.id, "reason": s.reason}))
            .collect::<Vec<_>>(),
    })
}

fn run(name: &str) {
    let dir = fixture_dir(name);
    let expect: Value = serde_json::from_str(
        &std::fs::read_to_string(dir.join("expect.json")).expect("expect.json"),
    )
    .expect("parse expect.json");
    let con =
        rusqlite::Connection::open(dir.join("scan.sqlite")).expect("open fixture scan.sqlite");
    let config = expect.get("config").cloned().unwrap_or_else(|| json!({}));
    let mut ctx =
        AuditContext::new(exports_from(&expect), config, None, false).expect("AuditContext");
    ctx.scan_con = Some(con);
    if let Some(su) = expect.get("skip_unsupported").filter(|v| !v.is_null()) {
        let set = |key: &str| -> HashSet<String> {
            su.get(key)
                .and_then(Value::as_array)
                .map(|a| {
                    a.iter()
                        .filter_map(|v| v.as_str().map(str::to_owned))
                        .collect()
                })
                .unwrap_or_default()
        };
        let available = set("available");
        let native = set("native");
        let av: HashSet<&str> = available.iter().map(String::as_str).collect();
        let nv: HashSet<&str> = native.iter().map(String::as_str).collect();
        ctx.skip_unsupported(&av, &nv);
    }
    let module = expect.get("module").and_then(Value::as_str).unwrap_or("");
    let mut got_summary = Value::Null;
    match module {
        "native_sitemap" => {
            let recon: Value = serde_json::from_str(
                &std::fs::read_to_string(dir.join("recon.json")).expect("recon.json"),
            )
            .expect("parse recon.json");
            let fixture = FixtureReconciliation { data: recon };
            let summary = run_retained_sitemap(&mut ctx, Some(&fixture), START)
                .expect("run_retained_sitemap");
            got_summary = Value::Object(summary);
        }
        "markup" => check_document_markup(&mut ctx).expect("check_document_markup"),
        "mobile" => check_mobile_render_layout(&mut ctx).expect("check_mobile_render_layout"),
        other => panic!("unknown module {other}"),
    }
    let got = snapshot(&ctx);
    for key in ["issues", "skipped"] {
        let got_v = got.get(key).cloned().unwrap_or(Value::Null);
        let want_v = expect.get(key).cloned().unwrap_or(Value::Null);
        assert_eq!(
            got_v,
            want_v,
            "{name}: {key} mismatch\ngot:  {}\nwant: {}",
            serde_json::to_string_pretty(&got_v).unwrap_or_default(),
            serde_json::to_string_pretty(&want_v).unwrap_or_default(),
        );
    }
    if let Some(want) = expect.get("summary") {
        assert_eq!(
            &got_summary,
            want,
            "{name}: summary mismatch\ngot:  {}\nwant: {}",
            serde_json::to_string_pretty(&got_summary).unwrap_or_default(),
            serde_json::to_string_pretty(want).unwrap_or_default(),
        );
    }
}

macro_rules! scenario {
    ($name:ident) => {
        #[test]
        fn $name() {
            run(stringify!($name));
        }
    };
}

scenario!(sitemap_contradictions);
scenario!(sitemap_partial);
scenario!(sitemap_partial_crawl);
scenario!(sitemap_robots_ua);
scenario!(sitemap_bounded);
scenario!(sitemap_entrypoint);
scenario!(markup_defective);
scenario!(markup_healthy);
scenario!(markup_schema_error);
scenario!(markup_pairing);
scenario!(markup_missing_context);
scenario!(markup_unparsed);
scenario!(mobile_defective);
scenario!(mobile_clean);
scenario!(mobile_partial);
scenario!(mobile_malformed);
scenario!(mobile_absent);
