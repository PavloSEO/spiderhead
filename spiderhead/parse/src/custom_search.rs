//! A query language over a crawled corpus: presence and absence checks.
//! Pure. Pages whose fetch failed are dropped
//! from numerator and denominator alike.

use serde::{Deserialize, Serialize};

use crate::bs4_out::outer_html;
use crate::content_area::TEXT_EXCLUDED_TAGS;
use crate::dom::{find, parse_document, visible_strings, Selector};
use crate::duplicate::round4;
use crate::parser::shared::collapse_whitespace;
use crate::py_json::float_repr;
use crate::py_str::py_repr;
use crate::types::{CorpusDocument, Json, ParseError, Result};
use crate::xpath;

/// Search scopes.
pub const SCOPES: [&str; 8] = [
    "raw",
    "text",
    "element",
    "xpath",
    "raw_html",
    "head_markup",
    "body_text",
    "selector_markup",
];
/// Filter modes.
pub const MODES: [&str; 2] = ["contains", "not_contains"];
/// Query kinds.
pub const KINDS: [&str; 2] = ["text", "regex"];

/// One filter specification.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct FilterSpec {
    /// Label; defaults to `query`.
    #[serde(default)]
    pub name: Option<String>,
    /// `contains` (default) or `not_contains`.
    #[serde(default)]
    pub mode: Option<String>,
    /// `text` (default) or `regex`.
    #[serde(default)]
    pub kind: Option<String>,
    /// Scope (default `text`).
    #[serde(default)]
    pub scope: Option<String>,
    /// Text or regex.
    #[serde(default)]
    pub query: String,
    /// CSS selector / XPath for element-ish scopes.
    #[serde(default)]
    pub selector: String,
    /// Default false.
    #[serde(default)]
    pub case_sensitive: bool,
}

/// Result of one filter.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct FilterResult {
    /// Label.
    pub name: String,
    /// Mode.
    pub mode: String,
    /// Kind.
    pub kind: String,
    /// Scope.
    pub scope: String,
    /// Query.
    pub query: String,
    /// `static_markup`, `rendered_dom` or a sorted list of both.
    pub representation: Json,
    /// Fetched documents.
    pub pages_considered: u64,
    /// Fetch failures excluded.
    pub pages_excluded_fetch_failed: u64,
    /// URLs that satisfy the filter.
    pub matching_pages: Vec<String>,
    /// `matching_pages.len()`.
    pub count: u64,
    /// Rounded to 4 places; 0.0 when nothing was fetched.
    pub fraction: f64,
}

/// `run_search` result.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SearchReport {
    /// Always true.
    pub ok: bool,
    /// One result per filter.
    pub filters: Vec<FilterResult>,
}

fn invalid<T>(message: String) -> Result<T> {
    Err(ParseError::Invalid(message))
}

/// Scope names rendered as a tuple literal, the form the error messages quote.
fn tuple_repr(items: &[&str]) -> String {
    let inner: Vec<String> = items.iter().map(|i| py_repr(i)).collect();
    if inner.len() == 1 {
        format!("({},)", inner[0])
    } else {
        format!("({})", inner.join(", "))
    }
}

/// Collapsed visible body text, stripped of every non-prose element.
fn visible_text(html: &str) -> String {
    let soup = parse_document(html);
    let root = find(&soup, "body").map_or_else(|| soup.tree.root(), |b| *b);
    collapse_whitespace(&visible_strings(root, &TEXT_EXCLUDED_TAGS).join(" "))
}

fn css(selector: &str) -> std::result::Result<Selector, String> {
    Selector::parse(selector).map_err(|e| e.to_string())
}

/// Reject a CSS selector whose syntax is invalid before any document is scanned.
pub fn validate_css_selector(selector: &str) -> Result<()> {
    validate_selector_syntax("element", selector)
}

