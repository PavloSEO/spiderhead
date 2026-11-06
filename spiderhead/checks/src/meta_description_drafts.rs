//! Page-grounded, resumable meta-description drafting contracts.
//!
//! SEOHEAD prepares evidence and validates supplied drafts. It does not select
//! a hosted model, hold an API key, or claim control over the calling agent's
//! token or subscription limits: the executor boundary is caller-owned, so this
//! module only batches pages, replays supplied drafts against them, flags
//! review cues and keeps per-page resume state in a local SQLite checkpoint.

use std::collections::{BTreeSet, HashMap};
use std::path::Path;
use std::sync::OnceLock;

use regex::RegexBuilder;
use rusqlite::OptionalExtension;
use scraper::ElementRef;
use serde_json::{json, Map, Value as Json};

use spiderhead_core::common::{canonical_json, python_json_pretty};
use spiderhead_core::sqlite::{open_writer, DEFAULT_TIMEOUT};
use spiderhead_core::tabular::neutralize_formula;
use spiderhead_parse::dom::{self, Soup};
use spiderhead_parse::text_normalize::normalize_document;
use spiderhead_parse::types::ContentAreaConfig;

/// Version stamped on plans, executor declarations and draft rows.
pub const CONTRACT_VERSION: &str = "meta_description_drafts.v1";
/// Default `context.min_chars`.
pub const DEFAULT_MIN_CHARS: i64 = 120;
/// Default `context.max_chars`.
pub const DEFAULT_MAX_CHARS: i64 = 155;
/// Page content passed to review passes is capped at this many characters.
pub const MAX_CONTENT_CHARS: usize = 6_000;
/// Upper bound of the `batch_size` argument.
pub const MAX_BATCH_SIZE: i64 = 100;

fn superlative_re() -> &'static regex::Regex {
    static RE: OnceLock<regex::Regex> = OnceLock::new();
    RE.get_or_init(|| {
        RegexBuilder::new(r"\b(best|leading|number one|guaranteed)\b")
            .case_insensitive(true)
            .build()
            .expect("static superlative pattern")
    })
}

/// The `str()` form of one JSON scalar: text, Python booleans, Python numbers.
fn scalar_text(value: &Json) -> String {
    match value {
        Json::String(text) => text.clone(),
        Json::Bool(true) => "True".to_owned(),
        Json::Bool(false) => "False".to_owned(),
        other => other.to_string(),
    }
}

/// Python truthiness of one JSON value.
fn truthy(value: &Json) -> bool {
    match value {
        Json::Null => false,
        Json::Bool(flag) => *flag,
        Json::Number(number) => number.as_f64().map(|n| n != 0.0).unwrap_or(true),
        Json::String(text) => !text.is_empty(),
        Json::Array(items) => !items.is_empty(),
        Json::Object(map) => !map.is_empty(),
    }
}

/// Python type name used in error texts for one JSON value.
fn py_type(value: &Json) -> &'static str {
    match value {
        Json::Null => "NoneType",
        Json::Bool(_) => "bool",
        Json::Number(number) if number.is_i64() || number.is_u64() => "int",
        Json::Number(_) => "float",
        Json::String(_) => "str",
        Json::Array(_) => "list",
        Json::Object(_) => "dict",
    }
}

/// Python `int()` over one JSON value: ints pass, floats truncate, numeric
/// strings parse, everything else raises like the interpreter.
fn py_int(value: &Json) -> Result<i64, String> {
    match value {
        Json::Number(number) => {
            if let Some(int) = number.as_i64() {
                Ok(int)
            } else {
                Ok(number
                    .as_f64()
                    .map(|f| f.trunc() as i64)
                    .unwrap_or_default())
            }
        }
        Json::Bool(flag) => Ok(i64::from(*flag)),
        Json::String(text) => text
            .trim()
            .parse::<i64>()
            .map_err(|_| format!("invalid literal for int() with base 10: '{text}'")),
        other => Err(format!(
            "int() argument must be a string, a bytes-like object or a real number, not '{}'",
            py_type(other)
        )),
    }
}

/// `value.get(key) or default`: an absent or falsy entry falls back.
fn or_default(object: &Map<String, Json>, key: &str, default: Json) -> Json {
    match object.get(key) {
        Some(value) if truthy(value) => value.clone(),
        _ => default,
    }
}

/// The validated site context: defaults filled, prohibited phrases checked.
fn context_value(context: Option<&Json>) -> Result<Json, String> {
    let value = context.unwrap_or(&Json::Null);
    if !truthy(value) {
        return Ok(json!({
            "version": "site_context.v1",
            "language": "",
            "brand_voice": "",
            "page_type_instructions": {},
            "prohibited_phrases": [],
            "keywords": {},
            "min_chars": DEFAULT_MIN_CHARS,
            "max_chars": DEFAULT_MAX_CHARS,
        }));
    }
    let Some(object) = value.as_object() else {
        return Err("site context must be an object".to_owned());
    };
    let prohibited = match object.get("prohibited_phrases") {
        None => Vec::new(),
        Some(Json::Array(items)) => {
            if items.iter().any(|item| !item.is_string()) {
                return Err("prohibited_phrases must be a list of strings".to_owned());
            }
            items.clone()
        }
        Some(_) => return Err("prohibited_phrases must be a list of strings".to_owned()),
    };
    Ok(json!({
        "version": scalar_text(&or_default(object, "version", Json::from("site_context.v1"))),
        "language": scalar_text(&or_default(object, "language", Json::from(""))),
        "brand_voice": scalar_text(&or_default(object, "brand_voice", Json::from(""))),
        "page_type_instructions": or_default(object, "page_type_instructions", json!({})),
        "prohibited_phrases": prohibited,
        "keywords": or_default(object, "keywords", json!({})),
        "min_chars": py_int(object.get("min_chars").unwrap_or(&Json::from(DEFAULT_MIN_CHARS)))?,
        "max_chars": py_int(object.get("max_chars").unwrap_or(&Json::from(DEFAULT_MAX_CHARS)))?,
    }))
}

