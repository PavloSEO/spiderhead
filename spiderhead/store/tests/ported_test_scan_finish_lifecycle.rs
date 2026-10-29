//! `finished_at` stamping for drained frontiers.
//!
//! reference builds each scan with `NativeScan.create(path, **_metadata())`. Here every test copies
//! the reference-created running scan fixture `commit-pre.sqlite` into its own temp directory, and
//! page records come from the first `commit` record of the commit oracle fixture
//! `commit-expected.json` (with the unknown-field probe removed).

use std::path::{Path, PathBuf};

use rusqlite::{Connection, OpenFlags};
use serde_json::{json, Map, Value};
use spiderhead_store::error::StoreError;
use spiderhead_store::native_scan::NativeScan;
use spiderhead_store::scan_lease::{Lease, NativeScanLease, PageCommit};
use spiderhead_store::scan_schema::NativeScanSchemaMixin;

const ROOT: &str = "https://example.test/";
const FINALIZATION_TIMEOUT: f64 = 10.0;
const BLOCKED_TIMEOUT: f64 = 0.1;

fn fixtures() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures")
}

/// Removes the per-test scratch directory when the test ends, pass or fail.
struct Scratch(PathBuf);

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// Fresh running scan, the Rust counterpart of `NativeScan.create(path, **_metadata())`.
fn fresh(name: &str) -> (Scratch, PathBuf) {
    let dir = std::env::temp_dir().join(format!("sh-finish-{}-{name}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("scan.sqlite");
    std::fs::copy(fixtures().join("commit-pre.sqlite"), &path).unwrap();
    (Scratch(dir), path)
}

fn writer(path: &Path) -> NativeScan {
    NativeScanSchemaMixin::open(path, None, None, None, false).unwrap()
}

/// reference `_record(url)`: a committed page record for `url` at its crawl depth.
fn record(url: &str) -> Map<String, Value> {
    let fixture: Value = serde_json::from_str(
        &std::fs::read_to_string(fixtures().join("commit-expected.json")).unwrap(),
    )
    .unwrap();
    let op = fixture["a"]["ops"]
        .as_array()
        .unwrap()
        .iter()
        .find(|o| o["op"] == "commit")
        .unwrap();
    let mut rec = op["record"].as_object().cloned().unwrap();
    rec.remove("bogus");
    rec.insert("url".into(), json!(url));
    rec.insert("content_type".into(), json!("text/html"));
    rec.insert("title".into(), json!("Home"));
    rec.insert("crawl_depth".into(), json!(if url == ROOT { 0 } else { 1 }));
    rec
}

/// reference `_runtime()`.
fn runtime() -> Map<String, Value> {
    json!({
        "max_depth_reached": 1,
        "elapsed_seconds": 0.0,
        "circuit_timeout_streak": 0,
        "circuit_server_error_streak": 0,
        "crawl_delay_applied": null,
        "throttle": {"delay_seconds": 0.0, "concurrency": 1, "consecutive_ok": 0},
    })
    .as_object()
    .cloned()
    .unwrap()
}

/// reference `scan.enqueue([(url, depth)])` with the default reason.
fn enqueue(scan: &NativeScan, entries: &[(&str, i64)]) {
    let entries: Vec<(String, i64)> = entries.iter().map(|(u, d)| (u.to_string(), *d)).collect();
    scan.enqueue(&entries, "enqueue").unwrap();
}

/// reference `scan.claim(1)[0]`.
fn claim_one(scan: &NativeScan) -> Lease {
    scan.claim(1).unwrap().remove(0)
}

/// reference `scan.commit_page(lease, _record(url), runtime=_runtime(), partial_reasons=...)`.
fn commit(scan: &mut NativeScan, lease: Lease, url: &str, partial_reasons: &[&str]) {
    let commit = PageCommit {
        lease: Some(lease),
        record: record(url),
        runtime: Some(runtime()),
        partial_reasons: partial_reasons.iter().map(|r| r.to_string()).collect(),
        ..Default::default()
    };
    scan.commit_page(&commit).unwrap();
}

/// reference `_header(path)`: the scan row as the reader sees it.
#[derive(Debug)]
struct Header {
    lifecycle: String,
    finish_reason: Option<String>,
    finished_at: Option<String>,
    crawl_partial: i64,
}

fn header(path: &Path) -> Header {
    let con = Connection::open_with_flags(path, OpenFlags::SQLITE_OPEN_READ_ONLY).unwrap();
    con.query_row(
        "SELECT lifecycle, finish_reason, finished_at, crawl_partial FROM scan WHERE singleton=1",
        [],
        |r| {
            Ok(Header {
                lifecycle: r.get(0)?,
                finish_reason: r.get(1)?,
                finished_at: r.get(2)?,
                crawl_partial: r.get(3)?,
            })
        },
    )
    .unwrap()
}

/// reference `reader = sqlite3.connect(..., mode=ro); reader.execute("BEGIN")` plus a read of
/// `pages`: a read snapshot that blocks the checkpoint the finalizer needs.
fn hold_read_snapshot(path: &Path) -> Connection {
    let reader = Connection::open_with_flags(path, OpenFlags::SQLITE_OPEN_READ_ONLY).unwrap();
    reader.execute_batch("BEGIN").unwrap();
    let _: i64 = reader
        .query_row("SELECT COUNT(*) FROM pages", [], |r| r.get(0))
        .unwrap();
    reader
}

fn assert_immutable(result: Result<bool, StoreError>) {
    match result {
        Err(StoreError::ScanError(e)) => assert!(
            e.message.contains("immutable"),
            "unexpected message: {}",
            e.message
        ),
        other => panic!("expected an immutable-lifecycle error, got {other:?}"),
    }
}

#[test]
fn empty_queue_finishes_with_a_timestamp() {
    let (_dir, path) = fresh("empty-queue");
    {
        let mut scan = writer(&path);
        assert!(scan
            .finish_capture("finished", FINALIZATION_TIMEOUT)
            .unwrap());
        assert_immutable(scan.finish_capture("finished", FINALIZATION_TIMEOUT));
        scan.close();
    }
    let h = header(&path);
    assert_eq!(h.lifecycle, "finished");
    assert_eq!(h.finish_reason.as_deref(), Some("finished"));
    assert!(h.finished_at.is_some());
}

#[test]
fn drained_final_batch_finishes_exactly_once() {
    let (_dir, path) = fresh("drained-final");
    {
        let mut scan = writer(&path);
        enqueue(&scan, &[(ROOT, 0)]);
        let lease = claim_one(&scan);
        commit(&mut scan, lease, ROOT, &[]);
        assert!(scan
            .finish_capture("finished", FINALIZATION_TIMEOUT)
            .unwrap());
        scan.close();
    }
    let h = header(&path);
    assert_eq!(h.lifecycle, "finished");
    assert!(h.finished_at.is_some());
    assert_eq!(h.crawl_partial, 0);
}

#[test]
fn drained_but_partial_evidence_still_finishes() {
    // Issue #712: omitted observations must not leave finished_at empty.
    let (_dir, path) = fresh("drained-partial");
    {
        let mut scan = writer(&path);
        enqueue(&scan, &[(ROOT, 0)]);
        let lease = claim_one(&scan);
        commit(&mut scan, lease, ROOT, &["link_observations_omitted"]);
        let partial: i64 = scan
            .con()
            .query_row("SELECT crawl_partial FROM scan", [], |r| r.get(0))
            .unwrap();
        assert_eq!(partial, 1);
        assert!(scan
            .finish_capture("finished", FINALIZATION_TIMEOUT)
            .unwrap());
        scan.close();
    }
    let h = header(&path);
    assert_eq!(h.lifecycle, "finished");
    assert_eq!(h.crawl_partial, 1);
    assert!(h.finished_at.is_some());
}

#[test]
fn exclusion_only_frontier_is_a_drained_queue() {
    let (_dir, path) = fresh("exclusion-only");
    {
        let mut scan = writer(&path);
        let seed = json!({
            "requested_url": "https://example.test/a",
            "frontier_url": "https://example.test/a",
            "depth": 0,
            "reason": "outside_host",
            "source": "sitemap",
            "reserve_query": true,
            "seed": true,
        });
        scan.seed_frontier(&[seed.as_object().cloned().unwrap()])
            .unwrap();
        assert!(scan
            .finish_capture("finished", FINALIZATION_TIMEOUT)
            .unwrap());
        scan.close();
    }
    let h = header(&path);
    assert_eq!(h.lifecycle, "finished");
    assert!(h.finished_at.is_some());
}

#[test]
fn queued_or_inflight_work_never_stamps_finished() {
    let (_dir, path) = fresh("queued-inflight");
    {
        let mut scan = writer(&path);
        enqueue(&scan, &[(ROOT, 0), ("https://example.test/next", 1)]);
        let lease = claim_one(&scan);
        commit(&mut scan, lease, ROOT, &[]);
        scan.interrupt("operator_cancelled").unwrap();
        assert!(scan
            .finish_capture("interrupted", FINALIZATION_TIMEOUT)
            .unwrap());
        scan.close();
    }
    let h = header(&path);
    assert_eq!(h.lifecycle, "interrupted");
    assert_eq!(h.finish_reason.as_deref(), Some("interrupted"));
    assert!(h.finished_at.is_none());
}

#[test]
fn abort_before_any_accepted_work_is_not_finished() {
    // An empty frontier after an error stop is not a drained queue.
    let (_dir, path) = fresh("abort-empty");
    {
        let mut scan = writer(&path);
        scan.interrupt("robots.txt unavailable").unwrap();
        assert!(scan
            .finish_capture("robots_unavailable", FINALIZATION_TIMEOUT)
            .unwrap());
        scan.close();
    }
    let h = header(&path);
    assert_eq!(h.lifecycle, "interrupted");
    assert_eq!(h.finish_reason.as_deref(), Some("robots_unavailable"));
    assert!(h.finished_at.is_none());
}

#[test]
fn error_stop_after_the_last_page_is_not_finished() {
    // A drained queue after an error circuit is a stopped capture, not a finished one.
    let (_dir, path) = fresh("error-stop");
    {
        let mut scan = writer(&path);
        enqueue(&scan, &[(ROOT, 0)]);
        let lease = claim_one(&scan);
        commit(&mut scan, lease, ROOT, &[]);
        scan.interrupt("origin stopped responding or refused repeatedly")
            .unwrap();
        assert!(scan.finish_capture("errors", FINALIZATION_TIMEOUT).unwrap());
        scan.close();
    }
    let h = header(&path);
    assert_eq!(h.lifecycle, "interrupted");
    assert_eq!(h.finish_reason.as_deref(), Some("errors"));
    assert!(h.finished_at.is_none());
}

#[test]
fn inflight_only_frontier_is_not_finished() {
    let (_dir, path) = fresh("inflight-only");
    {
        let mut scan = writer(&path);
        enqueue(&scan, &[(ROOT, 0)]);
        let _ = claim_one(&scan);
        scan.interrupt("operator_cancelled").unwrap();
        assert!(scan
            .finish_capture("interrupted", FINALIZATION_TIMEOUT)
            .unwrap());
        scan.close();
    }
    let h = header(&path);
    assert_eq!(h.lifecycle, "interrupted");
    assert!(h.finished_at.is_none());
}

#[test]
fn resume_or_finalize_preserves_an_aborted_empty_capture() {
    // Reopening an aborted run must finalize the file, not stamp it finished.
    let (_dir, path) = fresh("resume-aborted");
    {
        let mut scan = writer(&path);
        scan.interrupt("robots.txt unavailable").unwrap();
        assert!(scan
            .finish_capture("robots_unavailable", FINALIZATION_TIMEOUT)
            .unwrap());
        scan.close();
    }
    {
        let scan = writer(&path);
        assert!(scan.resume_or_finalize().unwrap());
    }
    let h = header(&path);
    assert_eq!(h.lifecycle, "interrupted");
    assert_eq!(h.finish_reason.as_deref(), Some("robots_unavailable"));
    assert!(h.finished_at.is_none());
}

#[test]
fn resume_or_finalize_preserves_a_stopped_drained_capture() {
    // An error stop with nothing left to requeue stays interrupted on reopen.
    let (_dir, path) = fresh("resume-stopped");
    {
        let mut scan = writer(&path);
        enqueue(&scan, &[(ROOT, 0)]);
        let lease = claim_one(&scan);
        commit(&mut scan, lease, ROOT, &[]);
        scan.interrupt("origin stopped responding or refused repeatedly")
            .unwrap();
        assert!(scan.finish_capture("errors", FINALIZATION_TIMEOUT).unwrap());
        scan.close();
    }
    {
        let scan = writer(&path);
        assert!(scan.resume_or_finalize().unwrap());
    }
    let h = header(&path);
    assert_eq!(h.lifecycle, "interrupted");
    assert_eq!(h.finish_reason.as_deref(), Some("errors"));
    assert!(h.finished_at.is_none());
}

#[test]
fn recovery_after_interrupt_finishes_when_the_queue_drains() {
    let (_dir, path) = fresh("recovery");
    {
        let mut scan = writer(&path);
        enqueue(&scan, &[(ROOT, 0), ("https://example.test/next", 1)]);
        let lease = claim_one(&scan);
        commit(&mut scan, lease, ROOT, &[]);
        scan.interrupt("operator_cancelled").unwrap();
        scan.close();
    }
    assert_eq!(header(&path).lifecycle, "interrupted");
    {
        let mut scan = writer(&path);
        scan.begin_collection(true).unwrap();
        let lease = claim_one(&scan);
        commit(&mut scan, lease, "https://example.test/next", &[]);
        assert!(scan
            .finish_capture("finished", FINALIZATION_TIMEOUT)
            .unwrap());
        scan.close();
    }
    let h = header(&path);
    assert_eq!(h.lifecycle, "finished");
    assert!(h.finished_at.is_some());
}

#[test]
fn blocked_finalization_still_never_stamps_finished() {
    let (_dir, path) = fresh("blocked-finalize");
    {
        let mut scan = writer(&path);
        enqueue(&scan, &[(ROOT, 0)]);
        let lease = claim_one(&scan);
        commit(&mut scan, lease, ROOT, &[]);
        let reader = hold_read_snapshot(&path);
        let finished = scan.finish_capture("finished", BLOCKED_TIMEOUT).unwrap();
        drop(reader);
        assert!(!finished);
        assert!(scan.resume_or_finalize().unwrap());
        scan.close();
    }
    let h = header(&path);
    assert_eq!(h.lifecycle, "finished");
    assert!(h.finished_at.is_some());
}

#[test]
fn blocked_finalization_preserves_an_explicit_error_stop() {
    // A reader-blocked checkpoint must not rewrite 'errors' into a retryable
    // 'finalization_blocked' that a later retry reads as a drained queue.
    let (_dir, path) = fresh("blocked-errors");
    {
        let mut scan = writer(&path);
        enqueue(&scan, &[(ROOT, 0)]);
        let lease = claim_one(&scan);
        commit(&mut scan, lease, ROOT, &[]);
        scan.interrupt("origin stopped responding").unwrap();
        let reader = hold_read_snapshot(&path);
        let finished = scan.finish_capture("errors", BLOCKED_TIMEOUT).unwrap();
        drop(reader);
        assert!(!finished);
        let h = header(&path);
        assert_eq!(h.lifecycle, "interrupted");
        assert_eq!(h.finish_reason.as_deref(), Some("errors"));
        assert!(h.finished_at.is_none());
        assert!(scan.resume_or_finalize().unwrap());
        scan.close();
    }
    let h = header(&path);
    assert_eq!(h.lifecycle, "interrupted");
    assert_eq!(h.finish_reason.as_deref(), Some("errors"));
    assert!(h.finished_at.is_none());
    assert_eq!(h.crawl_partial, 1);
}

#[test]
fn blocked_finish_without_audit_preserves_an_explicit_stop() {
    let (_dir, path) = fresh("blocked-no-audit");
    {
        let mut scan = writer(&path);
        enqueue(&scan, &[(ROOT, 0)]);
        let lease = claim_one(&scan);
        commit(&mut scan, lease, ROOT, &[]);
        scan.interrupt("origin stopped responding").unwrap();
        let reader = hold_read_snapshot(&path);
        let finished = scan
            .finish_without_audit("capture_finished_no_audit", BLOCKED_TIMEOUT)
            .unwrap();
        drop(reader);
        assert!(!finished);
        scan.close();
    }
    let h = header(&path);
    assert_eq!(h.lifecycle, "interrupted");
    assert_eq!(
        h.finish_reason.as_deref(),
        Some("origin stopped responding")
    );
    assert!(h.finished_at.is_none());
}
