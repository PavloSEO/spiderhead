//! Joining retained bot observations to a finished crawl: which crawled URLs bots never
//! visited, which URLs bots visit that the crawl never found, and where bot requests are
//! wasted.

use rusqlite::functions::FunctionFlags;
use rusqlite::types::Value as SqlValue;
use rusqlite::Connection;
use serde_json::{json, Map, Value};

use crate::error::{LogsError, Result};
use crate::store::{
    import_metadata, open_log_source, paging, status_directives_indexable, url_section,
};
use crate::summary::{paged, where_clause, Filter};
use crate::urlsplit;

/// Facts about a crawl that the join needs.
#[derive(Debug, Clone)]
pub struct ScanHeader {
    /// `native` or `reanalysis` for a crawl this toolkit captured.
    pub source_kind: String,
    /// Lifecycle state; only `finished` supports absence conclusions.
    pub lifecycle: String,
    /// True when the crawl stopped early.
    pub crawl_partial: bool,
    /// Decoded capabilities document; `pages.state` must be `complete`.
    pub capabilities: Value,
    /// URL the crawl started from; its origin anchors log paths.
    pub start_url: String,
    pub scan_uuid: Value,
    pub evidence_revision: Value,
}

/// One crawled page, reduced to what indexability needs.
#[derive(Debug, Clone)]
pub struct ScanPage {
    pub url: String,
    pub status_code: Option<i64>,
    pub meta_robots: Option<String>,
    pub x_robots: Option<String>,
}

/// Read access to a finished crawl; never written by the join.
pub trait ScanSource {
    /// The crawl's header row.
    fn header(&self) -> std::result::Result<ScanHeader, String>;
    /// Every crawled page, in any order.
    fn pages(&self, each: &mut dyn FnMut(ScanPage)) -> std::result::Result<(), String>;
}

/// Opens crawls by path; the store layer implements it.
pub trait ScanOpener {
    /// Open and validate the crawl at `path`.
    fn open(&self, path: &str) -> std::result::Result<Box<dyn ScanSource>, String>;
}

const VIEWS: [&str; 5] = [
    "scan_without_bot_visits",
    "bot_urls_absent_scan",
    "status_waste",
    "nonindexable",
    "sections",
];
const ABSENCE_VIEWS: [&str; 3] = [
    "scan_without_bot_visits",
    "bot_urls_absent_scan",
    "sections",
];

/// Request for [`join_log_scan`].
#[derive(Debug, Clone)]
pub struct JoinQuery {
    pub view: String,
    pub base_url: String,
    pub bot: String,
    pub family: String,
    pub date_from: String,
    pub date_to: String,
    pub limit: i64,
    pub offset: i64,
}

impl Default for JoinQuery {
    fn default() -> Self {
        Self {
            view: "scan_without_bot_visits".into(),
            base_url: String::new(),
            bot: String::new(),
            family: String::new(),
            date_from: String::new(),
            date_to: String::new(),
            limit: 100,
            offset: 0,
        }
    }
}

/// `path?query` of a log or crawl URL on `origin`, or `None` for another origin or a
/// non-path reference. Query strings, case and trailing slashes are significant.
pub fn url_key(value: &str, origin: &str) -> Result<Option<String>> {
    let parts = urlsplit::split(value)?;
    if !parts.scheme.is_empty() || !parts.netloc.is_empty() {
        let own = format!(
            "{}://{}",
            parts.scheme.to_lowercase(),
            parts.netloc.to_lowercase()
        );
        if own != origin {
            return Ok(None);
        }
    } else if !value.starts_with('/') {
        return Ok(None);
    }
    let path = if parts.path.is_empty() {
        "/"
    } else {
        &parts.path
    };
    Ok(Some(if parts.query.is_empty() {
        path.to_string()
    } else {
        format!("{path}?{}", parts.query)
    }))
}

pub(crate) fn origin_of(url: &str) -> Result<Option<String>> {
    let parts = urlsplit::split(url)?;
    let http = parts.scheme == "http" || parts.scheme == "https";
    Ok((http && !parts.netloc.is_empty()).then(|| {
        format!(
            "{}://{}",
            parts.scheme.to_lowercase(),
            parts.netloc.to_lowercase()
        )
    }))
}

/// Join retained observations to a finished native crawl, never writing the crawl.
///
/// Log-only URLs are discovery candidates, not proven orphans, and absence means only
/// the supplied period. Unavailable evidence is reported as `{"available": false}` data.
pub fn join_log_scan(source: &str, scan: &str, opener: &dyn ScanOpener, q: &JoinQuery) -> Value {
    match join(source, scan, opener, q) {
        Ok(value) => value,
        Err(error) => {
            let text = error.to_string();
            json!({"ok": false, "available": false, "reason": text, "error": text})
        }
    }
}

