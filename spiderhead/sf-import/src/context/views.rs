//! Page/finding view accessors over the memory or disk stores.

use spiderhead_formats::models::{Group, Issue};
use std::collections::BTreeSet;

use super::disk::{dedupe_merge, DiskPages};
use super::{issue_sort_key, AuditContext, GroupsStore, IssuesStore, PageHandle, PagesStore};
use crate::normalize::norm_url;

impl AuditContext {
    // -- page views -----------------------------------------------------------

    /// `ctx._disk_pages.con`: the disk page store's connection, for the
    /// native-SQL helpers; `None` while pages live in memory (the reference
    /// attribute is unset there).
    pub fn disk_pages_con(&self) -> Option<&rusqlite::Connection> {
        match &self.pages {
            PagesStore::Disk(d) => Some(d.con()),
            PagesStore::Mem(_) => None,
        }
    }

    /// `len(ctx.pages)`.
    pub fn pages_len(&mut self) -> i64 {
        match &mut self.pages {
            PagesStore::Mem(v) => v.len() as i64,
            PagesStore::Disk(d) => d.len(),
        }
    }

    /// `pages[i]`: negative indexes wrap on the in-memory list like reference;
    /// the disk store takes the raw ordinal and misses surface as `None`
    /// (the reference `IndexError`).
    pub fn pages_index(&mut self, index: i64) -> Option<PageHandle> {
        match &mut self.pages {
            PagesStore::Mem(v) => {
                let len = v.len() as i64;
                let idx = if index < 0 { len + index } else { index };
                if idx < 0 || idx >= len {
                    None
                } else {
                    Some(PageHandle::Shared(v[idx as usize].clone()))
                }
            }
            PagesStore::Disk(d) => d.index(index).map(|p| PageHandle::Snapshot(Box::new(p))),
        }
    }

    /// `pages[a:b]` for step-1 slices; same wrap rules as `pages_index`.
    pub fn pages_slice(&mut self, start: i64, stop: i64) -> Vec<PageHandle> {
        match &mut self.pages {
            PagesStore::Mem(v) => {
                let len = v.len() as i64;
                let s = if start < 0 {
                    (len + start).max(0)
                } else {
                    start.min(len)
                };
                let e = if stop < 0 {
                    (len + stop).max(0)
                } else {
                    stop.min(len)
                };
                (s.max(0)..e.max(s.max(0)))
                    .filter_map(|i| v.get(i as usize))
                    .map(|rc| PageHandle::Shared(rc.clone()))
                    .collect()
            }
            PagesStore::Disk(d) => d
                .slice(start, stop)
                .into_iter()
                .map(|p| PageHandle::Snapshot(Box::new(p)))
                .collect(),
        }
    }

    /// `iter(ctx.pages)` in crawl order.
    pub fn pages_iter(&mut self) -> Vec<PageHandle> {
        match &mut self.pages {
            PagesStore::Mem(v) => v.iter().map(|rc| PageHandle::Shared(rc.clone())).collect(),
            PagesStore::Disk(d) => d
                .slice(0, d.len())
                .into_iter()
                .map(|p| PageHandle::Snapshot(Box::new(p)))
                .collect(),
        }
    }

    /// `page_by_url[url]` — `.get` semantics (`None` on a miss).
    pub fn page_by_url(&mut self, url: &str) -> Option<PageHandle> {
        match &mut self.pages {
            PagesStore::Mem(_) => self
                .page_by_url
                .get(url)
                .map(|rc| PageHandle::Shared(rc.clone())),
            PagesStore::Disk(d) => d.get(url).map(|p| PageHandle::Snapshot(Box::new(p))),
        }
    }

    /// `iter(page_by_url)` — crawled URLs in crawl order.
    pub fn iter_urls(&mut self) -> Vec<String> {
        match &mut self.pages {
            PagesStore::Mem(v) => v.iter().map(|rc| rc.borrow().url.clone()).collect(),
            PagesStore::Disk(d) => d.urls(),
        }
    }

