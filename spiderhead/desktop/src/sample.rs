//! Synthetic `scan.v1` (+audit.v2) generator for demos and speed checks.
//! `--sample site-small` writes 200 pages, `site-large` 1 000 000 — the
//! URL table's virtualisation and sort caps are exercised against these.

use std::path::{Path, PathBuf};

use rusqlite::{params, Connection};
use serde_json::{json, Map, Value};
use sha2::{Digest, Sha256};
use spiderhead_store::schema_sql::SCAN_V1_SQL;

use crate::data::session::DataError;

/// Sample kinds the `--sample` flag accepts.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Sample {
    /// 200 pages — quick demos.
    Small,
    /// 1 000 000 pages — scroll and sort stress.
    Large,
}

impl Sample {
    /// Parse `--sample <name>`.
    pub fn parse(name: &str) -> Option<Self> {
        match name {
            "site-small" => Some(Sample::Small),
            "site-large" => Some(Sample::Large),
            _ => None,
        }
    }

    /// Pages generated.
    pub fn pages(self) -> i64 {
        match self {
            Sample::Small => 200,
            Sample::Large => 1_000_000,
        }
    }
}

/// Write `dir/scan.sqlite` + its audit.v2 companion; returns the scan path.
/// Existing files are refused — a sample never overwrites real data.
pub fn generate(dir: &Path, sample: Sample) -> Result<PathBuf, DataError> {
    std::fs::create_dir_all(dir).map_err(|e| {
        DataError::Query(crate::data::filters::QueryError::invalid(
            "cannot_write",
            format!("cannot create sample dir: {e}"),
        ))
    })?;
    let path = dir.join("scan.sqlite");
    if path.exists() {
        return Err(DataError::Query(crate::data::filters::QueryError::invalid(
            "output_exists",
            format!("sample already exists: {}", path.display()),
        )));
    }
    // The audit companion is written next to the scan; refuse to merge
    // synthetic rows into an audit that already exists.
    let audit_path = spiderhead_store::audit_v2::audit_v2_path(&path);
    if audit_path.exists() {
        return Err(DataError::Query(crate::data::filters::QueryError::invalid(
            "output_exists",
            format!("sample already exists: {}", audit_path.display()),
        )));
    }
    let n = sample.pages();
    let total = n as usize;
    let host = "sample.site";
    let mut con = Connection::open(&path).map_err(DataError::Sqlite)?;
    con.execute_batch(SCAN_V1_SQL).map_err(DataError::Sqlite)?;
    let scan_uuid = "00000000-5a1e-4000-8000-000000000001";
    con.execute(
        "INSERT INTO scan (singleton, scan_uuid, format_version, evidence_version,
            writer_version, writer_revision, runtime_versions_json, created_at,
            finished_at, source_kind, parent_scan_uuid, start_url, config_json,
            config_fingerprint, lifecycle, finish_reason, crawl_partial,
            corpus_partial, evidence_revision, limitations_json, capabilities_json,
            retention_json, pinned)
         VALUES (1,?1,'scan.v1','evidence.v1','0.1.0','sample','{}','2026-10-27T10:00:00+00:00',
            '2026-10-27T10:05:00+00:00','native',NULL,?2,'{}','sample',\
            'finished','finished',0,0,1,'{}',?3,'{}',0)",
        params![
            scan_uuid,
            format!("https://{host}/"),
            json!({"links":{"state":"complete"},
                   "pages":{"state":"complete","reason":"committed native crawl observations"}})
            .to_string(),
        ],
    )
    .map_err(DataError::Sqlite)?;

    let tx = con.transaction().map_err(DataError::Sqlite)?;
    {
        let mut ins_url = tx
            .prepare("INSERT INTO urls (url_id, url) VALUES (?1, ?2)")
            .map_err(DataError::Sqlite)?;
        let page_columns: Vec<String> = {
            let mut st = tx
                .prepare("PRAGMA table_info(pages)")
                .map_err(DataError::Sqlite)?;
            let rows = st
                .query_map([], |r| r.get::<_, String>(1))
                .map_err(DataError::Sqlite)?;
            rows.collect::<Result<_, _>>().map_err(DataError::Sqlite)?
        };
        let marks = vec!["?"; page_columns.len()].join(",");
        let mut ins_page = tx
            .prepare(&format!(
                "INSERT INTO pages ({}) VALUES ({marks})",
                page_columns.join(",")
            ))
            .map_err(DataError::Sqlite)?;
        let mut ins_link = tx
            .prepare(
                "INSERT INTO links (source_url_id, destination_url_id, ordinal,
                    anchor, anchor_source, nofollow, position, rel_json, target,
                    raw_href, evidence_representation)
                 VALUES (?1,?2,?3,?4,'',0,'content','[]','',?5,'static')",
            )
            .map_err(DataError::Sqlite)?;
        // Pass one: every url + page, so links' FK targets all exist.
        for i in 1..=total {
            let url = page_url(host, i);
            ins_url
                .execute(params![i as i64, url])
                .map_err(DataError::Sqlite)?;
            let row = page_row(i, &url, &page_columns);
            ins_page
                .execute(rusqlite::params_from_iter(row.iter()))
                .map_err(DataError::Sqlite)?;
        }
        drop(ins_url);
        drop(ins_page);
        // Pass two: two internal links — one forward, one to home.
        for i in 1..=total {
            let next = if i == total { 1 } else { i + 1 };
            ins_link
                .execute(params![
                    i as i64,
                    next as i64,
                    1i64,
                    format!("page {next}"),
                    page_url(host, next)
                ])
                .map_err(DataError::Sqlite)?;
            ins_link
                .execute(params![i as i64, 1i64, 2i64, "home", page_url(host, 1)])
                .map_err(DataError::Sqlite)?;
        }
    }
    tx.commit().map_err(DataError::Sqlite)?;
    con.execute_batch("PRAGMA wal_checkpoint(FULL); ANALYZE;")
        .map_err(DataError::Sqlite)?;
    drop(con);

    write_sample_audit(&path, scan_uuid, host, n)?;
    Ok(path)
}

