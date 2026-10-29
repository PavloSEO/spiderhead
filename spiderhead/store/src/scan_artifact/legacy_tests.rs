//! Tests of the legacy `import_run` path and the read-only scan.v1 reader.
//!
//! The fixture under `testdata/legacy-run` is one offline crawl (`audit.json`, `pages.jsonl`,
//! `links.jsonl`) and the artifact that an import run produced from it; the import test compares
//! the Rust import against that artifact.

use std::fs;
use std::path::{Path, PathBuf};

use crate::scan_schema::oracle_tests::{diff, Options};
use regex::Regex;
use rusqlite::Connection;
use serde_json::{json, Value};

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

/// A writable copy of the legacy run directory.
fn legacy_run(dir: &Path) -> PathBuf {
    let run = dir.join("run");
    fs::create_dir_all(&run).expect("run dir");
    for name in ["audit.json", "pages.jsonl", "links.jsonl"] {
        fs::copy(fixture().join(name), run.join(name)).expect("copy fixture");
    }
    run
}

fn imported(dir: &Path) -> PathBuf {
    import_run(&legacy_run(dir), &dir.join("scan.sqlite"), BUILD, None).expect("import")
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

fn rewrite_pages(run: &Path, edit: impl Fn(&mut Value)) {
    let path = run.join("pages.jsonl");
    let text = fs::read_to_string(&path).expect("pages");
    let mut out = String::new();
    for (index, line) in text.lines().enumerate() {
        let mut row: Value = serde_json::from_str(line).expect("row");
        if index == 0 {
            edit(&mut row);
        }
        out.push_str(&serde_json::to_string(&row).expect("json"));
        out.push('\n');
    }
    fs::write(path, out).expect("write pages");
}

/// Every table of an artifact as JSON rows in rowid order (volatile header fields masked).
fn dump_tables(path: &Path) -> Value {
    let con = Connection::open(path).expect("open");
    let mut tables = serde_json::Map::new();
    let names: Vec<String> = con
        .prepare("SELECT name FROM sqlite_master WHERE type='table' AND name NOT LIKE 'sqlite_%' ORDER BY name")
        .and_then(|mut s| s.query_map([], |r| r.get(0)).and_then(Iterator::collect))
        .expect("tables");
    for name in names {
        let mut rows =
            query_maps(&con, &format!("SELECT * FROM \"{name}\" ORDER BY rowid")).expect("rows");
        if name == "scan" {
            for row in &mut rows {
                for key in ["scan_uuid", "created_at", "finished_at"] {
                    row.insert(key.into(), json!("<VOLATILE>"));
                }
            }
        }
        tables.insert(
            name,
            Value::Array(rows.into_iter().map(Value::Object).collect()),
        );
    }
    Value::Object(tables)
}

// Reference artifact: an import run over the same legacy run directory.
#[test]
fn import_run_matches_the_reference_artifact() {
    let dir = scratch("import");
    let rust = imported(&dir);
    let python = fixture().join("python-import.sqlite");
    let divergences = diff(&dump_tables(&python), &dump_tables(&rust), &Options);
    assert!(
        divergences.is_empty(),
        "{:#?}",
        &divergences[..divergences.len().min(10)]
    );
    let (con, mode) = open_scan_mode(&rust, true, None, false).expect("open");
    assert_eq!(mode, "full");
    drop(con);
    let _ = fs::remove_dir_all(dir);
}

// Original bytes, fields, occurrences and producer survive the import.
#[test]
fn audit_bytes_and_producer_survive() {
    let dir = scratch("bytes");
    let path = imported(&dir);
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
    let path = imported(&dir);
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
        let path = imported(&dir);
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
        ("UPDATE pages SET status_code='hello'", "scalar type"),
        ("UPDATE pages SET response_time=1e999", "non-finite"),
        ("UPDATE scan SET limitations_json='{}'", "string list"),
        (
            "UPDATE context_items SET payload_json='{\"source_format\":null,\"inputs\":\"wrong\",\"recovered_truncated_final_line\":\"yes\",\"resume_eligible\":false}'",
            "provenance",
        ),
    ];
    for (index, (mutation, message)) in cases.iter().enumerate() {
        let dir = scratch(&format!("meta{index}"));
        let path = imported(&dir);
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
        let path = imported(&dir);
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
        let path = imported(&dir);
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

// The import rejects negative page counts.
#[test]
fn import_rejects_negative_page_counts() {
    for field in PAGE_NONNEGATIVE_INTS {
        let dir = scratch(&format!("neg-import-{field}"));
        let run = legacy_run(&dir);
        rewrite_pages(&run, |row| row[*field] = json!(-1));
        refused(
            import_run(&run, &dir.join("out.sqlite"), BUILD, None),
            &format!(r"pages\.{field}: expected a nonnegative integer"),
        );
        let _ = fs::remove_dir_all(dir);
    }
}

// A missing input publishes no partial file.
#[test]
fn missing_input_does_not_publish_partial_file() {
    for name in ["pages.jsonl", "links.jsonl", "audit.json"] {
        let dir = scratch(&format!("missing-{name}"));
        let run = legacy_run(&dir);
        fs::remove_file(run.join(name)).unwrap();
        let target = dir.join("failed.sqlite");
        assert!(import_run(&run, &target, BUILD, None).is_err());
        assert!(!target.exists());
        let leftovers = fs::read_dir(&dir)
            .unwrap()
            .filter_map(Result::ok)
            .filter(|e| e.file_name().to_string_lossy().starts_with(".scan-import-"))
            .count();
        assert_eq!(leftovers, 0);
        let _ = fs::remove_dir_all(dir);
    }
}

// A truncated tail is recorded as partial without altering the audit.
#[test]
fn append_truncated_tail_records_partial_without_altering_audit() {
    use std::io::Write;
    let dir = scratch("truncated-tail");
    let run = legacy_run(&dir);
    fs::OpenOptions::new()
        .append(true)
        .open(run.join("links.jsonl"))
        .unwrap()
        .write_all(b"{\"source\":")
        .unwrap();
    let target = import_run(&run, &dir.join("partial.sqlite"), BUILD, None).unwrap();
    let con = open_scan(&target, true, None).unwrap();
    let (partial, limitations): (i64, String) = con
        .query_row(
            "SELECT crawl_partial, limitations_json FROM scan",
            [],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .unwrap();
    assert_eq!(partial, 1);
    assert!(limitations.contains("truncated final"));
    let saved: String = con
        .query_row("SELECT document_json FROM audit", [], |r| r.get(0))
        .unwrap();
    assert_eq!(
        saved.as_bytes(),
        fs::read(run.join("audit.json")).unwrap().as_slice()
    );
    let _ = fs::remove_dir_all(dir);
}

// A malformed middle row is never dropped silently.
#[test]
fn malformed_middle_row_is_not_silently_lost() {
    let dir = scratch("middle");
    let run = legacy_run(&dir);
    let path = run.join("links.jsonl");
    let mut bytes = b"{\"source\":\n".to_vec();
    bytes.extend(fs::read(&path).unwrap());
    fs::write(&path, bytes).unwrap();
    refused(
        import_run(&run, &dir.join("out.sqlite"), BUILD, None),
        "invalid JSON",
    );
    let _ = fs::remove_dir_all(dir);
}

// A second truncated record is refused.
#[test]
fn second_truncated_record_is_refused() {
    use std::io::Write;
    let dir = scratch("second-truncated");
    let run = legacy_run(&dir);
    for name in ["pages.jsonl", "links.jsonl"] {
        fs::OpenOptions::new()
            .append(true)
            .open(run.join(name))
            .unwrap()
            .write_all(b"{\"url\":")
            .unwrap();
    }
    refused(
        import_run(&run, &dir.join("out.sqlite"), BUILD, None),
        "only one truncated",
    );
    let _ = fs::remove_dir_all(dir);
}

// Duplicate JSON keys are not mistaken for truncation.
#[test]
fn duplicate_json_keys_are_not_mistaken_for_truncation() {
    use std::io::Write;
    let dir = scratch("duplicate-keys");
    let run = legacy_run(&dir);
    fs::OpenOptions::new()
        .append(true)
        .open(run.join("links.jsonl"))
        .unwrap()
        .write_all(b"{\"source\":\"a\",\"source\":\"b\"}")
        .unwrap();
    refused(
        import_run(&run, &dir.join("out.sqlite"), BUILD, None),
        "duplicate JSON key",
    );
    let _ = fs::remove_dir_all(dir);
}

// An oversized integer is reported as an input error.
#[test]
fn huge_integer_is_an_actionable_input_error() {
    let dir = scratch("huge-int");
    let run = legacy_run(&dir);
    let path = run.join("pages.jsonl");
    let text = fs::read_to_string(&path).unwrap();
    let first = text.lines().next().unwrap();
    let edited = Regex::new(r#""size_bytes": ?\d+"#)
        .unwrap()
        .replace(
            first,
            format!("\"size_bytes\": 1{}", "0".repeat(400)).as_str(),
        )
        .into_owned();
    fs::write(&path, text.replacen(first, &edited, 1)).unwrap();
    refused(
        import_run(&run, &dir.join("out.sqlite"), BUILD, None),
        "expected INTEGER|size_bytes|invalid JSON|number",
    );
    let _ = fs::remove_dir_all(dir);
}

// An existing destination or symlink is never overwritten.
// Symlink creation needs privilege on Windows.
#[test]
#[cfg(unix)]
fn existing_destination_and_symlink_are_never_overwritten() {
    let dir = scratch("clobber");
    let run = legacy_run(&dir);
    let existing = dir.join("existing.sqlite");
    fs::write(&existing, b"keep me").unwrap();
    let link = dir.join("link.sqlite");
    std::os::unix::fs::symlink(&existing, &link).unwrap();
    for target in [&existing, &link] {
        refused(import_run(&run, target, BUILD, None), "already exists");
    }
    assert_eq!(fs::read(&existing).unwrap(), b"keep me");
    assert!(fs::symlink_metadata(&link)
        .unwrap()
        .file_type()
        .is_symlink());
    let _ = fs::remove_dir_all(dir);
}

// Producer and configuration are never inferred from the current checkout.
#[test]
fn missing_producer_or_configuration_cannot_be_inferred() {
    let dir = scratch("producer");
    let run = legacy_run(&dir);
    refused(
        import_run(&run, &dir.join("a.sqlite"), "", None),
        "original crawl",
    );
    let path = run.join("audit.json");
    let mut data: Value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
    let effective = data["run"]
        .as_object_mut()
        .unwrap()
        .remove("crawl_config")
        .unwrap();
    fs::write(&path, serde_json::to_vec(&data).unwrap()).unwrap();
    refused(
        import_run(&run, &dir.join("b.sqlite"), BUILD, None),
        "effective configuration",
    );
    let out = import_run(&run, &dir.join("c.sqlite"), BUILD, Some(&effective)).unwrap();
    assert!(out.exists());
    let _ = fs::remove_dir_all(dir);
}

// An explicit configuration must agree with the artifact.
#[test]
fn explicit_configuration_must_agree() {
    let dir = scratch("agree");
    let run = legacy_run(&dir);
    refused(
        import_run(
            &run,
            &dir.join("out.sqlite"),
            BUILD,
            Some(&json!({"different": true})),
        ),
        "differs",
    );
    let _ = fs::remove_dir_all(dir);
}

// Provenance hashes the consumed stream, not a later replacement.
#[test]
fn provenance_hashes_the_consumed_pages_stream() {
    let dir = scratch("provenance");
    let path = imported(&dir);
    let con = open_scan(&path, true, None).unwrap();
    let payload: String = con
        .query_row(
            "SELECT payload_json FROM context_items WHERE kind='legacy_import_provenance'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    let data: Value = serde_json::from_str(&payload).unwrap();
    let consumed = fs::read(fixture().join("pages.jsonl")).unwrap();
    assert_eq!(data["inputs"][0]["bytes"], consumed.len());
    assert_eq!(
        data["inputs"][0]["sha256"].as_str().unwrap(),
        format!("{:x}", <Sha256 as Digest>::digest(&consumed))
    );
    let _ = fs::remove_dir_all(dir);
}

// Legacy render-blocking states survive import and export.
#[test]
fn render_blocking_states_survive_import() {
    for (index, value) in [
        json!(null),
        json!("[]"),
        json!("[\"https://example.com/app.js\"]"),
        json!(""),
        json!("https://example.com/app.js\n"),
    ]
    .into_iter()
    .enumerate()
    {
        let dir = scratch(&format!("render-blocking{index}"));
        let run = legacy_run(&dir);
        rewrite_pages(&run, |row| row["render_blocking"] = value.clone());
        let out = import_run(&run, &dir.join("out.sqlite"), BUILD, None).unwrap();
        assert!(open_scan(&out, true, None).is_ok(), "{value}");
        let _ = fs::remove_dir_all(dir);
    }
}

// Invalid render-blocking measurements are rejected on import.
#[test]
fn invalid_render_blocking_measurement_is_rejected() {
    for (index, value) in [json!(1), json!("{}"), json!("[1]"), json!("[\"\"]")]
        .into_iter()
        .enumerate()
    {
        let dir = scratch(&format!("render-bad{index}"));
        let run = legacy_run(&dir);
        rewrite_pages(&run, |row| row["render_blocking"] = value.clone());
        assert!(
            import_run(&run, &dir.join("out.sqlite"), BUILD, None).is_err(),
            "{value}"
        );
        let _ = fs::remove_dir_all(dir);
    }
}

// Legacy imports are not eligible for light open.
#[test]
fn legacy_import_is_not_light_eligible() {
    let dir = scratch("not-light");
    let path = imported(&dir);
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
    let path = imported(&dir);
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
