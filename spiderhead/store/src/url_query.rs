//! Server-side filtered, sorted and paginated reads of a saved scan's page table.
//!
//!
//! The query is read-only and never materializes the table: filters and sort names come from a
//! fixed allow-list that maps to SQL fragments, every user value is a bound parameter, and only
//! the ordered row IDs of one page are selected before the requested columns are read for those
//! IDs. This module is the entry point and facade: the fixed vocabulary lives in
//! `url_query_schema`, the filter predicates in `url_query_filters` and the preset views and facet
//! groups in `url_query_presets`.

use std::collections::BTreeSet;
use std::path::Path;
use std::time::{Duration, Instant};

use rusqlite::{params_from_iter, Connection, ErrorCode};
use serde_json::{json, Map, Value};

use crate::click_depth;
use crate::error::StoreError;
use crate::link_context::cell_value;
use crate::scan_artifact::APPLICATION_ID;
use crate::url_query_filters::where_;

pub use crate::url_query_presets::FACET_GROUPS;
pub use crate::url_query_schema::*;

mod export;
mod run;

pub use export::{export, export_scan_query, ExportArgs};
pub use run::{query, scan_url_query, QueryArgs};

/// Largest number of filter sets one batch count accepts.
pub const MAX_BATCH_FILTER_SETS: i64 = 100;
/// Wire format tag of a batch count answer.
pub const COUNTS_FORMAT: &str = "spiderhead.scan-url-counts.v1";
/// Wire format tag of a query export answer.
pub const EXPORT_FORMAT: &str = "spiderhead.scan-url-query-export.v1";
/// Export file formats.
pub const EXPORT_FORMATS: &[&str] = &["csv", "xlsx"];
/// Default row cap for a filtered export (issue #1007).
pub const EXPORT_MAX_ROWS: i64 = 100_000;
/// One XLSX sheet holds 1,048,576 rows; the export writes a header row above its data.
pub const EXPORT_XLSX_ROWS: i64 = 1_048_575;

/// True when SQLite aborted the statement through the progress handler.
pub fn is_interrupt(error: &rusqlite::Error) -> bool {
    matches!(
        error,
        rusqlite::Error::SqliteFailure(e, _) if e.code == ErrorCode::OperationInterrupted
    )
}

/// Map a SQLite failure to the query error callers report (`query_timeout` or `cannot_read`).
pub fn sql_error(error: rusqlite::Error) -> QueryError {
    if is_interrupt(&error) {
        QueryError::new("query_timeout", "query exceeded its time budget")
    } else {
        QueryError::new("cannot_read", format!("cannot read scan: {error}"))
    }
}

impl From<rusqlite::Error> for QueryError {
    fn from(error: rusqlite::Error) -> Self {
        sql_error(error)
    }
}

/// First column of a `SELECT COUNT(*)`-style statement.
pub fn count(con: &Connection, sql: &str) -> Result<i64, QueryError> {
    Ok(con.query_row(sql, [], |r| r.get(0))?)
}

/// True when the statement returns at least one row.
pub fn exists(con: &Connection, sql: &str) -> Result<bool, QueryError> {
    let mut stmt = con.prepare(sql)?;
    let mut rows = stmt.query([])?;
    Ok(rows.next()?.is_some())
}

/// Abort a statement once a wall-clock deadline passes.
pub struct Budget<'a> {
    con: &'a Connection,
    /// SQLite VM steps between deadline checks (tests rebind it).
    pub progress_ops: i32,
}

impl<'a> Budget<'a> {
    /// Budget over one connection; no deadline until [`Budget::start`].
    pub fn new(con: &'a Connection) -> Self {
        Budget {
            con,
            progress_ops: PROGRESS_OPS as i32,
        }
    }

    /// Start a new deadline `seconds` from now, replacing the previous one.
    pub fn start(&self, seconds: f64) {
        let deadline = Instant::now() + Duration::from_secs_f64(seconds.max(0.0));
        self.con
            .progress_handler(self.progress_ops, Some(move || Instant::now() > deadline));
    }

    /// Remove the deadline.
    pub fn stop(&self) {
        self.con.progress_handler(0, None::<fn() -> bool>);
    }
}

impl Drop for Budget<'_> {
    fn drop(&mut self) {
        self.stop();
    }
}

/// Header facts of an opened scan, in a fixed field order.
pub type Source = Map<String, Value>;

