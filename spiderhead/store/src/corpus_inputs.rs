//! Read bounded duplicate/boilerplate corpus inputs from one validated scan snapshot.
//!
//! Raw bodies never leave this module: the analyzers (Markdown extraction, text normalisation,
//! boilerplate hashing) run here and only their private inputs plus public provenance come back.

use std::collections::BTreeMap;
use std::path::Path;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use rusqlite::Connection;
use serde_json::{json, Map, Value};
use spiderhead_core::artifacts::SCAN_V1;
use spiderhead_parse::types::ContentAreaConfig;

use crate::discovery_ledger::query_maps;
use crate::error::StoreError;
use crate::scan_artifact::{open_scan, READ_TIMEOUT_SECONDS};
use crate::url_query::is_interrupt;

/// Most documents one corpus read prepares.
pub const MAX_CORPUS_DOCUMENTS: i64 = 10000;
/// Most analyzer input bytes one corpus read keeps.
pub const MAX_CORPUS_INPUT_BYTES: i64 = 16777216;
/// Largest decoded document the corpus reader accepts.
pub const MAX_DOCUMENT_BYTES: i64 = 5242880;
/// Most shingles a duplicate comparison may hold.
pub const MAX_DUPLICATE_SHINGLES: i64 = 1000000;
/// Most candidate comparisons a duplicate pass may make.
pub const MAX_DUPLICATE_CANDIDATE_COMPARISONS: i64 = 250000;

/// Decoder version recorded with every corpus identity.
const DECODER_VERSION: &str = "scan_decoder.v1";

fn invalid(message: impl Into<String>) -> StoreError {
    StoreError::Invalid(message.into())
}

/// Implementation identity of each corpus kind: the file
/// names and the SHA-256 of their bytes, pinned like the scan header's
/// `writer_revision` (same policy as `content_evidence::implementation`).
const ORACLE_IDENTITY: [(&str, &str, &str); 3] = [
    (
        "duplicate",
        "62daf0a5baa4c4f0bf84a27a50ab81365423fcff5937b079e7149a039c32bf1e",
        "[\"storage/corpus_inputs.py\",\"storage/bodies.py\",\"crawl/evidence.py\",\"checks/duplicate.py\",\"checks/markdown_extract.py\",\"checks/content_area.py\"]",
    ),
    (
        "boilerplate",
        "387c693b42feec1b3f6296af56f30eab4e7dc4d10d575efd9f416d7ccf5d976c",
        "[\"storage/corpus_inputs.py\",\"storage/bodies.py\",\"crawl/evidence.py\",\"checks/boilerplate_report.py\"]",
    ),
    (
        "semantic",
        "f43be33ab772b911ec40369840d008272fb3858fa16512302e563efdb488d047",
        "[\"storage/corpus_inputs.py\",\"storage/bodies.py\",\"crawl/evidence.py\",\"checks/content_area.py\",\"checks/text_normalize.py\"]",
    ),
];

/// Name the current byte-to-input implementation, separately from the capture
/// writer: its provenance is pinned to recorded hashes.
pub fn implementation_identity(kind: &str) -> Result<Map<String, Value>, StoreError> {
    let pinned = ORACLE_IDENTITY
        .iter()
        .find(|(name, _, _)| *name == kind)
        .ok_or_else(|| invalid("unsupported scan corpus kind"))?;
    let files: Vec<Value> = serde_json::from_str(pinned.2).unwrap_or_default();
    let mut out = Map::new();
    out.insert("source_sha256".into(), json!(pinned.1));
    out.insert("files".into(), Value::Array(files));
    out.insert("decoder_version".into(), json!(DECODER_VERSION));
    Ok(out)
}

fn robots_tokens(values: &[&str]) -> Vec<String> {
    const VALUED: [&str; 4] = [
        "max-snippet",
        "max-image-preview",
        "max-video-preview",
        "unavailable_after",
    ];
    let mut tokens: Vec<String> = Vec::new();
    for value in values {
        for raw in value.replace(';', ",").split(',') {
            let mut token = raw.trim().to_lowercase();
            if token.contains(':') && !VALUED.iter().any(|v| token.starts_with(v)) {
                let (agent, rest) = token.split_once(':').unwrap_or(("", ""));
                if !["googlebot", "googlebot-news"].contains(&agent.trim()) {
                    continue;
                }
                token = rest.trim().to_string();
            }
            if token.is_empty() {
                continue;
            }
            if token == "none" {
                tokens.push("noindex".into());
                tokens.push("nofollow".into());
            }
            tokens.push(token);
        }
    }
    tokens
}

