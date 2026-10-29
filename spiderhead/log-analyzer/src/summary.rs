//! Filtered, paged aggregate views over a retained log artifact.

use rusqlite::functions::FunctionFlags;
use rusqlite::types::Value as SqlValue;
use rusqlite::Connection;
use serde_json::{json, Map, Value};

use crate::clock::valid_date;
use crate::error::{LogsError, Result};
use crate::net::{parse_ip, Network};
use crate::store::{import_metadata, open_log_source, paging, positive, query_rows};

/// Row filters shared by the summary and join views; empty strings mean "any".
#[derive(Debug, Clone, Default)]
pub struct Filter {
    pub bot: String,
    pub family: String,
    pub ip: String,
    pub date_from: String,
    pub date_to: String,
    pub status: Option<i64>,
    pub section: String,
}

fn check_date(value: &str) -> Result<()> {
    let b = value.as_bytes();
    if b.len() == 8 && b.iter().all(u8::is_ascii_digit) {
        return Err(LogsError::value("dates must use YYYY-MM-DD"));
    }
    let shaped = b.len() == 10
        && b[4] == b'-'
        && b[7] == b'-'
        && b.iter()
            .enumerate()
            .all(|(i, c)| i == 4 || i == 7 || c.is_ascii_digit());
    if !shaped {
        return Err(LogsError::value(format!(
            "Invalid isoformat string: {}",
            crate::text::quoted(value)
        )));
    }
    let n = |r: std::ops::Range<usize>| value[r].parse::<i64>().unwrap_or(0);
    if !valid_date(n(0..4), n(5..7), n(8..10)) {
        return Err(LogsError::value("dates must use YYYY-MM-DD"));
    }
    Ok(())
}

/// Build the `WHERE` clause and parameters for `filter`, registering the `ip_matches`
/// function on `con` when an IP network is requested.
pub fn where_clause(con: &Connection, filter: &Filter) -> Result<(String, Vec<SqlValue>)> {
    let mut clauses: Vec<String> = Vec::new();
    let mut params: Vec<SqlValue> = Vec::new();
    for (name, value) in [
        ("bot", &filter.bot),
        ("family", &filter.family),
        ("section", &filter.section),
    ] {
        if !value.is_empty() {
            clauses.push(format!("{name}=?"));
            params.push(SqlValue::Text(value.clone()));
        }
    }
    for (value, operator) in [(&filter.date_from, ">="), (&filter.date_to, "<=")] {
        if !value.is_empty() {
            check_date(value)?;
            clauses.push(format!("date!='' AND date{operator}?"));
            params.push(SqlValue::Text(value.clone()));
        }
    }
    if !filter.date_from.is_empty()
        && !filter.date_to.is_empty()
        && filter.date_from > filter.date_to
    {
        return Err(LogsError::value("date_from must not be after date_to"));
    }
    if let Some(status) = filter.status {
        positive(status, "status", 599)?;
        if status < 100 {
            return Err(LogsError::value("status must be between 100 and 599"));
        }
        clauses.push("status=?".into());
        params.push(SqlValue::Integer(status));
    }
    if !filter.ip.is_empty() {
        let network = Network::parse(&filter.ip)?;
        con.create_scalar_function("ip_matches", 1, FunctionFlags::SQLITE_UTF8, move |ctx| {
            let value: String = ctx.get(0)?;
            Ok(i64::from(
                parse_ip(&value).is_some_and(|a| network.contains(a)),
            ))
        })?;
        clauses.push("ip_matches(ip)=1".into());
    }
    let clause = if clauses.is_empty() {
        "1".to_string()
    } else {
        clauses.join(" AND ")
    };
    Ok((clause, params))
}

