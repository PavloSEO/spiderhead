//! Optional scan.v2 URL-query projection, built only by a finished writer.
//!

use std::collections::{BTreeMap, BTreeSet};
use std::time::{Duration, Instant};

use indexmap::IndexMap;
use rusqlite::types::Value as SqlValue;
use rusqlite::{params_from_iter, Connection};
use serde_json::Value;
use sha2::{Digest, Sha256};

use crate::error::StoreError;
use crate::url_query::{Budget, FACET_GROUPS};
use crate::url_query_presets::facet_counts;

/// Projection version tag.
pub const FORMAT: &str = "scan_url_query_projection.v2";

/// Name of the one-row metadata table.
pub const META: &str = "scan_url_query_projection_meta";

/// Name of the per-page projection table.
pub const PAGES: &str = "scan_url_query_projection_pages";

/// Wall-clock budget of one projection build.
pub const BUILD_BUDGET_SECONDS: f64 = 8.0;

/// Resource kinds with a per-kind page counter.
pub const RESOURCE_KINDS: &[&str] = &[
    "image",
    "media",
    "document",
    "stylesheet",
    "module",
    "manifest",
    "script",
    "css_import",
    "css_url",
    "font",
];

/// Exact column list of the projection page table.
pub const PAGE_COLUMNS: &[&str] = &[
    "url_id",
    "document_id",
    "resource_coverage_state",
    "resource_count",
    "resource_images",
    "resource_scripts",
    "resource_stylesheets",
    "kind_image",
    "kind_media",
    "kind_document",
    "kind_stylesheet",
    "kind_module",
    "kind_manifest",
    "kind_script",
    "kind_css_import",
    "kind_css_url",
    "kind_font",
    "title_duplicate",
];

/// Indexes the projection must carry, sorted for a stable order.
pub const PAGE_INDEXES: &[&str] = &[
    "scan_url_query_kind_css_import",
    "scan_url_query_kind_css_url",
    "scan_url_query_kind_document",
    "scan_url_query_kind_font",
    "scan_url_query_kind_image",
    "scan_url_query_kind_manifest",
    "scan_url_query_kind_media",
    "scan_url_query_kind_module",
    "scan_url_query_kind_script",
    "scan_url_query_kind_stylesheet",
    "scan_url_query_resource_images",
    "scan_url_query_resource_scripts",
    "scan_url_query_resource_stylesheets",
    "scan_url_query_title_duplicate",
];

/// A validated, source-bound projection read.
#[derive(Debug, Clone)]
pub struct QueryProjection {
    /// Rows in the projection page table.
    pub page_count: i64,
    /// `complete` or `unavailable`.
    pub resource_coverage_state: String,
    /// Pages per resource coverage state.
    pub resource_coverage_counts: BTreeMap<String, i64>,
    /// Facet counts by group id.
    pub facets: BTreeMap<String, Option<i64>>,
    /// Facet states by group id.
    pub facet_states: BTreeMap<String, String>,
}

fn sha256_hex(text: &str) -> String {
    let digest = Sha256::digest(text.as_bytes());
    digest.iter().map(|b| format!("{b:02x}")).collect()
}

fn table_names(con: &Connection, names: &[&str]) -> Result<BTreeSet<String>, StoreError> {
    let marks = vec!["?"; names.len()].join(",");
    let mut stmt = con.prepare(&format!(
        "SELECT name FROM main.sqlite_master WHERE type='table' AND name IN ({marks})"
    ))?;
    let rows = stmt.query_map(params_from_iter(names.iter()), |r| r.get::<_, String>(0))?;
    Ok(rows.collect::<Result<_, _>>()?)
}

/// Return a source-bound projection; never create or migrate anything on a read.
pub fn load(con: &Connection) -> Result<Option<QueryProjection>, StoreError> {
    let version: i64 = con.query_row("PRAGMA user_version", [], |r| r.get(0))?;
    if version != 2 {
        return Ok(None);
    }
    if table_names(con, &[META, PAGES])?.len() != 2 {
        return Ok(None);
    }
    // Any read or shape failure below means "no usable projection", never an error.
    Ok(load_checked(con).ok().flatten())
}

type Source = (String, i64, String, Option<String>, String);

