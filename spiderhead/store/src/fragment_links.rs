//! Broken in-page fragment detection over retained documents (issue #827).
//!
//! A broken bookmark is not a broken HTTP link: the fragment in `href="page#target"` is
//! resolved inside the destination document. Each `(page, representation)` lane is evaluated
//! on its own; only `complete` bodies are evidence; `<template>` content is inert.
#![allow(clippy::too_many_arguments, clippy::type_complexity)]

use crate::bodies::read_document;
use crate::discovery_ledger::{document_base_url, is_py_int, py_str, query_maps};
use crate::error::StoreError;
use crate::frontier::{urlsplit, urlunsplit};
use rusqlite::Connection;
use serde_json::{json, Map, Value};
use spiderhead_parse::py_url::urljoin as py_urljoin;
use std::collections::{BTreeMap, BTreeSet};

pub const EVALUATION_VERSION: &str = "fragment_links.v1";

pub const REP_ORDER: &[&str] = &["static", "rendered", "legacy_fragment"];

pub const REP_RANK: &[(&str, i64)] = &[("static", 0), ("rendered", 1), ("legacy_fragment", 2)];

pub const MAX_ANCHORS_PER_DOCUMENT: i64 = 10000;

pub const MAX_TARGETS_PER_DOCUMENT: i64 = 100000;

pub const MAX_OCCURRENCES: i64 = 100000;

pub const MAX_UNAVAILABLE_SOURCES: i64 = 500;

pub const MAX_REDIRECT_HOPS: i64 = 8;

pub const DEFAULT_MAX_DECODED_BYTES: i64 = 8388608;

pub const SKIP: &str = "skipped";

pub const RESOLVED: &str = "resolved";

pub const MISSING: &str = "missing";

/// Text of the value; a missing key prints as `None`.
fn py_str_text(v: Option<&Value>) -> String {
    py_str(v.unwrap_or(&Value::Null))
}

fn str_field<'a>(m: &'a Map<String, Value>, k: &str) -> &'a str {
    m.get(k).and_then(Value::as_str).unwrap_or("")
}

fn int_field(m: &Map<String, Value>, k: &str) -> Option<i64> {
    m.get(k).filter(|v| is_py_int(v)).and_then(Value::as_i64)
}

/// The scan's fragment-free URL identity, the same key the crawler de-duplicates on.
/// Returns `""` for input the split cannot describe.
pub fn canonical_key(url: &str) -> String {
    let parts = urlsplit(url);
    let path = if parts.path.is_empty() {
        "/"
    } else {
        parts.path.as_str()
    };
    urlunsplit(&parts.scheme, &parts.netloc, path, &parts.query)
}

pub fn scheme(url: &str) -> String {
    urlsplit(url).scheme
}