/// One bounded page of `query`'s rows, with exact totals.
pub fn paged(
    con: &Connection,
    query: &str,
    params: &[SqlValue],
    limit: i64,
    offset: i64,
) -> Result<Map<String, Value>> {
    paging(limit, offset)?;
    let refs: Vec<&dyn rusqlite::ToSql> =
        params.iter().map(|p| p as &dyn rusqlite::ToSql).collect();
    let total: i64 = con.query_row(
        &format!("SELECT COUNT(*) FROM ({query})"),
        refs.as_slice(),
        |r| r.get(0),
    )?;
    let mut all = params.to_vec();
    all.push(SqlValue::Integer(limit));
    all.push(SqlValue::Integer(offset));
    let rows = query_rows(con, &format!("{query} LIMIT ? OFFSET ?"), &all)?;
    let shown = rows.len() as i64;
    let more = offset + shown < total;
    let mut out = Map::new();
    out.insert(
        "rows".into(),
        Value::Array(rows.into_iter().map(Value::Object).collect()),
    );
    out.insert("total".into(), json!(total));
    out.insert("limit".into(), json!(limit));
    out.insert("offset".into(), json!(offset));
    out.insert("has_more".into(), json!(more));
    out.insert(
        "next_offset".into(),
        if more {
            json!(offset + shown)
        } else {
            Value::Null
        },
    );
    Ok(out)
}

/// Views of [`log_summary`] and the columns each groups by.
pub(crate) const VIEWS: [(&str, &str); 6] = [
    ("urls", "url"),
    ("bot_ips", "bot,family,ip"),
    ("bots", "bot,family"),
    ("daily", "date"),
    ("statuses", "family,status"),
    ("sections", "family,section"),
];

/// Paging and filters of a summary request.
#[derive(Debug, Clone)]
pub struct SummaryQuery {
    pub view: String,
    pub limit: i64,
    pub offset: i64,
    pub filter: Filter,
}

impl Default for SummaryQuery {
    fn default() -> Self {
        Self {
            view: "urls".into(),
            limit: 100,
            offset: 0,
            filter: Filter::default(),
        }
    }
}

/// Exact totals and one bounded filtered aggregate page; never performs DNS.
pub fn log_summary(source: &str, query: &SummaryQuery) -> Value {
    match summarize(source, query) {
        Ok(value) => value,
        Err(error) => json!({"ok": false, "error": error.to_string()}),
    }
}

/// Views beyond the oracle's six, added on top of the same aggregates:
/// `file_types` (content group from the extension), `urls_detail` (per-URL averages and
/// first/last day) and `browsers` (human browser families; needs per-request rows).
pub(crate) const EXTRA_VIEWS: [&str; 3] = ["file_types", "urls_detail", "browsers"];

