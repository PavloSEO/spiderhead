//! Link composition over a stored scan: the
//! `StoredGraph` composition population (canonical page identities), the
//! per-destination position rows, and the aggregate metadata the `link_position`
//! block of `_audit_crawl_result` emits.

use rusqlite::{Connection, OptionalExtension};
use serde_json::{Map, Value};
use spiderhead_parse::py_url::{urlsplit, urlunsplit};
use spiderhead_store::analysis_graph::selected_links_cte;

use super::link_predicates::stored_list;
use super::{details, py_truthy, round_to};
use crate::audit::AuditContext;

/// Link positions counted as boilerplate.
const BOILERPLATE_POSITIONS: [&str; 5] = ["nav", "header", "sidebar", "footer", "breadcrumbs"];
/// `composition(graph, max_pages=...)`.
const MAX_PAGES: usize = 100_000;

/// Canonical key: fragment dropped, nothing else rewritten.
fn canonical_key(url: &str) -> String {
    match urlsplit(url) {
        Ok(parts) => urlunsplit(
            &parts.scheme,
            &parts.netloc,
            if parts.path.is_empty() {
                "/"
            } else {
                &parts.path
            },
            &parts.query,
            "",
        ),
        Err(_) => url.to_owned(),
    }
}

/// `_ensure_composition_population`: `e_graph_page_keys` and
/// `e_graph_destination_ids` as connection-local TEMP tables. Mirrors the
/// `StoredGraph` pragma dance so a read-only main database still works.
fn ensure_population(con: &Connection) -> rusqlite::Result<i64> {
    let before: i64 = con.query_row("PRAGMA query_only", [], |r| r.get(0))?;
    if before != 0 {
        con.execute("PRAGMA query_only=OFF", [])?;
    }
    let result = (|| -> rusqlite::Result<()> {
        con.execute(
            "CREATE TEMP TABLE e_graph_page_keys(key TEXT PRIMARY KEY) WITHOUT ROWID",
            [],
        )?;
        con.execute(
            "CREATE TEMP TABLE e_graph_destination_ids(url_id INTEGER PRIMARY KEY) WITHOUT ROWID",
            [],
        )?;
        let page_urls: Vec<String> = {
            let mut stmt = con.prepare(
                "SELECT u.url FROM pages AS p JOIN urls AS u USING(url_id) ORDER BY p.page_ordinal",
            )?;
            let rows = stmt.query_map([], |r| r.get::<_, String>(0))?;
            rows.collect::<rusqlite::Result<Vec<String>>>()?
        };
        for url in page_urls {
            con.execute(
                "INSERT OR IGNORE INTO e_graph_page_keys(key) VALUES(?)",
                [canonical_key(&url)],
            )?;
        }
        let urls: Vec<(i64, String)> = {
            let mut stmt = con.prepare("SELECT url_id,url FROM urls ORDER BY url_id")?;
            let rows = stmt.query_map([], |r| Ok((r.get::<_, i64>(0)?, r.get::<_, String>(1)?)))?;
            rows.collect::<rusqlite::Result<Vec<(i64, String)>>>()?
        };
        for (url_id, url) in urls {
            let present = con
                .query_row(
                    "SELECT 1 FROM e_graph_page_keys WHERE key=?",
                    [canonical_key(&url)],
                    |r| r.get::<_, i64>(0),
                )
                .optional()?
                .is_some();
            if present {
                con.execute(
                    "INSERT OR IGNORE INTO e_graph_destination_ids(url_id) VALUES(?)",
                    [url_id],
                )?;
            }
        }
        Ok(())
    })();
    match result {
        Ok(()) => Ok(before),
        Err(err) => {
            drop_population(con);
            if before != 0 {
                let _ = con.execute("PRAGMA query_only=ON", []);
            }
            Err(err)
        }
    }
}

/// `StoredGraph.close`: remove the eligibility TEMP tables and restore
/// `PRAGMA query_only` to the value observed on entry.
fn drop_population(con: &Connection) {
    let _ = con.execute("DROP TABLE IF EXISTS temp.e_graph_inlink_counts", []);
    let _ = con.execute("DROP TABLE IF EXISTS temp.e_graph_destination_ids", []);
    let _ = con.execute("DROP TABLE IF EXISTS temp.e_graph_page_keys", []);
}

/// `_composition_unclassified_by_destination`: DISTINCT (destination, source)
/// count of unclassified edges per destination.
fn unclassified_by_destination(con: &Connection) -> Map<String, Value> {
    let sql = selected_links_cte("l")
        + "SELECT dst.url AS url, COUNT(DISTINCT l.source_url_id) AS count \
           FROM l JOIN e_graph_destination_ids AS p ON p.url_id=l.destination_url_id \
           JOIN urls AS dst ON dst.url_id=l.destination_url_id \
           WHERE l.position='' GROUP BY l.destination_url_id, dst.url";
    let mut out = Map::new();
    if let Ok(mut stmt) = con.prepare(&sql) {
        if let Ok(rows) = stmt.query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)?)))
        {
            for row in rows.flatten() {
                out.insert(row.0, Value::from(row.1));
            }
        }
    }
    out
}