fn load_checked(con: &Connection) -> Result<Option<QueryProjection>, StoreError> {
    let source: Option<Source> = {
        let mut stmt = con.prepare(
            "SELECT scan_uuid,evidence_revision,capabilities_json,start_url,lifecycle FROM scan WHERE singleton=1",
        )?;
        let mut rows = stmt.query([])?;
        match rows.next()? {
            Some(r) => Some((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?)),
            None => None,
        }
    };
    let meta = {
        let mut stmt = con.prepare(&format!(
            "SELECT projection_version,scan_uuid,evidence_revision,capabilities_sha256,start_url,page_count,resource_coverage_state,resource_coverage_counts_json,facet_counts_json,facet_states_json FROM main.{META} WHERE singleton=1"
        ))?;
        let mut rows = stmt.query([])?;
        match rows.next()? {
            Some(r) => Some((
                r.get::<_, String>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, i64>(2)?,
                r.get::<_, String>(3)?,
                r.get::<_, Option<String>>(4)?,
                r.get::<_, i64>(5)?,
                r.get::<_, String>(6)?,
                r.get::<_, String>(7)?,
                r.get::<_, String>(8)?,
                r.get::<_, String>(9)?,
            )),
            None => None,
        }
    };
    let (Some(source), Some(meta)) = (source, meta) else {
        return Ok(None);
    };
    if source.4 != "finished" {
        return Ok(None);
    }
    let page_columns: Vec<String> = {
        let mut stmt = con.prepare(&format!("PRAGMA main.table_info({PAGES})"))?;
        let rows = stmt.query_map([], |r| r.get::<_, String>(1))?;
        rows.collect::<Result<_, _>>()?
    };
    let page_indexes: BTreeSet<String> = {
        let mut stmt = con.prepare(
            "SELECT name FROM main.sqlite_master WHERE type='index' AND name GLOB 'scan_url_query_*'",
        )?;
        let rows = stmt.query_map([], |r| r.get::<_, String>(0))?;
        rows.collect::<Result<_, _>>()?
    };
    if page_columns != PAGE_COLUMNS || !PAGE_INDEXES.iter().all(|i| page_indexes.contains(*i)) {
        return Ok(None);
    }
    let (
        version,
        uuid,
        revision,
        cap_sha,
        start_url,
        page_count,
        coverage_state,
        coverage_json,
        counts_json,
        states_json,
    ) = meta;
    if version != FORMAT
        || uuid != source.0
        || revision != source.1
        || cap_sha != sha256_hex(&source.2)
        || start_url != source.3
    {
        return Ok(None);
    }
    let counts: BTreeMap<String, Value> = serde_json::from_str(&counts_json)?;
    let states: BTreeMap<String, Value> = serde_json::from_str(&states_json)?;
    let coverage: BTreeMap<String, Value> = serde_json::from_str(&coverage_json)?;
    let projection_rows: i64 =
        con.query_row(&format!("SELECT COUNT(*) FROM main.{PAGES}"), [], |r| {
            r.get(0)
        })?;
    let group_ids: BTreeSet<&str> = FACET_GROUPS.iter().map(|(k, _)| *k).collect();
    if page_count < 0
        || projection_rows != page_count
        || counts.keys().map(String::as_str).collect::<BTreeSet<_>>()
            != states.keys().map(String::as_str).collect::<BTreeSet<_>>()
        || counts.keys().map(String::as_str).collect::<BTreeSet<_>>() != group_ids
    {
        return Ok(None);
    }
    let mut facets = BTreeMap::new();
    let mut facet_states = BTreeMap::new();
    for (name, state) in &states {
        let Some(state) = state.as_str() else {
            return Ok(None);
        };
        match state {
            "exact" => match counts[name].as_i64() {
                Some(n) if (0..=page_count).contains(&n) => {
                    facets.insert(name.clone(), Some(n));
                }
                _ => return Ok(None),
            },
            "unavailable" | "capped" => {
                if !counts[name].is_null() {
                    return Ok(None);
                }
                facets.insert(name.clone(), None);
            }
            _ => return Ok(None),
        }
        facet_states.insert(name.clone(), state.to_string());
    }
    let mut coverage_counts = BTreeMap::new();
    for (k, v) in &coverage {
        match v.as_i64() {
            Some(n) if n >= 0 && ["complete", "partial", "unavailable"].contains(&k.as_str()) => {
                coverage_counts.insert(k.clone(), n);
            }
            _ => return Ok(None),
        }
    }
    if coverage_counts.values().sum::<i64>() != page_count {
        return Ok(None);
    }
    if coverage_state != "complete" && coverage_state != "unavailable" {
        return Ok(None);
    }
    let got = |k: &str| coverage_counts.get(k).copied().unwrap_or(0);
    if coverage_state == "complete"
        && (got("complete") != page_count || got("partial") != 0 || got("unavailable") != 0)
    {
        return Ok(None);
    }
    Ok(Some(QueryProjection {
        page_count,
        resource_coverage_state: coverage_state,
        resource_coverage_counts: coverage_counts,
        facets,
        facet_states,
    }))
}

