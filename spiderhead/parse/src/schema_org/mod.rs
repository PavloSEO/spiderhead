//! Schema.org structured-data validation: JSON-LD, microdata and RDFa against the bundled
//! vocabulary, entity-graph connectivity and rich-result readiness.

use serde::{Deserialize, Serialize};

mod check;
mod dates;
mod findings;
mod microdata;
mod nodes;
mod rdfa;
mod rdfa_copy;
mod rdfa_reader;

use std::collections::HashSet;

use serde_json::{json, Value};

use crate::dom::parse_document;
use crate::duplicate::is_word_char;
use crate::parser::pyjson;
use crate::parser::shared::MAX_REDIRECTS;
use crate::parser::tokenize::{document_base_url as html_document_base_url, DocumentSource};
use crate::parser::{meta, HtmlFetcher};
use crate::py_str::{is_py_space, py_repr, py_strip};
use crate::types::{Json, Result};

mod oversize;
pub use oversize::*;

/// Embedded rich-result rules (`data/rich_results_rules.json`).
pub const RICH_RESULTS_RULES_JSON: &str = include_str!("../../data/rich_results_rules.json");
/// Embedded Schema.org vocabulary snapshot (`data/schemaorg.json`).
pub const SCHEMAORG_JSON: &str = include_str!("../../data/schemaorg.json");

/// Maximum HTML bytes validated.
pub const MAX_HTML_BYTES: usize = 5_000_000;
/// Maximum microdata/RDFa nodes.
pub const MAX_MARKUP_NODES: usize = 50_000;
/// Maximum RDFa triples.
pub const MAX_RDFA_TRIPLES: usize = 20_000;
/// Maximum RDFa tokens.
pub const MAX_RDFA_TOKENS: usize = 50_000;
/// Maximum RDFa `rdfa:copy` rounds.
pub const MAX_RDFA_COPY_ROUNDS: usize = 128;
/// Maximum RDFa `rdfa:copy` steps.
pub const MAX_RDFA_COPY_STEPS: usize = 100_000;

/// Vocabulary-snapshot summary in the result.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct VocabularySummary {
    /// Snapshot date string (`"2026-08-12"`) or `false` on the oversize branch (`Json`).
    pub fetched: Json,
    /// Type count.
    pub types: u64,
    /// Property count.
    pub properties: u64,
}

/// One `@context` vocabulary seen.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct VocabularyUse {
    /// Normalised context.
    pub context: String,
    /// Whether it is Schema.org.
    pub supported: bool,
    /// JSON-LD blocks using it.
    pub blocks: u64,
}

/// Graph connectivity measures.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct GraphShape {
    /// Node count.
    pub nodes: u64,
    /// Nodes with `@id`.
    pub with_id: u64,
    /// Nodes referenced by id.
    pub linked_by_id: u64,
    /// Top-level nodes.
    pub top_level: u64,
    /// Disconnected components.
    pub islands: Vec<Json>,
    /// Whether the markup forms one connected graph.
    pub is_graph: bool,
}

/// Coverage of one markup format.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MarkupCoverage {
    /// `microdata` or `rdfa`.
    pub format: String,
    /// `checked`, `absent` or `partial`.
    pub status: String,
    /// Reasons for `partial`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reasons: Option<Vec<String>>,
}

/// Which non-JSON-LD markup exists.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct OtherMarkup {
    /// Microdata present.
    pub microdata: bool,
    /// RDFa present.
    pub rdfa: bool,
}

/// Counts of non-JSON-LD markup items.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MarkupCounts {
    /// Microdata item scopes.
    pub microdata: u64,
    /// RDFa subjects.
    pub rdfa: u64,
}

/// Result of [`check_schema`]. On a failed fetch only `ok`, `error` (and `url`) are set.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SchemaCheck {
    /// False on an invalid URL or failed fetch.
    pub ok: bool,
    /// Error text (failure only).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    /// Normalised URL (fetch path only).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub url: Option<String>,
    /// URL after redirects (fetch path only).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub final_url: Option<String>,
    /// HTTP status (fetch path only).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub status_code: Option<u16>,
    /// Vocabulary summary.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub vocabulary: Option<VocabularySummary>,
    /// Parsed JSON-LD blocks.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub blocks: Option<u64>,
    /// JSON-LD blocks found.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub blocks_found: Option<u64>,
    /// JSON-LD blocks that failed to parse.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub blocks_invalid: Option<u64>,
    /// Vocabularies seen.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub vocabularies: Option<Vec<VocabularyUse>>,
    /// JSON-LD parse errors (`{index,error,excerpt,hint?}` records).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parse_errors: Option<Vec<Json>>,
    /// Graph connectivity.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub graph: Option<GraphShape>,
    /// Per-node validation records.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub entities: Option<Vec<Json>>,
    /// Rich-result readiness records.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rich_results: Option<Vec<Json>>,
    /// Version of the rich-result rules.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rules_version: Option<String>,
    /// Which other markup exists.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub other_markup: Option<OtherMarkup>,
    /// Other-markup counts.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub markup_counts: Option<MarkupCounts>,
    /// Coverage per format.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub markup_coverage: Option<Vec<MarkupCoverage>>,
    /// Human-readable findings.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub findings: Option<Vec<String>>,
}

