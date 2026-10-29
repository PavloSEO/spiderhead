//! Oracle parity for the heuristics checks. Each
//! `tests/fixtures/heuristics/<name>/expect.json` holds the exports, config, the
//! check list and the oracle's issues, groups, skips and derived page metrics;
//! scenarios that read stored documents ship an `html/` tree (`{dir}` in the
//! recorded config). Regenerate with the reference suite.

use std::path::{Path, PathBuf};

use serde_json::{json, Map, Value};
use spiderhead_sf_import::context::AuditContext;
use spiderhead_sf_import::heuristics as h;
use spiderhead_sf_import::loader::LoadedExports;
use spiderhead_sf_import::table::Table;

fn fixture_dir(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/heuristics")
        .join(name)
}

fn exports_from(expect: &Value) -> LoadedExports {
    let mut loaded = LoadedExports::default();
    for (name, frame) in expect["exports"].as_object().unwrap() {
        let columns: Vec<String> = frame["columns"]
            .as_array()
            .unwrap()
            .iter()
            .map(|v| v.as_str().unwrap_or("").to_owned())
            .collect();
        let rows = frame["rows"]
            .as_array()
            .unwrap()
            .iter()
            .map(|r| r.as_array().cloned().unwrap_or_default())
            .collect();
        loaded
            .frames
            .insert(name.clone(), Table::new(columns, rows));
    }
    loaded
}

/// Integral floats and integers compare equal, as in reference.
fn norm(v: &Value) -> Value {
    match v {
        Value::Number(n) => match n.as_f64() {
            Some(f) if f.fract() == 0.0 && f.abs() < 9.0e15 => json!(f as i64),
            Some(f) => json!(f),
            None => v.clone(),
        },
        Value::Array(a) => Value::Array(a.iter().map(norm).collect()),
        Value::Object(o) => Value::Object(o.iter().map(|(k, v)| (k.clone(), norm(v))).collect()),
        other => other.clone(),
    }
}

fn run(name: &str) {
    let dir = fixture_dir(name);
    let expect: Value =
        serde_json::from_str(&std::fs::read_to_string(dir.join("expect.json")).unwrap()).unwrap();
    let mut config = expect["config"].clone();
    if config["input"]["html_store_dir"] == "{dir}" {
        config["input"]["html_store_dir"] = json!(dir.join("html").to_string_lossy());
    }
    let mut ctx = AuditContext::new(exports_from(&expect), config, None, false).unwrap();
    let mut stats = Map::new();
    for check in expect["checks"].as_array().unwrap() {
        match check.as_str().unwrap() {
            "check_html_weight" => stats = h::check_html_weight(&mut ctx).unwrap(),
            "check_dom" => h::check_dom(&mut ctx).unwrap(),
            "check_content_duplication" => h::check_content_duplication(&mut ctx).unwrap(),
            "check_templated_titles" => h::check_templated_titles(&mut ctx).unwrap(),
            other => panic!("unknown check {other}"),
        }
    }
    let issues: Vec<Value> = ctx
        .issues_iter()
        .iter()
        .map(|i| {
            json!({
                "check": i.check, "target_url": i.target_url, "status_code": i.status_code,
                "group_id": i.group_id, "details": i.details,
            })
        })
        .collect();
    let groups: Vec<Value> = ctx
        .groups_iter()
        .iter()
        .map(
            |g| json!({"group_id": g.group_id, "check": g.check, "urls": g.urls, "count": g.count}),
        )
        .collect();
    let skipped: Vec<Value> = ctx
        .skipped
        .iter()
        .map(|s| json!({"id": s.id, "reason": s.reason}))
        .collect();
    let mut metrics = Map::new();
    for page in ctx.pages_iter() {
        let derived: Map<String, Value> = page.map(|p| {
            [
                "bytes_per_word",
                "size_vs_median_ratio",
                "dom_depth",
                "dom_nodes",
            ]
            .iter()
            .filter_map(|k| p.metrics.get(*k).map(|v| (k.to_string(), v.clone())))
            .collect()
        });
        metrics.insert(page.url(), Value::Object(derived));
    }
    for (key, got) in [
        ("stats", Value::Object(stats)),
        ("issues", Value::Array(issues)),
        ("groups", Value::Array(groups)),
        ("skipped", Value::Array(skipped)),
        ("metrics", Value::Object(metrics)),
    ] {
        assert_eq!(
            norm(&got),
            norm(&expect[key]),
            "{name}: {key}\ngot:  {}\nwant: {}",
            serde_json::to_string_pretty(&got).unwrap(),
            serde_json::to_string_pretty(&expect[key]).unwrap()
        );
    }
}

macro_rules! scenario {
    ($($name:ident),* $(,)?) => { $(#[test] fn $name() { run(stringify!($name)); })* };
}

scenario!(
    weight,
    weight_templated,
    weight_even,
    weight_none,
    titles,
    titles_prefix,
    titles_few,
    titles_tie,
    dom,
    dom_none_stored,
    dom_no_pages,
    dup,
    dup_two_only,
    dup_native_cols,
    dup_hash_only_native,
    dup_disabled,
    combined,
);

#[test]
fn dom_metrics_single_element_fragment_is_its_own_root() {
    assert_eq!(h::dom_metrics("<div><p>x</p></div>"), Ok((1, 2)));
    assert_eq!(h::dom_metrics("<p>a</p><p>b</p>"), Ok((1, 3)));
    assert!(h::dom_metrics("").is_err());
}