fn text_of(page: &Map<String, Value>, key: &str) -> String {
    match page.get(key) {
        Some(Value::String(s)) => s.clone(),
        _ => String::new(),
    }
}

/// Use the crawler's one indexability policy rather than a scan-only shortcut.
pub fn indexable(page: &Map<String, Value>, blocked: bool) -> bool {
    if blocked {
        return false;
    }
    let status = page.get("status_code").and_then(Value::as_i64);
    // no status (with or without an error) is "Response unavailable"
    let Some(code) = status else { return false };
    if code >= 300 {
        return false;
    }
    let tokens = robots_tokens(&[&text_of(page, "meta_robots"), &text_of(page, "x_robots")]);
    if tokens.iter().any(|t| t == "noindex") {
        return false;
    }
    let canonical = text_of(page, "canonical");
    if !canonical.is_empty() {
        let (a, b) = (
            same_page_key(&text_of(page, "url")),
            same_page_key(&canonical),
        );
        if a != b {
            return false;
        }
    }
    true
}

/// `(scheme, netloc, path without trailing slash, query, fragment)` for same-page checks.
fn same_page_key(url: &str) -> (String, String, String, String, String) {
    let parts = crate::ledger::urlparts::urlsplit(url);
    (
        parts.scheme.to_lowercase(),
        parts.netloc.to_lowercase(),
        parts.path.trim_end_matches('/').to_string(),
        parts.query,
        parts.fragment,
    )
}

/// Public provenance of one corpus read.
pub fn source(
    header: &Map<String, Value>,
    kind: &str,
    representations: &BTreeMap<String, i64>,
) -> Result<Map<String, Value>, StoreError> {
    let get = |k: &str| header.get(k).cloned().unwrap_or(Value::Null);
    let capabilities: Value = serde_json::from_str(
        header
            .get("capabilities_json")
            .and_then(Value::as_str)
            .unwrap_or("null"),
    )?;
    let html_bodies = capabilities
        .get("html_bodies")
        .cloned()
        .ok_or_else(|| invalid("scan capabilities lack html_bodies"))?;
    let flag = |k: &str| json!(crate::ledger::ingest::truthy(&get(k)));
    let mut out = Map::new();
    out.insert("kind".into(), json!(SCAN_V1));
    for k in [
        "scan_uuid",
        "evidence_revision",
        "writer_revision",
        "config_fingerprint",
        "lifecycle",
        "finish_reason",
    ] {
        out.insert(k.into(), get(k));
    }
    out.insert("crawl_partial".into(), flag("crawl_partial"));
    out.insert("corpus_partial".into(), flag("corpus_partial"));
    out.insert("html_body_capability".into(), html_bodies);
    out.insert(
        "representations".into(),
        Value::Object(
            representations
                .iter()
                .map(|(k, v)| (k.clone(), json!(v)))
                .collect(),
        ),
    );
    out.insert(
        "implementation".into(),
        Value::Object(implementation_identity(kind)?),
    );
    Ok(out)
}

/// Coverage block of a corpus read.
pub fn coverage(
    eligible: i64,
    prepared: i64,
    omitted: &BTreeMap<String, i64>,
    unavailable: &str,
    partial_reasons: Option<&[String]>,
    measured_empty: i64,
) -> Map<String, Value> {
    let mut reasons: Vec<String> = partial_reasons.map(<[String]>::to_vec).unwrap_or_default();
    let mut unavailable = unavailable.to_string();
    if !unavailable.is_empty() {
        reasons.insert(0, unavailable.clone());
    } else if eligible > 0 && prepared == 0 {
        unavailable = "no retained supported page bodies are available".into();
        reasons.insert(0, unavailable.clone());
    }
    let state = if !unavailable.is_empty() {
        "unavailable"
    } else if !omitted.is_empty() || !reasons.is_empty() {
        "partial"
    } else {
        "complete"
    };
    let mut out = Map::new();
    out.insert("state".into(), json!(state));
    out.insert("reason".into(), json!(reasons.join("; ")));
    out.insert("eligible_documents".into(), json!(eligible));
    out.insert("prepared_documents".into(), json!(prepared));
    out.insert("analyzed_documents".into(), json!(0));
    out.insert("measured_empty_documents".into(), json!(measured_empty));
    out.insert(
        "omitted_documents".into(),
        json!(omitted.values().sum::<i64>()),
    );
    out.insert(
        "omission_reasons".into(),
        Value::Object(omitted.iter().map(|(k, v)| (k.clone(), json!(v))).collect()),
    );
    out.insert("input_byte_limit".into(), json!(MAX_CORPUS_INPUT_BYTES));
    out.insert("document_limit".into(), json!(MAX_CORPUS_DOCUMENTS));
    out
}