/// `_composition_row`.
fn composition_row(
    url: &str,
    counts: &Map<String, Value>,
    dest_unclassified: i64,
) -> Map<String, Value> {
    let total: i64 = counts.values().map(|v| v.as_i64().unwrap_or(0)).sum();
    let boilerplate: i64 = BOILERPLATE_POSITIONS
        .iter()
        .map(|position| counts.get(*position).and_then(Value::as_i64).unwrap_or(0))
        .sum();
    details([
        ("url", Value::from(url)),
        ("inlinks_total", Value::from(total + dest_unclassified)),
        ("by_position", Value::Object(counts.clone())),
        ("inlinks_unclassified", Value::from(dest_unclassified)),
        (
            "boilerplate_only",
            Value::from(!counts.is_empty() && boilerplate == total && dest_unclassified == 0),
        ),
    ])
}

/// `iter_composition_rows`: one dict per crawled destination, sorted by URL then
/// position (SQL order).
fn composition_rows(con: &Connection) -> Vec<Map<String, Value>> {
    let sql = selected_links_cte("l")
        + ", classified AS (\
           SELECT DISTINCT l.destination_url_id, l.source_url_id, l.position \
           FROM l JOIN e_graph_destination_ids AS p ON p.url_id=l.destination_url_id \
           WHERE l.position <> '') \
           SELECT dst.url AS url, classified.position, COUNT(*) AS count \
           FROM classified JOIN urls AS dst ON dst.url_id=classified.destination_url_id \
           GROUP BY classified.destination_url_id, dst.url, classified.position \
           ORDER BY dst.url COLLATE BINARY, classified.position COLLATE BINARY";
    let dest_unclassified = unclassified_by_destination(con);
    let mut pages: Vec<Map<String, Value>> = Vec::new();
    let mut current_url: Option<String> = None;
    let mut counts = Map::new();
    let flush = |pages: &mut Vec<Map<String, Value>>,
                 current_url: &Option<String>,
                 counts: &Map<String, Value>| {
        if let Some(url) = current_url {
            let unclassified = dest_unclassified
                .get(url)
                .and_then(Value::as_i64)
                .unwrap_or(0);
            pages.push(composition_row(url, counts, unclassified));
        }
    };
    if let Ok(mut stmt) = con.prepare(&sql) {
        if let Ok(rows) = stmt.query_map([], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, i64>(2)?,
            ))
        }) {
            for row in rows.flatten() {
                let (url, position, count) = row;
                if current_url.as_ref().is_some_and(|cur| cur != &url) {
                    flush(&mut pages, &current_url, &counts);
                    counts = Map::new();
                }
                current_url = Some(url);
                counts.insert(position, Value::from(count));
            }
        }
    }
    flush(&mut pages, &current_url, &counts);
    pages.truncate(MAX_PAGES + 1);
    pages
}

/// `CompositionMetadata.as_dict` + the queries behind it.
fn composition_metadata(con: &Connection) -> Map<String, Value> {
    let classified_sql = selected_links_cte("l")
        + ", classified AS (\
           SELECT DISTINCT l.destination_url_id, l.source_url_id, l.position \
           FROM l JOIN e_graph_destination_ids AS p ON p.url_id=l.destination_url_id \
           WHERE l.position <> '') \
           SELECT COUNT(*) AS edges, COUNT(DISTINCT destination_url_id) AS pages FROM classified";
    let (edges_classified, pages_with_inlinks) = con
        .prepare(&classified_sql)
        .and_then(|mut stmt| stmt.query_row([], |r| Ok((r.get::<_, i64>(0)?, r.get::<_, i64>(1)?))))
        .unwrap_or((0, 0));
    let unclassified_sql = selected_links_cte("l")
        + "SELECT COUNT(*) FROM l \
           JOIN e_graph_destination_ids AS p ON p.url_id=l.destination_url_id WHERE l.position=''";
    let edges_unclassified = con
        .prepare(&unclassified_sql)
        .and_then(|mut stmt| stmt.query_row([], |r| r.get::<_, i64>(0)))
        .unwrap_or(0);
    let nonpage_sql = selected_links_cte("l")
        + "SELECT COUNT(*) FROM l \
           LEFT JOIN e_graph_destination_ids AS p ON p.url_id=l.destination_url_id \
           WHERE p.url_id IS NULL";
    let edges_nonpage_destination = con
        .prepare(&nonpage_sql)
        .and_then(|mut stmt| stmt.query_row([], |r| r.get::<_, i64>(0)))
        .unwrap_or(0);
    let total = edges_classified + edges_unclassified;
    let classified_fraction = if total > 0 {
        round_to(edges_classified as f64 / total as f64, 4)
    } else {
        0.0
    };
    details([
        ("ok", Value::from(true)),
        ("measured", Value::from(edges_classified > 0)),
        ("edges_classified", Value::from(edges_classified)),
        ("edges_unclassified", Value::from(edges_unclassified)),
        (
            "edges_nonpage_destination",
            Value::from(edges_nonpage_destination),
        ),
        ("classified_fraction", Value::from(classified_fraction)),
        ("pages_with_inlinks", Value::from(pages_with_inlinks)),
    ])
}

