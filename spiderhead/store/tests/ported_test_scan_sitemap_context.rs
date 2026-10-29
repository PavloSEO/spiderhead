//!
//! Ported: the collection guard and the invalid-root diagnostic. Deferred (see the report):
//! the discovery replay test, which patches reference `scan_sitemaps` internals.

use std::fs;
use std::path::{Path, PathBuf};

use rusqlite::Connection;
use serde_json::{json, Map, Value};
use spiderhead_core::settings_data;
use spiderhead_store::native_scan::NativeScan;
use spiderhead_store::scan_schema::{crawl_fingerprint, NativeScanSchemaMixin};

const ROOT_A: &str = "https://example.test/a.xml";

fn scratch(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "spiderhead-tests-24-sitemap-context-{}-{name}",
        std::process::id()
    ));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).expect("scratch dir");
    dir
}

/// reference `_metadata()` with `speed.min_delay_seconds` set to 0.
fn create_scan(path: &Path, initial: &[(String, String)]) -> NativeScan {
    let mut config = settings_data::defaults().clone();
    config["speed"]["min_delay_seconds"] = json!(0);
    let config: Map<String, Value> = config.as_object().cloned().expect("config object");
    let fingerprint = crawl_fingerprint(&config).expect("fingerprint");
    let versions: Map<String, Value> = ["python", "sqlite", "httpx", "lxml", "beautifulsoup4"]
        .iter()
        .map(|k| ((*k).to_owned(), json!("test")))
        .collect();
    NativeScanSchemaMixin::create(
        path,
        "https://example.test/",
        &config,
        Some(&fingerprint),
        "3.0.0",
        &"a".repeat(40),
        &versions,
        &[],
        None,
        initial,
        "scan.v1",
    )
    .expect("create scan")
}

fn explicit_root() -> Vec<(String, String)> {
    vec![(ROOT_A.to_owned(), "explicit".to_owned())]
}

#[test]
fn collection_cannot_erase_an_unfetched_selected_root() {
    let dir = scratch("collection");
    let scan = create_scan(&dir.join("scan.sqlite"), &explicit_root());
    scan.begin_collection(true).expect("begin collection");
    // reference reads `resume_snapshot()["scan"]["crawl_partial"]`; the Rust store has no
    // resume snapshot, so the same persisted flag is read from the scan row.
    let partial: i64 = scan
        .con()
        .query_row(
            "SELECT crawl_partial FROM scan WHERE singleton=1",
            [],
            |r| r.get(0),
        )
        .expect("crawl_partial");
    assert_eq!(partial, 1);
    let _ = fs::remove_dir_all(&dir);
}

// (both parametrized cases: duplicate_root, overflow_id)
#[test]
fn invalid_persisted_selected_root_has_scan_diagnostic() {
    for mutation in ["duplicate_root", "overflow_id"] {
        let dir = scratch(mutation);
        let path = dir.join("scan.sqlite");
        drop(create_scan(&path, &explicit_root()));

        let con = Connection::open(&path).expect("open scan for mutation");
        let stored: String = con
            .query_row(
                "SELECT payload_json FROM context_items WHERE kind='sitemap_declaration'",
                [],
                |r| r.get(0),
            )
            .expect("declaration payload");
        let payload: Value = serde_json::from_str(&stored).expect("payload json");
        if mutation == "duplicate_root" {
            let mut changed = payload.clone();
            changed["ordinal"] = json!(1);
            con.execute(
                "INSERT INTO context_items SELECT kind,'ordinal:1',payload_version,?,completeness,reason \
                 FROM context_items WHERE kind='sitemap_declaration'",
                [changed.to_string()],
            )
            .expect("insert duplicate root");
        } else {
            // 2**100 does not fit in serde_json's integer type, so the number is written
            // as a literal in the payload text.
            let text = format!(
                "{{\"ordinal\":{},\"source\":{},\"sitemap_url_id\":1267650600228229401496703205376}}",
                payload["ordinal"], payload["source"]
            );
            con.execute(
                "UPDATE context_items SET payload_json=? WHERE kind='sitemap_declaration'",
                [text],
            )
            .expect("overflow id");
        }
        drop(con);

        let err = NativeScanSchemaMixin::inspect(&path, None)
            .expect_err("invalid selected root must be refused");
        assert!(
            err.to_string().contains("sitemap"),
            "{mutation}: diagnostic should mention sitemap, got: {err}"
        );
        let _ = fs::remove_dir_all(&dir);
    }
}