/// Text for one scope of a document that carries only its retained HTML. `body_text`
/// resolves through the visible-text rules (no text column exists).
/// Scopes: `raw_html`, `head_markup`, `body_text`, `selector_markup`.
pub fn html_target_text(html: &str, scope: &str, selector: &str) -> Result<String> {
    Ok(match scope {
        "raw_html" => html.to_owned(),
        "body_text" => visible_text(html),
        "head_markup" => head_markup(html),
        "selector_markup" => selector_markup(html, selector),
        other => {
            return Err(ParseError::Invalid(format!(
                "unknown scope {}",
                py_repr(other)
            )))
        }
    })
}

/// Reject a selector whose syntax is invalid, once, before any document is scanned.
fn validate_selector_syntax(scope: &str, selector: &str) -> Result<()> {
    if scope == "element" {
        css(selector).map_err(|e| {
            ParseError::Invalid(format!("invalid CSS selector {}: {e}", py_repr(selector)))
        })?;
    } else if scope == "xpath" {
        xpath::XPath::compile(selector).map_err(|e| {
            ParseError::Invalid(format!(
                "invalid XPath expression {}: {}",
                py_repr(selector),
                e.message()
            ))
        })?;
    }
    Ok(())
}

fn element_text(html: &str, selector: &str) -> String {
    let soup = parse_document(html);
    let Ok(sel) = css(selector) else {
        return String::new();
    };
    soup.select(&sel)
        .map(|m| crate::dom::get_text(&m, " "))
        .collect::<Vec<_>>()
        .join(" ")
}

fn head_markup(html: &str) -> String {
    let soup = parse_document(html);
    find(&soup, "head").map_or_else(String::new, |h| outer_html(&h))
}

fn selector_markup(html: &str, selector: &str) -> String {
    let soup = parse_document(html);
    let Ok(sel) = css(selector) else {
        return String::new();
    };
    soup.select(&sel)
        .map(|n| outer_html(&n))
        .collect::<Vec<_>>()
        .join("\n")
}

fn xpath_text(html: &str, expression: &str) -> Result<String> {
    let doc = xpath::parse_lxml(html);
    let Ok(Some(value)) = xpath::run(expression, &doc) else {
        return Ok(String::new());
    };
    Ok(match value {
        xpath::XValue::Str(s) => s,
        xpath::XValue::Num(n) => float_repr(n),
        xpath::XValue::Bool(b) => if b { "True" } else { "False" }.to_owned(),
        xpath::XValue::Nodes(nodes) => {
            let mut parts = Vec::new();
            for node in nodes {
                if xpath::is_comment(&doc, node) {
                    // `itertext()` refuses a comment node and the error is not caught upstream.
                    return Err(ParseError::Invalid(
                        "Input object is not an XML element: lxml.etree._Comment".to_owned(),
                    ));
                }
                parts.extend(xpath::node_text(&doc, node));
            }
            parts.join(" ")
        }
    })
}

fn target_text(document: &CorpusDocument, scope: &str, selector: &str) -> Result<String> {
    let html = document.html.as_deref().unwrap_or("");
    Ok(match scope {
        "raw" | "raw_html" => html.to_owned(),
        "text" | "body_text" => document.text.clone().unwrap_or_else(|| visible_text(html)),
        "head_markup" => head_markup(html),
        "selector_markup" => selector_markup(html, selector),
        "element" => element_text(html, selector),
        _ => xpath_text(html, selector)?,
    })
}

fn matches_query(
    target: &str,
    regex: Option<&fancy_regex::Regex>,
    query: &str,
    case_sensitive: bool,
) -> bool {
    if let Some(re) = regex {
        return re.is_match(target).unwrap_or(false);
    }
    if case_sensitive {
        target.contains(query)
    } else {
        target.to_lowercase().contains(&query.to_lowercase())
    }
}

