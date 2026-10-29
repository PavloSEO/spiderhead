//! Trust-evidence checks: authorship and content dates, trust pages, outbound
//! citations and the YMYL review flag (issue #823).
//!
//! Every check reports observable markup facts only. Absence inside a measured
//! page is a finding; absence the crawl never observed is an unmeasured skip,
//! never a clean pass.

use std::collections::{BTreeSet, HashMap};

use serde_json::{json, Map, Value};

use crate::context::{AddOpts, AuditContext, ContextError, PageHandle};
use crate::inlinks::shared::{all_inlink_records, host_of, rec, site_host};
use crate::normalize::{norm_url, Object};
use crate::pyvalue::truthy;
use crate::rules::shared::{decoded_path, has_column};

/// The `(url, status_code, indexability)` identity of one candidate page.
type PageSummary = (String, Option<i64>, Option<String>);

/// `_MAX_DISCOVERED_URLS`.
const MAX_DISCOVERED_URLS: usize = 5;

/// The skip reason shared by the three checks when the article scope matched
/// no page at all.
const DID_NOT_APPLY: &str = "no article-scope pages (no declared article markers and no \
content-path URL) -- the check did not apply";

/// `_CONTENT_SEGMENTS`: conventional content path segments, EN and RU.
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

/// One trust-page family: its finding, its evidence kind, and the fixed
/// EN/RU path-segment and anchor vocabulary that identifies it.
struct TrustPage {
    check: &'static str,
    kind: &'static str,
    paths: &'static [&'static str],
    anchors: &'static [&'static str],
}

/// `_TRUST_PAGES`, in declaration order — the order findings and the
/// `trust_pages` summary carry.
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

