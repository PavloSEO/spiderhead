//! Serialisation and text helpers over the shared tree. Output follows the HTML
//! "minimal" entity rules and `get_text(sep, strip=True)` semantics.

use scraper::{ElementRef, Node};

use crate::dom::container_kind;
use crate::py_str::{is_py_space, py_strip};

/// Void elements, written as `<tag/>` when they have no children.
const EMPTY_ELEMENT_TAGS: [&str; 24] = [
    "area", "base", "basefont", "bgsound", "br", "col", "command", "embed", "frame", "hr", "image",
    "img", "input", "isindex", "keygen", "link", "menuitem", "meta", "nextid", "param", "source",
    "spacer", "track", "wbr",
];

/// Whether `name`'s `attr` is a whitespace-separated list (`class`, `rel`, ...).
fn is_list_attribute(name: &str, attr: &str) -> bool {
    if matches!(attr, "class" | "accesskey" | "dropzone") {
        return true;
    }
    matches!(
        (name, attr),
        ("a", "rel" | "rev")
            | ("link", "rel" | "rev")
            | ("td" | "th", "headers")
            | ("form", "accept-charset")
            | ("object", "archive")
            | ("area", "rel")
            | ("icon", "sizes")
            | ("iframe", "sandbox")
            | ("output", "for")
    )
}

/// Whitespace-separated tokens joined by one space: how list attributes are written out.
fn join_list_value(value: &str) -> String {
    value
        .split(is_py_space)
        .filter(|p| !p.is_empty())
        .collect::<Vec<_>>()
        .join(" ")
}

/// Descendants to leave out of the output (`root` itself is never left out).
struct Skip<'a> {
    names: &'a [&'a str],
    root: ego_tree::NodeId,
}

fn escape_into(out: &mut String, text: &str) {
    for c in text.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            _ => out.push(c),
        }
    }
}

/// Rewrites each `charset=...` run of a `content` attribute to `utf-8`, across lines.
fn rewrite_content_charset(content: &str) -> String {
    let chars: Vec<char> = content.chars().collect();
    let mut out = String::new();
    let mut i = 0;
    while i < chars.len() {
        let line_start = i == 0 || chars[i - 1] == '\n';
        let mut hit: Option<usize> = None;
        for start in [line_start.then_some(i), (chars[i] == ';').then_some(i + 1)]
            .into_iter()
            .flatten()
        {
            let mut j = start;
            while j < chars.len() && is_py_space(chars[j]) {
                j += 1;
            }
            if chars[j..].iter().take(8).collect::<String>() == "charset=" {
                hit = Some(j + 8);
                break;
            }
        }
        match hit {
            Some(prefix_end) => {
                out.extend(&chars[i..prefix_end]);
                out.push_str("utf-8");
                i = prefix_end;
                while i < chars.len() && chars[i] != ';' {
                    i += 1;
                }
            }
            None => {
                out.push(chars[i]);
                i += 1;
            }
        }
    }
    out
}

/// The declared encoding of `<meta>` tags is rewritten to the output encoding.
fn meta_substitution(el: &scraper::node::Element, key: &str, raw: &str) -> Option<String> {
    if el.name() != "meta" {
        return None;
    }
    if el.attr("charset").is_some() {
        return (key == "charset").then(|| "utf-8".to_owned());
    }
    let http_equiv = el.attr("http-equiv")?;
    if key == "content" && http_equiv.to_lowercase() == "content-type" {
        return Some(rewrite_content_charset(raw));
    }
    None
}

fn write_attribute(out: &mut String, tag: &str, key: &str, raw: &str) {
    let joined;
    let value = if is_list_attribute(tag, key) {
        joined = join_list_value(raw);
        joined.as_str()
    } else {
        raw
    };
    let mut escaped = String::new();
    escape_into(&mut escaped, value);
    out.push(' ');
    out.push_str(key);
    out.push('=');
    // Double quotes unless the value has a double quote and no single quote.
    if escaped.contains('"') {
        if escaped.contains('\'') {
            out.push('"');
            out.push_str(&escaped.replace('"', "&quot;"));
            out.push('"');
        } else {
            out.push('\'');
            out.push_str(&escaped);
            out.push('\'');
        }
    } else {
        out.push('"');
        out.push_str(&escaped);
        out.push('"');
    }
}