/// Remove the published source binding inside a writer's current transaction.
pub fn invalidate(con: &Connection) -> Result<(), StoreError> {
    if table_names(con, &[META])?.contains(META) {
        con.execute(&format!("DELETE FROM main.{META}"), [])?;
    }
    Ok(())
}

/// Build an optional v2 page/facet projection without making finalization depend on it.
pub fn persist(con: &Connection, timeout_seconds: f64) -> Result<bool, StoreError> {
    let version: i64 = con.query_row("PRAGMA user_version", [], |r| r.get(0))?;
    if version != 2 {
        return Ok(false);
    }
    if !(0.1..=60.0).contains(&timeout_seconds) {
        return Err(StoreError::Invalid(
            "query projection timeout must be 0.1..60 seconds".into(),
        ));
    }
    if con.is_autocommit() {
        return Ok(false);
    }
    let prior_query_only: i64 = con.query_row("PRAGMA query_only", [], |r| r.get(0))?;
    let deadline = Instant::now() + Duration::from_secs_f64(timeout_seconds);
    let mut savepoint = false;
    let built = build(con, timeout_seconds, deadline, &mut savepoint);
    con.progress_handler(0, None::<fn() -> bool>);
    let ok = match built {
        Ok(()) => true,
        Err(_) => {
            if savepoint {
                let _ = con.execute_batch(
                    "ROLLBACK TO SAVEPOINT build_scan_url_query_projection; RELEASE SAVEPOINT build_scan_url_query_projection",
                );
            }
            false
        }
    };
    let _ = con.execute_batch(&format!(
        "PRAGMA query_only={}",
        if prior_query_only != 0 { "ON" } else { "OFF" }
    ));
    Ok(ok)
}

fn deadline_handler(con: &Connection, deadline: Instant) {
    con.progress_handler(1000, Some(move || Instant::now() > deadline));
}

