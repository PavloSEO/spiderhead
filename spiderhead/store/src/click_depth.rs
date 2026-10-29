//! Click depth: shortest internal link path from the scan's start page, derived at query time.
//!
//!
//! A lookup failure ("the scan cannot support the derivation") is reported as
//! [`StoreError::Invalid`] with the same message, so callers can say why instead of showing a
//! false NULL depth.

use crate::error::StoreError;
use rusqlite::{Connection, OptionalExtension};
use serde_json::Value;
use std::time::{Duration, Instant};

/// The scan cannot support the derivation.
fn lookup_error(message: impl Into<String>) -> StoreError {
    StoreError::Invalid(message.into())
}

/// `links.state` from the scan's capabilities JSON; `None` when absent or unreadable.
fn links_state(capabilities_json: &str) -> Option<String> {
    let caps: Value = serde_json::from_str(capabilities_json).ok()?;
    let links = caps.get("links")?;
    links.get("state")?.as_str().map(str::to_string)
}

/// Fail unless link observations are complete (or known empty) for this scan.
fn require_complete_links(capabilities_json: &str) -> Result<(), StoreError> {
    match links_state(capabilities_json) {
        Some(state) if state == "complete" || state == "empty" => Ok(()),
        state => Err(lookup_error(format!(
            "link observations are not complete for this scan ({})",
            state
                .filter(|s| !s.is_empty())
                .unwrap_or_else(|| "unknown".to_string())
        ))),
    }
}

/// The scan's start page id, or `LookupError` when the start page is not stored.
fn start_page_id(con: &Connection, start: Option<&str>) -> Result<i64, StoreError> {
    let root = match start.filter(|s| !s.is_empty()) {
        Some(start) => con
            .query_row(
                "SELECT u.url_id FROM urls u JOIN pages p ON p.url_id=u.url_id WHERE u.url=?1",
                [start],
                |row| row.get::<_, i64>(0),
            )
            .optional()?,
        None => None,
    };
    root.ok_or_else(|| lookup_error("start page is not stored in this scan"))
}

/// Create temp.cd_depth(url_id, depth) for the scan open on ``con``.
pub fn materialize(con: &Connection) -> Result<(), StoreError> {
    let (capabilities_json, start): (String, Option<String>) = con.query_row(
        "SELECT capabilities_json, start_url FROM scan WHERE singleton=1",
        [],
        |row| Ok((row.get(0)?, row.get(1)?)),
    )?;
    require_complete_links(&capabilities_json)?;
    let root = start_page_id(con, start.as_deref())?;
    // prepare_query_inner may have left a TEMP VIEW named cd_depth; DROP TABLE and DROP VIEW
    // both error on the other kind, so drop by the recorded object type.
    let kind: Option<String> = con
        .query_row(
            "SELECT type FROM sqlite_temp_master WHERE name='cd_depth'",
            [],
            |r| r.get(0),
        )
        .optional()?;
    match kind.as_deref() {
        Some("view") => con.execute_batch("DROP VIEW temp.cd_depth")?,
        Some(_) => con.execute_batch("DROP TABLE temp.cd_depth")?,
        None => {}
    }
    con.execute_batch(
        "DROP TABLE IF EXISTS temp.cd_edges;
         DROP TABLE IF EXISTS temp.cd_level;
         DROP TABLE IF EXISTS temp.cd_next;",
    )?;
    // One deduplicated edge copy with an index on source, so each BFS level is an index lookup.
    con.execute_batch(
        "CREATE TEMP TABLE cd_edges AS SELECT DISTINCT source_url_id AS s, destination_url_id AS d \
         FROM links;
         CREATE INDEX temp.cd_edges_s ON cd_edges (s);
         CREATE TEMP TABLE cd_depth (url_id INTEGER PRIMARY KEY, depth INTEGER NOT NULL);
         CREATE TEMP TABLE cd_level (url_id INTEGER PRIMARY KEY);
         CREATE TEMP TABLE cd_next (url_id INTEGER PRIMARY KEY);",
    )?;
    con.execute("INSERT INTO temp.cd_depth VALUES (?1, 0)", [root])?;
    con.execute("INSERT INTO temp.cd_level VALUES (?1)", [root])?;
    let mut depth: i64 = 0;
    loop {
        depth += 1;
        con.execute("DELETE FROM temp.cd_next", [])?;
        // Only crawled pages count as reachable; external destinations are not pages.
        con.execute(
            "INSERT OR IGNORE INTO temp.cd_next (url_id) \
             SELECT e.d FROM temp.cd_level c \
             JOIN temp.cd_edges e ON e.s=c.url_id \
             JOIN pages p ON p.url_id=e.d \
             WHERE NOT EXISTS (SELECT 1 FROM temp.cd_depth x WHERE x.url_id=e.d)",
            [],
        )?;
        let has_next = con
            .query_row("SELECT 1 FROM temp.cd_next LIMIT 1", [], |_| Ok(()))
            .optional()?
            .is_some();
        if !has_next {
            break;
        }
        con.execute(
            "INSERT INTO temp.cd_depth (url_id, depth) SELECT url_id, ?1 FROM temp.cd_next",
            [depth],
        )?;
        con.execute("DELETE FROM temp.cd_level", [])?;
        con.execute(
            "INSERT INTO temp.cd_level (url_id) SELECT url_id FROM temp.cd_next",
            [],
        )?;
    }
    con.execute_batch(
        "DROP TABLE temp.cd_level;
         DROP TABLE temp.cd_next;
         CREATE INDEX temp.cd_depth_depth ON cd_depth(depth,url_id);",
    )?;
    Ok(())
}