/// `composition(graph, max_pages=100_000)`: the `link_position` block of
/// `_audit_crawl_result` — metadata keys first, then population/pages/lists.
pub fn composition(con: &Connection) -> Map<String, Value> {
    let Ok(before) = ensure_population(con) else {
        return Map::new();
    };
    let mut pages = composition_rows(con);
    if pages.len() > MAX_PAGES {
        // `ScanError("native composition output exceeds its compatibility
        // limit")` — the reference audit fails at this bound; no fixture reaches
        // it, so the output is truncated rather than invented.
        pages.truncate(MAX_PAGES);
    }
    let mut link_position = composition_metadata(con);
    drop_population(con);
    if before != 0 {
        let _ = con.execute("PRAGMA query_only=ON", []);
    }
    let boilerplate: Vec<Value> = pages
        .iter()
        .filter(|page| {
            page.get("boilerplate_only")
                .and_then(Value::as_bool)
                .unwrap_or(false)
        })
        .map(|page| page.get("url").cloned().unwrap_or(Value::Null))
        .collect();
    let mut findings = Vec::new();
    if !boilerplate.is_empty() {
        let shown: Vec<String> = boilerplate
            .iter()
            .take(5)
            .filter_map(|v| v.as_str().map(str::to_owned))
            .collect();
        let more = if boilerplate.len() > 5 {
            format!(" and {} more", boilerplate.len() - 5)
        } else {
            String::new()
        };
        findings.push(Value::from(format!(
            "{} page(s) are linked only from navigation, header, sidebar, or footer — never from body content: {}{}",
            boilerplate.len(),
            shown.join(", "),
            more,
        )));
    }
    link_position.insert("population".into(), Value::from("crawled_destinations"));
    link_position.insert(
        "pages".into(),
        Value::Array(pages.into_iter().map(Value::Object).collect()),
    );
    link_position.insert("pages_boilerplate_only".into(), Value::Array(boilerplate));
    link_position.insert("findings".into(), Value::Array(findings));
    link_position
}

/// The `link_position` block of `_audit_crawl_result`: stored-graph composition
/// feeds `ctx.link_position` and emits INLINK_BOILERPLATE_ONLY — withheld on a
/// partial crawl, where "never linked from body content" cannot be proven.
pub fn check_link_composition(ctx: &mut AuditContext) {
    let classify = ctx
        .config
        .get("link_position")
        .and_then(|v| v.get("classify"));
    // A list scan keeps a stored start_url the handler never uses as `url`.
    let list_scan = ctx.scan.as_ref().is_some_and(stored_list);
    let url = if list_scan {
        None
    } else {
        ctx.start_url.clone()
    };
    if url.is_none() || !py_truthy(classify) || ctx.scan.is_none() {
        return;
    }
    let con = ctx.scan.take();
    ctx.link_position = con.as_ref().map(composition).unwrap_or_default();
    ctx.scan = con;
    if ctx.crawl_partial {
        ctx.skip(
            "INLINK_BOILERPLATE_ONLY",
            "crawl is partial: 'never linked from body content' cannot be proven when the crawl did not reach every URL",
        );
        return;
    }
    let pages: Vec<(String, i64, Value)> = ctx
        .link_position
        .get("pages")
        .and_then(Value::as_array)
        .map(|pages| {
            pages
                .iter()
                .filter(|page| {
                    page.get("boilerplate_only")
                        .and_then(Value::as_bool)
                        .unwrap_or(false)
                })
                .map(|page| {
                    (
                        page.get("url")
                            .and_then(Value::as_str)
                            .unwrap_or_default()
                            .to_owned(),
                        page.get("inlinks_total")
                            .and_then(Value::as_i64)
                            .unwrap_or(0),
                        page.get("by_position").cloned().unwrap_or(Value::Null),
                    )
                })
                .collect()
        })
        .unwrap_or_default();
    for (url, total, by_position) in pages {
        ctx.add_with(
            "INLINK_BOILERPLATE_ONLY",
            Some(&url),
            details([("by_position", by_position)]),
            None,
            total,
            None,
        );
    }
}