/// Open read-only and check identity and schema generation without validating contents.
///
/// Returns the connection, the source header and whether the `pages_status` index exists.
pub fn open(path: &str) -> Result<(Connection, Source, bool), QueryError> {
    let file = Path::new(path);
    if !file.is_file() {
        return Err(QueryError::with_state(
            "cannot_open",
            "scan file does not exist",
            "unavailable",
        ));
    }
    let absolute = std::path::absolute(file).unwrap_or_else(|_| file.to_path_buf());
    let con = spiderhead_core::sqlite::open_readonly(
        &absolute,
        Duration::from_secs(5),
        &[
            "PRAGMA trusted_schema=OFF",
            "PRAGMA query_only=ON",
            "PRAGMA cache_size=-65536",
            "PRAGMA temp_store=FILE",
        ],
    );
    let con = match con {
        Ok(c) => c,
        Err(e) => return Err(open_failure(&e)),
    };
    let (source, status_index) = read_header(&con)?;
    Ok((con, source, status_index))
}

fn open_failure(error: &rusqlite::Error) -> QueryError {
    let text = error.to_string().to_lowercase();
    if text.contains("not a database")
        || text.contains("no such table")
        || text.contains("no such column")
    {
        QueryError::with_state(
            "not_a_scan",
            "file is not a readable SEOHEAD scan",
            "unavailable",
        )
    } else {
        QueryError::with_state(
            "cannot_open",
            format!("cannot read scan: {error}"),
            "unavailable",
        )
    }
}

fn read_header(con: &Connection) -> Result<(Source, bool), QueryError> {
    let db = |e: rusqlite::Error| open_failure(&e);
    con.execute_batch("BEGIN").map_err(db)?;
    let application_id: i64 = con
        .query_row("PRAGMA application_id", [], |r| r.get(0))
        .map_err(db)?;
    if application_id != APPLICATION_ID {
        return Err(QueryError::with_state(
            "not_a_scan",
            "file is not a SEOHEAD scan",
            "unavailable",
        ));
    }
    let version: i64 = con
        .query_row("PRAGMA user_version", [], |r| r.get(0))
        .map_err(db)?;
    if version != 1 && version != 2 {
        return Err(QueryError::new(
            "unsupported_schema",
            format!("unsupported scan user_version {version}"),
        ));
    }
    let row = {
        let mut stmt = con
            .prepare(
                "SELECT scan_uuid,format_version,source_kind,evidence_revision,lifecycle,finish_reason,crawl_partial,corpus_partial,capabilities_json FROM scan WHERE singleton=1",
            )
            .map_err(db)?;
        let mut rows = stmt.query([]).map_err(db)?;
        match rows.next().map_err(db)? {
            Some(r) => {
                let mut cells = Vec::new();
                for i in 0..9 {
                    cells.push(cell_value(r.get_ref(i).map_err(db)?));
                }
                Some(cells)
            }
            None => None,
        }
    };
    let Some(row) = row.filter(|r| r[1].as_str() == Some(&format!("scan.v{version}"))) else {
        return Err(QueryError::new(
            "unsupported_schema",
            "scan header disagrees with its schema version",
        ));
    };
    let have: BTreeSet<String> = {
        let mut stmt = con.prepare("PRAGMA table_info(pages)").map_err(db)?;
        let rows = stmt.query_map([], |r| r.get::<_, String>(1)).map_err(db)?;
        rows.collect::<Result<_, _>>().map_err(db)?
    };
    let has = |sql: &str| -> Result<bool, QueryError> {
        let mut stmt = con.prepare(sql).map_err(db)?;
        let mut rows = stmt.query([]).map_err(db)?;
        Ok(rows.next().map_err(db)?.is_some())
    };
    let has_urls = has("SELECT 1 FROM sqlite_master WHERE type='table' AND name='urls'")?;
    if !REQUIRED_PAGE_COLUMNS.iter().all(|c| have.contains(*c)) || !has_urls {
        return Err(QueryError::new(
            "unsupported_schema",
            "scan pages table lacks required columns",
        ));
    }
    let status_index =
        has("SELECT 1 FROM sqlite_master WHERE type='index' AND name='pages_status'")?;
    let truthy = |v: &Value| v.as_i64().is_some_and(|n| n != 0);
    let mut source = Source::new();
    source.insert("scan_uuid".into(), row[0].clone());
    source.insert("format_version".into(), row[1].clone());
    source.insert("source_kind".into(), row[2].clone());
    source.insert("evidence_revision".into(), row[3].clone());
    source.insert("lifecycle".into(), row[4].clone());
    source.insert("finish_reason".into(), row[5].clone());
    source.insert("crawl_partial".into(), Value::Bool(truthy(&row[6])));
    source.insert("corpus_partial".into(), Value::Bool(truthy(&row[7])));
    let pages_cap = row[8]
        .as_str()
        .and_then(|t| serde_json::from_str::<Value>(t).ok())
        .and_then(|v| v.get("pages").cloned())
        .filter(Value::is_object)
        .unwrap_or(Value::Null);
    source.insert("pages_coverage".into(), pages_cap);
    Ok((source, status_index))
}

