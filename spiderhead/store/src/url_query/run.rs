//! The page-table query: validation, SQL assembly and the answer object.

use std::collections::BTreeSet;
use std::path::Path;
use std::time::Instant;

use rusqlite::params_from_iter;
use rusqlite::types::Value as SqlValue;
use serde_json::{json, Map, Value};

use super::*;
use crate::discovery_ledger::py_dumps;
use crate::error::StoreError;
use crate::link_context::cell_value;
use crate::url_query_filters::{filter_names, int, issue_filter_set, where_, Params};
use crate::url_query_presets::{
    facet_counts, facet_ids, links_measured, preset_filters, sitemap_membership_measured,
};

/// Arguments of one URL query, with the documented defaults.
#[derive(Debug, Clone)]
pub struct QueryArgs {
    /// List of filter objects.
    pub filters: Value,
    /// Sort column name.
    pub sort: Value,
    /// `asc` or `desc`.
    pub direction: Value,
    /// Column names to return.
    pub columns: Value,
    /// Rows to skip.
    pub offset: Value,
    /// Page size.
    pub limit: Value,
    /// Budget of the exact-count query in seconds.
    pub count_timeout_seconds: Value,
    /// Byte cap of the returned rows.
    pub max_bytes: Value,
    /// Preset view id.
    pub preset: Value,
    /// Facet group ids or `"all"`.
    pub facets: Value,
    /// One check id or a list of them.
    pub issue_check: Value,
    /// Issue severity filter.
    pub issue_severity: Value,
    /// Whether to attach the saved audit's issue counters.
    pub include_check_counts: Value,
}

impl Default for QueryArgs {
    fn default() -> Self {
        QueryArgs {
            filters: Value::Null,
            sort: Value::Null,
            direction: json!("asc"),
            columns: Value::Null,
            offset: json!(0),
            limit: json!(DEFAULT_LIMIT),
            count_timeout_seconds: json!(DEFAULT_COUNT_BUDGET_SECONDS),
            max_bytes: json!(1_048_576),
            preset: Value::Null,
            facets: Value::Null,
            issue_check: Value::Null,
            issue_severity: Value::Null,
            include_check_counts: json!(false),
        }
    }
}

impl QueryArgs {
    /// Arguments from a JSON object with snake_case keys; unknown keys are ignored.
    pub fn from_kwargs(kwargs: &Map<String, Value>) -> Self {
        let mut args = QueryArgs::default();
        for (key, value) in kwargs {
            let slot = match key.as_str() {
                "filters" => &mut args.filters,
                "sort" => &mut args.sort,
                "direction" => &mut args.direction,
                "columns" => &mut args.columns,
                "offset" => &mut args.offset,
                "limit" => &mut args.limit,
                "count_timeout_seconds" => &mut args.count_timeout_seconds,
                "max_bytes" => &mut args.max_bytes,
                "preset" => &mut args.preset,
                "facets" => &mut args.facets,
                "issue_check" => &mut args.issue_check,
                "issue_severity" => &mut args.issue_severity,
                "include_check_counts" => &mut args.include_check_counts,
                _ => continue,
            };
            *slot = value.clone();
        }
        args
    }
}

/// Answer one query; failures come back as an `ok: false` object, never as an error.
pub fn scan_url_query(input_path: &str, args: &QueryArgs) -> Map<String, Value> {
    let started = Instant::now();
    match query(input_path, args, started) {
        Ok(v) => v,
        Err(e) => {
            let mut out = Map::new();
            out.insert("ok".into(), json!(false));
            out.insert("state".into(), json!(e.state));
            out.insert("format".into(), json!(FORMAT));
            out.insert("reason_code".into(), json!(e.reason_code));
            out.insert("error".into(), json!(e.message));
            out
        }
    }
}

fn unavailable_json_source(include_counts: bool) -> Map<String, Value> {
    // Clients match on this exact reason text, so it must not be reworded.
    let reason = "Screaming Frog audit JSON is not a saved scan with a page table";
    let mut result = Map::new();
    result.insert("ok".into(), json!(true));
    result.insert("state".into(), json!("unavailable"));
    result.insert("format".into(), json!(FORMAT));
    result.insert("reason_code".into(), json!("source_not_scan"));
    result.insert("reason".into(), json!(reason));
    if include_counts {
        result.insert(
            "issue_summary".into(),
            json!({"state": "unavailable", "reason": reason}),
        );
    }
    result
}

