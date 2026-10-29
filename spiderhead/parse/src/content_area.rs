//! Content-area resolution for word counts, duplicate and language checks.
//! Pure resolution of the content area; no network access.

use scraper::Html;

use crate::dom::{ElementRef, Selector, Soup};
use crate::parser::shared::is_inert_template_content;
use crate::types::{ContentAreaConfig, ParseError, Result};

/// Tags removed from the resolved root by default (menus, mastheads, sidebars, footers).
pub const DEFAULT_EXCLUDE_TAGS: [&str; 4] = ["nav", "header", "aside", "footer"];
/// Auto-detection order: `(css selector, strategy name)`.
pub const AUTO_STRATEGIES: [(&str, &str); 3] = [
    ("main", "auto_main"),
    ("[role=\"main\"]", "auto_role_main"),
    ("article", "auto_article"),
];
/// Tags whose text is never visible prose.
pub const TEXT_EXCLUDED_TAGS: [&str; 6] =
    ["script", "style", "noscript", "template", "svg", "math"];

/// The selected content root on the live tree plus how it was picked.
#[derive(Debug, Clone)]
pub struct ContentRoot<'a> {
    /// Root element on the untouched document (identity-comparable with link ancestors);
    /// `None` when the document has no `<body>` and the whole document is the root.
    pub element: Option<ElementRef<'a>>,
    /// `include_selector`, `root_selector`, `auto_main`, `auto_role_main`, `auto_article`,
    /// `default_body` or `fallback_default_body`.
    pub strategy: String,
}

/// The content area as a detached copy with excluded elements stripped.
#[derive(Debug, Clone)]
pub struct ResolvedArea {
    /// Detached, stripped copy of the selected root.
    pub fragment: Html,
    /// Same strategy names as [`ContentRoot::strategy`].
    pub strategy: String,
}

impl ResolvedArea {
    /// The root element of the detached copy, for callers that render it (Markdown). For a
    /// body-less document this is its first element.
    pub fn root(&self) -> ElementRef<'_> {
        // The fragment always holds at least one element: `resolve_content_area` copies one, or
        // the whole document, whose parser always yields an `<html>` for non-empty input.
        self.fragment
            .tree
            .root()
            .children()
            .find_map(ElementRef::wrap)
            .unwrap_or_else(|| self.fragment.root_element())
    }
}

fn selector(text: &str) -> Result<Selector> {
    Selector::parse(text).map_err(|e| ParseError::Selector(format!("{text}: {e}")))
}

/// First element matching `css` that is not inside a `<template>` (`first_visible`).
fn first_visible<'a>(soup: &'a Soup, css: &str) -> Result<Option<ElementRef<'a>>> {
    let sel = selector(css)?;
    Ok(soup.select(&sel).find(|c| !is_inert_template_content(c)))
}

/// Select the configured content area on the live tree; nothing is mutated.
/// A selector that does not parse is an error.
pub fn find_content_root<'a>(
    soup: &'a Soup,
    config: Option<&ContentAreaConfig>,
) -> Result<ContentRoot<'a>> {
    let include = config
        .and_then(|c| c.include_selector.as_deref())
        .filter(|s| !s.is_empty());
    let root = config
        .and_then(|c| c.root_selector.as_deref())
        .filter(|s| !s.is_empty());
    let mut requested_but_missing = false;
    for (css, strategy) in [(include, "include_selector"), (root, "root_selector")] {
        if let Some(css) = css {
            if let Some(found) = first_visible(soup, css)? {
                return Ok(ContentRoot {
                    element: Some(found),
                    strategy: strategy.to_owned(),
                });
            }
            requested_but_missing = true;
        }
    }
    if !requested_but_missing {
        for (css, strategy) in AUTO_STRATEGIES {
            if let Some(found) = first_visible(soup, css)? {
                return Ok(ContentRoot {
                    element: Some(found),
                    strategy: strategy.to_owned(),
                });
            }
        }
    }
    let strategy = if requested_but_missing {
        "fallback_default_body"
    } else {
        "default_body"
    };
    Ok(ContentRoot {
        element: crate::dom::find(soup, "body"),
        strategy: strategy.to_owned(),
    })
}

/// Attribute written onto copied `<iframe>`s so they can be recognised in the stripped copy.
pub(crate) const FRAME_MARKER: &str = "data-seohead-frame";

