//! Ordered frontier changes; callers supply the writer's active transaction.

use crate::discovery_ledger::{canonical_json, is_py_int};
use crate::error::StoreError;
use rusqlite::{Connection, OptionalExtension};
use serde_json::{Map, Value};
use sha2::{Digest, Sha256};

/// A frontier row, with the columns of the frontier table.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FrontierRow {
    /// Interned URL id.
    pub url_id: i64,
    /// Queue position.
    pub queue_ordinal: i64,
    /// Crawl depth.
    pub depth: i64,
    /// `queued`, `inflight`, `done` or `excluded`.
    pub state: String,
}

/// Parsed URL components; the scheme is lowercased.
pub(crate) struct Parts {
    pub(crate) scheme: String,
    pub(crate) netloc: String,
    pub(crate) path: String,
    pub(crate) query: String,
}

const USES_NETLOC: &[&str] = &[
    "", "ftp", "http", "gopher", "nntp", "telnet", "imap", "wais", "file", "mms", "https", "shttp",
    "snews", "prospero", "rtsp", "rtspu", "rsync", "svn", "svn+ssh", "sftp", "nfs", "git",
    "git+ssh", "ws", "wss",
];

fn fail(msg: &str) -> StoreError {
    StoreError::Invalid(msg.to_string())
}

/// URL parts without the fragment, which every caller discards.
pub(crate) fn urlsplit(url: &str) -> Parts {
    let (mut scheme, mut rest) = (String::new(), url);
    if let Some(i) = url.find(':') {
        let cand = &url[..i];
        let valid = cand.chars().next().is_some_and(|c| c.is_ascii_alphabetic())
            && cand
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || c == '+' || c == '-' || c == '.');
        if valid {
            scheme = cand.to_ascii_lowercase();
            rest = &url[i + 1..];
        }
    }
    let mut netloc = String::new();
    if let Some(stripped) = rest.strip_prefix("//") {
        let end = stripped.find(['/', '?', '#']).unwrap_or(stripped.len());
        netloc = stripped[..end].to_string();
        rest = &stripped[end..];
    }
    let rest = rest.split('#').next().unwrap_or("");
    let (path, query) = match rest.split_once('?') {
        Some((p, q)) => (p.to_string(), q.to_string()),
        None => (rest.to_string(), String::new()),
    };
    Parts {
        scheme,
        netloc,
        path,
        query,
    }
}

/// Rebuild a URL from scheme, netloc, path and query, with no fragment.
pub(crate) fn urlunsplit(scheme: &str, netloc: &str, path: &str, query: &str) -> String {
    let mut url = path.to_string();
    if !netloc.is_empty()
        || (!scheme.is_empty() && USES_NETLOC.contains(&scheme) && !url.starts_with("//"))
    {
        if !url.is_empty() && !url.starts_with('/') {
            url = format!("/{url}");
        }
        url = format!("//{netloc}{url}");
    }
    if !scheme.is_empty() {
        url = format!("{scheme}:{url}");
    }
    if !query.is_empty() {
        url = format!("{url}?{query}");
    }
    url
}

/// The URL with its fragment removed.
pub(crate) fn defrag(url: &str) -> &str {
    url.split('#').next().unwrap_or(url)
}

fn row_of(r: &rusqlite::Row<'_>) -> rusqlite::Result<FrontierRow> {
    Ok(FrontierRow {
        url_id: r.get(0)?,
        queue_ordinal: r.get(1)?,
        depth: r.get(2)?,
        state: r.get(3)?,
    })
}

const FRONTIER_COLS: &str = "f.url_id,f.queue_ordinal,f.depth,f.state";

fn exact_frontier(con: &Connection, url: &str) -> Result<Option<FrontierRow>, StoreError> {
    Ok(con
        .query_row(
            &format!(
                "SELECT {FRONTIER_COLS} FROM frontier f JOIN urls u USING(url_id) WHERE u.url=?1"
            ),
            [url],
            row_of,
        )
        .optional()?)
}