fn page_url(host: &str, i: usize) -> String {
    let section = (i - 1) / 100;
    format!("https://{host}/s{section}/page-{i}.html")
}

/// A `pages` row matching the live schema: values by column name. The value
/// is produced per requested column — a schema-order map never materializes,
/// so a million-row sample does not pay ~110 map inserts per page.
fn page_row(i: usize, url: &str, columns: &[String]) -> Vec<rusqlite::types::Value> {
    columns.iter().map(|c| page_value(i, url, c)).collect()
}

/// One synthetic `pages` value for column `col` of page `i` (unknown columns
/// read as NULL, like the old map lookup did).
fn page_value(i: usize, url: &str, col: &str) -> rusqlite::types::Value {
    use rusqlite::types::Value as V;
    let status: i64 = match i % 97 {
        0 => 404,
        x if x % 53 == 0 => 301,
        _ => 200,
    };
    match col {
        "url_id" | "page_ordinal" => V::Integer(i as i64),
        "document_id" => V::Null,
        "status_code" => V::Integer(status),
        "content_type" => V::Text("text/html; charset=utf-8".into()),
        "size_bytes" => V::Integer((i * 977) as i64 % 250_000 + 300),
        "response_time" => V::Real((i % 100) as f64 / 100.0 + 0.001),
        "redirect_url" => V::Text(String::new()),
        "title" => V::Text(if i.is_multiple_of(11) {
            "Duplicate synthetic title".to_string()
        } else if i.is_multiple_of(13) {
            String::new()
        } else {
            format!("Synthetic page {i}")
        }),
        "meta_description" => V::Text(if i.is_multiple_of(17) {
            String::new()
        } else {
            format!("Synthetic description for page {i} with a reasonable length")
        }),
        "h1" => V::Text(format!("Heading {i}")),
        "h1_2" | "h2" => V::Text(String::new()),
        "canonical" => V::Text(if status == 301 {
            format!("https://sample.site/s0/page-{}.html", i + 1)
        } else {
            String::new()
        }),
        "meta_robots" => V::Text(if i.is_multiple_of(29) {
            "noindex".into()
        } else {
            String::new()
        }),
        "x_robots" | "og_title" | "og_description" | "og_image" => V::Text(String::new()),
        "word_count" => V::Integer(((i * 37) % 4000 + 5) as i64),
        "text_ratio" => V::Real(0.42),
        "crawl_depth" => V::Integer(((i - 1) / 100) as i64),
        "content_encoding" => V::Text("identity".into()),
        "charset" => V::Text("utf-8".into()),
        "doctype" => V::Text("<!DOCTYPE html>".into()),
        "viewport" => V::Text("width=device-width".into()),
        "meta_description_count" => V::Integer(1),
        "images_total" => V::Integer((i % 8) as i64),
        "images_missing_alt_attr" => V::Integer(if i.is_multiple_of(19) { 1 } else { 0 }),
        "title_outside_head"
        | "meta_description_outside_head"
        | "canonical_outside_head"
        | "directives_outside_head"
        | "hreflang_outside_head"
        | "transport_upgraded" => V::Integer(0),
        "head_count" => V::Integer(12),
        "body_count" => V::Integer(6),
        "head_not_first" => V::Integer(0),
        "invalid_head_elements" => V::Text(String::new()),
        "outlinks" => V::Integer(2),
        "external_outlinks" | "jsonld_blocks_found" | "jsonld_blocks_parsed" => V::Integer(0),
        "error" | "error_kind" | "cache_status" => V::Text(String::new()),
        "representation" => V::Text("static".into()),
        "redirect_chain_json" => V::Text("[]".into()),
        "final_url" => V::Text(if status == 301 {
            url.to_string()
        } else {
            String::new()
        }),
        _ => V::Null,
    }
}