fn build(
    con: &Connection,
    timeout_seconds: f64,
    deadline: Instant,
    savepoint: &mut bool,
) -> Result<(), StoreError> {
    con.execute_batch("PRAGMA query_only=OFF")?;
    // A failed optional rebuild must never leave the previous publication usable.
    invalidate(con)?;
    deadline_handler(con, deadline);
    con.execute_batch("SAVEPOINT build_scan_url_query_projection")?;
    *savepoint = true;
    let source: Option<Source> = {
        let mut stmt = con.prepare(
            "SELECT scan_uuid,evidence_revision,capabilities_json,start_url,lifecycle FROM scan WHERE singleton=1",
        )?;
        let mut rows = stmt.query([])?;
        match rows.next()? {
            Some(r) => Some((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?)),
            None => None,
        }
    };
    let source = match source {
        Some(s) if s.4 == "finished" => s,
        _ => {
            return Err(StoreError::Invalid(
                "URL query projection requires a finished scan.v2".into(),
            ))
        }
    };
    let capabilities_sha256 = sha256_hex(&source.2);
    let resource_tables_complete = table_names(
        con,
        &["resource_graph_occurrences", "resource_graph_fetches"],
    )?
    .len()
        == 2;

    con.execute_batch(
        "DROP TABLE IF EXISTS temp.query_projection_duplicate_titles;
         CREATE TEMP TABLE query_projection_duplicate_titles(title TEXT PRIMARY KEY) WITHOUT ROWID;
         INSERT INTO temp.query_projection_duplicate_titles(title) SELECT lower(title) FROM pages WHERE trim(title)<>'' GROUP BY lower(title) HAVING COUNT(*)>1;",
    )?;

    let kind_columns = RESOURCE_KINDS
        .iter()
        .map(|k| format!("kind_{k} INTEGER"))
        .collect::<Vec<_>>()
        .join(",");
    let kind_values = RESOURCE_KINDS
        .iter()
        .map(|k| format!("SUM(kind='{k}')"))
        .collect::<Vec<_>>()
        .join(",");
    con.execute_batch("DROP TABLE IF EXISTS temp.query_projection_resource_counts")?;
    con.execute_batch(&format!(
        "CREATE TEMP TABLE query_projection_resource_counts(page_url_id INTEGER NOT NULL,source_document_id INTEGER NOT NULL,resource_count INTEGER NOT NULL,resource_images INTEGER NOT NULL,resource_scripts INTEGER NOT NULL,resource_stylesheets INTEGER NOT NULL,{kind_columns},PRIMARY KEY(page_url_id,source_document_id)) WITHOUT ROWID"
    ))?;
    if resource_tables_complete {
        con.execute_batch(&format!(
            "INSERT INTO temp.query_projection_resource_counts SELECT page_url_id,source_document_id,COUNT(*),SUM(kind='image'),SUM(kind IN ('script','module')),SUM(kind='stylesheet'),{kind_values} FROM resource_graph_occurrences GROUP BY page_url_id,source_document_id"
        ))?;
    }

    con.execute_batch("DROP TABLE IF EXISTS temp.query_projection_pages")?;
    con.execute_batch(&format!(
        "CREATE TEMP TABLE query_projection_pages(url_id INTEGER PRIMARY KEY,document_id INTEGER,resource_coverage_state TEXT NOT NULL,resource_count INTEGER,resource_images INTEGER,resource_scripts INTEGER,resource_stylesheets INTEGER,{kind_columns},title_duplicate INTEGER NOT NULL) WITHOUT ROWID"
    ))?;
    let per_kind = RESOURCE_KINDS
        .iter()
        .map(|k| {
            format!("CASE WHEN ? AND c.completeness='complete' THEN COALESCE(r.kind_{k},0) END")
        })
        .collect::<Vec<_>>()
        .join(",");
    let flag = SqlValue::Integer(i64::from(resource_tables_complete));
    let flags: Vec<SqlValue> = vec![flag; 5 + RESOURCE_KINDS.len()];
    con.execute(
        &format!(
            "INSERT INTO temp.query_projection_pages SELECT p.url_id,p.document_id,CASE WHEN NOT ? OR p.document_id IS NULL THEN 'unavailable' WHEN c.completeness='complete' THEN 'complete' WHEN c.completeness='partial' THEN 'partial' ELSE 'unavailable' END,CASE WHEN ? AND c.completeness='complete' THEN COALESCE(r.resource_count,0) END,CASE WHEN ? AND c.completeness='complete' THEN COALESCE(r.resource_images,0) END,CASE WHEN ? AND c.completeness='complete' THEN COALESCE(r.resource_scripts,0) END,CASE WHEN ? AND c.completeness='complete' THEN COALESCE(r.resource_stylesheets,0) END,{per_kind},CASE WHEN d.title IS NULL THEN 0 ELSE 1 END FROM pages p LEFT JOIN context_items c ON c.kind='resource_graph_coverage' AND c.item_key='document:'||p.document_id LEFT JOIN temp.query_projection_resource_counts r ON r.page_url_id=p.url_id AND r.source_document_id=p.document_id LEFT JOIN temp.query_projection_duplicate_titles d ON d.title=lower(p.title) AND trim(p.title)<>''"
        ),
        params_from_iter(flags.iter()),
    )?;
    let page_count: i64 = con.query_row("SELECT COUNT(*) FROM pages", [], |r| r.get(0))?;
    let coverage_counts: BTreeMap<String, i64> = {
        let mut stmt = con.prepare(
            "SELECT resource_coverage_state,COUNT(*) FROM temp.query_projection_pages GROUP BY resource_coverage_state",
        )?;
        let rows = stmt.query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)?)))?;
        rows.collect::<Result<_, _>>()?
    };
    let resource_state = if resource_tables_complete
        && coverage_counts.get("complete").copied().unwrap_or(0) == page_count
    {
        "complete"
    } else {
        "unavailable"
    };

    let mut projection_facets: IndexMap<String, String> = FACET_GROUPS
        .iter()
        .map(|(k, v)| (k.to_string(), v.to_string()))
        .collect();
    for (k, v) in [
        ("title_duplicate", "q.title_duplicate=1"),
        ("with_resources", "q.resource_count>0"),
        ("with_images", "q.resource_images>0"),
        ("with_scripts", "q.resource_scripts>0"),
        ("with_stylesheets", "q.resource_stylesheets>0"),
    ] {
        projection_facets.insert(k.into(), v.into());
    }
    let budget = Budget::new(con);
    let ids: Vec<String> = FACET_GROUPS.iter().map(|(k, _)| k.to_string()).collect();
    let remaining = deadline
        .saturating_duration_since(Instant::now())
        .as_secs_f64();
    let (counts, states) = facet_counts(
        con,
        &budget,
        &ids,
        " JOIN urls u ON u.url_id=p.url_id LEFT JOIN temp.query_projection_pages q ON q.url_id=p.url_id",
        "",
        &[],
        timeout_seconds.min(remaining).max(0.1),
        Some(resource_state),
        Some(&projection_facets),
    )
    .map_err(|e| StoreError::Invalid(e.message))?;
    deadline_handler(con, deadline);

    let existing: Vec<String> = {
        let mut stmt = con.prepare(&format!("PRAGMA main.table_info({PAGES})"))?;
        let rows = stmt.query_map([], |r| r.get::<_, String>(1))?;
        rows.collect::<Result<_, _>>()?
    };
    if !existing.is_empty() && existing != PAGE_COLUMNS {
        con.execute_batch(&format!(
            "DROP TABLE IF EXISTS main.{META}; DROP TABLE IF EXISTS main.{PAGES}"
        ))?;
    }
    con.execute_batch(&format!(
        "CREATE TABLE IF NOT EXISTS main.{PAGES}(url_id INTEGER PRIMARY KEY,document_id INTEGER,resource_coverage_state TEXT NOT NULL,resource_count INTEGER,resource_images INTEGER,resource_scripts INTEGER,resource_stylesheets INTEGER,{kind_columns},title_duplicate INTEGER NOT NULL);
         CREATE TABLE IF NOT EXISTS main.{META}(singleton INTEGER PRIMARY KEY CHECK(singleton=1),projection_version TEXT NOT NULL,scan_uuid TEXT NOT NULL,evidence_revision INTEGER NOT NULL,capabilities_sha256 TEXT NOT NULL,start_url TEXT,page_count INTEGER NOT NULL,resource_coverage_state TEXT NOT NULL,resource_coverage_counts_json TEXT NOT NULL,facet_counts_json TEXT NOT NULL,facet_states_json TEXT NOT NULL);
         DELETE FROM main.{PAGES};
         INSERT INTO main.{PAGES} SELECT * FROM temp.query_projection_pages;
         CREATE INDEX IF NOT EXISTS scan_url_query_resource_images ON {PAGES}(resource_images,url_id);
         CREATE INDEX IF NOT EXISTS scan_url_query_resource_scripts ON {PAGES}(resource_scripts,url_id);
         CREATE INDEX IF NOT EXISTS scan_url_query_resource_stylesheets ON {PAGES}(resource_stylesheets,url_id);"
    ))?;
    for kind in RESOURCE_KINDS {
        con.execute_batch(&format!(
            "CREATE INDEX IF NOT EXISTS scan_url_query_kind_{kind} ON {PAGES}(kind_{kind},url_id)"
        ))?;
    }
    con.execute_batch(&format!(
        "CREATE INDEX IF NOT EXISTS scan_url_query_title_duplicate ON {PAGES}(title_duplicate,url_id);
         DELETE FROM main.{META}"
    ))?;
    let counts_json = serde_json::to_string(&counts.iter().collect::<BTreeMap<_, _>>())?;
    let states_json = serde_json::to_string(&states.iter().collect::<BTreeMap<_, _>>())?;
    con.execute(
        &format!("INSERT INTO main.{META} VALUES(1,?,?,?,?,?,?,?,?,?,?)"),
        rusqlite::params![
            FORMAT,
            source.0,
            source.1,
            capabilities_sha256,
            source.3,
            page_count,
            resource_state,
            serde_json::to_string(&coverage_counts)?,
            counts_json,
            states_json,
        ],
    )?;
    con.execute_batch("RELEASE SAVEPOINT build_scan_url_query_projection")?;
    Ok(())
}
