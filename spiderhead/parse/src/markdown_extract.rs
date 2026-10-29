//! Render a page as Markdown in two scopes: content area and full document.
//! Pure and deterministic: no network access.

use serde::{Deserialize, Serialize};

use ego_tree::NodeRef;
use scraper::Node;

use crate::content_area::{find_content_root, resolve_content_area, TEXT_EXCLUDED_TAGS};
use crate::dom::{find, parse_document, ElementRef};
use crate::py_str::py_strip;
use crate::types::{ContentAreaConfig, Result};

/// Result of [`extract_markdown`].
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct MarkdownExtract {
    /// Boilerplate-stripped Markdown.
    pub content_markdown: String,
    /// Whole `<body>` (or document) Markdown.
    pub full_markdown: String,
    /// Strategy name from the content-area resolution.
    pub content_area_strategy: String,
}

/// Block tags rendered as one line (or recursed into when they wrap structure).
const BLOCK_TAGS: [&str; 6] = ["p", "div", "section", "article", "li", "blockquote"];

fn heading_level(name: &str) -> Option<usize> {
    match name {
        "h1" => Some(1),
        "h2" => Some(2),
        "h3" => Some(3),
        "h4" => Some(4),
        "h5" => Some(5),
        "h6" => Some(6),
        _ => None,
    }
}

fn is_structural(name: &str) -> bool {
    heading_level(name).is_some() || matches!(name, "ul" | "ol") || BLOCK_TAGS.contains(&name)
}

/// Text, comment and doctype nodes are all treated as strings.
fn string_of<'a>(node: &NodeRef<'a, Node>) -> Option<&'a str> {
    match node.value() {
        Node::Text(t) => Some(&**t),
        Node::Comment(c) => Some(&**c),
        Node::Doctype(d) => Some(&*d.name),
        _ => None,
    }
}

/// Marker applied to `out[start..]` once the element's children have been rendered.
enum Wrap<'a> {
    /// `[{text}]({href})` or the stripped text alone.
    Link(Option<&'a str>),
    /// `**{text}**` or nothing.
    Strong,
    /// `*{text}*` or nothing.
    Em,
}