/// A minimal but valid audit.v2 companion over the synthetic scan.
fn write_sample_audit(
    scan_path: &Path,
    scan_uuid: &str,
    host: &str,
    n: i64,
) -> Result<(), DataError> {
    // One critical + one warning check over real urls, plus a silent check.
    let issues: Vec<Value> = (1..=n.min(50))
        .map(|i| {
            json!({
                "id": format!("ISSUE-{i:06}"),
                "check": if i % 2 == 0 { "TITLE_DUPLICATE" } else { "BROKEN_INTERNAL_LINK" },
                "severity": if i % 2 == 0 { "warning" } else { "critical" },
                "target_url": page_url(host, i as usize),
                "status_code": 200,
                "source": "synthetic",
                "message": "synthetic finding",
                "details": {},
                "locations": [],
                "occurrences_count": 1,
                "fix_hint": "",
                "fingerprint": format!("{i:016x}"),
                "evidence": {},
            })
        })
        .collect();
    let crit = issues
        .iter()
        .filter(|v| v["severity"] == "critical")
        .count() as i64;
    let warn = issues.len() as i64 - crit;
    let mut header = Map::new();
    header.insert("format_version".into(), json!("audit.v2"));
    header.insert("issues".into(), json!([]));
    header.insert("groups".into(), json!([]));
    header.insert("pages".into(), json!([]));
    header.insert(
        "run".into(),
        json!({"checks_skipped": [], "checks_disabled": []}),
    );
    header.insert(
        "summary".into(),
        json!({
            "totals": {"issues_total": issues.len() as i64, "findings_total": issues.len() as i64},
            "by_severity": {"critical": crit, "warning": warn},
            "by_check": {"TITLE_DUPLICATE": warn, "BROKEN_INTERNAL_LINK": crit},
            "check_coverage": {
                "checks_total": 3, "checks_fired": 2, "checks_skipped": 0,
                "checks_disabled": 0, "checks_silent": 1,
                "checks_silent_ids": ["H1_DUPLICATE"], "checks_disabled_ids": [],
            },
        }),
    );
    let mut binding = Map::new();
    binding.insert("scan_uuid".into(), json!(scan_uuid));
    binding.insert("evidence_revision".into(), json!(1));
    binding.insert("analyzer_version".into(), json!("0.1.0"));
    binding.insert("analyzer_revision".into(), json!("sample"));
    let collections: Vec<(String, Vec<Value>)> = vec![
        ("/groups".into(), vec![]),
        ("/issues".into(), issues),
        ("/pages".into(), vec![]),
    ];
    write_companion(scan_path, &header, &collections, &binding)
}