/// The keyword one page carries: only a non-empty string counts as supplied.
fn page_keyword(keywords: &Json, url: &str) -> (String, &'static str) {
    match keywords.get(url) {
        Some(Json::String(text)) if !text.is_empty() => (text.clone(), "operator_supplied"),
        _ => (String::new(), "unknown"),
    }
}

/// Text of one element the way bs4's `get_text(" ", strip=True)` builds it:
/// every descendant string trimmed, empties dropped, joined with one space.
fn strip_get_text(el: &ElementRef<'_>) -> String {
    let mut parts: Vec<&str> = Vec::new();
    for node in el.descendants() {
        let scraper::Node::Text(text) = node.value() else {
            continue;
        };
        let part = text.text.trim();
        if !part.is_empty() {
            parts.push(part);
        }
    }
    parts.join(" ")
}

/// `soup.title.get_text(" ", strip=True)` or `""`.
fn title_text(soup: &Soup) -> String {
    dom::find(soup, "title")
        .map(|el| strip_get_text(&el))
        .unwrap_or_default()
}

/// `meta[name=description]` content of the first match, `""` when absent.
fn old_description(soup: &Soup) -> String {
    dom::all_elements(soup)
        .find(|el| dom::tag_name(el) == "meta")
        .filter(|el| {
            dom::attr(el, "name")
                .map(|name| name.eq_ignore_ascii_case("description"))
                .unwrap_or(false)
        })
        .and_then(|el| dom::attr(&el, "content"))
        .map(str::to_owned)
        .unwrap_or_default()
}

/// `h1`/`h2` texts in document order (`soup.find_all(["h1", "h2"])`).
fn heading_texts(soup: &Soup) -> Vec<String> {
    dom::all_elements(soup)
        .filter(|el| matches!(dom::tag_name(el).as_str(), "h1" | "h2"))
        .map(|el| strip_get_text(&el))
        .collect()
}

/// The per-item content-area config, when the item carries one.
fn item_content_area(item: &Map<String, Json>) -> Result<Option<ContentAreaConfig>, String> {
    match item.get("content_area") {
        None | Some(Json::Null) => Ok(None),
        Some(value) => serde_json::from_value(value.clone())
            .map(Some)
            .map_err(|error| error.to_string()),
    }
}

/// `str(item.get(key) or "")`: a falsy entry becomes empty text.
fn str_or_empty(object: &Map<String, Json>, key: &str) -> String {
    object
        .get(key)
        .filter(|value| truthy(value))
        .map(scalar_text)
        .unwrap_or_default()
}

/// The configured `language` override, empty when unset or falsy.
fn configured_language(configured: &Json) -> String {
    configured
        .get("language")
        .filter(|value| truthy(value))
        .map(scalar_text)
        .unwrap_or_default()
}

/// One page's drafting input built from a supplied `{url, html}` item.
fn page_input(item: &Json, configured: &Json) -> Result<Json, String> {
    let Some(object) = item.as_object() else {
        return Err("items must be a list of objects".to_owned());
    };
    let url = str_or_empty(object, "url");
    let unavailable = |reason: &str| json!({"url": url, "state": "unavailable", "reason": reason});
    if url.is_empty() {
        return Ok(unavailable("page has no supplied HTML body"));
    }
    let Some(Json::String(html)) = object.get("html") else {
        return Ok(unavailable("page has no supplied HTML body"));
    };
    if html.trim().is_empty() {
        return Ok(unavailable("page has no supplied HTML body"));
    }
    let over_400 = match object.get("status_code").unwrap_or(&Json::from(200)) {
        Json::Number(number) => number.as_f64().map(|f| f >= 400.0).unwrap_or(false),
        Json::Bool(_) => false,
        other => {
            return Err(format!(
                "'>=' not supported between instances of '{}' and 'int'",
                py_type(other)
            ))
        }
    };
    let content_type = str_or_empty(object, "content_type");
    let content_type = if content_type.is_empty() {
        "text/html".to_owned()
    } else {
        content_type
    };
    if over_400 || !content_type.to_lowercase().contains("html") {
        return Ok(unavailable(
            "page representation is not an eligible HTML page",
        ));
    }
    let normalized = normalize_document(html, item_content_area(object)?.as_ref())
        .map_err(|error| error.to_string())?;
    if normalized.text.is_empty() {
        return Ok(unavailable("page content area is empty"));
    }
    let soup = dom::parse_document_lxml(html);
    let keyword = page_keyword(configured.get("keywords").unwrap_or(&Json::Null), &url);
    let configured_language = configured_language(configured);
    let language = if configured_language.is_empty() {
        scalar_text(
            normalized
                .language
                .get("declared_primary")
                .unwrap_or(&Json::from("")),
        )
    } else {
        configured_language
    };
    Ok(json!({
        "url": url,
        "state": "eligible",
        "source_reference": {"normalized_sha256": normalized.normalized_sha256},
        "title": title_text(&soup),
        "headings": heading_texts(&soup),
        "old_description": old_description(&soup),
        "content": char_prefix(&normalized.text, MAX_CONTENT_CHARS),
        "content_truncated": char_len(&normalized.text) > MAX_CONTENT_CHARS,
        "language": language,
        "language_evidence": normalized.language,
        "keyword": keyword.0,
        "keyword_origin": keyword.1,
    }))
}

