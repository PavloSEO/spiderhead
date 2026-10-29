//! Pure structural AMP and conservative static-HTML accessibility evidence.
//! Not an AMP validator nor an accessible-name engine.
//! AMP facts read the raw source tags (a tolerant tokenizer, not the tree builder's repaired
//! head/body), so this module runs its own tolerant tokenizer over the source.

use std::collections::HashMap;

use serde::{Deserialize, Serialize};
use serde_json::{json, Map};

use crate::dom::{all_elements, find, ElementRef, Soup};
use crate::parser::pyscan::{scan, Event};
use crate::parser::shared::{char_prefix, collapse_whitespace};
use crate::py_str::py_strip;
use crate::types::{Json, Result};

mod aria_named;
pub use aria_named::*;

const SAMPLE_LIMIT: usize = 20;
const VOID_TAGS: [&str; 14] = [
    "area", "base", "br", "col", "embed", "hr", "img", "input", "link", "meta", "param", "source",
    "track", "wbr",
];

/// AMP issue codes in reporting order.
pub const AMP_CODES: [&str; 7] = [
    "AMP_INVALID_DOCTYPE",
    "AMP_MISSING_HEAD",
    "AMP_MISSING_BODY",
    "AMP_INVALID_CHARSET",
    "AMP_INVALID_VIEWPORT",
    "AMP_INVALID_RUNTIME",
    "AMP_INVALID_BOILERPLATE",
];
/// Accessibility issue codes in reporting order.
pub const A11Y_CODES: [&str; 5] = [
    "A11Y_HTML_LANG_MISSING",
    "A11Y_FORM_LABEL_MISSING",
    "A11Y_IMAGE_ALT_MISSING",
    "A11Y_HEADING_ORDER",
    "A11Y_MAIN_LANDMARK_MISSING",
];
/// Elements whose content is not rendered.
pub const INERT_TAGS: [&str; 4] = ["template", "script", "style", "noscript"];

/// Accessibility block of [`MarkupFacts`].
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AccessibilityFacts {
    /// Always `static_html`.
    pub method: String,
    /// Issue records `{code,count,samples,samples_truncated}` for codes seen.
    pub issues: Vec<Json>,
    /// Samples kept per code (20).
    pub sample_limit: u64,
    /// What this method cannot measure.
    pub unavailable: Vec<String>,
}

/// `{"amp": ..., "accessibility": ...}` evidence bag stored as `ParsedPage.markup`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct MarkupFacts {
    /// AMP structural facts.
    pub amp: Json,
    /// Accessibility facts.
    pub accessibility: AccessibilityFacts,
}

/// One tag written in the source `<head>`.
struct Record {
    tag: String,
    attrs: Vec<(String, Option<String>)>,
    parent: String,
    /// The open-tag path is only ever compared to `html>head` and `html>head>noscript`;
    /// every record already sits under `html>head`, so one bit holds that answer.
    boilerplate_path: bool,
    line: usize,
    column: usize,
    has_content: bool,
}

impl Record {
    fn attr(&self, name: &str) -> Option<&str> {
        self.attrs
            .iter()
            .rev()
            .find(|(k, _)| k == name)
            .and_then(|(_, v)| v.as_deref())
    }

    fn has_attr(&self, name: &str) -> bool {
        self.attrs.iter().any(|(k, _)| k == name)
    }

    fn sample(&self) -> Json {
        let mut attrs = Map::new();
        for (name, value) in &self.attrs {
            if [
                "charset",
                "name",
                "content",
                "async",
                "src",
                "type",
                "amp-boilerplate",
            ]
            .contains(&name.as_str())
            {
                attrs.insert(
                    name.clone(),
                    value
                        .as_deref()
                        .map_or(Json::Null, |v| json!(char_prefix(v, 300))),
                );
            }
        }
        json!({"tag": self.tag, "attrs": attrs, "line": self.line, "column": self.column,
               "has_content": self.has_content})
    }
}

/// Explicit source tags, as a tolerant tokenizer reads them.
struct DocumentMarkup {
    stack: Vec<String>,
    root: Option<Vec<(String, Option<String>)>>,
    doctype: Option<String>,
    head: bool,
    body: bool,
    records: Vec<Record>,
    first_head_child: Option<usize>,
    head_tags: Vec<usize>,
    content_started: bool,
    /// Byte offset up to which `line`/`column` have been computed; tag positions are
    /// monotonic, so the source prefix is walked once across all records.
    scanned: usize,
    line: usize,
    column: usize,
}

impl Default for DocumentMarkup {
    fn default() -> Self {
        Self {
            stack: Vec::new(),
            root: None,
            doctype: None,
            head: false,
            body: false,
            records: Vec::new(),
            first_head_child: None,
            head_tags: Vec::new(),
            content_started: false,
            scanned: 0,
            line: 1,
            column: 0,
        }
    }
}

