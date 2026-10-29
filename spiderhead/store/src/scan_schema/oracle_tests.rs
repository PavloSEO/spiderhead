//! Fixture tests for the scan schema: captured scan artifacts opened through the light and full
//! validation paths and compared with recorded status output through a structural JSON diff.
//! The diff tool sits above this crate, so the store crate cannot depend on it, even for tests.
//!
//! Functions owned by modules not implemented yet are replaced by [`PassThrough`]; switch a
//! test to the real [`Owners`](super::deps::Owners) once it is implemented.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use rusqlite::Connection;
use serde_json::{json, Map, Value};

use super::deps::{with_deps, SchemaDeps};
use super::{bad, NativeScanSchemaMixin};
use crate::error::StoreError;
use crate::native_scan::NativeScan;
use crate::scan_artifact::{open_scan_mode, query_maps};

/// Stand-in for functions of modules not implemented yet: accepts everything it cannot check.
pub(crate) struct PassThrough;

impl SchemaDeps for PassThrough {
    fn redact_config(&self, settings: &Value) -> Result<Value, StoreError> {
        Ok(settings.clone())
    }
    fn validate_recorded_credentials(&self, snapshot: &Value) -> Result<Value, StoreError> {
        Ok(snapshot.clone())
    }
    fn credential_verifier(&self, _: &Value, _: &str) -> Result<Option<String>, StoreError> {
        Ok(None)
    }
    fn validate_context(
        &self,
        _: &Connection,
        _: &Map<String, Value>,
        _: Option<&BTreeSet<i64>>,
    ) -> Result<(), StoreError> {
        Ok(())
    }
    fn validate_ledger(&self, _: &Connection) -> Result<(), StoreError> {
        Ok(())
    }
    fn validate_v2(&self, _: &Connection) -> Result<(), StoreError> {
        Ok(())
    }
    fn upgrade_to_v2(&self, _: &Connection) -> Result<(), StoreError> {
        Ok(())
    }
    fn sitemap_root_ids(&self, _: &Connection) -> Result<BTreeSet<i64>, StoreError> {
        Ok(BTreeSet::new())
    }
    fn declare_sitemap(&self, _: &Connection, _: &str, _: &str, _: i64) -> Result<(), StoreError> {
        Ok(())
    }
    fn validate_corpus(
        &self,
        _: &Connection,
        _: &Map<String, Value>,
        _: &Map<String, Value>,
    ) -> Result<(), StoreError> {
        Ok(())
    }
    fn corpus_summary(
        &self,
        con: &Connection,
        _: &Map<String, Value>,
    ) -> Result<Value, StoreError> {
        // Echoes the saved header; the real owner derives it from the retained corpus.
        let (partial, saved): (i64, String) = con.query_row(
            "SELECT corpus_partial, capabilities_json FROM scan WHERE singleton=1",
            [],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )?;
        Ok(
            json!({"corpus_partial": partial != 0, "capabilities": serde_json::from_str::<Value>(&saved)?}),
        )
    }
    fn reanalysis_state(&self, con: &Connection) -> Result<String, StoreError> {
        // Agrees with whatever the artifact recorded; the real owner computes it from the corpus.
        let saved: String = con.query_row(
            "SELECT capabilities_json FROM scan WHERE singleton=1",
            [],
            |r| r.get(0),
        )?;
        let saved: Value = serde_json::from_str(&saved)?;
        Ok(saved["offline_reanalysis"]["state"]
            .as_str()
            .unwrap_or_default()
            .to_owned())
    }
    fn open_native(
        &self,
        _: &Path,
        _: Connection,
        _: Option<std::fs::File>,
    ) -> Result<NativeScan, StoreError> {
        Err(bad("NativeScan is not available without storage-pack-06"))
    }
    fn record_anonymous_session_resume(&self, _: &NativeScan, _: bool) -> Result<(), StoreError> {
        Ok(())
    }
}

pub(super) fn passthrough<T>(body: impl FnOnce() -> T) -> T {
    with_deps(Arc::new(PassThrough), body)
}

/// No diff options are needed here: the comparison is exact structural equality.
#[derive(Default)]
pub(crate) struct Options;

/// Pointers of every differing leaf between `expected` and `actual`.
pub(crate) fn diff(expected: &Value, actual: &Value, _: &Options) -> Vec<String> {
    fn walk(e: &Value, a: &Value, path: &str, out: &mut Vec<String>) {
        match (e, a) {
            (Value::Object(x), Value::Object(y)) => {
                for (k, v) in x {
                    match y.get(k) {
                        Some(w) => walk(v, w, &format!("{path}/{k}"), out),
                        None => out.push(format!("{path}/{k}: missing")),
                    }
                }
                for k in y.keys().filter(|k| !x.contains_key(*k)) {
                    out.push(format!("{path}/{k}: extra"));
                }
            }
            (Value::Array(x), Value::Array(y)) if x.len() == y.len() => {
                for (i, (v, w)) in x.iter().zip(y).enumerate() {
                    walk(v, w, &format!("{path}/{i}"), out);
                }
            }
            _ if e == a => {}
            _ => out.push(format!("{path}: {e} != {a}")),
        }
    }
    let mut out = Vec::new();
    walk(expected, actual, "", &mut out);
    out
}

