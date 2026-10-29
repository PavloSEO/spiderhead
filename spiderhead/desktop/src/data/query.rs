//! The paged, filtered and sorted read of `pages` the URL table and the
//! `table.*` commands share. Port of `_query` in the reference suite
//! reduced to scan.v1 vocabulary: same JSON shape, same limits, same sort
//! strategy (index walk above the row cap, materialized sort below it).

use std::time::Instant;

use rusqlite::types::Value as SqlValue;
use rusqlite::OptionalExtension;
use serde_json::{json, Map, Value};

use super::columns::{
    column, DEFAULT_COLUMNS, DEFAULT_COUNT_BUDGET_SECONDS, MAX_FILTERS, MAX_LIMIT,
    PAGE_BUDGET_SECONDS, SORT_ROW_CAP,
};
use super::filters::{
    filter_names, int_param, issue_checks, issue_filter_set, where_clause, QueryError,
};
use super::presets::{facet_groups, facet_ids, preset_filters, RESOURCE_FACETS};
use super::presets::{has_start_url, links_measured, sitemap_membership_measured};
use super::session::ScanSession;

/// Result format identifier, same as the reference.
pub const FORMAT: &str = "spiderhead.scan-url-query.v1";

/// One table query: filters, sort and the requested window.
#[derive(Debug, Clone, Default)]
pub struct Query {
    /// AND-combined filter objects (reference `filters`).
    pub filters: Vec<Value>,
    /// Sort column name; `None` orders by `page_ordinal`.
    pub sort: Option<String>,
    /// `asc` or `desc`.
    pub direction: String,
    /// Requested column names; empty means [`DEFAULT_COLUMNS`].
    pub columns: Vec<String>,
    /// First row index.
    pub offset: i64,
    /// Page size (1..=200).
    pub limit: i64,
    /// Bound on the filtered-total count.
    pub count_timeout: f64,
    /// Result byte cap; over it rows stop mid-page (`truncated`).
    pub max_bytes: i64,
    /// Preset name (merged before `filters`).
    pub preset: Option<String>,
    /// Facet spec: null, `"all"` or `[id, …]`.
    pub facets: Value,
    /// Issue check id(s): null, string or list.
    pub issue_check: Value,
    /// Issue severity: null or one of critical/warning/notice/opportunity.
    pub issue_severity: Value,
}

impl Query {
    /// The base request the table issues when nothing else is set.
    pub fn base() -> Self {
        Self {
            direction: "asc".into(),
            limit: MAX_LIMIT.min(200),
            count_timeout: DEFAULT_COUNT_BUDGET_SECONDS,
            max_bytes: 1_048_576,
            ..Default::default()
        }
    }

    /// Build a query from command params (`table.page`/`table.sort`/`table.filter`).
    pub fn from_params(params: &Value, previous: &Query) -> Result<Self, QueryError> {
        let mut q = previous.clone();
        if let Some(v) = params.get("filters") {
            q.filters = super::filters::validate_filters(v)?;
        }
        if let Some(v) = params.get("preset") {
            q.preset = match v {
                Value::String(s) => Some(s.clone()),
                _ => None,
            };
        }
        if let Some(v) = params.get("sort") {
            q.sort = match v {
                Value::String(s) => Some(s.clone()),
                _ => None,
            };
        }
        if let Some(v) = params.get("direction").and_then(Value::as_str) {
            q.direction = v.to_string();
        }
        if let Some(v) = params.get("columns") {
            q.columns = match v {
                Value::Array(a) => a
                    .iter()
                    .filter_map(|x| x.as_str().map(str::to_owned))
                    .collect(),
                _ => vec![],
            };
        }
        if let Some(v) = params.get("offset") {
            q.offset = v.as_i64().unwrap_or(q.offset);
        }
        if let Some(v) = params.get("limit") {
            q.limit = v.as_i64().unwrap_or(q.limit);
        }
        if let Some(v) = params.get("count_timeout_seconds").and_then(Value::as_f64) {
            q.count_timeout = v;
        }
        if let Some(v) = params.get("facets") {
            q.facets = v.clone();
        }
        if let Some(v) = params.get("issue_check") {
            q.issue_check = v.clone();
        }
        if let Some(v) = params.get("issue_severity") {
            q.issue_severity = v.clone();
        }
        Ok(q)
    }
}

