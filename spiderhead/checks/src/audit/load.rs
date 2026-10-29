//! Reading a scan.v1 file into audit pages (reference the reference suite, native path).

use std::path::Path;

use rusqlite::{types::ValueRef, Connection, OpenFlags};
use serde_json::{Map, Value};
use spiderhead_parse::parser::meta::robots_directives;
use spiderhead_parse::py_url::urlsplit;
use thiserror::Error;

use super::model::Page;

/// Failures while reading a scan.
#[derive(Debug, Error)]
pub enum LoadError {
    /// SQLite refused the file or a query.
    #[error("scan database: {0}")]
    Sqlite(#[from] rusqlite::Error),
    /// No scan connection was opened.
    #[error("no scan connection")]
    Missing,
    /// A store-crate helper failed.
    #[error("store: {0}")]
    Store(String),
}

/// `_same_page`: scheme/host case-folded, path compared without a trailing slash.
pub(crate) fn same_page(url: &str, canonical: &str) -> bool {
    let key = |u: &str| {
        urlsplit(u)
            .map(|s| {
                (
                    s.scheme.to_lowercase(),
                    s.netloc.to_lowercase(),
                    s.path.trim_end_matches('/').to_owned(),
                    s.query,
                    s.fragment,
                )
            })
            .ok()
    };
    key(url) == key(canonical)
}

/// `_indexability`: SF's Indexability pair derived from the stored response and head directives.
/// `blocked_by_robots` takes priority over the fetched outcome exactly as the reference version.
fn indexability(
    blocked_by_robots: bool,
    code: Option<i64>,
    error: &str,
    meta_robots: &str,
    x_robots: &str,
    url: &str,
    canonical: &str,
) -> (&'static str, &'static str) {
    if blocked_by_robots {
        return ("Non-Indexable", "Blocked by Robots.txt");
    }
    let Some(code) = code else {
        return ("Non-Indexable", "Response unavailable");
    };
    let _ = error;
    if (300..400).contains(&code) {
        return ("Non-Indexable", "Redirected");
    }
    if code >= 400 {
        return (
            "Non-Indexable",
            if code < 500 {
                "Client Error"
            } else {
                "Server Error"
            },
        );
    }
    let directives = robots_directives(&[Some(meta_robots), Some(x_robots)]);
    if directives.iter().any(|d| d == "noindex") {
        return ("Non-Indexable", "noindex");
    }
    if !canonical.is_empty() && !same_page(url, canonical) {
        return ("Non-Indexable", "Canonicalised");
    }
    ("Indexable", "")
}

fn cell(v: ValueRef<'_>) -> Value {
    match v {
        ValueRef::Null => Value::Null,
        ValueRef::Integer(i) => Value::from(i),
        ValueRef::Real(f) => Value::from(f),
        ValueRef::Text(t) => Value::from(String::from_utf8_lossy(t).into_owned()),
        ValueRef::Blob(_) => Value::Null,
    }
}

/// Per-destination (Inlinks, Unique Inlinks) over the selected link occurrences that land on a page.
fn inlink_counts(
    con: &Connection,
) -> Result<std::collections::HashMap<String, (i64, i64)>, LoadError> {
    let sql = format!(
        "{}SELECT dst.url, COUNT(*), COUNT(DISTINCT l.source_url_id) FROM l \
         JOIN pages AS p ON p.url_id=l.destination_url_id JOIN urls AS dst ON dst.url_id=l.destination_url_id \
         GROUP BY l.destination_url_id, dst.url",
        super::rules::stored::SELECTED_LINKS
    );
    let mut stmt = con.prepare(&sql)?;
    let rows = stmt.query_map([], |r| {
        Ok((
            r.get::<_, String>(0)?,
            (r.get::<_, i64>(1)?, r.get::<_, i64>(2)?),
        ))
    })?;
    Ok(rows.collect::<Result<_, _>>()?)
}

/// URLs the crawl's own robots evidence marks as disallowed (`robots_blocked_url` context items).
fn robots_blocked_urls(con: &Connection) -> Result<std::collections::HashSet<String>, LoadError> {
    let mut stmt = con.prepare(
        "SELECT u.url FROM urls u JOIN context_items c \
         ON c.kind='robots_blocked_url' AND c.item_key='url:' || u.url_id",
    )?;
    let rows = stmt.query_map([], |r| r.get::<_, String>(0))?;
    Ok(rows.collect::<Result<_, _>>()?)
}

/// Load every page of the scan at `path`, in page order, with its derived verdicts.
pub fn load_pages(path: &Path) -> Result<Vec<Page>, LoadError> {
    let con = Connection::open_with_flags(path, OpenFlags::SQLITE_OPEN_READ_ONLY)?;
    let counts = inlink_counts(&con)?;
    let robots_blocked = robots_blocked_urls(&con)?;
    let mut stmt = con.prepare(
        "SELECT p.*, u.url AS _url FROM pages p JOIN urls u USING(url_id) ORDER BY p.page_ordinal",
    )?;
    let names: Vec<String> = stmt
        .column_names()
        .iter()
        .map(|s| (*s).to_owned())
        .collect();
    let mut rows = stmt.query([])?;
    let mut pages = Vec::new();
    while let Some(row) = rows.next()? {
        let mut rec = Map::new();
        for (i, name) in names.iter().enumerate() {
            rec.insert(name.clone(), cell(row.get_ref(i)?));
        }
        let text = |rec: &Map<String, Value>, k: &str| {
            rec.get(k).and_then(Value::as_str).unwrap_or("").to_owned()
        };
        let url = text(&rec, "_url");
        let title = text(&rec, "title");
        let description = text(&rec, "meta_description");
        rec.insert("title_length".into(), Value::from(title.chars().count()));
        rec.insert(
            "desc_length".into(),
            Value::from(description.chars().count()),
        );
        if text(&rec, "readability").is_empty() {
            rec.insert("readability".into(), Value::Null);
        }
        let code = rec.get("status_code").and_then(Value::as_i64);
        let (inlinks, unique) = counts.get(&url).copied().unwrap_or((0, 0));
        rec.insert("inlinks".into(), Value::from(inlinks));
        rec.insert("unique_inlinks".into(), Value::from(unique));
        let outlinks = rec.get("outlinks").and_then(Value::as_i64).unwrap_or(0);
        let external = rec
            .get("external_outlinks")
            .and_then(Value::as_i64)
            .unwrap_or(0);
        rec.insert("outlinks".into(), Value::from((outlinks - external).max(0)));
        let charset = text(&rec, "charset");
        rec.insert("meta_charset".into(), Value::from(charset));
        let error = text(&rec, "error");
        let status = if !error.is_empty() {
            error
        } else if code == Some(200) {
            "OK".to_owned()
        } else {
            String::new()
        };
        rec.insert("status".into(), Value::from(status));
        let (state, reason) = indexability(
            robots_blocked.contains(&url),
            code,
            &text(&rec, "error"),
            &text(&rec, "meta_robots"),
            &text(&rec, "x_robots"),
            &url,
            &text(&rec, "canonical"),
        );
        let content_type = text(&rec, "content_type");
        for (column, field) in [
            ("trust_signals_json", "trust_signals"),
            ("link_placement_json", "link_placement"),
            ("invalid_link_targets_json", "invalid_link_targets"),
            ("heading_outline_json", "heading_outline"),
            ("duplicate_ids_json", "duplicate_ids"),
            ("canonical_chain_json", "canonical_chain"),
            ("hreflang_json", "hreflang"),
        ] {
            let parsed = rec
                .get(column)
                .and_then(Value::as_str)
                .and_then(|t| serde_json::from_str::<Value>(t).ok());
            rec.insert(field.into(), parsed.unwrap_or(Value::Null));
        }
        let mut page = Page {
            status_code: code,
            content_type,
            indexability: state.to_owned(),
            indexability_status: reason.to_owned(),
            url,
            rec,
            ..Page::default()
        };
        page.fill_metrics();
        pages.push(page);
    }
    Ok(pages)
}

/// The `run` fields a native scan stores on its `scan` row and context items —
/// everything `_audit_crawl_result`'s `native_run` block needs from the DB.
#[derive(Debug, Default, Clone)]
pub struct ScanRun {
    /// `scan_uuid` column.
    pub scan_uuid: String,
    /// `source_kind` column (`native`, `import`, ...).
    pub source_kind: String,
    /// `start_url` column.
    pub start_url: String,
    /// `config_fingerprint` column.
    pub config_fingerprint: String,
    /// `finish_reason` column.
    pub finish_reason: String,
    /// `lifecycle` column.
    pub lifecycle: String,
    /// `crawl_partial` column as a bool.
    pub crawl_partial: bool,
    /// `corpus_partial` column as a bool.
    pub corpus_partial: bool,
    /// Whether a `resume_state` row marks this scan as resumed.
    pub crawl_resumed: bool,
    /// `external_check_summary` payload, when the opt-in phase ran.
    pub external_checks: Option<Value>,
    /// `cache_replay`/`cache_stats` retained in `limitations_json`, when present.
    pub cache_replay: bool,
    /// See `cache_replay`.
    pub cache_stats: Map<String, Value>,
    /// Whether the scan was produced by explicit-list input (`list_mode` context item).
    pub list_mode: bool,
}

/// Read the `scan` row plus the context items `run` metadata needs.
///
/// `crawl_resumed`, `cache_replay` and `cache_stats` are live `CrawlResult` fields the
/// scan does not retain; a stored-scan audit reports their defaults exactly as the
/// reference boundary does when no provenance says otherwise.
pub fn scan_run(con: &Connection) -> Result<ScanRun, LoadError> {
    let mut stmt = con.prepare(
        "SELECT scan_uuid, source_kind, start_url, config_fingerprint, finish_reason, lifecycle, \
         crawl_partial, corpus_partial FROM scan WHERE singleton=1",
    )?;
    let mut run = stmt.query_row([], |r| {
        Ok(ScanRun {
            scan_uuid: r.get::<_, String>(0)?,
            source_kind: r.get::<_, String>(1)?,
            start_url: r.get::<_, String>(2)?,
            config_fingerprint: r.get::<_, String>(3)?,
            finish_reason: r.get::<_, String>(4)?,
            lifecycle: r.get::<_, String>(5)?,
            crawl_partial: r.get::<_, i64>(6)? != 0,
            corpus_partial: r.get::<_, i64>(7)? != 0,
            ..ScanRun::default()
        })
    })?;
    drop(stmt);
    run.list_mode = con
        .query_row(
            "SELECT COUNT(*) FROM context_items WHERE kind='list_mode' AND item_key='run'",
            [],
            |r| r.get::<_, i64>(0),
        )
        .map(|n| n > 0)
        .unwrap_or(false);
    run.external_checks = con
        .query_row(
            "SELECT payload_json FROM external_check_summary WHERE singleton=1",
            [],
            |r| r.get::<_, String>(0),
        )
        .ok()
        .and_then(|t| serde_json::from_str::<Value>(&t).ok());
    Ok(run)
}