    /// `pages_by_norm[norm]` — every page under the key (`[]` on a miss).
    pub fn pages_by_norm(&mut self, norm: &str) -> Vec<PageHandle> {
        match &mut self.pages {
            PagesStore::Mem(_) => self
                .pages_by_norm
                .get(norm)
                .map(|bucket| {
                    bucket
                        .iter()
                        .map(|rc| PageHandle::Shared(rc.clone()))
                        .collect()
                })
                .unwrap_or_default(),
            PagesStore::Disk(d) => d
                .by_norm(norm)
                .into_iter()
                .map(|p| PageHandle::Snapshot(Box::new(p)))
                .collect(),
        }
    }

    /// `page_by_norm[norm]` — the 2xx-preferring representative.
    pub fn page_by_norm(&mut self, norm: &str) -> Option<PageHandle> {
        match &mut self.pages {
            PagesStore::Mem(_) => self
                .page_by_norm
                .get(norm)
                .map(|rc| PageHandle::Shared(rc.clone())),
            PagesStore::Disk(d) => d
                .representative(norm)
                .map(|p| PageHandle::Snapshot(Box::new(p))),
        }
    }

    /// `iter(pages_by_norm)` — mem: reference dict insertion order; disk:
    /// `SELECT DISTINCT norm ORDER BY norm`.
    pub fn iter_norms(&mut self) -> Vec<String> {
        match &mut self.pages {
            PagesStore::Mem(_) => self.norms_order.clone(),
            PagesStore::Disk(d) => d.norms(),
        }
    }

    /// `len(pages_by_norm)` — distinct normalized keys.
    pub fn norm_count(&mut self) -> i64 {
        match &mut self.pages {
            PagesStore::Mem(_) => self.pages_by_norm.len() as i64,
            PagesStore::Disk(d) => d.norm_count(),
        }
    }

    /// `len(page_by_norm)` — the in-memory dict counts its keys (distinct
    /// norms); `_PageLookup.__len__` on the disk store returns `len(pages)`
    /// even in normalized mode.
    pub fn page_by_norm_len(&mut self) -> i64 {
        match &mut self.pages {
            PagesStore::Mem(_) => self.page_by_norm.len() as i64,
            PagesStore::Disk(d) => d.len(),
        }
    }

    /// `len(page_by_url)` — distinct crawled URLs (= page count).
    pub fn url_count(&mut self) -> i64 {
        match &mut self.pages {
            PagesStore::Mem(_) => self.page_by_url.len() as i64,
            PagesStore::Disk(d) => d.len(),
        }
    }

    /// `_StoredDict`/`_StoredList` write-through: apply `f` to the stored
    /// page; the disk store persists the full state blob.
    pub fn mutate_page(&mut self, url: &str, f: impl FnOnce(&mut super::Page)) -> bool {
        match &mut self.pages {
            PagesStore::Mem(_) => {
                if let Some(rc) = self.page_by_url.get(url) {
                    f(&mut rc.borrow_mut());
                    true
                } else {
                    false
                }
            }
            PagesStore::Disk(d) => d.mutate(url, f),
        }
    }

    /// `_DiskPages.attach_issue`/`Page.issue_ids.append`: record a finding id
    /// on the page (suppressed ids go to their own list).
    pub fn attach_issue(&mut self, url: &str, check: &str, issue_id: &str, suppressed: bool) {
        match &mut self.pages {
            PagesStore::Mem(_) => {
                if let Some(rc) = self.page_by_url.get(url) {
                    let mut p = rc.borrow_mut();
                    if suppressed {
                        p.suppressed_issue_ids.push(issue_id.to_owned());
                    } else {
                        if !p.issues.iter().any(|c| *c == *check) {
                            p.issues.push(check.to_owned());
                        }
                        p.issue_ids.push(issue_id.to_owned());
                    }
                }
            }
            PagesStore::Disk(d) => d.attach_issue(url, check, issue_id, suppressed),
        }
    }

    /// `html_page_keys`: unique normalized keys of fetched HTML pages —
    /// SQL-side on the disk store, `sorted` set on memory.
    pub fn html_page_keys(&mut self) -> Vec<String> {
        if let PagesStore::Disk(d) = &mut self.pages {
            return d.html_page_keys();
        }
        self.html_pages()
            .iter()
            .map(|h| h.map(|p| norm_url(Some(&p.url)).unwrap_or_default()))
            .collect::<BTreeSet<String>>()
            .into_iter()
            .collect()
    }