/// One page's drafting input built from an already-normalized scan record.
fn normalized_page_input(item: &Json, configured: &Json) -> Json {
    let object = item.as_object();
    let first_truthy = |keys: &[&str]| {
        keys.iter()
            .filter_map(|key| object.and_then(|map| map.get(*key)))
            .find(|value| truthy(value))
            .map(scalar_text)
            .unwrap_or_default()
    };
    let url = first_truthy(&["url", "id"]);
    let unavailable = || {
        json!({
            "url": url,
            "state": "unavailable",
            "reason": "normalized retained page content is unavailable",
        })
    };
    let Some(object) = object else {
        return unavailable();
    };
    if url.is_empty() {
        return unavailable();
    }
    let text = match object.get("text") {
        Some(Json::String(text)) if !text.is_empty() => text.clone(),
        _ => return unavailable(),
    };
    let Some(Json::String(source_hash)) = object.get("normalized_sha256") else {
        return unavailable();
    };
    let keyword = page_keyword(configured.get("keywords").unwrap_or(&Json::Null), &url);
    let configured_language = configured_language(configured);
    let language = if configured_language.is_empty() {
        object
            .get("language")
            .and_then(|language| language.get("declared_primary"))
            .map(scalar_text)
            .unwrap_or_default()
    } else {
        configured_language
    };
    json!({
        "url": url,
        "state": "eligible",
        "source_reference": {
            "normalized_sha256": source_hash,
            "body_sha256": object.get("body_sha256").cloned().unwrap_or(Json::Null),
        },
        "title": str_or_empty(object, "title"),
        "headings": or_default(object, "headings", json!([])),
        "old_description": str_or_empty(object, "old_description"),
        "content": char_prefix(&text, MAX_CONTENT_CHARS),
        "content_truncated": char_len(&text) > MAX_CONTENT_CHARS,
        "language": language,
        "language_evidence": or_default(object, "language", json!({})),
        "keyword": keyword.0,
        "keyword_origin": keyword.1,
    })
}

/// Plan body shared by both source shapes: context, pages, batches, coverage.
fn plan_header(
    configured: Json,
    pages: Vec<Json>,
    batch_size: i64,
    representation: &str,
) -> Result<Json, String> {
    if !(1..=MAX_BATCH_SIZE).contains(&batch_size) {
        return Err(format!("batch_size must be between 1 and {MAX_BATCH_SIZE}"));
    }
    let as_int = |key: &str| {
        configured
            .get(key)
            .and_then(Json::as_i64)
            .unwrap_or_default()
    };
    if as_int("min_chars") < 1 || as_int("max_chars") < as_int("min_chars") {
        return Err("meta description length policy is invalid".to_owned());
    }
    let eligible: Vec<Json> = pages
        .iter()
        .filter(|page| {
            page.get("state")
                .map(|state| state == "eligible")
                .unwrap_or(false)
        })
        .cloned()
        .collect();
    let unavailable = pages
        .iter()
        .filter(|page| {
            page.get("state")
                .map(|state| state == "unavailable")
                .unwrap_or(false)
        })
        .count();
    let batches: Vec<Json> = eligible
        .chunks(batch_size as usize)
        .map(|chunk| Json::Array(chunk.to_vec()))
        .collect();
    Ok(json!({
        "contract_version": CONTRACT_VERSION,
        "context": configured,
        "pages": pages,
        "batches": batches,
        "coverage": {
            "state": if eligible.len() != pages.len() { "partial" } else { "complete" },
            "eligible": eligible.len(),
            "excluded": pages.len() - eligible.len(),
            "unavailable": unavailable,
            "content_representation": representation,
        },
    }))
}

/// Build bounded, page-evidence batches from supplied `{url, html}` items.
pub fn prepare_draft_plan(
    items: &[Json],
    context: Option<&Json>,
    batch_size: i64,
) -> Result<Json, String> {
    let configured = context_value(context)?;
    let mut pages = Vec::with_capacity(items.len());
    for item in items {
        pages.push(page_input(item, &configured)?);
    }
    plan_header(configured, pages, batch_size, "supplied HTML content area")
}

/// Build the same batches from already-normalized retained scan records.
pub fn prepare_draft_plan_from_normalized(
    items: &[Json],
    context: Option<&Json>,
    batch_size: i64,
) -> Result<Json, String> {
    let configured = context_value(context)?;
    let pages = items
        .iter()
        .map(|item| normalized_page_input(item, &configured))
        .collect();
    plan_header(
        configured,
        pages,
        batch_size,
        "normalized retained scan content",
    )
}

/// The caller-owned executor boundary: what it declares and how it replays drafts.
pub trait DraftExecutor {
    /// The versioned contract declaration.
    fn describe(&self) -> Json;
    /// The drafts supplied for one batch of records; an error is
    /// checkpoint-visible, never a false success.
    fn execute(&self, records: &[Json]) -> Result<Vec<Json>, String>;
}

/// Replay structured drafts from any declared caller-owned executor kind.
///
/// The drafts collection is replayed exactly like Python's: a non-list
/// collection surfaces its iteration error at `execute` time, and a non-object
/// draft raises inside the url filter rather than being skipped.
pub struct DeclaredDraftExecutor {
    declaration: Json,
    drafts: Vec<Json>,
    iterable_error: Option<String>,
}

impl DeclaredDraftExecutor {
    /// Bind one executor declaration to its supplied drafts collection.
    pub fn new(declaration: &Json, drafts: &Json) -> Self {
        let (items, iterable_error) = match drafts {
            Json::Array(items) => (items.clone(), None),
            Json::String(_) | Json::Object(_) => (
                Vec::new(),
                Some("'str' object has no attribute 'get'".to_owned()),
            ),
            other => (
                Vec::new(),
                Some(format!("'{}' object is not iterable", py_type(other))),
            ),
        };
        Self {
            declaration: declaration.clone(),
            drafts: items,
            iterable_error,
        }
    }
}

