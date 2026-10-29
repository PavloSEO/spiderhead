//! Ports of the storage tests in the reference suite (issue #632): the heading outline
//! survives `import_run` unchanged, and a malformed outline is refused. Runs over a writable copy
//! of the legacy run fixture in `testdata/legacy-run`.

use std::fs;
use std::path::{Path, PathBuf};

use serde_json::{json, Value};
use spiderhead_store::error::StoreError;
use spiderhead_store::scan_artifact::import_run;

const BUILD: &str = "25fd2ed032a31d63c5811722619e35c14b631476";

fn scratch(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "spiderhead-tests33-heading-{}-{name}",
        std::process::id()
    ));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).expect("scratch");
    dir
}

/// Copy the legacy run into a scratch directory so `pages.jsonl` can be rewritten.
fn legacy_run(dir: &Path) -> PathBuf {
    let source = Path::new(env!("CARGO_MANIFEST_DIR")).join("testdata/legacy-run");
    let run = dir.join("run");
    fs::create_dir_all(&run).expect("run dir");
    for name in ["audit.json", "pages.jsonl", "links.jsonl"] {
        fs::copy(source.join(name), run.join(name)).expect("copy fixture");
    }
    run
}

/// Set `heading_outline` on the first page row of `pages.jsonl`; return that row's outline.
fn write_first_outline(run: &Path, outline: Value) -> Value {
    let pages = run.join("pages.jsonl");
    let text = fs::read_to_string(&pages).expect("read pages");
    let mut rows: Vec<Value> = text
        .lines()
        .map(|line| serde_json::from_str(line).expect("row"))
        .collect();
    rows[0]["heading_outline"] = outline.clone();
    let mut out = String::new();
    for row in &rows {
        out.push_str(&serde_json::to_string(row).expect("encode"));
        out.push('\n');
    }
    fs::write(&pages, out).expect("write pages");
    outline
}

#[test]
fn the_stored_outline_survives_an_import_unchanged() {
    let dir = scratch("roundtrip");
    let run = legacy_run(&dir);
    let outline = write_first_outline(
        &run,
        json!([
            {"level": 2, "text": "Menu", "region": "nav"},
            {"level": 1, "text": "Home", "region": "content"},
        ]),
    );
    let out = import_run(&run, &dir.join("scan.sqlite"), BUILD, None).expect("import");
    let con = rusqlite::Connection::open(&out).expect("open");
    let stored: String = con
        .query_row(
            "SELECT heading_outline_json FROM pages WHERE page_ordinal=0",
            [],
            |row| row.get(0),
        )
        .expect("stored outline");
    let stored: Value = serde_json::from_str(&stored).expect("json");
    assert_eq!(stored, outline);
    let _ = fs::remove_dir_all(dir);
}

#[test]
fn an_outline_that_is_not_level_text_region_objects_is_refused() {
    // The four parametrized cases of the reference test, one assertion each.
    let cases = [
        json!([{"level": 7, "text": "Out of range", "region": "nav"}]),
        json!([{"level": 1, "text": "No region key"}]),
        json!([{"level": "1", "text": "Level as text", "region": "nav"}]),
        json!("not a list at all"),
    ];
    for (index, outline) in cases.into_iter().enumerate() {
        let dir = scratch(&format!("refused-{index}"));
        let run = legacy_run(&dir);
        write_first_outline(&run, outline);
        let err = import_run(&run, &dir.join("bad.sqlite"), BUILD, None)
            .expect_err("malformed outline must be refused");
        match &err {
            StoreError::ScanError(scan) => assert!(
                scan.message.contains("heading_outline"),
                "case {index}: {}",
                scan.message
            ),
            other => panic!("case {index}: expected ScanError, got {other:?}"),
        }
        let _ = fs::remove_dir_all(dir);
    }
}
