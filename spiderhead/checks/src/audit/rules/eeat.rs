//! Authorship, date and trust-page evidence (reference the reference suite, issue #823).

use std::collections::{BTreeMap, BTreeSet, HashMap};

use serde_json::{json, Map, Value};
use spiderhead_parse::py_url::urlsplit;

use super::stored::SELECTED_LINKS;
use super::{details, unquote};
use crate::audit::{norm_url, AuditContext, Page};

const CONTENT_SEGMENTS: [&str; 24] = [
    "blog",
    "blogs",
    "news",
    "article",
    "articles",
    "post",
    "posts",
    "stories",
    "press",
    "insights",
    "journal",
    "journals",
    "guide",
    "guides",
    "learn",
    "resources",
    "wiki",
    "publication",
    "publications",
    "materialy",
    "stati",
    "novosti",
    "blogi",
    "publikacii",
];

struct TrustPage {
    check: &'static str,
    kind: &'static str,
    paths: &'static [&'static str],
    anchors: &'static [&'static str],
}

const TRUST_PAGES: [TrustPage; 4] = [
    TrustPage {
        check: "MISSING_ABOUT_PAGE",
        kind: "about",
        paths: &[
            "about",
            "about-us",
            "about_us",
            "company",
            "who-we-are",
            "o-nas",
            "o-kompanii",
        ],
        anchors: &[
            "about",
            "about us",
            "our story",
            "who we are",
            "о нас",
            "о компании",
        ],
    },
    TrustPage {
        check: "MISSING_CONTACT_PAGE",
        kind: "contact",
        paths: &[
            "contact",
            "contacts",
            "contact-us",
            "contact_us",
            "kontakty",
            "kontakt",
        ],
        anchors: &[
            "contact",
            "contact us",
            "contacts",
            "get in touch",
            "контакты",
            "связаться с нами",
        ],
    },
    TrustPage {
        check: "MISSING_PRIVACY_POLICY",
        kind: "privacy",
        paths: &[
            "privacy",
            "privacy-policy",
            "privacy_policy",
            "privacy-notice",
            "personal-data",
            "politika-konfidentsialnosti",
            "politika-konfidencialnosti",
            "personalnye-dannye",
        ],
        anchors: &[
            "privacy",
            "privacy policy",
            "privacy notice",
            "политика конфиденциальности",
            "обработка персональных данных",
        ],
    },
    TrustPage {
        check: "MISSING_TERMS_PAGE",
        kind: "terms",
        paths: &[
            "terms",
            "terms-of-service",
            "terms-of-use",
            "terms-and-conditions",
            "tos",
            "oferta",
            "usloviya",
            "user-agreement",
        ],
        anchors: &[
            "terms",
            "terms of service",
            "terms of use",
            "terms and conditions",
            "user agreement",
            "оферта",
            "публичная оферта",
            "условия использования",
            "пользовательское соглашение",
        ],
    },
];

const YMYL: [(&str, &[&str]); 3] = [
    (
        "health",
        &[
            "health",
            "medical",
            "clinic",
            "symptom",
            "symptoms",
            "treatment",
            "medication",
            "therapy",
            "diagnosis",
            "pharmacy",
            "dental",
        ],
    ),
    (
        "finance",
        &[
            "finance",
            "financial",
            "insurance",
            "mortgage",
            "loan",
            "loans",
            "credit",
            "invest",
            "investing",
            "investment",
            "tax",
            "taxes",
            "banking",
            "pension",
            "debt",
        ],
    ),
    (
        "legal",
        &[
            "legal",
            "lawyer",
            "lawyers",
            "attorney",
            "attorneys",
            "lawsuit",
            "solicitor",
            "visa",
            "visas",
            "immigration",
            "divorce",
        ],
    ),
];

struct Edge {
    source: String,
    destination: String,
    anchor: String,
    position: String,
}

fn path_of(url: &str) -> String {
    urlsplit(url).map(|s| s.path).unwrap_or_default()
}

fn path_segments(url: &str) -> BTreeSet<String> {
    unquote(&path_of(url))
        .split('/')
        .map(|seg| {
            let lower = seg.to_lowercase();
            for ext in [".html", ".htm", ".php", ".aspx", ".asp", ".jsp"] {
                if let Some(stem) = lower.strip_suffix(ext) {
                    return stem.to_owned();
                }
            }
            lower
        })
        .filter(|s| !s.is_empty())
        .collect()
}

