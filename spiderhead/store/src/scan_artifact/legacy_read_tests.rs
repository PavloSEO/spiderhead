//! Tests of the read-only scan.v1 reader over artifacts of imported legacy runs.
//!
//! The fixture under `testdata/legacy-run` is one offline crawl (`audit.json`, `pages.jsonl`,
//! `links.jsonl`) and the scan.v1 artifact that importing it produced. The importer is gone;
//! the committed artifacts stand in for every run imported before its removal, and the reader
//! must keep accepting exactly those bytes.

use std::fs;
use std::path::{Path, PathBuf};

use regex::Regex;
use rusqlite::Connection;

use super::*;

const BUILD: &str = "25fd2ed032a31d63c5811722619e35c14b631476";

fn fixture() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("testdata/legacy-run")
}

/// Fresh scratch directory for one test.
fn scratch(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("spiderhead-w2s1-{}-{name}", std::process::id()));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).expect("scratch");
    dir
}

/// A writable copy of the committed artifact of the imported legacy run.
fn artifact(dir: &Path) -> PathBuf {
    let path = dir.join("scan.sqlite");
    fs::copy(fixture().join("scan.sqlite"), &path).expect("copy fixture");
    path
}

fn mutate(path: &Path, statement: &str) {
    let con = Connection::open(path).expect("open rw");
    // Foreign keys stay off, which the damage cases rely on.
    con.execute_batch("PRAGMA foreign_keys=OFF")
        .expect("pragma");
    con.execute_batch(statement).expect("mutation");
}

#[track_caller]
fn refused<T>(result: Result<T, StoreError>, pattern: &str) {
    match result {
        Ok(_) => panic!("expected a refusal matching {pattern:?}"),
        Err(error) => {
            let text = error.to_string();
            assert!(
                Regex::new(pattern).expect("pattern").is_match(&text),
                "{text:?} does not match {pattern:?}"
            );
        }
    }
}

// The committed artifact opens with the full validation, and the original
// audit bytes, fields, occurrences and producer survive in it.
#[test]
fn audit_bytes_and_producer_survive() {
    let dir = scratch("bytes");
    let path = artifact(&dir);
    let con = open_scan(&path, true, None).expect("open");
    let raw = fs::read(fixture().join("audit.json")).expect("audit");
    let (saved, sha_saved, revision): (String, String, String) = con
        .query_row(
            "SELECT document_json, sha256, analyzer_revision FROM audit",
            [],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        )
        .expect("audit row");
    assert_eq!(saved.as_bytes(), raw.as_slice());
    assert_eq!(sha_saved, sha(&String::from_utf8(raw).expect("utf-8")));
    assert_eq!(revision, BUILD);
    let application: i64 = con
        .query_row("PRAGMA application_id", [], |r| r.get(0))
        .unwrap();
    let version: i64 = con
        .query_row("PRAGMA user_version", [], |r| r.get(0))
        .unwrap();
    assert_eq!((application, version), (APPLICATION_ID, 1));
    for table in [
        "bodies",
        "documents",
        "responses",
        "frontier",
        "resume_state",
    ] {
        let n: i64 = con
            .query_row(&format!("SELECT COUNT(*) FROM {table}"), [], |r| r.get(0))
            .unwrap();
        assert_eq!(n, 0, "{table}");
    }
    let _ = fs::remove_dir_all(dir);
}

// Reads are read-only and never re-save the artifact.
#[test]
fn reads_are_read_only_and_do_not_resave() {
    let dir = scratch("readonly");
    let path = artifact(&dir);
    let before = fs::read(&path).unwrap();
    let con = open_scan(&path, true, None).unwrap();
    let error = con
        .execute("UPDATE pages SET title = 'changed'", [])
        .unwrap_err();
    assert!(error.to_string().contains("readonly"), "{error}");
    drop(con);
    assert_eq!(fs::read(&path).unwrap(), before);
    let _ = fs::remove_dir_all(dir);
}

// Inconsistent or unsupported artifacts are refused.
#[test]
fn inconsistent_or_unsupported_artifact_is_refused() {
    let cases = [
        ("PRAGMA user_version=2", "user_version"),
        ("PRAGMA application_id=1", "application_id"),
        ("DROP TABLE pages", "schema differs"),
        (
            "CREATE VIEW surprise AS SELECT load_extension('evil')",
            "schema differs",
        ),
        ("CREATE VIEW sqliteXsurprise AS SELECT 1", "schema differs"),
        (
            "CREATE TRIGGER surprise AFTER INSERT ON pages BEGIN SELECT 1; END",
            "schema differs",
        ),
        ("UPDATE audit SET sha256=printf('%064d', 0)", "hash"),
        ("UPDATE scan SET evidence_revision=2", "revision"),
        ("UPDATE scan SET config_fingerprint='wrong'", "fingerprint"),
        ("UPDATE links SET destination_url_id=999999", "foreign-key"),
        (
            "DELETE FROM pages WHERE page_ordinal=1",
            "foreign-key|populations",
        ),
        ("UPDATE links SET rel_json='{}'", "string tokens"),
        (
            "INSERT INTO bodies VALUES(printf('%064d',0),'identity',1,1,x'61')",
            "point-A",
        ),
    ];
    for (index, (mutation, message)) in cases.iter().enumerate() {
        let dir = scratch(&format!("refuse{index}"));
        let path = artifact(&dir);
        mutate(&path, mutation);
        let before = fs::read(&path).unwrap();
        refused(open_scan(&path, true, None), message);
        assert_eq!(fs::read(&path).unwrap(), before, "{mutation}");
        let _ = fs::remove_dir_all(dir);
    }
}

