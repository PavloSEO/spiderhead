//! lxml serialisation (`etree.tostring(..., encoding="unicode")`) and `itertext()` over the
//! lxml-shaped tree.

use ego_tree::{NodeId, NodeRef};
use scraper::{Html, Node};

use crate::dom::ordered_attrs;

fn escape_text(out: &mut String, text: &str) {
    for c in text.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '\r' => out.push_str("&#13;"),
            _ => out.push(c),
        }
    }
}

fn escape_attribute(out: &mut String, text: &str) {
    for c in text.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\n' => out.push_str("&#10;"),
            '\r' => out.push_str("&#13;"),
            '\t' => out.push_str("&#9;"),
            _ => out.push(c),
        }
    }
}

/// One unit of [`write`] work: emit a node's markup, or its `</name>` once children are done.
enum Work<'a> {
    /// Emit the node itself.
    Open(NodeRef<'a, Node>),
    /// Emit the closing tag of an element already opened.
    Close(NodeRef<'a, Node>),
}

/// Iterative serialisation: untrusted nesting depth cannot overflow the call stack
/// (lxml's C writer is equally non-recursive).
fn write(out: &mut String, node: NodeRef<'_, Node>) {
    let mut stack = vec![Work::Open(node)];
    while let Some(work) = stack.pop() {
        match work {
            Work::Open(node) => match node.value() {
                Node::Text(t) => escape_text(out, t),
                Node::Comment(c) => {
                    out.push_str("<!--");
                    out.push_str(c);
                    out.push_str("-->");
                }
                Node::Element(el) => {
                    out.push('<');
                    out.push_str(el.name());
                    for (key, value) in ordered_attrs(el) {
                        out.push(' ');
                        out.push_str(key);
                        out.push_str("=\"");
                        escape_attribute(out, value);
                        out.push('"');
                    }
                    if node.has_children() {
                        out.push('>');
                        stack.push(Work::Close(node));
                        stack.extend(node.children().rev().map(Work::Open));
                    } else {
                        out.push_str("/>");
                    }
                }
                _ => {}
            },
            Work::Close(node) => {
                if let Node::Element(el) = node.value() {
                    out.push_str("</");
                    out.push_str(el.name());
                    out.push('>');
                }
            }
        }
    }
}

fn write_tail(out: &mut String, node: NodeRef<'_, Node>) {
    for sibling in node.next_siblings() {
        match sibling.value() {
            Node::Text(t) => escape_text(out, t),
            Node::Doctype(_) => {}
            _ => break,
        }
    }
}

/// `etree.tostring(node, encoding="unicode")` for an element or comment: node plus its tail.
pub(crate) fn node_to_xml_with_tail(doc: &Html, id: NodeId) -> String {
    let mut out = String::new();
    if let Some(node) = doc.tree.get(id) {
        write(&mut out, node);
        write_tail(&mut out, node);
    }
    out
}

/// `"".join(etree.tostring(c, encoding="unicode") for c in node)`: element and comment
/// children with their tails; the element's own leading text is not included.
pub(crate) fn element_to_xml(doc: &Html, id: NodeId) -> String {
    let mut out = String::new();
    if let Some(node) = doc.tree.get(id) {
        for child in node.children() {
            if matches!(child.value(), Node::Element(_) | Node::Comment(_)) {
                write(&mut out, child);
                write_tail(&mut out, child);
            }
        }
    }
    out
}

/// `"".join(node.itertext())`: text and tails of the subtree, comment text excluded.
pub(crate) fn itertext(doc: &Html, id: NodeId) -> String {
    let Some(node) = doc.tree.get(id) else {
        return String::new();
    };
    if !matches!(node.value(), Node::Element(_)) {
        return String::new();
    }
    node.descendants()
        .skip(1)
        .filter_map(|d| match d.value() {
            Node::Text(t) => Some(&**t),
            _ => None,
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Deeply nested markup must serialise without overflowing the call stack.
    #[test]
    fn serialising_deep_nesting_does_not_recurse() {
        let depth = 100_000usize;
        let html = "<div>".repeat(depth);
        let doc = crate::dom::parse_document_lxml(&html);
        let root = doc.tree.root();
        let Some(top) = root
            .descendants()
            .find(|d| matches!(d.value(), Node::Element(el) if el.name() == "div"))
        else {
            panic!("no element");
        };
        let xml = node_to_xml_with_tail(&doc, top.id());
        assert!(xml.starts_with("<div>"));
        assert!(xml.ends_with("</div>"));
        let divs = doc
            .tree
            .nodes()
            .filter(|n| matches!(n.value(), Node::Element(el) if el.name() == "div"))
            .count();
        // The innermost div has no children and is emitted as `<div/>`.
        assert_eq!(xml.matches("</div>").count(), divs - 1);
    }
}
