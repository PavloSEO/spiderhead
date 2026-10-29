//! Offline derivations over persisted corpus, structured and language evidence.
//!
//!
//! This is intentionally an audit-core consumer rather than a collector.  It does not re-open
//! page bodies or contact targets: an unfetched URL row is not a target observation, and every
//! unavailable relationship names that fact.
//!
//! Streaming differs only in memory use: pages and language
//! relations live in a throw-away SQLite index instead of maps, and the derived rows are
//! materialized before returning. Outputs differ only where a page carries more than one
//! language evidence record: streaming derivations see one record at a time, eager groups
//! them by (page, document), the same per-record scope the batch path uses.

use crate::content_evidence::{
    self, ContextRows, MAX_DUPLICATE_CANDIDATES, MAX_DUPLICATE_PAIRS, MIN_DUPLICATE_TOKENS,
};
use crate::error::StoreError;
use crate::evidence_contract::{Obj, SAVED_CORPUS_VERSION};
use crate::structured_evidence::{LANGUAGE_KIND, STRUCTURED_KIND};
use rusqlite::types::Value as SqlValue;
use rusqlite::{params, Connection};
use serde_json::{json, Value};
use spiderhead_core::url_normalize::norm_url as core_norm_url;
use spiderhead_parse::py_str::py_strip;
use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::path::PathBuf;

const PAGE_COLUMNS: [&str; 12] = [
    "url_id",
    "url",
    "document_id",
    "status_code",
    "redirect_url",
    "canonical",
    "meta_robots",
    "x_robots",
    "representation",
    "hreflang_outside_head",
    "body_state",
    "body_sha256",
];

fn invalid(message: impl Into<String>) -> StoreError {
    StoreError::Invalid(message.into())
}

/// URL normalisation, with its error mapped to a store error.
fn norm_url(url: Option<&str>) -> Result<String, StoreError> {
    core_norm_url(url).map_err(|error| invalid(error.to_string()))
}

fn sql_to_json(value: SqlValue) -> Value {
    match value {
        SqlValue::Null | SqlValue::Blob(_) => Value::Null,
        SqlValue::Integer(number) => Value::from(number),
        SqlValue::Real(number) => Value::from(number),
        SqlValue::Text(text) => Value::String(text),
    }
}

/// Text of a declaration value, or empty when it is falsy.
fn text_or_empty(value: Option<&Value>) -> String {
    match value {
        None | Some(Value::Null) | Some(Value::Bool(false)) => String::new(),
        Some(Value::String(text)) => text.clone(),
        Some(Value::Number(number)) if number.as_f64() == Some(0.0) => String::new(),
        Some(Value::Bool(true)) => "True".into(),
        Some(other) => other.to_string(),
    }
}

fn str_of(obj: &Obj, key: &str) -> Option<String> {
    obj.get(key).and_then(Value::as_str).map(str::to_string)
}

fn int_of(obj: &Obj, key: &str) -> Result<i64, StoreError> {
    obj.get(key)
        .and_then(Value::as_i64)
        .ok_or_else(|| invalid(format!("evidence has no integer {key}")))
}

fn fixed(state: &str, reason: &str) -> Obj {
    let mut map = Obj::new();
    map.insert("state".into(), state.into());
    map.insert("reason".into(), reason.into());
    map
}

/// Call `each` with every captured page (`urls` alone never establishes an observation).
fn for_each_page(
    con: &Connection,
    mut each: impl FnMut(Obj) -> Result<(), StoreError>,
) -> Result<(), StoreError> {
    let mut statement = con.prepare(
        "SELECT p.url_id,u.url,p.document_id,p.status_code,p.redirect_url,p.canonical,\
         p.meta_robots,p.x_robots,p.representation,p.hreflang_outside_head,d.body_state,\
         d.body_sha256 FROM pages p JOIN urls u USING(url_id) \
         LEFT JOIN documents d ON d.document_id=p.document_id",
    )?;
    let mut rows = statement.query([])?;
    while let Some(row) = rows.next()? {
        let mut page = Obj::new();
        for (index, name) in PAGE_COLUMNS.iter().enumerate() {
            page.insert((*name).into(), sql_to_json(row.get::<_, SqlValue>(index)?));
        }
        each(page)?;
    }
    Ok(())
}

/// Read only captured pages; `urls` alone never establishes an observation.
pub fn page_rows(con: &Connection) -> Result<Vec<Obj>, StoreError> {
    let mut pages = Vec::new();
    for_each_page(con, |page| {
        pages.push(page);
        Ok(())
    })?;
    Ok(pages)
}

