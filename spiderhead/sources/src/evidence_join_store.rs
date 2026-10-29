//! Durable SQLite-backed evidence joins: one row per retained page, normalized provider row,
//! and match edge.

use std::path::{Path, PathBuf};

use rusqlite::{Connection, OpenFlags};
use serde_json::{json, Map, Value};
use sha2::Digest;

use crate::cloudflare::CfError;
use crate::evidence_join::{key_fn_for_store, row_join_key, NORMALIZED_FORMAT};
use crate::pystr;

mod support;
pub use support::*;

/// Format identifier of the SQLite evidence-join store.
pub const FORMAT: &str = "spiderhead.evidence-join-sqlite.v1";
/// `MAX_PAGES` — retained crawl pages bound.
pub const MAX_PAGES: i64 = 1_000_000;
/// `MAX_EVIDENCE_ROWS` — normalized evidence row bound.
pub const MAX_EVIDENCE_ROWS: usize = 100_000;
/// `MAX_MATCH_EDGES` — durable page/evidence edge bound.
pub const MAX_MATCH_EDGES: i64 = 5_000_000;

/// `EvidenceJoinStore` — immutable metadata plus re-iterable cursors.
pub struct EvidenceJoinStore {
    /// Absolute store path.
    pub path: PathBuf,
    /// The validated `meta.document` payload.
    pub metadata: Value,
}

/// Open and validate a join store.
pub fn open_store(value: &str) -> Result<EvidenceJoinStore, CfError> {
    let path = std::fs::canonicalize(value).unwrap_or_else(|_| PathBuf::from(value));
    let metadata = metadata_document(&path)?;
    let summary = &metadata["summary"];
    let con = open_read(&path)?;
    let count = |query: &str| -> i64 {
        con.query_row(query, [], |r| r.get::<_, i64>(0))
            .unwrap_or_default()
    };
    let actual: Vec<(&str, i64)> = vec![
        ("pages", count("SELECT COUNT(*) FROM pages")),
        ("rows", count("SELECT COUNT(*) FROM evidence_rows")),
        ("candidate_pairs", count("SELECT COUNT(*) FROM matches")),
        (
            "matched_rows",
            count("SELECT COUNT(*) FROM evidence_rows WHERE population='matched'"),
        ),
        (
            "matched_pages",
            count("SELECT COUNT(*) FROM pages WHERE population='matched'"),
        ),
        (
            "crawl_only",
            count("SELECT COUNT(*) FROM pages WHERE population='crawl_only'"),
        ),
        (
            "external_only",
            count("SELECT COUNT(*) FROM evidence_rows WHERE population='external_only'"),
        ),
        (
            "unkeyable_rows",
            count("SELECT COUNT(*) FROM evidence_rows WHERE population='unkeyable_rows'"),
        ),
        (
            "unkeyable_pages",
            count("SELECT COUNT(*) FROM pages WHERE population='unkeyable_pages'"),
        ),
    ];
    let dangling: Option<i64> = con
        .query_row(
            "SELECT 1 FROM matches m LEFT JOIN evidence_rows r \
             ON r.row_index=m.row_index AND r.natural_key_sha256=m.natural_key_sha256 \
             LEFT JOIN pages p ON p.page_ordinal=m.page_ordinal \
             WHERE r.row_index IS NULL OR p.page_ordinal IS NULL LIMIT 1",
            [],
            |r| r.get(0),
        )
        .ok();
    if dangling.is_some()
        || actual
            .iter()
            .any(|(name, value)| summary[name].as_i64().unwrap_or(-1) != *value)
    {
        return Err(store_error(
            "evidence join store rows disagree with its metadata",
        ));
    }
    Ok(EvidenceJoinStore { path, metadata })
}