impl DraftExecutor for DeclaredDraftExecutor {
    fn describe(&self) -> Json {
        self.declaration.clone()
    }

    fn execute(&self, records: &[Json]) -> Result<Vec<Json>, String> {
        if let Some(error) = &self.iterable_error {
            return Err(error.clone());
        }
        let expected: BTreeSet<&str> = records
            .iter()
            .filter_map(|record| record.get("url").and_then(Json::as_str))
            .collect();
        let mut supplied = Vec::new();
        for draft in &self.drafts {
            // The Python executor reads draft.get("url") before filtering, so a
            // non-object draft raises instead of being skipped.
            if !draft.is_object() {
                return Err(format!(
                    "'{}' object has no attribute 'get'",
                    py_type(draft)
                ));
            }
            let Some(url) = draft.get("url") else {
                continue;
            };
            match url.as_str() {
                Some(url) => {
                    if expected.contains(url) {
                        supplied.push(draft.clone());
                    }
                }
                // An unhashable url raises in Python's `in expected` check.
                None => {
                    if url.is_object() || url.is_array() {
                        return Err(format!("unhashable type: '{}'", py_type(url)));
                    }
                }
            }
        }
        Ok(supplied)
    }
}

/// Validate one executor declaration; every deviation names the broken contract.
fn declared_executor(executor: &dyn DraftExecutor) -> Result<Json, String> {
    let declared = executor.describe();
    let Some(object) = declared.as_object() else {
        return Err("draft executor does not declare the versioned contract".to_owned());
    };
    for key in [
        "kind",
        "contract_version",
        "model_identity",
        "data_transfer",
    ] {
        if !object.contains_key(key) {
            return Err("draft executor does not declare the versioned contract".to_owned());
        }
    }
    if object.get("contract_version").and_then(Json::as_str) != Some(CONTRACT_VERSION) {
        return Err("draft executor contract version is unsupported".to_owned());
    }
    if !matches!(
        object.get("kind").and_then(Json::as_str),
        Some("calling_agent") | Some("delegated_agent") | Some("external_provider")
    ) {
        return Err("draft executor kind is unsupported".to_owned());
    }
    Ok(declared)
}

/// The resume key: source, context, executor and prompt version together, so a
/// change to any of them starts a new draft.
fn draft_key(page: &Json, context: &Json, executor: &Json) -> String {
    let payload = json!({
        "url": page.get("url").cloned().unwrap_or(Json::Null),
        "source": page.get("source_reference").cloned().unwrap_or(Json::Null),
        "context": context,
        "executor": executor,
        "prompt_version": CONTRACT_VERSION,
    });
    sha_hex(&canonical_json(&payload))
}

fn sha_hex(text: &str) -> String {
    use sha2::{Digest, Sha256};
    let mut hasher = Sha256::new();
    hasher.update(text.as_bytes());
    let digest = hasher.finalize();
    let mut out = String::with_capacity(digest.len() * 2);
    for byte in digest {
        out.push_str(&format!("{byte:02x}"));
    }
    out
}

/// Per-page local resume state; source/context/executor changes get a new key.
pub struct DraftCheckpoint {
    connection: rusqlite::Connection,
}

impl DraftCheckpoint {
    /// Open (creating when needed) the checkpoint database and its table.
    pub fn open(path: &str) -> rusqlite::Result<Self> {
        let connection = open_writer(
            Path::new(path),
            DEFAULT_TIMEOUT,
            &["PRAGMA journal_mode=WAL"],
        )?;
        connection.execute(
            "CREATE TABLE IF NOT EXISTS meta_description_drafts (\
             draft_key TEXT PRIMARY KEY, record_json TEXT NOT NULL, updated_at TEXT NOT NULL \
             DEFAULT CURRENT_TIMESTAMP)",
            [],
        )?;
        Ok(Self { connection })
    }

    /// The saved record for one key, when it parses to a JSON object.
    pub fn get(&self, key: &str) -> Option<Json> {
        let row: String = self
            .connection
            .query_row(
                "SELECT record_json FROM meta_description_drafts WHERE draft_key=?",
                [key],
                |row| row.get(0),
            )
            .optional()
            .ok()
            .flatten()?;
        let value = serde_json::from_str::<Json>(&row).ok()?;
        if value.is_object() {
            Some(value)
        } else {
            None
        }
    }

    /// Store one record under its key, replacing any earlier one.
    pub fn put(&self, key: &str, record: &Json) -> rusqlite::Result<()> {
        self.connection.execute(
            "INSERT OR REPLACE INTO meta_description_drafts (draft_key, record_json) VALUES (?, ?)",
            rusqlite::params![key, canonical_json(record)],
        )?;
        Ok(())
    }
}

