//! Custom extraction: pull arbitrary fields into columns with a bounded budget.
//! Modes: CSS, XPath 1.0, and regex on raw HTML.
//! Each (document, extractor) pair runs under a cooperative deadline: a regex backtrack limit
//! plus elapsed-time checks, since a signal-based alarm is not available here.

use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};

use crate::bs4_out::{contents_str, outer_html};
use crate::dom::{parse_document, Selector};
use crate::parser::shared::collapse_whitespace;
use crate::py_json::float_repr;
use crate::py_str::{py_repr, py_strip};
use crate::types::{CorpusDocument, Json, ParseError, Result};
use crate::xpath;

/// Supported modes, preferred first.
pub const MODES: [&str; 3] = ["css", "xpath", "regex"];
/// Default per-document budget in seconds.
pub const DEFAULT_TIMEOUT_SECONDS: f64 = 2.0;

/// Valid outputs of a mode (last one is the default): css/xpath -> element, html, text;
/// regex -> text, group.
pub fn outputs_for_mode(mode: &str) -> Option<&'static [&'static str]> {
    match mode {
        "css" | "xpath" => Some(&["element", "html", "text"]),
        "regex" => Some(&["text", "group"]),
        _ => None,
    }
}

/// One extractor specification (`{name?, mode?, output?, query, group?, case_sensitive?}`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ExtractorSpec {
    /// Column name; defaults to `query`.
    #[serde(default)]
    pub name: Option<String>,
    /// `css` (default), `xpath` or `regex`.
    #[serde(default)]
    pub mode: Option<String>,
    /// Output kind; defaults per mode.
    #[serde(default)]
    pub output: Option<String>,
    /// Selector / expression / pattern (required).
    #[serde(default)]
    pub query: String,
    /// Regex group index for `output=group`.
    #[serde(default)]
    pub group: Option<usize>,
    /// Regex case sensitivity; sensitive by default.
    #[serde(default)]
    pub case_sensitive: Option<bool>,
}

/// One document's extracted values.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ExtractionRow {
    /// Document URL.
    pub url: String,
    /// Extracted values.
    pub values: Vec<String>,
    /// `values.len()`.
    pub count: u64,
    /// The per-document budget was exceeded.
    pub budget_exceeded: bool,
}

/// Result of one extractor over a corpus.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ExtractorReport {
    /// Column name.
    pub name: String,
    /// Mode.
    pub mode: String,
    /// Output.
    pub output: String,
    /// Query.
    pub query: String,
    /// `static_markup`, `rendered_dom` or a sorted list of both.
    pub representation: Json,
    /// Fetched documents.
    pub pages_considered: u64,
    /// Documents dropped because the fetch failed.
    pub pages_excluded_fetch_failed: u64,
    /// Budget used.
    pub timeout_seconds: f64,
    /// One row per considered document.
    pub rows: Vec<ExtractionRow>,
    /// URLs aborted by the budget.
    pub aborted_pages: Vec<String>,
    /// Caveats (regex-on-raw-HTML, aborted pages).
    pub notes: Vec<String>,
}

/// `run_extraction` result.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ExtractionReport {
    /// Always true.
    pub ok: bool,
    /// One report per extractor.
    pub extractors: Vec<ExtractorReport>,
}

/// One extractor's outcome on a single sample document.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SampleResult {
    /// Column name.
    pub name: String,
    /// Mode.
    pub mode: String,
    /// Output.
    pub output: String,
    /// Query.
    pub query: String,
    /// Values found.
    pub values: Vec<String>,
    /// Number found.
    pub count: u64,
    /// `budget_exceeded`, `empty_html`, `no_match` or `None`.
    pub reason: Option<String>,
    /// Caveats.
    pub notes: Vec<String>,
}

/// `check_sample` result.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SampleReport {
    /// Always true.
    pub ok: bool,
    /// Label only.
    pub url: String,
    /// `static_markup`, `rendered_dom` or a sorted list of both.
    pub representation: Json,
    /// One entry per extractor.
    pub extractors: Vec<SampleResult>,
}

fn invalid<T>(message: String) -> Result<T> {
    Err(ParseError::Invalid(message))
}

fn tuple_repr(items: &[&str]) -> String {
    let inner: Vec<String> = items.iter().map(|i| py_repr(i)).collect();
    format!("({})", inner.join(", "))
}

fn extract_css(html: &str, selector: &str, output: &str) -> Result<Vec<String>> {
    let soup = parse_document(html);
    let sel =
        Selector::parse(selector).map_err(|e| ParseError::Selector(format!("{selector}: {e}")))?;
    Ok(soup
        .select(&sel)
        .map(|m| match output {
            "element" => outer_html(&m),
            "html" => contents_str(&m),
            _ => collapse_whitespace(&crate::dom::get_text(&m, " ")),
        })
        .collect())
}