/// Prepare the saved or temporary click-depth index only when requested.
pub fn click_depth_check(con: &Connection, names: &BTreeSet<String>) -> Result<(), QueryError> {
    if !names.contains("click_depth") {
        return Ok(());
    }
    // The file stays read-only (mode=ro); only the temp database is written, so lift query_only.
    con.execute_batch("PRAGMA query_only=OFF")?;
    let outcome = click_depth::prepare_query(con);
    con.execute_batch("PRAGMA query_only=ON")?;
    match outcome {
        Ok(()) => Ok(()),
        Err(StoreError::Invalid(message)) => Err(QueryError::with_state(
            "click_depth_unavailable",
            format!("click_depth is not available: {message}"),
            "unavailable",
        )),
        Err(StoreError::Sqlite(e)) => Err(sql_error(e)),
        Err(other) => Err(QueryError::new("cannot_read", other.to_string())),
    }
}

/// Validate full source-document coverage under a bounded SQLite query.
pub fn resource_graph_state(con: &Connection, budget: &Budget<'_>) -> Result<String, QueryError> {
    let version: i64 = con.query_row("PRAGMA user_version", [], |r| r.get(0))?;
    if version != 2 {
        return Ok("unavailable".into());
    }
    let tables: BTreeSet<String> = {
        let mut stmt = con.prepare(
            "SELECT name FROM main.sqlite_master WHERE type='table' AND name IN ('resource_graph_occurrences','resource_graph_fetches')",
        )?;
        let rows = stmt.query_map([], |r| r.get::<_, String>(0))?;
        rows.collect::<Result<_, _>>()?
    };
    if tables.len() != 2 {
        return Ok("unavailable".into());
    }
    budget.start(PAGE_BUDGET_SECONDS);
    match exists(
        con,
        "SELECT 1 FROM pages p LEFT JOIN context_items c ON c.kind='resource_graph_coverage' AND c.item_key='document:'||p.document_id WHERE p.document_id IS NULL OR c.completeness IS NOT 'complete' LIMIT 1",
    ) {
        Ok(missing) => Ok(if missing { "unavailable" } else { "complete" }.into()),
        Err(e) if e.reason_code == "query_timeout" => Ok("capped".into()),
        Err(e) => Err(e),
    }
}

/// Expose unavailable phase columns for older/frozen scan files without schema writes.
pub fn prepare_transport_phase_projection(con: &Connection) -> Result<(), QueryError> {
    const PHASES: [&str; 5] = ["dns", "connect", "tls", "ttfb", "download"];
    let present = exists(
        con,
        "SELECT name FROM main.sqlite_master WHERE type='table' AND name='response_transport_meta'",
    )?;
    if !present {
        con.execute_batch("PRAGMA query_only=OFF")?;
        let phases = PHASES
            .iter()
            .map(|p| {
                format!(
                    "NULL AS {p}_seconds,'unavailable' AS {p}_state,'transport phase was not retained in this scan' AS {p}_reason"
                )
            })
            .collect::<Vec<_>>()
            .join(",");
        let outcome = con.execute_batch(&format!(
            "CREATE TEMP VIEW response_transport_meta AS SELECT NULL AS response_id,NULL AS protocol,NULL AS protocol_state,NULL AS total_seconds,{phases} WHERE 0"
        ));
        con.execute_batch("PRAGMA query_only=ON")?;
        return Ok(outcome?);
    }
    let columns: BTreeSet<String> = {
        let mut stmt = con.prepare("PRAGMA main.table_info(response_transport_meta)")?;
        let rows = stmt.query_map([], |r| r.get::<_, String>(1))?;
        rows.collect::<Result<_, _>>()?
    };
    let complete = PHASES.iter().all(|p| {
        ["seconds", "state", "reason"]
            .iter()
            .all(|s| columns.contains(&format!("{p}_{s}")))
    });
    if complete {
        return Ok(());
    }
    con.execute_batch("PRAGMA query_only=OFF")?;
    let phases = PHASES
        .iter()
        .map(|p| {
            format!(
                "NULL AS {p}_seconds,'unavailable' AS {p}_state,'transport phase durations were not retained in this scan' AS {p}_reason"
            )
        })
        .collect::<Vec<_>>()
        .join(",");
    let outcome = con.execute_batch(&format!(
        "CREATE TEMP VIEW response_transport_meta AS SELECT response_id,protocol,protocol_state,total_seconds,{phases} FROM main.response_transport_meta"
    ));
    con.execute_batch("PRAGMA query_only=ON")?;
    Ok(outcome?)
}