/// Tell a progress-handler interrupt apart from other statement failures.
pub fn read_interrupted(exc: &StoreError) -> bool {
    matches!(exc, StoreError::Sqlite(e) if is_interrupt(e))
}

/// Thin wrapper over `bodies::read_document`, which returns a
/// `Result` once filled.
fn read_document(con: &Connection, document_id: i64) -> Result<String, StoreError> {
    crate::bodies::read_document(con, document_id, MAX_DOCUMENT_BYTES)
}

fn content_area(config: &Value) -> Result<Option<ContentAreaConfig>, StoreError> {
    match config.get("content_area") {
        None | Some(Value::Null) => Ok(None),
        Some(v) => Ok(Some(serde_json::from_value(v.clone())?)),
    }
}

fn parse_failed(e: spiderhead_parse::ParseError) -> StoreError {
    invalid(e.to_string())
}

/// Return private analyzer input plus public provenance; raw bodies never leave this module.
pub fn scan_corpus(scan: &str, kind: &str) -> Result<Map<String, Value>, StoreError> {
    if !["duplicate", "boilerplate", "semantic"].contains(&kind) {
        return Err(invalid("unsupported scan corpus kind"));
    }
    if scan.is_empty() {
        return Err(invalid("scan must name a scan.v1 SQLite artifact"));
    }
    if !Path::new(scan).is_file() {
        return Err(invalid(format!("scan does not exist: {scan}")));
    }
    let con = open_scan(Path::new(scan), false, None)?;
    // open_scan's deadline covers its validation pass; corpus reading instead re-budgets each
    // document so total wall clock no longer scales with the corpus, while one pathological
    // document read still gets cancelled (issue #710).
    let budget = Duration::from_secs(READ_TIMEOUT_SECONDS as u64);
    let deadline = Arc::new(Mutex::new(Instant::now() + budget));
    let watched = Arc::clone(&deadline);
    con.progress_handler(
        10_000,
        Some(move || watched.lock().map(|d| Instant::now() > *d).unwrap_or(true)),
    );
    let result = read_corpus(&con, kind, &deadline, budget);
    con.progress_handler(0, None::<fn() -> bool>);
    result
}