fn join(source: &str, scan: &str, opener: &dyn ScanOpener, q: &JoinQuery) -> Result<Value> {
    paging(q.limit, q.offset)?;
    if !VIEWS.contains(&q.view.as_str()) {
        return Err(LogsError::value("unsupported log/scan join view"));
    }
    let logs = open_log_source(source)?;
    let native = opener.open(scan).map_err(LogsError::Value)?;
    let header = native.header().map_err(LogsError::Value)?;
    let metadata = import_metadata(&logs)?;
    if header.source_kind != "native" && header.source_kind != "reanalysis" {
        return Err(LogsError::value("join requires a native scan"));
    }
    let pages_state = header
        .capabilities
        .pointer("/pages/state")
        .and_then(Value::as_str);
    if header.lifecycle != "finished" || header.crawl_partial || pages_state != Some("complete") {
        return Err(LogsError::value(
            "native scan is incomplete; missing URLs cannot be inferred",
        ));
    }
    let absence = ABSENCE_VIEWS.contains(&q.view.as_str());
    if absence && metadata["bot_detection_complete"] != json!(true) {
        return Err(LogsError::value(
            "log bot observations are incomplete; absent visits cannot be inferred",
        ));
    }
    let unknown_time = metadata["lines"]["unknown_time"].as_u64().unwrap_or(0);
    if (!q.date_from.is_empty() || !q.date_to.is_empty()) && unknown_time > 0 && absence {
        return Err(LogsError::value(
            "undated log observations prevent absence inference for a date range",
        ));
    }
    let base = if q.base_url.is_empty() {
        header.start_url.as_str()
    } else {
        q.base_url.as_str()
    };
    let origin = origin_of(base)?
        .ok_or_else(|| LogsError::value("an absolute base_url is required to match log paths"))?;
    let target = metadata
        .get("project_target")
        .and_then(Value::as_str)
        .unwrap_or("");
    let log_origin = origin_of(target)?.ok_or_else(|| {
        LogsError::value(
            "log import project origin is unavailable; reimport with an existing project",
        )
    })?;
    if origin != log_origin {
        return Err(LogsError::value(
            "log import project origin differs from the native scan origin",
        ));
    }
    let native_origin = urlsplit::split(&header.start_url)?;
    let native_origin = format!(
        "{}://{}",
        native_origin.scheme.to_lowercase(),
        native_origin.netloc.to_lowercase()
    );
    if !q.base_url.is_empty() && origin != native_origin {
        return Err(LogsError::value(
            "base_url origin differs from the native scan origin",
        ));
    }
    let scan_origin = origin.clone();
    logs.create_scalar_function("url_key", 1, FunctionFlags::SQLITE_UTF8, move |ctx| {
        let url: Option<String> = ctx.get(0)?;
        url.map_or(Ok(None), |u| {
            url_key(&u, &scan_origin).map_err(|e| rusqlite::Error::UserFunctionError(Box::new(e)))
        })
    })?;
    logs.create_scalar_function("url_section", 1, FunctionFlags::SQLITE_UTF8, |ctx| {
        let url: String = ctx.get(0)?;
        url_section(&url).map_err(|e| rusqlite::Error::UserFunctionError(Box::new(e)))
    })?;
    logs.execute_batch("CREATE TEMP TABLE scan_urls(key TEXT PRIMARY KEY,url TEXT,status INTEGER,indexable INTEGER,section TEXT) WITHOUT ROWID")?;
    load_scan_urls(&logs, native.as_ref(), &origin)?;
    let filter = Filter {
        bot: q.bot.clone(),
        family: q.family.clone(),
        date_from: q.date_from.clone(),
        date_to: q.date_to.clone(),
        ..Filter::default()
    };
    let (clause, params) = where_clause(&logs, &filter)?;
    logs.execute_batch("CREATE TEMP TABLE bot_urls(key TEXT PRIMARY KEY,visits INTEGER,last_visit TEXT,redirects INTEGER,client_errors INTEGER,server_errors INTEGER) WITHOUT ROWID")?;
    let refs: Vec<&dyn rusqlite::ToSql> =
        params.iter().map(|p| p as &dyn rusqlite::ToSql).collect();
    logs.execute(
        &format!("INSERT INTO bot_urls SELECT url_key(url),SUM(visits),MAX(last_visit),SUM(CASE WHEN status BETWEEN 300 AND 399 THEN visits ELSE 0 END),SUM(CASE WHEN status BETWEEN 400 AND 499 THEN visits ELSE 0 END),SUM(CASE WHEN status BETWEEN 500 AND 599 THEN visits ELSE 0 END) FROM visits WHERE {clause} AND family NOT IN ('human','unknown') AND url_key(url) IS NOT NULL GROUP BY url_key(url)"),
        refs.as_slice(),
    )?;
    let sql = view_sql(&q.view);
    let mut page = paged(&logs, sql, &[] as &[SqlValue], q.limit, q.offset)?;
    let rows = page.get("rows").cloned().unwrap_or(Value::Null);
    let unknown: i64 = logs.query_row(
        "SELECT COUNT(*) FROM scan_urls WHERE indexable IS NULL",
        [],
        |r| r.get(0),
    )?;
    let mut out = Map::new();
    out.insert("ok".into(), json!(true));
    out.insert("available".into(), json!(true));
    out.insert("view".into(), json!(q.view));
    out.insert("source".into(), json!(source));
    out.insert("scan".into(), json!(scan));
    for (k, v) in page.iter_mut() {
        out.insert(k.clone(), v.take());
    }
    out.insert(q.view.clone(), rows);
    out.insert("scan_uuid".into(), header.scan_uuid);
    out.insert("evidence_revision".into(), header.evidence_revision);
    out.insert("period".into(), metadata["period"].clone());
    out.insert(
        "matching".into(),
        json!("same origin, exact path and query; no case/slash/query normalization"),
    );
    out.insert(
        "limitations".into(),
        json!([
            "Bot identity is a user-agent claim",
            "Absence refers only to the supplied logs and scan scope",
            "Log-only URLs are discovery candidates, not proven orphans",
            "Path-only logs are assumed to belong to the selected scan origin",
        ]),
    );
    out.insert("log_complete".into(), metadata["complete"].clone());
    out.insert(
        "indexability".into(),
        json!({
            "basis": "recorded HTTP status and meta/X-Robots noindex or none directives only; canonical and robots.txt are not inferred",
            "unknown_urls": unknown,
            "state": if unknown > 0 { "partial" } else { "complete_for_basis" },
        }),
    );
    Ok(Value::Object(out))
}