/// Write an audit.v2 companion ourselves: `write_audit_v2` also binds against
/// the scan through the strict native validator, which synthetic files do not
/// carry — the rows/digest math here matches `fill_companion` exactly.
fn write_companion(
    scan_path: &Path,
    header: &Map<String, Value>,
    collections: &[(String, Vec<Value>)],
    binding: &Map<String, Value>,
) -> Result<(), DataError> {
    use spiderhead_store::audit_v2::{
        audit_v2_path, canonical, digest_item, digest_start, set_pointer, COLLECTION_MARKER,
    };
    let fail =
        |m: String| DataError::Query(crate::data::filters::QueryError::invalid("sample_audit", m));
    let path =
        audit_v2_path(&std::path::absolute(scan_path).map_err(|e| fail(format!("bad path: {e}")))?);
    let con = Connection::open(&path).map_err(DataError::Sqlite)?;
    con.execute_batch(&format!(
        "PRAGMA trusted_schema=OFF; PRAGMA foreign_keys=OFF; PRAGMA synchronous=FULL; \
         PRAGMA journal_mode=DELETE; PRAGMA application_id={}; PRAGMA user_version={};",
        spiderhead_store::audit_v2::APPLICATION_ID,
        spiderhead_store::audit_v2::USER_VERSION,
    ))
    .map_err(DataError::Sqlite)?;
    con.execute_batch(spiderhead_store::schema_sql::AUDIT_V2_SQL)
        .map_err(DataError::Sqlite)?;
    let mut stored_header = serde_json::from_str::<Value>(
        &canonical(&Value::Object(header.clone())).map_err(DataError::Store)?,
    )
    .map_err(|e| fail(format!("bad header: {e}")))?;
    let binding_json = canonical(&Value::Object(binding.clone())).map_err(DataError::Store)?;
    con.execute_batch("BEGIN IMMEDIATE")
        .map_err(DataError::Sqlite)?;
    let outcome = (|| -> Result<(), DataError> {
        let mut counts = Vec::new();
        for (pointer, rows) in collections {
            con.execute(
                "INSERT INTO collections(pointer,item_count) VALUES (?,0)",
                [pointer],
            )
            .map_err(DataError::Sqlite)?;
            let mut count: i64 = 0;
            for source in rows {
                let raw = serde_json::to_string(source).map_err(|e| fail(e.to_string()))?;
                con.execute(
                    "INSERT INTO items(pointer,ordinal,value_json) VALUES (?,?,?)",
                    params![pointer, count, raw],
                )
                .map_err(DataError::Sqlite)?;
                count += 1;
            }
            con.execute(
                "UPDATE collections SET item_count=? WHERE pointer=?",
                params![count, pointer],
            )
            .map_err(DataError::Sqlite)?;
            counts.push((pointer, count));
        }
        for (pointer, count) in &counts {
            let mut marker = Map::new();
            marker.insert(COLLECTION_MARKER.into(), Value::from((*pointer).clone()));
            marker.insert("count".into(), Value::from(*count));
            set_pointer(&mut stored_header, pointer, Value::Object(marker))
                .map_err(DataError::Store)?;
        }
        let header_json = canonical(&stored_header).map_err(DataError::Store)?;
        let mut digest = digest_start(
            &binding_json,
            &header_json,
            spiderhead_store::audit_v2::USER_VERSION,
        );
        let mut sorted: Vec<&String> = collections.iter().map(|(p, _)| p).collect();
        sorted.sort();
        for pointer in sorted {
            let mut stmt = con
                .prepare("SELECT ordinal,value_json FROM items WHERE pointer=? ORDER BY ordinal")
                .map_err(DataError::Sqlite)?;
            let mut rows = stmt.query([pointer]).map_err(DataError::Sqlite)?;
            while let Some(r) = rows.next().map_err(DataError::Sqlite)? {
                digest_item(
                    &mut digest,
                    pointer,
                    r.get(0).map_err(DataError::Sqlite)?,
                    &r.get::<_, String>(1).map_err(DataError::Sqlite)?,
                );
            }
        }
        let sha: String = digest
            .finalize()
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect();
        con.execute(
            "INSERT INTO audit_meta VALUES (1,?,?,?,?)",
            params!["audit.v2", binding_json, header_json, sha],
        )
        .map_err(DataError::Sqlite)?;
        Ok(())
    })();
    match outcome {
        Ok(()) => con.execute_batch("COMMIT").map_err(DataError::Sqlite),
        Err(e) => {
            let _ = con.execute_batch("ROLLBACK");
            Err(e)
        }
    }
}

/// Digest helper kept for future body rows (bodies carry sha256 PKs).
#[allow(dead_code)]
pub(crate) fn sha256_hex(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::AppModel;
    use serde_json::json;

    fn scratch(tag: &str) -> PathBuf {
        let dir =
            std::env::temp_dir().join(format!("spiderhead-desktop-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn small_sample_opens_and_queries() {
        let dir = scratch("small");
        let path = generate(&dir, Sample::Small).expect("generate");
        assert!(spiderhead_store::audit_v2::audit_v2_path(&path).is_file());
        let mut app = AppModel::new();
        let out = app.dispatch("scan.open", &json!({"path": path.display().to_string()}));
        assert!(out.answer.is_ok(), "{:?}", out.answer);
        let out = app.dispatch("table.page", &json!({"limit": 10}));
        let page = out.answer.unwrap();
        assert_eq!(page["total"], 200);
        let out = app.dispatch("dashboard.inspect", &json!({}));
        let dash = out.answer.unwrap();
        assert_eq!(dash["state"], "available");
        assert_eq!(dash["issues_total"], 50);
        let out = app.dispatch(
            "dashboard.filter",
            &json!({"issue_check": "TITLE_DUPLICATE"}),
        );
        let page = out.answer.unwrap();
        assert_eq!(page["filtered_total"], 25);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