/// Find the fragment/empty-path identity without rewriting its request.
pub fn matching_frontier(
    con: &Connection,
    requested_url: &str,
) -> Result<Option<FrontierRow>, StoreError> {
    if let Some(found) = exact_frontier(con, requested_url)? {
        return Ok(Some(found));
    }
    let parts = urlsplit(requested_url);
    if !matches!(parts.scheme.as_str(), "http" | "https") {
        let key = urlunsplit(
            &parts.scheme,
            &parts.netloc,
            if parts.path.is_empty() {
                "/"
            } else {
                &parts.path
            },
            &parts.query,
        );
        return exact_frontier(con, &key);
    }
    // The key lowercases only the scheme, preserves netloc/path/query,
    // supplies '/' for an empty path, and drops the fragment. Scheme case has
    // at most 32 aliases; index probes avoid a table scan.
    let mut aliases: Vec<String> = Vec::new();
    let variants: Vec<Vec<char>> = parts
        .scheme
        .chars()
        .map(|c| {
            c.to_lowercase()
                .chain(c.to_uppercase())
                .collect::<Vec<char>>()
        })
        .collect();
    let mut combos: Vec<String> = vec![String::new()];
    for options in &variants {
        let mut next = Vec::new();
        for prefix in &combos {
            for ch in options {
                next.push(format!("{prefix}{ch}"));
            }
        }
        combos = next;
    }
    let paths: Vec<&str> = if parts.path.is_empty() || parts.path == "/" {
        vec!["", "/"]
    } else {
        vec![parts.path.as_str()]
    };
    for scheme in &combos {
        for path in &paths {
            aliases.push(urlunsplit(scheme, &parts.netloc, path, &parts.query));
        }
    }
    let mut clauses = Vec::new();
    let mut params: Vec<String> = Vec::new();
    for alias in &aliases {
        clauses.push("url=? OR (url>=? AND url<?)".to_string());
        params.push(alias.clone());
        params.push(format!("{alias}#"));
        params.push(format!("{alias}$"));
    }
    let sql = format!(
        "SELECT {FRONTIER_COLS} FROM frontier f WHERE f.url_id IN (SELECT url_id FROM urls WHERE {}) \
         ORDER BY f.queue_ordinal LIMIT 1",
        clauses.join(" OR ")
    );
    Ok(con
        .query_row(&sql, rusqlite::params_from_iter(params.iter()), row_of)
        .optional()?)
}

/// Intern `url` in `urls` and return its id.
fn url_id_of(con: &Connection, url: &str) -> Result<i64, StoreError> {
    if url.is_empty() {
        return Err(fail("a page/link URL must be a nonempty string"));
    }
    con.execute("INSERT OR IGNORE INTO urls(url) VALUES (?1)", [url])?;
    Ok(
        con.query_row("SELECT url_id FROM urls WHERE url = ?1", [url], |r| {
            r.get(0)
        })?,
    )
}

/// Append a frontier row at the next queue ordinal; returns the interned URL id.
pub fn frontier_row(
    con: &Connection,
    url: &str,
    depth: i64,
    state: &str,
) -> Result<i64, StoreError> {
    let url_id = url_id_of(con, url)?;
    let ordinal: i64 = con.query_row(
        "SELECT COALESCE(MAX(queue_ordinal)+1,0) FROM frontier",
        [],
        |r| r.get(0),
    )?;
    con.execute(
        "INSERT INTO frontier(url_id,queue_ordinal,depth,state) VALUES(?1,?2,?3,?4)",
        rusqlite::params![url_id, ordinal, depth, state],
    )?;
    Ok(url_id)
}

/// Record one decision row.
pub fn decision(
    con: &Connection,
    url: &str,
    reason: &str,
    source: &str,
    depth: Option<i64>,
    key: &str,
) -> Result<(), StoreError> {
    con.execute(
        "INSERT INTO decisions(url,reason,source,depth,occurrence_key) VALUES(?1,?2,?3,?4,?5)",
        rusqlite::params![url, reason, source, depth, key],
    )?;
    Ok(())
}

/// Reserve one query variant for `url` under `limit` per path. `None` or `Some(0)` is unlimited.
pub fn reserve_query(con: &Connection, url: &str, limit: Option<i64>) -> Result<bool, StoreError> {
    let Some(limit) = limit.filter(|l| *l != 0) else {
        return Ok(true);
    };
    let parts = urlsplit(url);
    let path = if parts.path.is_empty() {
        "/".to_string()
    } else {
        parts.path.clone()
    };
    let query = parts.query.clone();
    let seen: Option<i64> = con
        .query_row(
            "SELECT 1 FROM query_variants WHERE path_key=?1 AND query_key=?2",
            rusqlite::params![path, query],
            |r| r.get(0),
        )
        .optional()?;
    if seen.is_some() {
        return Ok(true);
    }
    let count: i64 = con.query_row(
        "SELECT COUNT(*) FROM query_variants WHERE path_key=?1",
        [&path],
        |r| r.get(0),
    )?;
    if count >= limit {
        return Ok(false);
    }
    con.execute(
        "INSERT INTO query_variants(path_key,query_key) VALUES(?1,?2)",
        rusqlite::params![path, query],
    )?;
    Ok(true)
}