/// Percent-decode, then UTF-8 decode with replacement (URL decoder: `+` stays a plus).
pub fn decode_fragment(fragment: &str) -> String {
    let bytes = fragment.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' && i + 2 < bytes.len() {
            let hex = std::str::from_utf8(&bytes[i + 1..i + 3]).ok();
            if let Some(v) = hex.and_then(|h| u8::from_str_radix(h, 16).ok()) {
                out.push(v);
                i += 3;
                continue;
            }
        }
        out.push(bytes[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// ASCII-case-insensitive `top`.
pub fn ascii_case_top(value: &str) -> bool {
    value.len() == 3 && value.eq_ignore_ascii_case("top")
}

/// WHATWG "find a potential indicated element" over an id/name inventory.
pub fn indicated_element(
    fragment: &str,
    ids: &BTreeSet<String>,
    names: &BTreeSet<String>,
) -> Option<(String, String)> {
    let decoded = decode_fragment(fragment);
    if ids.contains(fragment) {
        return Some(("element_id".into(), fragment.into()));
    }
    if names.contains(fragment) {
        return Some(("anchor_name".into(), fragment.into()));
    }
    if ids.contains(&decoded) {
        return Some(("element_id".into(), decoded));
    }
    if names.contains(&decoded) {
        return Some(("anchor_name".into(), decoded));
    }
    if ascii_case_top(&decoded) {
        return Some(("top".into(), "top".into()));
    }
    None
}

/// Parsed retained document: fragment anchors plus the id/name inventory.
pub struct DocEvidence {
    /// Fragment-bearing anchors in document order (keys: ordinal, raw_href, resolved_url, fragment, unresolvable).
    pub anchors: Vec<BTreeMap<String, Value>>,
    /// Anchors counted but not stored (per-document cap).
    pub anchors_omitted: i64,
    /// Element ids, shared; per-occurrence use only borrows them.
    pub ids: std::rc::Rc<BTreeSet<String>>,
    /// Legacy `<a name>` values (shared).
    pub names: std::rc::Rc<BTreeSet<String>>,
    /// Target inventory hit its cap.
    pub targets_truncated: bool,
    /// Effective `<base href>` URL.
    pub base_url: String,
}

/// Strict `text/html` media-type test: parameters and case ignored.
pub fn is_html(content_type: &str) -> bool {
    content_type
        .split(';')
        .next()
        .unwrap_or("")
        .trim()
        .eq_ignore_ascii_case("text/html")
}

/// Whether `element` sits inside a `<template>` (inert content).
fn inside_template(el: scraper::ElementRef<'_>) -> bool {
    el.ancestors()
        .filter_map(scraper::ElementRef::wrap)
        .any(|a| a.value().name() == "template")
}

/// One parse pass: fragment anchors, element ids and `<a name>` targets.
pub fn extract(html: &str, final_url: &str) -> DocEvidence {
    let base_url = document_base_url(html, final_url);
    let doc = scraper::Html::parse_document(html);
    let mut ids = BTreeSet::new();
    let mut names = BTreeSet::new();
    let mut anchors = Vec::new();
    let mut anchors_omitted = 0i64;
    let mut targets_truncated = false;
    let mut targets: i64 = 0;
    for el in doc
        .root_element()
        .descendants()
        .filter_map(scraper::ElementRef::wrap)
    {
        if inside_template(el) {
            continue;
        }
        let attrs = el.value();
        if let Some(id) = attrs.attr("id") {
            if targets < MAX_TARGETS_PER_DOCUMENT {
                ids.insert(id.to_string());
                targets += 1;
            } else {
                targets_truncated = true;
            }
        }
        if attrs.name() != "a" {
            continue;
        }
        if let Some(name) = attrs.attr("name") {
            if targets < MAX_TARGETS_PER_DOCUMENT {
                names.insert(name.to_string());
                targets += 1;
            } else {
                targets_truncated = true;
            }
        }
        let Some(href) = attrs.attr("href") else {
            continue;
        };
        if anchors.len() as i64 >= MAX_ANCHORS_PER_DOCUMENT {
            anchors_omitted += 1;
            continue;
        }
        let (resolved, unresolvable) = match py_urljoin(&base_url, href) {
            Ok(mut resolved) => {
                // Empty fragments (`#`) are dropped by URL parsing, yet HTML treats them as a valid target.
                if href.contains('#') && !resolved.contains('#') {
                    resolved.push('#');
                    resolved.push_str(href.split_once('#').map_or("", |x| x.1));
                }
                (resolved, false)
            }
            Err(_) => (String::new(), true),
        };
        let fragment = if let Some((_, f)) = resolved.split_once('#') {
            f.to_string()
        } else if unresolvable && href.contains('#') {
            // The resolver refused the href but it visibly carries a fragment: keep it as
            // an unmeasurable occurrence rather than dropping unverified evidence.
            href.split_once('#').map_or("", |x| x.1).to_string()
        } else {
            continue;
        };
        let mut anchor = BTreeMap::new();
        anchor.insert("ordinal".to_string(), Value::from(anchors.len() as i64));
        anchor.insert("raw_href".to_string(), Value::from(href));
        anchor.insert("resolved_url".to_string(), Value::from(resolved));
        anchor.insert("fragment".to_string(), Value::from(fragment));
        anchor.insert("unresolvable".to_string(), Value::Bool(unresolvable));
        anchors.push(anchor);
    }
    DocEvidence {
        anchors,
        anchors_omitted,
        ids: std::rc::Rc::new(ids),
        names: std::rc::Rc::new(names),
        targets_truncated,
        base_url,
    }
}

/// The active retained document per `(url_id, representation)`, selection order as in body-diff.
fn document_rows(
    con: &Connection,
) -> Result<BTreeMap<(i64, String), Map<String, Value>>, StoreError> {
    let rows = query_maps(
        con,
        "SELECT d.*, r.content_type AS response_content_type, \
         (p.document_id=d.document_id) AS selected, \
         (i.item_key IS NOT NULL) AS inventory_marked \
         FROM documents d \
         LEFT JOIN responses r ON r.response_id=d.source_response_id \
         LEFT JOIN pages p ON p.url_id=d.url_id \
         LEFT JOIN context_items i ON i.kind='resource_inventory' \
         AND i.item_key='document:'||d.document_id \
         ORDER BY d.url_id,d.representation,selected DESC,inventory_marked DESC,d.document_id DESC",
        [],
    )?;
    let mut active = BTreeMap::new();
    for row in rows {
        let key = (
            row.get("url_id").and_then(Value::as_i64).unwrap_or(0),
            str_field(&row, "representation").to_string(),
        );
        active.entry(key).or_insert(row);
    }
    Ok(active)
}

fn document_content_type(document: &Map<String, Value>, page_content_type: &str) -> String {
    match document.get("response_content_type") {
        Some(Value::String(s)) if !s.is_empty() => s.clone(),
        _ => page_content_type.to_string(),
    }
}

/// The URL the retained document was served at; the page URL when unknown.
fn document_final_url(con: &Connection, document: &Map<String, Value>, fallback: &str) -> String {
    let lookup = || -> rusqlite::Result<Option<String>> {
        if str_field(document, "representation") == "static" {
            let Some(response_id) = int_field(document, "source_response_id") else {
                return Ok(None);
            };
            con.query_row(
                "SELECT u.url FROM responses r JOIN urls u ON u.url_id=r.effective_url_id WHERE r.response_id=?1",
                [response_id],
                |r| r.get::<_, Option<String>>(0),
            )
            .or_else(|e| match e {
                rusqlite::Error::QueryReturnedNoRows => Ok(None),
                other => Err(other),
            })
        } else {
            let text = py_str_text(document.get("renderer_json"));
            let renderer: Value = if text.is_empty() {
                json!({})
            } else {
                serde_json::from_str(&text).unwrap_or_else(|_| json!({}))
            };
            let renderer = if renderer.is_object() {
                renderer
            } else {
                json!({})
            };
            let mut final_id = renderer.get("final_url_id").and_then(Value::as_i64);
            if final_id.is_none() {
                final_id = renderer.get("navigation_url_id").and_then(Value::as_i64);
            }
            let Some(final_id) = final_id else {
                return Ok(None);
            };
            con.query_row("SELECT url FROM urls WHERE url_id=?1", [final_id], |r| {
                r.get::<_, Option<String>>(0)
            })
            .or_else(|e| match e {
                rusqlite::Error::QueryReturnedNoRows => Ok(None),
                other => Err(other),
            })
        }
    };
    match lookup() {
        Ok(Some(url)) if !url.is_empty() => url,
        _ => fallback.to_string(),
    }
}

/// Evaluate every fragment-bearing anchor in retained complete HTML/DOM bodies.
/// Read-only and deterministic: occurrences follow page ordinal, representation rank and anchor order.
pub fn evaluate(
    con: &Connection,
    max_decoded_bytes: i64,
    max_occurrences: i64,
) -> Result<Map<String, Value>, StoreError> {
    let pages = query_maps(
        con,
        "SELECT p.url_id,p.page_ordinal,p.document_id,p.redirect_url,p.final_url,\
         p.content_type,p.status_code,u.url FROM pages p JOIN urls u USING(url_id) \
         ORDER BY p.page_ordinal",
        [],
    )?;
    let active = document_rows(con)?;
    let mut page_by_key: BTreeMap<String, Map<String, Value>> = BTreeMap::new();
    for page in &pages {
        page_by_key
            .entry(canonical_key(str_field(page, "url")))
            .or_insert_with(|| page.clone());
    }
    let mut ev = Evaluator {
        con,
        max_decoded_bytes,
        page_by_key,
        active,
        documents: BTreeMap::new(),
        unavailable_sources: Vec::new(),
        unavailable_omitted: 0,
        lanes_evaluated: 0,
        lanes_unavailable: 0,
        lanes_non_html: 0,
        occurrences: Vec::new(),
        occurrences_omitted: 0,
        max_occurrences,
    };
    for page in &pages {
        ev.evaluate_page(page)?;
    }
    Ok(ev.finish(max_decoded_bytes, max_occurrences))
}

struct Evaluator<'a> {
    con: &'a Connection,
    max_decoded_bytes: i64,
    page_by_key: BTreeMap<String, Map<String, Value>>,
    active: BTreeMap<(i64, String), Map<String, Value>>,
    documents: BTreeMap<i64, DocEvidence>,
    unavailable_sources: Vec<Value>,
    unavailable_omitted: i64,
    lanes_evaluated: i64,
    lanes_unavailable: i64,
    lanes_non_html: i64,
    occurrences: Vec<Map<String, Value>>,
    occurrences_omitted: i64,
    max_occurrences: i64,
}

impl Evaluator<'_> {
    fn evidence(
        &mut self,
        document: &Map<String, Value>,
        final_url: &str,
    ) -> Result<&DocEvidence, StoreError> {
        let id = int_field(document, "document_id").unwrap_or(0);
        if !self.documents.contains_key(&id) {
            let html = read_document(self.con, id, self.max_decoded_bytes)?;
            self.documents.insert(id, extract(&html, final_url));
        }
        Ok(&self.documents[&id])
    }

    fn record_unavailable(&mut self, url: &str, representation: &str, reason: &str) {
        self.lanes_unavailable += 1;
        if (self.unavailable_sources.len() as i64) < MAX_UNAVAILABLE_SOURCES {
            self.unavailable_sources
                .push(json!({"url": url, "representation": representation, "reason": reason}));
        } else {
            self.unavailable_omitted += 1;
        }
    }

    /// Resolve a fragment-free URL to its retained document.
    /// Returns `(page, document, final_url, skip_reason)`.
    fn destination(
        &self,
        dest_url: &str,
        representation: &str,
    ) -> (
        Option<Map<String, Value>>,
        Option<Map<String, Value>>,
        String,
        String,
    ) {
        let Some(mut page) = self.page_by_key.get(&canonical_key(dest_url)).cloned() else {
            return (None, None, String::new(), "destination_not_in_scan".into());
        };
        let mut seen: BTreeSet<i64> = BTreeSet::new();
        seen.insert(int_field(&page, "url_id").unwrap_or(0));
        let mut hops = 0i64;
        loop {
            let redirect = py_str_text(page.get("redirect_url"));
            let nxt = if redirect.is_empty() {
                py_str_text(page.get("final_url"))
            } else {
                redirect
            };
            if nxt.is_empty() {
                break;
            }
            let Some(nxt_page) = self.page_by_key.get(&canonical_key(&nxt)).cloned() else {
                return (
                    Some(page),
                    None,
                    nxt,
                    "destination_redirect_target_absent".into(),
                );
            };
            let nxt_id = int_field(&nxt_page, "url_id").unwrap_or(0);
            if nxt_id == int_field(&page, "url_id").unwrap_or(0) {
                return (
                    Some(page),
                    None,
                    String::new(),
                    "destination_redirect_unresolved".into(),
                );
            }
            if seen.contains(&nxt_id) || hops >= MAX_REDIRECT_HOPS {
                return (
                    Some(page),
                    None,
                    String::new(),
                    "destination_redirect_unresolved".into(),
                );
            }
            seen.insert(nxt_id);
            page = nxt_page;
            hops += 1;
        }
        let page_id = int_field(&page, "url_id").unwrap_or(0);
        let page_url = str_field(&page, "url").to_string();
        let Some(document) = self
            .active
            .get(&(page_id, representation.to_string()))
            .cloned()
        else {
            return (
                Some(page),
                None,
                page_url,
                "destination_document_absent".into(),
            );
        };
        let content_type = str_field(&page, "content_type").to_string();
        if !is_html(&document_content_type(&document, &content_type)) {
            return (
                Some(page),
                Some(document),
                page_url,
                "destination_not_html".into(),
            );
        }
        let body_state = str_field(&document, "body_state");
        if body_state != "complete" || document.get("body_sha256").is_none_or(Value::is_null) {
            let reason = format!("destination_body_{body_state}");
            return (Some(page), Some(document), page_url, reason);
        }
        (Some(page), Some(document), page_url, String::new())
    }

    fn evaluate_page(&mut self, page: &Map<String, Value>) -> Result<(), StoreError> {
        let page_id = int_field(page, "url_id").unwrap_or(0);
        let page_url = str_field(page, "url").to_string();
        let page_ct = str_field(page, "content_type").to_string();
        let lanes: Vec<(&str, Map<String, Value>)> = REP_ORDER
            .iter()
            .filter_map(|rep| {
                self.active
                    .get(&(page_id, rep.to_string()))
                    .map(|d| (*rep, d.clone()))
            })
            .collect();
        if lanes.is_empty() {
            if is_html(&page_ct) {
                self.record_unavailable(&page_url, "page", "document_absent");
            } else {
                self.lanes_non_html += 1;
            }
            return Ok(());
        }
        for (representation, document) in lanes {
            let final_url = document_final_url(self.con, &document, &page_url);
            if !is_html(&document_content_type(&document, &page_ct)) {
                self.lanes_non_html += 1;
                continue;
            }
            let body_state = str_field(&document, "body_state").to_string();
            if body_state != "complete" || document.get("body_sha256").is_none_or(Value::is_null) {
                let reason = format!(
                    "body_{}/{}",
                    body_state,
                    py_str_text(document.get("body_reason"))
                );
                self.record_unavailable(&page_url, representation, &reason);
                continue;
            }
            self.lanes_evaluated += 1;
            let anchors = self.evidence(&document, &final_url)?.anchors.clone();
            for anchor in anchors {
                self.evaluate_anchor(&page_url, representation, &anchor, &document)?;
            }
        }
        Ok(())
    }

    fn evaluate_anchor(
        &mut self,
        page_url: &str,
        representation: &str,
        anchor: &BTreeMap<String, Value>,
        _document: &Map<String, Value>,
    ) -> Result<(), StoreError> {
        let fragment = anchor["fragment"].as_str().unwrap_or("").to_string();
        let resolved = anchor["resolved_url"].as_str().unwrap_or("").to_string();
        let dest_url_base = resolved
            .split_once('#')
            .map_or(resolved.clone(), |(a, _)| a.to_string());
        let mut base = Map::new();
        base.insert("source_url".into(), Value::from(page_url));
        base.insert("source_representation".into(), Value::from(representation));
        base.insert("ordinal".into(), anchor["ordinal"].clone());
        base.insert("raw_href".into(), anchor["raw_href"].clone());
        base.insert("resolved_url".into(), Value::from(resolved.clone()));
        base.insert("fragment".into(), Value::from(fragment.clone()));
        base.insert(
            "decoded_fragment".into(),
            Value::from(decode_fragment(&fragment)),
        );
        base.insert("destination_url".into(), Value::from(dest_url_base.clone()));
        base.insert("destination_document_url".into(), Value::Null);
        base.insert(
            "destination_representation".into(),
            Value::from(representation),
        );
        let unresolvable = anchor["unresolvable"].as_bool().unwrap_or(false);
        if self.occurrences.len() as i64 >= self.max_occurrences {
            self.occurrences_omitted += 1;
            return Ok(());
        }
        let skip = |mut base: Map<String, Value>, reason: &str, note: &str| {
            base.insert("state".into(), Value::from(SKIP));
            base.insert("match".into(), Value::Null);
            base.insert("reason".into(), Value::from(reason));
            base.insert("note".into(), Value::from(note));
            base
        };
        if unresolvable {
            self.occurrences.push(skip(base, "href_unresolvable", ""));
            return Ok(());
        }
        if !matches!(scheme(&dest_url_base).as_str(), "http" | "https") {
            self.occurrences
                .push(skip(base, "destination_scheme_unsupported", ""));
            return Ok(());
        }
        let (dest_page, dest_document, landing_url, skip_reason) =
            self.destination(&dest_url_base, representation);
        let (Some(dest_page), Some(dest_document)) = (dest_page.as_ref(), dest_document.as_ref())
        else {
            let note = dest_document
                .as_ref()
                .map(|d| py_str_text(d.get("body_reason")))
                .unwrap_or_default();
            let mut item = skip(base, &skip_reason, &note);
            item.insert(
                "destination_document_url".into(),
                if landing_url.is_empty() {
                    Value::Null
                } else {
                    Value::from(landing_url)
                },
            );
            self.occurrences.push(item);
            return Ok(());
        };
        if !skip_reason.is_empty() {
            let note = py_str_text(dest_document.get("body_reason"));
            let mut item = skip(base, &skip_reason, &note);
            item.insert(
                "destination_document_url".into(),
                if landing_url.is_empty() {
                    Value::Null
                } else {
                    Value::from(landing_url)
                },
            );
            self.occurrences.push(item);
            return Ok(());
        }
        let dest_final = document_final_url(self.con, dest_document, str_field(dest_page, "url"));
        let dest_evidence = self.evidence(dest_document, &dest_final)?;
        let ids = dest_evidence.ids.clone();
        let names = dest_evidence.names.clone();
        let truncated = dest_evidence.targets_truncated;
        base.insert(
            "destination_document_url".into(),
            Value::from(landing_url.clone()),
        );
        let resolved_item = |mut base: Map<String, Value>, kind: &str, value: &str, note: &str| {
            base.insert("state".into(), Value::from(RESOLVED));
            base.insert("match".into(), json!({"kind": kind, "value": value}));
            base.insert("reason".into(), Value::from(""));
            base.insert("note".into(), Value::from(note));
            base
        };
        if let Some((element_part, _)) = fragment.split_once(":~:") {
            let found = if element_part.is_empty() {
                None
            } else {
                indicated_element(element_part, &ids, &names)
            };
            let item = match found {
                Some((kind, value)) => {
                    resolved_item(base, &kind, &value, "fragment_directive_unverified")
                }
                None => skip(base, "fragment_directive_unverified", ""),
            };
            self.occurrences.push(item);
            return Ok(());
        }
        let matched = if fragment.is_empty() {
            Some(("top".to_string(), String::new()))
        } else {
            indicated_element(&fragment, &ids, &names)
        };
        let item = if let Some((kind, value)) = matched {
            resolved_item(base, &kind, &value, "")
        } else if truncated {
            skip(base, "destination_target_inventory_truncated", "")
        } else {
            let mut item = base;
            item.insert("state".into(), Value::from(MISSING));
            item.insert("match".into(), Value::Null);
            item.insert("reason".into(), Value::from("no_matching_fragment_target"));
            item.insert("note".into(), Value::from(""));
            item
        };
        self.occurrences.push(item);
        Ok(())
    }

    fn finish(self, max_decoded_bytes: i64, max_occurrences: i64) -> Map<String, Value> {
        let docs = self.documents.values();
        let ids_total: i64 = self.documents.values().map(|d| d.ids.len() as i64).sum();
        let names_total: i64 = self.documents.values().map(|d| d.names.len() as i64).sum();
        let anchors_total: i64 = self
            .documents
            .values()
            .map(|d| d.anchors.len() as i64 + d.anchors_omitted)
            .sum();
        let anchors_omitted: i64 = self.documents.values().map(|d| d.anchors_omitted).sum();
        let truncated_docs = docs.clone().filter(|d| d.targets_truncated).count() as i64;
        let count_state = |s: &str| {
            self.occurrences
                .iter()
                .filter(|o| o.get("state").and_then(Value::as_str) == Some(s))
                .count() as i64
        };
        let mut skip_reasons: BTreeMap<String, i64> = BTreeMap::new();
        for item in &self.occurrences {
            if item.get("state").and_then(Value::as_str) == Some(SKIP) {
                *skip_reasons
                    .entry(str_field(item, "reason").to_string())
                    .or_insert(0) += 1;
            }
        }
        let resolved = count_state(RESOLVED);
        let missing = count_state(MISSING);
        let skipped = count_state(SKIP);
        let partial = anchors_omitted > 0
            || self.occurrences_omitted > 0
            || self.unavailable_omitted > 0
            || truncated_docs > 0
            || self.lanes_unavailable > 0
            || !skip_reasons.is_empty();
        let occurrences_seen = self.occurrences.len() as i64 + self.occurrences_omitted;
        let skip_reason_map: Map<String, Value> = skip_reasons
            .into_iter()
            .map(|(k, v)| (k, Value::from(v)))
            .collect();
        let mut out = Map::new();
        out.insert("analysis".into(), Value::from(EVALUATION_VERSION));
        out.insert(
            "occurrences".into(),
            Value::Array(self.occurrences.into_iter().map(Value::Object).collect()),
        );
        let mut states = Map::new();
        states.insert(RESOLVED.into(), Value::from(resolved));
        states.insert(MISSING.into(), Value::from(missing));
        states.insert(SKIP.into(), Value::from(skipped));
        out.insert("states".into(), Value::Object(states));
        out.insert(
            "coverage".into(),
            json!({
                "state": if partial { "partial" } else { "complete" },
                "source_documents_evaluated": self.lanes_evaluated,
                "source_documents_unavailable": self.lanes_unavailable,
                "source_documents_non_html": self.lanes_non_html,
                "unavailable_sources": self.unavailable_sources,
                "unavailable_sources_omitted": self.unavailable_omitted,
                "anchors_seen": anchors_total,
                "anchors_omitted": anchors_omitted,
                "occurrences_seen": occurrences_seen,
                "occurrences_omitted": self.occurrences_omitted,
                "occurrences_skipped": skipped,
                "skip_reasons": Value::Object(skip_reason_map),
                "target_ids_recorded": ids_total,
                "target_names_recorded": names_total,
                "target_inventory_truncated_documents": truncated_docs,
                "limits": {
                    "max_decoded_bytes": max_decoded_bytes,
                    "max_occurrences": max_occurrences,
                    "max_anchors_per_document": MAX_ANCHORS_PER_DOCUMENT,
                    "max_targets_per_document": MAX_TARGETS_PER_DOCUMENT,
                },
            }),
        );
        out
    }
}