fn tokens(text: &str) -> BTreeSet<String> {
    let mut out = BTreeSet::new();
    let mut cur = String::new();
    for c in text.chars() {
        if c.is_ascii_alphabetic()
            || ('а'..='я').contains(&c)
            || ('А'..='Я').contains(&c)
            || c == 'ё'
            || c == 'Ё'
        {
            cur.push(c);
        } else if !cur.is_empty() {
            out.insert(cur.to_lowercase());
            cur.clear();
        }
    }
    if !cur.is_empty() {
        out.insert(cur.to_lowercase());
    }
    out
}

fn netloc(url: &str) -> String {
    urlsplit(url)
        .map(|s| s.netloc.to_lowercase())
        .unwrap_or_default()
}

fn site_host(ctx: &AuditContext) -> String {
    let mut counts: Vec<(String, usize)> = Vec::new();
    for p in &ctx.pages {
        let host = netloc(&p.url);
        if host.is_empty() {
            continue;
        }
        match counts.iter_mut().find(|(h, _)| *h == host) {
            Some(e) => e.1 += 1,
            None => counts.push((host, 1)),
        }
    }
    let mut best: Option<&(String, usize)> = None;
    for c in &counts {
        if best.is_none_or(|b| c.1 > b.1) {
            best = Some(c);
        }
    }
    best.map(|b| b.0.clone()).unwrap_or_default()
}

fn article_scope(page: &Page) -> Option<String> {
    if let Some(markers) = page
        .rec
        .get("trust_signals")
        .and_then(|s| s["article"].as_array())
    {
        let m: Vec<&str> = markers.iter().filter_map(Value::as_str).collect();
        if !m.is_empty() {
            return Some(format!("declared:{}", m.join(",")));
        }
    }
    let segs: Vec<String> = path_segments(&page.url)
        .into_iter()
        .filter(|s| CONTENT_SEGMENTS.contains(&s.as_str()))
        .collect();
    (!segs.is_empty()).then(|| format!("url_path:{}", segs.join(",")))
}

fn load_edges(ctx: &AuditContext) -> Vec<Edge> {
    let Some(con) = ctx.scan.as_ref() else {
        return Vec::new();
    };
    let sql = format!(
        "{SELECTED_LINKS}SELECT src.url,dst.url,l.anchor,l.position FROM l JOIN pages sp ON sp.url_id=l.source_url_id \
         JOIN urls src ON src.url_id=l.source_url_id JOIN urls dst ON dst.url_id=l.destination_url_id ORDER BY sp.page_ordinal,l.ordinal,l.link_id"
    );
    let Ok(mut stmt) = con.prepare(&sql) else {
        return Vec::new();
    };
    let rows = stmt.query_map([], |r| {
        Ok(Edge {
            source: r.get(0)?,
            destination: r.get(1)?,
            anchor: r.get(2)?,
            position: r.get(3)?,
        })
    });
    rows.map(|r| r.flatten().collect()).unwrap_or_default()
}

/// `run_eeat`: attribution, trust pages, citations and the YMYL review flag.
pub fn run_eeat(ctx: &mut AuditContext) {
    if ctx.scan.is_none() {
        return;
    }
    let scope = json!({
        "indexable_html_pages": ctx.indexable_html_pages().len(),
        "crawl_pages": ctx.pages.len(),
        "basis": "declared article markers or conventional content-path segments",
    });
    ctx.trust_evidence
        .entry("scope".to_string())
        .or_insert(scope);
    let edges = load_edges(ctx);
    check_attribution(ctx);
    check_trust_pages(ctx, &edges);
    check_citations(ctx, &edges);
    check_ymyl_review(ctx);
}