/// Expose the persisted depth index when present; otherwise derive it for old scans.
pub fn prepare_query(con: &Connection) -> Result<(), StoreError> {
    let prior: i64 = con.query_row("PRAGMA query_only", [], |r| r.get(0))?;
    con.execute_batch("PRAGMA query_only=OFF")?;
    let outcome = prepare_query_inner(con);
    let restored = con.execute_batch(if prior == 0 {
        "PRAGMA query_only=OFF"
    } else {
        "PRAGMA query_only=ON"
    });
    match (outcome, restored) {
        (Ok(()), Ok(())) => Ok(()),
        (Err(err), _) => Err(err),
        (Ok(()), Err(err)) => Err(err.into()),
    }
}

fn prepare_query_inner(con: &Connection) -> Result<(), StoreError> {
    let indexed = table_exists(con, "scan_click_depth")?;
    if !indexed {
        return materialize(con);
    }
    let (capabilities_json, start, lifecycle): (String, Option<String>, Option<String>) = con
        .query_row(
            "SELECT capabilities_json,start_url,lifecycle FROM scan WHERE singleton=1",
            [],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )?;
    require_complete_links(&capabilities_json)?;
    let root = start_page_id(con, start.as_deref())?;
    let ready = lifecycle.as_deref() == Some("finished")
        && con
            .query_row(
                "SELECT 1 FROM main.scan_click_depth WHERE url_id=?1",
                [root],
                |_| Ok(()),
            )
            .optional()?
            .is_some();
    if ready {
        con.execute_batch(
            "CREATE TEMP VIEW cd_depth AS SELECT url_id,depth FROM main.scan_click_depth",
        )?;
        Ok(())
    } else {
        materialize(con)
    }
}

/// True when a table of this name exists in the main database.
fn table_exists(con: &Connection, name: &str) -> Result<bool, StoreError> {
    let found = con
        .query_row(
            "SELECT 1 FROM main.sqlite_master WHERE type='table' AND name=?1",
            [name],
            |_| Ok(()),
        )
        .optional()?;
    Ok(found.is_some())
}

/// Persist an optional indexed projection in scan.v2 without changing frozen scan tables.
///
pub fn persist(con: &Connection, timeout_seconds: f64) -> Result<bool, StoreError> {
    let user_version: i64 = con.query_row("PRAGMA user_version", [], |row| row.get(0))?;
    if user_version != 2 {
        return Ok(false);
    }
    let deadline = Instant::now() + Duration::from_secs_f64(timeout_seconds.max(0.0));
    con.progress_handler(1000, Some(move || Instant::now() > deadline));
    let prior: i64 = con.query_row("PRAGMA query_only", [], |r| r.get(0))?;
    let outcome = persist_inner(con);
    con.progress_handler(0, None::<fn() -> bool>);
    // Drop the stale projection while writes are still allowed, then restore the caller's
    // query_only mode (it is not always ON).
    let cleanup = match &outcome {
        Err(StoreError::Invalid(_)) | Err(StoreError::Sqlite(_)) => {
            Some(con.execute_batch("DROP TABLE IF EXISTS main.scan_click_depth"))
        }
        _ => None,
    };
    let restored = con.execute_batch(if prior == 0 {
        "PRAGMA query_only=OFF"
    } else {
        "PRAGMA query_only=ON"
    });
    match outcome {
        Ok(()) => {
            restored?;
            Ok(true)
        }
        // This is a query optimization. A scan can still finish, and older URL-query reads can
        // derive depth from links with their own query budget when the projection is absent.
        Err(StoreError::Invalid(_)) | Err(StoreError::Sqlite(_)) => {
            cleanup.unwrap_or(Ok(()))?;
            restored?;
            Ok(false)
        }
        Err(other) => Err(other),
    }
}

