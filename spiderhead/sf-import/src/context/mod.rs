//! The shared audit context passed to every check.
//!
//! `_PageLookup`/`_PageBuckets`/`_PageFilter` views. It builds the normalized
//! `Page` list from `internal_all` and exposes config-aware helpers:
//! `enabled`, `severity`, `add`, `skip`.

mod findings;
mod views;

pub mod disk;

use serde_json::Value;
use spiderhead_core::graph::GraphAccess;
use spiderhead_formats::check_registry::{self, CheckRegistry};
use spiderhead_formats::models::{Group, Issue, Page, SkippedCheck};
use std::cell::{Ref, RefCell, RefMut};
use std::collections::{HashMap, HashSet};
use std::rc::Rc;
use thiserror::Error;

use self::disk::{DiskGroups, DiskIssueResults, DiskIssues, DiskPages};
use crate::loader::LoadedExports;
use crate::normalize::{iter_records_from_df, norm_url, Object, INTERNAL_FIELD_MAP};
use crate::table::Table;

/// Context construction failure surface.
#[derive(Debug, Error)]
pub enum ContextError {
    /// The embedded check registry failed to load.
    #[error("check registry unavailable")]
    Registry,
    /// sqlite-backed store failure.
    #[error(transparent)]
    Disk(#[from] disk::DiskError),
    /// URL normalization failure (`_norm_url` raises the same way).
    #[error("url normalize failed: {0}")]
    Norm(#[from] spiderhead_core::url_normalize::NormUrlError),
    /// A caller-input problem the oracle raises `ValueError` for.
    #[error("{0}")]
    Value(String),
}

/// `_representative`: the page a consumer means when it resolves a normalized
/// URL — the first 2xx, else the first page under the key.
fn representative(pages: &[Rc<RefCell<Page>>]) -> Option<Rc<RefCell<Page>>> {
    for page in pages {
        let p = page.borrow();
        if let Some(code) = p.status_code {
            if (200..300).contains(&code) {
                return Some(page.clone());
            }
        }
    }
    pages.first().cloned()
}

/// One page as handed out by the context: a shared handle in the in-memory
/// store (mutations through `borrow_mut` are visible everywhere, like reference
/// object identity) or a decoded snapshot from the disk store (persist
/// mutations through `AuditContext::mutate_page`).
#[derive(Debug, Clone)]
pub enum PageHandle {
    /// Shared, in-memory page.
    Shared(Rc<RefCell<Page>>),
    /// Decoded disk-backed snapshot.
    Snapshot(Box<Page>),
}

impl PageHandle {
    /// Read access: `f` runs against the `Page` either way.
    pub fn map<T>(&self, f: impl FnOnce(&Page) -> T) -> T {
        match self {
            Self::Shared(rc) => f(&rc.borrow()),
            Self::Snapshot(p) => f(p),
        }
    }

    /// Mutable access. For `Shared` this mutates the single underlying page;
    /// for `Snapshot` only this copy changes — disk persistence needs
    /// `AuditContext::mutate_page`.
    pub fn mutate(&mut self, f: impl FnOnce(&mut Page)) {
        match self {
            Self::Shared(rc) => f(&mut rc.borrow_mut()),
            Self::Snapshot(p) => f(p),
        }
    }

    /// Read borrow for `Shared` handles.
    pub fn borrow(&self) -> Option<Ref<'_, Page>> {
        match self {
            Self::Shared(rc) => Some(rc.borrow()),
            Self::Snapshot(_) => None,
        }
    }

    /// Mutable borrow for `Shared` handles.
    pub fn borrow_mut(&mut self) -> Option<RefMut<'_, Page>> {
        match self {
            Self::Shared(rc) => Some(rc.borrow_mut()),
            Self::Snapshot(_) => None,
        }
    }

    /// The page's URL.
    pub fn url(&self) -> String {
        self.map(|p| p.url.clone())
    }

    /// A detached copy of the page.
    pub fn to_page(&self) -> Page {
        self.map(|p| p.clone())
    }
}

/// The page collection: in-memory list or sqlite-backed disk store.
enum PagesStore {
    /// `self.pages` as a reference list — `Rc` handles give the shared identity
    /// reference objects get for free.
    Mem(Vec<Rc<RefCell<Page>>>),
    /// `_DiskPages`.
    Disk(Box<DiskPages>),
}

/// The pre-aggregation findings collection.
enum IssuesStore {
    /// `self.issues` as a reference list.
    Mem(Vec<Issue>),
    /// `_DiskIssues`.
    Disk(Box<DiskIssues>),
}

