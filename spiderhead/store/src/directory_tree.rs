//! Directory tree: page counts per URL path segment, bounded for large sites (#1004, S2).
//!
//! A page is counted once at every directory above it, the root included. A path that does not
//! end in `/` names a file, so its last segment is not a directory. Query strings and fragments
//! are ignored; the caller passes URLs of one site.
//!
//! Bounds are applied when the tree is read out, not while it is built, so the build holds one
//! node per distinct directory. `max_depth` cuts the walk, `max_children` keeps the busiest
//! children per node and folds the rest into `other_pages`.

use std::collections::{BTreeMap, HashMap};
use std::path::Path;
use std::time::{Duration, Instant};

use rusqlite::{params, params_from_iter, Connection};
use serde_json::{json, Map, Value};

use crate::discovery_ledger::py_dumps;
use crate::error::StoreError;
use crate::ledger::urlparts::urlsplit;
use crate::scan_artifact::open_scan_mode;
use crate::url_query::is_interrupt;

/// One directory with the number of pages at or below it.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct DirNode {
    /// Segment name (`/` for the root).
    pub name: String,
    /// Pages counted at this directory.
    pub pages: i64,
    /// Child directories by segment.
    pub children: BTreeMap<String, DirNode>,
}

impl DirNode {
    /// A node with no pages and no children.
    pub fn new(name: &str) -> Self {
        DirNode {
            name: name.to_string(),
            pages: 0,
            children: BTreeMap::new(),
        }
    }

    /// Return this node as plain data, cut at `max_depth` and `max_children`.
    pub fn to_dict(&self, max_depth: i64, max_children: i64) -> Map<String, Value> {
        let mut ranked: Vec<&DirNode> = self.children.values().collect();
        ranked.sort_by(|a, b| b.pages.cmp(&a.pages).then_with(|| a.name.cmp(&b.name)));
        let keep = if max_depth <= 0 {
            0
        } else {
            (max_children.max(0) as usize).min(ranked.len())
        };
        let (kept, folded) = ranked.split_at(keep);
        let mut out = Map::new();
        out.insert("name".into(), json!(self.name));
        out.insert("pages".into(), json!(self.pages));
        out.insert(
            "children".into(),
            Value::Array(
                kept.iter()
                    .map(|c| Value::Object(c.to_dict(max_depth - 1, max_children)))
                    .collect(),
            ),
        );
        if !folded.is_empty() {
            out.insert("other_children".into(), json!(folded.len()));
            out.insert(
                "other_pages".into(),
                json!(folded.iter().map(|c| c.pages).sum::<i64>()),
            );
        }
        out
    }
}

/// Directory segments above the page named by `url` (the last segment is a file unless the path
/// ends in `/`).
pub fn directory_segments(url: &str) -> Vec<String> {
    let path = urlsplit(url).path;
    let mut parts: Vec<String> = path
        .split('/')
        .filter(|p| !p.is_empty())
        .map(str::to_string)
        .collect();
    if !parts.is_empty() && !path.ends_with('/') {
        parts.pop();
    }
    parts
}

/// Return the directory tree of `urls` as plain data, bounded by the given limits.
///
/// ponytail: plain tree, built in memory | ceiling: one node per distinct directory of the whole
/// site | upgrade: count per directory in SQL at query time when a 1M-URL scan exceeds the
/// memory budget (S6 of #1004)
pub fn build_directory_tree(
    urls: &[String],
    max_depth: i64,
    max_children: i64,
) -> Result<Map<String, Value>, StoreError> {
    if max_depth < 0 || max_children < 0 {
        return Err(StoreError::Invalid(
            "max_depth and max_children must be zero or more".into(),
        ));
    }
    let mut root = DirNode::new("/");
    for url in urls {
        root.pages += 1;
        let mut node = &mut root;
        for segment in directory_segments(url) {
            node = node
                .children
                .entry(segment.clone())
                .or_insert_with(|| DirNode::new(&segment));
            node.pages += 1;
        }
    }
    Ok(root.to_dict(max_depth, max_children))
}

fn netloc_fold(url: &str) -> String {
    caseless::default_case_fold_str(&urlsplit(url).netloc)
}

/// Read-out state shared by the recursive tree walk.
struct Walk<'a> {
    con: &'a Connection,
    max_depth: i64,
    max_children: i64,
    remaining: i64,
    remaining_bytes: i64,
    truncated: bool,
}

