//! `_DiskPages`: ordered, re-iterable native audit pages with mutable disk
//! state and a bounded LRU of decoded pages.

use rusqlite::{params, Connection};
use serde_json::Value;
use spiderhead_formats::models::Page;
use std::collections::{HashMap, VecDeque};
use std::path::PathBuf;

use super::{normalize_slice, page_from_state, page_state, temp_sqlite, unlink, DiskError};
use crate::normalize::{norm_url, Object};

/// disk state and a bounded LRU of decoded pages.
pub struct DiskPages {
    path: PathBuf,
    con: Connection,
    cache: HashMap<String, Page>,
    lru: VecDeque<String>,
    weights: HashMap<String, usize>,
    cache_bytes: usize,
    cache_limit_override: Option<usize>,
    cache_byte_limit_override: Option<usize>,
    closed: bool,
}

const CACHE_LIMIT: usize = 16_384;
const CACHE_BYTE_LIMIT: usize = 128 * 1024 * 1024;

impl DiskPages {
    /// Build the store from normalized records; `page_factory` produces the
    /// `Page` for one record. A record without a truthy `url` is skipped
    /// without consuming an ordinal; a duplicate url is `INSERT OR IGNORE`d
    /// but still consumes one.
    pub fn new<F>(
        records: impl IntoIterator<Item = Object>,
        page_factory: F,
    ) -> Result<Self, DiskError>
    where
        F: Fn(&Object) -> Result<Page, DiskError>,
    {
        let (path, con) = temp_sqlite("seohead-audit-pages-")?;
        con.execute_batch(
            "CREATE TABLE pages (ordinal INTEGER PRIMARY KEY, url TEXT UNIQUE NOT NULL, \
             norm TEXT NOT NULL, status_code INTEGER, state_json TEXT NOT NULL);\
             CREATE INDEX pages_norm ON pages(norm, ordinal)",
        )?;
        let mut ordinal: i64 = 0;
        for record in records {
            let url = record.get("url").and_then(Value::as_str);
            let Some(url) = url.filter(|u| !u.is_empty()) else {
                continue;
            };
            let page = page_factory(&record)?;
            let state = page_state(&page);
            con.execute(
                "INSERT OR IGNORE INTO pages VALUES (?,?,?,?,?)",
                params![
                    ordinal,
                    url,
                    norm_url(Some(url))?,
                    page.status_code,
                    serde_json::to_string(&state)?,
                ],
            )?;
            ordinal += 1;
        }
        Ok(Self {
            path,
            con,
            cache: HashMap::new(),
            lru: VecDeque::new(),
            weights: HashMap::new(),
            cache_bytes: 0,
            cache_limit_override: None,
            cache_byte_limit_override: None,
            closed: false,
        })
    }

    /// Raw connection — the same "one process-local transaction" contract.
    pub fn con(&self) -> &Connection {
        &self.con
    }

    /// The backing temp-file path (unlinked by `close`/`Drop`).
    pub fn path(&self) -> &PathBuf {
        &self.path
    }

    /// Current cache occupancy (the reference `len(store._cache)`).
    pub fn cache_len(&self) -> usize {
        self.cache.len()
    }

    /// Current estimated cache weight in bytes.
    pub fn cache_bytes(&self) -> usize {
        self.cache_bytes
    }

    /// The summed per-entry weights (reference `_cache_weights` bookkeeping).
    pub fn cache_weight_sum(&self) -> usize {
        self.weights.values().sum()
    }

    /// Override the entry-count bound (tests shrink it to force eviction).
    pub fn set_cache_limit(&mut self, limit: usize) {
        self.cache_limit_override = Some(limit);
    }

    /// Override the byte bound (tests shrink it to force eviction).
    pub fn set_cache_byte_limit(&mut self, limit: usize) {
        self.cache_byte_limit_override = Some(limit);
    }

    /// `len(pages)` — unique page rows.
    pub fn len(&self) -> i64 {
        self.con
            .query_row("SELECT COUNT(*) FROM pages", [], |r| r.get(0))
            .unwrap_or(0)
    }

