//! Whole-document audit.v2 parity: the Rust audit document written through
//! `store::audit_v2::write_audit_v2` is read back and structurally diffed
//! against the reference baselines, after the same normalisation the reference suite
//! applies (`oracle/NORMALISATION.md`).

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use indexmap::IndexMap;
use regex::Regex;
use serde_json::{Map, Value};
use spiderhead_checks::audit;
use spiderhead_oracle_diff::{diff, Options};

const SITES: [&str; 6] = [
    "basic",
    "canonical-hreflang",
    "errors",
    "redirects",
    "robots-sitemap",
    "schema",
];

fn oracle_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../oracle")
}

/// the reference suite on the Rust side: same rules, same order.
struct Norm {
    base: String,
    uuids: BTreeMap<String, usize>,
    uuid_re: Regex,
    ts_re: Regex,
    evid_re: Regex,
    port_re: Regex,
}

const VOLATILE_KEYS: &[&str] = &[
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

fn const_key(key: &str) -> Option<&'static str> {
    match key {
        "config_fingerprint" => Some("<FINGERPRINT>"),
        "sha256" => Some("<SHA256>"),
        "input" => Some("<INPUT>"),
        "digest" => Some("<DIGEST>"),
        _ => None,
    }
}

impl Norm {
    fn new(base: String) -> Self {
        Norm {
            base,
            uuids: BTreeMap::new(),
            uuid_re: Regex::new(r"[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}")
                .unwrap(),
            ts_re: Regex::new(r"\d{4}-\d\d-\d\d[T ]\d\d:\d\d:\d\d(?:\.\d+)?(?:Z|[+-]\d\d:?\d\d)?")
                .unwrap(),
            evid_re: Regex::new(r"(evidence:v1:<UUID:\d+>:\w+:)[0-9a-f]{24}").unwrap(),
            port_re: Regex::new(r"127\.0\.0\.1:\d+").unwrap(),
        }
    }

    fn s(&mut self, v: &str) -> String {
        let mut v = v.replace(&self.base, "{BASE}");
        v = self
            .port_re
            .replace_all(&v, "127.0.0.1:{PORT}")
            .into_owned();
        let mut found: Vec<String> = Vec::new();
        for m in self.uuid_re.find_iter(&v) {
            let u = m.as_str().to_string();
            if !self.uuids.contains_key(&u) && !found.contains(&u) {
                found.push(u);
            }
        }
        for u in found {
            let n = self.uuids.len() + 1;
            self.uuids.insert(u.clone(), n);
        }
        for (u, n) in self.uuids.clone() {
            v = v.replace(&u, &format!("<UUID:{n}>"));
        }
        v = self.evid_re.replace_all(&v, "${1}<HASH>").into_owned();
        self.ts_re.replace_all(&v, "<TS>").into_owned()
    }

    fn walk(&mut self, o: &Value, key: Option<&str>) -> Value {
        match o {
            Value::Object(m) => {
                let mut out = Map::new();
                for (k, v) in m {
                    out.insert(k.clone(), self.walk(v, Some(k)));
                }
                Value::Object(out)
            }
            Value::Array(a) => Value::Array(a.iter().map(|v| self.walk(v, None)).collect()),
            Value::Number(_) if key.map(|k| VOLATILE_KEYS.contains(&k)).unwrap_or(false) => {
                Value::from("<DURATION>")
            }
            Value::String(s) => {
                if let Some(k) = key.and_then(const_key) {
                    return Value::from(k);
                }
                Value::from(self.s(s))
                // fallthrough kept unreachable: string const keys fully shadow s()
            }
            _ => o.clone(),
        }
    }
}

