//! Oracle-fixture replay harness shared by `*_parity.rs` tests.
//!
//! A fixture (`tests/fixtures/<stage>_oracle.json`) holds scenarios recorded by
//! `gen/gen_<stage>the reference suite: setup ops, provider answers, stage args, and the
//! expected post-stage snapshot (every table, scrubbed timestamps), result
//! object, provider call log, and emitted files.
//!
//! Helpers for provider seams not exercised by an early stage light up as the
//! later parity tests land, so unused-code warnings stay off here.
#![allow(dead_code)]

use serde_json::{json, Map, Value};
use spiderhead_semantics::config::Cfg;
use spiderhead_semantics::providers::{
    ExpandResult, FakeArsenkin, FakeDemand, FakePages, FakeSerp, ProviderError,
};
use spiderhead_semantics::store::Store;
use std::path::{Path, PathBuf};

/// Timestamp-like columns scrubbed on both sides before comparing.
const VOLATILE: &[&str] = &[
    "first_seen",
    "last_update",
    "expanded_at",
    "fetched_at",
    "ts",
    "computed_at",
    "at",
    "created_at",
    "updated_at",
    "started_at",
    "finished_at",
];

/// Load and parse a fixture by stage name.
pub fn fixture(stage: &str) -> Value {
    let path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures")
        .join(format!("{stage}_oracle.json"));
    let text = std::fs::read_to_string(&path)
        .unwrap_or_else(|e| panic!("cannot read {}: {e}", path.display()));
    serde_json::from_str(&text).expect("fixture JSON")
}

