//! Differential test of retained link findings, the resource-graph extractor, the image-header
//! reader and `store_document`. the reference suite records the reference answers.

use std::path::PathBuf;

use rusqlite::Connection;
use serde_json::{json, Value};
use spiderhead_store::resource_graph::{extract, image_dimensions, store_document};
use spiderhead_store::retained_link_findings::RetainedLinkFindings;
use spiderhead_store::schema_sql::{SCAN_V1_SQL, SCAN_V2_SQL};

fn fixtures() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures")
}

fn expected() -> Value {
    let text = std::fs::read_to_string(fixtures().join("store-stubs-expected.json")).unwrap();
    serde_json::from_str(&text).unwrap()
}

fn scratch(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("sh-stubs-{}-{name}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

fn unhex(text: &str) -> Vec<u8> {
    (0..text.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&text[i..i + 2], 16).unwrap())
        .collect()
}

#[test]
fn link_findings_match_the_reference_for_every_limit() {
    let dir = scratch("links");
    let path = dir.join("scan.sqlite");
    std::fs::copy(fixtures().join("retained-links-pre.sqlite"), &path).unwrap();
    let want = expected();
    for (label, limit, hops) in [
        ("default", 200, 20),
        ("limit2_hops2", 2, 2),
        ("limit1_hops3", 1, 3),
    ] {
        let con = Connection::open(&path).unwrap();
        {
            let retained = RetainedLinkFindings::new(&con, limit, hops).unwrap();
            let got = json!({
                "status": retained.status_findings().unwrap(),
                "empty_anchor": retained.empty_anchor_findings().unwrap(),
                "redirect": retained.redirect_findings().unwrap(),
                "external": retained.external_findings().unwrap(),
                "all": retained.findings().unwrap(),
            });
            assert_eq!(got, want["links"][label], "scenario {label}");
        }
        let temp_tables: i64 = con
            .query_row(
                "SELECT COUNT(*) FROM sqlite_temp_master WHERE type='table'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(temp_tables, 0, "projection table must be dropped");
    }
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn projection_restores_query_only_mode() {
    let dir = scratch("queryonly");
    let path = dir.join("scan.sqlite");
    std::fs::copy(fixtures().join("retained-links-pre.sqlite"), &path).unwrap();
    let con = Connection::open(&path).unwrap();
    con.execute_batch("PRAGMA query_only=ON").unwrap();
    {
        let retained = RetainedLinkFindings::new(&con, 200, 20).unwrap();
        assert!(!retained.status_findings().unwrap().is_empty());
    }
    let mode: i64 = con
        .query_row("PRAGMA query_only", [], |r| r.get(0))
        .unwrap();
    assert_eq!(mode, 1);
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn extract_matches_the_reference() {
    for case in expected()["extract"].as_array().unwrap() {
        let html = case["html"].as_str();
        let (rows, omitted) = extract(
            html,
            case["base"].as_str().unwrap(),
            case["max"].as_i64().unwrap(),
        )
        .unwrap();
        let got: Vec<Value> = rows
            .iter()
            .map(|d| {
                json!({
                    "kind": d.kind, "carrier": d.carrier, "raw_url": d.raw_url,
                    "resolved_url": d.resolved_url, "integrity": d.integrity,
                    "integrity_state": d.integrity_state,
                })
            })
            .collect();
        assert_eq!(Value::Array(got), case["rows"], "html {html:?}");
        assert_eq!(omitted, case["omitted"].as_i64().unwrap(), "html {html:?}");
    }
}

#[test]
fn image_dimensions_match_the_reference() {
    for case in expected()["images"].as_array().unwrap() {
        let body = case["hex"].as_str().map(unhex);
        let (w, h) = image_dimensions(body.as_deref()).unwrap();
        let want: Vec<Option<i64>> = case["dims"]
            .as_array()
            .unwrap()
            .iter()
            .map(Value::as_i64)
            .collect();
        assert_eq!(vec![w, h], want, "image {}", case["name"]);
    }
}

fn row_values(con: &Connection, sql: &str) -> Vec<Value> {
    let mut stmt = con.prepare(sql).unwrap();
    let names: Vec<String> = stmt.column_names().iter().map(|s| s.to_string()).collect();
    stmt.query_map([], |r| {
        let mut map = serde_json::Map::new();
        for (i, name) in names.iter().enumerate() {
            let value: rusqlite::types::Value = r.get(i)?;
            map.insert(
                name.clone(),
                match value {
                    rusqlite::types::Value::Null => Value::Null,
                    rusqlite::types::Value::Integer(n) => json!(n),
                    rusqlite::types::Value::Real(f) => json!(f),
                    rusqlite::types::Value::Text(t) => json!(t),
                    rusqlite::types::Value::Blob(_) => Value::Null,
                },
            );
        }
        Ok(Value::Object(map))
    })
    .unwrap()
    .map(Result::unwrap)
    .collect()
}

#[test]
fn store_document_matches_the_reference() {
    let con = Connection::open_in_memory().unwrap();
    con.execute_batch(SCAN_V1_SQL).unwrap();
    con.execute_batch(SCAN_V2_SQL).unwrap();
    con.execute_batch("PRAGMA user_version=2").unwrap();
    con.execute(
        "INSERT INTO urls(url) VALUES ('https://example.test/page')",
        [],
    )
    .unwrap();
    con.execute(
        "INSERT INTO documents(url_id,representation,captured_at,decoder_version,decoder_source,\
         decoder_charset,decoder_errors,fidelity,body_state,body_reason,renderer_json) \
         VALUES (1,'static','2026-01-01T00:00:00Z','scan_decoder.v1','not_applicable','',\
         'not_applicable','unavailable','unavailable','none','{}')",
        [],
    )
    .unwrap();
    let want = &expected()["store_document"];
    let steps = [
        (
            "complete",
            Some("<img src=\"/a.png\"><script src=\"b.js\"></script>"),
            "static",
            1,
        ),
        (
            "replace",
            Some("<link rel=\"stylesheet\" href=\"/c.css\">"),
            "static",
            1,
        ),
        ("empty", Some("<p>none</p>"), "static", 1),
        ("unavailable", None, "static", 1),
        ("bad_representation", Some("<p/>"), "nope", 1),
        ("bad_binding", Some("<p/>"), "static", 99),
        (
            "final",
            Some("<img src=\"/z.png\" srcset=\"y.png 1x\">"),
            "static",
            1,
        ),
    ];
    for (i, (name, html, representation, document)) in steps.iter().enumerate() {
        let step = &want["steps"][i];
        assert_eq!(step["name"], *name);
        let result = store_document(&con, 1, *document, representation, *html);
        match result {
            Ok(payload) => {
                assert!(step["ok"].as_bool().unwrap(), "{name}");
                let got = serde_json::to_value(payload.unwrap()).unwrap();
                assert_eq!(got, step["payload"], "{name}");
            }
            Err(err) => {
                assert!(!step["ok"].as_bool().unwrap(), "{name}");
                assert!(
                    err.to_string().contains(step["error"].as_str().unwrap()),
                    "{name}: {err}"
                );
            }
        }
    }
    let occurrences = row_values(
        &con,
        "SELECT * FROM resource_graph_occurrences ORDER BY occurrence_id",
    );
    assert_eq!(Value::Array(occurrences), want["occurrences"]);
    let items = row_values(&con, "SELECT * FROM context_items ORDER BY kind,item_key");
    assert_eq!(Value::Array(items), want["context_items"]);
}

#[test]
fn store_document_is_a_no_op_on_v1() {
    let con = Connection::open_in_memory().unwrap();
    con.execute_batch(SCAN_V1_SQL).unwrap();
    assert!(
        store_document(&con, 1, 1, "static", Some("<img src=/a.png>"))
            .unwrap()
            .is_none()
    );
}