/// `iter_observations` — each provider row once with its matched page count.
pub fn iter_observations(path: &Path) -> Result<Vec<Value>, CfError> {
    let con = open_read(path)?;
    let mut stmt = con
        .prepare(
            "SELECT r.row_index,r.natural_key_sha256,r.row_json,r.population,\
             COUNT(m.page_ordinal) FROM evidence_rows r LEFT JOIN matches m \
             ON m.row_index=r.row_index AND m.natural_key_sha256=r.natural_key_sha256 \
             GROUP BY r.row_index,r.natural_key_sha256,r.row_json,r.population \
             ORDER BY r.row_index,r.natural_key_sha256",
        )
        .map_err(|e| store_error(e.to_string()))?;
    let rows = stmt
        .query_map([], |r| {
            Ok((
                r.get::<_, i64>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, String>(2)?,
                r.get::<_, String>(3)?,
                r.get::<_, i64>(4)?,
            ))
        })
        .map_err(|e| store_error(e.to_string()))?;
    let mut out = Vec::new();
    for item in rows {
        let (row_index, digest, row_json, population, matched) =
            item.map_err(|e| store_error(e.to_string()))?;
        let row: Value = serde_json::from_str(&row_json)
            .map_err(|_| store_error("evidence join store row JSON is invalid"))?;
        out.push(json!({
            "identity": [row_index, digest],
            "row": row,
            "population": population,
            "matched_page_count": matched,
        }));
    }
    Ok(out)
}

/// `iter_matches` — every provider-row to retained-URL provenance edge.
pub fn iter_matches(path: &Path) -> Result<Vec<Value>, CfError> {
    let con = open_read(path)?;
    let mut stmt = con
        .prepare(
            "SELECT m.row_index,m.natural_key_sha256,p.page_ordinal,p.url,p.join_key \
             FROM matches m JOIN pages p ON p.page_ordinal=m.page_ordinal \
             ORDER BY m.row_index,m.natural_key_sha256,p.page_ordinal",
        )
        .map_err(|e| store_error(e.to_string()))?;
    let rows = stmt
        .query_map([], |r| {
            Ok((
                r.get::<_, i64>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, i64>(2)?,
                r.get::<_, String>(3)?,
                r.get::<_, Option<String>>(4)?,
            ))
        })
        .map_err(|e| store_error(e.to_string()))?;
    let mut out = Vec::new();
    for item in rows {
        let (row_index, digest, ordinal, url, key) =
            item.map_err(|e| store_error(e.to_string()))?;
        out.push(json!({
            "identity": [row_index, digest],
            "page_ordinal": ordinal,
            "url": url,
            "url_key": key,
        }));
    }
    Ok(out)
}

/// `iter_pages(population)` — retained pages, optionally filtered.
pub fn iter_pages(path: &Path, population: Option<&str>) -> Result<Vec<Value>, CfError> {
    let con = open_read(path)?;
    let sql = format!(
        "SELECT page_ordinal,url,join_key,population FROM pages {}ORDER BY page_ordinal",
        if population.is_some() {
            "WHERE population=? "
        } else {
            ""
        }
    );
    let mut stmt = con.prepare(&sql).map_err(|e| store_error(e.to_string()))?;
    let rows: Vec<(i64, String, Option<String>, String)> = match population {
        Some(pop) => stmt
            .query_map([pop], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)))
            .map_err(|e| store_error(e.to_string()))?
            .collect::<Result<_, _>>()
            .map_err(|e: rusqlite::Error| store_error(e.to_string()))?,
        None => stmt
            .query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)))
            .map_err(|e| store_error(e.to_string()))?
            .collect::<Result<_, _>>()
            .map_err(|e: rusqlite::Error| store_error(e.to_string()))?,
    };
    Ok(rows
        .into_iter()
        .map(|(ordinal, url, key, pop)| {
            json!({
                "page_ordinal": ordinal,
                "url": url,
                "url_key": key,
                "population": pop,
            })
        })
        .collect())
}