/// Group `missing` occurrences into bounded audit-finding records, in first-seen order.
pub fn findings(evaluation: &Map<String, Value>) -> Vec<Map<String, Value>> {
    let mut index: BTreeMap<(String, String, String), usize> = BTreeMap::new();
    let mut groups: Vec<Map<String, Value>> = Vec::new();
    let empty = Vec::new();
    for item in evaluation
        .get("occurrences")
        .and_then(Value::as_array)
        .unwrap_or(&empty)
    {
        let Some(item) = item.as_object() else {
            continue;
        };
        if str_field(item, "state") != MISSING {
            continue;
        }
        let doc_url = str_field(item, "destination_document_url");
        let destination = if doc_url.is_empty() {
            str_field(item, "destination_url").to_string()
        } else {
            doc_url.to_string()
        };
        let rep = str_field(item, "destination_representation").to_string();
        let fragment = str_field(item, "fragment").to_string();
        let location = json!({
            "source_url": item.get("source_url").cloned().unwrap_or(Value::Null),
            "source_representation": item.get("source_representation").cloned().unwrap_or(Value::Null),
            "raw_href": item.get("raw_href").cloned().unwrap_or(Value::Null),
            "resolved_url": item.get("resolved_url").cloned().unwrap_or(Value::Null),
            "ordinal": item.get("ordinal").cloned().unwrap_or(Value::Null),
        });
        let key = (destination.clone(), rep.clone(), fragment.clone());
        let slot = match index.get(&key) {
            Some(i) => *i,
            None => {
                let mut group = Map::new();
                group.insert("target_url".into(), Value::from(destination));
                group.insert("fragment".into(), Value::from(fragment));
                group.insert(
                    "decoded_fragment".into(),
                    item.get("decoded_fragment").cloned().unwrap_or(Value::Null),
                );
                group.insert("destination_representation".into(), Value::from(rep));
                group.insert("occurrences_count".into(), Value::from(0));
                group.insert("locations".into(), Value::Array(Vec::new()));
                group.insert("locations_omitted".into(), Value::from(0));
                groups.push(group);
                index.insert(key, groups.len() - 1);
                groups.len() - 1
            }
        };
        let group = &mut groups[slot];
        let count = group["occurrences_count"].as_i64().unwrap_or(0) + 1;
        group.insert("occurrences_count".into(), Value::from(count));
        let locations = group.get_mut("locations").and_then(Value::as_array_mut);
        if let Some(locations) = locations {
            if locations.len() < 100 {
                locations.push(location);
                continue;
            }
        }
        let omitted = group["locations_omitted"].as_i64().unwrap_or(0) + 1;
        group.insert("locations_omitted".into(), Value::from(omitted));
    }
    groups
}