/// The groups collection.
enum GroupsStore {
    /// `self.groups` as a reference list.
    Mem(Vec<Group>),
    /// `_DiskGroups`.
    Disk(Box<DiskGroups>),
}

/// Extra `Issue` fields `add` accepts — the `**kw` of the reference signature.
#[derive(Debug, Default)]
pub struct AddOpts {
    /// Stable document id.
    pub id: Option<String>,
    /// Stable hash for diffing runs.
    pub fingerprint: Option<String>,
    /// The URL the issue is about.
    pub target_url: Option<String>,
    /// HTTP status of the target.
    pub status_code: Option<i64>,
    /// Occurrences this issue stands for (defaults to the model's `1`).
    pub occurrences_count: Option<i64>,
    /// One object per location.
    pub locations: Vec<Object>,
    /// Check-specific details.
    pub details: Object,
    /// Group the issue belongs to.
    pub group_id: Option<String>,
    /// Evidence the verdict rests on.
    pub evidence: Object,
    /// Source override (defaults to the registry `source`).
    pub source: Option<String>,
    /// Message override (defaults to the registry `message`).
    pub message: Option<String>,
    /// Fix-hint override: `Some(v)` wins over the registry `fix`
    /// (`Some(None)` clears it); `None` keeps the registry default.
    pub fix_hint: Option<Option<String>>,
}

/// Whatever `ctx.saved_corpus` may hold: anything closeable.
pub trait SavedCorpus {
    /// `close()` — free the held resource.
    fn close(&mut self);

    /// `owner.self_codes(target)`: the hreflang codes the page at this
    /// normalized URL declares for itself, from the retained derivations. An
    /// empty set reads as "no self-statement", which is falsy in reference and
    /// must stay falsy here.
    fn self_codes(&self, _target: &str) -> std::collections::BTreeSet<String> {
        Default::default()
    }
}

/// The shared audit context passed to every check.
pub struct AuditContext {
    /// The exports the audit runs on.
    pub exports: LoadedExports,
    /// The run configuration object.
    pub config: Value,
    /// Cursor-backed graph reader for native scans (export audits: `None`).
    pub graph_access: Option<Box<dyn GraphAccess>>,
    /// Document-bound hreflang relations for native scans.
    pub native_hreflang: Option<Value>,
    /// Stored probes of canonical targets no crawled page answered (#987).
    pub target_probes: Object,
    /// Where the crawl actually started, when the producer knows.
    pub start_url: Option<String>,
    /// The internal link graph's shape (`inlinks` fills it).
    pub internal_linking: Object,
    /// The objective trust-evidence block (#823, `eeat` fills it).
    pub trust_evidence: Object,
    /// Per-asset measurements recorded by checks.
    pub asset_measurements: Object,
    /// `config["thresholds"]`.
    pub thresholds: Value,
    /// `config["requirements"]`.
    pub requirements: Value,
    /// Checks that did not run, in order.
    pub skipped: Vec<SkippedCheck>,
    /// The `internal_all` frame, when present.
    pub internal_df: Option<Table>,
    /// `crawled URL -> redirect URL` for redirect rows (insertion-ordered
    /// like the reference dict — `redirect_chains` iterates it).
    pub redirect_map: indexmap::IndexMap<String, String>,
    /// Final active/suppressed findings for the disk pipeline (native path).
    pub disk_final_issues: Option<DiskIssueResults>,
    /// The corpus a producer may attach for `close()` to release.
    pub saved_corpus: Option<Box<dyn SavedCorpus>>,
    /// Native stored-scan connection (audit-v2 handler sets it).
    pub scan_con: Option<rusqlite::Connection>,
    /// Embedding adapter selected by a core caller.
    pub semantic_adapter: Option<Value>,
    /// Adapter-side vector cache.
    pub semantic_cache: Option<Value>,
    /// Lexical evidence collected for semantic checks.
    pub semantic_lexical_evidence: Object,
    /// Semantic evidence collected for the summary.
    pub semantic_evidence: Object,

    registry: &'static CheckRegistry,
    pages: PagesStore,
    issues: IssuesStore,
    groups: GroupsStore,
    page_by_url: HashMap<String, Rc<RefCell<Page>>>,
    pages_by_norm: HashMap<String, Vec<Rc<RefCell<Page>>>>,
    page_by_norm: HashMap<String, Rc<RefCell<Page>>>,
    /// First-appearance order of norms — reference dict iteration order for the
    /// in-memory `pages_by_norm`/`page_by_norm` mappings.
    norms_order: Vec<String>,
    skipped_ids: HashSet<String>,
    fired_ids: HashSet<String>,
    group_seq: i64,
    html_pages_cache: Option<Vec<PageHandle>>,
    indexable_html_cache: Option<Vec<PageHandle>>,
}

