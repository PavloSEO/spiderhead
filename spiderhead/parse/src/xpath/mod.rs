//! XPath 1.0 with libxml2-compatible behaviour: value conversions, number formatting and error classes.
//! Used by the XPath scopes of `custom_extract` and `custom_search`.
//! `custom_search`; nothing here is public API of the crate.

mod eval;
mod funcs;
mod number;
mod parse;
mod serialize;

use ego_tree::NodeId;
use scraper::Html;

pub(crate) use serialize::{element_to_xml, itertext, node_to_xml_with_tail};

use scraper::Node;

/// A node an expression can yield.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(crate) enum XNode {
    /// Document, element, text or comment node.
    Tree(NodeId),
    /// Attribute `index` of an element.
    Attr(NodeId, usize),
    /// The implicit `xml` namespace node of an element.
    Ns(NodeId),
}

impl XNode {
    fn key(&self) -> (NodeId, usize) {
        match self {
            XNode::Tree(id) => (*id, 0),
            XNode::Ns(id) => (*id, 1),
            XNode::Attr(id, i) => (*id, 2 + i),
        }
    }
}

impl PartialOrd for XNode {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for XNode {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        self.key().cmp(&other.key())
    }
}

/// An XPath value.
#[derive(Debug, Clone, PartialEq)]
pub(crate) enum XValue {
    /// Document-ordered, duplicate-free.
    Nodes(Vec<XNode>),
    Str(String),
    Num(f64),
    Bool(bool),
}

/// XPath error: a syntax error at compile time or an evaluation error, with libxml2's message.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum XError {
    Syntax(String),
    Eval(String),
}

impl XError {
    /// The libxml2 message.
    pub(crate) fn message(&self) -> &str {
        match self {
            XError::Syntax(m) | XError::Eval(m) => m,
        }
    }
}

/// A compiled expression.
pub(crate) struct XPath {
    expr: parse::Expr,
}

impl XPath {
    /// `etree.XPath(source)`: syntax errors only.
    pub(crate) fn compile(source: &str) -> Result<XPath, XError> {
        parse::parse(source)
            .map(|expr| XPath { expr })
            .map_err(|e| XError::Syntax(e.0.to_owned()))
    }

    /// Evaluated against the document root element; `None` when the document has no element.
    pub(crate) fn evaluate(&self, doc: &Html) -> Result<Option<XValue>, XError> {
        eval::run(&self.expr, doc)
    }
}

/// Evaluate `source` against the tree; compile errors surface as `XPathEvalError` with the same message.
pub(crate) fn run(source: &str, doc: &Html) -> Result<Option<XValue>, XError> {
    match XPath::compile(source) {
        Ok(compiled) => compiled.evaluate(doc),
        Err(XError::Syntax(m)) => Err(XError::Eval(m)),
        Err(other) => Err(other),
    }
}

/// Latin-1 decoding of a byte slice (what libxml2 assumes without a declared encoding).
fn latin1(bytes: &[u8]) -> String {
    bytes.iter().map(|b| char::from(*b)).collect()
}

/// Byte offset just after the first `<meta>` tag that declares an encoding, and the label.
fn declared_charset(bytes: &[u8]) -> Option<(usize, String)> {
    let lower: Vec<u8> = bytes.to_ascii_lowercase();
    let find = |from: usize, needle: &[u8]| -> Option<usize> {
        lower
            .get(from..)?
            .windows(needle.len())
            .position(|w| w == needle)
            .map(|i| i + from)
    };
    let mut i = 0;
    while let Some(lt) = lower
        .get(i..)
        .and_then(|s| s.iter().position(|b| *b == b'<'))
        .map(|p| p + i)
    {
        if lower[lt..].starts_with(b"<!--") {
            i = find(lt + 4, b"-->").map_or(lower.len(), |e| e + 3);
            continue;
        }
        let openers: [&[u8]; 2] = [b"<script", b"<style"];
        let raw_text = openers.iter().find(|t| lower[lt..].starts_with(t));
        if let Some(open) = raw_text {
            let close: Vec<u8> = [b"</".as_slice(), &open[1..]].concat();
            i = find(lt + 1, &close).map_or(lower.len(), |e| e + close.len());
            continue;
        }
        if lower[lt..].starts_with(b"<meta")
            && lower
                .get(lt + 5)
                .is_some_and(|b| b.is_ascii_whitespace() || *b == b'/')
        {
            let end = find(lt, b">").map_or(lower.len(), |e| e + 1);
            let tag = String::from_utf8_lossy(&lower[lt..end]).into_owned();
            // libxml2 reads real attributes only: a substring search would match inside other names
            // or values (e.g. `data-charset="x"`) and yield a bogus label.
            let attrs = meta_attrs(&tag);
            let attr = |name: &str| -> Option<&str> {
                attrs
                    .iter()
                    .find(|(n, _)| n == name)
                    .map(|(_, v)| v.as_str())
            };
            if let Some(label) = attr("charset").filter(|l| !l.is_empty()) {
                return Some((end, label.to_owned()));
            }
            let is_content_type = attr("http-equiv").is_some_and(|v| v.contains("content-type"));
            if is_content_type {
                if let Some(content) = attr("content") {
                    if let Some(at) = content.find("charset=") {
                        let label: String = content[at + 8..]
                            .chars()
                            .take_while(|c| !matches!(c, ';' | ' ' | '"' | '\''))
                            .collect();
                        if !label.is_empty() {
                            return Some((end, label));
                        }
                    }
                }
            }
            i = end;
            continue;
        }
        i = lt + 1;
    }
    None
}