impl Walk<'_> {
    fn node(
        &mut self,
        path: &str,
        name: &str,
        pages: i64,
        depth: i64,
    ) -> Result<Value, rusqlite::Error> {
        self.remaining_bytes -= name.len() as i64 * 2 + 128;
        let child_count: i64 = self.con.query_row(
            "SELECT COUNT(*),COALESCE(SUM(pages),0) FROM temp.directory_counts WHERE parent=?",
            [path],
            |r| r.get(0),
        )?;
        let allowance = if depth >= self.max_depth {
            0
        } else {
            self.max_children.min(self.remaining)
        };
        let children: Vec<(String, String, i64)> = {
            let mut stmt = self.con.prepare(
                "SELECT path,name,pages FROM temp.directory_counts WHERE parent=? ORDER BY pages DESC,name ASC LIMIT ?",
            )?;
            let rows = stmt.query_map(params![path, allowance], |r| {
                Ok((r.get(0)?, r.get(1)?, r.get(2)?))
            })?;
            rows.collect::<Result<_, _>>()?
        };
        let mut kept = Vec::new();
        let mut kept_paths: Vec<String> = Vec::new();
        for (child_path, child_name, count) in children {
            let estimate = child_name.len() as i64 * 2 + 128;
            if self.remaining <= 0 || self.remaining_bytes < estimate {
                self.truncated = true;
                break;
            }
            self.remaining -= 1;
            kept.push(self.node(&child_path, &child_name, count, depth + 1)?);
            kept_paths.push(child_path);
        }
        let shown = kept.len() as i64;
        let mut output = Map::new();
        output.insert("name".into(), json!(name));
        output.insert("pages".into(), json!(pages));
        output.insert("children".into(), Value::Array(kept));
        if child_count > shown {
            let (count, sum): (i64, i64) = if kept_paths.is_empty() {
                self.con.query_row(
                    "SELECT COUNT(*),COALESCE(SUM(pages),0) FROM temp.directory_counts WHERE parent=?",
                    [path],
                    |r| Ok((r.get(0)?, r.get(1)?)),
                )?
            } else {
                let marks = vec!["?"; kept_paths.len()].join(",");
                let mut values: Vec<&str> = vec![path];
                values.extend(kept_paths.iter().map(String::as_str));
                self.con.query_row(
                    &format!("SELECT COUNT(*),COALESCE(SUM(pages),0) FROM temp.directory_counts WHERE parent=? AND path NOT IN ({marks})"),
                    params_from_iter(values),
                    |r| Ok((r.get(0)?, r.get(1)?)),
                )?
            };
            output.insert("other_children".into(), json!(count));
            output.insert("other_pages".into(), json!(sum));
            self.truncated = true;
        }
        Ok(Value::Object(output))
    }
}

fn flush(
    con: &Connection,
    pending: &mut HashMap<(String, String), i64>,
) -> Result<(), rusqlite::Error> {
    if pending.is_empty() {
        return Ok(());
    }
    let mut stmt = con.prepare_cached(
        "INSERT INTO temp.directory_counts(path,parent,name,pages) VALUES(?,?,?,?) ON CONFLICT(path) DO UPDATE SET pages=pages+excluded.pages",
    )?;
    for ((parent, name), count) in pending.drain() {
        let path = if parent.is_empty() {
            name.clone()
        } else {
            format!("{parent}/{name}")
        };
        stmt.execute(params![path, parent, name, count])?;
    }
    Ok(())
}

/// Build a bounded directory summary from stored URLs with SQLite-backed counts.
///
/// Unlike [`build_directory_tree`], this read does not retain one node per site path. Distinct
/// path counts live in a temporary table and only the requested output tree is read into
/// memory. It never fetches URLs or mutates the scan.
pub fn scan_directory_tree(
    scan_path: &Path,
    max_depth: i64,
    max_children: i64,
    max_nodes: i64,
    max_bytes: i64,
    timeout_seconds: f64,
) -> Result<Map<String, Value>, StoreError> {
    if !(0..=20).contains(&max_depth)
        || !(0..=500).contains(&max_children)
        || !(1..=10_000).contains(&max_nodes)
        || !(4096..=8 * 1024 * 1024).contains(&max_bytes)
        || !(0.1..=30.0).contains(&timeout_seconds)
    {
        return Err(StoreError::Invalid(
            "invalid directory-tree budget; depth 0..20, children 0..500, nodes 1..10000, bytes 4096..8388608, timeout 0.1..30s"
                .into(),
        ));
    }
    let (con, validation) = match open_scan_mode(scan_path, false, None, true) {
        Ok(opened) => opened,
        Err(e) => {
            let reason = match &e {
                StoreError::ScanError(s) => s.message.clone(),
                other => other.to_string(),
            };
            return Ok(
                json!({"state": "unavailable", "reason": reason, "tree": null})
                    .as_object()
                    .cloned()
                    .unwrap_or_default(),
            );
        }
    };
    let deadline = Instant::now() + Duration::from_secs_f64(timeout_seconds);
    con.progress_handler(10_000, Some(move || Instant::now() >= deadline));
    let outcome = read_tree(
        &con,
        validation,
        max_depth,
        max_children,
        max_nodes,
        max_bytes,
        deadline,
    );
    con.progress_handler(0, None::<fn() -> bool>);
    match outcome {
        Err(StoreError::Sqlite(e)) if is_interrupt(&e) => Ok(json!({
            "state": "limit_reached",
            "reason": "query_time_budget_exceeded",
            "validation": validation,
            "tree": null,
        })
        .as_object()
        .cloned()
        .unwrap_or_default()),
        other => other,
    }
}