/// Review cues for one completed draft; each names what an editor must check.
fn review_reasons(row: &Json, page: &Json, context: &Json) -> Vec<String> {
    let text = row
        .get("proposed_description")
        .and_then(Json::as_str)
        .unwrap_or_default();
    let mut reasons: Vec<String> = Vec::new();
    // Unicode code points, deliberately not bytes or pixels.
    let count = char_len(text);
    let min_chars = context.get("min_chars").and_then(Json::as_i64).unwrap_or(0);
    let max_chars = context.get("max_chars").and_then(Json::as_i64).unwrap_or(0);
    if count < min_chars as usize || count > max_chars as usize {
        reasons.push("outside_configured_character_policy".to_owned());
    }
    let folded = text.to_lowercase();
    let prohibited = context
        .get("prohibited_phrases")
        .and_then(Json::as_array)
        .cloned()
        .unwrap_or_default();
    if prohibited
        .iter()
        .filter_map(Json::as_str)
        .any(|phrase| folded.contains(&phrase.to_lowercase()))
    {
        reasons.push("contains_prohibited_brand_phrase".to_owned());
    }
    // This is a review cue, not a factual-verification claim: a marketing
    // superlative absent from the supplied page evidence needs an editor to
    // find support or remove it before publication.
    let source = {
        let title = page.get("title").and_then(Json::as_str).unwrap_or_default();
        let headings = page
            .get("headings")
            .and_then(Json::as_array)
            .cloned()
            .unwrap_or_default();
        let content = page
            .get("content")
            .and_then(Json::as_str)
            .unwrap_or_default();
        let mut parts = vec![title.to_owned()];
        parts.extend(headings.iter().map(scalar_text));
        parts.push(content.to_owned());
        parts.join(" ").to_lowercase()
    };
    if superlative_re().is_match(text) && !superlative_re().is_match(&source) {
        reasons.push("marketing_claim_not_observed_in_page_evidence".to_owned());
    }
    let language = page
        .get("language")
        .and_then(Json::as_str)
        .unwrap_or_default();
    let script = page
        .get("language_evidence")
        .and_then(|evidence| evidence.get("script"))
        .and_then(Json::as_str)
        .unwrap_or_default();
    if !language.is_empty() && script == "cyrillic" && text.chars().any(|c| c.is_ascii_alphabetic())
    {
        reasons.push("possible_language_mismatch".to_owned());
    }
    reasons
}

/// Execute supplied/delegated drafts, validate them and preserve resume state.
pub fn run_draft_plan(
    plan: &Json,
    executor: &dyn DraftExecutor,
    checkpoint: &DraftCheckpoint,
) -> Result<Json, String> {
    let context = plan.get("context").cloned().unwrap_or_else(|| json!({}));
    let declared = declared_executor(executor)?;
    let pages = plan
        .get("pages")
        .and_then(Json::as_array)
        .cloned()
        .unwrap_or_default();
    let mut rows: Vec<Json> = Vec::new();
    let mut pending: Vec<Json> = Vec::new();
    for page in &pages {
        if page
            .get("state")
            .map(|state| state != "eligible")
            .unwrap_or(true)
        {
            let reason = page.get("reason").cloned().unwrap_or(Json::Null);
            let mut row = page.clone();
            if let Some(object) = row.as_object_mut() {
                object.insert("generation_state".to_owned(), Json::from("unavailable"));
                object.insert("review_reasons".to_owned(), Json::Array(vec![reason]));
            }
            rows.push(row);
            continue;
        }
        let key = draft_key(page, &context, &declared);
        match checkpoint.get(&key) {
            Some(saved)
                if saved.get("generation_state").and_then(Json::as_str) == Some("completed") =>
            {
                let mut row = saved;
                if let Some(object) = row.as_object_mut() {
                    object.insert("resumed".to_owned(), Json::Bool(true));
                }
                rows.push(row);
            }
            _ => {
                let mut row = page.clone();
                if let Some(object) = row.as_object_mut() {
                    object.insert("draft_key".to_owned(), Json::from(key));
                }
                pending.push(row);
            }
        }
    }
    // Python re-chunks pending by the first planned batch's size.
    let batch_len = plan
        .get("batches")
        .and_then(Json::as_array)
        .and_then(|batches| batches.first())
        .and_then(Json::as_array)
        .map(Vec::len)
        .unwrap_or(1)
        .max(1);
    for chunk in pending.chunks(batch_len) {
        let supplied = match executor.execute(chunk) {
            Ok(supplied) => supplied,
            // Executor errors are checkpoint-visible, never a false success.
            Err(error) => {
                for page in chunk {
                    let mut row = page.clone();
                    if let Some(object) = row.as_object_mut() {
                        object.insert("generation_state".to_owned(), Json::from("failed"));
                        object.insert("error".to_owned(), Json::from(error.clone()));
                        object.insert("review_reasons".to_owned(), json!([]));
                    }
                    checkpoint
                        .put(
                            page.get("draft_key")
                                .and_then(Json::as_str)
                                .unwrap_or_default(),
                            &row,
                        )
                        .map_err(|error| error.to_string())?;
                    rows.push(row);
                }
                continue;
            }
        };
        // Last supplied draft per URL wins.
        let mut by_url: HashMap<&str, &Json> = HashMap::new();
        for draft in &supplied {
            if let Some(url) = draft.get("url").and_then(Json::as_str) {
                by_url.insert(url, draft);
            }
        }
        for page in chunk {
            let page_url = page.get("url").and_then(Json::as_str).unwrap_or_default();
            let draft = by_url.get(page_url).copied();
            let source_hash = page
                .get("source_reference")
                .and_then(|reference| reference.get("normalized_sha256"))
                .cloned()
                .unwrap_or(Json::Null);
            let stale = !matches!(draft, Some(draft)
                if draft.get("source_sha256").cloned().unwrap_or(Json::Null) == source_hash);
            let missing = !matches!(draft,
                Some(draft) if draft
                    .get("proposed_description")
                    .and_then(Json::as_str)
                    .map(|text| !text.trim().is_empty())
                    .unwrap_or(false));
            let mut row = page.clone();
            if let Some(object) = row.as_object_mut() {
                object.insert("executor".to_owned(), declared.clone());
                object.insert("prompt_version".to_owned(), Json::from(CONTRACT_VERSION));
                object.insert("correction_attempts".to_owned(), Json::from(0));
                object.insert("resumed".to_owned(), Json::Bool(false));
                if stale {
                    object.insert("generation_state".to_owned(), Json::from("failed"));
                    object.insert(
                        "error".to_owned(),
                        Json::from("missing or stale structured draft"),
                    );
                    object.insert("review_reasons".to_owned(), json!([]));
                } else if missing {
                    object.insert("generation_state".to_owned(), Json::from("failed"));
                    object.insert(
                        "error".to_owned(),
                        Json::from("draft description is missing"),
                    );
                    object.insert("review_reasons".to_owned(), json!([]));
                } else {
                    let draft = draft.unwrap_or(&Json::Null);
                    object.insert("generation_state".to_owned(), Json::from("completed"));
                    object.insert(
                        "proposed_description".to_owned(),
                        Json::from(
                            draft
                                .get("proposed_description")
                                .and_then(Json::as_str)
                                .unwrap_or_default()
                                .trim(),
                        ),
                    );
                    // A present null stays null; only an absent key takes the declared default.
                    object.insert(
                        "model_identity".to_owned(),
                        match draft.get("model_identity") {
                            Some(value) => value.clone(),
                            None => declared
                                .get("model_identity")
                                .cloned()
                                .unwrap_or(Json::Null),
                        },
                    );
                    object.insert(
                        "review_state".to_owned(),
                        Json::from(match draft.get("review_state") {
                            Some(value) if truthy(value) => scalar_text(value),
                            _ => "needs_human_review".to_owned(),
                        }),
                    );
                }
            }
            if row.get("generation_state").and_then(Json::as_str) == Some("completed") {
                let count = char_len(
                    row.get("proposed_description")
                        .and_then(Json::as_str)
                        .unwrap_or_default(),
                );
                let reasons = review_reasons(&row, page, &context);
                if let Some(object) = row.as_object_mut() {
                    object.insert("character_count".to_owned(), Json::from(count as i64));
                    object.insert(
                        "review_reasons".to_owned(),
                        Json::Array(reasons.into_iter().map(Json::from).collect()),
                    );
                }
            }
            checkpoint
                .put(
                    page.get("draft_key")
                        .and_then(Json::as_str)
                        .unwrap_or_default(),
                    &row,
                )
                .map_err(|error| error.to_string())?;
            rows.push(row);
        }
    }
    append_repetition_reasons(&mut rows, "duplicate_proposed_description", |text| {
        text.to_lowercase()
    });
    append_repetition_reasons(&mut rows, "repetitive_opening", |text| {
        text.to_lowercase()
            .split_whitespace()
            .take(3)
            .collect::<Vec<_>>()
            .join(" ")
    });
    Ok(json!({
        "contract_version": CONTRACT_VERSION,
        "executor": declared,
        "rows": rows,
    }))
}