/// `_YMYL_VOCAB`, in declaration order.
const YMYL_VOCAB: [(&str, &[&str]); 3] = [
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

/// `_tokens`: lower-cased word tokens — letters and Cyrillic only, which is
/// what the reference `[a-zA-Zа-яА-ЯёЁ]+` scanner keeps.
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

/// `_path_segments`: decoded path split on `/`, one conventional extension
/// folded away per segment (`_EXTENSION_RE`), lower-cased, empties dropped.
fn path_segments(url: &str) -> Result<BTreeSet<String>, ContextError> {
    Ok(decoded_path(url)?
        .split('/')
        .map(|segment| {
            let lower = segment.to_lowercase();
            for ext in [".html", ".htm", ".php", ".aspx", ".asp", ".jsp"] {
                if let Some(stem) = lower.strip_suffix(ext) {
                    return stem.to_owned();
                }
            }
            lower
        })
        .filter(|s| !s.is_empty())
        .collect())
}

/// `_signals`: the page's trust-signal object, or `None` when never measured.
/// A non-empty *string* cell — an operator's Custom Extraction column named
/// "Trust Signals" — cannot say which family it observed, so it counts once
/// as a medium-confidence author signal; it never fabricates a date.
fn signals_of(record: &Object) -> Option<Object> {
    match record.get("trust_signals") {
        Some(Value::Object(map)) => Some(map.clone()),
        Some(Value::String(raw)) if !raw.trim().is_empty() => {
            let mut author = Map::new();
            author.insert("signal".into(), Value::from("custom_column"));
            author.insert("confidence".into(), Value::from("medium"));
            author.insert("value".into(), Value::from(raw.trim()));
            let mut out = Map::new();
            out.insert("author".into(), Value::Array(vec![Value::Object(author)]));
            out.insert("dates".into(), Value::Array(Vec::new()));
            out.insert("article".into(), Value::Array(Vec::new()));
            Some(out)
        }
        _ => None,
    }
}

/// `_article_scope`: why the page is inside the authorship scope. `declared:`
/// names the markup markers the page carries; `url_path:` the conventional
/// content segments its URL sits under. Outside both, the page stays silent.
fn article_scope(
    page: &PageHandle,
    signals: Option<&Object>,
) -> Result<Option<String>, ContextError> {
    if let Some(markers) = signals
        .and_then(|s| s.get("article"))
        .and_then(Value::as_array)
    {
        let named: Vec<&str> = markers.iter().filter_map(Value::as_str).collect();
        if !named.is_empty() {
            return Ok(Some(format!("declared:{}", named.join(","))));
        }
    }
    let segments: Vec<String> = path_segments(&page.url())?
        .into_iter()
        .filter(|s| CONTENT_SEGMENTS.contains(&s.as_str()))
        .collect();
    Ok((!segments.is_empty()).then(|| format!("url_path:{}", segments.join(","))))
}

/// `_invalid_jsonld_blocks`: JSON-LD blocks found minus parsed — the malformed
/// count, when both columns exist. An export carrying only "Structured Data"
/// cannot state it.
fn invalid_jsonld_blocks(record: &Object) -> Option<i64> {
    let found = record.get("structured_data")?.as_f64()?;
    let parsed = record.get("structured_data_parsed")?.as_f64()?;
    Some(((found as i64) - (parsed as i64)).max(0))
}

/// reference `str(value)` for the cell shapes the citation checks quote.
fn py_str(value: &Value) -> String {
    match value {
        Value::String(s) => s.clone(),
        other => other.to_string(),
    }
}

/// `_signal_evidence`: the verbatim signal inventory a finding stands on.
fn signal_evidence(signals: &Object, record: &Object) -> Object {
    let mut evidence = Map::new();
    for (from, to) in [
        ("author", "author_signals"),
        ("dates", "date_signals"),
        ("article", "article_markers"),
    ] {
        let items = signals
            .get(from)
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();
        evidence.insert(to.to_owned(), Value::Array(items));
    }
    if let Some(malformed) = invalid_jsonld_blocks(record).filter(|count| *count != 0) {
        evidence.insert("jsonld_blocks_malformed".into(), Value::from(malformed));
    }
    if record.get("last_modified").is_some_and(truthy) {
        evidence.insert(
            "last_modified_header".into(),
            record.get("last_modified").cloned().unwrap_or(Value::Null),
        );
    }
    evidence
}

/// Where the complete link evidence comes from. `Graph` is the stored graph
/// reader (reference `_StoredInlinks`) — always truthy even when it iterates zero
/// rows; `Export` is `_all_inlink_records()`, constructed only when non-empty;
/// `Absent` is reference `None`. `is_present` therefore answers both reference
/// questions: `inlinks is not None` and `if inlinks:`.
enum EvidenceLinks {
    Absent,
    Graph(Vec<Object>),
    Export(Vec<Object>),
}

impl EvidenceLinks {
    fn is_present(&self) -> bool {
        !matches!(self, EvidenceLinks::Absent)
    }

    fn rows(&self) -> &[Object] {
        match self {
            EvidenceLinks::Absent => &[],
            EvidenceLinks::Graph(rows) | EvidenceLinks::Export(rows) => rows,
        }
    }
}

/// `_inlinks`: the stored graph when the audit carries one, else the
/// `all_inlinks` export (absent or empty counts as absent).
fn evidence_links(ctx: &AuditContext) -> EvidenceLinks {
    if let Some(graph) = ctx.graph_access.as_deref() {
        return EvidenceLinks::Graph(graph.iter_evidence_links().collect());
    }
    match all_inlink_records(ctx) {
        Some(rows) => EvidenceLinks::Export(rows),
        None => EvidenceLinks::Absent,
    }
}

/// `check_attribution`: NO_AUTHOR_BYLINE / NO_CONTENT_DATES — objective markup
/// presence. Fires only inside the article scope, only when the signal column
/// exists, and reports the signals it consulted. Presence is evidence, never a
/// trustworthiness verdict; absence on a page is "no declared markup", which
/// is what the finding says.
pub fn check_attribution(ctx: &mut AuditContext) -> Result<(), ContextError> {
    let has_signals = has_column(ctx, "trust_signals");
    let has_lastmod = has_column(ctx, "last_modified");
    if !has_signals {
        ctx.skip(
            "NO_AUTHOR_BYLINE",
            "no Trust Signals column (native crawl or a same-named custom \
             extraction) -- no author markup evidence at all",
        );
    }
    if !has_signals && !has_lastmod {
        ctx.skip(
            "NO_CONTENT_DATES",
            "no Trust Signals column and no Last Modified column -- no date evidence at all",
        );
        return Ok(());
    }
    let mut applicable = 0;
    let mut unmeasured = 0;
    for page in ctx.indexable_html_pages() {
        let record = rec(&page);
        let signals = signals_of(&record);
        let Some(scope) = article_scope(&page, signals.as_ref())? else {
            continue;
        };
        applicable += 1;
        if has_signals && signals.is_none() {
            // The column exists but this row holds none: an unparsed body,
            // never a measured absence. Counted, not passed as clean.
            unmeasured += 1;
            continue;
        }
        let signals = signals.unwrap_or_else(|| {
            json!({"author": [], "dates": [], "article": []})
                .as_object()
                .cloned()
                .unwrap_or_default()
        });
        let mut details = Map::new();
        details.insert("scope".into(), Value::from(scope));
        details.insert("declared_signals".into(), Value::from(0));
        let evidence = signal_evidence(&signals, &record);
        if has_signals && !signals.get("author").is_some_and(truthy) {
            ctx.add(
                "NO_AUTHOR_BYLINE",
                AddOpts {
                    target_url: Some(page.url()),
                    details: details.clone(),
                    evidence: evidence.clone(),
                    ..Default::default()
                },
            )?;
        }
        if !signals.get("dates").is_some_and(truthy)
            && !record.get("last_modified").is_some_and(truthy)
        {
            ctx.add(
                "NO_CONTENT_DATES",
                AddOpts {
                    target_url: Some(page.url()),
                    details,
                    evidence,
                    ..Default::default()
                },
            )?;
        }
    }
    if applicable == 0 {
        // Nothing evaluated is not a clean result: the scope never applied,
        // which the run must say rather than count as silent coverage.
        if has_signals {
            ctx.skip("NO_AUTHOR_BYLINE", DID_NOT_APPLY);
        }
        ctx.skip("NO_CONTENT_DATES", DID_NOT_APPLY);
    } else if unmeasured > 0 {
        for check_id in ["NO_AUTHOR_BYLINE", "NO_CONTENT_DATES"] {
            ctx.skip(
                check_id,
                &format!(
                    "{unmeasured} in-scope page(s) carry no trust-signal evidence \
                     -- their bodies were never measured"
                ),
            );
        }
    }
    Ok(())
}

/// `check_trust_pages`: MISSING_{ABOUT,CONTACT,PRIVACY,TERMS} — discovery and
/// index state. "Missing" is never the raw absence of a match: each finding
/// names one of found_non_indexable / found_error / discovered_not_crawled /
/// not_discovered, and the matched pages or anchors are quoted back so a
/// reviewer can see exactly what the vocabulary caught. The same states are
/// mirrored into `ctx.trust_evidence` for the audit summary.
pub fn check_trust_pages(ctx: &mut AuditContext) -> Result<(), ContextError> {
    if ctx.pages_len() == 0 {
        for cfg in &TRUST_PAGES {
            ctx.skip(
                cfg.check,
                "no pages in scope -- nothing was crawled or exported",
            );
        }
        return Ok(());
    }
    let host = site_host(ctx);
    let inlinks = evidence_links(ctx);
    let mut trust_pages = Map::new();
    for cfg in &TRUST_PAGES {
        // Candidate group order chooses the first representative, including a
        // later indexable spelling of an earlier normalized URL. Keep that
        // scalar order, but never retain the groups' full Page objects.
        let mut candidate_order: HashMap<String, usize> = HashMap::new();
        let mut first: Option<PageSummary> = None;
        let mut first_indexable: Option<PageSummary> = None;
        let mut indexable_order: Option<usize> = None;
        let mut has_2xx = false;
        let mut evidence_kinds: BTreeSet<String> = BTreeSet::new();
        let mut matched: BTreeSet<String> = BTreeSet::new();
        let mut remember =
            |key: String, page: &PageHandle, evidence_kind: &str, matched_values: &[String]| {
                let next = candidate_order.len();
                let order = *candidate_order.entry(key).or_insert(next);
                let summary = page.map(|p| (p.url.clone(), p.status_code, p.indexability.clone()));
                if first.is_none() {
                    first = Some(summary.clone());
                }
                let (is_2xx, is_indexable) = page.map(|p| (p.is_2xx(), p.is_indexable()));
                has_2xx = has_2xx || is_2xx;
                if is_2xx && is_indexable && indexable_order.is_none_or(|seen| order < seen) {
                    first_indexable = Some(summary);
                    indexable_order = Some(order);
                }
                evidence_kinds.insert(evidence_kind.to_owned());
                matched.extend(matched_values.iter().cloned());
            };
        for page in ctx.pages_iter() {
            let matched_paths: Vec<String> = path_segments(&page.url())?
                .into_iter()
                .filter(|s| cfg.paths.contains(&s.as_str()))
                .collect();
            if !matched_paths.is_empty() {
                remember(
                    norm_url(Some(&page.url()))?,
                    &page,
                    "url_path",
                    &matched_paths,
                );
            }
        }
        // Preserve exactly the existing lexicographically first five URL
        // examples, without retaining all other targets.
        let mut discovered_only: BTreeSet<String> = BTreeSet::new();
        if inlinks.is_present() {
            for item in inlinks.rows() {
                let anchor_raw = item
                    .get("anchor")
                    .filter(|v| truthy(v))
                    .and_then(Value::as_str)
                    .unwrap_or("");
                let anchor = anchor_raw.split_whitespace().collect::<Vec<_>>().join(" ");
                let anchor = anchor.to_lowercase();
                let Some(destination) = item
                    .get("destination_url")
                    .filter(|v| truthy(v))
                    .and_then(Value::as_str)
                else {
                    continue;
                };
                if anchor.is_empty() || !cfg.anchors.contains(&anchor.as_str()) {
                    continue;
                }
                if host_of(destination) != host {
                    continue;
                }
                let key = norm_url(Some(destination))?;
                match ctx.page_by_norm(&key) {
                    Some(page) => {
                        remember(key, &page, "anchor", std::slice::from_ref(&anchor));
                    }
                    None => {
                        discovered_only.insert(destination.to_owned());
                        if discovered_only.len() > MAX_DISCOVERED_URLS {
                            if let Some(largest) = discovered_only.iter().next_back().cloned() {
                                discovered_only.remove(&largest);
                            }
                        }
                    }
                }
            }
        }
        let mut state = "not_discovered";
        if first.is_some() {
            if first_indexable.is_some() {
                state = "found_indexable";
            } else if has_2xx {
                state = "found_non_indexable";
            } else {
                state = "found_error";
            }
        } else if !discovered_only.is_empty() {
            state = "discovered_not_crawled";
        }
        let (representative_url, representative_status, representative_indexability) =
            first_indexable
                .or(first)
                .map_or((None, None, None), |(url, status, indexability)| {
                    (Some(url), status, indexability)
                });
        let evidence: Vec<Value> = evidence_kinds
            .iter()
            .map(|s| Value::String(s.clone()))
            .collect();
        let matched_values: Vec<Value> = matched.iter().map(|s| Value::String(s.clone())).collect();
        let discovered: Vec<Value> = discovered_only
            .iter()
            .map(|s| Value::String(s.clone()))
            .collect();
        let mut entry = Map::new();
        entry.insert("state".into(), Value::from(state));
        entry.insert(
            "url".into(),
            representative_url
                .clone()
                .map(Value::from)
                .unwrap_or(Value::Null),
        );
        entry.insert(
            "status_code".into(),
            representative_status
                .map(Value::from)
                .unwrap_or(Value::Null),
        );
        entry.insert(
            "indexability".into(),
            representative_indexability
                .clone()
                .map(Value::from)
                .unwrap_or(Value::Null),
        );
        entry.insert("evidence".into(), Value::Array(evidence.clone()));
        entry.insert("matched".into(), Value::Array(matched_values.clone()));
        entry.insert(
            "discovered_not_crawled".into(),
            Value::Array(discovered.clone()),
        );
        trust_pages.insert(cfg.kind.to_owned(), Value::Object(entry));
        if state == "found_indexable" {
            continue;
        }
        let mut details = Map::new();
        details.insert("state".into(), Value::from(state));
        details.insert("crawl_pages".into(), Value::from(ctx.pages_len()));
        details.insert(
            "vocabulary".into(),
            Value::from("whole path segments and anchor texts from a fixed EN/RU list"),
        );
        details.insert("matched".into(), Value::Array(matched_values));
        details.insert("evidence".into(), Value::Array(evidence));
        details.insert(
            "indexability".into(),
            representative_indexability
                .map(Value::from)
                .unwrap_or(Value::Null),
        );
        details.insert("discovered_not_crawled".into(), Value::Array(discovered));
        ctx.add(
            cfg.check,
            AddOpts {
                target_url: representative_url,
                status_code: representative_status,
                details,
                ..Default::default()
            },
        )?;
    }
    ctx.trust_evidence
        .insert("trust_pages".into(), Value::Object(trust_pages));
    ctx.trust_evidence.insert(
        "anchor_evidence".into(),
        Value::from(if inlinks.is_present() {
            "all_inlinks"
        } else {
            "none"
        }),
    );
    Ok(())
}

/// `check_citations`: FEW_CITATIONS — observable outbound-reference facts
/// only. A citation claim needs link positions: an outbound link inside the
/// body content is the only carrier that reads as a citation. When positions
/// were never classified the check cannot tell a content link from a footer
/// link, so those pages are counted unmeasured. The two facts that do stand
/// on their own — the page declares zero external outlinks, or every
/// positioned external link sits outside the content — are the only two it
/// reports.
pub fn check_citations(ctx: &mut AuditContext) -> Result<(), ContextError> {
    let has_outlinks = has_column(ctx, "external_outlinks");
    let inlinks = evidence_links(ctx);
    if !has_outlinks && !inlinks.is_present() {
        ctx.skip(
            "FEW_CITATIONS",
            "no External Outlinks column and no all_inlinks export -- \
             nothing to count outbound citations from",
        );
        return Ok(());
    }
    let host = site_host(ctx);
    let mut external_by_source: HashMap<String, Vec<Object>> = HashMap::new();
    if inlinks.is_present() {
        for item in inlinks.rows() {
            let (Some(source), Some(destination)) = (
                item.get("source_url")
                    .filter(|v| truthy(v))
                    .and_then(Value::as_str),
                item.get("destination_url")
                    .filter(|v| truthy(v))
                    .and_then(Value::as_str),
            ) else {
                continue;
            };
            let dest_host = host_of(destination);
            if !dest_host.is_empty() && dest_host != host {
                external_by_source
                    .entry(norm_url(Some(source))?)
                    .or_default()
                    .push(item.clone());
            }
        }
    }
    let mut applicable = 0;
    let mut unmeasured = 0;
    let mut cited = 0;
    for page in ctx.indexable_html_pages() {
        let record = rec(&page);
        let signals = signals_of(&record);
        let Some(scope) = article_scope(&page, signals.as_ref())? else {
            continue;
        };
        applicable += 1;
        let rows = if inlinks.is_present() {
            external_by_source.get(&norm_url(Some(&page.url()))?)
        } else {
            None
        };
        let positioned: Vec<&Object> = rows
            .into_iter()
            .flatten()
            .filter(|row| row.get("link_position").is_some_and(truthy))
            .collect();
        let content_cites = positioned.iter().any(|row| {
            row.get("link_position")
                .is_some_and(|v| py_str(v).trim().to_lowercase() == "content")
        });
        if content_cites {
            cited += 1;
            continue;
        }
        let outlinks = if has_outlinks {
            record.get("external_outlinks")
        } else {
            None
        };
        let all_positioned =
            matches!(rows, Some(rows) if !rows.is_empty() && positioned.len() == rows.len());
        if all_positioned {
            let positions_seen: BTreeSet<String> = positioned
                .iter()
                .filter_map(|row| row.get("link_position").map(py_str))
                .collect();
            let mut details = Map::new();
            details.insert("scope".into(), Value::from(scope));
            details.insert(
                "external_links_positioned".into(),
                Value::from(positioned.len() as i64),
            );
            details.insert("content_position_links".into(), Value::from(0));
            details.insert(
                "positions_seen".into(),
                Value::Array(
                    positions_seen
                        .iter()
                        .map(|s| Value::String(s.clone()))
                        .collect(),
                ),
            );
            let mut evidence = Map::new();
            evidence.insert("measured".into(), Value::from(true));
            evidence.insert(
                "citation_carrier".into(),
                Value::from("link position = content"),
            );
            ctx.add(
                "FEW_CITATIONS",
                AddOpts {
                    target_url: Some(page.url()),
                    details,
                    evidence,
                    ..Default::default()
                },
            )?;
        } else if outlinks.and_then(Value::as_f64) == Some(0.0) {
            let mut details = Map::new();
            details.insert("scope".into(), Value::from(scope));
            details.insert("external_outlinks".into(), Value::from(0));
            let mut evidence = Map::new();
            evidence.insert("measured".into(), Value::from(true));
            evidence.insert(
                "citation_carrier".into(),
                Value::from("external outlinks (a page with none cites nothing)"),
            );
            ctx.add(
                "FEW_CITATIONS",
                AddOpts {
                    target_url: Some(page.url()),
                    details,
                    evidence,
                    ..Default::default()
                },
            )?;
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
            "position_evidence": if inlinks.is_present() { "all_inlinks" } else { "none" },
        }),
    );
    if applicable == 0 {
        ctx.skip("FEW_CITATIONS", DID_NOT_APPLY);
    } else if unmeasured > 0 {
        ctx.skip(
            "FEW_CITATIONS",
            &format!(
                "{unmeasured} in-scope page(s) have external links whose positions \
                 were never classified -- citations unmeasured, not absent"
            ),
        );
    }
    Ok(())
}