pub(super) fn string_list(value: &Value) -> Option<Vec<String>> {
    match value {
        Value::Array(items) => items
            .iter()
            .map(|v| v.as_str().map(str::to_string))
            .collect(),
        _ => None,
    }
}

fn projection_columns() -> [&'static str; 9] {
    [
        "title_duplicate",
        "resource_count",
        "resource_images",
        "resource_scripts",
        "resource_stylesheets",
        "resource_bytes",
        "resource_bytes_state",
        "resource_url",
        "resource_kind",
    ]
}

/// Run one query and build the answer object.
pub fn query(
    input_path: &str,
    args: &QueryArgs,
    started: Instant,
) -> Result<Map<String, Value>, QueryError> {
    if input_path.is_empty() {
        return Err(QueryError::new("invalid_input", "input_path is required"));
    }
    let offset = int(&args.offset, "offset", 0, 1 << 40)?;
    if args.limit.as_i64().is_some_and(|l| l > MAX_LIMIT) {
        return Err(QueryError::new(
            "limit_too_large",
            format!("limit must be at most {MAX_LIMIT}"),
        ));
    }
    let limit = int(&args.limit, "limit", 1, MAX_LIMIT)?;
    let max_bytes = int(&args.max_bytes, "max_bytes", 4096, 8 * 1024 * 1024)?;
    let count_timeout = match &args.count_timeout_seconds {
        Value::Number(n) if n.as_f64().is_some_and(|t| (0.05..=30.0).contains(&t)) => {
            n.as_f64().unwrap_or(1.0)
        }
        _ => {
            return Err(QueryError::new(
                "invalid_count_timeout",
                "count_timeout_seconds must be 0.05..30",
            ))
        }
    };
    let direction = args
        .direction
        .as_str()
        .filter(|d| *d == "asc" || *d == "desc");
    let Some(direction) = direction else {
        return Err(QueryError::new(
            "invalid_sort",
            "direction must be asc or desc",
        ));
    };
    let mut filters: Vec<Value> = match &args.filters {
        Value::Null => Vec::new(),
        Value::Array(items) => items.clone(),
        _ => {
            return Err(QueryError::new(
                "invalid_filter",
                format!("filters must be a list of at most {MAX_FILTERS}"),
            ))
        }
    };
    if !args.preset.is_null() {
        let mut combined = preset_filters(&args.preset)?;
        combined.extend(filters);
        filters = combined;
    }
    if filters.len() as i64 > MAX_FILTERS {
        return Err(QueryError::new(
            "invalid_filter",
            format!("filters must be a list of at most {MAX_FILTERS}"),
        ));
    }
    let all_columns = columns();
    let selected: Vec<String> = if args.columns.is_null() {
        DEFAULT_COLUMNS.iter().map(|s| s.to_string()).collect()
    } else {
        let list = match &args.columns {
            Value::Array(items) if (1..=all_columns.len()).contains(&items.len()) => items,
            _ => {
                return Err(QueryError::new(
                    "unknown_column",
                    "columns must be a nonempty list of column names",
                ))
            }
        };
        let mut names = Vec::new();
        for item in list {
            match item.as_str() {
                Some(n) if all_columns.contains_key(n) => names.push(n.to_string()),
                _ => {
                    return Err(QueryError::new(
                        "unknown_column",
                        "columns names an unavailable column",
                    ))
                }
            }
        }
        names
    };
    if selected.iter().collect::<BTreeSet<_>>().len() != selected.len() {
        return Err(QueryError::new(
            "unknown_column",
            "columns repeats a column",
        ));
    }
    let mut sort: Option<String> = match &args.sort {
        Value::Null => None,
        Value::String(s) if all_columns.contains_key(s.as_str()) => Some(s.clone()),
        _ => {
            return Err(QueryError::new(
                "unknown_column",
                "sort names an unavailable column",
            ))
        }
    };
    if sort.as_deref() == Some("status_class") {
        sort = Some("status_code".into());
    }
    let issue_filtered = issue_filter_set(&args.issue_check, &args.issue_severity)?;
    let include_check_counts = match &args.include_check_counts {
        Value::Bool(b) => *b,
        _ => {
            return Err(QueryError::new(
                "invalid_check_counts",
                "include_check_counts must be a boolean",
            ))
        }
    };
    let (mut conditions, mut params, mut needs_url) = where_(&filters, false, None)?;
    let facet_list = facet_ids(&args.facets)?;
    if input_path.to_lowercase().ends_with(".json") {
        return Ok(unavailable_json_source(include_check_counts));
    }
    let (con, source, status_index) = open(input_path)?;
    let budget = Budget::new(&con);
    let columns_json = Value::Array(selected.iter().map(|s| json!(s)).collect());

    let fnames = filter_names(&filters);
    let mut requested: BTreeSet<String> = selected.iter().cloned().collect();
    requested.extend(fnames.iter().cloned());
    if let Some(s) = &sort {
        requested.insert(s.clone());
    }
    let projection_requested =
        !facet_list.is_empty() || projection_columns().iter().any(|c| requested.contains(*c));
    let query_projection = if projection_requested {
        budget.start(PAGE_BUDGET_SECONDS);
        // `load` may have hit its bounded integrity count; start a fresh read budget after it.
        let loaded = crate::url_query_projection::load(&con).ok().flatten();
        budget.start(PAGE_BUDGET_SECONDS);
        loaded
    } else {
        None
    };
    let projection_join = if query_projection.is_some() {
        format!(
            " LEFT JOIN main.{} q ON q.url_id=p.url_id",
            crate::url_query_projection::PAGES
        )
    } else {
        String::new()
    };
    let mut query_columns = all_columns.clone();
    let mut query_facets: indexmap::IndexMap<String, String> = FACET_GROUPS
        .iter()
        .map(|(k, v)| (k.to_string(), v.to_string()))
        .collect();
    if query_projection.is_some() {
        for (name, expr, kind) in [
            ("title_duplicate", "q.title_duplicate", "bool"),
            ("resource_count", "q.resource_count", "int"),
            ("resource_images", "q.resource_images", "int"),
            ("resource_scripts", "q.resource_scripts", "int"),
            ("resource_stylesheets", "q.resource_stylesheets", "int"),
        ] {
            query_columns.insert(name.into(), Column::new(expr, kind, false));
        }
        for (k, v) in [
            ("title_duplicate", "q.title_duplicate=1"),
            ("with_resources", "q.resource_count>0"),
            ("with_images", "q.resource_images>0"),
            ("with_scripts", "q.resource_scripts>0"),
            ("with_stylesheets", "q.resource_stylesheets>0"),
        ] {
            query_facets.insert(k.into(), v.into());
        }
        (conditions, params, needs_url) = where_(&filters, true, Some(&query_columns))?;
    }
    let mut where_sql = if conditions.is_empty() {
        String::new()
    } else {
        format!(" WHERE {conditions}")
    };
    let join = if needs_url {
        " JOIN urls u ON u.url_id=p.url_id"
    } else {
        ""
    };
    let mut issue_summary: Option<Value> = None;
    let issue_wanted = requested.contains("issue_count");
    if include_check_counts {
        issue_summary = Some(Value::Object(crate::issue_counters::summarize(
            Path::new(input_path),
            &registry_value(),
            Some(&con),
            Some(&source),
            PAGE_BUDGET_SECONDS,
        )));
    }
    if issue_filtered || issue_wanted {
        budget.start(PAGE_BUDGET_SECONDS);
        let checks: Vec<String> = match &args.issue_check {
            Value::String(s) => vec![s.clone()],
            other => string_list(other).unwrap_or_default(),
        };
        let outcome = crate::issue_selection::materialize(
            &con,
            Path::new(input_path),
            &checks,
            args.issue_severity.as_str(),
            PAGE_BUDGET_SECONDS,
        );
        if let Err(error) = outcome {
            let (reason_code, text) = match &error {
                StoreError::AuditReadTimeout(e) => ("issue_selection_timeout", e.message.clone()),
                StoreError::UnmeasuredChecks(e) => ("issue_check_unmeasured", e.message.clone()),
                StoreError::ScanError(e) => ("issue_index_missing", e.message.clone()),
                StoreError::Invalid(m) => ("issue_index_missing", m.clone()),
                _ => return Err(QueryError::new("cannot_read", error.to_string())),
            };
            let mut out = Map::new();
            out.insert("ok".into(), json!(true));
            out.insert("state".into(), json!("unavailable"));
            out.insert("format".into(), json!(FORMAT));
            out.insert("reason_code".into(), json!(reason_code));
            out.insert(
                "reason".into(),
                json!(format!("saved findings are unavailable: {text}")),
            );
            out.insert("columns".into(), columns_json);
            out.insert("rows".into(), json!([]));
            out.insert("total".into(), Value::Null);
            out.insert("filtered_total".into(), Value::Null);
            out.insert("filters".into(), Value::Array(filters));
            if let Some(s) = issue_summary {
                out.insert("issue_summary".into(), s);
            }
            return Ok(out);
        }
        if issue_filtered {
            let extra = "p.url_id IN (SELECT url_id FROM temp.issue_urls)";
            conditions = if conditions.is_empty() {
                extra.to_string()
            } else {
                format!("{conditions} AND {extra}")
            };
            where_sql = format!(" WHERE {conditions}");
        }
    }
    if requested.contains("in_sitemap") && !sitemap_membership_measured(&con)? {
        return Err(QueryError::with_state(
            "sitemap_membership_unavailable",
            "sitemap membership was not completely retained for this scan",
            "unavailable",
        ));
    }
    if requested.contains("inlinks") && !links_measured(&con)? {
        return Err(QueryError::with_state(
            "inlinks_unavailable",
            "link observations were not retained for this scan",
            "unavailable",
        ));
    }
    let resource_names = [
        "resource_count",
        "resource_images",
        "resource_scripts",
        "resource_stylesheets",
        "resource_bytes",
        "resource_bytes_state",
        "resource_url",
        "resource_kind",
    ];
    let resource_facets = [
        "with_resources",
        "with_images",
        "with_scripts",
        "with_stylesheets",
    ];
    let needs_resource_state = resource_names.iter().any(|c| requested.contains(*c))
        || facet_list
            .iter()
            .any(|f| resource_facets.contains(&f.as_str()));
    let graph_state: Option<String> = if !needs_resource_state {
        None
    } else if let Some(p) = &query_projection {
        Some(p.resource_coverage_state.clone())
    } else {
        Some(resource_graph_state(&con, &budget)?)
    };
    if resource_names.iter().any(|c| requested.contains(*c))
        && graph_state.as_deref() != Some("complete")
    {
        let capped = graph_state.as_deref() == Some("capped");
        return Err(QueryError::with_state(
            if capped {
                "resource_graph_coverage_capped"
            } else {
                "resource_graph_unavailable"
            },
            if capped {
                "resource declaration coverage could not be validated within the query budget"
            } else {
                "complete retained resource declarations are unavailable for every page"
            },
            if capped { "capped" } else { "unavailable" },
        ));
    }
    if requested
        .iter()
        .any(|c| transport_columns().contains_key(c.as_str()))
    {
        prepare_transport_phase_projection(&con)?;
    }
    if fnames.contains("has_hreflang")
        && exists(
            &con,
            "SELECT 1 FROM pages WHERE hreflang_json IS NULL LIMIT 1",
        )?
    {
        return Err(QueryError::with_state(
            "hreflang_unavailable",
            "hreflang presence is not measured for every page",
            "unavailable",
        ));
    }
    click_depth_check(&con, &requested)?;
    let mut indexed: BTreeSet<String> = all_columns
        .iter()
        .filter(|(_, c)| c.indexed)
        .map(|(n, _)| n.clone())
        .collect();
    if status_index {
        indexed.insert("status_code".into());
    }
    if requested.contains("click_depth") {
        indexed.insert("click_depth".into());
    }
    budget.start(PAGE_BUDGET_SECONDS);
    let total = count(&con, "SELECT COUNT(*) FROM pages")?;
    let mut filtered: Option<i64> = Some(total);
    if !conditions.is_empty() {
        budget.start(count_timeout);
        let counted: Result<i64, rusqlite::Error> = con.query_row(
            &format!("SELECT COUNT(*) FROM pages p{join}{projection_join}{where_sql}"),
            params_from_iter(params.iter()),
            |r| r.get(0),
        );
        filtered = match counted {
            Ok(n) => Some(n),
            Err(e) if is_interrupt(&e) => None,
            Err(e) => return Err(sql_error(e)),
        };
    }
    let mut facet_out: Option<(Value, Value, Value)> = None;
    if !facet_list.is_empty() {
        let (counts, states) = match (&query_projection, conditions.is_empty(), issue_filtered) {
            (Some(p), true, false) => (
                facet_list
                    .iter()
                    .map(|n| (n.clone(), p.facets.get(n).copied().flatten()))
                    .collect(),
                facet_list
                    .iter()
                    .map(|n| {
                        (
                            n.clone(),
                            p.facet_states.get(n).cloned().unwrap_or_default(),
                        )
                    })
                    .collect(),
            ),
            _ => {
                let facet_join = format!(" JOIN urls u ON u.url_id=p.url_id{projection_join}");
                facet_counts(
                    &con,
                    &budget,
                    &facet_list,
                    &facet_join,
                    &where_sql,
                    &params,
                    count_timeout,
                    graph_state.as_deref(),
                    Some(&query_facets),
                )?
            }
        };
        let distinct: BTreeSet<&String> = states.values().collect();
        let state = if distinct.len() == 1 {
            json!((*distinct.iter().next().unwrap_or(&&String::new())).clone())
        } else {
            json!("partial")
        };
        let counts_json: Map<String, Value> = counts
            .iter()
            .map(|(k, v)| (k.clone(), v.map_or(Value::Null, |n| json!(n))))
            .collect();
        let states_json: Map<String, Value> =
            states.iter().map(|(k, v)| (k.clone(), json!(v))).collect();
        facet_out = Some((
            Value::Object(counts_json),
            state,
            Value::Object(states_json),
        ));
    }
    let small = filtered.is_some_and(|f| f <= SORT_ROW_CAP);
    let order_col = sort.clone().unwrap_or_else(|| "page_ordinal".into());
    if !small && !indexed.contains(&order_col) {
        let names: Vec<&str> = indexed.iter().map(String::as_str).collect();
        return Err(QueryError::new(
            "sort_not_indexed",
            format!(
                "sorting by {order_col} needs a filter that leaves at most {SORT_ROW_CAP} rows (or a counted result); indexed sorts: {}",
                names.join(", ")
            ),
        ));
    }
    let sense = if direction == "desc" { "DESC" } else { "ASC" };
    let key = query_columns[&order_col].expr.clone();
    budget.start(PAGE_BUDGET_SECONDS);
    let sql = if small {
        format!(
            "WITH f AS MATERIALIZED (SELECT p.url_id AS i, {key} AS k FROM pages p JOIN urls u ON u.url_id=p.url_id{projection_join}{where_sql}) SELECT i FROM f ORDER BY k {sense}, i {sense} LIMIT ? OFFSET ?"
        )
    } else {
        let mut src = match order_col.as_str() {
            "url" => "urls u CROSS JOIN pages p ON p.url_id=u.url_id".to_string(),
            "status_code" => {
                "pages p INDEXED BY pages_status CROSS JOIN urls u ON u.url_id=p.url_id".to_string()
            }
            // The urls join stays in the click-depth walk so u.* filters keep resolving.
            "click_depth" => "pages p JOIN temp.cd_depth cd ON cd.url_id=p.url_id \
                 JOIN urls u ON u.url_id=p.url_id "
                .to_string(),
            _ => "pages p JOIN urls u ON u.url_id=p.url_id".to_string(),
        };
        src.push_str(&projection_join);
        let tie = if ["url", "url_id", "page_ordinal"].contains(&order_col.as_str()) {
            String::new()
        } else {
            format!(", p.url_id {sense}")
        };
        let sort_key = if order_col == "click_depth" {
            "cd.depth".to_string()
        } else {
            key.clone()
        };
        format!("SELECT p.url_id FROM {src}{where_sql} ORDER BY {sort_key} {sense}{tie} LIMIT ? OFFSET ?")
    };
    let mut ids: Vec<i64> = Vec::new();
    let mut partial = false;
    {
        let mut bound: Params = params.clone();
        bound.push(SqlValue::Integer(limit + 1));
        bound.push(SqlValue::Integer(offset));
        let mut stmt = con.prepare(&sql)?;
        let mut rows = stmt.query(params_from_iter(bound.iter()))?;
        loop {
            match rows.next() {
                Ok(Some(r)) => ids.push(r.get(0)?),
                Ok(None) => break,
                // An index-ordered walk yields an exact prefix; a sorted set has nothing until done.
                Err(e) if !small && is_interrupt(&e) => {
                    partial = true;
                    break;
                }
                Err(e) => return Err(sql_error(e)),
            }
        }
    }
    budget.start(PAGE_BUDGET_SECONDS);
    let mut has_more = ids.len() as i64 > limit || partial;
    ids.truncate(limit as usize);
    let mut rows_out: Vec<Value> = Vec::new();
    let mut used: i64 = 0;
    let mut truncated = false;
    if !ids.is_empty() {
        let select = selected
            .iter()
            .enumerate()
            .map(|(i, n)| format!("{} AS c{i}", query_columns[n].expr))
            .collect::<Vec<_>>()
            .join(",");
        let marks = vec!["?"; ids.len()].join(",");
        let sql = format!(
            "SELECT p.url_id,{select} FROM pages p JOIN urls u ON u.url_id=p.url_id{projection_join} WHERE p.url_id IN ({marks})"
        );
        let mut found: std::collections::HashMap<i64, Vec<Value>> =
            std::collections::HashMap::new();
        {
            let mut stmt = con.prepare(&sql)?;
            let mut rows = stmt.query(params_from_iter(ids.iter()))?;
            while let Some(r) = rows.next()? {
                let mut cells = Vec::new();
                for i in 0..=selected.len() {
                    cells.push(cell_value(r.get_ref(i)?));
                }
                found.insert(r.get(0)?, cells);
            }
        }
        for id in &ids {
            let Some(cells) = found.get(id) else { continue };
            let mut item = Map::new();
            for (name, cell) in selected.iter().zip(cells.iter().skip(1)) {
                item.insert(name.clone(), cell.clone());
            }
            if let Some(v) = item.get("indexable").filter(|v| !v.is_null()).cloned() {
                item.insert("indexable".into(), json!(v.as_i64().is_none_or(|n| n != 0)));
            }
            let item = Value::Object(item);
            let size = py_dumps(&item, false, false).len() as i64;
            if used + size > max_bytes {
                truncated = true;
                has_more = true;
                break;
            }
            rows_out.push(item);
            used += size;
        }
    }
    drop(budget);
    let lifecycle = source.get("lifecycle").cloned().unwrap_or(Value::Null);
    let complete = lifecycle.as_str() == Some("finished")
        && source.get("crawl_partial") == Some(&json!(false))
        && source.get("corpus_partial") == Some(&json!(false));
    drop(con);
    let returned = rows_out.len() as i64;
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
            json!("the time budget ended an index-ordered walk before the page filled; rows are the exact leading rows found so far"),
        );
    }
    out.insert("format".into(), json!(FORMAT));
    out.insert("source".into(), Value::Object(source));
    out.insert("columns".into(), columns_json);
    out.insert("rows".into(), Value::Array(rows_out));
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
        filtered.map_or(Value::Null, |n| json!(n)),
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
    if let Some((counts, state, states)) = facet_out {
        out.insert("facets".into(), counts);
        out.insert("facets_state".into(), state);
        out.insert("facet_states".into(), states);
    }
    out.insert(
        "sort".into(),
        json!({
            "column": order_col,
            "direction": direction,
            "tie_breaker": "url_id",
            "mode": if small { "materialized" } else { "index" },
        }),
    );
    out.insert("filters".into(), Value::Array(filters));
    out.insert("preset".into(), args.preset.clone());
    out.insert(
        "coverage".into(),
        json!({
            "rows": "committed page records; queued or excluded URLs without a page are not rows",
            "scan_complete": complete,
            "scan_lifecycle": lifecycle,
        }),
    );
    if let Some(s) = issue_summary {
        out.insert("issue_summary".into(), s);
    }
    out.insert(
        "elapsed_ms".into(),
        json!((started.elapsed().as_secs_f64() * 1000.0).round() as i64),
    );
    Ok(out)
}