impl DocumentMarkup {
    fn start(&mut self, html: &str, tag: &str, attrs: Vec<(String, Option<String>)>, pos: usize) {
        if tag == "html" && self.stack.is_empty() && self.root.is_none() {
            self.root = Some(attrs.clone());
        }
        if self.stack == ["html"] {
            self.head |= tag == "head";
            self.body |= tag == "body";
        }
        if self.stack.len() >= 2
            && self.stack[0] == "html"
            && self.stack[1] == "head"
            && !self.stack.iter().any(|t| t == "template")
        {
            let before = &html[self.scanned..pos];
            for c in before.chars() {
                if c == '\n' {
                    self.line += 1;
                    self.column = 0;
                } else {
                    self.column += 1;
                }
            }
            self.scanned = pos;
            let line = self.line;
            let column = self.column;
            self.records.push(Record {
                tag: tag.to_owned(),
                attrs,
                parent: self.stack.last().cloned().unwrap_or_default(),
                boilerplate_path: self.stack.len() == 2
                    || (self.stack.len() == 3 && self.stack[2] == "noscript"),
                line,
                column,
                has_content: false,
            });
            let index = self.records.len() - 1;
            if self.stack.len() == 2 && self.first_head_child.is_none() {
                self.first_head_child = Some(index);
            }
            if matches!(tag, "meta" | "script" | "style") {
                self.head_tags.push(index);
            }
        }
        self.content_started = true;
        if !VOID_TAGS.contains(&tag) {
            self.stack.push(tag.to_owned());
        }
    }

    fn end(&mut self, tag: &str) {
        if let Some(at) = self.stack.iter().rposition(|t| t == tag) {
            self.stack.truncate(at);
        }
    }

    fn data(&mut self, data: &str) {
        if py_strip(data).is_empty() {
            return;
        }
        self.content_started = true;
        if self.stack.last().is_some_and(|t| t == "style") {
            if let Some(&last) = self.head_tags.last() {
                self.records[last].has_content = true;
            }
        }
    }
}

fn amp_facts(html: &str) -> Json {
    let mut source = DocumentMarkup::default();
    scan(html, false, |event| {
        match event {
            Event::Decl(decl) => {
                if source.stack.is_empty() && !source.content_started && source.doctype.is_none() {
                    source.doctype = Some(decl);
                }
            }
            Event::Start {
                tag,
                attrs,
                closing,
                pos,
            } => {
                source.start(html, &tag, attrs, pos);
                if closing && !VOID_TAGS.contains(&tag.as_str()) {
                    source.end(&tag);
                }
            }
            Event::End(tag) => source.end(&tag),
            Event::Data(data) => source.data(&data),
        }
        true
    });
    let declared = source
        .root
        .as_ref()
        .is_some_and(|root| root.iter().any(|(k, _)| k == "amp" || k == "⚡"));
    let mut errors: Vec<Json> = Vec::new();
    if declared {
        amp_errors(&source, &mut errors);
    }
    json!({
        "declared": declared,
        "errors": errors,
        "method": "static_structure",
        "validator_state": "unavailable",
        "validator_reason": "full AMP validator rule set unavailable pending owner licence decision",
        "unavailable": ["boilerplate CSS contents and full AMP validity not validated"],
    })
}

fn list_observed(source: &DocumentMarkup, indexes: &[usize]) -> Json {
    json!({
        "count": indexes.len(),
        "samples": indexes.iter().take(SAMPLE_LIMIT).map(|&i| source.records[i].sample()).collect::<Vec<_>>(),
        "samples_truncated": indexes.len() > SAMPLE_LIMIT,
    })
}

