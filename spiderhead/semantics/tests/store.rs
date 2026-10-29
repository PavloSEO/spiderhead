//! Store tests (the reference suite).

use indexmap::IndexMap;
use rusqlite::Connection;
use serde_json::{json, Value};
use spiderhead_semantics::store::{Row, Store};
use std::path::PathBuf;

fn tmp(name: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("semstore-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    d
}

#[test]
fn upsert_accumulates_the_highest_demand() {
    let s = Store::open_memory().unwrap();
    assert!(s
        .upsert("краулер сайта онлайн", Some(120), 0, Some("import"), "225")
        .unwrap());
    s.set_fields(
        "краулер сайта онлайн",
        &[
            ("impr".into(), json!(47)),
            ("pos_y".into(), json!(3)),
            ("pos_g".into(), json!(8)),
            ("exact".into(), Value::Null),
        ],
    )
    .unwrap();
    assert!(!s
        .upsert("краулер сайта онлайн", Some(12), 0, Some("collect"), "225")
        .unwrap());
    assert!(!s
        .upsert("краулер сайта онлайн", None, 0, Some("collect"), "225")
        .unwrap());
    let row = s
        .q1(
            "SELECT * FROM phrases WHERE norm=?",
            &[json!("краулер сайта онлайн")],
        )
        .unwrap()
        .unwrap();
    assert_eq!(row["base"], json!(120));
    assert_eq!(row["impr"], json!(47));
    assert_eq!(row["pos_y"], json!(3));
    assert_eq!(row["pos_g"], json!(8));
    assert!(row["exact"].is_null());
}

#[test]
fn legacy_database_migrates() {
    let dir = tmp("legacy");
    let db_path = dir.join("legacy.db");
    {
        let c = Connection::open(&db_path).unwrap();
        c.execute_batch(
            "CREATE TABLE phrases(
                id INTEGER PRIMARY KEY, norm TEXT NOT NULL UNIQUE, base INTEGER, quoted INTEGER, exact INTEGER,
                region TEXT, depth INTEGER, src TEXT, status TEXT, reason TEXT, reason_stage TEXT,
                route TEXT, lemma_group INTEGER, cluster_id INTEGER, first_seen TEXT, last_update TEXT
            );
            CREATE TABLE clusters(id INTEGER PRIMARY KEY, name TEXT, marker_norm TEXT, coherence REAL, is_real INTEGER, note TEXT);
            INSERT INTO phrases(norm, base, status) VALUES('парсер для seo', 91, 'kept');",
        )
        .unwrap();
    }
    let s = Store::open(&db_path, false).unwrap();
    let row = s
        .q1(
            "SELECT norm, base, status, impr, pos_y, pos_g, page_types FROM phrases",
            &[],
        )
        .unwrap()
        .unwrap();
    assert_eq!(row["norm"], json!("парсер для seo"));
    assert_eq!(row["base"], json!(91));
    assert_eq!(row["status"], json!("kept"));
    assert!(
        row["impr"].is_null()
            && row["pos_y"].is_null()
            && row["pos_g"].is_null()
            && row["page_types"].is_null()
    );
    let cols = s.columns("phrases").unwrap();
    for c in ["cluster_soft", "cluster_mid", "cluster_hard", "pos_serp"] {
        assert!(cols.iter().any(|x| x == c), "missing {c}");
    }
}

#[test]
fn export_keeps_nullable_columns() {
    let dir = tmp("export");
    let s = Store::open_memory().unwrap();
    s.upsert("краулер сайта", Some(55), 0, Some("webmaster"), "225")
        .unwrap();
    s.set_fields(
        "краулер сайта",
        &[
            ("impr".into(), json!(21)),
            ("pos_y".into(), json!(6)),
            ("pos_g".into(), Value::Null),
            ("quoted".into(), Value::Null),
            ("exact".into(), Value::Null),
        ],
    )
    .unwrap();
    let out = dir.join("export");
    s.export(&out, "all", false).unwrap();
    let rows: Vec<Value> =
        serde_json::from_str(&std::fs::read_to_string(out.join("pool.json")).unwrap()).unwrap();
    assert_eq!(rows[0]["impr"], json!(21));
    assert_eq!(rows[0]["pos_y"], json!(6));
    assert!(rows[0]["quoted"].is_null() && rows[0]["exact"].is_null());
    let csv = std::fs::read_to_string(out.join("pool.csv")).unwrap();
    let head: Vec<&str> = csv
        .trim_start_matches('\u{feff}')
        .lines()
        .next()
        .unwrap()
        .split(';')
        .collect();
    let body: Vec<&str> = csv.lines().nth(1).unwrap().split(';').collect();
    let at = |name: &str| body[head.iter().position(|h| *h == name).unwrap()];
    assert_eq!(at("impr"), "21");
    assert_eq!(at("pos_y"), "6");
    assert_eq!(at("exact"), "");
}