/// Attribute name/value pairs of a `<meta ...>` tag body, the way libxml2 reads them:
/// quoted or bare values, names bounded by whitespace, `/`, `>` or `=`.
fn meta_attrs(tag: &str) -> Vec<(String, String)> {
    let bytes = tag.as_bytes();
    let blank = |b: u8| matches!(b, b' ' | b'\t' | b'\n' | b'\r' | 0x0c);
    let mut out = Vec::new();
    let mut i = 5usize.min(tag.len()); // skip "<meta"
    while i < tag.len() {
        while i < tag.len() && (blank(bytes[i]) || bytes[i] == b'/') {
            i += 1;
        }
        if i >= tag.len() || bytes[i] == b'>' {
            break;
        }
        let name_start = i;
        while i < tag.len() && !(blank(bytes[i]) || matches!(bytes[i], b'=' | b'/' | b'>')) {
            i += 1;
        }
        let name = &tag[name_start..i];
        while i < tag.len() && blank(bytes[i]) {
            i += 1;
        }
        let mut value = "";
        if bytes.get(i) == Some(&b'=') {
            i += 1;
            while i < tag.len() && blank(bytes[i]) {
                i += 1;
            }
            if i < tag.len() && matches!(bytes[i], b'"' | b'\'') {
                let quote = bytes[i];
                let from = i + 1;
                let end = bytes[from..]
                    .iter()
                    .position(|b| *b == quote)
                    .map_or(tag.len(), |p| from + p);
                value = &tag[from..end];
                i = end.saturating_add(1).min(tag.len());
            } else {
                let from = i;
                while i < tag.len() && !(blank(bytes[i]) || matches!(bytes[i], b'/' | b'>')) {
                    i += 1;
                }
                value = &tag[from..i];
            }
        }
        out.push((name.to_owned(), value.to_owned()));
    }
    out
}

/// Decode bytes as Latin-1 until a `<meta>` declares an encoding, then decode with that encoding
/// as libxml2 does when no encoding argument is given.
pub(crate) fn parse_lxml(html: &str) -> Html {
    let bytes = html.as_bytes();
    // libxml2 only honours the declaration while everything read so far is ASCII.
    let declared = declared_charset(bytes).filter(|(end, _)| bytes[..*end].is_ascii());
    let decoded = match declared {
        Some((end, label)) => {
            let plain = matches!(
                label.as_str(),
                "iso-8859-1" | "iso8859-1" | "iso_8859-1" | "latin1" | "l1" | "us-ascii" | "ascii"
            );
            match encoding_rs::Encoding::for_label(label.as_bytes()) {
                Some(enc) if !plain && !enc.name().starts_with("UTF-16") => {
                    let (rest, _) = enc.decode_without_bom_handling(&bytes[end..]);
                    format!("{}{rest}", latin1(&bytes[..end]))
                }
                _ => latin1(bytes),
            }
        }
        None => latin1(bytes),
    };
    crate::dom::parse_document_lxml(&decoded)
}

/// Text for one node-set member: text and attribute results give their string, elements give
/// all their text, comments give an empty string, namespace nodes give their tuple text; the document node
/// is skipped (`None`).
pub(crate) fn node_text(doc: &Html, node: XNode) -> Option<String> {
    match node {
        XNode::Attr(id, i) => doc.tree.get(id).and_then(|n| match n.value() {
            Node::Element(el) => crate::dom::ordered_attrs(el)
                .get(i)
                .map(|(_, v)| (*v).to_owned()),
            _ => None,
        }),
        XNode::Ns(_) => Some("('xml', 'http://www.w3.org/XML/1998/namespace')".to_owned()),
        XNode::Tree(id) => match doc.tree.get(id).map(|n| n.value()) {
            Some(Node::Text(t)) => Some(t.to_string()),
            Some(Node::Element(_)) => Some(itertext(doc, id)),
            Some(Node::Comment(_)) => Some(String::new()),
            _ => None,
        },
    }
}

/// Whether `node` is a comment node.
pub(crate) fn is_comment(doc: &Html, node: XNode) -> bool {
    matches!(node, XNode::Tree(id) if matches!(doc.tree.get(id).map(|n| n.value()), Some(Node::Comment(_))))
}

/// Whether the document has a root element; without one the parse returns no tree.
pub(crate) fn has_root(doc: &Html) -> bool {
    doc.tree
        .root()
        .children()
        .any(|c| matches!(c.value(), Node::Element(_)))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Only the attribute named `charset` (or `content` under a `content-type` http-equiv)
    /// declares the encoding; a substring inside another attribute name or value never does.
    #[test]
    fn declared_charset_reads_attributes_not_substrings() {
        let tagged = |meta: &str| declared_charset(meta.as_bytes()).map(|(_, l)| l);
        assert_eq!(tagged("<meta charset=\"utf-8\">"), Some("utf-8".to_owned()));
        assert_eq!(tagged("<meta charset=utf-8>"), Some("utf-8".to_owned()));
        // `charset` inside a longer attribute name or inside a value must not match.
        assert_eq!(tagged("<meta data-charset=\"koi8-r\">"), None);
        assert_eq!(
            tagged("<meta data-charset=\"koi8-r\" charset=\"utf-8\">"),
            Some("utf-8".to_owned())
        );
        assert_eq!(
            tagged("<meta http-equiv=\"content-type\" content=\"text/html; charset=koi8-r\">"),
            Some("koi8-r".to_owned())
        );
        // `name="content-type"` without http-equiv does not trigger content parsing.
        assert_eq!(
            tagged("<meta name=\"content-type\" content=\"text/html; charset=koi8-r\">"),
            None
        );
    }
}