fn interrupted(e: &rusqlite::Error) -> bool {
    matches!(
        e,
        rusqlite::Error::SqliteFailure(
            rusqlite::ffi::Error {
                code: rusqlite::ErrorCode::OperationInterrupted,
                ..
            },
            _,
        )
    ) || e.to_string().to_lowercase().contains("interrupt")
}

/// Run the query; the result mirrors `scan-url-query` on the same file.
pub fn run(session: &mut ScanSession, q: &Query) -> Result<Value, QueryError> {
    let started = Instant::now();
    let offset = int_param(&json!(q.offset), "invalid_offset", "offset", 0, 1i64 << 40)?;
    if q.limit > MAX_LIMIT {
        return Err(QueryError::invalid(
            "limit_too_large",
            format!("limit must be at most {MAX_LIMIT}"),
        ));
    }
    let limit = int_param(&json!(q.limit), "invalid_limit", "limit", 1, MAX_LIMIT)?;
    let max_bytes = int_param(
        &json!(q.max_bytes),
        "invalid_max_bytes",
        "max_bytes",
        4096,
        8 * 1024 * 1024,
    )?;
    if !(0.05..=30.0).contains(&q.count_timeout) {
        return Err(QueryError::invalid(
            "invalid_count_timeout",
            "count_timeout_seconds must be 0.05..30",
        ));
    }
    if q.direction != "asc" && q.direction != "desc" {
        return Err(QueryError::invalid(
            "invalid_sort",
            "direction must be asc or desc",
        ));
    }
    let mut filters = q.filters.clone();
    if let Some(preset) = &q.preset {
        let mut head = preset_filters(preset)?;
        head.append(&mut filters);
        filters = head;
    }
    if filters.len() > MAX_FILTERS {
        return Err(QueryError::invalid(
            "invalid_filter",
            format!("filters must be a list of at most {MAX_FILTERS}"),
        ));
    }
    let columns: Vec<String> = if q.columns.is_empty() {
        DEFAULT_COLUMNS.iter().map(|s| s.to_string()).collect()
    } else {
        q.columns.clone()
    };
    if columns.is_empty() || columns.len() > super::columns::COLUMNS.len() {
        return Err(QueryError::invalid(
            "unknown_column",
            "columns must be a nonempty list of column names",
        ));
    }
    let mut dedup = columns.clone();
    dedup.sort();
    dedup.dedup();
    if dedup.len() != columns.len() {
        return Err(QueryError::invalid(
            "unknown_column",
            "columns repeats a column",
        ));
    }
    for name in &columns {
        if column(name).is_none() {
            return Err(QueryError::invalid(
                "unknown_column",
                "columns names an unavailable column",
            ));
        }
    }
    let mut sort = q.sort.clone();
    if let Some(name) = &sort {
        if column(name).is_none() {
            return Err(QueryError::invalid(
                "unknown_column",
                "sort names an unavailable column",
            ));
        }
        if name == "status_class" {
            sort = Some("status_code".into());
        }
    }
    let issue_filtered = issue_filter_set(&q.issue_check, &q.issue_severity)?;
    let (mut conditions, params, needs_url) = where_clause(&filters)?;
    let facet_id_list = facet_ids(&q.facets)?;

    // issue_count needs the same selection the issue filter reads.
    let issue_wanted = {
        let names = filter_names(&filters);
        names.contains("issue_count")
            || columns.iter().any(|c| c == "issue_count")
            || sort.as_deref() == Some("issue_count")
    };
    if issue_filtered || issue_wanted {
        let checks = issue_checks(&q.issue_check);
        let severity = q.issue_severity.as_str();
        if let Err(e) = session.materialize_issues(&checks, severity) {
            return Ok(json!({
                "ok": true,
                "state": "unavailable",
                "format": FORMAT,
                "reason_code": e.reason_code,
                "reason": format!("saved findings are unavailable: {}", e.message),
                "columns": columns,
                "rows": [],
                "total": Value::Null,
                "filtered_total": Value::Null,
                "filters": filters,
                "preset": q.preset,
            }));
        }
        if issue_filtered {
            let clause = "p.url_id IN (SELECT url_id FROM temp.issue_urls)";
            conditions = if conditions.is_empty() {
                clause.to_string()
            } else {
                format!("{conditions} AND {clause}")
            };
        }
    }
    let names = filter_names(&filters);
    let mut requested: std::collections::BTreeSet<&str> =
        names.iter().map(String::as_str).collect();
    requested.extend(columns.iter().map(String::as_str));
    if let Some(s) = &sort {
        requested.insert(s.as_str());
    }
    if requested.contains("in_sitemap") && !sitemap_membership_measured(&session.con) {
        return Err(QueryError::unavailable(
            "sitemap_membership_unavailable",
            "sitemap membership was not completely retained for this scan",
        ));
    }
    if requested.contains("inlinks") && !links_measured(&session.con) {
        return Err(QueryError::unavailable(
            "inlinks_unavailable",
            "link observations were not retained for this scan",
        ));
    }
    if names.contains("has_hreflang")
        && session
            .con
            .query_row(
                "SELECT 1 FROM pages WHERE hreflang_json IS NULL LIMIT 1",
                [],
                |r| r.get::<_, i64>(0),
            )
            .optional()
            .map_err(db_err)?
            .is_some()
    {
        return Err(QueryError::unavailable(
            "hreflang_unavailable",
            "hreflang presence is not measured for every page",
        ));
    }
    let indexed: std::collections::BTreeSet<&str> = {
        let mut set: std::collections::BTreeSet<&str> = super::columns::COLUMNS
            .iter()
            .filter(|(_, c)| c.indexed)
            .map(|(n, _)| *n)
            .collect();
        if session.status_index {
            set.insert("status_code");
        }
        set
    };
    session.budget(PAGE_BUDGET_SECONDS);
    let result = run_inner(
        session,
        offset,
        limit,
        max_bytes,
        q,
        &filters,
        &columns,
        sort.as_deref(),
        conditions.as_str(),
        params.as_slice(),
        needs_url,
        &indexed,
        &facet_id_list,
        started,
    );
    session.clear_budget();
    result
}