#[test]
fn own_domain_requires_hostname_boundary() {
    let s = Store::open_memory().unwrap();
    let docs: Vec<Row> = [
        ("https://example.invalid/a", "example.invalid", 1),
        ("https://sub.example.invalid/a", "sub.example.invalid", 2),
        (
            "https://example.invalid.other.invalid/a",
            "example.invalid.other.invalid",
            3,
        ),
    ]
    .iter()
    .map(|(url, domain, pos)| {
        Row::from([
            ("url".to_string(), json!(url)),
            ("domain".to_string(), json!(domain)),
            ("pos".to_string(), json!(pos)),
        ])
    })
    .collect();
    s.add_serp("synthetic phrase", &docs, None).unwrap();
    s.refresh_domains(Some("example.invalid"), &|_| false)
        .unwrap();
    let kinds: IndexMap<String, String> = s
        .q("SELECT domain, kind FROM domains", &[])
        .unwrap()
        .into_iter()
        .map(|r| {
            (
                r["domain"].as_str().unwrap().into(),
                r["kind"].as_str().unwrap().into(),
            )
        })
        .collect();
    assert_eq!(kinds["example.invalid"], "own");
    assert_eq!(kinds["sub.example.invalid"], "own");
    assert_eq!(kinds["example.invalid.other.invalid"], "competitor");
}

#[test]
fn stage_journal_and_landing_audit() {
    let s = Store::open_memory().unwrap();
    let cid = s
        .upsert_cluster("industrial pumps", Some("serp"), None, None)
        .unwrap();
    let run = s
        .start_stage("clean", Some(&json!({"request_estimate": {"count": 0}})))
        .unwrap();
    s.finish_stage(run, "done", Some(&json!({"kept": 2})), None)
        .unwrap();
    s.set_landing(cid, Some("https://example.test/pumps"))
        .unwrap();
    let state = s.stage_states(200).unwrap();
    assert_eq!(state["latest"]["clean"]["state"], json!("done"));
    assert!(state["latest"]["clean"]["started_at"]
        .as_str()
        .unwrap()
        .ends_with('Z'));
    assert_eq!(state["latest"]["clean"]["result"], json!({"kept": 2}));
    let audit = s
        .q(
            "SELECT action,before_json,after_json FROM semantic_audit",
            &[],
        )
        .unwrap();
    assert_eq!(audit.len(), 1);
    assert_eq!(audit[0]["action"], json!("set_landing"));
    let before: Value = serde_json::from_str(audit[0]["before_json"].as_str().unwrap()).unwrap();
    let after: Value = serde_json::from_str(audit[0]["after_json"].as_str().unwrap()).unwrap();
    assert_eq!(before, json!({"landing_url": null}));
    assert_eq!(after, json!({"landing_url": "https://example.test/pumps"}));
}

#[test]
fn scoped_export() {
    let dir = tmp("scoped");
    let s = Store::open_memory().unwrap();
    let cid = s
        .upsert_cluster("industrial pumps", None, None, None)
        .unwrap();
    s.upsert("industrial pumps", Some(240), 0, None, "225")
        .unwrap();
    s.assign_cluster("industrial pumps", cid).unwrap();
    s.upsert("water pumps", Some(80), 0, None, "225").unwrap();
    let plan = s
        .export(&dir.join("export"), "cluster:industrial pumps", true)
        .unwrap();
    assert_eq!(plan["rows"], json!(1));
    assert_eq!(plan["planned_files"].as_array().unwrap().len(), 2);
    assert!(!dir.join("export").exists());
    let written = s
        .export(&dir.join("scoped"), "cluster:industrial pumps", false)
        .unwrap();
    assert_eq!(written, json!(1));
    let rows: Vec<Value> =
        serde_json::from_str(&std::fs::read_to_string(dir.join("scoped/pool.json")).unwrap())
            .unwrap();
    assert_eq!(rows[0]["norm"], json!("industrial pumps"));
}

#[test]
fn csv_export_neutralizes_formula() {
    let dir = tmp("formula");
    let s = Store::open_memory().unwrap();
    s.upsert("synthetic formula phrase", None, 0, None, "225")
        .unwrap();
    s.set_fields(
        "synthetic formula phrase",
        &[(
            "comment".into(),
            json!("=HYPERLINK(\"https://example.test/\")"),
        )],
    )
    .unwrap();
    s.export(&dir.join("safe"), "all", false).unwrap();
    let csv = std::fs::read_to_string(dir.join("safe/pool.csv")).unwrap();
    let head: Vec<&str> = csv
        .trim_start_matches('\u{feff}')
        .lines()
        .next()
        .unwrap()
        .split(';')
        .collect();
    let body = csv.lines().nth(1).unwrap();
    let idx = head.iter().position(|h| *h == "comment").unwrap();
    let cells = parse_csv_line(body);
    assert!(cells[idx].starts_with("'="));
    let rows: Vec<Value> =
        serde_json::from_str(&std::fs::read_to_string(dir.join("safe/pool.json")).unwrap())
            .unwrap();
    assert!(rows[0]["comment"].as_str().unwrap().starts_with('='));
}

fn parse_csv_line(line: &str) -> Vec<String> {
    let mut cells = Vec::new();
    let mut cur = String::new();
    let mut in_q = false;
    let mut chars = line.chars().peekable();
    while let Some(c) = chars.next() {
        if in_q {
            if c == '"' {
                if chars.peek() == Some(&'"') {
                    chars.next();
                    cur.push('"');
                } else {
                    in_q = false;
                }
            } else {
                cur.push(c);
            }
        } else if c == '"' {
            in_q = true;
        } else if c == ';' {
            cells.push(std::mem::take(&mut cur));
        } else {
            cur.push(c);
        }
    }
    cells.push(cur);
    cells
}
