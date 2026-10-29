//! Raw-HTML scanners: facts the parse tree repairs away must be read from the source tokens.
//! One small tolerant tokenizer; RCDATA and rawtext content is opaque.

use super::meta;
use super::pyscan::{scan, Event};
use super::shared::is_inert_template_content;
use crate::dom::{find, find_all, ElementRef, Soup};
use crate::models::DocumentPosition;
use crate::py_str::py_strip;
use crate::urlutil::urljoin;

/// Tags allowed inside `<head>`.
pub const ALLOWED_HEAD_TAGS: [&str; 8] = [
    "title", "base", "link", "meta", "style", "script", "noscript", "template",
];

/// Either a parsed tree or raw HTML source.
#[derive(Debug, Clone, Copy)]
pub enum DocumentSource<'a> {
    /// A built tree.
    Tree(&'a Soup),
    /// Raw markup scanned without building a tree.
    Raw(&'a str),
}

/// Document base URL: the first live `<base href>` resolved against `final_url`, else
/// `final_url`. A `<base>` inside `<template>` is not live.
pub fn document_base_url(document: DocumentSource<'_>, final_url: &str) -> String {
    let href = match document {
        DocumentSource::Raw(html) => live_base_href(html).unwrap_or_default(),
        DocumentSource::Tree(soup) => find_all(soup, "base")
            .filter(|tag| !is_inert_template_content(tag))
            .map(|tag| py_strip(tag.value().attr("href").unwrap_or("")).to_owned())
            .find(|candidate| !candidate.is_empty())
            .unwrap_or_default(),
    };
    if href.is_empty() {
        return final_url.to_owned();
    }
    urljoin(final_url, &href).unwrap_or_else(|_| final_url.to_owned())
}

/// The stripped `href` of the first `<base>` outside any `<template>`, for raw HTML.
fn live_base_href(html: &str) -> Option<String> {
    let mut template_depth = 0usize;
    let mut found = None;
    scan(html, true, |event| {
        match event {
            Event::Start { tag, attrs, .. } => {
                if tag == "template" {
                    template_depth += 1;
                } else if found.is_none() && template_depth == 0 && tag == "base" {
                    found = attrs.into_iter().find_map(|(name, value)| {
                        let value = value?;
                        (name == "href" && !py_strip(&value).is_empty())
                            .then(|| py_strip(&value).to_owned())
                    });
                }
            }
            Event::End(tag) if tag == "template" && template_depth > 0 => template_depth -= 1,
            _ => {}
        }
        found.is_none()
    });
    found
}

/// Tag names written inside `<head>...</head>` that do not belong there, in source order.
pub fn invalid_head_elements(html: &str) -> Vec<String> {
    let mut found: Vec<String> = Vec::new();
    let mut in_head = false;
    let mut done = false;
    let mut template_depth = 0usize;
    scan(html, true, |event| {
        match event {
            Event::Start { tag, .. } => {
                if !in_head {
                    in_head = tag == "head";
                } else if template_depth == 0 && tag == "body" {
                    done = true;
                } else {
                    if tag == "template" {
                        template_depth += 1;
                    }
                    if template_depth == 0
                        && !ALLOWED_HEAD_TAGS.contains(&tag.as_str())
                        && !found.contains(&tag)
                    {
                        found.push(tag);
                    }
                }
            }
            Event::End(tag) if in_head => {
                if tag == "template" && template_depth > 0 {
                    template_depth -= 1;
                } else if template_depth == 0 && tag == "head" {
                    done = true;
                }
            }
            _ => {}
        }
        !done
    });
    found
}

fn in_head(tag: &ElementRef<'_>) -> bool {
    tag.ancestors()
        .filter_map(ElementRef::wrap)
        .any(|a| a.value().name() == "head")
}

/// True when some element under `<html>` precedes its `<head>`.
fn head_not_first(html_tag: Option<ElementRef<'_>>, head_count: usize) -> bool {
    let Some(html_tag) = html_tag else {
        return false;
    };
    if head_count == 0 {
        return false;
    }
    html_tag
        .children()
        .find_map(ElementRef::wrap)
        .is_some_and(|first| first.value().name() != "head")
}

/// Where key elements sit relative to `<head>`, as the parsed tree resolved them.
pub fn document_position(soup: &Soup, html: &str) -> DocumentPosition {
    let head_count = find_all(soup, "head").count();
    let outside = |tag: Option<ElementRef<'_>>| tag.map(|t| !in_head(&t));
    let all_outside =
        |tags: Vec<ElementRef<'_>>| (!tags.is_empty()).then(|| tags.iter().all(|t| !in_head(t)));
    DocumentPosition {
        head_count: head_count as u64,
        body_count: find_all(soup, "body").count() as u64,
        head_not_first: head_not_first(find(soup, "html"), head_count),
        invalid_head_elements: invalid_head_elements(html),
        title_outside_head: outside(meta::title_tag(soup)),
        meta_description_outside_head: outside(meta::first_meta_tag(soup, "description")),
        canonical_outside_head: outside(meta::canonical_tag(soup)),
        directives_outside_head: all_outside(meta::robots_directive_tags(soup)),
        hreflang_outside_head: all_outside(meta::hreflang_tags(soup)),
    }
}