fn int_field(item: &Map<String, Value>, key: &str) -> Option<i64> {
    item.get(key)
        .filter(|v| is_py_int(v))
        .and_then(Value::as_i64)
}

/// Apply document-order discoveries, reserving queries before the seen test.
pub fn apply_candidates(
    con: &Connection,
    candidates: &[Map<String, Value>],
    source: &str,
    queue_ordinal: i64,
    limit: Option<i64>,
) -> Result<(), StoreError> {
    const KEYS: [&str; 5] = [
        "path_key",
        "query_key",
        "requested_url",
        "frontier_url",
        "depth",
    ];
    for (index, item) in candidates.iter().enumerate() {
        if item.len() != KEYS.len() || KEYS.iter().any(|k| !item.contains_key(*k)) {
            return Err(fail("candidate input has unknown or missing fields"));
        }
        let url = item["frontier_url"].as_str().unwrap_or("");
        let requested = item["requested_url"].as_str().unwrap_or("");
        let depth = int_field(item, "depth");
        let (Some(depth), false, false) = (depth, url.is_empty(), requested.is_empty()) else {
            return Err(fail("candidate URL/depth is invalid"));
        };
        if depth < 0 {
            return Err(fail("candidate URL/depth is invalid"));
        }
        let parts = urlsplit(url);
        let path = if parts.path.is_empty() {
            "/"
        } else {
            parts.path.as_str()
        };
        if path != item["path_key"].as_str().unwrap_or("\u{0}")
            || parts.query != item["query_key"].as_str().unwrap_or("\u{0}")
            || url != defrag(requested)
        {
            return Err(fail("candidate query/request identity disagrees"));
        }
        if !reserve_query(con, url, limit)? {
            decision(
                con,
                requested,
                "query_variants_limit",
                source,
                Some(depth),
                &format!("{queue_ordinal}:candidate:{index}"),
            )?;
            if matching_frontier(con, url)?.is_none() {
                frontier_row(con, url, depth, "excluded")?;
            }
        } else if matching_frontier(con, url)?.is_none() {
            frontier_row(con, url, depth, "queued")?;
        }
    }
    Ok(())
}