const SITES: [&str; 6] = [
    "basic",
    "canonical-hreflang",
    "errors",
    "redirects",
    "robots-sitemap",
    "schema",
];

fn oracle_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../oracle")
}

fn raw_scan(site: &str) -> PathBuf {
    oracle_root()
        .join("raw-scans")
        .join(site)
        .join("scan.sqlite")
}

fn baseline(site: &str) -> Value {
    let text = std::fs::read_to_string(
        oracle_root()
            .join("baselines")
            .join(site)
            .join("scan-status.json"),
    )
    .expect("baseline");
    serde_json::from_str(&text).expect("baseline json")
}

/// The `source` block of `scan status` built from a header row (booleans for the partial flags,
/// timestamps and the scan uuid replaced by fixed placeholders).
fn status_source(header: &Map<String, Value>) -> Value {
    let pick = |key: &str| header.get(key).cloned().unwrap_or(Value::Null);
    let flag = |key: &str| Value::Bool(header[key].as_i64() != Some(0));
    let stamp = |key: &str| {
        if header[key].is_null() {
            Value::Null
        } else {
            json!("<TS>")
        }
    };
    json!({
        "corpus_partial": flag("corpus_partial"),
        "crawl_partial": flag("crawl_partial"),
        "created_at": stamp("created_at"),
        "evidence_revision": pick("evidence_revision"),
        "finish_reason": pick("finish_reason"),
        "finished_at": stamp("finished_at"),
        "format_version": pick("format_version"),
        "lifecycle": pick("lifecycle"),
        "parent_scan_uuid": pick("parent_scan_uuid"),
        "scan_uuid": "<UUID:1>",
        "source_kind": pick("source_kind"),
        "writer_revision": pick("writer_revision"),
        "writer_version": pick("writer_version"),
    })
}

// Expected `source` and `validation` blocks of the recorded status output.
#[test]
fn light_open_matches_scan_status_baseline() {
    for site in SITES {
        let expected = baseline(site);
        let (con, mode) = passthrough(|| open_scan_mode(&raw_scan(site), false, None, true))
            .unwrap_or_else(|e| panic!("{site}: {e}"));
        let header = query_maps(&con, "SELECT * FROM scan WHERE singleton=1")
            .unwrap()
            .remove(0);
        let actual = json!({"source": status_source(&header), "validation": mode});
        let wanted = json!({
            "source": expected["stdout"]["source"],
            "validation": expected["stdout"]["validation"],
        });
        let divergences = diff(&wanted, &actual, &Options);
        assert!(divergences.is_empty(), "{site}: {divergences:?}");
    }
}

// Expected `frontier.counts` of the recorded status output.
#[test]
fn observe_counts_match_scan_status_baseline() {
    for site in SITES {
        let expected = baseline(site);
        let observed = NativeScanSchemaMixin::observe(&raw_scan(site), 2.0).unwrap();
        assert_eq!(observed["validation"], "metadata_only");
        let counts = &observed["counts"];
        let actual = json!({
            "done": counts["done"], "excluded": counts["excluded"],
            "inflight": counts["inflight"], "queued": counts["queued"],
        });
        let divergences = diff(&expected["stdout"]["frontier"]["counts"], &actual, &Options);
        assert!(divergences.is_empty(), "{site}: {divergences:?}");
        let outcomes = &expected["stdout"]["committed_page_outcomes"];
        let pages: i64 = ["2xx", "3xx", "4xx", "5xx", "no_response", "other"]
            .iter()
            .map(|k| outcomes[*k].as_i64().unwrap())
            .sum();
        assert_eq!(counts["pages"].as_i64(), Some(pages), "{site}");
    }
}

// The full validation pass accepts every captured scan and reports the header
#[test]
fn full_inspection_accepts_every_raw_scan() {
    for site in SITES {
        let inspected = passthrough(|| NativeScanSchemaMixin::inspect(&raw_scan(site), None))
            .unwrap_or_else(|e| panic!("{site}: {e}"));
        let expected = baseline(site);
        let header = inspected["scan"].as_object().unwrap();
        let divergences = diff(
            &expected["stdout"]["source"],
            &status_source(header),
            &Options,
        );
        assert!(divergences.is_empty(), "{site}: {divergences:?}");
        let pages = inspected["counts"]["pages"].as_i64().unwrap();
        let frontier = inspected["counts"]["frontier"].as_i64().unwrap();
        assert!(frontier >= pages, "{site}");
    }
}