/// Build a join store atomically from a scan and a document.
pub fn write(
    scan_rows: ScanRows,
    scan: &str,
    document: &Value,
    out: &str,
    policy: Option<&Value>,
) -> Result<Value, CfError> {
    if !document.is_object() || document.get("format") != Some(&json!(NORMALIZED_FORMAT)) {
        return Err(store_error(format!(
            "expected a {NORMALIZED_FORMAT} document"
        )));
    }
    let rows = match document.get("rows") {
        Some(Value::Array(items)) if items.len() <= MAX_EVIDENCE_ROWS => items.clone(),
        _ => {
            return Err(store_error(format!(
                "normalized evidence exceeds {MAX_EVIDENCE_ROWS} rows"
            )))
        }
    };
    let target = new_target(out)?;
    let policy_map = url_policy(policy)?;
    let key_fn = key_fn_for_store(&policy_map);
    let parent = target.parent().unwrap_or(Path::new("."));
    let temporary = parent.join(format!(
        ".evidence-join-{}-{}.sqlite",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or_default()
    ));
    let staged: Option<PathBuf> = Some(temporary.clone());
    let result = write_inner(
        scan_rows,
        scan,
        &rows,
        document,
        &target,
        &temporary,
        &key_fn,
        &policy_map,
    );
    match result {
        Ok(value) => Ok(value),
        Err(error) => {
            if let Some(path) = staged {
                let _ = std::fs::remove_file(path);
            }
            Err(error)
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn write_inner(
    scan_rows: ScanRows,
    scan: &str,
    rows: &[Value],
    document: &Value,
    target: &Path,
    temporary: &Path,
    key_fn: &dyn Fn(Option<&Value>) -> Option<String>,
    policy_map: &Map<String, Value>,
) -> Result<Value, CfError> {
    let con = Connection::open(temporary)
        .map_err(|e| store_error(format!("cannot write evidence join store: {e}")))?;
    con.execute_batch(
        "PRAGMA journal_mode=DELETE; PRAGMA synchronous=FULL;
         CREATE TABLE meta (key TEXT PRIMARY KEY,value TEXT NOT NULL);
         CREATE TABLE pages (page_ordinal INTEGER PRIMARY KEY,url TEXT NOT NULL,join_key TEXT,population TEXT NOT NULL);
         CREATE INDEX pages_join_key ON pages(join_key);
         CREATE TABLE evidence_rows (row_index INTEGER NOT NULL,natural_key_sha256 TEXT NOT NULL,join_key TEXT,row_json TEXT NOT NULL,population TEXT NOT NULL,PRIMARY KEY(row_index,natural_key_sha256));
         CREATE INDEX evidence_rows_join_key ON evidence_rows(join_key);
         CREATE TABLE matches (row_index INTEGER NOT NULL,natural_key_sha256 TEXT NOT NULL,page_ordinal INTEGER NOT NULL,PRIMARY KEY(row_index,natural_key_sha256,page_ordinal)) WITHOUT ROWID;
         CREATE INDEX matches_page_ordinal ON matches(page_ordinal);",
    )
    .map_err(|e| store_error(format!("cannot write evidence join store: {e}")))?;
    let mut page_digest = sha2::Sha256::new();
    let mut row_digest = sha2::Sha256::new();
    let (scan_uuid, page_rows) = (scan_rows)(scan)?;
    if scan_uuid.is_empty() {
        return Err(store_error("saved scan has no scan UUID"));
    }
    let mut pages: i64 = 0;
    {
        let mut stmt = con
            .prepare("INSERT INTO pages VALUES (?,?,?,?)")
            .map_err(|e| store_error(e.to_string()))?;
        for (ordinal, raw_url) in &page_rows {
            pages += 1;
            if pages > MAX_PAGES {
                return Err(store_error(format!(
                    "saved scan exceeds {MAX_PAGES} retained pages"
                )));
            }
            let key = key_fn(Some(&json!(raw_url)));
            stmt.execute(rusqlite::params![
                ordinal,
                raw_url,
                key.as_deref(),
                if key.is_none() {
                    "unkeyable_pages"
                } else {
                    "crawl_only"
                },
            ])
            .map_err(|e| store_error(format!("cannot write evidence join store: {e}")))?;
            update(
                &mut page_digest,
                &[
                    ordinal.to_string(),
                    raw_url.clone(),
                    key.unwrap_or_default(),
                ],
            );
        }
    }
    let mut dimension_names: Vec<String> = Vec::new();
    {
        let mut stmt = con
            .prepare("INSERT INTO evidence_rows VALUES (?,?,?,?,?)")
            .map_err(|e| store_error(e.to_string()))?;
        for raw in rows {
            if !raw.is_object() {
                return Err(store_error("normalized evidence contains a non-object row"));
            }
            let index = raw.get("row_index").cloned().unwrap_or(Value::Null);
            let digest = raw
                .get("natural_key_sha256")
                .cloned()
                .unwrap_or(Value::Null);
            if !pystr::is_py_int(&index) || index.as_i64().unwrap_or(-1) < 0 || !digest.is_string()
            {
                return Err(store_error(
                    "normalized evidence row lacks its stable identity",
                ));
            }
            let dimensions = raw
                .get("dimensions")
                .filter(|v| pystr::truthy(Some(v)))
                .cloned()
                .unwrap_or(json!({}));
            if !dimensions.is_object() {
                return Err(store_error(
                    "normalized evidence dimensions must be an object",
                ));
            }
            if let Value::Object(map) = &dimensions {
                for name in map.keys() {
                    if !dimension_names.contains(name) {
                        dimension_names.push(name.clone());
                    }
                }
            }
            let key = row_join_key(raw, key_fn)?;
            let payload = spiderhead_core::common::canonical_json(raw);
            stmt.execute(rusqlite::params![
                index.as_i64().unwrap_or_default(),
                digest.as_str().unwrap_or_default(),
                key.as_deref(),
                payload,
                if key.is_none() {
                    "unkeyable_rows"
                } else {
                    "external_only"
                },
            ])
            .map_err(|e| store_error(format!("cannot write evidence join store: {e}")))?;
            update(
                &mut row_digest,
                &[
                    pyjson_str(&index),
                    pyjson_str(&digest),
                    key.clone().unwrap_or_default(),
                    payload,
                ],
            );
        }
    }
    con.execute(
        "INSERT INTO matches SELECT r.row_index,r.natural_key_sha256,p.page_ordinal \
         FROM evidence_rows r JOIN pages p ON p.join_key=r.join_key WHERE r.join_key IS NOT NULL",
        [],
    )
    .map_err(|e| store_error(format!("cannot write evidence join store: {e}")))?;
    let count = |query: &str| -> i64 {
        con.query_row(query, [], |r| r.get::<_, i64>(0))
            .unwrap_or_default()
    };
    let candidate_pairs = count("SELECT COUNT(*) FROM matches");
    if candidate_pairs > MAX_MATCH_EDGES {
        return Err(store_error(format!(
            "provider join exceeds {MAX_MATCH_EDGES} durable page/evidence edges"
        )));
    }
    con.execute(
        "UPDATE evidence_rows SET population='matched' WHERE population='external_only' \
         AND EXISTS (SELECT 1 FROM matches m WHERE m.row_index=evidence_rows.row_index \
         AND m.natural_key_sha256=evidence_rows.natural_key_sha256)",
        [],
    )
    .map_err(|e| store_error(format!("cannot write evidence join store: {e}")))?;
    con.execute(
        "UPDATE pages SET population='matched' WHERE population='crawl_only' \
         AND EXISTS (SELECT 1 FROM matches m WHERE m.page_ordinal=pages.page_ordinal)",
        [],
    )
    .map_err(|e| store_error(format!("cannot write evidence join store: {e}")))?;
    let summary = json!({
        "pages": pages,
        "rows": rows.len(),
        "matched_rows": count("SELECT COUNT(*) FROM evidence_rows WHERE population='matched'"),
        "matched_pages": count("SELECT COUNT(*) FROM pages WHERE population='matched'"),
        "crawl_only": count("SELECT COUNT(*) FROM pages WHERE population='crawl_only'"),
        "external_only": count("SELECT COUNT(*) FROM evidence_rows WHERE population='external_only'"),
        "unkeyable_rows": count("SELECT COUNT(*) FROM evidence_rows WHERE population='unkeyable_rows'"),
        "unkeyable_pages": count("SELECT COUNT(*) FROM pages WHERE population='unkeyable_pages'"),
        "candidate_pairs": candidate_pairs,
        "page_key_collisions": count(
            "SELECT COUNT(*) FROM (SELECT join_key FROM pages WHERE join_key IS NOT NULL \
             GROUP BY join_key HAVING COUNT(*) > 1)"
        ),
        "row_key_collisions": count(
            "SELECT COUNT(*) FROM (SELECT join_key FROM evidence_rows WHERE join_key IS NOT NULL \
             GROUP BY join_key HAVING COUNT(*) > 1)"
        ),
        "multi_match_keys": count(
            "SELECT COUNT(*) FROM (SELECT p.join_key FROM pages p JOIN evidence_rows r \
             ON r.join_key=p.join_key WHERE p.join_key IS NOT NULL GROUP BY p.join_key \
             HAVING COUNT(DISTINCT p.page_ordinal)>1 \
             OR COUNT(DISTINCT r.row_index || ':' || r.natural_key_sha256)>1)"
        ),
        "ambiguous_rows": rows
            .iter()
            .filter(|row| row.get("ambiguous").map(|v| pystr::truthy(Some(v))).unwrap_or(false))
            .count(),
    });
    let mut sorted_dimensions = dimension_names.clone();
    sorted_dimensions.sort();
    let mut metadata = json!({
        "format": FORMAT,
        "crawl": {
            "source": "scan",
            "scan_uuid": scan_uuid,
            "page_digest_sha256": format!("{:x}", page_digest.finalize_reset()),
        },
        "evidence": crate::evidence_join::evidence_header_pub(document)?,
        "url_policy": {"version": "external_join.v1", "ignore_query": policy_map["ignore_query"], "ignore_scheme": policy_map["ignore_scheme"], "casefold_path": policy_map["casefold_path"]},
        "allocation_policy": {
            "version": "evidence_join_store.v1",
            "domain_totals": "never_allocate_to_urls",
            "missing_or_partial": "preserve_availability_state",
            "ambiguous_rows": "preserve_without_collapse",
        },
        "summary": summary,
        "dimension_names": sorted_dimensions,
        "evidence_rows_digest_sha256": format!("{:x}", row_digest.finalize()),
    });
    validate_metadata(&metadata)?;
    let mut content = sha2::Sha256::new();
    content.update(spiderhead_core::common::canonical_json(&metadata).as_bytes());
    {
        let mut stmt = con
            .prepare(
                "SELECT row_index,natural_key_sha256,page_ordinal FROM matches \
                 ORDER BY row_index,natural_key_sha256,page_ordinal",
            )
            .map_err(|e| store_error(e.to_string()))?;
        let edges = stmt
            .query_map([], |r| {
                Ok((
                    r.get::<_, i64>(0)?,
                    r.get::<_, String>(1)?,
                    r.get::<_, i64>(2)?,
                ))
            })
            .map_err(|e| store_error(e.to_string()))?;
        for edge in edges {
            let (a, b, c) = edge.map_err(|e| store_error(e.to_string()))?;
            update(&mut content, &[a.to_string(), b, c.to_string()]);
        }
    }
    metadata["content_sha256"] = json!(format!("{:x}", content.finalize()));
    con.execute(
        "INSERT INTO meta VALUES (?,?)",
        rusqlite::params![
            "document",
            spiderhead_core::common::canonical_json(&metadata)
        ],
    )
    .map_err(|e| store_error(format!("cannot write evidence join store: {e}")))?;
    drop(con);
    spiderhead_core::filesystem::set_permissions_mode(temporary, 0o600)
        .map_err(|e| CfError::Python("OSError", e.to_string()))?;
    std::fs::rename(temporary, target).map_err(|e| CfError::Python("OSError", e.to_string()))?;
    metadata["path"] = json!(target.to_string_lossy());
    Ok(metadata)
}

/// Text form for digest fields: integers render plainly, strings as-is.
fn pyjson_str(value: &Value) -> String {
    crate::pyjson::str_value(value).unwrap_or_default()
}
