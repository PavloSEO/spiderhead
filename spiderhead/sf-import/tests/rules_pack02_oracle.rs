//! Oracle parity for rules pack 02 (`_shared`, `_pagination`, `_technical`).
//! Each `tests/fixtures/rules_pack02/<name>/` dir holds an `expect.json` with
//! the exports frames, run config, the check list and the oracle's observable
//! result; native scenarios add a `scan.sqlite`. Regenerate with
//! the reference suite.

use std::path::{Path, PathBuf};

use serde_json::{json, Map, Value};
use spiderhead_sf_import::context::AuditContext;
use spiderhead_sf_import::loader::LoadedExports;
use spiderhead_sf_import::rules::{pagination, technical};
use spiderhead_sf_import::table::Table;

fn fixture_dir(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/rules_pack02")
        .join(name)
}

fn exports_from(expect: &Value) -> LoadedExports {
    let mut loaded = LoadedExports::default();
    if let Some(frames) = expect.get("exports").and_then(Value::as_object) {
        for (name, frame) in frames {
            let columns: Vec<String> = frame
                .get("columns")
                .and_then(Value::as_array)
                .map(|c| {
                    c.iter()
                        .map(|v| v.as_str().unwrap_or("").to_owned())
                        .collect()
                })
                .unwrap_or_default();
            let rows = frame
                .get("rows")
                .and_then(Value::as_array)
                .cloned()
                .unwrap_or_default()
                .into_iter()
                .map(|r| r.as_array().cloned().unwrap_or_default())
                .collect();
            loaded
                .frames
                .insert(name.clone(), Table::new(columns, rows));
        }
    }
    loaded
}

fn issue_json(i: &spiderhead_formats::models::Issue) -> Value {
    json!({
        "check": i.check,
        "target_url": i.target_url,
        "status_code": i.status_code,
        "details": i.details,
    })
}

fn snapshot(ctx: &AuditContext) -> Value {
    json!({
        "issues": ctx.issues_iter().iter().map(issue_json).collect::<Vec<_>>(),
        "skipped": ctx
            .skipped
            .iter()
            .map(|s| json!({"id": s.id, "reason": s.reason}))
            .collect::<Vec<_>>(),
        "asset_measurements": Value::Object(ctx.asset_measurements.clone()),
    })
}

fn run_check(name: &str, ctx: &mut AuditContext) {
    let result = match name {
        "check_pagination" => pagination::check_pagination(ctx),
        "check_pagination_series" => pagination::check_pagination_series(ctx),
        "check_pagination_sequence" => pagination::check_pagination_sequence(ctx),
        "check_amp_pairing" => technical::check_amp_pairing(ctx),
        "check_tech_extra" => technical::check_tech_extra(ctx),
        "check_charset" => technical::check_charset(ctx),
        "check_doctype" => technical::check_doctype(ctx),
        "check_mobile_alternate" => technical::check_mobile_alternate(ctx),
        "check_viewport" => technical::check_viewport(ctx),
        "check_compression" => technical::check_compression(ctx),
        "check_render_blocking" => technical::check_render_blocking(ctx),
        "check_element_position" => technical::check_element_position(ctx),
        "check_document_skeleton" => technical::check_document_skeleton(ctx),
        "check_html_structure" => technical::check_html_structure(ctx),
        other => panic!("unknown check {other}"),
    };
    result.unwrap_or_else(|e| panic!("{name}: {e}"));
}

fn run(name: &str) {
    let dir = fixture_dir(name);
    let expect: Value = serde_json::from_str(
        &std::fs::read_to_string(dir.join("expect.json")).expect("expect.json"),
    )
    .expect("parse expect.json");
    let config = expect.get("config").cloned().unwrap_or_else(|| json!({}));
    let mut ctx =
        AuditContext::new(exports_from(&expect), config, None, false).expect("AuditContext");
    if let Some(probes) = expect.get("target_probes").and_then(Value::as_object) {
        ctx.target_probes = probes.clone();
    }
    let scan = dir.join("scan.sqlite");
    if scan.exists() {
        ctx.scan_con = Some(rusqlite::Connection::open(&scan).expect("open scan.sqlite"));
    }
    for check in expect
        .get("checks")
        .and_then(Value::as_array)
        .expect("checks list")
    {
        run_check(check.as_str().expect("check name"), &mut ctx);
    }
    let got = snapshot(&ctx);
    for key in ["issues", "skipped", "asset_measurements"] {
        let got_v = got.get(key).cloned().unwrap_or(Value::Null);
        let want_v = expect.get(key).cloned().unwrap_or(Value::Null);
        // serde_json maps and reference dicts agree on unordered key lookup; the
        // nested `details`/`asset_measurements` payloads compare as values.
        assert_eq!(
            got_v,
            want_v,
            "{name}: {key} mismatch\ngot:  {}\nwant: {}",
            serde_json::to_string_pretty(&got_v).unwrap_or_default(),
            serde_json::to_string_pretty(&want_v).unwrap_or_default(),
        );
    }
}

macro_rules! scenario {
    ($name:ident) => {
        #[test]
        fn $name() {
            run(stringify!($name));
        }
    };
}

scenario!(minimal);
scenario!(pagination_cycle);
scenario!(pagination_tail_cycle);
scenario!(pagination_terminating);
scenario!(pagination_unlinked_series);
scenario!(pagination_gap);
scenario!(pagination_strides);
scenario!(pagination_contradiction);
scenario!(pagination_unnumbered);
scenario!(pagination_nonindexable);
scenario!(amp);
scenario!(tech);
scenario!(charset);
scenario!(doctype_viewport);
scenario!(mobile_alternate);
scenario!(compression);
scenario!(element_skeleton);
scenario!(html_structure);
scenario!(render_blocking);
scenario!(pagination_native);
scenario!(render_blocking_native);

/// Belt-and-suspenders: the fixture dir and the scenario! list stay in sync —
/// a generator run that adds a scenario fails here until it is replayed too.
#[test]
fn every_fixture_is_replayed() {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/rules_pack02");
    let mut names: Vec<String> = std::fs::read_dir(&dir)
        .expect("fixture dir")
        .filter_map(|e| e.ok())
        .filter(|e| e.path().is_dir())
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .collect();
    names.sort();
    let want: Vec<String> = [
        "amp",
        "charset",
        "compression",
        "doctype_viewport",
        "element_skeleton",
        "html_structure",
        "minimal",
        "mobile_alternate",
        "pagination_contradiction",
        "pagination_cycle",
        "pagination_gap",
        "pagination_native",
        "pagination_nonindexable",
        "pagination_strides",
        "pagination_tail_cycle",
        "pagination_terminating",
        "pagination_unlinked_series",
        "pagination_unnumbered",
        "render_blocking",
        "render_blocking_native",
        "tech",
    ]
    .iter()
    .map(|s| s.to_string())
    .collect();
    assert_eq!(names, want);
    let _ = Map::new(); // keep the import meaningful if the list changes shape
}