impl AuditContext {
    /// `AuditContext(exports, config, graph_access=None, disk_backed_pages=False)`.
    ///
    /// `config` is the run configuration object; `graph_access` is the
    /// cursor-backed graph reader native scans supply (export audits pass
    /// `None`). `disk_backed_pages` swaps every collection to its sqlite
    /// store.
    pub fn new(
        exports: LoadedExports,
        config: Value,
        graph_access: Option<Box<dyn GraphAccess>>,
        disk_backed_pages: bool,
    ) -> Result<Self, ContextError> {
        let registry = check_registry::builtin().map_err(|_| ContextError::Registry)?;
        let internal_df = exports.get("internal_all").cloned();
        let empty = || Value::Object(Object::new());
        let thresholds = config.get("thresholds").cloned().unwrap_or_else(empty);
        let requirements = config.get("requirements").cloned().unwrap_or_else(empty);
        let mut ctx = Self {
            exports,
            config,
            graph_access,
            native_hreflang: None,
            target_probes: Object::new(),
            start_url: None,
            internal_linking: Object::new(),
            trust_evidence: Object::new(),
            asset_measurements: Object::new(),
            thresholds,
            requirements,
            skipped: Vec::new(),
            internal_df,
            redirect_map: indexmap::IndexMap::new(),
            disk_final_issues: None,
            saved_corpus: None,
            scan_con: None,
            semantic_adapter: None,
            semantic_cache: None,
            semantic_lexical_evidence: Object::new(),
            semantic_evidence: Object::new(),
            registry,
            pages: PagesStore::Mem(Vec::new()),
            issues: if disk_backed_pages {
                IssuesStore::Disk(Box::new(DiskIssues::new()?))
            } else {
                IssuesStore::Mem(Vec::new())
            },
            groups: if disk_backed_pages {
                GroupsStore::Disk(Box::new(DiskGroups::new()?))
            } else {
                GroupsStore::Mem(Vec::new())
            },
            page_by_url: HashMap::new(),
            pages_by_norm: HashMap::new(),
            page_by_norm: HashMap::new(),
            norms_order: Vec::new(),
            skipped_ids: HashSet::new(),
            fired_ids: HashSet::new(),
            group_seq: 0,
            html_pages_cache: None,
            indexable_html_cache: None,
        };
        ctx.build_pages(disk_backed_pages)?;
        Ok(ctx)
    }

    // -- page construction --------------------------------------------------

    /// `_build_pages`: dedupe records by URL and populate the page stores,
    /// the normalized lookups, and `redirect_map`.
    ///
    /// `norm_url` is deliberately many-to-one: it folds a trailing slash away
    /// so a canonical written without one still matches the page that has it.
    /// A crawl can therefore hold two pages under one key — `pages_by_norm`
    /// keeps them all, `page_by_norm` is the 2xx-preferring representative.
    fn build_pages(&mut self, disk_backed_pages: bool) -> Result<(), ContextError> {
        let Some(df) = &self.internal_df else {
            return Ok(());
        };
        let records: Vec<Object> = iter_records_from_df(df, INTERNAL_FIELD_MAP).collect();
        if disk_backed_pages {
            self.pages = PagesStore::Disk(Box::new(DiskPages::new(records, |rec| {
                Ok(Self::page_from_record(rec))
            })?));
            return Ok(());
        }
        let mut pages = Vec::new();
        for rec in records {
            let Some(url) = rec
                .get("url")
                .and_then(Value::as_str)
                .filter(|u| !u.is_empty())
            else {
                continue;
            };
            let url = url.to_owned();
            if self.page_by_url.contains_key(&url) {
                // Duplicate row for the same URL (e.g. merged exports) — keep
                // the first; otherwise pages and page_by_url would disagree.
                continue;
            }
            let page = Rc::new(RefCell::new(Self::page_from_record(&rec)));
            let norm = norm_url(Some(&url))?;
            if !self.pages_by_norm.contains_key(&norm) {
                self.norms_order.push(norm.clone());
            }
            self.pages_by_norm
                .entry(norm.clone())
                .or_default()
                .push(page.clone());
            let rep = self
                .pages_by_norm
                .get(&norm)
                .and_then(|bucket| representative(bucket));
            if let Some(rep) = rep {
                self.page_by_norm.insert(norm, rep);
            }
            if let Some(v) = rec
                .get("redirect_url")
                .filter(|v| crate::pyvalue::truthy(v))
            {
                self.redirect_map
                    .insert(url.clone(), crate::pyvalue::str_or_dumps(v));
            }
            self.page_by_url.insert(url, page.clone());
            pages.push(page);
        }
        self.pages = PagesStore::Mem(pages);
        Ok(())
    }