/// Deterministic offset/limit slice over the evaluated occurrences.
pub fn paginate(
    evaluation: &Map<String, Value>,
    offset: i64,
    limit: i64,
    state: Option<&str>,
    representation: Option<&str>,
) -> Map<String, Value> {
    let empty = Vec::new();
    let mut items: Vec<&Value> = evaluation
        .get("occurrences")
        .and_then(Value::as_array)
        .unwrap_or(&empty)
        .iter()
        .collect();
    if let Some(state) = state {
        items.retain(|i| i.get("state").and_then(Value::as_str) == Some(state));
    }
    if let Some(rep) = representation {
        items.retain(|i| i.get("source_representation").and_then(Value::as_str) == Some(rep));
    }
    let total = items.len() as i64;
    let start = offset.max(0).min(total) as usize;
    let end = (offset.max(0).saturating_add(limit.max(0)))
        .min(total)
        .max(start as i64) as usize;
    let page: Vec<Value> = items[start..end].iter().map(|v| (*v).clone()).collect();
    let returned = page.len() as i64;
    let next_offset = if offset + returned < total {
        Value::from(offset + returned)
    } else {
        Value::Null
    };
    let mut out = Map::new();
    out.insert(
        "analysis".into(),
        evaluation.get("analysis").cloned().unwrap_or(Value::Null),
    );
    out.insert("total".into(), Value::from(total));
    out.insert("offset".into(), Value::from(offset));
    out.insert("limit".into(), Value::from(limit));
    out.insert("returned".into(), Value::from(returned));
    out.insert("next_offset".into(), next_offset);
    out.insert(
        "states".into(),
        evaluation.get("states").cloned().unwrap_or(Value::Null),
    );
    out.insert(
        "coverage".into(),
        evaluation.get("coverage").cloned().unwrap_or(Value::Null),
    );
    out.insert("occurrences".into(), Value::Array(page));
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decode_fragment_is_percent_only_not_form_decoding() {
        assert_eq!(decode_fragment("%41+b%zz"), "A+b%zz");
        assert_eq!(decode_fragment("%FF"), "\u{FFFD}");
    }

    #[test]
    fn indicated_element_follows_whatwg_order() {
        let ids: BTreeSet<String> = ["a b".to_string(), "top2".to_string()]
            .into_iter()
            .collect();
        let names: BTreeSet<String> = ["n".to_string()].into_iter().collect();
        assert_eq!(
            indicated_element("a%20b", &ids, &names),
            Some(("element_id".into(), "a b".into()))
        );
        assert_eq!(
            indicated_element("n", &ids, &names),
            Some(("anchor_name".into(), "n".into()))
        );
        assert_eq!(
            indicated_element("TOP", &ids, &names),
            Some(("top".into(), "top".into()))
        );
        assert_eq!(indicated_element("missing", &ids, &names), None);
    }

    #[test]
    fn extract_collects_fragment_anchors_and_ids_outside_templates() {
        let html = r##"<html><body><a id="x" href="#sec">s</a><template><a href="#hidden" id="hid"></a></template><a name="legacy" href="/p#q">q</a></body></html>"##;
        let ev = extract(html, "https://example.test/p");
        assert!(ev.ids.contains("x"));
        assert!(!ev.ids.contains("hid"));
        assert!(ev.names.contains("legacy"));
        let fragments: Vec<&str> = ev
            .anchors
            .iter()
            .map(|a| a["fragment"].as_str().unwrap())
            .collect();
        assert_eq!(fragments, vec!["sec", "q"]);
    }

    #[test]
    fn is_html_requires_exact_media_type() {
        assert!(is_html("text/html; charset=utf-8"));
        assert!(!is_html("application/xhtml+xml"));
        assert!(!is_html("application/nothtml"));
    }
}