/// Serialise one node, escaping text and quoting attributes with the "minimal" rules.
/// Attributes are written in name order.
fn write_node(
    out: &mut String,
    node: ego_tree::NodeRef<'_, Node>,
    parent_name: &str,
    skip: &Skip<'_>,
) {
    match node.value() {
        Node::Text(text) => {
            if matches!(parent_name, "script" | "style") {
                out.push_str(text);
            } else {
                escape_into(out, text);
            }
        }
        Node::Comment(comment) => {
            out.push_str("<!--");
            out.push_str(comment);
            out.push_str("-->");
        }
        Node::Doctype(doctype) => {
            out.push_str("<!DOCTYPE ");
            out.push_str(&doctype.name);
            out.push_str(">\n");
        }
        Node::Element(el) => {
            let name = el.name();
            if skip.names.contains(&name) && node.id() != skip.root {
                return;
            }
            out.push('<');
            out.push_str(name);
            let mut attrs: Vec<(&str, &str)> = el.attrs().collect();
            attrs.sort();
            for (key, value) in attrs {
                match meta_substitution(el, key, value) {
                    Some(replaced) => write_attribute(out, name, key, &replaced),
                    None => write_attribute(out, name, key, value),
                }
            }
            if node.has_children() {
                out.push('>');
                for child in node.children() {
                    write_node(out, child, name, skip);
                }
                out.push_str("</");
                out.push_str(name);
                out.push('>');
            } else if EMPTY_ELEMENT_TAGS.contains(&name) {
                out.push_str("/>");
            } else {
                out.push_str("></");
                out.push_str(name);
                out.push('>');
            }
        }
        Node::Document | Node::Fragment => {
            for child in node.children() {
                write_node(out, child, "", skip);
            }
        }
        Node::ProcessingInstruction(_) => {}
    }
}

/// The element with its subtree.
pub fn outer_html(el: &ElementRef<'_>) -> String {
    outer_html_without(el, &[])
}

/// The element with every descendant named in `skip` left out. The root
/// itself is never skipped.
pub fn outer_html_without(el: &ElementRef<'_>, skip: &[&str]) -> String {
    let mut out = String::new();
    let parent = el
        .parent()
        .and_then(ElementRef::wrap)
        .map_or("", |p| p.value().name());
    // Skipping applies only below the root.
    let skip: Vec<&str> = skip.to_vec();
    write_node(
        &mut out,
        **el,
        parent,
        &Skip {
            names: &skip,
            root: el.id(),
        },
    );
    out
}

/// Inner markup: child tags serialised, strings and comments written raw
/// (no escaping, no comment delimiters).
pub fn contents_str(el: &ElementRef<'_>) -> String {
    let mut out = String::new();
    for child in el.children() {
        match child.value() {
            Node::Text(text) => out.push_str(text),
            Node::Comment(comment) => out.push_str(comment),
            Node::Doctype(doctype) => out.push_str(&doctype.name),
            _ => write_node(
                &mut out,
                child,
                el.value().name(),
                &Skip {
                    names: &[],
                    root: el.id(),
                },
            ),
        }
    }
    out
}

/// Text below the element: plain strings stripped, empties dropped, then joined with the separator.
/// A string counts when its innermost string-container ancestor matches the tag's own kind.
pub fn get_text_stripped(el: &ElementRef<'_>, separator: &str) -> String {
    let own = container_kind(el.value().name());
    let mut parts: Vec<&str> = Vec::new();
    for node in el.descendants() {
        let Node::Text(text) = node.value() else {
            continue;
        };
        let kind = node
            .ancestors()
            .filter_map(ElementRef::wrap)
            .find_map(|a| container_kind(a.value().name()));
        if kind == own {
            let stripped = py_strip(text);
            if !stripped.is_empty() {
                parts.push(stripped);
            }
        }
    }
    parts.join(separator)
}

/// Attribute as a string. List-valued attributes (`class`, `rel` on links, ...) render as a
/// bracketed list such as `['a', 'b']`; other attributes are the string as written.
pub fn attribute_python_str(el: &ElementRef<'_>, attr: &str, raw: &str) -> String {
    if !is_list_attribute(el.value().name(), attr) {
        return raw.to_owned();
    }
    let items: Vec<String> = raw
        .split(is_py_space)
        .filter(|p| !p.is_empty())
        .map(crate::py_str::py_repr)
        .collect();
    format!("[{}]", items.join(", "))
}