#[allow(clippy::too_many_arguments)]
fn run_inner(
    session: &ScanSession,
    offset: i64,
    limit: i64,
    max_bytes: i64,
    q: &Query,
    filters: &[Value],
    columns: &[String],
    sort: Option<&str>,
    conditions: &str,
    params: &[SqlValue],
    needs_url: bool,
    indexed: &std::collections::BTreeSet<&str>,
    facet_id_list: &[String],
    started: Instant,
) -> Result<Value, QueryError> {
    let con = &session.con;
    let where_sql = if conditions.is_empty() {
        String::new()
    } else {
        format!(" WHERE {conditions}")
    };
    // The filtered count joins `urls` only when a condition reads it; the
    // materialized/index paths and facets always join it (their keys do).
    let count_join = if needs_url {
        " JOIN urls u ON u.url_id=p.url_id"
    } else {
        ""
    };
    session.budget(PAGE_BUDGET_SECONDS);
    let total: i64 = con
        .query_row("SELECT COUNT(*) FROM pages", [], |r| r.get(0))
        .map_err(db_err)?;
    let mut filtered: Option<i64> = Some(total);
    if !conditions.is_empty() {
        session.budget(q.count_timeout);
        filtered = match con.query_row(
            &format!("SELECT COUNT(*) FROM pages p{count_join}{where_sql}"),
            rusqlite::params_from_iter(params.iter()),
            |r| r.get(0),
        ) {
            Ok(v) => Some(v),
            Err(e) if interrupted(&e) => None,
            Err(e) => return Err(db_err(e)),
        };
    }
    // Facet counts in one pass; unavailable groups stay explicit.
    let mut facet_counts = Map::new();
    let mut facet_states = Map::new();
    if !facet_id_list.is_empty() {
        let groups = facet_groups();
        let expr_of = |id: &str| -> String {
            groups
                .iter()
                .find(|(g, _)| g == id)
                .map(|(_, e)| e.clone())
                .unwrap_or_else(|| "NULL".into())
        };
        let mut unavailable: Vec<&String> = Vec::new();
        let capped: Vec<&String> = Vec::new();
        for id in facet_id_list {
            if ((id == "internal" || id == "external") && !has_start_url(con))
                || (id == "in_sitemap" && !sitemap_membership_measured(con))
                || (id == "with_inlinks" && !links_measured(con))
                || RESOURCE_FACETS.contains(&id.as_str())
            {
                // resource graph coverage is never complete on scan.v1
                unavailable.push(id);
            }
        }
        let active: Vec<&String> = facet_id_list
            .iter()
            .filter(|id| !unavailable.contains(id) && !capped.contains(id))
            .collect();
        for id in unavailable {
            facet_counts.insert(id.clone(), Value::Null);
            facet_states.insert(id.clone(), json!("unavailable"));
        }
        if !active.is_empty() {
            let exprs: Vec<String> = active
                .iter()
                .map(|id| {
                    format!(
                        "COALESCE(SUM(CASE WHEN ({}) THEN 1 ELSE 0 END),0)",
                        expr_of(id)
                    )
                })
                .collect();
            session.budget(q.count_timeout);
            let sql = format!(
                "SELECT {} FROM pages p JOIN urls u ON u.url_id=p.url_id",
                exprs.join(","),
            );
            match con.query_row(
                &format!("{sql}{where_sql}"),
                rusqlite::params_from_iter(params.iter()),
                |r| {
                    (0..active.len())
                        .map(|i| r.get::<_, i64>(i))
                        .collect::<Result<Vec<_>, _>>()
                },
            ) {
                Ok(counts) => {
                    for (id, n) in active.iter().zip(counts) {
                        facet_counts.insert((*id).clone(), json!(n));
                        facet_states.insert((*id).clone(), json!("exact"));
                    }
                }
                Err(e) if interrupted(&e) => {
                    for id in &active {
                        facet_counts.insert((*id).clone(), Value::Null);
                        facet_states.insert((*id).clone(), json!("capped"));
                    }
                }
                Err(e) => return Err(db_err(e)),
            }
        }
    }
    let small = filtered.is_some_and(|f| f <= SORT_ROW_CAP);
    let order_col = sort.unwrap_or("page_ordinal");
    if !small && !indexed.contains(order_col) {
        let list: Vec<&str> = indexed.iter().copied().collect();
        return Err(QueryError::invalid(
            "sort_not_indexed",
            format!(
                "sorting by {order_col} needs a filter that leaves at most {SORT_ROW_CAP} rows \
                 (or a counted result); indexed sorts: {}",
                list.join(", ")
            ),
        ));
    }
    let sense = if q.direction == "desc" { "DESC" } else { "ASC" };
    let key = column(order_col)
        .map(|c| c.expr)
        .unwrap_or("p.page_ordinal");
    session.budget(PAGE_BUDGET_SECONDS);
    let sql = if small {
        format!(
            "WITH f AS MATERIALIZED (SELECT p.url_id AS i, {key} AS k \
             FROM pages p JOIN urls u ON u.url_id=p.url_id{where_sql}) \
             SELECT i FROM f ORDER BY k {sense}, i {sense} LIMIT ? OFFSET ?"
        )
    } else {
        let src = if order_col == "url" {
            "urls u CROSS JOIN pages p ON p.url_id=u.url_id".to_string()
        } else if order_col == "status_code" {
            "pages p INDEXED BY pages_status CROSS JOIN urls u ON u.url_id=p.url_id".to_string()
        } else {
            "pages p JOIN urls u ON u.url_id=p.url_id".to_string()
        };
        let tie = if matches!(order_col, "url" | "url_id" | "page_ordinal") {
            String::new()
        } else {
            format!(", p.url_id {sense}")
        };
        format!(
            "SELECT p.url_id FROM {src}{where_sql} ORDER BY {key} {sense}{tie} LIMIT ? OFFSET ?"
        )
    };
    let mut ids: Vec<i64> = Vec::new();
    let mut partial = false;
    {
        let mut stmt = con.prepare(&sql).map_err(db_err)?;
        let found = stmt
            .query_map(
                rusqlite::params_from_iter(
                    params
                        .iter()
                        .cloned()
                        .chain([SqlValue::from(limit + 1), SqlValue::from(offset)]),
                ),
                |r| r.get::<_, i64>(0),
            )
            .map_err(db_err)?;
        for row in found {
            match row {
                Ok(id) => ids.push(id),
                Err(e) if interrupted(&e) && !small => {
                    partial = true;
                    break;
                }
                Err(e) => return Err(db_err(e)),
            }
        }
    }
    session.budget(PAGE_BUDGET_SECONDS);
    let mut has_more = ids.len() as i64 > limit || partial;
    ids.truncate(limit as usize);
    let mut rows: Vec<Value> = Vec::new();
    let mut used = 0usize;
    let mut truncated = false;
    if !ids.is_empty() {
        let select: Vec<String> = columns
            .iter()
            .enumerate()
            .map(|(i, n)| format!("{} AS c{i}", column(n).map(|c| c.expr).unwrap_or("NULL")))
            .collect();
        let marks = vec!["?"; ids.len()].join(",");
        let fetch = format!(
            "SELECT p.url_id,{} FROM pages p JOIN urls u ON u.url_id=p.url_id \
             WHERE p.url_id IN ({marks})",
            select.join(",")
        );
        let mut stmt = con.prepare(&fetch).map_err(db_err)?;
        let width = columns.len();
        let found_rows = stmt
            .query_map(rusqlite::params_from_iter(ids.iter().cloned()), |r| {
                let mut v = Vec::with_capacity(width + 1);
                for i in 0..=width {
                    v.push(r.get::<_, SqlValue>(i)?);
                }
                Ok(v)
            })
            .map_err(db_err)?;
        let mut by_id: std::collections::HashMap<i64, Vec<SqlValue>> =
            std::collections::HashMap::new();
        for row in found_rows {
            let row = row.map_err(db_err)?;
            if let Some(SqlValue::Integer(id)) = row.first() {
                by_id.insert(*id, row);
            }
        }
        for id in &ids {
            let Some(row) = by_id.get(id) else { continue };
            let mut item = Map::new();
            for (i, name) in columns.iter().enumerate() {
                let v = row.get(i + 1).cloned().unwrap_or(SqlValue::Null);
                let v = sql_to_json(v);
                let v = if (name == "indexable"
                    || name == "title_duplicate"
                    || name == "description_duplicate"
                    || name == "canonical_self"
                    || name == "canonical_other"
                    || name == "canonical_non_indexable"
                    || name == "robots_blocked"
                    || name == "in_sitemap")
                    && !v.is_null()
                {
                    json!(v.as_i64().unwrap_or(0) != 0)
                } else {
                    v
                };
                item.insert(name.clone(), v);
            }
            let size = serde_json::to_string(&item)
                .map(|s| s.len())
                .unwrap_or_default();
            if used + size > max_bytes as usize {
                truncated = true;
                has_more = true;
                break;
            }
            used += size;
            rows.push(Value::Object(item));
        }
    }
    let distinct_states: std::collections::BTreeSet<String> = facet_states
        .values()
        .filter_map(|v| v.as_str().map(str::to_owned))
        .collect();
    let facets_state = if facet_id_list.is_empty() {
        None
    } else if distinct_states.len() == 1 {
        distinct_states.into_iter().next()
    } else {
        Some("partial".into())
    };
    let complete = session.source.get("lifecycle").and_then(Value::as_str) == Some("finished")
        && !session
            .source
            .get("crawl_partial")
            .and_then(Value::as_bool)
            .unwrap_or(false)
        && !session
            .source
            .get("corpus_partial")
            .and_then(Value::as_bool)
            .unwrap_or(false);
    let returned = rows.len() as i64;
    let mut out = Map::new();
    out.insert("ok".into(), json!(true));
    out.insert(
        "state".into(),
        json!(if partial { "partial" } else { "available" }),
    );
    if partial {
        out.insert("reason_code".into(), json!("page_budget_exceeded"));
        out.insert(
            "reason".into(),
            json!(
                "the time budget ended an index-ordered walk before the page filled; \
                   rows are the exact leading rows found so far"
            ),
        );
    }
    out.insert("format".into(), json!(FORMAT));
    out.insert("source".into(), Value::Object(session.source.clone()));
    out.insert("columns".into(), json!(columns));
    out.insert("rows".into(), Value::Array(rows));
    out.insert("offset".into(), json!(offset));
    out.insert("limit".into(), json!(limit));
    out.insert("returned".into(), json!(returned));
    out.insert("has_more".into(), json!(has_more));
    out.insert("next_offset".into(), json!(offset + returned));
    out.insert("truncated".into(), json!(truncated));
    out.insert("bytes".into(), json!(used));
    out.insert("total".into(), json!(total));
    out.insert(
        "filtered_total".into(),
        filtered.map_or(Value::Null, |f| json!(f)),
    );
    out.insert(
        "filtered_total_state".into(),
        json!(if filtered.is_some() {
            "exact"
        } else {
            "capped"
        }),
    );
    out.insert("total_state".into(), json!("exact"));
    if !facet_id_list.is_empty() {
        out.insert("facets".into(), Value::Object(facet_counts));
        if let Some(s) = facets_state {
            out.insert("facets_state".into(), json!(s));
        }
        out.insert("facet_states".into(), Value::Object(facet_states));
    }
    out.insert(
        "sort".into(),
        json!({
            "column": order_col,
            "direction": q.direction,
            "tie_breaker": "url_id",
            "mode": if small { "materialized" } else { "index" },
        }),
    );
    out.insert("filters".into(), json!(filters));
    out.insert(
        "preset".into(),
        q.preset.as_ref().map_or(Value::Null, |s| json!(s)),
    );
    out.insert(
        "coverage".into(),
        json!({
            "rows": "committed page records; queued or excluded URLs without a page are not rows",
            "scan_complete": complete,
            "scan_lifecycle": session.source.get("lifecycle").cloned().unwrap_or(Value::Null),
        }),
    );
    out.insert(
        "elapsed_ms".into(),
        json!(started.elapsed().as_millis() as i64),
    );
    Ok(Value::Object(out))
}

fn db_err(e: rusqlite::Error) -> QueryError {
    if interrupted(&e) {
        QueryError::invalid("query_timeout", "query exceeded its time budget")
    } else {
        QueryError::unavailable("cannot_read", format!("cannot read scan: {e}"))
    }
}

/// SQLite value → JSON value in the reference output's shape.
pub fn sql_to_json(v: SqlValue) -> Value {
    match v {
        SqlValue::Null => Value::Null,
        SqlValue::Integer(i) => json!(i),
        SqlValue::Real(f) => json!(f),
        SqlValue::Text(t) => json!(t),
        SqlValue::Blob(b) => json!(format!("<{} bytes>", b.len())),
    }
}