fn read_corpus(
    con: &Connection,
    kind: &str,
    deadline: &Arc<Mutex<Instant>>,
    budget: Duration,
) -> Result<Map<String, Value>, StoreError> {
    let header = query_maps(con, "SELECT * FROM scan WHERE singleton=1", [])?
        .into_iter()
        .next()
        .ok_or_else(|| invalid("scan has no header"))?;
    let config: Value = serde_json::from_str(
        header
            .get("config_json")
            .and_then(Value::as_str)
            .unwrap_or("null"),
    )?;
    let eligible: i64 = con.query_row("SELECT COUNT(*) FROM pages", [], |r| r.get(0))?;
    let mut omitted: BTreeMap<String, i64> = BTreeMap::new();
    let mut representations: BTreeMap<String, i64> = BTreeMap::new();
    let mut partial_reasons: Vec<String> = Vec::new();
    if header
        .get("crawl_partial")
        .is_some_and(crate::ledger::ingest::truthy)
    {
        partial_reasons.push("scan crawl is partial".into());
    }
    let lifecycle = header
        .get("lifecycle")
        .and_then(Value::as_str)
        .unwrap_or("");
    if lifecycle != "finished" {
        partial_reasons.push(format!("scan lifecycle is {lifecycle}"));
    }
    let capabilities: Value = serde_json::from_str(
        header
            .get("capabilities_json")
            .and_then(Value::as_str)
            .unwrap_or("null"),
    )?;
    let lane = capabilities
        .get("html_bodies")
        .ok_or_else(|| invalid("scan capabilities lack html_bodies"))?;
    if lane["state"] != "complete" {
        partial_reasons.push(format!(
            "HTML body lane is {}: {}",
            lane["state"].as_str().unwrap_or(""),
            lane["reason"].as_str().unwrap_or("")
        ));
    }
    let area = content_area(&config)?;
    let mut items: Vec<Value> = Vec::new();
    let mut input_bytes: i64 = 0;
    let mut measured_empty: i64 = 0;
    let mut stop_reason = String::new();
    let mut page_body = |page: Map<String, Value>| -> Result<bool, StoreError> {
        if items.len() as i64 >= MAX_CORPUS_DOCUMENTS {
            stop_reason = "scan corpus document limit exceeded".into();
            return Ok(false);
        }
        if let Ok(mut d) = deadline.lock() {
            *d = Instant::now() + budget;
        }
        *representations
            .entry(text_of(&page, "representation"))
            .or_insert(0) += 1;
        let Some(document_id) = page.get("document_id").and_then(Value::as_i64) else {
            *omitted.entry("document_not_recorded".into()).or_insert(0) += 1;
            return Ok(true);
        };
        if !text_of(&page, "content_type")
            .to_lowercase()
            .contains("html")
        {
            *omitted.entry("unsupported_format".into()).or_insert(0) += 1;
            return Ok(true);
        }
        let html = match read_document(con, document_id) {
            Ok(h) => h,
            Err(StoreError::ScanError(e)) => {
                *omitted.entry(e.message).or_insert(0) += 1;
                return Ok(true);
            }
            Err(e) if read_interrupted(&e) => {
                stop_reason = "scan corpus read exceeded the per-document read budget".into();
                return Ok(false);
            }
            Err(e) => return Err(e),
        };
        let url = text_of(&page, "url");
        let blocked = page
            .get("robots_blocked")
            .is_some_and(crate::ledger::ingest::truthy);
        let (value, size, prepared): (String, i64, Option<Map<String, Value>>) = match kind {
            "duplicate" => {
                let markdown =
                    spiderhead_parse::markdown_extract::extract_markdown(&html, area.as_ref())
                        .map_err(parse_failed)?
                        .content_markdown;
                if markdown.trim().is_empty() {
                    measured_empty += 1;
                }
                // The retained analyzer input is the extracted Markdown.
                let size = markdown.len() as i64;
                (markdown, size, None)
            }
            "semantic" => {
                // The retained analyzer input is normalized content-area text with its own
                // provenance: the verified body hash is the corpus's evidence identity, the
                // input hash is the exact decoded string the normalizer consumed, and the
                // normalized hash is its output (issue #801).
                let normalized =
                    spiderhead_parse::text_normalize::normalize_document(&html, area.as_ref())
                        .map_err(parse_failed)?;
                let mut map = match serde_json::to_value(&normalized)? {
                    Value::Object(m) => m,
                    _ => Map::new(),
                };
                let text = map
                    .remove("text")
                    .and_then(|t| t.as_str().map(str::to_string))
                    .unwrap_or_default();
                if text.is_empty() {
                    measured_empty += 1;
                }
                let size = text.len() as i64;
                (text, size, Some(map))
            }
            _ => {
                // Only the boilerplate digest is retained, so coverage must not depend on the
                // corpus's raw HTML size (issue #710).
                let hash = spiderhead_parse::boilerplate_report::boilerplate_hash(&html);
                let size = (url.len() + hash.len()) as i64;
                (hash, size, None)
            }
        };
        if input_bytes + size > MAX_CORPUS_INPUT_BYTES {
            stop_reason = "scan corpus input-byte budget exceeded".into();
            return Ok(false);
        }
        input_bytes += size;
        match kind {
            "duplicate" => items
                .push(json!({"id": url, "text": value, "indexable": indexable(&page, blocked)})),
            "semantic" => {
                let mut headings: Vec<Value> = vec![
                    page.get("h1").cloned().unwrap_or(Value::Null),
                    page.get("h1_2").cloned().unwrap_or(Value::Null),
                    page.get("h2").cloned().unwrap_or(Value::Null),
                ];
                if let Some(raw) = page
                    .get("heading_outline_json")
                    .and_then(Value::as_str)
                    .filter(|s| !s.is_empty())
                {
                    match serde_json::from_str::<Value>(raw) {
                        Ok(Value::Array(entries)) => {
                            headings = entries
                                .iter()
                                .filter(|e| e.is_object())
                                .map(|e| match e.get("text") {
                                    Some(v) if crate::ledger::ingest::truthy(v) => match v {
                                        Value::String(s) => json!(s),
                                        other => json!(other.to_string()),
                                    },
                                    _ => json!(""),
                                })
                                .collect();
                        }
                        Ok(Value::Object(_) | Value::String(_)) => headings = Vec::new(),
                        _ => {}
                    }
                }
                let headings: Vec<Value> = headings
                    .into_iter()
                    .filter(crate::ledger::ingest::truthy)
                    .collect();
                let mut item = Map::new();
                item.insert("id".into(), json!(url));
                item.insert("url".into(), json!(url));
                item.insert("text".into(), json!(value));
                item.insert(
                    "representation".into(),
                    page.get("representation").cloned().unwrap_or(Value::Null),
                );
                item.insert("indexable".into(), json!(indexable(&page, blocked)));
                item.insert(
                    "body_sha256".into(),
                    page.get("document_body_sha256")
                        .cloned()
                        .unwrap_or(Value::Null),
                );
                item.insert(
                    "title".into(),
                    page.get("title").cloned().unwrap_or(Value::Null),
                );
                item.insert(
                    "old_description".into(),
                    page.get("meta_description").cloned().unwrap_or(Value::Null),
                );
                item.insert("headings".into(), Value::Array(headings));
                for (k, v) in prepared.unwrap_or_default() {
                    item.insert(k, v);
                }
                items.push(Value::Object(item));
            }
            _ => items.push(json!({"url": url, "hash": value})),
        }
        Ok(true)
    };
    match crate::discovery_ledger::for_each_row(
        con,
        "SELECT p.document_id,p.representation,p.content_type,p.status_code,p.canonical,p.meta_robots,p.x_robots,p.error,p.title,p.meta_description,p.h1,p.h1_2,p.h2,p.heading_outline_json,u.url,d.body_sha256 AS document_body_sha256,EXISTS(SELECT 1 FROM context_items c WHERE c.kind='robots_blocked_url' AND c.item_key='url:'||p.url_id) AS robots_blocked FROM pages p JOIN urls u USING(url_id) LEFT JOIN documents d ON d.document_id=p.document_id ORDER BY p.page_ordinal",
        [],
        &mut page_body,
    ) {
        Err(e) if read_interrupted(&e) => {
            stop_reason = "scan corpus read exceeded the per-document read budget".into();
        }
        Err(e) => return Err(e),
        Ok(()) => {}
    }
    if !stop_reason.is_empty() {
        // A cap that ends the read is partial coverage with exact counts: prepared items are
        // still analyzed, not a clean or empty result.
        let remaining = eligible - items.len() as i64 - omitted.values().sum::<i64>();
        if remaining > 0 {
            *omitted.entry(stop_reason.clone()).or_insert(0) += remaining;
        }
        partial_reasons.push(stop_reason);
    }
    let mut corpus = Map::new();
    corpus.insert("items".into(), Value::Array(items.clone()));
    corpus.insert(
        "source".into(),
        Value::Object(source(&header, kind, &representations)?),
    );
    corpus.insert(
        "coverage".into(),
        Value::Object(coverage(
            eligible,
            items.len() as i64,
            &omitted,
            "",
            Some(&partial_reasons),
            measured_empty,
        )),
    );
    if kind == "semantic" {
        // The normalization record travels with the public manifest so an output hash can be
        // reproduced or rejected rather than trusted.
        corpus.insert(
            "normalization".into(),
            spiderhead_parse::text_normalize::normalization_policy(area.as_ref()),
        );
    }
    Ok(corpus)
}