/// Captured pages by `url_id` and by normalized URL.
pub type PageMaps = (BTreeMap<i64, Obj>, BTreeMap<String, Vec<Obj>>);

pub fn page_index(con: &Connection) -> Result<PageMaps, StoreError> {
    let (mut by_id, mut by_normalized) = (BTreeMap::new(), BTreeMap::<String, Vec<Obj>>::new());
    for page in page_rows(con)? {
        let id = int_of(&page, "url_id")?;
        let key = norm_url(page.get("url").and_then(Value::as_str))?;
        by_normalized.entry(key).or_default().push(page.clone());
        by_id.insert(id, page);
    }
    Ok((by_id, by_normalized))
}

/// Lookup of captured pages, backed by maps or by a [`CorpusIndex`].
pub trait PageSource {
    /// The captured page with this `url_id`.
    fn page(&self, id: i64) -> Result<Option<Obj>, StoreError>;
    /// Captured pages with this normalized URL (the consumer only distinguishes 0, 1, many).
    fn captured(&self, key: &str) -> Result<Vec<Obj>, StoreError>;
}

impl PageSource for PageMaps {
    fn page(&self, id: i64) -> Result<Option<Obj>, StoreError> {
        Ok(self.0.get(&id).cloned())
    }
    fn captured(&self, key: &str) -> Result<Vec<Obj>, StoreError> {
        Ok(self.1.get(key).cloned().unwrap_or_default())
    }
}

/// Indexed captured pages and language relations, never a full in-memory graph.
pub struct CorpusIndex {
    con: Option<Connection>,
    path: PathBuf,
    declaration_count: usize,
}

impl CorpusIndex {
    pub fn new(source: &Connection, language: &[Value]) -> Result<Self, StoreError> {
        let path =
            std::env::temp_dir().join(format!("spiderhead-corpus-{}.sqlite", uuid::Uuid::new_v4()));
        let con = Connection::open(&path)?;
        con.execute_batch(
            "CREATE TABLE pages(id INTEGER PRIMARY KEY,url TEXT,norm TEXT,value TEXT);\
             CREATE INDEX page_norm ON pages(norm);\
             CREATE TABLE evidence(id INTEGER PRIMARY KEY,n INTEGER);\
             CREATE TABLE self_codes(target TEXT,lang TEXT,PRIMARY KEY(target,lang)) WITHOUT ROWID;\
             CREATE TABLE edges(representation TEXT,source TEXT,target TEXT,\
             PRIMARY KEY(representation,source,target)) WITHOUT ROWID;\
             BEGIN;",
        )?;
        let mut index = Self {
            con: Some(con),
            path,
            declaration_count: 0,
        };
        index.fill(source, language)?;
        Ok(index)
    }

    fn db(&self) -> Result<&Connection, StoreError> {
        self.con
            .as_ref()
            .ok_or_else(|| invalid("corpus index is closed"))
    }

    fn fill(&mut self, source: &Connection, language: &[Value]) -> Result<(), StoreError> {
        let con = self.db()?;
        for_each_page(source, |page| {
            let id = int_of(&page, "url_id")?;
            let url = str_of(&page, "url");
            let norm = norm_url(url.as_deref())?;
            con.execute(
                "INSERT INTO pages VALUES(?1,?2,?3,?4)",
                params![id, url, norm, serde_json::to_string(&page)?],
            )?;
            Ok(())
        })?;
        let mut declaration_count = 0;
        for evidence in language {
            let evidence = evidence
                .as_object()
                .ok_or_else(|| invalid("language evidence is not an object"))?;
            let empty = Vec::new();
            let declarations = evidence
                .get("declarations")
                .and_then(Value::as_array)
                .unwrap_or(&empty);
            declaration_count += declarations.iter().filter(|item| item.is_object()).count();
            let page_id = int_of(evidence, "page_url_id")?;
            let page = self.get(page_id)?;
            if !source_active(evidence, page.as_ref()) {
                continue;
            }
            let Some(page) = page else { continue };
            con.execute(
                "INSERT INTO evidence VALUES(?1,1) ON CONFLICT(id) DO UPDATE SET n=n+1",
                params![page_id],
            )?;
            let page_url = str_of(&page, "url").unwrap_or_default();
            for item in declarations.iter().filter_map(Value::as_object) {
                let ordinal_ok = item
                    .get("ordinal")
                    .and_then(Value::as_i64)
                    .is_some_and(|n| n >= 0);
                let target = item.get("target").and_then(Value::as_str).unwrap_or("");
                if item.get("state").and_then(Value::as_str) != Some("declared")
                    || !ordinal_ok
                    || target.is_empty()
                    || page_url.is_empty()
                {
                    continue;
                }
                let (from, to) = (norm_url(Some(&page_url))?, norm_url(Some(target))?);
                let representation = evidence.get("representation").and_then(Value::as_str);
                con.execute(
                    "INSERT OR IGNORE INTO edges VALUES(?1,?2,?3)",
                    params![representation, from, to],
                )?;
                let code = py_strip(&text_or_empty(item.get("lang"))).to_lowercase();
                if from == to && !code.is_empty() && code != "x-default" {
                    con.execute(
                        "INSERT OR IGNORE INTO self_codes VALUES(?1,?2)",
                        params![from, code],
                    )?;
                }
            }
        }
        con.execute_batch("COMMIT")?;
        self.declaration_count = declaration_count;
        Ok(())
    }

