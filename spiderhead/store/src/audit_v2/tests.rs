//! audit.v2 tests: read/write round trips on fixture scans, plus regression cases.

use super::*;
use indexmap::IndexMap;
use regex::Regex;
use serde_json::json;
use std::collections::HashMap;
use std::path::PathBuf;

fn repo(rel: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .join(rel)
}

const SITES: [(&str, u16); 6] = [
    ("basic", 18701),
    ("errors", 18702),
    ("redirects", 18703),
    ("canonical-hreflang", 18704),
    ("schema", 18705),
    ("robots-sitemap", 18706),
];

struct Scratch(PathBuf);

impl Scratch {
    fn new(tag: &str) -> Self {
        let dir = std::env::temp_dir().join(format!(
            "spiderhead-audit-v2-{}-{tag}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map_or(0, |d| d.as_nanos())
        ));
        fs::create_dir_all(&dir).unwrap();
        Self(dir)
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

/// A copy of the `basic` fixture scan (no companion) and its binding.
fn fixture_scan(dir: &Path) -> (PathBuf, Map<String, Value>) {
    let scan = dir.join("scan.sqlite");
    fs::copy(repo("oracle/raw-scans/basic/scan.sqlite"), &scan).unwrap();
    let con =
        Connection::open_with_flags(&scan, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY).unwrap();
    let (uuid, rev, ver, wrev): (String, i64, String, String) = con
        .query_row(
            "SELECT scan_uuid,evidence_revision,writer_version,writer_revision FROM scan WHERE singleton=1",
            [],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
        )
        .unwrap();
    let binding = json!({"scan_uuid": uuid, "evidence_revision": rev, "analyzer_version": ver, "analyzer_revision": wrev});
    (scan, binding.as_object().unwrap().clone())
}

fn obj(v: Value) -> Map<String, Value> {
    v.as_object().unwrap().clone()
}

fn colls(items: Vec<(&str, Vec<Value>)>) -> IndexMap<String, Vec<Value>> {
    items.into_iter().map(|(k, v)| (k.to_owned(), v)).collect()
}

fn err_text<T>(r: Result<T, StoreError>) -> String {
    match r {
        Ok(_) => panic!("expected an error"),
        Err(e) => e.to_string(),
    }
}

fn sql(scan: &Path, statement: &str) {
    let con = Connection::open(audit_v2_path(scan)).unwrap();
    con.execute_batch(statement).unwrap();
}

// ---- fixture round trips ----

struct Norm {
    base: String,
    uuids: HashMap<String, usize>,
    sha: HashMap<String, String>,
    re: [Regex; 5],
}

impl Norm {
    fn new(base: String, sha: HashMap<String, String>) -> Self {
        let re = [
            r"127\.0\.0\.1:\d+",
            r"[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}",
            r"(evidence:v1:<UUID:\d+>:\w+:)[0-9a-f]{24}",
            r"\d{4}-\d\d-\d\d[T ]\d\d:\d\d:\d\d(?:\.\d+)?(?:Z|[+-]\d\d:?\d\d)?",
            r"\b[0-9a-f]{64}\b",
        ]
        .map(|p| Regex::new(p).unwrap());
        Self {
            base,
            uuids: HashMap::new(),
            sha,
            re,
        }
    }

    fn s(&mut self, v: &str) -> String {
        let v = v.replace(&self.base, "{BASE}");
        let v = self.re[0].replace_all(&v, "127.0.0.1:{PORT}").into_owned();
        let mut out = String::new();
        let mut last = 0;
        for m in self.re[1].find_iter(&v) {
            out.push_str(&v[last..m.start()]);
            let n = self.uuids.len() + 1;
            let n = *self.uuids.entry(m.as_str().to_owned()).or_insert(n);
            out.push_str(&format!("<UUID:{n}>"));
            last = m.end();
        }
        out.push_str(&v[last..]);
        let v = self.re[2].replace_all(&out, "${1}<HASH>").into_owned();
        let v = self.re[3].replace_all(&v, "<TS>").into_owned();
        let sha = &self.sha;
        self.re[4]
            .replace_all(&v, |c: &regex::Captures| {
                sha.get(&c[0]).cloned().unwrap_or_else(|| c[0].to_owned())
            })
            .into_owned()
    }

    fn walk(&mut self, value: &Value, key: Option<&str>) -> Value {
        const VOLATILE: [&str; 11] = [
            "elapsed_ms",
            "ttfb_ms",
            "response_time",
            "elapsed_seconds",
            "duration",
            "duration_ms",
            "latency_ms",
            "total_ms",
            "time_ms",
            "delay_seconds",
            "crawl_delay_applied_at",
        ];
        match value {
            Value::Object(m) => Value::Object(
                m.iter()
                    .map(|(k, v)| (k.clone(), self.walk(v, Some(k))))
                    .collect(),
            ),
            Value::Array(a) => Value::Array(a.iter().map(|v| self.walk(v, None)).collect()),
            Value::Number(_) if key.is_some_and(|k| VOLATILE.contains(&k)) => json!("<DURATION>"),
            Value::String(_) if matches!(key, Some("config_fingerprint")) => json!("<FINGERPRINT>"),
            Value::String(_) if matches!(key, Some("sha256")) => json!("<SHA256>"),
            Value::String(_) if matches!(key, Some("input")) => json!("<INPUT>"),
            Value::String(_) if matches!(key, Some("digest")) => json!("<DIGEST>"),
            Value::String(s) => Value::String(self.s(s)),
            other => other.clone(),
        }
    }
}

/// Body-hash re-keying of `scan.tables.json`: decoded body with the origin replaced, re-hashed.
fn body_hash_map(scan: &Path, base: &str) -> HashMap<String, String> {
    use sha2::{Digest, Sha256};
    let con =
        Connection::open_with_flags(scan, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY).unwrap();
    let hashes: Vec<String> = con
        .prepare("SELECT sha256 FROM bodies ORDER BY rowid")
        .unwrap()
        .query_map([], |r| r.get(0))
        .unwrap()
        .map(Result::unwrap)
        .collect();
    hashes
        .into_iter()
        .map(|old| {
            let raw = crate::bodies::read_body(&con, &old, 1 << 30).unwrap();
            let needle = base.as_bytes();
            let mut data = Vec::new();
            let mut i = 0;
            while i < raw.len() {
                if raw[i..].starts_with(needle) {
                    data.extend_from_slice(b"{BASE}");
                    i += needle.len();
                } else {
                    data.push(raw[i]);
                    i += 1;
                }
            }
            let new: String = Sha256::digest(&data)
                .iter()
                .map(|b| format!("{b:02x}"))
                .collect();
            (old, new)
        })
        .collect()
}

/// Path and values of the first difference between two JSON values.
fn first_diff(a: &Value, b: &Value, path: &str) -> String {
    match (a, b) {
        (Value::Object(x), Value::Object(y)) => {
            for (k, v) in x {
                match y.get(k) {
                    Some(w) if v != w => return first_diff(v, w, &format!("{path}/{k}")),
                    None => return format!("{path}/{k} only in actual"),
                    _ => {}
                }
            }
            y.keys()
                .find(|k| !x.contains_key(*k))
                .map_or("?".into(), |k| format!("{path}/{k} only in baseline"))
        }
        (Value::Array(x), Value::Array(y)) if x.len() == y.len() => {
            let i = x.iter().zip(y).position(|(p, q)| p != q).unwrap_or(0);
            first_diff(&x[i], &y[i], &format!("{path}[{i}]"))
        }
        _ => format!("{path}: actual {a} vs baseline {b}"),
    }
}

fn actual_document(scan: &Path) -> Value {
    let reader = AuditV2Reader::new(scan, true, false, None).unwrap();
    let con = Connection::open_with_flags(
        audit_v2_path(scan),
        rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
    )
    .unwrap();
    let mut items = Map::new();
    // Grouping by `items` rows means empty collections have no entry.
    for pointer in reader
        .collections
        .iter()
        .filter(|(_, n)| **n > 0)
        .map(|(p, _)| p)
    {
        items.insert(
            pointer.clone(),
            Value::Array(reader.iter_collection(pointer).unwrap()),
        );
    }
    let mut members = Vec::new();
    let mut stmt = con
        .prepare(
            "SELECT pointer,group_ordinal,ordinal,value_json FROM group_members ORDER BY rowid",
        )
        .unwrap();
    let mut rows = stmt.query([]).unwrap();
    while let Some(r) = rows.next().unwrap() {
        let mut m = Map::new();
        m.insert("pointer".into(), json!(r.get::<_, String>(0).unwrap()));
        m.insert("group_ordinal".into(), json!(r.get::<_, i64>(1).unwrap()));
        m.insert("ordinal".into(), json!(r.get::<_, i64>(2).unwrap()));
        m.insert(
            "value_json".into(),
            serde_json::from_str(&r.get::<_, String>(3).unwrap()).unwrap(),
        );
        members.push(Value::Object(m));
    }
    let mut doc = Map::new();
    doc.insert("format_version".into(), json!(FORMAT_VERSION));
    doc.insert("binding".into(), reader.binding.clone());
    doc.insert("header".into(), reader.header.clone());
    doc.insert(
        "collections".into(),
        Value::Object(
            reader
                .collections
                .iter()
                .map(|(k, v)| (k.clone(), json!(v)))
                .collect(),
        ),
    );
    doc.insert("items".into(), Value::Object(items));
    doc.insert("group_members".into(), Value::Array(members));
    Value::Object(doc)
}

#[test]
fn reader_matches_oracle_baselines_for_every_site() {
    for (site, port) in SITES {
        let dir = Scratch::new(site);
        let scan = dir.0.join("scan.sqlite");
        fs::copy(repo(&format!("oracle/raw-scans/{site}/scan.sqlite")), &scan).unwrap();
        fs::copy(
            repo(&format!(
                "oracle/raw-scans/{site}/scan.sqlite.audit-v2.sqlite"
            )),
            audit_v2_path(&scan),
        )
        .unwrap();
        let base = format!("http://127.0.0.1:{port}");
        let mut norm = Norm::new(base.clone(), body_hash_map(&scan, &base));
        let got = norm.walk(&actual_document(&scan), None);
        let want: Value = serde_json::from_str(
            &fs::read_to_string(repo(&format!("oracle/baselines/{site}/audit.v2.json"))).unwrap(),
        )
        .unwrap();
        if got != want {
            panic!(
                "audit.v2 baseline divergence at {site}: {}",
                first_diff(&got, &want, "")
            );
        }
    }
}

#[test]
fn writer_reproduces_oracle_companions_byte_for_byte() {
    for (site, _) in SITES {
        let dir = Scratch::new(site);
        let scan = dir.0.join("scan.sqlite");
        fs::copy(repo(&format!("oracle/raw-scans/{site}/scan.sqlite")), &scan).unwrap();
        fs::copy(
            repo(&format!(
                "oracle/raw-scans/{site}/scan.sqlite.audit-v2.sqlite"
            )),
            audit_v2_path(&scan),
        )
        .unwrap();
        let reader = AuditV2Reader::new(&scan, true, false, None).unwrap();
        // Rebuild the writer input: header with arrays, collections in first-seen rowid order.
        let mut header = reader.header.clone();
        let mut order: IndexMap<String, Vec<Value>> = IndexMap::new();
        let con = Connection::open_with_flags(
            audit_v2_path(&scan),
            rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
        )
        .unwrap();
        let pointers: Vec<String> = con
            .prepare("SELECT pointer FROM collections ORDER BY rowid")
            .unwrap()
            .query_map([], |r| r.get(0))
            .unwrap()
            .map(Result::unwrap)
            .collect();
        for p in pointers {
            order.entry(p).or_default();
        }
        for (pointer, rows) in order.iter_mut() {
            for row in reader.iter_collection(pointer).unwrap() {
                let mut row = row;
                if let Some(members) = reader.group_members(&row) {
                    let urls = Value::Array(members.to_vec().unwrap());
                    row.as_object_mut().unwrap().insert("urls".into(), urls);
                }
                rows.push(row);
            }
            set_pointer(&mut header, pointer, Value::Array(Vec::new())).unwrap();
        }
        let before: Vec<(String, String)> = {
            let mut stmt = con
                .prepare("SELECT header_json,sha256 FROM audit_meta")
                .unwrap();
            stmt.query_map([], |r| Ok((r.get(0)?, r.get(1)?)))
                .unwrap()
                .map(Result::unwrap)
                .collect()
        };
        let items_before = dump_rows(
            &con,
            "SELECT pointer,ordinal,value_json FROM items ORDER BY rowid",
        );
        let members_before = dump_rows(
            &con,
            "SELECT group_ordinal,ordinal,value_json FROM group_members ORDER BY rowid",
        );
        drop(con);
        drop(reader);
        let binding = obj(AuditV2Reader::new(&scan, true, false, None)
            .unwrap()
            .binding
            .clone());
        write_audit_v2(&scan, header.as_object().unwrap(), &order, &binding).unwrap();
        let con = Connection::open_with_flags(
            audit_v2_path(&scan),
            rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
        )
        .unwrap();
        let after: Vec<(String, String)> = {
            let mut stmt = con
                .prepare("SELECT header_json,sha256 FROM audit_meta")
                .unwrap();
            stmt.query_map([], |r| Ok((r.get(0)?, r.get(1)?)))
                .unwrap()
                .map(Result::unwrap)
                .collect()
        };
        assert_eq!(before, after, "header/sha256 at {site}");
        assert_eq!(
            items_before,
            dump_rows(
                &con,
                "SELECT pointer,ordinal,value_json FROM items ORDER BY rowid"
            ),
            "{site}"
        );
        assert_eq!(
            members_before,
            dump_rows(
                &con,
                "SELECT group_ordinal,ordinal,value_json FROM group_members ORDER BY rowid"
            ),
            "{site}"
        );
    }
}

fn dump_rows(con: &Connection, query: &str) -> Vec<Vec<String>> {
    let mut stmt = con.prepare(query).unwrap();
    let n = stmt.column_count();
    stmt.query_map([], |r| {
        (0..n)
            .map(|i| {
                r.get::<_, rusqlite::types::Value>(i)
                    .map(|v| format!("{v:?}"))
            })
            .collect()
    })
    .unwrap()
    .map(Result::unwrap)
    .collect()
}

// ---- regression tests ----

#[test]
fn reopens_ordered_collections_and_complete_document() {
    let dir = Scratch::new("reopen");
    let (scan, binding) = fixture_scan(&dir.0);
    let header = obj(json!({
        "schema_version": "2.0", "run": {"source": "https://example.test/", "crawl_partial": false},
        "issues": [], "pages": [], "groups": [], "summary": {"sitemap": {"linked_not_in_sitemap": []}}
    }));
    let issues: Vec<Value> = (0..10000)
        .map(
            |i| json!({"check": "TEST", "target_url": format!("https://example.test/{i}"), "n": i}),
        )
        .collect();
    let pages: Vec<Value> = (0..3)
        .map(|i| json!({"url": format!("https://example.test/{i}")}))
        .collect();
    let sitemap = vec![json!({"url": "https://example.test/orphan"})];
    let groups = vec![json!({"check": "TEST", "count": 1})];
    let written = write_audit_v2(
        &scan,
        &header,
        &colls(vec![
            ("/issues", issues),
            ("/pages", pages.clone()),
            ("/summary/sitemap/linked_not_in_sitemap", sitemap.clone()),
            ("/groups", groups.clone()),
        ]),
        &binding,
    )
    .unwrap();
    assert_eq!(written, audit_v2_path(&scan));
    let reader = AuditV2Reader::new(&scan, true, false, None).unwrap();
    assert_eq!(reader.count("/issues").unwrap(), 10000);
    assert_eq!(reader.count("/pages").unwrap(), 3);
    assert_eq!(reader.count("/groups").unwrap(), 1);
    assert_eq!(reader.iter_collection("/pages").unwrap(), pages);
    let doc = reader.materialize_legacy(64 * 1024 * 1024).unwrap();
    assert_eq!(doc["issues"].as_array().unwrap().len(), 10000);
    assert!(doc["issues"][0]["target_url"]
        .as_str()
        .unwrap()
        .ends_with("/0"));
    assert_eq!(doc["issues"][9999]["n"], 9999);
    assert_eq!(
        doc["summary"]["sitemap"]["linked_not_in_sitemap"],
        json!(sitemap)
    );
    assert_eq!(doc["groups"], json!(groups));
}

#[test]
fn enforces_legacy_export_limit_while_streaming() {
    let dir = Scratch::new("limit");
    let (scan, binding) = fixture_scan(&dir.0);
    let rows: Vec<Value> = (0..100)
        .map(|_| json!({"message": "x".repeat(1024)}))
        .collect();
    write_audit_v2(
        &scan,
        &obj(json!({"issues": [], "pages": []})),
        &colls(vec![("/issues", rows)]),
        &binding,
    )
    .unwrap();
    let reader = AuditV2Reader::new(&scan, true, false, None).unwrap();
    assert!(err_text(reader.document_chunks(Some(4096))).contains("legacy JSON export exceeds"));
    assert!(err_text(reader.materialize_legacy(4096)).contains("legacy JSON export exceeds"));
}

#[test]
fn refuses_wrong_scan_binding_and_tampering() {
    let dir = Scratch::new("binding");
    let (scan, binding) = fixture_scan(&dir.0);
    let mut wrong = binding.clone();
    wrong.insert(
        "scan_uuid".into(),
        json!("00000000-0000-0000-0000-000000000000"),
    );
    let header = obj(json!({"issues": []}));
    assert!(err_text(write_audit_v2(
        &scan,
        &header,
        &colls(vec![("/issues", vec![])]),
        &wrong
    ))
    .contains("binding disagrees"));
    write_audit_v2(
        &scan,
        &header,
        &colls(vec![("/issues", vec![json!({"check": "X"})])]),
        &binding,
    )
    .unwrap();
    sql(&scan, "UPDATE items SET value_json='{}'");
    assert!(err_text(AuditV2Reader::new(&scan, true, false, None)).contains("hash"));
}

#[test]
fn saves_and_reopens_populations_above_ten_thousand() {
    let dir = Scratch::new("big");
    let (scan, binding) = fixture_scan(&dir.0);
    let rows: Vec<Value> = (0..50001)
        .map(|i| json!({"check": "HIGH_VOLUME", "target_url": format!("https://example.test/{i}")}))
        .collect();
    write_audit_v2(
        &scan,
        &obj(json!({"issues": []})),
        &colls(vec![("/issues", rows)]),
        &binding,
    )
    .unwrap();
    let reader = AuditV2Reader::new(&scan, true, false, None).unwrap();
    assert_eq!(reader.count("/issues").unwrap(), 50001);
    assert_eq!(reader.iter_collection("/issues").unwrap().len(), 50001);
}

#[test]
fn sitemap_url_lists_move_out_of_the_header_and_come_back() {
    let dir = Scratch::new("sitemap");
    let (scan, binding) = fixture_scan(&dir.0);
    let pointer = "/summary/sitemap/linked_not_in_sitemap";
    let urls = vec![
        json!("https://example.test/a"),
        json!("https://example.test/b"),
    ];
    let header =
        obj(json!({"summary": {"sitemap": {"urls_in_sitemap": 2, "linked_not_in_sitemap": []}}}));
    write_audit_v2(
        &scan,
        &header,
        &colls(vec![(pointer, urls.clone())]),
        &binding,
    )
    .unwrap();
    let reader = AuditV2Reader::new(&scan, true, false, None).unwrap();
    assert_eq!(
        get_pointer(&reader.header, pointer).unwrap(),
        &json!({COLLECTION_MARKER: pointer, "count": 2})
    );
    assert_eq!(
        reader.materialize_legacy(1 << 20).unwrap()["summary"]["sitemap"]["linked_not_in_sitemap"],
        json!(urls)
    );
}

fn group_scan(tag: &str, groups: Vec<Value>) -> (Scratch, PathBuf, Map<String, Value>) {
    let dir = Scratch::new(tag);
    let (scan, binding) = fixture_scan(&dir.0);
    write_audit_v2(
        &scan,
        &obj(json!({"groups": []})),
        &colls(vec![("/groups", groups)]),
        &binding,
    )
    .unwrap();
    (dir, scan, binding)
}

#[test]
fn large_group_streams_members_and_complete_json() {
    let urls: Vec<Value> = (0..25_000)
        .map(|i| json!(format!("https://example.test/{i:06}/{}", "x".repeat(400))))
        .collect();
    let (_d, scan, _) = group_scan(
        "large",
        vec![json!({"group_id": "GROUP", "count": 25000, "urls": urls})],
    );
    let reader = AuditV2Reader::new(&scan, true, false, None).unwrap();
    assert_eq!(reader.storage_version, 3);
    let group = reader.get_item("/groups", 0).unwrap();
    let members = reader.group_members(&group).unwrap();
    assert_eq!(members.len(), 25_000);
    let page = reader
        .group_members_page(0, 24_997, 100, 1024 * 1024)
        .unwrap();
    assert_eq!(page["rows"], json!(urls[24_997..]));
    let longest: i64 = reader
        .con
        .query_row("SELECT MAX(length(value_json)) FROM items", [], |r| {
            r.get(0)
        })
        .unwrap();
    assert!(longest < 1024);
    let stored: i64 = reader
        .con
        .query_row("SELECT COUNT(*) FROM group_members", [], |r| r.get(0))
        .unwrap();
    assert_eq!(stored, 25_000);
    assert!(err_text(reader.materialize_legacy(1024)).contains("legacy JSON export exceeds"));
    let document = reader.document_chunks(None).unwrap().concat();
    let expected = format!(
        r#"{{"groups":[{{"group_id":"GROUP","count":25000,"urls":[{}]}}]}}"#,
        urls.iter()
            .map(|u| dumps(u, false))
            .collect::<Vec<_>>()
            .join(",")
    );
    assert_eq!(document, expected);
    assert!(document.len() > 8 * 1024 * 1024);
}

#[test]
fn group_member_corruption_refuses_before_exposing_a_row() {
    for mutation in [
        "DELETE FROM group_members WHERE ordinal=1",
        "UPDATE group_members SET ordinal=9 WHERE ordinal=1",
        "UPDATE group_members SET value_json='\"changed\"' WHERE ordinal=1",
        r#"UPDATE items SET value_json='{"urls":{"$audit_v2_group_members":1,"count":2}}'"#,
    ] {
        let (_d, scan, _) = group_scan("corrupt", vec![json!({"urls": ["a", "b"]})]);
        let con = Connection::open(audit_v2_path(&scan)).unwrap();
        con.execute_batch("PRAGMA foreign_keys=OFF").unwrap();
        con.execute_batch(mutation).unwrap();
        drop(con);
        assert!(
            matches!(
                AuditV2Reader::new(&scan, true, false, None),
                Err(StoreError::AuditV2Error(_))
            ),
            "{mutation}"
        );
    }
}

#[test]
fn group_metadata_is_literal_and_supplied_marker_is_rejected() {
    let dir = Scratch::new("literal");
    let (scan, binding) = fixture_scan(&dir.0);
    let group = json!({"urls": ["a"], "metadata": {"$audit_v2_collection": "/issues", "count": 1}});
    write_audit_v2(
        &scan,
        &obj(json!({"groups": [], "issues": []})),
        &colls(vec![
            ("/groups", vec![group.clone()]),
            ("/issues", vec![json!({"n": 1})]),
        ]),
        &binding,
    )
    .unwrap();
    let reader = AuditV2Reader::new(&scan, true, false, None).unwrap();
    assert_eq!(
        reader.materialize_legacy(1 << 20).unwrap()["groups"],
        json!([group])
    );
    drop(reader);
    let prior = fs::read(audit_v2_path(&scan)).unwrap();
    let bad = json!({"urls": {"$audit_v2_group_members": 0, "count": 0}});
    let text = err_text(write_audit_v2(
        &scan,
        &obj(json!({"groups": []})),
        &colls(vec![("/groups", vec![bad])]),
        &binding,
    ));
    assert!(text.contains("reserved"), "{text}");
    assert_eq!(fs::read(audit_v2_path(&scan)).unwrap(), prior);
}

#[test]
fn group_slices_never_silently_shorten() {
    let (_d, scan, _) = group_scan(
        "slices",
        vec![json!({"urls": ["x".repeat(600_000), "y".repeat(600_000)]})],
    );
    let reader = AuditV2Reader::new(&scan, true, false, None).unwrap();
    let page = reader.group_members_page(0, 0, 2, 1024 * 1024).unwrap();
    assert_eq!(page["returned"], 1);
    assert_eq!(page["has_more"], true);
    let group = reader.get_item("/groups", 0).unwrap();
    let members = reader.group_members(&group).unwrap();
    assert!(err_text(members.slice(0, 2)).contains("byte bound"));
}

#[test]
fn fractional_member_ordinal_refuses_with_valid_digest() {
    let (_d, scan, _) = group_scan("fraction", vec![json!({"urls": ["a", "b", "c"]})]);
    let con = Connection::open(audit_v2_path(&scan)).unwrap();
    con.execute_batch(
        "PRAGMA foreign_keys=OFF; UPDATE group_members SET ordinal=0.5 WHERE ordinal=1",
    )
    .unwrap();
    let (b, h): (String, String) = con
        .query_row("SELECT binding_json,header_json FROM audit_meta", [], |r| {
            Ok((r.get(0)?, r.get(1)?))
        })
        .unwrap();
    let mut digest = digest_start(&b, &h, 3);
    {
        let mut stmt = con
            .prepare("SELECT pointer,ordinal,value_json FROM items ORDER BY pointer,ordinal")
            .unwrap();
        let mut rows = stmt.query([]).unwrap();
        while let Some(r) = rows.next().unwrap() {
            digest_item(
                &mut digest,
                &r.get::<_, String>(0).unwrap(),
                r.get::<_, i64>(1).unwrap(),
                &r.get::<_, String>(2).unwrap(),
            );
        }
    }
    {
        let mut stmt = con.prepare("SELECT group_ordinal,CAST(ordinal AS TEXT),value_json FROM group_members ORDER BY group_ordinal,ordinal").unwrap();
        let mut rows = stmt.query([]).unwrap();
        while let Some(r) = rows.next().unwrap() {
            // The float is hashed as its text form, since the integer slot cannot carry it.
            let texts = [
                format!("@group-members/{}", r.get::<_, i64>(0).unwrap()),
                r.get::<_, String>(1).unwrap(),
                r.get::<_, String>(2).unwrap(),
            ];
            for text in texts {
                sha2::Digest::update(&mut digest, (text.len() as u64).to_be_bytes());
                sha2::Digest::update(&mut digest, text.as_bytes());
            }
        }
    }
    let sha: String = sha2::Digest::finalize(digest)
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect();
    con.execute("UPDATE audit_meta SET sha256=?", [sha])
        .unwrap();
    drop(con);
    assert!(err_text(AuditV2Reader::new(&scan, true, false, None))
        .contains("ordinals must be integers"));
}

#[test]
fn storage_revision_two_keeps_its_digest_and_group_values() {
    let dir = Scratch::new("rev2");
    let (scan, binding) = fixture_scan(&dir.0);
    let group = json!({"group_id": "OLD", "count": 2, "urls": ["https://example.test/\u{439}", "https://example.test/a/"]});
    let header = json!({"groups": {COLLECTION_MARKER: "/groups", "count": 1}});
    let (bj, hj, rj) = (
        canonical(&Value::Object(binding)).unwrap(),
        canonical(&header).unwrap(),
        canonical(&group).unwrap(),
    );
    let mut digest = Sha256::new();
    for text in ["audit.v2", &bj, &hj, "/groups", "0", &rj] {
        digest.update((text.len() as u64).to_be_bytes());
        digest.update(text.as_bytes());
    }
    let sha: String = digest
        .finalize()
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect();
    let con = Connection::open(audit_v2_path(&scan)).unwrap();
    for stmt in &SCHEMA[..3] {
        con.execute_batch(stmt).unwrap();
    }
    con.execute_batch(&format!(
        "PRAGMA application_id={APPLICATION_ID}; PRAGMA user_version=2"
    ))
    .unwrap();
    con.execute(
        "INSERT INTO audit_meta VALUES(1,'audit.v2',?,?,?)",
        params![bj, hj, sha],
    )
    .unwrap();
    con.execute("INSERT INTO collections VALUES('/groups',1)", [])
        .unwrap();
    con.execute("INSERT INTO items VALUES('/groups',0,?)", [&rj])
        .unwrap();
    drop(con);
    let before = fs::read(audit_v2_path(&scan)).unwrap();
    let reader = AuditV2Reader::new(&scan, true, false, None).unwrap();
    assert_eq!(reader.storage_version, 2);
    assert_eq!(reader.sha256, sha);
    assert_eq!(
        reader.materialize_legacy(1 << 20).unwrap(),
        json!({"groups": [group]})
    );
    assert_eq!(
        reader.group_members_page(0, 1, 100, 1 << 20).unwrap()["rows"],
        json!(["https://example.test/a/"])
    );
    drop(reader);
    assert_eq!(fs::read(audit_v2_path(&scan)).unwrap(), before);
}

// ---- helper unit tests ----

#[test]
fn pointer_helpers_follow_python_semantics() {
    assert_eq!(pointer_parts("/a~1b/~0").unwrap(), vec!["a/b", "~"]);
    assert!(pointer_parts("a").is_err());
    assert!(pointer_parts("/a~2").is_err());
    assert!(pointer_parts("/~").is_err());
    let mut root = json!({"a": [1, 2], "b": {}});
    assert_eq!(get_pointer(&root, "/a/-1").unwrap(), &json!(2));
    assert_eq!(get_pointer(&root, "/a/ 0 ").unwrap(), &json!(1));
    assert!(get_pointer(&root, "/a/2").is_err());
    set_pointer(&mut root, "/b/new", json!(true)).unwrap();
    assert_eq!(root["b"]["new"], json!(true));
    assert!(set_pointer(&mut root, "/a/9", json!(0)).is_err());
    assert_eq!(
        audit_v2_path(Path::new("/x/scan.sqlite")),
        PathBuf::from("/x/scan.sqlite.audit-v2.sqlite")
    );
}

#[test]
fn marker_detection_is_exact() {
    let v = json!({"a": {COLLECTION_MARKER: "/x", "count": 1}, "b": [{COLLECTION_MARKER: "/y"}], "c": {COLLECTION_MARKER: "/z", "other": 1}});
    assert_eq!(
        collection_markers(&v),
        ["/x".to_owned(), "/y".to_owned()].into_iter().collect()
    );
    assert!(contains_marker(&v));
    assert!(!contains_group_marker(&v));
    assert!(contains_group_marker(&json!([{GROUP_MEMBERS_MARKER: 0}])));
}

#[test]
fn reader_enforces_explicit_timeout_argument_and_missing_companion() {
    let dir = Scratch::new("misc");
    let (scan, _) = fixture_scan(&dir.0);
    assert!(AuditV2Reader::new(&scan, true, false, Some(0.0)).is_err());
    assert!(AuditV2Reader::new(&scan, true, false, Some(f64::NAN)).is_err());
    assert!(err_text(AuditV2Reader::new(&scan, true, false, None)).contains("unavailable"));
    let header = obj(json!({"issues": []}));
    assert!(err_text(write_audit_v2(
        &scan,
        &header,
        &IndexMap::new(),
        &Map::new()
    ))
    .contains("binding must name"));
}