/// Deep-copy `source` (and its subtree) under `target`; excluded descendants are left out and
/// iframes found in `marks` get [`FRAME_MARKER`] with their document-order index.
fn copy_subtree(
    source: ego_tree::NodeRef<'_, scraper::Node>,
    target: &mut ego_tree::NodeMut<'_, scraper::Node>,
    skip: &dyn Fn(&scraper::node::Element) -> bool,
    marks: &std::collections::HashMap<ego_tree::NodeId, usize>,
    is_root: bool,
) {
    let mut value = source.value().clone();
    if let scraper::Node::Element(el) = &value {
        if !is_root && skip(el) {
            return;
        }
        let container = is_root
            .then(|| {
                source
                    .ancestors()
                    .filter_map(ElementRef::wrap)
                    .find_map(|a| crate::dom::container_kind(a.value().name()))
            })
            .flatten();
        let index = marks.get(&source.id());
        if index.is_some() || container.is_some() {
            let mut attributes: Vec<html5ever::Attribute> = el
                .attrs()
                .map(|(k, v)| html5ever::Attribute {
                    name: html5ever::QualName::new(
                        None,
                        html5ever::ns!(),
                        html5ever::LocalName::from(k),
                    ),
                    value: html5ever::tendril::StrTendril::from_slice(v),
                })
                .collect();
            let mut extra = |name: &str, value: &str| {
                attributes.push(html5ever::Attribute {
                    name: html5ever::QualName::new(
                        None,
                        html5ever::ns!(),
                        html5ever::LocalName::from(name),
                    ),
                    value: html5ever::tendril::StrTendril::from_slice(value),
                });
            };
            if let Some(index) = index {
                extra(FRAME_MARKER, &index.to_string());
            }
            if let Some(kind) = container {
                extra(crate::dom::CONTAINER_MARKER, kind);
            }
            value =
                scraper::Node::Element(scraper::node::Element::new(el.name.clone(), attributes));
        }
    }
    let mut copy = target.append(value);
    for child in source.children() {
        copy_subtree(child, &mut copy, skip, marks, false);
    }
}

/// Detached, stripped copy of the content area (word count, duplicates, language).
pub fn resolve_content_area(
    soup: &Soup,
    config: Option<&ContentAreaConfig>,
) -> Result<ResolvedArea> {
    resolve(soup, config, false)
}

/// [`resolve_content_area`] that also tags every copied `<iframe>` with [`FRAME_MARKER`].
pub(crate) fn resolve_content_area_marked(
    soup: &Soup,
    config: Option<&ContentAreaConfig>,
) -> Result<ResolvedArea> {
    resolve(soup, config, true)
}

fn resolve(
    soup: &Soup,
    config: Option<&ContentAreaConfig>,
    mark_frames: bool,
) -> Result<ResolvedArea> {
    let marks: std::collections::HashMap<ego_tree::NodeId, usize> = if mark_frames {
        crate::dom::find_all(soup, "iframe")
            .enumerate()
            .map(|(i, f)| (f.id(), i))
            .collect()
    } else {
        std::collections::HashMap::new()
    };
    let found = find_content_root(soup, config)?;
    let default_tags: Vec<String> = DEFAULT_EXCLUDE_TAGS
        .iter()
        .map(|t| (*t).to_owned())
        .collect();
    let exclude_tags = config
        .and_then(|c| c.exclude_tags.clone())
        .unwrap_or(default_tags);
    let skip = |el: &scraper::node::Element| exclude_tags.iter().any(|t| t == el.name());
    let mut fragment = Html::new_fragment();
    {
        let mut root = fragment.tree.root_mut();
        match found.element {
            Some(el) => copy_subtree(*el, &mut root, &skip, &marks, true),
            None => {
                for child in soup.tree.root().children() {
                    copy_subtree(child, &mut root, &skip, &marks, true);
                }
            }
        }
    }
    // `exclude_selectors` run one by one on the copy, each over the tree as the previous left it.
    for css in config
        .and_then(|c| c.exclude_selectors.as_ref())
        .into_iter()
        .flatten()
    {
        let sel = selector(css)?;
        let roots: Vec<_> = fragment.tree.root().children().map(|n| n.id()).collect();
        let doomed: Vec<_> = roots
            .iter()
            .filter_map(|&id| fragment.tree.get(id).and_then(ElementRef::wrap))
            .flat_map(|root| {
                root.select(&sel)
                    .filter(move |m| *m != root)
                    .map(|m| m.id())
                    .collect::<Vec<_>>()
            })
            .collect();
        for id in doomed {
            if let Some(mut node) = fragment.tree.get_mut(id) {
                node.detach();
            }
        }
    }
    Ok(ResolvedArea {
        fragment,
        strategy: found.strategy,
    })
}

/// Collapsed visible text of an already-resolved content area (skips [`TEXT_EXCLUDED_TAGS`]).
pub fn extract_area_text(area: &ResolvedArea) -> String {
    let mut parts = Vec::new();
    for root in area.fragment.tree.root().children() {
        parts.extend(crate::dom::visible_strings(root, &TEXT_EXCLUDED_TAGS));
    }
    crate::parser::shared::collapse_whitespace(&parts.join(" "))
}