/// Flag completed rows that share a normalized description form: whole text for
/// duplicates, the first three words for repetitive openings.
fn append_repetition_reasons(rows: &mut [Json], reason: &str, normalize: fn(&str) -> String) {
    let mut groups: HashMap<String, Vec<usize>> = HashMap::new();
    for (index, row) in rows.iter().enumerate() {
        if row.get("generation_state").and_then(Json::as_str) != Some("completed") {
            continue;
        }
        let text = row
            .get("proposed_description")
            .and_then(Json::as_str)
            .unwrap_or_default();
        let key = normalize(text);
        if !key.is_empty() {
            groups.entry(key).or_default().push(index);
        }
    }
    for group in groups.into_values().filter(|group| group.len() > 1) {
        for index in group {
            if let Some(reasons) = rows[index]
                .get_mut("review_reasons")
                .and_then(Json::as_array_mut)
            {
                reasons.push(Json::from(reason));
            }
        }
    }
}

fn char_len(text: &str) -> usize {
    text.chars().count()
}

fn char_prefix(text: &str, n: usize) -> String {
    match text.char_indices().nth(n) {
        Some((at, _)) => text[..at].to_owned(),
        None => text.to_owned(),
    }
}

/// The review cell form: Python `str()` of the value with spreadsheet formula
/// leads neutralised by the shared prefix rule.
fn spreadsheet_text(value: &Json) -> String {
    neutralize_formula(&scalar_text(value))
}