/// Isolated project dir for one scenario (files land here via `cfg["_dir"]`).
pub fn project_dir(stage: &str, name: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("sem-parity-{stage}-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    d
}

/// `cfg` overrides plus `_dir` pointing at the scenario dir.
pub fn cfg_of(scenario: &Value, dir: &Path) -> Cfg {
    let mut cfg = Cfg::new();
    if let Some(obj) = scenario.get("cfg").and_then(|c| c.as_object()) {
        for (k, v) in obj {
            cfg.insert(k.clone(), v.clone());
        }
    }
    cfg.insert("_dir".into(), json!(dir.to_string_lossy().to_string()));
    cfg
}

fn scrub_value(v: &Value) -> Value {
    match v {
        Value::Object(map) => Value::Object(
            map.iter()
                .map(|(k, x)| {
                    (
                        k.clone(),
                        if VOLATILE.contains(&k.as_str()) && !x.is_null() {
                            json!("__ts__")
                        } else {
                            scrub_value(x)
                        },
                    )
                })
                .collect(),
        ),
        Value::Array(items) => Value::Array(items.iter().map(scrub_value).collect()),
        other => other.clone(),
    }
}

/// `SELECT * FROM <table> ORDER BY rowid` for every table, scrubbed — the
/// same shape `gen_common.dump_db` produces.
pub fn dump_db(store: &Store) -> Value {
    let mut out = Map::new();
    for row in store
        .q(
            "SELECT name FROM sqlite_master WHERE type='table' ORDER BY name",
            &[],
        )
        .unwrap()
    {
        let name = row["name"].as_str().unwrap_or_default().to_string();
        if name.starts_with("sqlite_") {
            continue;
        }
        let rows: Vec<Value> = store
            .q(&format!("SELECT * FROM {name} ORDER BY rowid"), &[])
            .unwrap()
            .into_iter()
            .map(|r| Value::Object(r.into_iter().collect()))
            .collect();
        out.insert(name, Value::Array(rows));
    }
    scrub_value(&Value::Object(out))
}

fn apply_table_rows(store: &Store, tables: &Value) {
    if let Some(obj) = tables.as_object() {
        for (table, rows) in obj {
            for row in rows.as_array().cloned().unwrap_or_default() {
                let obj = row.as_object().expect("setup row object");
                let cols: Vec<String> = obj.keys().cloned().collect();
                let marks = vec!["?"; cols.len()].join(",");
                let args: Vec<Value> = cols.iter().map(|c| obj[c].clone()).collect();
                store
                    .exec(
                        &format!("INSERT INTO {table}({}) VALUES({marks})", cols.join(",")),
                        &args,
                    )
                    .unwrap();
            }
        }
    }
    store.commit().unwrap();
}

/// One setup op (`["upsert", norm, base, depth, src, region]`, ...).
fn apply_op(store: &Store, op: &[Value]) {
    let name = op[0].as_str().unwrap();
    let s = |i: usize| op[i].as_str().unwrap_or_default();
    match name {
        "upsert" => {
            store
                .upsert(
                    s(1),
                    op[2].as_i64(),
                    op[3].as_i64().unwrap_or(0),
                    op.get(4).and_then(|v| v.as_str()),
                    s(5),
                )
                .unwrap();
        }
        "set_status" => {
            store
                .set_status(
                    s(1),
                    s(2),
                    op.get(3).and_then(|v| v.as_str()),
                    op.get(4).and_then(|v| v.as_str()),
                )
                .unwrap();
        }
        "set_fields" => {
            let kw: Vec<(String, Value)> = op[2]
                .as_array()
                .unwrap()
                .iter()
                .map(|p| (p[0].as_str().unwrap().to_string(), p[1].clone()))
                .collect();
            store.set_fields(s(1), &kw).unwrap();
        }
        "mark_expanded" => {
            store
                .mark_expanded(
                    s(1),
                    op[2].as_i64().unwrap_or(0),
                    op[3].as_i64().unwrap_or(0),
                    s(4),
                )
                .unwrap();
        }
        "add_edge" => store.add_edge(s(1), s(2), s(3)).unwrap(),
        "add_serp" => {
            let docs: Vec<spiderhead_semantics::store::Row> = op[2]
                .as_array()
                .unwrap()
                .iter()
                .map(|d| {
                    d.as_object()
                        .unwrap()
                        .iter()
                        .map(|(k, v)| (k.clone(), v.clone()))
                        .collect()
                })
                .collect();
            store.add_serp(s(1), &docs, None).unwrap();
        }
        "add_run" => {
            store
                .add_run(
                    s(1),
                    op[2].as_i64().unwrap_or(0),
                    op[3].as_i64().unwrap_or(0),
                    s(4),
                )
                .unwrap();
        }
        "upsert_cluster" => {
            store
                .upsert_cluster(
                    s(1),
                    op.get(2).and_then(|v| v.as_str()),
                    op.get(3).and_then(|v| v.as_str()),
                    op.get(4).and_then(|v| v.as_str()),
                )
                .unwrap();
        }
        "assign_cluster" => {
            store.assign_cluster(s(1), op[2].as_i64().unwrap()).unwrap();
        }
        "exec" => {
            store
                .exec(
                    s(1),
                    op.get(2).and_then(|a| a.as_array()).unwrap_or(&vec![]),
                )
                .unwrap();
        }
        "commit" => store.commit().unwrap(),
        "rollback" => store.rollback().unwrap(),
        other => panic!("unknown setup op: {other}"),
    }
}

/// Prepare the store and project dir for one scenario.
pub fn setup(scenario: &Value) -> (Store, PathBuf) {
    let dir = project_dir("stage", scenario["name"].as_str().unwrap_or("x"));
    let store = Store::open(&dir.join("sya.db"), false).unwrap();
    let setup = &scenario["setup"];
    for ddl in setup["ddl"].as_array().cloned().unwrap_or_default() {
        store.exec(ddl.as_str().unwrap(), &[]).unwrap();
    }
    store.commit().unwrap();
    for op in setup["ops"].as_array().cloned().unwrap_or_default() {
        apply_op(&store, op.as_array().unwrap());
    }
    apply_table_rows(&store, &setup["tables"]);
    if let Some(files) = scenario.get("files").and_then(|f| f.as_object()) {
        for (name, text) in files {
            std::fs::write(dir.join(name), text.as_str().unwrap()).unwrap();
        }
    }
    (store, dir)
}

fn decode_answer(item: &Value) -> Result<Value, ProviderError> {
    if let Some(msg) = item.get("error") {
        return Err(ProviderError::retryable(msg.as_str().unwrap_or_default()));
    }
    if let Some(msg) = item.get("error_ambiguous") {
        return Err(ProviderError::ambiguous(msg.as_str().unwrap_or_default()));
    }
    if let Some(err) = item.get("error_payload") {
        return Err(ProviderError::with_payload(
            err["message"].as_str().unwrap_or_default(),
            err["payload"].clone(),
        ));
    }
    Ok(item.get("ok").cloned().unwrap_or(Value::Null))
}

/// `FakeDemand` built from `providers.expand`/`providers.top` maps.
pub fn demand_of(scenario: &Value) -> FakeDemand {
    let mut fake = FakeDemand::default();
    let providers = &scenario["providers"];
    if let Some(map) = providers.get("expand").and_then(|m| m.as_object()) {
        for (key, answers) in map {
            let (phrase, regions) = key.split_once('@').unwrap();
            let entry = fake
                .expansions
                .entry((phrase.to_string(), regions.to_string()))
                .or_default();
            for ans in answers.as_array().unwrap() {
                entry.push_back(match decode_answer(ans) {
                    Ok(v) => Ok(ExpandResult {
                        pool: v["pool"]
                            .as_object()
                            .unwrap_or(&Map::new())
                            .iter()
                            .map(|(k, x)| (k.clone(), x.as_i64().unwrap_or(0)))
                            .collect(),
                        origin: v["origin"]
                            .as_object()
                            .unwrap_or(&Map::new())
                            .iter()
                            .map(|(k, x)| (k.clone(), x.as_str().unwrap().to_string()))
                            .collect(),
                        results: v["results"].as_i64().unwrap_or(0),
                        associations: v["associations"].as_i64().unwrap_or(0),
                    }),
                    Err(e) => Err(e),
                });
            }
        }
    }
    if let Some(map) = providers.get("top").and_then(|m| m.as_object()) {
        for (key, answers) in map {
            let (phrase, regions) = key.split_once('@').unwrap();
            let ans = &answers.as_array().unwrap()[0];
            fake.tops.insert(
                (phrase.to_string(), regions.to_string()),
                decode_answer(ans),
            );
        }
    }
    fake
}

/// `FakeSerp` built from the `providers.serp` map.
pub fn serp_of(scenario: &Value) -> FakeSerp {
    let mut fake = FakeSerp::default();
    if let Some(map) = scenario["providers"]
        .get("serp")
        .and_then(|m| m.as_object())
    {
        for (key, v) in map {
            let (phrase, region) = key.split_once('@').unwrap();
            fake.responses
                .insert((phrase.to_string(), region.to_string()), v.clone());
        }
    }
    fake
}

/// `FakeArsenkin` built from `providers.arsenkin` (`submits`/`waits`).
pub fn arsenkin_of(scenario: &Value) -> FakeArsenkin {
    let mut fake = FakeArsenkin {
        configured: true,
        ..FakeArsenkin::default()
    };
    let spec = &scenario["providers"]["arsenkin"];
    for ans in spec["submits"].as_array().cloned().unwrap_or_default() {
        fake.submits.push_back(decode_answer(&ans));
    }
    if let Some(waits) = spec.get("waits").and_then(|w| w.as_object()) {
        for (tid, answers) in waits {
            let q = fake.waits.entry(tid.clone()).or_default();
            for ans in answers.as_array().unwrap() {
                q.push_back(decode_answer(ans));
            }
        }
    }
    fake
}

/// `FakePages` built from the `providers.pages` map.
pub fn pages_of(scenario: &Value) -> FakePages {
    let mut fake = FakePages::default();
    if let Some(map) = scenario["providers"]
        .get("pages")
        .and_then(|m| m.as_object())
    {
        for (url, item) in map {
            let parts = if item.get("error").is_some() || item.is_null() {
                None
            } else {
                Some(spiderhead_semantics::providers::PageParts {
                    title: item["title"].as_str().unwrap_or_default().to_string(),
                    meta: item["meta"].as_str().unwrap_or_default().to_string(),
                    h1: item["h1"]
                        .as_array()
                        .map(|a| {
                            a.iter()
                                .filter_map(|v| v.as_str().map(String::from))
                                .collect()
                        })
                        .unwrap_or_default(),
                    h2: item["h2"]
                        .as_array()
                        .map(|a| {
                            a.iter()
                                .filter_map(|v| v.as_str().map(String::from))
                                .collect()
                        })
                        .unwrap_or_default(),
                })
            };
            fake.pages.insert(url.clone(), parts);
        }
    }
    fake
}

/// Demand fake's call log in the fixture's shape (`[["expand", p, r], ...]`).
pub fn demand_calls(fake: &FakeDemand) -> Value {
    Value::Array(
        fake.calls
            .iter()
            .map(|(m, p, r)| json!([m, p, r]))
            .collect(),
    )
}

/// SERP fake's call log (`[{"queries": [...], "region": r, "groups": g}]`).
pub fn serp_calls(fake: &FakeSerp) -> Value {
    Value::Array(
        fake.calls
            .iter()
            .map(|(qs, r, g)| json!({"queries": qs, "region": r, "groups": g}))
            .collect(),
    )
}

/// Arsenkin fake's submit log (`[[tool, payload], ...]`).
pub fn arsenkin_submits(fake: &FakeArsenkin) -> Value {
    Value::Array(fake.submitted.iter().map(|(t, p)| json!([t, p])).collect())
}

/// Files expected in `expect.files` vs the scenario dir contents.
pub fn check_files(scenario: &Value, dir: &Path, diffs: &mut Vec<String>) {
    for (name, want) in scenario["expect"]
        .get("files")
        .and_then(|f| f.as_object())
        .cloned()
        .unwrap_or_default()
    {
        let path = dir.join(&name);
        match std::fs::read_to_string(&path) {
            Ok(got) if got == want.as_str().unwrap_or_default() => {}
            Ok(got) => diffs.push(format!(
                "file {name}: {} vs {} chars",
                got.len(),
                want.as_str().unwrap_or_default().len()
            )),
            Err(e) => diffs.push(format!("file {name}: unreadable {e}")),
        }
    }
}

/// Compare `expect.tables` with a fresh dump; returns divergence lines.
pub fn check_tables(store: &Store, scenario: &Value, diffs: &mut Vec<String>) {
    let got = dump_db(store);
    let want = &scenario["expect"]["tables"];
    let names: std::collections::BTreeSet<String> = got
        .as_object()
        .unwrap()
        .keys()
        .chain(want.as_object().unwrap().keys())
        .cloned()
        .collect();
    for name in names {
        let g = &got[&name];
        let w = &want[&name];
        if g != w {
            let gn = g.as_array().map(|a| a.len()).unwrap_or(0);
            let wn = w.as_array().map(|a| a.len()).unwrap_or(0);
            diffs.push(format!("table {name}: {gn} rows vs oracle {wn}"));
            if let (Some(gr), Some(wr)) = (g.as_array(), w.as_array()) {
                for (i, (a, b)) in gr.iter().zip(wr.iter()).enumerate() {
                    if a != b {
                        diffs.push(format!("  row {i}: rust={a} oracle={b}"));
                        if i > 4 {
                            break;
                        }
                    }
                }
            }
        }
    }
}

/// Assert `result == expect.result` (objects order-insensitive).
pub fn check_result(scenario: &Value, result: &Value, diffs: &mut Vec<String>) {
    let want = &scenario["expect"]["result"];
    if result != want {
        diffs.push(format!("result: rust={result} oracle={want}"));
    }
}

/// Workbook cells as `{sheet: [[value, ...], ...]}` — the JSON shape
/// `gen_common.xlsx_cells` records via `openpyxl.iter_rows(values_only=True)`.
pub fn xlsx_cells(path: &Path) -> Value {
    use calamine::{open_workbook_auto, Data, Reader};
    let mut wb = open_workbook_auto(path).expect("open xlsx");
    let names = wb.sheet_names().to_vec();
    let mut out = Map::new();
    for name in names {
        let range = wb.worksheet_range(&name).expect("sheet range");
        let rows: Vec<Value> = range
            .rows()
            .map(|r| {
                Value::Array(
                    r.iter()
                        .map(|c| match c {
                            Data::Empty => Value::Null,
                            Data::Bool(b) => json!(b),
                            Data::Int(i) => json!(i),
                            Data::Float(f) => json!(f),
                            Data::String(s) => json!(s),
                            Data::DateTime(d) => json!(d.as_f64()),
                            Data::DateTimeIso(s) | Data::DurationIso(s) => json!(s),
                            Data::Error(e) => json!(format!("error:{e:?}")),
                        })
                        .collect(),
                )
            })
            .collect();
        out.insert(name, Value::Array(rows));
    }
    Value::Object(out)
}

/// Compare `expect.xlsx` with `xlsx_cells(path)`: numbers normalize through
/// `f64`, short rows pad with `null` (openpyxl pads to `max_column`).
pub fn check_xlsx(scenario: &Value, path: &Path, diffs: &mut Vec<String>) {
    let want = &scenario["expect"]["xlsx"];
    if want.is_null() {
        return;
    }
    let got = xlsx_cells(path);
    let norm = |v: &Value| -> Value {
        match v {
            Value::Number(n) if n.is_i64() || n.is_u64() => json!(n.as_f64()),
            other => other.clone(),
        }
    };
    for (sheet, want_rows) in want.as_object().unwrap() {
        let got_rows = got[sheet].as_array().cloned().unwrap_or_default();
        let want_rows = want_rows.as_array().cloned().unwrap_or_default();
        if got_rows.len() != want_rows.len() {
            diffs.push(format!(
                "xlsx {sheet}: {} rows vs oracle {}",
                got_rows.len(),
                want_rows.len()
            ));
        }
        for (i, wr) in want_rows.iter().enumerate() {
            let gr = got_rows.get(i).cloned().unwrap_or(Value::Array(vec![]));
            let (mut g, mut w) = (
                gr.as_array().cloned().unwrap_or_default(),
                wr.as_array().cloned().unwrap_or_default(),
            );
            while g.len() < w.len() {
                g.push(Value::Null);
            }
            while w.len() < g.len() {
                w.push(Value::Null);
            }
            for (j, (a, b)) in g.iter().zip(w.iter()).enumerate() {
                if norm(a) != norm(b) {
                    diffs.push(format!(
                        "xlsx {sheet} r{}c{}: rust={a} oracle={b}",
                        i + 1,
                        j + 1
                    ));
                }
            }
        }
    }
    for sheet in got.as_object().unwrap().keys() {
        if want.get(sheet).is_none() {
            diffs.push(format!("xlsx: extra sheet {sheet}"));
        }
    }
}