fn check_attribution(ctx: &mut AuditContext) {
    let (mut applicable, mut unmeasured) = (0, 0);
    for i in ctx.indexable_html_pages() {
        let page = ctx.pages[i].clone();
        let signals = page
            .rec
            .get("trust_signals")
            .filter(|v| v.is_object())
            .cloned();
        let Some(scope) = article_scope(&page) else {
            continue;
        };
        applicable += 1;
        let Some(signals) = signals else {
            unmeasured += 1;
            continue;
        };
        let empty = |k: &str| signals[k].as_array().is_none_or(Vec::is_empty);
        // `_signal_evidence`: the verbatim signal inventory the finding stands on.
        let mut evidence = Map::new();
        for (from, to) in [
            ("author", "author_signals"),
            ("dates", "date_signals"),
            ("article", "article_markers"),
        ] {
            evidence.insert(
                to.into(),
                signals
                    .get(from)
                    .and_then(Value::as_array)
                    .cloned()
                    .map(Value::Array)
                    .unwrap_or_else(|| Value::Array(Vec::new())),
            );
        }
        if let (Some(found), Some(parsed)) = (
            page.num("structured_data"),
            page.num("structured_data_parsed"),
        ) {
            let malformed = (found as i64 - parsed as i64).max(0);
            if malformed > 0 {
                evidence.insert("jsonld_blocks_malformed".into(), Value::from(malformed));
            }
        }
        let last_modified = page.text("last_modified");
        if !last_modified.is_empty() {
            evidence.insert(
                "last_modified_header".into(),
                Value::from(last_modified.to_owned()),
            );
        }
        if empty("author") {
            ctx.add_evidence(
                "NO_AUTHOR_BYLINE",
                Some(&page.url),
                None,
                1,
                (Vec::new(), evidence.clone()),
                details([("scope", json!(scope)), ("declared_signals", json!(0))]),
            );
        }
        if empty("dates") && last_modified.is_empty() {
            ctx.add_evidence(
                "NO_CONTENT_DATES",
                Some(&page.url),
                None,
                1,
                (Vec::new(), evidence),
                details([("scope", json!(scope)), ("declared_signals", json!(0))]),
            );
        }
    }
    let none = "no article-scope pages (no declared article markers and no content-path URL) -- the check did not apply";
    if applicable == 0 {
        ctx.skip("NO_AUTHOR_BYLINE", none);
        ctx.skip("NO_CONTENT_DATES", none);
    } else if unmeasured > 0 {
        for id in ["NO_AUTHOR_BYLINE", "NO_CONTENT_DATES"] {
            ctx.skip(id, &format!("{unmeasured} in-scope page(s) carry no trust-signal evidence -- their bodies were never measured"));
        }
    }
}