fn python_error<T>(class: &str, message: String) -> Result<T> {
    Err(ParseError::Invalid(format!("{class}: {message}")))
}

fn extract_xpath(html: &str, expression: &str, output: &str) -> Result<Vec<String>> {
    let doc = xpath::parse_lxml(html);
    if !xpath::has_root(&doc) {
        return Ok(Vec::new());
    }
    let value = match xpath::run(expression, &doc) {
        Ok(Some(value)) => value,
        Ok(None) => return Ok(Vec::new()),
        Err(e) => return python_error("XPathEvalError", e.message().to_owned()),
    };
    let mut values = Vec::new();
    let scalar = |class: &str, what: &str| -> Result<Vec<String>> {
        match output {
            "element" => python_error("TypeError", format!("Type '{class}' cannot be serialized.")),
            "html" => python_error("TypeError", format!("'{class}' object is not iterable")),
            _ => python_error(
                "AttributeError",
                format!("'{class}' object has no attribute '{what}'"),
            ),
        }
    };
    match value {
        xpath::XValue::Str(s) => values.push(s),
        xpath::XValue::Num(_) => return scalar("float", "text_content"),
        xpath::XValue::Bool(_) => return scalar("bool", "text_content"),
        xpath::XValue::Nodes(nodes) => {
            for node in nodes {
                match node {
                    xpath::XNode::Attr(..) => {
                        if let Some(text) = xpath::node_text(&doc, node) {
                            values.push(text);
                        }
                    }
                    xpath::XNode::Ns(_) => return scalar("tuple", "text_content"),
                    xpath::XNode::Tree(id) => {
                        match doc.tree.get(id).map(|n| n.value()) {
                            Some(scraper::Node::Text(t)) => values.push(t.to_string()),
                            Some(scraper::Node::Element(_)) => match output {
                                "element" => values.push(xpath::node_to_xml_with_tail(&doc, id)),
                                "html" => values.push(xpath::element_to_xml(&doc, id)),
                                _ => return python_error(
                                    "AttributeError",
                                    "'lxml.etree._Element' object has no attribute 'text_content'"
                                        .to_owned(),
                                ),
                            },
                            Some(scraper::Node::Comment(_)) => match output {
                                "element" => values.push(xpath::node_to_xml_with_tail(&doc, id)),
                                "html" => values.push(String::new()),
                                _ => return python_error(
                                    "AttributeError",
                                    "'lxml.etree._Comment' object has no attribute 'text_content'"
                                        .to_owned(),
                                ),
                            },
                            _ => {}
                        }
                    }
                }
            }
        }
    }
    Ok(values)
}

/// Compiled regex plus the match loop; an empty match advances one character.
fn extract_regex(
    html: &str,
    pattern: &str,
    output: &str,
    group: i64,
    case_sensitive: bool,
    deadline: Option<Instant>,
) -> Result<Option<Vec<String>>> {
    let compiled = crate::py_regex::build(pattern, !case_sensitive, Some(BACKTRACK_LIMIT))
        .map_err(|e| ParseError::Selector(format!("{pattern}: {e}")))?;
    let groups = compiled.captures_len() as i64 - 1;
    if output == "group" && group > groups {
        return Ok(Some(Vec::new()));
    }
    if output == "group" && group < 0 {
        return python_error("IndexError", "no such group".to_owned());
    }
    let mut values = Vec::new();
    let mut pos = 0usize;
    while pos <= html.len() {
        if deadline.is_some_and(|d| Instant::now() >= d) {
            return Ok(None);
        }
        let captures = match compiled.captures_from_pos(html, pos) {
            Ok(Some(c)) => c,
            Ok(None) => break,
            Err(_) => return Ok(None),
        };
        let Some(whole) = captures.get(0) else {
            break;
        };
        if output == "group" {
            if let Some(g) = captures.get(group as usize) {
                if !g.as_str().is_empty() {
                    values.push(g.as_str().to_owned());
                }
            }
        } else {
            values.push(whole.as_str().to_owned());
        }
        pos = if whole.end() == whole.start() {
            match html[whole.end()..].chars().next() {
                Some(c) => whole.end() + c.len_utf8(),
                None => break,
            }
        } else {
            whole.end()
        };
    }
    Ok(Some(values))
}

/// Backtracking cap that bounds a pathological pattern in place of a wall-clock alarm.
const BACKTRACK_LIMIT: usize = 1_000_000;

fn representation(documents: &[&CorpusDocument]) -> Json {
    crate::custom_search::representation(documents)
}