/// Seed chunks mark accepted and rejected identities once, before retries.
/// Returns the `queued` / `excluded` / `already_seen` counters.
pub fn apply_seeds(
    con: &Connection,
    entries: &[Map<String, Value>],
    limit: Option<i64>,
    start_url: &str,
) -> Result<Map<String, Value>, StoreError> {
    const KEYS: [&str; 7] = [
        "requested_url",
        "frontier_url",
        "depth",
        "reason",
        "source",
        "reserve_query",
        "seed",
    ];
    let (mut queued, mut excluded, mut already_seen) = (0i64, 0i64, 0i64);
    for item in entries {
        if item.len() != KEYS.len() || KEYS.iter().any(|k| !item.contains_key(*k)) {
            return Err(fail("seed input has unknown or missing fields"));
        }
        let text = |k: &str| item[k].as_str();
        let (Some(url), Some(requested), Some(reason_in), Some(source)) = (
            text("frontier_url"),
            text("requested_url"),
            text("reason"),
            text("source"),
        ) else {
            return Err(fail("seed scalar fields are invalid"));
        };
        let reserve = item["reserve_query"].as_bool();
        let seed = item["seed"].as_bool();
        let depth_ok = int_field(item, "depth") == Some(0);
        let (Some(reserve), Some(seed)) = (reserve, seed) else {
            return Err(fail("seed scalar fields are invalid"));
        };
        if url.is_empty() || requested.is_empty() || source.is_empty() || !depth_ok {
            return Err(fail("seed scalar fields are invalid"));
        }
        if url != requested {
            return Err(fail(
                "seed request text must preserve the original legacy identity",
            ));
        }
        if !seed && (url != start_url || reserve || !reason_in.is_empty()) {
            return Err(fail("only the scan start bypasses sitemap seed checks"));
        }
        if matching_frontier(con, url)?.is_some() {
            already_seen += 1;
            continue;
        }
        let mut reason = reason_in.to_string();
        if reason.is_empty() && reserve && !reserve_query(con, url, limit)? {
            reason = "query_variants_limit".to_string();
        }
        let parts = urlsplit(url);
        let mut url = url.to_string();
        if !matches!(parts.scheme.as_str(), "http" | "https") {
            if reason.is_empty() {
                return Err(fail("an accepted seed must use HTTP(S)"));
            }
            // only the normalized key is seen.
            let path = if parts.path.is_empty() {
                "/"
            } else {
                parts.path.as_str()
            };
            url = urlunsplit(&parts.scheme, &parts.netloc, path, &parts.query);
        }
        let url_id = frontier_row(
            con,
            &url,
            0,
            if reason.is_empty() {
                "queued"
            } else {
                "excluded"
            },
        )?;
        if !reason.is_empty() {
            let key = hex(Sha256::digest(requested.as_bytes()).as_slice());
            decision(
                con,
                requested,
                &reason,
                source,
                Some(0),
                &format!("seed:{key}"),
            )?;
            excluded += 1;
        } else {
            queued += 1;
            if seed {
                let mut payload = Map::new();
                payload.insert("url_id".into(), Value::from(url_id));
                payload.insert("depth".into(), Value::from(0));
                payload.insert("source".into(), Value::from("sitemap"));
                con.execute(
                    "INSERT INTO context_items(kind,item_key,payload_version,payload_json,completeness,reason) \
                     VALUES(?1,?2,?3,?4,?5,?6)",
                    rusqlite::params![
                        "seed_url",
                        format!("url:{url_id}"),
                        "scan_context.v1",
                        canonical_json(&Value::Object(payload)),
                        "complete",
                        "accepted sitemap seed",
                    ],
                )?;
            }
        }
    }
    let mut counts = Map::new();
    counts.insert("queued".into(), Value::from(queued));
    counts.insert("excluded".into(), Value::from(excluded));
    counts.insert("already_seen".into(), Value::from(already_seen));
    Ok(counts)
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn db() -> Connection {
        let con = Connection::open_in_memory().unwrap();
        con.execute_batch(crate::schema_sql::SCAN_V1_SQL).unwrap();
        con
    }

    #[test]
    fn urlsplit_and_unsplit_round_trip_legacy_key() {
        let p = urlsplit("HTTP://Example.TEST/a?b=1#frag");
        assert_eq!(p.scheme, "http");
        assert_eq!(p.netloc, "Example.TEST");
        assert_eq!(
            urlunsplit(&p.scheme, &p.netloc, &p.path, &p.query),
            "http://Example.TEST/a?b=1"
        );
        assert_eq!(defrag("https://example.test/x#y"), "https://example.test/x");
    }

    #[test]
    fn query_variant_limit_reserves_per_path() {
        let con = db();
        assert!(reserve_query(&con, "https://example.test/a?one", Some(1)).unwrap());
        assert!(!reserve_query(&con, "https://example.test/a?two", Some(1)).unwrap());
        assert!(reserve_query(&con, "https://example.test/a?one", Some(1)).unwrap());
        assert!(reserve_query(&con, "https://example.test/b?x", None).unwrap());
    }

    #[test]
    fn apply_seeds_counts_queued_and_already_seen() {
        let con = db();
        let mut seed = Map::new();
        for (k, v) in [
            ("requested_url", json!("https://example.test/")),
            ("frontier_url", json!("https://example.test/")),
            ("depth", json!(0)),
            ("reason", json!("")),
            ("source", json!("start")),
            ("reserve_query", json!(false)),
            ("seed", json!(false)),
        ] {
            seed.insert(k.into(), v);
        }
        seed.insert("source".into(), json!("start"));
        let counts = apply_seeds(
            &con,
            std::slice::from_ref(&seed),
            None,
            "https://example.test/",
        )
        .unwrap();
        assert_eq!(counts["queued"], json!(1));
        let again = apply_seeds(&con, &[seed], None, "https://example.test/").unwrap();
        assert_eq!(again["already_seen"], json!(1));
    }

    #[test]
    fn seed_with_unknown_field_is_rejected() {
        let con = db();
        let mut seed = Map::new();
        seed.insert("requested_url".into(), json!("https://example.test/"));
        assert!(apply_seeds(&con, &[seed], None, "").is_err());
    }
}