/// The compiled vocabulary (types/properties maps), parsed once from [`SCHEMAORG_JSON`].
pub fn load_vocab() -> &'static Json {
    nodes::vocab()
}

/// The rich-result rules (`version` + `rules`), parsed once from [`RICH_RESULTS_RULES_JSON`].
pub fn rich_results_rules() -> &'static Json {
    nodes::rules()
}

/// A type's inheritance chain to the root, for property validation.
pub fn type_chain(name: &str, vocab: &Json) -> Vec<String> {
    nodes::type_chain(name, vocab)
}

/// Copy a JSON-LD tree and resolve Schema.org identities against its local base.
/// `Err` where relative identities cannot be joined to the base.
pub fn normalize_jsonld_ids(payload: &Json, base_url: &str) -> Result<Json> {
    nodes::normalize_ids(payload, base_url, true, 0)
}

/// Name the two syntax mistakes that most often void a JSON-LD block (comments, trailing
/// commas); `""` when none.
pub fn json_syntax_hint(text: &str) -> String {
    let chars: Vec<char> = text.chars().collect();
    let block_comment = text
        .find("/*")
        .is_some_and(|at| text[at + 2..].contains("*/"));
    let line_comment = chars.windows(2).enumerate().any(|(i, w)| {
        w == ['/', '/']
            && (i == 0 || {
                let prev = chars[i - 1];
                prev != ':' && !is_word_char(prev)
            })
    });
    let trailing_comma = chars.iter().enumerate().any(|(i, c)| {
        *c == ','
            && chars[i + 1..]
                .iter()
                .find(|n| !is_py_space(**n))
                .is_some_and(|n| matches!(n, '}' | ']'))
    });
    let mut causes = Vec::new();
    if block_comment || line_comment {
        causes.push("JSON permits no comments");
    }
    if trailing_comma {
        causes.push("JSON permits no trailing comma");
    }
    if causes.is_empty() {
        String::new()
    } else {
        format!(" \u{2014} {}", causes.join("; "))
    }
}

/// Where the page comes from: a URL to fetch or HTML the caller already holds.
#[derive(Debug, Clone, Copy)]
pub enum SchemaSource<'a> {
    /// Fetch through the HTTP seam.
    Url(&'a str),
    /// Validate this HTML directly.
    Html(&'a str),
}

/// Normalise a URL: an absolute HTTP(S) URL without credentials, or `""`.
fn normalize_url(value: &str) -> String {
    let raw = py_strip(value);
    if raw.is_empty() {
        return String::new();
    }
    let scheme_prefix = raw.split(':').next().unwrap_or("").to_lowercase();
    // Slice by the character length of the lowered prefix: some characters, such as `İ`,
    // lower to more characters than they occupy in `raw`.
    let after: String = raw.chars().skip(scheme_prefix.chars().count()).collect();
    if !raw.contains("//")
        && !scheme_prefix.is_empty()
        && scheme_prefix.chars().all(char::is_alphabetic)
        && after.starts_with(':')
    {
        return String::new();
    }
    let raw = if raw.contains("//") {
        raw.to_owned()
    } else {
        format!("https://{raw}")
    };
    let Ok(parts) = crate::py_url::urlsplit(&raw) else {
        return String::new();
    };
    if !matches!(parts.scheme.as_str(), "http" | "https")
        || parts.netloc.is_empty()
        || parts.netloc.contains('@')
    {
        return String::new();
    }
    raw
}

fn extract_blocks(soup: &crate::dom::Soup) -> (Vec<Json>, Vec<String>, usize) {
    let mut blocks = Vec::new();
    let mut errors = Vec::new();
    let mut found = 0;
    for (i, tag) in meta::live_jsonld_tags(soup).iter().enumerate() {
        let index = i + 1;
        found += 1;
        let raw = crate::dom::get_text(tag, "");
        let text = py_strip(&raw);
        if text.is_empty() {
            errors.push(format!("JSON-LD block #{index} is empty"));
            continue;
        }
        match pyjson::loads(text) {
            Ok(value) => blocks.push(value),
            Err(err) => {
                let full = err.0;
                let (msg, line) = match full.rfind(": line ") {
                    Some(at) => {
                        let line: String = full[at + 7..]
                            .chars()
                            .take_while(char::is_ascii_digit)
                            .collect();
                        (full[..at].to_owned(), line)
                    }
                    None => (full.clone(), "1".to_owned()),
                };
                errors.push(format!(
                    "JSON-LD block #{index} cannot be parsed: {msg} (line {line}){}",
                    json_syntax_hint(text)
                ));
            }
        }
    }
    (blocks, errors, found)
}