fn extra_view(source: &str, q: &SummaryQuery) -> Result<Value> {
    use crate::groups::{browser_of, file_type};
    let con = open_log_source(source)?;
    con.create_scalar_function("file_type", 1, FunctionFlags::SQLITE_UTF8, |ctx| {
        Ok(file_type(&ctx.get::<String>(0)?))
    })?;
    con.create_scalar_function("browser_of", 1, FunctionFlags::SQLITE_UTF8, |ctx| {
        Ok(browser_of(
            &ctx.get::<Option<String>>(0)?.unwrap_or_default(),
        ))
    })?;
    let (clause, mut params) = where_clause(&con, &q.filter)?;
    let meta = import_metadata(&con)?;
    let sql = match q.view.as_str() {
        "file_types" => format!(
            "SELECT file_type(url) file_type,SUM(visits) visits,COUNT(DISTINCT url) urls,SUM(bytes) bytes,MAX(last_visit) last_visit FROM visits WHERE {clause} GROUP BY 1 ORDER BY visits DESC,1"
        ),
        "urls_detail" => {
            let end = meta["period"]["to"].as_str().unwrap_or("").get(..10).unwrap_or("");
            params.insert(0, SqlValue::Text(end.to_string()));
            format!(
                "SELECT url,file_type(url) file_type,section,SUM(visits) visits,SUM(bytes) bytes,
                 ROUND(1.0*SUM(bytes)/SUM(visits),1) avg_bytes,
                 MIN(NULLIF(date,'')) first_date,MAX(NULLIF(date,'')) last_date,MAX(last_visit) last_visit,
                 CASE WHEN ?='' OR MAX(NULLIF(date,'')) IS NULL THEN NULL
                  ELSE CAST(julianday(?)-julianday(MAX(NULLIF(date,''))) AS INTEGER) END days_since_last,
                 COUNT(DISTINCT status) statuses,COUNT(DISTINCT bot) bots
                 FROM visits WHERE {clause} GROUP BY url ORDER BY visits DESC,url"
            )
        }
        _ => {
            if !crate::store::has_requests(&con) {
                return Err(LogsError::value(
                    "the browsers view needs per-request rows; import with keep_requests",
                ));
            }
            if !q.filter.section.is_empty() {
                return Err(LogsError::value("the browsers view has no section filter"));
            }
            format!(
                "SELECT browser_of(user_agent) browser,COUNT(*) visits,COUNT(DISTINCT url) urls,SUM(bytes) bytes,MAX(ts) last_visit FROM requests WHERE bot='human' AND {clause} GROUP BY 1 ORDER BY visits DESC,1"
            )
        }
    };
    if q.view == "urls_detail" {
        // The end-of-period parameter appears twice.
        let first = params[0].clone();
        params.insert(1, first);
    }
    let mut page = paged(&con, &sql, &params, q.limit, q.offset)?;
    let rows = page.get("rows").cloned().unwrap_or(Value::Null);
    let mut out = Map::new();
    out.insert("ok".into(), json!(true));
    out.insert("source".into(), json!(source));
    out.insert("view".into(), json!(q.view));
    for (k, v) in page.iter_mut() {
        out.insert(k.clone(), v.take());
    }
    out.insert(q.view.clone(), rows);
    out.insert("import".into(), meta);
    Ok(Value::Object(out))
}

fn summarize(source: &str, q: &SummaryQuery) -> Result<Value> {
    paging(q.limit, q.offset)?;
    if EXTRA_VIEWS.contains(&q.view.as_str()) {
        return extra_view(source, q);
    }
    let group = VIEWS
        .iter()
        .find(|(name, _)| *name == q.view)
        .map(|(_, g)| *g)
        .ok_or_else(|| LogsError::value("unsupported log summary view"))?;
    let con = open_log_source(source)?;
    let (mut clause, params) = where_clause(&con, &q.filter)?;
    if q.view == "bot_ips" {
        clause.push_str(" AND family NOT IN ('human','unknown')");
    }
    let sql = format!(
        "SELECT {group},SUM(visits) visits,SUM(bytes) bytes,MAX(last_visit) last_visit FROM visits WHERE {clause} GROUP BY {group} ORDER BY visits DESC,{group}"
    );
    let mut page = paged(&con, &sql, &params, q.limit, q.offset)?;
    let totals = query_rows(
        &con,
        &format!("SELECT COALESCE(SUM(visits),0) visits,COUNT(DISTINCT url) urls,COUNT(DISTINCT ip) ips FROM visits WHERE {clause}"),
        &params,
    )?;
    let rows = page.get("rows").cloned().unwrap_or(Value::Null);
    let mut out = Map::new();
    out.insert("ok".into(), json!(true));
    out.insert("source".into(), json!(source));
    out.insert("view".into(), json!(q.view));
    for (k, v) in page.iter_mut() {
        out.insert(k.clone(), v.take());
    }
    out.insert(q.view.clone(), rows);
    out.insert(
        "totals".into(),
        totals.into_iter().next().map_or(Value::Null, Value::Object),
    );
    out.insert("import".into(), import_metadata(&con)?);
    out.insert(
        "coverage".into(),
        json!("exact counts for retained and filtered observations; bots are user-agent claims"),
    );
    Ok(Value::Object(out))
}