/// Write local review artifacts; this never applies a CMS change.
pub fn export_draft_review(result: &Json, json_path: &str, csv_path: &str) -> Result<(), String> {
    let Some(rows) = result.get("rows").and_then(Json::as_array) else {
        return Err("draft result rows are required".to_owned());
    };
    std::fs::write(json_path, format!("{}\n", python_json_pretty(result)))
        .map_err(|error| error.to_string())?;
    let fields = [
        "url",
        "generation_state",
        "old_description",
        "proposed_description",
        "character_count",
        "review_state",
        "review_reasons",
    ];
    let mut writer = csv::WriterBuilder::new()
        .terminator(csv::Terminator::CRLF)
        .from_path(Path::new(csv_path))
        .map_err(|error| error.to_string())?;
    writer
        .write_record(fields.iter())
        .map_err(|error| error.to_string())?;
    for row in rows {
        let record: Vec<String> = fields
            .iter()
            .map(|field| match row.get(*field) {
                // Joined only when the value is the reason list; anything else
                // keeps its own str() form.
                Some(Json::Array(items)) if *field == "review_reasons" => neutralize_formula(
                    &items.iter().map(scalar_text).collect::<Vec<_>>().join("; "),
                ),
                Some(value) => spreadsheet_text(value),
                None => String::new(),
            })
            .collect();
        writer
            .write_record(&record)
            .map_err(|error| error.to_string())?;
    }
    writer.flush().map_err(|error| error.to_string())?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::cell::Cell;
    use std::path::Path;
    use tempfile::TempDir;

    // from tests/test_meta_description_drafts.py::_pages
    fn pages() -> Vec<Json> {
        vec![
            json!({
                "url": "https://example.test/pumps",
                "html": "<html lang=en><head><title>Pumps</title><meta name=description content='Old copy'></head><body><nav>menu</nav><main><h1>Industrial pumps</h1><p>Industrial water pumps for factories with delivery options.</p></main></body></html>",
            }),
            json!({
                "url": "https://example.test/valves",
                "html": "<html lang=en><head><title>Valves</title></head><body><main><h1>Control valves</h1><p>Control valves for water systems and industrial maintenance.</p></main></body></html>",
            }),
            json!({"url": "https://example.test/missing", "html": ""}),
        ]
    }

    // from tests/test_meta_description_drafts.py::_draft
    fn draft(page: &Json, text: &str) -> Json {
        json!({
            "url": page.get("url").cloned().unwrap_or(Json::Null),
            "source_sha256": page
                .pointer("/source_reference/normalized_sha256")
                .cloned()
                .unwrap_or(Json::Null),
            "proposed_description": text,
        })
    }

    fn supplied_executor(drafts: Vec<Json>) -> DeclaredDraftExecutor {
        DeclaredDraftExecutor::new(
            &json!({
                "kind": "calling_agent",
                "contract_version": CONTRACT_VERSION,
                "model_identity": Json::Null,
                "data_transfer": "caller_runtime",
            }),
            &Json::Array(drafts),
        )
    }

    fn delegated_executor(drafts: Vec<Json>) -> DeclaredDraftExecutor {
        DeclaredDraftExecutor::new(
            &json!({
                "kind": "delegated_agent",
                "contract_version": CONTRACT_VERSION,
                "model_identity": Json::Null,
                "data_transfer": "caller_runtime",
            }),
            &Json::Array(drafts),
        )
    }

    fn checkpoint(dir: &TempDir, name: &str) -> DraftCheckpoint {
        DraftCheckpoint::open(dir.path().join(name).to_str().expect("utf-8 path"))
            .expect("checkpoint opens")
    }

    fn run(plan: &Json, executor: &dyn DraftExecutor, dir: &TempDir, name: &str) -> Json {
        run_draft_plan(plan, executor, &checkpoint(dir, name)).expect("run succeeds")
    }

    fn csv_column(path: &Path, field: &str) -> Vec<String> {
        let mut reader = csv::ReaderBuilder::new()
            .from_path(path)
            .expect("csv opens");
        let headers = reader.headers().expect("csv headers").clone();
        let index = headers
            .iter()
            .position(|header| header == field)
            .expect("column exists");
        reader
            .records()
            .map(|record| record.expect("csv record")[index].to_owned())
            .collect()
    }

    // from tests/test_meta_description_drafts.py::test_supplied_and_delegated_synthetic_executors_share_the_same_contract
    #[test]
    fn supplied_and_delegated_synthetic_executors_share_the_same_contract() {
        let dir = TempDir::new().expect("temp dir");
        let plan = prepare_draft_plan(
            &pages(),
            Some(&json!({"min_chars": 10, "max_chars": 155})),
            1,
        )
        .expect("plan builds");
        let batches = plan
            .get("batches")
            .and_then(Json::as_array)
            .expect("batches");
        let drafts = vec![
            draft(
                &batches[0][0],
                "Industrial pumps for factory water systems with delivery options.",
            ),
            draft(
                &batches[1][0],
                "Control valves for water systems and industrial maintenance projects.",
            ),
        ];

        let supplied = run(
            &plan,
            &supplied_executor(drafts.clone()),
            &dir,
            "supplied.sqlite",
        );
        let delegated = run(&plan, &delegated_executor(drafts), &dir, "delegated.sqlite");

        fn states(result: &Json) -> Vec<&str> {
            result["rows"]
                .as_array()
                .expect("rows")
                .iter()
                .map(|row| row["generation_state"].as_str().expect("state"))
                .collect()
        }
        assert_eq!(states(&supplied), ["unavailable", "completed", "completed"]);
        assert_eq!(
            states(&delegated),
            ["unavailable", "completed", "completed"]
        );
        assert_eq!(supplied["executor"]["kind"], "calling_agent");
        assert_eq!(delegated["executor"]["kind"], "delegated_agent");
    }

    // from tests/test_meta_description_drafts.py::test_checkpoint_resumes_completed_rows_and_invalidates_changed_context
    #[test]
    fn checkpoint_resumes_completed_rows_and_invalidates_changed_context() {
        let dir = TempDir::new().expect("temp dir");
        let plan = prepare_draft_plan(&pages()[..1], Some(&json!({"min_chars": 10})), 20)
            .expect("plan builds");
        let page = plan["batches"][0][0].clone();
        let executor = supplied_executor(vec![draft(
            &page,
            "Industrial pumps for factory water systems and planned delivery support.",
        )]);
        let shared = checkpoint(&dir, "drafts.sqlite");
        let first = run_draft_plan(&plan, &executor, &shared).expect("first run");
        let second = run_draft_plan(&plan, &executor, &shared).expect("second run");
        let changed_plan = prepare_draft_plan(&pages()[..1], Some(&json!({"min_chars": 11})), 20)
            .expect("changed plan");
        let changed = run_draft_plan(&changed_plan, &executor, &shared).expect("changed run");

        assert_eq!(first["rows"][0]["resumed"], false);
        assert_eq!(second["rows"][0]["resumed"], true);
        assert_eq!(changed["rows"][0]["generation_state"], "completed");
        assert_eq!(changed["rows"][0]["resumed"], false);
    }

    // from tests/test_meta_description_drafts.py::test_stale_malformed_and_repetitive_drafts_remain_reviewable
    #[test]
    fn stale_malformed_and_repetitive_drafts_remain_reviewable() {
        let dir = TempDir::new().expect("temp dir");
        let plan = prepare_draft_plan(
            &pages()[..2],
            Some(&json!({
                "min_chars": 120,
                "max_chars": 155,
                "prohibited_phrases": ["best"],
            })),
            20,
        )
        .expect("plan builds");
        let first = plan["batches"][0][0].clone();
        let second = plan["batches"][0][1].clone();
        let text = "Best industrial equipment for water systems, delivery planning and maintenance support for every project today.";
        let result = run(
            &plan,
            &supplied_executor(vec![draft(&first, text), draft(&second, text)]),
            &dir,
            "drafts.sqlite",
        );

        for row in result["rows"].as_array().expect("rows") {
            assert_eq!(row["generation_state"], "completed");
        }
        let reasons: Vec<&str> = result["rows"][0]["review_reasons"]
            .as_array()
            .expect("reasons")
            .iter()
            .map(|reason| reason.as_str().expect("reason"))
            .collect();
        assert!(reasons.contains(&"duplicate_proposed_description"));
        assert!(reasons.contains(&"repetitive_opening"));
        assert!(reasons.contains(&"contains_prohibited_brand_phrase"));
        assert!(reasons.contains(&"marketing_claim_not_observed_in_page_evidence"));
    }

    // from tests/test_meta_description_drafts.py::test_export_is_formula_safe_and_no_cms_write
    #[test]
    fn export_is_formula_safe_and_no_cms_write() {
        let dir = TempDir::new().expect("temp dir");
        let plan = prepare_draft_plan(&pages()[..1], Some(&json!({"min_chars": 1})), 20)
            .expect("plan builds");
        let page = plan["batches"][0][0].clone();
        let result = run(
            &plan,
            &supplied_executor(vec![draft(&page, "=not a spreadsheet formula")]),
            &dir,
            "drafts.sqlite",
        );
        let json_path = dir.path().join("review.json");
        let csv_path = dir.path().join("review.csv");
        export_draft_review(
            &result,
            json_path.to_str().unwrap(),
            csv_path.to_str().unwrap(),
        )
        .expect("export succeeds");

        assert!(json_path.exists());
        let values = csv_column(&csv_path, "proposed_description");
        assert_eq!(values.len(), 1);
        assert!(values[0].starts_with("'="));
    }

    // from tests/test_meta_description_drafts.py::test_tab_and_cr_leading_drafts_are_neutralised_in_csv
    #[test]
    fn tab_and_cr_leading_drafts_are_neutralised_in_csv() {
        let dir = TempDir::new().expect("temp dir");
        let plan = prepare_draft_plan(&pages()[..1], Some(&json!({"min_chars": 1})), 20)
            .expect("plan builds");
        let page = plan["batches"][0][0].clone();
        let result = run(
            &plan,
            &supplied_executor(vec![
                draft(&page, "\tcmd|' /C calc'!A0"),
                draft(&page, "\r=HYPERLINK(1)"),
            ]),
            &dir,
            "drafts.sqlite",
        );
        let json_path = dir.path().join("review.json");
        let csv_path = dir.path().join("review.csv");
        export_draft_review(
            &result,
            json_path.to_str().unwrap(),
            csv_path.to_str().unwrap(),
        )
        .expect("export succeeds");

        let values = csv_column(&csv_path, "proposed_description");
        assert!(!values.is_empty());
        assert!(values.iter().all(|value| value.starts_with('\'')));
    }

    // from tests/test_meta_description_drafts.py::test_executor_interruption_is_checkpointed_and_a_later_run_resumes_completed_work
    #[test]
    fn executor_interruption_is_checkpointed_and_a_later_run_resumes_completed_work() {
        struct InterruptingExecutor {
            calls: Cell<i64>,
            pumps_draft: Json,
        }

        impl DraftExecutor for InterruptingExecutor {
            fn describe(&self) -> Json {
                json!({
                    "kind": "calling_agent",
                    "contract_version": CONTRACT_VERSION,
                    "model_identity": Json::Null,
                    "data_transfer": "caller_runtime",
                })
            }

            fn execute(&self, _records: &[Json]) -> Result<Vec<Json>, String> {
                self.calls.set(self.calls.get() + 1);
                if self.calls.get() == 2 {
                    return Err("caller interrupted".to_owned());
                }
                Ok(vec![self.pumps_draft.clone()])
            }
        }

        let dir = TempDir::new().expect("temp dir");
        let plan = prepare_draft_plan(&pages()[..2], Some(&json!({"min_chars": 1})), 1)
            .expect("plan builds");
        let first = plan["batches"][0][0].clone();
        let second = plan["batches"][1][0].clone();
        let interrupting = InterruptingExecutor {
            calls: Cell::new(0),
            pumps_draft: draft(
                &first,
                "Industrial pumps for factory water systems and delivery support.",
            ),
        };
        let shared = checkpoint(&dir, "drafts.sqlite");
        let interrupted = run_draft_plan(&plan, &interrupting, &shared).expect("interrupted run");
        assert_eq!(interrupting.calls.get(), 2);

        let recovered_executor = supplied_executor(vec![draft(
            &second,
            "Control valves for water systems and industrial maintenance projects.",
        )]);
        let recovered = run_draft_plan(&plan, &recovered_executor, &shared).expect("recovered run");

        let states: BTreeSet<&str> = interrupted["rows"]
            .as_array()
            .expect("rows")
            .iter()
            .map(|row| row["generation_state"].as_str().expect("state"))
            .collect();
        assert_eq!(states, BTreeSet::from(["completed", "failed"]));
        assert_eq!(recovered["rows"][0]["resumed"], true);
        assert_eq!(recovered["rows"][1]["generation_state"], "completed");
    }
}