    /// Whether the store is empty.
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    fn drop_cached(&mut self, url: &str) {
        if self.cache.remove(url).is_some() {
            if let Some(w) = self.weights.remove(url) {
                self.cache_bytes = self.cache_bytes.saturating_sub(w);
            }
            self.lru.retain(|u| u != url);
        }
    }

    fn cache_weight(page: &Page) -> usize {
        256 + serde_json::to_string(&page_state(page))
            .map(|s| s.len())
            .unwrap_or(0)
    }

    fn cache_insert(&mut self, url: &str, page: Page) {
        let byte_limit = self.cache_byte_limit_override.unwrap_or(CACHE_BYTE_LIMIT);
        let count_limit = self.cache_limit_override.unwrap_or(CACHE_LIMIT);
        let weight = Self::cache_weight(&page);
        if weight > byte_limit {
            return;
        }
        self.drop_cached(url);
        self.cache.insert(url.to_owned(), page);
        self.weights.insert(url.to_owned(), weight);
        self.cache_bytes += weight;
        self.lru.push_back(url.to_owned());
        while self.cache.len() > count_limit || self.cache_bytes > byte_limit {
            if let Some(front) = self.lru.pop_front() {
                self.drop_cached(&front);
            } else {
                break;
            }
        }
    }

    /// `pages[url]`: the page for an exact crawled URL, decoded from state.
    /// Returns a snapshot — persist mutations via [`DiskPages::mutate`].
    pub fn get(&mut self, url: &str) -> Option<Page> {
        if self.cache.contains_key(url) {
            self.lru.retain(|u| u != url);
            self.lru.push_back(url.to_owned());
            return self.cache.get(url).cloned();
        }
        let row = self
            .con
            .query_row(
                "SELECT url,status_code,state_json FROM pages WHERE url=?",
                params![url],
                |r| {
                    Ok((
                        r.get::<_, String>(0)?,
                        r.get::<_, Option<i64>>(1)?,
                        r.get::<_, String>(2)?,
                    ))
                },
            )
            .ok()?;
        let state: Object = serde_json::from_str(&row.2).ok()?;
        let page = page_from_state(&row.0, row.1, &state);
        self.cache_insert(url, page.clone());
        Some(page)
    }

    /// `_write`/`_StoredDict`/`_StoredList`: apply `f` to the stored page and
    /// persist the full state blob, exactly like the reference write-through.
    pub fn mutate(&mut self, url: &str, f: impl FnOnce(&mut Page)) -> bool {
        let Some(mut page) = self.get(url) else {
            return false;
        };
        f(&mut page);
        let state = page_state(&page);
        let Ok(text) = serde_json::to_string(&state) else {
            return false;
        };
        let ok = self
            .con
            .execute(
                "UPDATE pages SET state_json=? WHERE url=?",
                params![text, url],
            )
            .is_ok();
        if ok {
            self.drop_cached(url);
        }
        ok
    }

    /// `attach_issue`: record one final finding id against the page (or a
    /// suppressed id), one read/write like the reference method.
    pub fn attach_issue(&mut self, url: &str, check: &str, issue_id: &str, suppressed: bool) {
        let check = check.to_owned();
        let id = issue_id.to_owned();
        self.mutate(url, |p| {
            if suppressed {
                p.suppressed_issue_ids.push(id);
            } else {
                if !p.issues.contains(&check) {
                    p.issues.push(check);
                }
                p.issue_ids.push(id);
            }
        });
    }

    /// `by_norm(norm)`: every page under a normalized key, in crawl order.
    pub fn by_norm(&mut self, norm: &str) -> Vec<Page> {
        let urls: Vec<String> = {
            let mut stmt = match self
                .con
                .prepare("SELECT url FROM pages WHERE norm=? ORDER BY ordinal")
            {
                Ok(s) => s,
                Err(_) => return Vec::new(),
            };
            stmt.query_map(params![norm], |r| r.get(0))
                .map(|rows| rows.filter_map(Result::ok).collect())
                .unwrap_or_default()
        };
        urls.into_iter().filter_map(|u| self.get(&u)).collect()
    }

