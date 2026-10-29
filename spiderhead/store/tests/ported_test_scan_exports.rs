//! Ported tests of the reference suite: legacy exports from scan.v1 stay deterministic
//! and safely published. The legacy run is the offline fixture under `testdata/legacy-run`
//! (captured from the reference build). The one reference test that monkeypatches the JSONL writer
//! to inject a failure is deferred: it needs reference-only mechanics.

use std::fs;
use std::path::{Path, PathBuf};

use rusqlite::Connection;
use serde_json::{json, Value};
use spiderhead_store::exports::export_run;
use spiderhead_store::scan_artifact::{import_run, open_scan};

const BUILD: &str = "25fd2ed032a31d63c5811722619e35c14b631476";

fn fixture() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("testdata/legacy-run")
}

fn scratch(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "spiderhead-tests40-exports-{}-{name}",
        std::process::id()
    ));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).expect("scratch");
    dir
}

/// A writable copy of the legacy run directory (`legacy_run` fixture of the reference suite).
fn legacy_run(dir: &Path) -> PathBuf {
    let run = dir.join("legacy-run");
    fs::create_dir_all(&run).expect("run dir");
    for name in ["audit.json", "pages.jsonl", "links.jsonl"] {
        fs::copy(fixture().join(name), run.join(name)).expect("copy fixture");
    }
    run
}

fn artifact(dir: &Path, run: &Path) -> PathBuf {
    import_run(run, &dir.join("scan.sqlite"), BUILD, None).expect("import")
}

/// reference `_pages`: JSONL pages with the live-only `attempts` projection dropped.
fn pages(path: &Path) -> Vec<Value> {
    fs::read_to_string(path.join("pages.jsonl"))
        .expect("pages.jsonl")
        .lines()
        .map(|line| {
            let mut row: Value = serde_json::from_str(line).expect("json row");
            if let Some(object) = row.as_object_mut() {
                object.remove("attempts");
            }
            row
        })
        .collect()
}

fn links(path: &Path) -> Vec<Value> {
    fs::read_to_string(path.join("links.jsonl"))
        .expect("links.jsonl")
        .lines()
        .map(|line| serde_json::from_str(line).expect("json row"))
        .collect()
}

fn rows(con: &Connection, sql: &str) -> Vec<Vec<String>> {
    let mut stmt = con.prepare(sql).expect("prepare");
    let columns = stmt.column_count();
    stmt.query_map([], |row| {
        (0..columns)
            .map(|i| Ok(format!("{:?}", row.get::<_, rusqlite::types::Value>(i)?)))
            .collect::<Result<Vec<String>, rusqlite::Error>>()
    })
    .expect("query")
    .collect::<Result<_, _>>()
    .expect("rows")
}

fn document(con: &Connection) -> String {
    con.query_row("SELECT document_json FROM audit", [], |r| r.get(0))
        .expect("audit")
}

#[test]
fn export_reimports_all_observations_and_audit_bytes() {
    let dir = scratch("reimport");
    let run = legacy_run(&dir);
    let scan = artifact(&dir, &run);
    let before = fs::read(&scan).expect("bytes");
    let exported = dir.join("exported");

    let result = export_run(&scan, &exported).expect("export");
    assert_eq!(
        Value::Object(result),
        json!({"ok": true, "path": exported.to_string_lossy(), "counts": {"pages": 2, "links": 3}})
    );
    assert_eq!(fs::read(&scan).expect("bytes"), before);
    let mut names: Vec<String> = fs::read_dir(&exported)
        .expect("dir")
        .map(|e| e.expect("entry").file_name().to_string_lossy().into_owned())
        .collect();
    names.sort();
    assert_eq!(names, ["audit.json", "links.jsonl", "pages.jsonl"]);
    assert_eq!(
        fs::read(exported.join("audit.json")).expect("audit"),
        fs::read(run.join("audit.json")).expect("audit")
    );
    assert_eq!(pages(&exported), pages(&run));
    assert_eq!(links(&exported), links(&run));

    let reimported = dir.join("reimported.sqlite");
    import_run(&exported, &reimported, BUILD, None).expect("reimport");
    let original = open_scan(&scan, true, None).expect("open original");
    let restored = open_scan(&reimported, true, None).expect("open restored");
    assert_eq!(
        rows(&original, "SELECT * FROM pages ORDER BY page_ordinal"),
        rows(&restored, "SELECT * FROM pages ORDER BY page_ordinal")
    );
    assert_eq!(
        rows(&original, "SELECT * FROM links ORDER BY link_id"),
        rows(&restored, "SELECT * FROM links ORDER BY link_id")
    );
    assert_eq!(document(&original), document(&restored));
}