/// Return only public scan metadata after an analyzer consumed its private items.
pub fn corpus_public(
    corpus: &Map<String, Value>,
    analyzed: i64,
    unavailable: &str,
) -> Map<String, Value> {
    let mut coverage = corpus
        .get("coverage")
        .and_then(Value::as_object)
        .cloned()
        .unwrap_or_default();
    coverage.insert("analyzed_documents".into(), json!(analyzed));
    if !unavailable.is_empty() {
        coverage.insert("state".into(), json!("unavailable"));
        let previous = coverage
            .get("reason")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string();
        let parts: Vec<&str> = [unavailable, previous.as_str()]
            .into_iter()
            .filter(|p| !p.is_empty())
            .collect();
        coverage.insert("reason".into(), json!(parts.join("; ")));
    }
    let mut out = Map::new();
    out.insert(
        "source".into(),
        corpus.get("source").cloned().unwrap_or(Value::Null),
    );
    out.insert("coverage".into(), Value::Object(coverage));
    if let Some(n) = corpus.get("normalization") {
        out.insert("normalization".into(), n.clone());
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn page(json: Value) -> Map<String, Value> {
        json.as_object().cloned().unwrap()
    }

    #[test]
    fn coverage_states_follow_omissions_and_reasons() {
        let none = BTreeMap::new();
        assert_eq!(coverage(3, 3, &none, "", None, 0)["state"], "complete");
        let mut omitted = BTreeMap::new();
        omitted.insert("unsupported_format".to_string(), 2);
        let c = coverage(3, 1, &omitted, "", None, 0);
        assert_eq!(
            (c["state"].as_str(), c["omitted_documents"].as_i64()),
            (Some("partial"), Some(2))
        );
        let c = coverage(3, 0, &none, "", None, 0);
        assert_eq!(c["state"], "unavailable");
        assert_eq!(
            c["reason"],
            "no retained supported page bodies are available"
        );
        let c = coverage(
            0,
            0,
            &none,
            "",
            Some(&["scan crawl is partial".to_string()]),
            0,
        );
        assert_eq!(
            (c["state"].as_str(), c["reason"].as_str()),
            (Some("partial"), Some("scan crawl is partial"))
        );
        let c = coverage(2, 2, &none, "late failure", Some(&["x".to_string()]), 1);
        assert_eq!(c["reason"], "late failure; x");
        assert_eq!(c["measured_empty_documents"], 1);
    }

    #[test]
    fn public_corpus_keeps_only_source_and_coverage() {
        let corpus = page(json!({
            "items": [{"id": "u", "text": "secret"}],
            "source": {"kind": "scan.v1"},
            "coverage": {"state": "partial", "reason": "r", "analyzed_documents": 0},
            "normalization": {"v": 1},
        }));
        let public = corpus_public(&corpus, 4, "late");
        assert!(!public.contains_key("items"));
        assert_eq!(public["coverage"]["analyzed_documents"], 4);
        assert_eq!(public["coverage"]["state"], "unavailable");
        assert_eq!(public["coverage"]["reason"], "late; r");
        assert_eq!(public["normalization"], json!({"v": 1}));
    }

    #[test]
    fn indexable_follows_the_crawl_policy() {
        let ok = page(
            json!({"url": "https://a.test/x", "status_code": 200, "meta_robots": "", "x_robots": "", "canonical": "", "error": ""}),
        );
        assert!(indexable(&ok, false));
        assert!(!indexable(&ok, true));
        let mut m = ok.clone();
        m.insert("meta_robots".into(), json!("NoIndex, follow"));
        assert!(!indexable(&m, false));
        m.insert("meta_robots".into(), json!("bingbot: noindex"));
        assert!(indexable(&m, false));
        m.insert("meta_robots".into(), json!("none"));
        assert!(!indexable(&m, false));
        let mut c = ok.clone();
        c.insert("canonical".into(), json!("https://a.test/other"));
        assert!(!indexable(&c, false));
        c.insert("canonical".into(), json!("HTTPS://A.TEST/x/"));
        assert!(indexable(&c, false));
        let mut r = ok.clone();
        r.insert("status_code".into(), json!(301));
        assert!(!indexable(&r, false));
        r.insert("status_code".into(), Value::Null);
        assert!(!indexable(&r, false));
    }

    #[test]
    fn identity_names_the_files_of_each_kind() {
        let identity = implementation_identity("boilerplate").unwrap();
        assert_eq!(identity["decoder_version"], "scan_decoder.v1");
        assert_eq!(identity["files"].as_array().unwrap().len(), 4);
        assert!(implementation_identity("other").is_err());
        assert_eq!(identity["source_sha256"].as_str().unwrap().len(), 64);
    }
}