    /// `representative(norm)`: the first 2xx page under the key, else the
    /// first page — the SQL-side equivalent of `_representative`.
    pub fn representative(&mut self, norm: &str) -> Option<Page> {
        let url: String = self
            .con
            .query_row(
                "SELECT url FROM pages WHERE norm=? \
                 ORDER BY CASE WHEN status_code BETWEEN 200 AND 299 THEN 0 ELSE 1 END, \
                 ordinal LIMIT 1",
                params![norm],
                |r| r.get(0),
            )
            .ok()?;
        self.get(&url)
    }

    /// `pages[i]` — the row at the raw ordinal; out-of-range and negative
    /// indexes find no row (the reference `IndexError` surfaces as `None`).
    pub fn index(&mut self, index: i64) -> Option<Page> {
        let url: String = self
            .con
            .query_row(
                "SELECT url FROM pages WHERE ordinal=?",
                params![index],
                |r| r.get(0),
            )
            .ok()?;
        self.get(&url)
    }

    /// `pages[start:stop]` with `step == 1`.
    pub fn slice(&mut self, start: i64, stop: i64) -> Vec<Page> {
        let len = self.len();
        let (s, e) = normalize_slice(start, stop, len);
        let urls: Vec<String> = {
            let mut stmt = match self
                .con
                .prepare("SELECT url FROM pages WHERE ordinal>=? AND ordinal<? ORDER BY ordinal")
            {
                Ok(s) => s,
                Err(_) => return Vec::new(),
            };
            stmt.query_map(params![s, e], |r| r.get(0))
                .map(|rows| rows.filter_map(Result::ok).collect())
                .unwrap_or_default()
        };
        urls.into_iter().filter_map(|u| self.get(&u)).collect()
    }

    /// Iterate all pages in crawl order (re-iterable).
    pub fn urls(&self) -> Vec<String> {
        let mut stmt = match self.con.prepare("SELECT url FROM pages ORDER BY ordinal") {
            Ok(s) => s,
            Err(_) => return Vec::new(),
        };
        stmt.query_map([], |r| r.get(0))
            .map(|rows| rows.filter_map(Result::ok).collect())
            .unwrap_or_default()
    }

    /// `DISTINCT norm` keys in lexical order (the `_PageBuckets`/`_PageLookup`
    /// normalized iteration order).
    pub fn norms(&self) -> Vec<String> {
        let mut stmt = match self
            .con
            .prepare("SELECT DISTINCT norm FROM pages ORDER BY norm")
        {
            Ok(s) => s,
            Err(_) => return Vec::new(),
        };
        stmt.query_map([], |r| r.get(0))
            .map(|rows| rows.filter_map(Result::ok).collect())
            .unwrap_or_default()
    }

    /// Distinct-norm count (`len(_PageBuckets)`).
    pub fn norm_count(&self) -> i64 {
        self.con
            .query_row("SELECT COUNT(DISTINCT norm) FROM pages", [], |r| r.get(0))
            .unwrap_or(0)
    }

    /// `html_page_keys` disk path: distinct normalized keys of fetched
    /// 2xx HTML pages, lexical order.
    pub fn html_page_keys(&self) -> Vec<String> {
        let mut stmt = match self.con.prepare(
            "SELECT DISTINCT norm FROM pages WHERE status_code BETWEEN 200 AND 299 \
             AND lower(json_extract(state_json, '$.content_type')) LIKE '%html%' ORDER BY norm",
        ) {
            Ok(s) => s,
            Err(_) => return Vec::new(),
        };
        stmt.query_map([], |r| r.get(0))
            .map(|rows| rows.filter_map(Result::ok).collect())
            .unwrap_or_default()
    }

    /// Close the connection and unlink the temp file.
    pub fn close(&mut self) {
        if self.closed {
            return;
        }
        self.closed = true;
        self.cache.clear();
        self.weights.clear();
        self.lru.clear();
        self.cache_bytes = 0;
        unlink(&self.path);
    }
}

impl Drop for DiskPages {
    fn drop(&mut self) {
        self.close();
    }
}