/// `"rendered_dom"` / `"static_markup"` per document, one string or a sorted list.
pub(crate) fn representation(documents: &[&CorpusDocument]) -> Json {
    let mut stamps: Vec<&str> = documents
        .iter()
        .map(|d| {
            if d.rendered {
                "rendered_dom"
            } else {
                "static_markup"
            }
        })
        .collect();
    stamps.sort_unstable();
    stamps.dedup();
    if stamps.len() == 1 {
        Json::from(stamps[0])
    } else {
        Json::from(stamps)
    }
}

/// Apply one filter. An invalid mode, kind, scope or regex yields `Invalid`.
pub fn run_filter(documents: &[CorpusDocument], spec: &FilterSpec) -> Result<FilterResult> {
    let query = spec.query.as_str();
    let name = spec
        .name
        .clone()
        .filter(|n| !n.is_empty())
        .unwrap_or_else(|| query.to_owned());
    let mode = spec.mode.as_deref().unwrap_or("contains");
    let kind = spec.kind.as_deref().unwrap_or("text");
    let scope = spec.scope.as_deref().unwrap_or("text");
    let selector = spec.selector.as_str();

    if !MODES.contains(&mode) {
        return invalid(format!(
            "unknown mode {}; expected one of {}",
            py_repr(mode),
            tuple_repr(&MODES)
        ));
    }
    if !KINDS.contains(&kind) {
        return invalid(format!(
            "unknown kind {}; expected one of {}",
            py_repr(kind),
            tuple_repr(&KINDS)
        ));
    }
    if !SCOPES.contains(&scope) {
        return invalid(format!(
            "unknown scope {}; expected one of {}",
            py_repr(scope),
            tuple_repr(&SCOPES)
        ));
    }
    if matches!(scope, "element" | "xpath" | "selector_markup") && selector.is_empty() {
        return invalid(format!("scope {} requires a selector", py_repr(scope)));
    }
    if matches!(scope, "element" | "selector_markup") {
        validate_selector_syntax("element", selector)?;
    } else if scope == "xpath" {
        validate_selector_syntax(scope, selector)?;
    }
    let regex = if kind == "regex" {
        let compiled = crate::py_regex::build(query, !spec.case_sensitive, None)
            .map_err(|e| ParseError::Invalid(format!("invalid regex {}: {e}", py_repr(query))))?;
        Some(compiled)
    } else {
        None
    };

    let fetched: Vec<&CorpusDocument> = documents.iter().filter(|d| d.ok).collect();
    let mut hits: Vec<&str> = Vec::new();
    for doc in &fetched {
        let target = target_text(doc, scope, selector)?;
        if matches_query(&target, regex.as_ref(), query, spec.case_sensitive) {
            hits.push(doc.url.as_str());
        }
    }
    let matching: Vec<String> = if mode == "contains" {
        hits.iter().map(|u| (*u).to_owned()).collect()
    } else {
        let hit_set: std::collections::HashSet<&str> = hits.iter().copied().collect();
        fetched
            .iter()
            .filter(|d| !hit_set.contains(d.url.as_str()))
            .map(|d| d.url.clone())
            .collect()
    };
    let fraction = if fetched.is_empty() {
        0.0
    } else {
        round4(matching.len() as f64 / fetched.len() as f64)
    };
    Ok(FilterResult {
        name,
        mode: mode.to_owned(),
        kind: kind.to_owned(),
        scope: scope.to_owned(),
        query: query.to_owned(),
        representation: representation(&fetched),
        pages_considered: fetched.len() as u64,
        pages_excluded_fetch_failed: (documents.len() - fetched.len()) as u64,
        count: matching.len() as u64,
        matching_pages: matching,
        fraction,
    })
}

/// Apply every filter. Pure; no network access.
pub fn run_search(documents: &[CorpusDocument], filters: &[FilterSpec]) -> Result<SearchReport> {
    let mut out = Vec::with_capacity(filters.len());
    for spec in filters {
        out.push(run_filter(documents, spec)?);
    }
    Ok(SearchReport {
        ok: true,
        filters: out,
    })
}