/// Read the companion audit database back into the the reference suite `audit_v2` shape.
fn read_audit_db(db: &Path) -> Value {
    let con = rusqlite::Connection::open_with_flags(db, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY)
        .expect("open audit db");
    let (fmt, binding_json, header_json) = con
        .query_row(
            "SELECT format_version, binding_json, header_json FROM audit_meta ORDER BY rowid LIMIT 1",
            [],
            |r| {
                Ok((
                    r.get::<_, String>(0)?,
                    r.get::<_, String>(1)?,
                    r.get::<_, String>(2)?,
                ))
            },
        )
        .expect("audit_meta");

    let mut collections = Map::new();
    {
        let mut st = con
            .prepare("SELECT pointer, item_count FROM collections ORDER BY rowid")
            .expect("collections");
        let rows = st
            .query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)?)))
            .expect("collections rows");
        for r in rows {
            let (p, n) = r.expect("row");
            collections.insert(p, Value::from(n));
        }
    }

    let mut items: IndexMap<String, Vec<Value>> = IndexMap::new();
    {
        let mut st = con
            .prepare("SELECT pointer, value_json FROM items ORDER BY rowid")
            .expect("items");
        let rows = st
            .query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))
            .expect("items rows");
        for r in rows {
            let (p, j) = r.expect("row");
            items
                .entry(p)
                .or_default()
                .push(serde_json::from_str(&j).expect("item json"));
        }
    }

    let mut group_members = Vec::new();
    {
        let mut st = con
            .prepare("SELECT * FROM group_members ORDER BY rowid")
            .expect("group_members");
        let cols: Vec<String> = st.column_names().iter().map(|s| s.to_string()).collect();
        let mut rows = st.query([]).expect("group_members rows");
        while let Some(r) = rows.next().expect("row") {
            let mut m = Map::new();
            for (i, c) in cols.iter().enumerate() {
                let v: rusqlite::types::Value = r.get_ref(i).expect("col").into();
                let j = match v {
                    rusqlite::types::Value::Null => Value::Null,
                    rusqlite::types::Value::Integer(n) => Value::from(n),
                    rusqlite::types::Value::Real(f) => Value::from(f),
                    rusqlite::types::Value::Text(t) => {
                        if c.ends_with("_json") {
                            serde_json::from_str(&t).unwrap_or(Value::from(t))
                        } else {
                            Value::from(t)
                        }
                    }
                    rusqlite::types::Value::Blob(b) => {
                        let mut bm = Map::new();
                        bm.insert("$blob_len".into(), Value::from(b.len() as i64));
                        Value::Object(bm)
                    }
                };
                m.insert(c.clone(), j);
            }
            group_members.push(Value::Object(m));
        }
    }

    let mut out = Map::new();
    out.insert("format_version".into(), Value::from(fmt));
    out.insert(
        "binding".into(),
        serde_json::from_str(&binding_json).expect("binding json"),
    );
    out.insert(
        "header".into(),
        serde_json::from_str(&header_json).expect("header json"),
    );
    out.insert("collections".into(), Value::Object(collections));
    let mut items_obj = Map::new();
    for (p, rows) in items {
        items_obj.insert(p, Value::Array(rows));
    }
    out.insert("items".into(), Value::Object(items_obj));
    out.insert("group_members".into(), Value::Array(group_members));
    Value::Object(out)
}

#[test]
fn audit_v2_matches_reference_baselines() {
    let known = spiderhead_oracle_diff::KnownDivergences::from_toml(
        &std::fs::read_to_string(oracle_dir().join("known-divergences.toml"))
            .expect("known-divergences.toml"),
    )
    .expect("parse known-divergences");
    let mut used = Vec::new();
    let mut failures = String::new();
    for site in SITES {
        let dir = oracle_dir().join("raw-scans").join(site);
        let tmp =
            std::env::temp_dir().join(format!("audit-v2-oracle-{site}-{}", std::process::id()));
        let _ = std::fs::create_dir_all(&tmp);
        let scan = tmp.join("scan.sqlite");
        std::fs::copy(dir.join("scan.sqlite"), &scan).expect("copy scan");
        let companion = tmp.join("scan.sqlite.audit-v2.sqlite");
        let _ = std::fs::remove_file(&companion);

        let (header, collections) = match audit::audit_v2_document(&scan) {
            Ok(v) => v,
            Err(e) => {
                failures.push_str(&format!("[{site}] document: {e}\n"));
                continue;
            }
        };
        if let Err(e) = audit::write_audit_v2(&scan, &header, &collections) {
            failures.push_str(&format!("[{site}] write: {e}\n"));
            continue;
        }

        // The fixture base is the stored start_url (scheme + host + port).
        let con = rusqlite::Connection::open_with_flags(
            &scan,
            rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
        )
        .expect("scan");
        let base: String = con
            .query_row("SELECT start_url FROM scan WHERE singleton=1", [], |r| {
                r.get(0)
            })
            .expect("start_url");
        drop(con);
        let base = base.trim_end_matches('/').to_string();

        let mut norm = Norm::new(base);
        let actual = norm.walk(&read_audit_db(&companion), None);

        let expected: Value = serde_json::from_str(
            &std::fs::read_to_string(
                oracle_dir()
                    .join("baselines")
                    .join(site)
                    .join("audit.v2.json"),
            )
            .expect("baseline"),
        )
        .expect("baseline json");

        let divs = diff(&expected, &actual, &Options::default());
        let divs = known.filter(site, "audit.v2.json", divs, &mut used);
        if !divs.is_empty() {
            failures.push_str(&format!(
                "[{site}] {} divergences\n{}",
                divs.len(),
                spiderhead_oracle_diff::report(&divs, 30)
            ));
        }
    }
    for stale in known
        .stale(&used)
        .into_iter()
        .filter(|e| e.file == "*" || e.file == "audit.v2.json")
    {
        failures.push_str(&format!(
            "stale divergence {} ({} {})\n",
            stale.id, stale.file, stale.path
        ));
    }
    assert!(failures.is_empty(), "{failures}");
}