/// Run one extractor over `documents`.
pub fn run_extractor(
    documents: &[CorpusDocument],
    spec: &ExtractorSpec,
    timeout_seconds: f64,
) -> Result<ExtractorReport> {
    let mode = spec.mode.as_deref().unwrap_or("css");
    let Some(outputs) = outputs_for_mode(mode) else {
        return invalid(format!(
            "unknown mode {}; expected one of {}",
            py_repr(mode),
            tuple_repr(&MODES)
        ));
    };
    let output = spec.output.as_deref().unwrap_or(outputs[outputs.len() - 1]);
    if !outputs.contains(&output) {
        return invalid(format!(
            "output {} is not valid for mode {}; expected one of {}",
            py_repr(output),
            py_repr(mode),
            tuple_repr(outputs)
        ));
    }
    if spec.query.is_empty() {
        return invalid("query is required".to_owned());
    }
    let name = spec
        .name
        .clone()
        .filter(|n| !n.is_empty())
        .unwrap_or_else(|| spec.query.clone());

    let fetched: Vec<&CorpusDocument> = documents.iter().filter(|d| d.ok).collect();
    let mut rows = Vec::new();
    let mut aborted: Vec<String> = Vec::new();
    for doc in &fetched {
        let html = doc.html.as_deref().unwrap_or("");
        let deadline = (timeout_seconds > 0.0)
            .then(|| Instant::now() + Duration::from_secs_f64(timeout_seconds));
        let extracted = match mode {
            "css" => Some(extract_css(html, &spec.query, output)?),
            "xpath" => Some(extract_xpath(html, &spec.query, output)?),
            _ => extract_regex(
                html,
                &spec.query,
                output,
                spec.group.map_or(1, |g| g as i64),
                spec.case_sensitive.unwrap_or(true),
                deadline,
            )?,
        };
        let timed_out = extracted.is_none();
        if timed_out {
            aborted.push(doc.url.clone());
        }
        let values = extracted.unwrap_or_default();
        rows.push(ExtractionRow {
            url: doc.url.clone(),
            count: values.len() as u64,
            values,
            budget_exceeded: timed_out,
        });
    }

    let mut notes = Vec::new();
    if mode == "regex" {
        notes.push(
            "regex runs against raw HTML: a value injected by JavaScript is invisible to it; \
             prefer css or xpath where the document supports it"
                .to_owned(),
        );
    }
    if !aborted.is_empty() {
        notes.push(format!(
            "{} document(s) exceeded the {}s budget for this extractor and were aborted; \
             their rows carry no values",
            aborted.len(),
            float_repr(timeout_seconds)
        ));
    }
    Ok(ExtractorReport {
        name,
        mode: mode.to_owned(),
        output: output.to_owned(),
        query: spec.query.clone(),
        representation: representation(&fetched),
        pages_considered: fetched.len() as u64,
        pages_excluded_fetch_failed: (documents.len() - fetched.len()) as u64,
        timeout_seconds,
        rows,
        aborted_pages: aborted,
        notes,
    })
}

/// Run every extractor over `documents`. Pure; always finishes.
pub fn run_extraction(
    documents: &[CorpusDocument],
    extractors: &[ExtractorSpec],
    timeout_seconds: f64,
) -> Result<ExtractionReport> {
    let mut out = Vec::with_capacity(extractors.len());
    for spec in extractors {
        out.push(run_extractor(documents, spec, timeout_seconds)?);
    }
    Ok(ExtractionReport {
        ok: true,
        extractors: out,
    })
}

/// Try extractors on one saved HTML document, without a scan.
pub fn check_sample(
    html: &str,
    extractors: &[ExtractorSpec],
    url: &str,
    timeout_seconds: f64,
) -> Result<SampleReport> {
    let document = CorpusDocument {
        url: url.to_owned(),
        ok: true,
        html: Some(html.to_owned()),
        text: None,
        rendered: false,
    };
    let report = run_extraction(std::slice::from_ref(&document), extractors, timeout_seconds)?;
    let has_markup = !py_strip(html).is_empty();
    let mut results = Vec::new();
    for extractor in report.extractors {
        let row = &extractor.rows[0];
        let reason = if row.budget_exceeded {
            Some("budget_exceeded")
        } else if !has_markup {
            Some("empty_html")
        } else if row.count == 0 {
            Some("no_match")
        } else {
            None
        };
        results.push(SampleResult {
            name: extractor.name.clone(),
            mode: extractor.mode.clone(),
            output: extractor.output.clone(),
            query: extractor.query.clone(),
            values: row.values.clone(),
            count: row.count,
            reason: reason.map(str::to_owned),
            notes: extractor.notes.clone(),
        });
    }
    Ok(SampleReport {
        ok: true,
        url: url.to_owned(),
        representation: representation(&[&document]),
        extractors: results,
    })
}