/// The check registry as `{check_id: {severity}}`, the shape the issue counters read.
fn registry_value() -> Value {
    let mut map = Map::new();
    if let Ok(registry) = spiderhead_formats::check_registry::builtin() {
        for check in registry.checks() {
            map.insert(
                check.id.clone(),
                json!({"severity": check.severity.as_str()}),
            );
        }
    }
    Value::Object(map)
}

fn failure_counts(error: &QueryError) -> Map<String, Value> {
    let mut out = Map::new();
    out.insert("ok".into(), json!(false));
    out.insert("format".into(), json!(COUNTS_FORMAT));
    out.insert("state".into(), json!(error.state));
    out.insert("reason_code".into(), json!(error.reason_code));
    out.insert("error".into(), json!(error.message));
    out
}

/// Exact `filtered_total` of many filter sets over one scan, in one process (#1553).
///
/// One connection and one shared time budget: a dashboard asks for every counter in a
/// single CLI call instead of paying interpreter start-up per count.
pub fn scan_url_counts(
    input_path: &str,
    filter_sets: &Value,
    count_timeout_seconds: f64,
) -> Map<String, Value> {
    let started = Instant::now();
    match url_counts(input_path, filter_sets, count_timeout_seconds, started) {
        Ok(v) => v,
        Err(e) => failure_counts(&e),
    }
}

fn url_counts(
    input_path: &str,
    filter_sets: &Value,
    timeout: f64,
    started: Instant,
) -> Result<Map<String, Value>, QueryError> {
    let sets = match filter_sets {
        Value::Array(items) if (1..=MAX_BATCH_FILTER_SETS as usize).contains(&items.len()) => items,
        _ => {
            return Err(QueryError::new(
                "invalid_filter_sets",
                format!("filter_sets must be a list of 1..{MAX_BATCH_FILTER_SETS} filter lists"),
            ))
        }
    };
    for filters in sets {
        match filters {
            Value::Array(items) if items.len() as i64 <= MAX_FILTERS => {}
            _ => {
                return Err(QueryError::new(
                    "invalid_filter",
                    format!("each set must be a list of at most {MAX_FILTERS}"),
                ))
            }
        }
    }
    let mut answers = Vec::new();
    let (con, _source, _status_index) = open(input_path)?;
    let budget = Budget::new(&con);
    for filters in sets {
        let list = filters.as_array().map(Vec::as_slice).unwrap_or(&[]);
        let (conditions, params, needs_url) = where_(list, false, None)?;
        let where_sql = if conditions.is_empty() {
            String::new()
        } else {
            format!(" WHERE {conditions}")
        };
        let join = if needs_url {
            " JOIN urls u ON u.url_id=p.url_id"
        } else {
            ""
        };
        let remaining = timeout - started.elapsed().as_secs_f64();
        budget.start(remaining.max(0.0));
        let counted: Result<i64, rusqlite::Error> = con.query_row(
            &format!("SELECT COUNT(*) FROM pages p{join}{where_sql}"),
            params_from_iter(params.iter()),
            |r| r.get(0),
        );
        match counted {
            Ok(n) => answers.push(json!({"filters": filters, "filtered_total": n})),
            Err(e) if is_interrupt(&e) => {
                answers.push(json!({"filters": filters, "filtered_total": null}))
            }
            Err(e) => return Err(sql_error(e)),
        }
    }
    drop(budget);
    drop(con);
    let mut out = Map::new();
    out.insert("ok".into(), json!(true));
    out.insert("format".into(), json!(COUNTS_FORMAT));
    out.insert("scan".into(), json!(input_path));
    out.insert("sets".into(), json!(answers.len()));
    out.insert(
        "elapsed_ms".into(),
        json!((started.elapsed().as_secs_f64() * 1000.0) as i64),
    );
    out.insert("counts".into(), Value::Array(answers));
    Ok(out)
}