/// `check_ymyl_review`: YMYL_REVIEW_CANDIDATE — a keyword flag for human
/// review only. Deterministic whole-word matches on the decoded path and the
/// title name candidate pages a specialist may want to check with the trust
/// signals above. It is deliberately not a classifier: the finding's own
/// message says so, and its details quote the matched terms so a reviewer can
/// discard a false positive in one glance. No model is consulted.
pub fn check_ymyl_review(ctx: &mut AuditContext) -> Result<(), ContextError> {
    let pages = ctx.indexable_html_pages();
    if pages.is_empty() {
        ctx.skip("YMYL_REVIEW_CANDIDATE", "no indexable HTML pages in scope");
        return Ok(());
    }
    for page in pages {
        let record = rec(&page);
        let path_tokens = tokens(&decoded_path(&page.url())?);
        let title_raw = record
            .get("title")
            .filter(|v| truthy(v))
            .and_then(Value::as_str)
            .unwrap_or("");
        let title_tokens = tokens(title_raw);
        let mut matched: Vec<(&str, Vec<String>)> = Vec::new();
        let mut matched_in: BTreeSet<&str> = BTreeSet::new();
        for (group, vocab) in YMYL_VOCAB {
            let hits: Vec<String> = path_tokens
                .union(&title_tokens)
                .filter(|token| vocab.contains(&token.as_str()))
                .cloned()
                .collect();
            if hits.is_empty() {
                continue;
            }
            if path_tokens
                .iter()
                .any(|token| vocab.contains(&token.as_str()))
            {
                matched_in.insert("path");
            }
            if title_tokens
                .iter()
                .any(|token| vocab.contains(&token.as_str()))
            {
                matched_in.insert("title");
            }
            matched.push((group, hits));
        }
        if matched.is_empty() {
            continue;
        }
        // Insertion order = vocabulary order, as in the reference dict.
        let mut matched_json = Map::new();
        for (group, hits) in &matched {
            matched_json.insert(
                (*group).to_owned(),
                Value::Array(hits.iter().map(|s| Value::String(s.clone())).collect()),
            );
        }
        let mut details = Map::new();
        details.insert("matched".into(), Value::Object(matched_json));
        details.insert(
            "matched_in".into(),
            Value::Array(matched_in.iter().map(|s| Value::from(*s)).collect()),
        );
        details.insert(
            "rationale".into(),
            Value::from(
                "deterministic whole-word match on URL path and title; a review \
                 candidate, not a YMYL classification",
            ),
        );
        ctx.add(
            "YMYL_REVIEW_CANDIDATE",
            AddOpts {
                target_url: Some(page.url()),
                details,
                ..Default::default()
            },
        )?;
    }
    Ok(())
}

/// `run_eeat`: the issue-#823 evidence pass — runs beside run_rules/run_inlinks.
pub fn run_eeat(ctx: &mut AuditContext) -> Result<(), ContextError> {
    if ctx.internal_df.is_none() {
        return Ok(());
    }
    if !ctx.trust_evidence.contains_key("scope") {
        let scope = json!({
            "indexable_html_pages": ctx.indexable_html_pages().len(),
            "crawl_pages": ctx.pages_len(),
            "basis": "declared article markers or conventional content-path segments",
        });
        ctx.trust_evidence.insert("scope".into(), scope);
    }
    check_attribution(ctx)?;
    check_trust_pages(ctx)?;
    check_citations(ctx)?;
    check_ymyl_review(ctx)
}
