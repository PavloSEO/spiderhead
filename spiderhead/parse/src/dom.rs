//! DOM seam over `scraper` nodes. Every module takes `&Soup`.
//!
//! The tree is not built by html5ever's tree builder: [`parse_document`] tokenises with
//! html5ever and builds the tree with libxml2's rules, producing a `scraper::Html` so that
//! selectors keep working (see `dom/build.rs`).

mod build;
mod tables;

use html5ever::tendril::StrTendril;
use html5ever::{ns, LocalName, QualName};
use scraper::node::{Comment, Doctype, Element, Text};
pub use scraper::{ElementRef, Selector};
use scraper::{Html, Node};

/// A parsed document.
pub type Soup = Html;

/// Parse a decoded HTML string into a document.
///
/// Never fails: libxml2 recovers from any input.
pub fn parse_document(html: &str) -> Soup {
    assemble(build::build(html, true), false)
}

/// Attribute recording the source order of an element's attributes (the tree sorts them);
/// read back by [`ordered_attrs`].
pub(crate) const ORDER_ATTR: &str = "\u{1}order";

/// HTML boolean attributes: libxml2 gives a value-less one its own name as value.
const BOOLEAN_ATTRS: [&str; 13] = [
    "checked", "compact", "declare", "defer", "disabled", "ismap", "multiple", "nohref",
    "noresize", "noshade", "nowrap", "readonly", "selected",
];

/// Attributes of a libxml2-shaped element in source order, without the bookkeeping attribute.
pub(crate) fn ordered_attrs(el: &Element) -> Vec<(&str, &str)> {
    let mut attrs: Vec<(&str, &str)> = el.attrs().filter(|(k, _)| *k != ORDER_ATTR).collect();
    if let Some(order) = el.attr(ORDER_ATTR) {
        let position = |name: &str| {
            order
                .split('\u{1}')
                .position(|n| n == name)
                .unwrap_or(usize::MAX)
        };
        attrs.sort_by_key(|(k, _)| position(k));
    }
    attrs
}

/// The libxml2-shaped tree without the collapsing of whitespace-only strings. Used by the
/// XPath scopes only.
pub fn parse_document_lxml(html: &str) -> Soup {
    assemble(build::build(html, false), true)
}

fn assemble(nodes: Vec<build::Node>, lxml: bool) -> Soup {
    let mut doc = Html::new_document();
    let mut ids = Vec::with_capacity(nodes.len());
    ids.push(doc.tree.root().id());
    // Parents are always created before children (indexes only grow), so one pass suffices.
    let mut parent_of = vec![0usize; nodes.len()];
    for (i, node) in nodes.iter().enumerate() {
        for &child in &node.children {
            parent_of[child] = i;
        }
    }
    for (i, node) in nodes.iter().enumerate().skip(1) {
        let value = match &node.kind {
            build::Kind::Root => continue,
            build::Kind::Element { name, attrs } => {
                let mut attributes: Vec<html5ever::Attribute> = attrs
                    .iter()
                    .map(|(k, v)| {
                        let boolean = lxml && v.is_empty() && BOOLEAN_ATTRS.contains(&k.as_str());
                        html5ever::Attribute {
                            name: QualName::new(None, ns!(), LocalName::from(k.as_str())),
                            value: StrTendril::from_slice(if boolean { k } else { v }),
                        }
                    })
                    .collect();
                if lxml && attrs.len() > 1 {
                    let order: Vec<&str> = attrs.iter().map(|(k, _)| k.as_str()).collect();
                    attributes.push(html5ever::Attribute {
                        name: QualName::new(None, ns!(), LocalName::from(ORDER_ATTR)),
                        value: StrTendril::from_slice(&order.join("\u{1}")),
                    });
                }
                Node::Element(Element::new(
                    QualName::new(None, ns!(html), LocalName::from(name.as_str())),
                    attributes,
                ))
            }
            build::Kind::Text(t) => Node::Text(Text {
                text: StrTendril::from_slice(t),
            }),
            build::Kind::Comment(t) => Node::Comment(Comment {
                comment: StrTendril::from_slice(t),
            }),
            // The whole rendering lives in `name`; the ids stay empty.
            build::Kind::Doctype { text } => Node::Doctype(Doctype {
                name: StrTendril::from_slice(text),
                public_id: StrTendril::new(),
                system_id: StrTendril::new(),
            }),
        };
        let parent = ids[parent_of[i]];
        if let Some(mut p) = doc.tree.get_mut(parent) {
            ids.push(p.append(value).id());
        }
    }
    doc
}