fn load_scan_urls(logs: &Connection, native: &dyn ScanSource, origin: &str) -> Result<()> {
    let mut failure: Option<LogsError> = None;
    let mut insert = logs.prepare("INSERT OR IGNORE INTO scan_urls VALUES(?,?,?,?,?)")?;
    native
        .pages(&mut |page| {
            if failure.is_some() {
                return;
            }
            let step = (|| -> Result<()> {
                let Some(key) = url_key(&page.url, origin)? else {
                    return Ok(());
                };
                let indexable = match page.status_code {
                    None => None,
                    Some(_) => {
                        let (Some(meta), Some(x)) = (&page.meta_robots, &page.x_robots) else {
                            return Err(LogsError::Type(
                                "can only concatenate str (not \"NoneType\") to str".into(),
                            ));
                        };
                        status_directives_indexable(page.status_code, meta, x).map(i64::from)
                    }
                };
                let section = url_section(&key)?;
                insert.execute(rusqlite::params![
                    key,
                    page.url,
                    page.status_code,
                    indexable,
                    section
                ])?;
                Ok(())
            })();
            if let Err(error) = step {
                failure = Some(error);
            }
        })
        .map_err(LogsError::Value)?;
    failure.map_or(Ok(()), Err)
}

fn view_sql(view: &str) -> &'static str {
    match view {
        "scan_without_bot_visits" => "SELECT s.url,s.status,s.indexable status_directives_indexable FROM scan_urls s LEFT JOIN bot_urls b USING(key) WHERE b.key IS NULL ORDER BY s.url",
        "bot_urls_absent_scan" => "SELECT b.key url,b.visits,b.last_visit,'discovery_candidate' classification FROM bot_urls b LEFT JOIN scan_urls s USING(key) WHERE s.key IS NULL ORDER BY b.visits DESC,b.key",
        "status_waste" => "SELECT key url,visits,redirects,client_errors,server_errors FROM bot_urls WHERE redirects+client_errors+server_errors>0 ORDER BY redirects+client_errors+server_errors DESC,key",
        "nonindexable" => "SELECT s.url,s.status,b.visits,b.last_visit FROM scan_urls s JOIN bot_urls b USING(key) WHERE s.indexable=0 ORDER BY b.visits DESC,s.url",
        _ => "WITH all_urls AS (SELECT key FROM scan_urls UNION SELECT key FROM bot_urls) SELECT url_section(a.key) section,COUNT(s.key) scan_urls,COUNT(b.key) bot_visited_urls,SUM(COALESCE(b.visits,0)) visits FROM all_urls a LEFT JOIN scan_urls s USING(key) LEFT JOIN bot_urls b USING(key) GROUP BY url_section(a.key) ORDER BY visits DESC,section",
    }
}