    /// The captured page with this id, or `None`.
    pub fn get(&self, page_id: i64) -> Result<Option<Obj>, StoreError> {
        let text: Option<String> = self
            .db()?
            .query_row(
                "SELECT value FROM pages WHERE id=?1",
                params![page_id],
                |row| row.get(0),
            )
            .map(Some)
            .or_else(|error| match error {
                rusqlite::Error::QueryReturnedNoRows => Ok(None),
                other => Err(other),
            })?;
        match text {
            Some(text) => Ok(serde_json::from_str::<Value>(&text)?.as_object().cloned()),
            None => Ok(None),
        }
    }

    /// Up to two pages with this normalized URL, in id order.
    pub fn candidates(&self, key: &str) -> Result<Vec<Obj>, StoreError> {
        let mut statement = self
            .db()?
            .prepare("SELECT value FROM pages WHERE norm=?1 ORDER BY id LIMIT 2")?;
        let texts = statement.query_map(params![key], |row| row.get::<_, String>(0))?;
        let mut out = Vec::new();
        for text in texts {
            if let Some(map) = serde_json::from_str::<Value>(&text?)?.as_object() {
                out.push(map.clone());
            }
        }
        Ok(out)
    }

    /// Number of active language evidence records for the page.
    pub fn evidence_count(&self, page_id: i64) -> Result<i64, StoreError> {
        Ok(self
            .db()?
            .query_row(
                "SELECT n FROM evidence WHERE id=?1",
                params![page_id],
                |row| row.get(0),
            )
            .unwrap_or(0))
    }

    pub fn has_edge(
        &self,
        representation: &str,
        source: &str,
        target: &str,
    ) -> Result<bool, StoreError> {
        Ok(self
            .db()?
            .query_row(
                "SELECT 1 FROM edges WHERE representation=?1 AND source=?2 AND target=?3",
                params![representation, source, target],
                |_| Ok(()),
            )
            .is_ok())
    }

    /// Language codes a page declares for itself.
    pub fn self_codes(&self, target: &str) -> Result<BTreeSet<String>, StoreError> {
        let mut statement = self
            .db()?
            .prepare("SELECT lang FROM self_codes WHERE target=?1")?;
        let codes = statement.query_map(params![target], |row| row.get::<_, String>(0))?;
        Ok(codes.collect::<Result<_, _>>()?)
    }

    /// Number of declaration objects seen while indexing.
    pub fn declaration_count(&self) -> usize {
        self.declaration_count
    }

    /// Drop the index file; calling it again is a no-op.
    pub fn close(&mut self) -> Result<(), StoreError> {
        if let Some(con) = self.con.take() {
            drop(con);
            std::fs::remove_file(&self.path)?;
        }
        Ok(())
    }

    /// Where the index file lives while open.
    pub fn path(&self) -> &std::path::Path {
        &self.path
    }
}

impl Drop for CorpusIndex {
    fn drop(&mut self) {
        let _ = self.close();
    }
}

impl PageSource for CorpusIndex {
    fn page(&self, id: i64) -> Result<Option<Obj>, StoreError> {
        self.get(id)
    }
    fn captured(&self, key: &str) -> Result<Vec<Obj>, StoreError> {
        self.candidates(key)
    }
}

/// Whether a language evidence record still describes the page's active document.
pub fn source_active(evidence: &Obj, source: Option<&Obj>) -> bool {
    let Some(source) = source.filter(|map| !map.is_empty()) else {
        return false;
    };
    let null = Value::Null;
    source.get("document_id").unwrap_or(&null)
        == evidence.get("source_document_id").unwrap_or(&null)
        && source.get("representation").unwrap_or(&null)
            == evidence.get("representation").unwrap_or(&null)
        && source.get("body_state").and_then(Value::as_str) == Some("complete")
        && source
            .get("body_sha256")
            .is_some_and(|value| value.as_str().is_some_and(|text| !text.is_empty()))
        && matches!(
            evidence.get("state").and_then(Value::as_str),
            Some("declared" | "absent")
        )
}