fn check_trust_pages(ctx: &mut AuditContext, edges: &[Edge]) {
    if ctx.pages.is_empty() {
        for t in &TRUST_PAGES {
            ctx.skip(
                t.check,
                "no pages in scope -- nothing was crawled or exported",
            );
        }
        return;
    }
    let host = site_host(ctx);
    let has_inlinks = ctx.scan.is_some();
    let mut trust_pages = Map::new();
    let mut by_norm: HashMap<String, Vec<usize>> = HashMap::new();
    for (i, p) in ctx.pages.iter().enumerate() {
        by_norm.entry(norm_url(&p.url)).or_default().push(i);
    }
    let representative = |list: &[usize], ctx: &AuditContext| -> usize {
        list.iter()
            .copied()
            .find(|&i| ctx.pages[i].is_2xx())
            .unwrap_or(list[0])
    };
    for t in &TRUST_PAGES {
        let mut order: HashMap<String, usize> = HashMap::new();
        let mut first: Option<usize> = None;
        let mut first_indexable: Option<(usize, usize)> = None;
        let mut has_2xx = false;
        let mut kinds: BTreeSet<&str> = BTreeSet::new();
        let mut matched: BTreeSet<String> = BTreeSet::new();
        let mut remember = |key: String,
                            page: usize,
                            kind: &'static str,
                            values: Vec<String>,
                            ctx: &AuditContext| {
            let n = order.len();
            let o = *order.entry(key).or_insert(n);
            first.get_or_insert(page);
            let p = &ctx.pages[page];
            has_2xx = has_2xx || p.is_2xx();
            if p.is_2xx() && p.is_indexable() && first_indexable.is_none_or(|(_, io)| o < io) {
                first_indexable = Some((page, o));
            }
            kinds.insert(kind);
            matched.extend(values);
        };
        for (i, p) in ctx.pages.iter().enumerate() {
            let hit: Vec<String> = path_segments(&p.url)
                .into_iter()
                .filter(|s| t.paths.contains(&s.as_str()))
                .collect();
            if !hit.is_empty() {
                remember(norm_url(&p.url), i, "url_path", hit, ctx);
            }
        }
        let mut discovered: BTreeSet<String> = BTreeSet::new();
        if has_inlinks {
            for e in edges {
                let anchor = e
                    .anchor
                    .split_whitespace()
                    .collect::<Vec<_>>()
                    .join(" ")
                    .to_lowercase();
                if anchor.is_empty()
                    || !t.anchors.contains(&anchor.as_str())
                    || e.destination.is_empty()
                    || netloc(&e.destination) != host
                {
                    continue;
                }
                let key = norm_url(&e.destination);
                match by_norm.get(&key) {
                    Some(list) => {
                        let rep = representative(list, ctx);
                        remember(key, rep, "anchor", vec![anchor], ctx);
                    }
                    None => {
                        discovered.insert(e.destination.clone());
                        if discovered.len() > 5 {
                            if let Some(max) = discovered.iter().next_back().cloned() {
                                discovered.remove(&max);
                            }
                        }
                    }
                }
            }
        }
        let rep = first_indexable.map(|(p, _)| p).or(first);
        let state = match (first, first_indexable, has_2xx, discovered.is_empty()) {
            (Some(_), Some(_), _, _) => "found_indexable",
            (Some(_), None, true, _) => "found_non_indexable",
            (Some(_), None, false, _) => "found_error",
            (None, _, _, false) => "discovered_not_crawled",
            _ => "not_discovered",
        };
        let (url, status, indexability) = match rep.map(|i| &ctx.pages[i]) {
            Some(p) => (
                Some(p.url.clone()),
                p.status_code,
                Value::from(p.indexability.clone()),
            ),
            None => (None, None, Value::Null),
        };
        let mut entry = Map::new();
        entry.insert("state".into(), state.into());
        entry.insert(
            "url".into(),
            url.clone().map(Value::from).unwrap_or(Value::Null),
        );
        entry.insert(
            "status_code".into(),
            status.map(Value::from).unwrap_or(Value::Null),
        );
        entry.insert("indexability".into(), indexability.clone());
        entry.insert(
            "evidence".into(),
            Value::Array(kinds.iter().map(|k| Value::from(*k)).collect()),
        );
        entry.insert(
            "matched".into(),
            Value::Array(matched.iter().map(|m| Value::from(m.clone())).collect()),
        );
        entry.insert(
            "discovered_not_crawled".into(),
            Value::Array(discovered.iter().map(|d| Value::from(d.clone())).collect()),
        );
        trust_pages.insert(t.kind.into(), Value::Object(entry));
        if state == "found_indexable" {
            continue;
        }
        let d = details([
            ("state", state.into()),
            ("crawl_pages", ctx.pages.len().into()),
            (
                "vocabulary",
                "whole path segments and anchor texts from a fixed EN/RU list".into(),
            ),
            ("matched", json!(matched.iter().collect::<Vec<_>>())),
            ("evidence", json!(kinds.iter().collect::<Vec<_>>())),
            ("indexability", indexability),
            (
                "discovered_not_crawled",
                json!(discovered.iter().collect::<Vec<_>>()),
            ),
        ]);
        let _ = t.kind;
        ctx.add_with(t.check, url.as_deref(), d, status, 1, None);
    }
    ctx.trust_evidence
        .insert("trust_pages".into(), Value::Object(trust_pages));
    ctx.trust_evidence.insert(
        "anchor_evidence".into(),
        Value::from(if has_inlinks { "all_inlinks" } else { "none" }),
    );
}