/// Render inline content: text, links, and bold/italic emphasis.
/// Iterative: untrusted nesting depth cannot overflow the call stack.
fn inline(node: NodeRef<'_, Node>) -> String {
    enum Work<'a> {
        Emit(NodeRef<'a, Node>),
        Close { start: usize, wrap: Wrap<'a> },
    }
    let mut out = String::new();
    let mut stack = vec![Work::Emit(node)];
    while let Some(work) = stack.pop() {
        match work {
            Work::Emit(node) => {
                if let Some(text) = string_of(&node) {
                    out.push_str(text);
                    continue;
                }
                let Some(el) = ElementRef::wrap(node) else {
                    continue;
                };
                let name = el.value().name();
                if TEXT_EXCLUDED_TAGS.contains(&name) {
                    continue;
                }
                let wrap = match name {
                    "br" => {
                        out.push('\n');
                        continue;
                    }
                    "a" => Some(Wrap::Link(el.value().attr("href"))),
                    "strong" | "b" => Some(Wrap::Strong),
                    "em" | "i" => Some(Wrap::Em),
                    _ => None,
                };
                if let Some(wrap) = wrap {
                    stack.push(Work::Close {
                        start: out.len(),
                        wrap,
                    });
                }
                stack.extend(el.children().rev().map(Work::Emit));
            }
            Work::Close { start, wrap } => {
                let inner = out[start..].to_owned();
                out.truncate(start);
                let text = py_strip(&inner);
                out.push_str(&match wrap {
                    Wrap::Link(Some(href)) if !href.is_empty() && !text.is_empty() => {
                        format!("[{text}]({href})")
                    }
                    Wrap::Link(_) => text.to_owned(),
                    Wrap::Strong if !text.is_empty() => format!("**{text}**"),
                    Wrap::Em if !text.is_empty() => format!("*{text}*"),
                    _ => String::new(),
                });
            }
        }
    }
    out
}

/// `has_structural_descendant(el)`: whether any element below `el` is structural.
/// The memo fills one subtree per query bottom-up, so a `walk` costs O(nodes), not
/// O(nodes x depth) from rescanning each block's subtree at every level.
fn has_structural_descendant(
    el: &ElementRef<'_>,
    memo: &mut std::collections::HashMap<ego_tree::NodeId, bool>,
) -> bool {
    if let Some(flag) = memo.get(&el.id()) {
        return *flag;
    }
    let subtree: Vec<ElementRef<'_>> = el
        .descendants()
        .skip(1)
        .filter_map(ElementRef::wrap)
        .collect();
    for d in subtree.iter().rev() {
        let flag = d.children().filter_map(ElementRef::wrap).any(|c| {
            is_structural(c.value().name()) || memo.get(&c.id()).copied().unwrap_or(false)
        });
        memo.insert(d.id(), flag);
    }
    let flag = el
        .children()
        .filter_map(ElementRef::wrap)
        .any(|c| is_structural(c.value().name()) || memo.get(&c.id()).copied().unwrap_or(false));
    memo.insert(el.id(), flag);
    flag
}

fn walk(node: NodeRef<'_, Node>, lines: &mut Vec<String>) {
    let mut structural: std::collections::HashMap<ego_tree::NodeId, bool> =
        std::collections::HashMap::new();
    let mut stack: Vec<NodeRef<'_, Node>> = node.children().rev().collect();
    while let Some(child) = stack.pop() {
        if let Some(text) = string_of(&child) {
            let text = py_strip(text);
            if !text.is_empty() {
                lines.push(text.to_owned());
            }
            continue;
        }
        let Some(el) = ElementRef::wrap(child) else {
            continue;
        };
        let name = el.value().name();
        if TEXT_EXCLUDED_TAGS.contains(&name) {
            continue;
        }
        if let Some(level) = heading_level(name) {
            let rendered = inline(child);
            let text = py_strip(&rendered);
            if !text.is_empty() {
                lines.push(format!("{} {text}", "#".repeat(level)));
            }
            continue;
        }
        if matches!(name, "ul" | "ol") {
            let items = el
                .children()
                .filter_map(ElementRef::wrap)
                .filter(|c| c.value().name() == "li");
            for (i, li) in items.enumerate() {
                let rendered = inline(*li);
                let text = py_strip(&rendered);
                if !text.is_empty() {
                    let marker = if name == "ol" {
                        format!("{}.", i + 1)
                    } else {
                        "-".to_owned()
                    };
                    lines.push(format!("{marker} {text}"));
                }
            }
            continue;
        }
        if BLOCK_TAGS.contains(&name) && has_structural_descendant(&el, &mut structural) {
            stack.extend(child.children().rev());
            continue;
        }
        if BLOCK_TAGS.contains(&name) {
            let rendered = inline(child);
            let text = py_strip(&rendered);
            if text.is_empty() {
                stack.extend(child.children().rev());
            } else {
                lines.push(text.to_owned());
            }
            continue;
        }
        stack.extend(child.children().rev());
    }
}

fn render_children_of(root: NodeRef<'_, Node>) -> String {
    let mut lines = Vec::new();
    walk(root, &mut lines);
    lines.join("\n\n")
}

/// Render an element tree as Markdown with headings, lists and links preserved.
pub fn to_markdown(root: ElementRef<'_>) -> String {
    render_children_of(*root)
}

/// Content and full Markdown for `html`.
pub fn extract_markdown(
    html: &str,
    content_area_config: Option<&ContentAreaConfig>,
) -> Result<MarkdownExtract> {
    let soup = parse_document(html);
    let body = find(&soup, "body");
    let area = resolve_content_area(&soup, content_area_config)?;
    // A body-less document resolves to a copy of the whole document: render its top level.
    let content_markdown = if find_content_root(&soup, content_area_config)?
        .element
        .is_some()
    {
        to_markdown(area.root())
    } else {
        render_children_of(area.fragment.tree.root())
    };
    let full_markdown = match body {
        Some(body) => to_markdown(body),
        None => render_children_of(soup.tree.root()),
    };
    Ok(MarkdownExtract {
        content_markdown,
        full_markdown,
        content_area_strategy: area.strategy,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::dom::{find, parse_document};

    /// Deeply nested elements must render without overflowing the call stack.
    #[test]
    fn deep_nesting_renders_iteratively() {
        let depth = 60_000usize;
        let html = format!("<html><body>{}tail</body></html>", "<div>".repeat(depth));
        let soup = parse_document(&html);
        let body = find(&soup, "body").expect("body");
        let markdown = to_markdown(body);
        assert!(markdown.ends_with("tail"), "tail is unreachable text");
    }

    /// Nested blocks keep their Markdown ordering (heading inside a div).
    #[test]
    fn nested_blocks_keep_order() {
        let html = "<html><body><div><div><h2>Deep</h2></div><p>leaf</p></div>\
<p>top</p></body></html>";
        let soup = parse_document(html);
        let body = find(&soup, "body").expect("body");
        assert_eq!(to_markdown(body), "## Deep\n\nleaf\n\ntop");
    }
}