    /// `html_pages`: fetched, 2xx, HTML-by-Content-Type — a 301's redirect
    /// stub and a 404's error stub are routinely served with an HTML
    /// Content-Type too, so the status-code gate belongs here (issue #133).
    pub fn html_pages(&mut self) -> Vec<PageHandle> {
        if let Some(cached) = &self.html_pages_cache {
            return cached.clone();
        }
        let out: Vec<PageHandle> = self
            .pages_iter()
            .into_iter()
            .filter(|h| h.map(|p| p.is_html() && p.is_2xx()))
            .collect();
        if let PagesStore::Mem(_) = self.pages {
            self.html_pages_cache = Some(out.clone());
        }
        out
    }

    /// `indexable_html_pages`: the HTML population filtered by indexability.
    pub fn indexable_html_pages(&mut self) -> Vec<PageHandle> {
        if let Some(cached) = &self.indexable_html_cache {
            return cached.clone();
        }
        let out: Vec<PageHandle> = self
            .html_pages()
            .into_iter()
            .filter(|h| h.map(|p| p.is_indexable()))
            .collect();
        if let PagesStore::Mem(_) = self.pages {
            self.indexable_html_cache = Some(out.clone());
        }
        out
    }

    // -- finding views --------------------------------------------------------

    /// `iter(ctx.issues)` in append order.
    pub fn issues_iter(&self) -> Vec<Issue> {
        match &self.issues {
            IssuesStore::Mem(v) => v.clone(),
            IssuesStore::Disk(d) => d.iter(),
        }
    }

    /// `len(ctx.issues)`.
    pub fn issues_len(&self) -> i64 {
        match &self.issues {
            IssuesStore::Mem(v) => v.len() as i64,
            IssuesStore::Disk(d) => d.len(),
        }
    }

    /// `_DiskIssues.has_check` — whether any finding names the check id.
    pub fn issues_has_check(&self, check_id: &str) -> bool {
        match &self.issues {
            IssuesStore::Mem(v) => v.iter().any(|i| i.check == check_id),
            IssuesStore::Disk(d) => d.has_check(check_id),
        }
    }

    /// `_DiskIssues.iter_deduped_sorted`: severity → check → target order
    /// with consecutive same-key, target-bearing issues merged.
    pub fn issues_deduped_sorted(&self) -> Vec<Issue> {
        match &self.issues {
            IssuesStore::Mem(v) => {
                let mut sorted = v.clone();
                sorted.sort_by_key(issue_sort_key);
                dedupe_merge(sorted)
            }
            IssuesStore::Disk(d) => d.iter_deduped_sorted(),
        }
    }

    /// `iter(ctx.groups)` in append order.
    pub fn groups_iter(&self) -> Vec<Group> {
        match &self.groups {
            GroupsStore::Mem(v) => v.clone(),
            GroupsStore::Disk(d) => d.iter(),
        }
    }

    /// `len(ctx.groups)`.
    pub fn groups_len(&self) -> i64 {
        match &self.groups {
            GroupsStore::Mem(v) => v.len() as i64,
            GroupsStore::Disk(d) => d.len(),
        }
    }

    /// Test-facing access to the disk page store (cache metrics, path).
    pub fn disk_pages(&mut self) -> Option<&mut DiskPages> {
        match &mut self.pages {
            PagesStore::Disk(d) => Some(d),
            PagesStore::Mem(_) => None,
        }
    }

    /// `close()`: release the saved corpus, the page store, the issue stores,
    /// and the group store — temp sqlite files are unlinked.
    pub fn close(&mut self) {
        if let Some(mut corpus) = self.saved_corpus.take() {
            corpus.close();
        }
        if let PagesStore::Disk(d) = &mut self.pages {
            d.close();
        }
        if let IssuesStore::Disk(d) = &mut self.issues {
            d.close();
        }
        if let Some(d) = self.disk_final_issues.as_mut() {
            d.close();
        }
        self.disk_final_issues = None;
        if let GroupsStore::Disk(d) = &mut self.groups {
            d.close();
        }
    }
}