fn read_tree(
    con: &Connection,
    validation: &str,
    max_depth: i64,
    max_children: i64,
    max_nodes: i64,
    max_bytes: i64,
    deadline: Instant,
) -> Result<Map<String, Value>, StoreError> {
    type Source = (String, i64, String, Option<String>, i64, Option<String>);
    let source: Option<Source> = {
        let mut stmt = con.prepare(
            "SELECT scan_uuid,evidence_revision,lifecycle,finish_reason,crawl_partial,start_url FROM scan WHERE singleton=1",
        )?;
        let mut rows = stmt.query([])?;
        match rows.next()? {
            Some(r) => Some((
                r.get(0)?,
                r.get(1)?,
                r.get(2)?,
                r.get(3)?,
                r.get(4)?,
                r.get(5)?,
            )),
            None => None,
        }
    };
    let Some(source) = source.filter(|s| s.5.as_deref().is_some_and(|u| !u.is_empty())) else {
        return Ok(json!({
            "state": "unavailable",
            "reason": "scan start URL is unavailable",
            "tree": null,
            "validation": validation,
        })
        .as_object()
        .cloned()
        .unwrap_or_default());
    };
    let origin = netloc_fold(source.5.as_deref().unwrap_or(""));
    con.execute_batch("PRAGMA query_only=OFF")?;
    con.execute_batch(
        "CREATE TEMP TABLE directory_counts(path TEXT PRIMARY KEY,parent TEXT NOT NULL,name TEXT NOT NULL,pages INTEGER NOT NULL) WITHOUT ROWID",
    )?;
    let total: i64 = con.query_row("SELECT COUNT(*) FROM pages", [], |r| r.get(0))?;
    let mut pending: HashMap<(String, String), i64> = HashMap::new();
    let (mut included, mut excluded_origin) = (0i64, 0i64);
    {
        let mut stmt = con.prepare(
            "SELECT u.url FROM pages p JOIN urls u USING(url_id) ORDER BY p.page_ordinal",
        )?;
        let mut rows = stmt.query([])?;
        while let Some(row) = rows.next()? {
            if Instant::now() >= deadline {
                return Err(rusqlite::Error::SqliteFailure(
                    rusqlite::ffi::Error::new(rusqlite::ffi::SQLITE_INTERRUPT),
                    Some("interrupted".into()),
                )
                .into());
            }
            let url: String = row.get(0)?;
            if netloc_fold(&url) != origin {
                excluded_origin += 1;
                continue;
            }
            included += 1;
            let mut parent = String::new();
            for name in directory_segments(&url) {
                *pending.entry((parent.clone(), name.clone())).or_insert(0) += 1;
                parent = if parent.is_empty() {
                    name
                } else {
                    format!("{parent}/{name}")
                };
                if pending.len() >= 1_000 {
                    flush(con, &mut pending)?;
                }
            }
        }
    }
    flush(con, &mut pending)?;
    con.execute_batch("CREATE INDEX temp.directory_counts_parent_rank ON directory_counts(parent,pages DESC,name ASC)")?;
    con.execute_batch("PRAGMA query_only=ON")?;
    let mut walk = Walk {
        con,
        max_depth,
        max_children,
        remaining: max_nodes - 1,
        remaining_bytes: max_bytes - 2_048,
        truncated: false,
    };
    let tree = walk.node("", "/", included, 0)?;
    let partial = walk.truncated || source.2 != "finished" || source.4 != 0;
    let source_block =
        json!({"scan_uuid": source.0, "evidence_revision": source.1, "lifecycle": source.2});
    let scope = json!({
        "origin": origin,
        "total_pages": total,
        "included_pages": included,
        "excluded_other_origins": excluded_origin,
    });
    let mut result = Map::new();
    result.insert(
        "state".into(),
        json!(if partial { "partial" } else { "available" }),
    );
    result.insert(
        "reason".into(),
        json!(if walk.truncated {
            "directory tree was bounded"
        } else {
            ""
        }),
    );
    result.insert("source".into(), source_block.clone());
    result.insert("validation".into(), json!(validation));
    result.insert("scope".into(), scope.clone());
    result.insert("max_depth".into(), json!(max_depth));
    result.insert("max_children".into(), json!(max_children));
    result.insert("max_nodes".into(), json!(max_nodes));
    result.insert("tree".into(), tree);
    if py_dumps(&Value::Object(result.clone()), false, false).len() as i64 > max_bytes {
        return Ok(json!({
            "state": "limit_reached",
            "reason": "output_byte_limit_exceeded",
            "source": source_block,
            "validation": validation,
            "scope": scope,
            "tree": null,
        })
        .as_object()
        .cloned()
        .unwrap_or_default());
    }
    Ok(result)
}