fn persist_inner(con: &Connection) -> Result<(), StoreError> {
    con.execute_batch("PRAGMA query_only=OFF")?;
    if table_exists(con, "scan_click_depth")? {
        con.execute_batch("DROP TABLE scan_click_depth")?;
    }
    materialize(con)?;
    con.execute_batch(
        "CREATE TABLE scan_click_depth (url_id INTEGER PRIMARY KEY, depth INTEGER NOT NULL);
         INSERT INTO scan_click_depth(url_id,depth) SELECT url_id,depth FROM temp.cd_depth;
         DROP TABLE temp.cd_depth;
         CREATE INDEX scan_click_depth_depth ON scan_click_depth(depth,url_id);",
    )?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Minimal scan: a start page, one link target, complete link capabilities.
    fn scan() -> Connection {
        let con = Connection::open_in_memory().unwrap();
        con.execute_batch(
            "CREATE TABLE scan(singleton INTEGER PRIMARY KEY CHECK(singleton=1),
                               capabilities_json TEXT, start_url TEXT, lifecycle TEXT);
             INSERT INTO scan VALUES(1,'{\"links\":{\"state\":\"complete\"}}','https://a/','finished');
             PRAGMA user_version=2;
             CREATE TABLE urls(url_id INTEGER PRIMARY KEY, url TEXT);
             INSERT INTO urls VALUES(1,'https://a/'),(2,'https://a/b');
             CREATE TABLE pages(url_id INTEGER PRIMARY KEY, document_id INTEGER);
             INSERT INTO pages VALUES(1,10),(2,20);
             CREATE TABLE links(source_url_id INTEGER, destination_url_id INTEGER);
             INSERT INTO links VALUES(1,2);",
        )
        .unwrap();
        con
    }

    // regression: materialize() after the read-only TEMP VIEW exists must not fail on DROP TABLE
    #[test]
    fn materialize_replaces_the_temp_view() {
        let con = scan();
        con.execute_batch(
            "CREATE TABLE scan_click_depth (url_id INTEGER PRIMARY KEY, depth INTEGER NOT NULL);
             INSERT INTO scan_click_depth VALUES(1,0);",
        )
        .unwrap();
        // ready path: creates TEMP VIEW cd_depth over the persisted index
        prepare_query(&con).unwrap();
        // a later materialize/persist on the same connection must drop the view, not error
        materialize(&con).unwrap();
        persist(&con, 5.0).unwrap();
    }

    // regression: prepare_query/persist must restore the caller's query_only, not force ON
    #[test]
    fn query_only_mode_is_restored_to_the_caller_value() {
        let con = scan();
        let before: i64 = con
            .query_row("PRAGMA query_only", [], |r| r.get(0))
            .unwrap();
        assert_eq!(before, 0);
        prepare_query(&con).unwrap();
        let after: i64 = con
            .query_row("PRAGMA query_only", [], |r| r.get(0))
            .unwrap();
        assert_eq!(after, before, "connection must stay writable");
        persist(&con, 5.0).unwrap();
        let after_persist: i64 = con
            .query_row("PRAGMA query_only", [], |r| r.get(0))
            .unwrap();
        assert_eq!(after_persist, before);
        // and a read-only caller stays read-only
        con.execute_batch("PRAGMA query_only=ON").unwrap();
        prepare_query(&con).unwrap();
        let still_on: i64 = con
            .query_row("PRAGMA query_only", [], |r| r.get(0))
            .unwrap();
        assert_eq!(still_on, 1);
    }
}
