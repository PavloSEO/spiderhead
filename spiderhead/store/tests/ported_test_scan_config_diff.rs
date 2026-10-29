//! Ports of the reference suite (#1529): field-by-field diff of saved crawl
//! configurations. The reference saved-scan tests write two real scans through `NativeScan.create`;
//! the Rust port has no equivalent header writer, so each saved scan is a temp copy of the
//! `commit-pre.sqlite` fixture whose recorded `scan.config_json` header is edited in place. The
//! reader under test (`read_effective_config`) only consumes that header plus the fixture's
//! extension items, which do not overlap the edited paths. The CLI test (subprocess of the reference
//! entry point) is deferred, see the slice report.

use std::path::PathBuf;

use rusqlite::Connection;
use serde_json::{json, Map, Value};
use spiderhead_store::config_diff::{diff_configs, scan_config_diff};

fn object(value: Value) -> Map<String, Value> {
    match value {
        Value::Object(map) => map,
        other => panic!("expected a JSON object, got {other}"),
    }
}

fn fixture() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/commit-pre.sqlite")
}

/// Removes the scratch directory when the test ends, pass or fail.
struct Scratch(PathBuf);

impl Scratch {
    fn new(name: &str) -> Self {
        let dir =
            std::env::temp_dir().join(format!("sh-config-diff-{}-{name}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        Scratch(dir)
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// Set a dotted setting path, creating intermediate objects as needed.
fn set_dotted(root: &mut Map<String, Value>, path: &str, value: Value) {
    let parts: Vec<&str> = path.split('.').collect();
    let (last, parents) = parts.split_last().unwrap();
    let mut node = root;
    for part in parents {
        node = node
            .entry((*part).to_owned())
            .or_insert_with(|| Value::Object(Map::new()))
            .as_object_mut()
            .unwrap();
    }
    node.insert((*last).to_owned(), value);
}

/// Copy the fixture scan into the scratch directory and record the given settings in its header.
fn saved_scan(scratch: &Scratch, name: &str, overrides: &[(&str, Value)]) -> PathBuf {
    let path = scratch.0.join(format!("{name}.sqlite"));
    std::fs::copy(fixture(), &path).unwrap();
    let con = Connection::open(&path).unwrap();
    let text: String = con
        .query_row(
            "SELECT config_json FROM scan WHERE singleton=1",
            [],
            |row| row.get(0),
        )
        .unwrap();
    let mut config = object(serde_json::from_str(&text).unwrap());
    for (dotted, value) in overrides {
        set_dotted(&mut config, dotted, value.clone());
    }
    con.execute(
        "UPDATE scan SET config_json=?1 WHERE singleton=1",
        [Value::Object(config).to_string()],
    )
    .unwrap();
    // The fixture's extension item fingerprints the original header; drop it so the reader
    // takes the legacy-header path (no fingerprint to disagree with the edited header).
    con.execute(
        "DELETE FROM context_items WHERE kind='native_config_extension'",
        [],
    )
    .unwrap();
    path
}

fn paths(changed: &Value) -> Vec<&str> {
    changed
        .as_array()
        .unwrap()
        .iter()
        .map(|item| item["path"].as_str().unwrap())
        .collect()
}

#[test]
fn three_changed_settings_list_exactly_those_three_fields() {
    let before = object(json!({
        "limits": {"max_urls": 100, "max_depth": 5},
        "robots": {"policy": "respect"},
        "http": {"timeout_seconds": 10.0},
    }));
    let after = object(json!({
        "limits": {"max_urls": 250, "max_depth": 5},
        "robots": {"policy": "ignore"},
        "http": {"timeout_seconds": 10.0, "user_agent": "bot"},
    }));
    let result = diff_configs(&before, &after);
    assert_eq!(
        paths(&result["changed"]),
        ["limits.max_urls", "robots.policy"]
    );
    assert_eq!(
        result["changed"][0],
        json!({"path": "limits.max_urls", "before": 100, "after": 250})
    );
    assert_eq!(
        result["added"],
        json!([{"path": "http.user_agent", "after": "bot"}])
    );
    assert_eq!(result["removed"], json!([]));
    assert_eq!(result["identical"], json!(false));
}

#[test]
fn identical_configurations_report_no_differences() {
    let config = object(json!({"limits": {"max_urls": 10}, "http": {"headers": {"x": "1"}}}));
    let copy = object(serde_json::from_str(&serde_json::to_string(&config).unwrap()).unwrap());
    let result = diff_configs(&config, &copy);
    assert_eq!(result["identical"], json!(true));
    assert_eq!(result["changed"], json!([]));
    assert_eq!(result["added"], json!([]));
    assert_eq!(result["removed"], json!([]));
}

#[test]
fn two_saved_scans_diff_through_their_recorded_headers() {
    let scratch = Scratch::new("diff-headers");
    let before = saved_scan(&scratch, "a", &[("limits.max_urls", json!(10))]);
    let after = saved_scan(
        &scratch,
        "b",
        &[
            ("limits.max_urls", json!(10)),
            ("robots.policy", json!("ignore")),
            ("speed.adaptive", json!(false)),
        ],
    );
    let result = scan_config_diff(&before, &after).unwrap();
    let changed = paths(&result["changed"]);
    assert!(changed.contains(&"robots.policy"));
    assert!(changed.contains(&"speed.adaptive"));
    let robots = result["changed"]
        .as_array()
        .unwrap()
        .iter()
        .find(|item| item["path"] == "robots.policy")
        .unwrap();
    assert_eq!(robots["before"], json!("respect"));
    assert_eq!(robots["after"], json!("ignore"));
    assert_eq!(result["identical"], json!(false));
}

#[test]
fn two_identical_saved_scans_are_identical() {
    let scratch = Scratch::new("identical");
    let before = saved_scan(&scratch, "a", &[("limits.max_urls", json!(10))]);
    let after = saved_scan(&scratch, "b", &[("limits.max_urls", json!(10))]);
    let result = scan_config_diff(&before, &after).unwrap();
    assert_eq!(result["identical"], json!(true));
}