    /// `_page_from_record`: the stored `Page` plus the raw record kept under
    /// `metrics["_record"]`.
    fn page_from_record(rec: &Object) -> Page {
        let mut page = Page {
            url: rec
                .get("url")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_owned(),
            status_code: rec.get("status_code").and_then(Value::as_i64),
            status: rec.get("status").and_then(Value::as_str).map(str::to_owned),
            content_type: rec
                .get("content_type")
                .and_then(Value::as_str)
                .map(str::to_owned),
            indexability: rec
                .get("indexability")
                .and_then(Value::as_str)
                .map(str::to_owned),
            indexability_status: rec
                .get("indexability_status")
                .and_then(Value::as_str)
                .map(str::to_owned),
            metrics: Self::metrics_from_record(rec),
            ..Page::default()
        };
        page.metrics
            .insert("_record".into(), Value::Object(rec.clone()));
        page
    }

    /// `_metrics_from_record`: the public per-page metrics block; derived
    /// statistics are added later.
    fn metrics_from_record(rec: &Object) -> Object {
        let get = |k: &str| rec.get(k).cloned().unwrap_or(Value::Null);
        let h1_count = i64::from(crate::pyvalue::truthy_opt(rec.get("h1")))
            + i64::from(crate::pyvalue::truthy_opt(rec.get("h1_2")));
        let h2_count = i64::from(crate::pyvalue::truthy_opt(rec.get("h2")))
            + i64::from(crate::pyvalue::truthy_opt(rec.get("h2_2")));
        let h1_list: Vec<Value> = [rec.get("h1"), rec.get("h1_2")]
            .into_iter()
            .flatten()
            .filter(|v| crate::pyvalue::truthy(v))
            .cloned()
            .collect();
        let representation = rec
            .get("representation")
            .filter(|v| crate::pyvalue::truthy(v))
            .cloned()
            .unwrap_or_else(|| Value::from("static"));
        let mut m = Object::new();
        m.insert("title".into(), get("title"));
        m.insert("title_length".into(), get("title_length"));
        m.insert("title_px".into(), get("title_px"));
        m.insert("meta_description".into(), get("meta_description"));
        m.insert("desc_length".into(), get("desc_length"));
        m.insert("h1_count".into(), Value::from(h1_count));
        m.insert("h1".into(), Value::Array(h1_list));
        m.insert("h2_count".into(), Value::from(h2_count));
        m.insert("word_count".into(), get("word_count"));
        m.insert("text_ratio".into(), get("text_ratio"));
        m.insert("size_bytes".into(), get("size_bytes"));
        m.insert("size_vs_median_ratio".into(), Value::Null);
        m.insert("bytes_per_word".into(), Value::Null);
        m.insert("dom_depth".into(), Value::Null);
        m.insert("dom_nodes".into(), Value::Null);
        m.insert("inlinks".into(), get("inlinks"));
        m.insert("unique_inlinks".into(), get("unique_inlinks"));
        m.insert("outlinks".into(), get("outlinks"));
        m.insert("external_outlinks".into(), get("external_outlinks"));
        m.insert("crawl_depth".into(), get("crawl_depth"));
        m.insert("response_time".into(), get("response_time"));
        m.insert("canonical".into(), get("canonical"));
        m.insert("meta_robots".into(), get("meta_robots"));
        m.insert("link_score".into(), get("link_score"));
        m.insert("is_in_sitemap".into(), Value::Null);
        m.insert("sitemap_lastmod".into(), Value::Null);
        // "static" unless the collector recorded a fuller fetch (#18). An
        // SF export never has this column, so it defaults the same way a
        // native crawl that never escalated would.
        m.insert("representation".into(), representation);
        m
    }
}

/// `severity_rank` + sort tuple for the deduped ordering (mem-store parity of
/// the `issues_order` index).
fn issue_sort_key(i: &Issue) -> (i64, String, String, String, String) {
    let rank = match i.severity.as_str() {
        "critical" => 0,
        "warning" => 1,
        "notice" => 2,
        _ => 3,
    };
    let target_sort = i.target_url.clone().unwrap_or_else(|| "None".into());
    let (src, scope) = if i.check.starts_with("IMG_") {
        let src = i
            .details
            .get("source_page")
            .and_then(Value::as_str)
            .map(str::to_owned);
        let scope = if src.is_some() {
            i.details
                .get("target_scope")
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_owned()
        } else {
            String::new()
        };
        (src.unwrap_or_default(), scope)
    } else {
        (String::new(), String::new())
    };
    (rank, i.check.clone(), target_sort, src, scope)
}