#[test]
fn repeated_exports_are_byte_identical() {
    let dir = scratch("repeat");
    let scan = artifact(&dir, &legacy_run(&dir));
    let first = dir.join("first");
    let second = dir.join("second");
    export_run(&scan, &first).expect("first");
    export_run(&scan, &second).expect("second");
    for name in ["pages.jsonl", "links.jsonl", "audit.json"] {
        assert_eq!(
            fs::read(first.join(name)).expect("first"),
            fs::read(second.join(name)).expect("second"),
            "{name}"
        );
    }
}

#[test]
fn export_omits_known_late_fields_that_were_absent_in_legacy_jsonl() {
    const LATE: [&str; 4] = [
        "content_frames",
        "content_frames_same_origin",
        "hreflang",
        "body_unavailable",
    ];
    let dir = scratch("late");
    let run = legacy_run(&dir);
    let mut edited = pages(&run);
    for page in edited.iter_mut() {
        for field in LATE {
            page.as_object_mut().expect("object").remove(field);
        }
    }
    let text: String = edited.iter().map(|page| format!("{page}\n")).collect();
    fs::write(run.join("pages.jsonl"), text).expect("write pages");
    let scan = artifact(&dir, &run);
    let exported = dir.join("older-export");
    export_run(&scan, &exported).expect("export");
    for page in pages(&exported) {
        let object = page.as_object().expect("object");
        for field in LATE {
            assert!(!object.contains_key(field), "{field}");
        }
    }
}

// Symlink creation needs privilege on Windows.
#[test]
#[cfg(unix)]
fn existing_and_symlinked_destinations_are_refused() {
    let dir = scratch("refuse");
    let scan = artifact(&dir, &legacy_run(&dir));
    let existing = dir.join("existing");
    fs::create_dir_all(&existing).expect("existing");
    let alias = dir.join("alias");
    std::os::unix::fs::symlink(&existing, &alias).expect("symlink");
    for target in [&existing, &alias] {
        let err = export_run(&scan, target).expect_err("must refuse");
        assert!(
            err.to_string().contains("destination already exists"),
            "{err}"
        );
    }
    assert!(existing.is_dir());
    assert!(alias
        .symlink_metadata()
        .expect("meta")
        .file_type()
        .is_symlink());
}

#[test]
fn recovery_only_partialness_cannot_be_exported_as_complete() {
    use std::io::Write;
    let dir = scratch("recovery");
    let run = legacy_run(&dir);
    fs::OpenOptions::new()
        .append(true)
        .open(run.join("links.jsonl"))
        .expect("open")
        .write_all(b"{\"source\":")
        .expect("append");
    let scan = import_run(&run, &dir.join("recovered.sqlite"), BUILD, None).expect("import");
    let before = fs::read(&scan).expect("bytes");
    let destination = dir.join("cannot-represent-recovery");
    let err = export_run(&scan, &destination).expect_err("must refuse");
    assert!(
        err.to_string()
            .contains("would hide recovered crawl partialness"),
        "{err}"
    );
    assert!(!destination.exists());
    assert_eq!(fs::read(&scan).expect("bytes"), before);
}

#[test]
fn partialness_recorded_in_audit_survives_export_and_reimport() {
    let dir = scratch("partial");
    let run = legacy_run(&dir);
    let audit_path = run.join("audit.json");
    let mut audit: Value =
        serde_json::from_str(&fs::read_to_string(&audit_path).expect("audit")).expect("json");
    audit["run"]["crawl_partial"] = json!(true);
    fs::write(&audit_path, audit.to_string()).expect("write audit");
    let scan = import_run(&run, &dir.join("partial.sqlite"), BUILD, None).expect("import");
    let destination = dir.join("partial-export");
    export_run(&scan, &destination).expect("export");
    let reimported =
        import_run(&destination, &dir.join("partial-again.sqlite"), BUILD, None).expect("reimport");
    let con = open_scan(&reimported, true, None).expect("open");
    let partial: i64 = con
        .query_row("SELECT crawl_partial FROM scan", [], |r| r.get(0))
        .expect("flag");
    assert_eq!(partial, 1);
}