/// Retain only the indexability facts the stored page row can support.
pub fn indexability(page: &Obj) -> Result<Obj, StoreError> {
    let Some(status) = page.get("status_code").and_then(Value::as_i64) else {
        return Ok(fixed("unmeasured", "captured page has no HTTP status"));
    };
    if !(200..300).contains(&status) {
        return Ok(fixed("non_indexable", &format!("HTTP {status}")));
    }
    let complete = page.get("body_state").and_then(Value::as_str) == Some("complete")
        && page
            .get("body_sha256")
            .and_then(Value::as_str)
            .is_some_and(|text| !text.is_empty());
    if !complete {
        return Ok(fixed(
            "unmeasured",
            "target body was not retained completely",
        ));
    }
    let directives = spiderhead_parse::parser::meta::robots_directives(&[
        page.get("meta_robots").and_then(Value::as_str),
        page.get("x_robots").and_then(Value::as_str),
    ]);
    if directives.iter().any(|item| item == "noindex") {
        return Ok(fixed("non_indexable", "noindex directive"));
    }
    if let Some(canonical) = page
        .get("canonical")
        .and_then(Value::as_str)
        .filter(|c| !c.is_empty())
    {
        if norm_url(Some(canonical))? != norm_url(page.get("url").and_then(Value::as_str))? {
            return Ok(fixed("non_indexable", "canonicalized to another URL"));
        }
    }
    Ok(fixed(
        "indexable_candidate",
        "stored status, directives and canonical do not exclude indexing",
    ))
}

pub const UNCAPTURED_TARGET_REASON: &str = "target has no captured page observation in this scan";

fn or_empty(page: &Obj, key: &str) -> Value {
    match page.get(key) {
        Some(value) if !matches!(value, Value::Null) && value != &Value::String(String::new()) => {
            value.clone()
        }
        _ => Value::String(String::new()),
    }
}

/// Resolve only one unambiguous captured target page, never a queued URL.
pub fn target(target_url: &str, pages: &dyn PageSource) -> Result<Obj, StoreError> {
    if target_url.is_empty() {
        return Ok(fixed("unmeasured", "declaration has no resolved target"));
    }
    let candidates = pages.captured(&norm_url(Some(target_url))?)?;
    match candidates.as_slice() {
        [] => Ok(fixed("unmeasured", UNCAPTURED_TARGET_REASON)),
        [page] => {
            let get = |key: &str| page.get(key).cloned().unwrap_or(Value::Null);
            let mut map = Obj::new();
            map.insert("state".into(), "observed".into());
            map.insert("page_url_id".into(), get("url_id"));
            map.insert("document_id".into(), get("document_id"));
            map.insert("representation".into(), get("representation"));
            map.insert("body_state".into(), get("body_state"));
            map.insert("status_code".into(), get("status_code"));
            map.insert("redirect_url".into(), or_empty(page, "redirect_url"));
            map.insert("canonical".into(), or_empty(page, "canonical"));
            map.insert("indexability".into(), Value::Object(indexability(page)?));
            Ok(map)
        }
        _ => Ok(fixed(
            "unmeasured",
            "target has multiple captured URL variants in this scan",
        )),
    }
}

pub fn relative(raw_href: &str) -> Obj {
    if raw_href.is_empty() {
        return fixed("malformed", "declaration has no href");
    }
    // urlsplit: a scheme is `[A-Za-z][A-Za-z0-9+.-]*:` at the start.
    let has_scheme = raw_href.find(':').is_some_and(|at| {
        let scheme = &raw_href[..at];
        scheme
            .chars()
            .next()
            .is_some_and(|c| c.is_ascii_alphabetic())
            && scheme
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || "+-.".contains(c))
    });
    if has_scheme || raw_href.starts_with("//") {
        fixed("absolute", "")
    } else {
        fixed("relative", "raw hreflang href is relative")
    }
}

pub fn placement(source: Option<&Obj>) -> Obj {
    let Some(source) = source.filter(|map| !map.is_empty()) else {
        return fixed("unmeasured", "declaring page was not captured");
    };
    let outside = match source.get("hreflang_outside_head") {
        Some(Value::Bool(flag)) => Some(*flag),
        Some(Value::Number(number)) => match number.as_f64() {
            Some(0.0) => Some(false),
            Some(1.0) => Some(true),
            _ => None,
        },
        _ => None,
    };
    let Some(outside) = outside else {
        return fixed(
            "unmeasured",
            "captured page has no hreflang placement observation",
        );
    };
    let mut map = Obj::new();
    map.insert("state".into(), "measured".into());
    map.insert("all_outside_head".into(), outside.into());
    map.insert(
        "reason".into(),
        if outside {
            "all declarations are outside head"
        } else {
            "at least one declaration is in head"
        }
        .into(),
    );
    map
}