fn check_citations(ctx: &mut AuditContext, edges: &[Edge]) {
    let host = site_host(ctx);
    let has_inlinks = ctx.scan.is_some();
    let mut external: HashMap<String, Vec<&Edge>> = HashMap::new();
    for e in edges {
        if e.source.is_empty() || e.destination.is_empty() {
            continue;
        }
        let h = netloc(&e.destination);
        if !h.is_empty() && h != host {
            external.entry(norm_url(&e.source)).or_default().push(e);
        }
    }
    let (mut applicable, mut unmeasured, mut cited) = (0, 0, 0);
    for i in ctx.indexable_html_pages() {
        let page = ctx.pages[i].clone();
        let Some(scope) = article_scope(&page) else {
            continue;
        };
        applicable += 1;
        let rows = if has_inlinks {
            external.get(&norm_url(&page.url)).cloned()
        } else {
            None
        };
        let positioned: Vec<&&Edge> = rows
            .iter()
            .flatten()
            .filter(|r| !r.position.is_empty())
            .collect();
        if positioned
            .iter()
            .any(|r| r.position.trim().to_lowercase() == "content")
        {
            cited += 1;
            continue;
        }
        let outlinks = page.num("external_outlinks");
        if rows
            .as_ref()
            .is_some_and(|r| !r.is_empty() && positioned.len() == r.len())
        {
            let seen: BTreeSet<&str> = positioned.iter().map(|r| r.position.as_str()).collect();
            ctx.add_evidence(
                "FEW_CITATIONS",
                Some(&page.url),
                None,
                1,
                (
                    Vec::new(),
                    details([
                        ("measured", json!(true)),
                        ("citation_carrier", "link position = content".into()),
                    ]),
                ),
                details([
                    ("scope", json!(scope)),
                    ("external_links_positioned", positioned.len().into()),
                    ("content_position_links", json!(0)),
                    ("positions_seen", json!(seen)),
                ]),
            );
        } else if outlinks == Some(0.0) {
            ctx.add_evidence(
                "FEW_CITATIONS",
                Some(&page.url),
                None,
                1,
                (
                    Vec::new(),
                    details([
                        ("measured", json!(true)),
                        (
                            "citation_carrier",
                            "external outlinks (a page with none cites nothing)".into(),
                        ),
                    ]),
                ),
                details([("scope", json!(scope)), ("external_outlinks", json!(0))]),
            );
        } else {
            unmeasured += 1;
        }
    }
    ctx.trust_evidence.insert(
        "citations".into(),
        json!({
            "applicable_pages": applicable,
            "with_content_citations": cited,
            "unmeasured_pages": unmeasured,
            "position_evidence": if has_inlinks { "all_inlinks" } else { "none" },
        }),
    );
    if applicable == 0 {
        ctx.skip("FEW_CITATIONS", "no article-scope pages (no declared article markers and no content-path URL) -- the check did not apply");
    } else if unmeasured > 0 {
        ctx.skip("FEW_CITATIONS", &format!("{unmeasured} in-scope page(s) have external links whose positions were never classified -- citations unmeasured, not absent"));
    }
}

fn check_ymyl_review(ctx: &mut AuditContext) {
    let pages = ctx.indexable_html_pages();
    if pages.is_empty() {
        ctx.skip("YMYL_REVIEW_CANDIDATE", "no indexable HTML pages in scope");
        return;
    }
    for i in pages {
        let page = ctx.pages[i].clone();
        let path_tokens = tokens(&unquote(&path_of(&page.url)));
        let title_tokens = tokens(page.text("title"));
        let mut matched: BTreeMap<&str, Vec<String>> = BTreeMap::new();
        let mut in_: BTreeSet<&str> = BTreeSet::new();
        for (group, vocab) in YMYL {
            let hits: Vec<String> = path_tokens
                .union(&title_tokens)
                .filter(|t| vocab.contains(&t.as_str()))
                .cloned()
                .collect();
            if !hits.is_empty() {
                matched.insert(group, hits);
                if path_tokens.iter().any(|t| vocab.contains(&t.as_str())) {
                    in_.insert("path");
                }
                if title_tokens.iter().any(|t| vocab.contains(&t.as_str())) {
                    in_.insert("title");
                }
            }
        }
        if !matched.is_empty() {
            ctx.add(
                "YMYL_REVIEW_CANDIDATE",
                Some(&page.url),
                details([("matched", json!(matched)), ("matched_in", json!(in_)), ("rationale", "deterministic whole-word match on URL path and title; a review candidate, not a YMYL classification".into())]),
            );
        }
    }
}
