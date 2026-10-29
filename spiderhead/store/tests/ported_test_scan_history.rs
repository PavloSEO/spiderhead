//! Ported cases of the reference suite that need only the store: a foreign SQLite file
//! is refused by `snapshot_scan` and no output is created.
//!
//! Deferred (see the report): every other test builds a finished native scan through
//! `PageRecord` and `commit_page`, which needs the crawl layer (the reference suite), or saves an
//! audit through the MCP handlers (`_save_audit`), or imports the legacy run fixture.

use std::path::{Path, PathBuf};

use rusqlite::Connection;
use spiderhead_store::history::snapshot_scan;

/// Scratch directory removed on drop, so a failed case leaves nothing behind.
struct Scratch(PathBuf);

impl Scratch {
    fn new(name: &str) -> Self {
        let dir = std::env::temp_dir().join(format!(
            "spiderhead-tests35-history-{}-{name}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("scratch dir");
        Scratch(dir)
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// Writes the header that makes `path` a foreign or incompatible SQLite file.
fn write_foreign_header(path: &Path, kind: &str) {
    let con = Connection::open(path).expect("open source");
    let header = match kind {
        "application" => "PRAGMA application_id=1",
        "version" => "PRAGMA application_id=1397051208; PRAGMA user_version=999",
        "schema" => {
            "PRAGMA application_id=1397051208; PRAGMA user_version=1; \
             CREATE TABLE scan(singleton INTEGER PRIMARY KEY)"
        }
        other => panic!("unknown case {other}"),
    };
    con.execute_batch(header).expect("write header");
}

/// reference: `snapshot_scan` raises `ScanError` and the target is never created.
fn refuses_foreign_input(kind: &str) {
    let scratch = Scratch::new(kind);
    let source = scratch.0.join(format!("{kind}.sqlite"));
    let target = scratch.0.join("copy.sqlite");
    write_foreign_header(&source, kind);

    assert!(snapshot_scan(&source, &target).is_err());
    assert!(!target.exists());
}

#[test]
fn snapshot_refuses_foreign_input_without_creating_output_application() {
    refuses_foreign_input("application");
}

#[test]
fn snapshot_refuses_foreign_input_without_creating_output_version() {
    refuses_foreign_input("version");
}

#[test]
fn snapshot_refuses_foreign_input_without_creating_output_schema() {
    refuses_foreign_input("schema");
}