fn value_text(value: Option<&Value>) -> String {
    match value {
        Some(Value::String(text)) => text.clone(),
        Some(Value::Null) | None => "None".into(),
        Some(other) => other.to_string(),
    }
}

pub fn source_id(item: &Obj) -> String {
    format!(
        "page:{}:document:{}:representation:{}:ordinal:{}",
        value_text(item.get("source_page_url_id")),
        value_text(item.get("source_document_id")),
        value_text(item.get("representation")),
        value_text(item.get("ordinal")),
    )
}

fn key_of(value: Option<&Value>) -> String {
    serde_json::to_string(value.unwrap_or(&Value::Null)).unwrap_or_default()
}

/// Validate saved declaration-to-target relations within retained representations.
///
/// `relations` is the streaming index; without it the relations come from `language` itself.
pub fn language_derivations(
    language: &[Value],
    pages: &dyn PageSource,
    relations: Option<&CorpusIndex>,
) -> Result<Vec<Obj>, StoreError> {
    let mut rows: Vec<Obj> = Vec::new();
    // Group key (source page, source document) -> row indexes, in first-seen order.
    let mut groups: Vec<Vec<usize>> = Vec::new();
    let mut group_of: HashMap<(i64, String), usize> = HashMap::new();
    let mut active_evidence: HashMap<i64, usize> = HashMap::new();
    for evidence in language {
        let evidence = evidence
            .as_object()
            .ok_or_else(|| invalid("language evidence is not an object"))?;
        let page_id = int_of(evidence, "page_url_id")?;
        let source = pages.page(page_id)?;
        let active = source_active(evidence, source.as_ref());
        if active {
            *active_evidence.entry(page_id).or_default() += 1;
        }
        let empty = Vec::new();
        let declarations = evidence
            .get("declarations")
            .and_then(Value::as_array)
            .unwrap_or(&empty);
        for declaration in declarations.iter().filter_map(Value::as_object) {
            let mut item = Obj::new();
            let lang = text_or_empty(declaration.get("lang"));
            let ordinal = declaration.get("ordinal").cloned().unwrap_or(Value::Null);
            item.insert("source_page_url_id".into(), page_id.into());
            item.insert(
                "source_url".into(),
                source
                    .as_ref()
                    .and_then(|map| map.get("url").cloned())
                    .unwrap_or(Value::Null),
            );
            item.insert(
                "source_document_id".into(),
                evidence
                    .get("source_document_id")
                    .cloned()
                    .unwrap_or(Value::Null),
            );
            item.insert(
                "representation".into(),
                evidence
                    .get("representation")
                    .cloned()
                    .unwrap_or(Value::Null),
            );
            item.insert("ordinal".into(), ordinal.clone());
            item.insert("source_declaration_id".into(), "".into());
            item.insert("lang".into(), lang.clone().into());
            item.insert(
                "lang_normalized".into(),
                py_strip(&lang).to_lowercase().into(),
            );
            item.insert(
                "raw_href".into(),
                text_or_empty(declaration.get("raw_href")).into(),
            );
            item.insert(
                "target".into(),
                text_or_empty(declaration.get("target")).into(),
            );
            item.insert(
                "declaration_state".into(),
                declaration.get("state").cloned().unwrap_or(Value::Null),
            );
            item.insert(
                "source_representation_state".into(),
                if active {
                    "active"
                } else {
                    "superseded_or_unmeasured"
                }
                .into(),
            );
            item.insert(
                "source_placement".into(),
                Value::Object(placement(source.as_ref())),
            );
            let ordinal_ok = ordinal.as_i64().is_some_and(|n| n >= 0);
            if ordinal_ok {
                let id = source_id(&item);
                item.insert("source_declaration_id".into(), id.into());
            } else {
                item.insert("declaration_state".into(), "malformed".into());
                item.insert("source_declaration_id".into(), "unavailable".into());
            }
            let source_url = str_of(&item, "source_url");
            let target_text = str_of(&item, "target").unwrap_or_default();
            item.insert(
                "source_identity".into(),
                norm_url(source_url.as_deref())?.into(),
            );
            item.insert(
                "target_identity".into(),
                norm_url(Some(&target_text))?.into(),
            );
            item.insert(
                "target_observation".into(),
                Value::Object(target(&target_text, pages)?),
            );
            let reason = spiderhead_parse::hreflang_code::code_error(&lang).unwrap_or_default();
            item.insert(
                "code".into(),
                json!({"state": if reason.is_empty() { "valid" } else { "invalid" }, "reason": reason}),
            );
            let raw_href = str_of(&item, "raw_href").unwrap_or_default();
            item.insert("href".into(), Value::Object(relative(&raw_href)));
            rows.push(item);
            if active && ordinal.as_i64().is_some() {
                let key = (page_id, key_of(evidence.get("source_document_id")));
                let slot = *group_of.entry(key).or_insert_with(|| {
                    groups.push(Vec::new());
                    groups.len() - 1
                });
                groups[slot].push(rows.len() - 1);
            }
        }
    }

    let text = |row: &Obj, key: &str| str_of(row, key).unwrap_or_default();
    let declared =
        |row: &Obj| row.get("declaration_state").and_then(Value::as_str) == Some("declared");
    let mut active_edges: HashSet<(String, String, String)> = HashSet::new();
    for &index in groups.iter().flatten() {
        let row = &rows[index];
        if row
            .get("source_url")
            .is_some_and(|v| v.as_str().is_some_and(|s| !s.is_empty()))
            && !text(row, "target").is_empty()
            && declared(row)
        {
            active_edges.insert((
                key_of(row.get("representation")),
                text(row, "source_identity"),
                text(row, "target_identity"),
            ));
        }
    }
    for group in &groups {
        let source_key = norm_url(str_of(&rows[group[0]], "source_url").as_deref())?;
        let mut targets_by_label: HashMap<String, BTreeSet<String>> = HashMap::new();
        let mut labels_by_target: HashMap<String, BTreeSet<String>> = HashMap::new();
        let mut duplicate_pairs: HashMap<(String, String), i64> = HashMap::new();
        for &index in group {
            let row = &rows[index];
            if !declared(row) || text(row, "target_identity").is_empty() {
                continue;
            }
            let (label, target_key) = (text(row, "lang_normalized"), text(row, "target_identity"));
            targets_by_label
                .entry(label.clone())
                .or_default()
                .insert(target_key.clone());
            labels_by_target
                .entry(target_key.clone())
                .or_default()
                .insert(text(row, "lang"));
            *duplicate_pairs.entry((label, target_key)).or_default() += 1;
        }
        let mut declared_targets = HashSet::new();
        for &index in group {
            let row = &rows[index];
            if !text(row, "target").is_empty() && declared(row) {
                declared_targets.insert(norm_url(Some(&text(row, "target")))?);
            }
        }
        let fallback = group.iter().any(|&index| {
            text(&rows[index], "lang").to_lowercase() == "x-default" && declared(&rows[index])
        });
        for &index in group {
            let row = &mut rows[index];
            let target_key = text(row, "target_identity");
            let label = text(row, "lang_normalized");
            let list = |set: Option<&BTreeSet<String>>| -> Vec<Value> {
                set.map(|s| s.iter().cloned().map(Value::from).collect())
                    .unwrap_or_default()
            };
            row.insert(
                "label_context".into(),
                json!({
                    "targets_for_label": list(targets_by_label.get(&label)),
                    "labels_for_target": list(labels_by_target.get(&target_key)),
                    "duplicate_pair_count": duplicate_pairs.get(&(label, target_key.clone())).copied().unwrap_or(0),
                }),
            );
            let present = declared_targets.contains(&source_key);
            row.insert(
                "self_reference".into(),
                Value::Object(fixed(
                    if present { "present" } else { "missing" },
                    if present {
                        ""
                    } else {
                        "no saved self-reference declaration"
                    },
                )),
            );
            row.insert(
                "fallback".into(),
                Value::Object(fixed(
                    if fallback { "present" } else { "missing" },
                    if fallback {
                        ""
                    } else {
                        "no x-default declaration in this representation"
                    },
                )),
            );
            let observation = row
                .get("target_observation")
                .and_then(Value::as_object)
                .cloned()
                .unwrap_or_default();
            let observed_id = observation.get("page_url_id").and_then(Value::as_i64);
            let representation = row.get("representation").cloned().unwrap_or(Value::Null);
            let reciprocity = if observation.get("state").and_then(Value::as_str)
                != Some("observed")
            {
                fixed(
                    "unmeasured",
                    "target page was not captured for a reciprocity check",
                )
            } else if observation.get("representation").unwrap_or(&Value::Null) != &representation {
                fixed(
                    "unmeasured",
                    "target representation differs from the declaring document",
                )
            } else {
                let id = observed_id.unwrap_or(0);
                let count = match relations {
                    Some(index) => index.evidence_count(id)?,
                    None => active_evidence.get(&id).copied().unwrap_or(0) as i64,
                };
                let repr_text = representation.as_str().unwrap_or("");
                let has_edge = match relations {
                    Some(index) => index.has_edge(repr_text, &target_key, &source_key)?,
                    None => active_edges.contains(&(
                        key_of(Some(&representation)),
                        target_key.clone(),
                        source_key.clone(),
                    )),
                };
                if count != 1 {
                    fixed(
                        "unmeasured",
                        "target has no unambiguous complete language declaration evidence",
                    )
                } else if has_edge {
                    fixed("present", "")
                } else {
                    fixed(
                        "missing",
                        "target representation has no saved return declaration",
                    )
                }
            };
            row.insert("reciprocity".into(), Value::Object(reciprocity));
        }
    }
    for row in &mut rows {
        if !row.contains_key("self_reference") {
            let unavailable = fixed(
                "unmeasured",
                "source representation is not the active captured page",
            );
            row.insert("self_reference".into(), Value::Object(unavailable.clone()));
            row.insert("fallback".into(), Value::Object(unavailable.clone()));
            row.insert("reciprocity".into(), Value::Object(unavailable));
        }
    }
    Ok(rows)
}