fn amp_errors(source: &DocumentMarkup, errors: &mut Vec<Json>) {
    let mut error = |code: &str, expected: &str, observed: Json| {
        errors.push(json!({"code": code, "expected": expected, "observed": observed}));
    };
    let doctype = source.doctype.clone().unwrap_or_default();
    if doctype
        .to_lowercase()
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        != "doctype html"
    {
        let shown = char_prefix(&doctype, 300);
        error(
            "AMP_INVALID_DOCTYPE",
            "<!doctype html> before document content",
            if shown.is_empty() {
                Json::Null
            } else {
                json!(shown)
            },
        );
    }
    if !source.head {
        error("AMP_MISSING_HEAD", "explicit head element", Json::Null);
    }
    if !source.body {
        error("AMP_MISSING_BODY", "explicit body element", Json::Null);
    }
    let first = source.first_head_child.map(|i| &source.records[i]);
    if !first.is_some_and(|f| {
        f.tag == "meta" && f.attr("charset").unwrap_or("").to_lowercase() == "utf-8"
    }) {
        error(
            "AMP_INVALID_CHARSET",
            "meta charset=utf-8 as the first head element",
            first.map_or(Json::Null, Record::sample),
        );
    }
    let tagged = |name: &str| -> Vec<usize> {
        source
            .head_tags
            .iter()
            .copied()
            .filter(|&i| source.records[i].tag == name)
            .collect()
    };
    let viewports: Vec<usize> = tagged("meta")
        .into_iter()
        .filter(|&i| {
            let r = &source.records[i];
            r.attr("name").unwrap_or("").to_lowercase() == "viewport" && r.parent == "head"
        })
        .collect();
    let width_ok = viewports.iter().any(|&i| {
        source.records[i]
            .attr("content")
            .unwrap_or("")
            .split(',')
            .any(|part| {
                part.to_lowercase().split_whitespace().collect::<String>() == "width=device-width"
            })
    });
    if !width_ok {
        error(
            "AMP_INVALID_VIEWPORT",
            "head meta viewport with width=device-width",
            list_observed(source, &viewports),
        );
    }
    let scripts = tagged("script");
    let runtime_ok = scripts.iter().any(|&i| {
        let r = &source.records[i];
        r.parent == "head"
            && r.has_attr("async")
            && r.attr("src") == Some("https://cdn.ampproject.org/v0.js")
            && matches!(
                r.attr("type").unwrap_or("").to_lowercase().as_str(),
                "" | "text/javascript" | "application/javascript"
            )
    });
    if !runtime_ok {
        error(
            "AMP_INVALID_RUNTIME",
            "async AMP v0.js script in head",
            list_observed(source, &scripts),
        );
    }
    let styles: Vec<usize> = tagged("style")
        .into_iter()
        .filter(|&i| source.records[i].has_attr("amp-boilerplate"))
        .collect();
    // Every record sits under `html>head`; the two accepted paths differ only in whether a
    // `noscript` wraps the `style`, which `boilerplate_path` plus `parent` already encode.
    let boilerplate_ok = ["head", "noscript"].iter().all(|&wrap| {
        styles.iter().any(|&i| {
            let r = &source.records[i];
            r.boilerplate_path && r.parent == wrap && r.has_content
        })
    });
    if !boilerplate_ok {
        error(
            "AMP_INVALID_BOILERPLATE",
            "nonempty head style[amp-boilerplate] and head noscript style[amp-boilerplate]",
            list_observed(source, &styles),
        );
    }
}

fn attr<'a>(tag: &ElementRef<'a>, name: &str) -> &'a str {
    tag.value().attr(name).unwrap_or("")
}

fn self_and_ancestors<'a>(tag: &ElementRef<'a>) -> impl Iterator<Item = ElementRef<'a>> {
    std::iter::once(*tag).chain(tag.ancestors().filter_map(ElementRef::wrap))
}

/// Whether `tag` is excluded from accessibility checks (inert/hidden subtree).
/// Also used by `parser::content`.
pub(crate) fn excluded(tag: &ElementRef<'_>) -> bool {
    if tag.value().name() == "input" && attr(tag, "type").to_lowercase() == "hidden" {
        return true;
    }
    self_and_ancestors(tag).any(|node| {
        let el = node.value();
        INERT_TAGS.contains(&el.name())
            || el.attr("hidden").is_some()
            || el.attr("inert").is_some()
            || attr(&node, "aria-hidden").to_lowercase() == "true"
            || attr(&node, "style").to_lowercase().split(';').any(|item| {
                let squeezed: String = item.split_whitespace().collect();
                let squeezed = squeezed.strip_suffix("!important").unwrap_or(&squeezed);
                matches!(
                    squeezed,
                    "display:none" | "visibility:hidden" | "visibility:collapse"
                )
            })
    })
}

/// Label/reference text of a control without mistaking its value for a label.
/// Also used by `parser::content`.
pub(crate) fn name_text(tag: &ElementRef<'_>) -> String {
    let label = py_strip(attr(tag, "aria-label"));
    if !label.is_empty() {
        return label.to_owned();
    }
    let mut parts: Vec<&str> = Vec::new();
    for node in tag.descendants() {
        let text: &str = match node.value() {
            scraper::Node::Text(text) => text,
            scraper::Node::Doctype(doctype) => doctype.name(),
            _ => continue,
        };
        let skipped = node.ancestors().filter_map(ElementRef::wrap).any(|n| {
            n != *tag
                && (INERT_TAGS.contains(&n.value().name())
                    || matches!(n.value().name(), "input" | "select" | "textarea"))
        });
        if !skipped {
            parts.push(text);
        }
    }
    let alts: Vec<&str> = find_in(tag, "img").map(|img| attr(&img, "alt")).collect();
    parts.extend(alts);
    collapse_whitespace(&parts.join(" "))
}

fn find_in<'a>(tag: &ElementRef<'a>, name: &'a str) -> impl Iterator<Item = ElementRef<'a>> + 'a {
    tag.descendants()
        .skip(1)
        .filter_map(ElementRef::wrap)
        .filter(move |e| e.value().name() == name)
}
