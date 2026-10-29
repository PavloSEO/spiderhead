//! Oracle parity for `render_checks`: each `tests/fixtures/render_checks/<name>/`
//! holds a minimal scan database (`scan.sqlite`) and the oracle's issues and skips
//! (`expect.json`). Regenerate with the reference suite.

use std::path::Path;

use serde_json::{json, Value};
use spiderhead_sf_import::context::AuditContext;
use spiderhead_sf_import::loader::LoadedExports;
use spiderhead_sf_import::render_checks::check_rendered_elements;
use spiderhead_sf_import::table::Table;

fn run(name: &str) {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/render_checks")
        .join(name);
    let expect: Value =
        serde_json::from_str(&std::fs::read_to_string(dir.join("expect.json")).unwrap()).unwrap();
    let mut exports = LoadedExports::default();
    exports.frames.insert(
        "internal_all".into(),
        Table::new(
            vec!["Address".into()],
            vec![vec![json!("https://example.test/")]],
        ),
    );
    let config = json!({"thresholds": {}, "requirements": {}});
    let mut ctx = AuditContext::new(exports, config, None, false).unwrap();
    if expect["has_scan"] == true {
        ctx.scan_con = Some(rusqlite::Connection::open(dir.join("scan.sqlite")).unwrap());
    }
    check_rendered_elements(&mut ctx).unwrap();
    let issues: Vec<Value> = ctx
        .issues_iter()
        .iter()
        .map(|i| json!({"check": i.check, "target_url": i.target_url, "details": i.details}))
        .collect();
    let skipped: Vec<Value> = ctx
        .skipped
        .iter()
        .map(|s| json!({"id": s.id, "reason": s.reason}))
        .collect();
    assert_eq!(Value::Array(issues), expect["issues"], "{name}: issues");
    assert_eq!(Value::Array(skipped), expect["skipped"], "{name}: skipped");
}

macro_rules! scenario {
    ($($name:ident),* $(,)?) => { $(#[test] fn $name() { run(stringify!($name)); })* };
}

scenario!(
    no_scan,
    empty,
    all_rules,
    malformed_rule,
    partial_network,
    mixed_availability,
    only_unavailable,
    bad_json,
);