/// Retained structured and language evidence, as `derive` consumes it.
pub struct Evidence {
    /// Structured-data context items.
    pub structured: Vec<Value>,
    /// Language (hreflang) context items.
    pub language: Vec<Value>,
}

/// The readers `derive` depends on, behind a seam so they can change independently.
pub trait CorpusSources {
    /// Typed structured and language evidence of the scan.
    fn evidence(&self, con: &Connection) -> Result<Evidence, StoreError>;
    /// Stored target probes keyed by normalized target URL; empty when no probe ran.
    fn probes(&self, con: &Connection) -> Result<BTreeMap<String, Obj>, StoreError>;
    /// The target observation a probe supports, or `None` when it answered no status.
    fn probe_observation(&self, payload: &Obj) -> Option<Obj>;
}

/// Reads the retained context items directly; probe observations come from `target_probes`.
pub struct DefaultSources;

impl CorpusSources for DefaultSources {
    fn evidence(&self, con: &Connection) -> Result<Evidence, StoreError> {
        Ok(Evidence {
            structured: ContextRows::new(con, STRUCTURED_KIND, false).rows()?,
            language: ContextRows::new(con, LANGUAGE_KIND, false).rows()?,
        })
    }

    fn probes(&self, con: &Connection) -> Result<BTreeMap<String, Obj>, StoreError> {
        let mut statement =
            con.prepare("SELECT item_key,payload_json FROM context_items WHERE kind=?1")?;
        let rows = statement.query_map(params![crate::target_probes::KIND], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
        })?;
        let mut out = BTreeMap::new();
        for row in rows {
            let (key, payload) = row?;
            let url: String = key.chars().skip("url:".len()).collect();
            if let Some(map) = serde_json::from_str::<Value>(&payload)?.as_object() {
                out.insert(url, map.clone());
            }
        }
        Ok(out)
    }

    fn probe_observation(&self, payload: &Obj) -> Option<Obj> {
        crate::target_probes::observation(payload)
    }
}

