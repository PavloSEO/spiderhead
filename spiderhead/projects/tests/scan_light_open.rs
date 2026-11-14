//! observer evidence reads a scan lightly. The `scan_status`/`open_scan_mode`/
//! `import_run` cases and the `_validate_*` counters are storage/mcp slices
//! with reference-only monkeypatch seams, deferred in `oracle/deferred-tests.toml`.

#[path = "common/testkit.rs"]
mod testkit;

use serde_json::{json, Map};
use spiderhead_projects::observer::read_scan_evidence;

#[test]
fn project_observer_evidence_reads_scan_lightly() {
    let dir = testkit::scratch("lightopen", "scan");
    let path = dir.0.join("scan.sqlite");
    std::fs::copy(testkit::fixture("crawl-evidence.sqlite"), &path).expect("scan");

    let row: Map<String, serde_json::Value> = serde_json::from_value(json!({
        "path": path.to_str().unwrap(),
        "source_kind": "native",
        "lifecycle": "interrupted",
    }))
    .unwrap();
    let evidence = read_scan_evidence(&row).expect("read_scan_evidence");

    assert_eq!(evidence["state"], "available");
    assert_eq!(evidence["validation"], "light");
    assert_eq!(evidence["committed_page_outcomes"]["2xx"], 1);
}