/// Lower-cased tag name of an element.
pub fn tag_name(el: &ElementRef<'_>) -> String {
    el.value().name().to_ascii_lowercase()
}

/// True when `el` itself or any ancestor is named in `names`.
pub fn has_ancestor_named(el: &ElementRef<'_>, names: &[&str]) -> bool {
    if names.contains(&el.value().name()) {
        return true;
    }
    el.ancestors()
        .filter_map(ElementRef::wrap)
        .any(|a| names.contains(&a.value().name()))
}

/// Every element of the document in document order.
pub fn all_elements(soup: &Soup) -> impl Iterator<Item = ElementRef<'_>> {
    soup.tree.nodes().filter_map(ElementRef::wrap)
}

/// Elements named `name`, document order (`find_all(name)`).
pub fn find_all<'a>(soup: &'a Soup, name: &'a str) -> impl Iterator<Item = ElementRef<'a>> + 'a {
    all_elements(soup).filter(move |e| e.value().name() == name)
}

/// First element named `name` (`find(name)`).
pub fn find<'a>(soup: &'a Soup, name: &str) -> Option<ElementRef<'a>> {
    all_elements(soup).find(|e| e.value().name() == name)
}

/// Attribute value as written (`tag.get(name)` for single-valued attributes).
pub fn attr<'a>(el: &ElementRef<'a>, name: &str) -> Option<&'a str> {
    el.value().attr(name)
}

/// `name in tag.attrs`.
pub fn has_attr(el: &ElementRef<'_>, name: &str) -> bool {
    el.value().attr(name).is_some()
}

/// String container tags: text below one is not a plain string.
pub(crate) fn container_kind(name: &str) -> Option<&'static str> {
    match name {
        "script" => Some("script"),
        "style" => Some("style"),
        "template" => Some("template"),
        "rt" => Some("rt"),
        "rp" => Some("rp"),
        _ => None,
    }
}

/// Concatenated plain strings below `el`. A string is plain unless its innermost container
/// ancestor (`script`, `style`, `template`, `rt`, `rp`) differs from `el`'s own kind: only
/// strings of the container kind matching the element are read.
pub fn get_text(el: &ElementRef<'_>, separator: &str) -> String {
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
            parts.push(&text.text);
        }
    }
    parts.join(separator)
}

/// Attribute on a copied content root naming the string container it was cut out of.
pub(crate) const CONTAINER_MARKER: &str = "data-seohead-container";

/// Plain strings below `root` after the `excluded` descendants are removed: text inside an
/// excluded element is dropped, and only strings whose innermost container ancestor (see
/// [`get_text`]) has the root's own kind count. `root` itself is never excluded and may be
/// a document or fragment node.
pub(crate) fn visible_strings<'a>(
    root: ego_tree::NodeRef<'a, Node>,
    excluded: &[&str],
) -> Vec<&'a str> {
    let own = ElementRef::wrap(root).and_then(|el| container_kind(el.value().name()));
    let mut parts = Vec::new();
    for node in root.descendants() {
        let Node::Text(text) = node.value() else {
            continue;
        };
        let mut kind = None;
        let mut dropped = false;
        let mut below_root = true;
        for ancestor in node.ancestors() {
            if ancestor.id() == root.id() {
                below_root = false;
            }
            let Some(el) = ElementRef::wrap(ancestor) else {
                continue;
            };
            if below_root && excluded.contains(&el.value().name()) {
                dropped = true;
                break;
            }
            if kind.is_none() {
                kind = container_kind(el.value().name());
            }
        }
        // A copied root remembers the container it was cut out of (see `content_area`).
        if kind.is_none() {
            kind = ElementRef::wrap(root)
                .and_then(|r| r.value().attr(CONTAINER_MARKER))
                .and_then(container_kind);
        }
        if !dropped && kind == own {
            parts.push(&**text);
        }
    }
    parts
}