/// Return reproducible content/structured/i18n derivations from one scan connection.
pub fn derive(
    con: &Connection,
    duplicate_threshold: f64,
    streaming: bool,
) -> Result<Obj, StoreError> {
    derive_with(con, duplicate_threshold, streaming, &DefaultSources)
}

/// [`derive`] with explicit readers for structured evidence and target probes.
pub fn derive_with(
    con: &Connection,
    duplicate_threshold: f64,
    streaming: bool,
    sources: &dyn CorpusSources,
) -> Result<Obj, StoreError> {
    let content = content_evidence::read(con, streaming)?;
    let items = match content.get("items") {
        Some(Value::Array(list)) => list.clone(),
        _ => Vec::new(),
    };
    let Evidence {
        structured,
        language,
    } = sources.evidence(con)?;
    // In streaming mode the page index is built first and handed to duplicate detection,
    // so pages are never all in memory at once.
    let mut streaming_index = streaming
        .then(|| CorpusIndex::new(con, &language))
        .transpose()?;
    let duplicates = match &streaming_index {
        Some(index) => content_evidence::derive_duplicates_lookup(
            &items,
            duplicate_threshold,
            false,
            &|id| Ok(index.get(id)?.and_then(|page| str_of(&page, "url"))),
            MIN_DUPLICATE_TOKENS,
            MAX_DUPLICATE_CANDIDATES,
            MAX_DUPLICATE_PAIRS,
            streaming,
        )?,
        None => {
            let page_urls: BTreeMap<i64, String> = page_rows(con)?
                .iter()
                .filter_map(|page| Some((page.get("url_id")?.as_i64()?, str_of(page, "url")?)))
                .collect();
            content_evidence::derive_duplicates(
                &items,
                duplicate_threshold,
                false,
                Some(&page_urls),
                MIN_DUPLICATE_TOKENS,
                MAX_DUPLICATE_CANDIDATES,
                MAX_DUPLICATE_PAIRS,
                streaming,
            )?
        }
    };
    let (crawl_partial, corpus_partial): (bool, bool) = con.query_row(
        "SELECT crawl_partial,corpus_partial FROM scan WHERE singleton=1",
        [],
        |row| {
            Ok((
                row.get::<_, Option<i64>>(0)?.unwrap_or(0) != 0,
                row.get::<_, Option<i64>>(1)?.unwrap_or(0) != 0,
            ))
        },
    )?;
    let probes = sources.probes(con)?;

    let mut declarations: Vec<Obj> = if let Some(index) = streaming_index.as_mut() {
        let mut all = Vec::new();
        for evidence in &language {
            all.extend(language_derivations(
                std::slice::from_ref(evidence),
                index,
                Some(&*index),
            )?);
        }
        index.close()?;
        all
    } else {
        let maps = page_index(con)?;
        language_derivations(&language, &maps, None)?
    };
    let empty = Obj::new();
    for declaration in &mut declarations {
        let reciprocity_state = declaration
            .get("reciprocity")
            .and_then(|v| v.get("state"))
            .and_then(Value::as_str)
            .map(str::to_string);
        if (crawl_partial || corpus_partial) && reciprocity_state.as_deref() == Some("present") {
            declaration.insert(
                "reciprocity".into(),
                Value::Object(fixed(
                    "unmeasured",
                    "the retained crawl or corpus is partial; a clean relationship is unproven",
                )),
            );
        }
        let uncaptured = declaration
            .get("target_observation")
            .and_then(|v| v.get("reason"))
            .and_then(Value::as_str)
            == Some(UNCAPTURED_TARGET_REASON);
        if !probes.is_empty() && uncaptured {
            let identity = str_of(declaration, "target_identity").unwrap_or_default();
            if let Some(observed) =
                sources.probe_observation(probes.get(&identity).unwrap_or(&empty))
            {
                declaration.insert("target_observation".into(), Value::Object(observed));
            }
        }
    }

    let mut relation_states = Obj::new();
    for declaration in &declarations {
        let state = declaration
            .get("reciprocity")
            .and_then(|v| v.get("state"))
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string();
        let count = relation_states
            .get(&state)
            .and_then(Value::as_i64)
            .unwrap_or(0)
            + 1;
        relation_states.insert(state, count.into());
    }
    let partial = crawl_partial || corpus_partial;
    let unmeasured = relation_states
        .get("unmeasured")
        .and_then(Value::as_i64)
        .unwrap_or(0)
        != 0;
    let coverage_state = if partial || unmeasured {
        "partial"
    } else if !declarations.is_empty() {
        "complete"
    } else {
        "unmeasured"
    };
    let reason = if partial {
        "crawl or corpus is partial"
    } else if unmeasured {
        "one or more targets lack complete declaration evidence"
    } else if declarations.is_empty() {
        "no saved hreflang declarations"
    } else {
        "all saved declaration relations were measured"
    };
    let mut structured_states = Obj::new();
    for item in &structured {
        let key = match item.get("state") {
            Some(Value::String(text)) => text.clone(),
            Some(Value::Null) | None => "null".into(),
            Some(other) => other.to_string(),
        };
        let count = structured_states
            .get(&key)
            .and_then(Value::as_i64)
            .unwrap_or(0)
            + 1;
        structured_states.insert(key, count.into());
    }
    let mut result = Obj::new();
    result.insert("schema_version".into(), SAVED_CORPUS_VERSION.into());
    result.insert(
        "duplicates".into(),
        Value::Object(duplicates.into_iter().collect()),
    );
    result.insert(
        "structured".into(),
        json!({"states": structured_states, "items": structured}),
    );
    result.insert(
        "internationalization".into(),
        json!({
            "declarations": declarations.into_iter().map(Value::Object).collect::<Vec<_>>(),
            "items": language,
            "coverage": {
                "state": coverage_state,
                "crawl_partial": crawl_partial,
                "corpus_partial": corpus_partial,
                "states": relation_states,
                "reason": reason,
            },
            "target_population": "captured pages only; queued or linked URLs are unmeasured",
        }),
    );
    Ok(result)
}

#[cfg(test)]
#[path = "corpus_derivations_tests.rs"]
mod tests;