// Metadata with wrong types or order is refused.
#[test]
fn false_metadata_types_and_order_are_refused() {
    let cases = [
        ("UPDATE scan SET lifecycle='running'", "lifecycle"),
        ("UPDATE scan SET evidence_version='crawl.v99'", "evidence version"),
        ("UPDATE scan SET corpus_partial=0", "corpus"),
        ("UPDATE pages SET page_ordinal=100 WHERE page_ordinal=0", "ordinals"),
        ("UPDATE links SET ordinal=100 WHERE link_id=1", "ordinals"),
        ("UPDATE pages SET size_bytes=-1", "negative"),
        ("UPDATE pages SET status_code='hello'", "holds a text value"),
        ("UPDATE pages SET response_time=1e999", "holds a real value"),
        ("UPDATE scan SET limitations_json='{}'", "string list"),
        (
            "UPDATE context_items SET payload_json='{\"source_format\":null,\"inputs\":\"wrong\",\"recovered_truncated_final_line\":\"yes\",\"resume_eligible\":false}'",
            "provenance",
        ),
    ];
    for (index, (mutation, message)) in cases.iter().enumerate() {
        let dir = scratch(&format!("meta{index}"));
        let path = artifact(&dir);
        mutate(&path, mutation);
        refused(open_scan(&path, true, None), message);
        let _ = fs::remove_dir_all(dir);
    }
}

// Legacy lineage and unavailable resume state must agree.
#[test]
fn legacy_lineage_and_unavailable_resume_cannot_disagree() {
    for (index, mutation) in [
        "UPDATE scan SET start_url='https://wrong.example/'",
        "UPDATE scan SET parent_scan_uuid='00000000-0000-0000-0000-000000000000'",
        "UPDATE context_items SET reason='resume complete'",
    ]
    .iter()
    .enumerate()
    {
        let dir = scratch(&format!("lineage{index}"));
        let path = artifact(&dir);
        mutate(&path, mutation);
        refused(open_scan(&path, true, None), "lineage|resume unavailable");
        let _ = fs::remove_dir_all(dir);
    }
}

// The reader rejects negative page counts.
#[test]
fn reader_rejects_mutated_negative_page_counts() {
    for field in PAGE_NONNEGATIVE_INTS {
        let dir = scratch(&format!("neg-read-{field}"));
        let path = artifact(&dir);
        mutate(
            &path,
            &format!("UPDATE pages SET {field}=-1 WHERE page_ordinal=0"),
        );
        refused(
            open_scan(&path, true, None),
            "invalid negative PageRecord count",
        );
        let _ = fs::remove_dir_all(dir);
    }
}

// Legacy imports are not eligible for light open.
#[test]
fn legacy_import_is_not_light_eligible() {
    let dir = scratch("not-light");
    let path = artifact(&dir);
    let (_con, mode) = open_scan_mode(&path, false, None, true).unwrap();
    assert_eq!(mode, "full");
    let _ = fs::remove_dir_all(dir);
}

// Every current page and link field is mapped in the schema (storage side).
#[test]
fn schema_page_columns_cover_the_record_keys() {
    let columns: std::collections::BTreeSet<String> = expected().unwrap().columns["pages"]
        .iter()
        .map(|c| c.name.clone())
        .collect();
    assert!(columns.is_disjoint(
        &["render_blocking".to_owned(), "attempts".to_owned()]
            .into_iter()
            .collect()
    ));
    for name in PAGE_NONNEGATIVE_INTS {
        assert!(columns.contains(*name), "{name}");
    }
    for (name, _) in LATE_PAGE_FIELDS {
        let json_column = format!("{name}_json");
        assert!(
            columns.contains(*name) || columns.contains(&json_column),
            "{name}"
        );
    }
}

// The format is readable without the crate tools.
#[test]
fn format_is_readable_with_plain_sqlite() {
    let dir = scratch("plain");
    let path = artifact(&dir);
    let con =
        Connection::open_with_flags(&path, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY).unwrap();
    let version: i64 = con
        .query_row("PRAGMA user_version", [], |r| r.get(0))
        .unwrap();
    let pages: i64 = con
        .query_row("SELECT COUNT(*) FROM pages", [], |r| r.get(0))
        .unwrap();
    assert_eq!((version, pages), (1, 2));
    let _ = fs::remove_dir_all(dir);
}
