//! Document decoding and the tolerant tree (the lxml recovering-parser answers).

use super::parse::is_valid_xml_char;

// ── Document decoding ───────────────────────────────────────────────────────

/// Decode raw bytes to text the way libxml2 does for the corpus inputs: BOM
/// first, then the XML declaration's encoding, UTF-8 as the default.
pub(super) fn decode_bytes(raw: &[u8]) -> String {
    if raw.starts_with(&[0xFE, 0xFF]) {
        return decode_utf16(raw, true);
    }
    if raw.starts_with(&[0xFF, 0xFE]) {
        return decode_utf16(raw, false);
    }
    if raw.starts_with(&[0xEF, 0xBB, 0xBF]) {
        return String::from_utf8_lossy(&raw[3..]).into_owned();
    }
    if let Some(declared) = declared_encoding(raw) {
        // A UTF-16 declaration without a UTF-16 byte order mark: libxml2
        // switches decoder and cannot start from the result (the corpus pins
        // `empty document`), while NUL-padded UTF-16 bytes still parse.
        if declared.starts_with("utf-16") {
            if raw.len() >= 2 && raw[0] == b'<' && raw[1] == 0 {
                return decode_utf16(raw, false);
            }
            if raw.len() >= 2 && raw[0] == 0 && raw[1] == b'<' {
                return decode_utf16(raw, true);
            }
            // Whitespace-only: no root element, so the answer is the
            // "empty document" wording rather than the text fallback.
            return " ".to_owned();
        }
        if !declared.is_empty() && declared != "utf-8" {
            return decode_label(raw, &declared);
        }
    }
    String::from_utf8_lossy(raw).into_owned()
}

/// The `encoding="..."` pseudo-attribute of a leading XML declaration.
pub(super) fn declared_encoding(raw: &[u8]) -> Option<String> {
    let head = String::from_utf8_lossy(raw.get(..256).unwrap_or(raw)).into_owned();
    let head = head.trim_start_matches('\u{feff}').trim_start();
    let rest = head.strip_prefix("<?xml")?;
    let end = rest.find("?>")?;
    let decl = &rest[..end];
    let at = decl.find("encoding")?;
    let after = decl[at + "encoding".len()..]
        .trim_start()
        .strip_prefix('=')?;
    let after = after.trim_start();
    let quote = after.chars().next()?;
    let value: String = after[quote.len_utf8()..]
        .chars()
        .take_while(|c| *c != quote)
        .collect();
    Some(value.to_lowercase())
}

pub(super) fn decode_utf16(raw: &[u8], big_endian: bool) -> String {
    let raw = if raw.starts_with(&[0xFE, 0xFF]) || raw.starts_with(&[0xFF, 0xFE]) {
        &raw[2..]
    } else {
        raw
    };
    raw.as_chunks::<2>()
        .0
        .iter()
        .map(|pair| {
            let unit = if big_endian {
                u16::from_be_bytes(*pair)
            } else {
                u16::from_le_bytes(*pair)
            };
            char::from_u32(unit as u32).unwrap_or('\u{fffd}')
        })
        .collect()
}

pub(super) fn decode_label(raw: &[u8], label: &str) -> String {
    if matches!(label, "latin-1" | "iso-8859-1" | "iso8859-1") {
        return raw.iter().map(|&b| b as char).collect();
    }
    match encoding_rs::Encoding::for_label(label.as_bytes()) {
        Some(encoding) => encoding.decode(raw).0.into_owned(),
        None => String::from_utf8_lossy(raw).into_owned(),
    }
}

// ── Tolerant tree ───────────────────────────────────────────────────────────

/// One recovered element: local tag name, attributes, the text before the
/// first child node (`None` when the element is empty), and the children.
#[derive(Debug, Clone)]
pub struct Element {
    /// Tag name with any namespace or prefix stripped.
    pub name: String,
    /// Attribute local names and values, in source order.
    pub attrs: Vec<(String, String)>,
    /// Text before the first child node; `Some("")` for whitespace-only text.
    pub text: Option<String>,
    /// Child elements.
    pub children: Vec<Element>,
}

impl Element {
    /// `_child_text`: the trimmed text of the first child `name`, or `None`.
    /// Whitespace-only text stays an empty string, an empty element is `None`.
    pub(super) fn child_text(&self, name: &str) -> Option<String> {
        for child in &self.children {
            if child.name == name {
                return child.text.as_ref().map(|text| text.trim().to_owned());
            }
        }
        None
    }

    /// The first attribute value by local attribute name.
    pub(super) fn attr(&self, name: &str) -> Option<&str> {
        self.attrs
            .iter()
            .find(|(key, _)| key == name)
            .map(|(_, value)| value.as_str())
    }
}

/// One open element and the text accumulated before its first child.
pub(super) struct Open {
    element: Element,
    text: String,
    /// `false` once a comment/PI or a child element ended the text run.
    text_open: bool,
    /// Whether any character event fed the text at all: an empty element has
    /// `text: None` where a whitespace-only element has `text: Some("")`.
    text_any: bool,
}

pub(super) fn finish_top(stack: &mut Vec<Open>, root: &mut Option<Element>) {
    let Some(done) = stack.pop() else { return };
    let text = if done.text_any { Some(done.text) } else { None };
    let element = Element {
        name: done.element.name,
        attrs: done.element.attrs,
        text,
        children: done.element.children,
    };
    match stack.last_mut() {
        Some(parent) => parent.element.children.push(element),
        None => {
            if root.is_none() {
                *root = Some(element);
            }
            // A second root element is dropped: libxml2 keeps the first.
        }
    }
}

/// What the tolerant parse produced: a root element, or the libxml2-style
/// reason there is none.
pub(super) enum Parsed {
    Root(Element),
    /// `empty document`: only declarations, comments and whitespace.
    Empty,
    /// Non-whitespace input before the first `<`.
    NoDocument,
}

/// Parse a decoded document with the recovery rules the corpus pins.
pub(super) fn parse_tree(text: &str) -> Parsed {
    if text.is_empty() {
        return Parsed::NoDocument;
    }
    let chars: Vec<char> = text.chars().collect();
    let mut pos = 0usize;
    let mut stack: Vec<Open> = Vec::new();
    let mut root: Option<Element> = None;

    while pos < chars.len() {
        let ch = chars[pos];
        if ch == '<' {
            let after = pos + 1;
            let preview: String = chars[after..].iter().take(9).collect();
            if preview.starts_with("![CDATA[") {
                let mut scan = after + 8;
                let end = {
                    let mut found = chars.len();
                    while scan + 2 < chars.len() {
                        if chars[scan] == ']' && chars[scan + 1] == ']' && chars[scan + 2] == '>' {
                            found = scan;
                            break;
                        }
                        scan += 1;
                    }
                    found
                };
                if let Some(top) = stack.last_mut() {
                    if top.text_open {
                        for ch in chars[after + 8..end].iter() {
                            if is_valid_xml_char(*ch) {
                                top.text.push(*ch);
                                top.text_any = true;
                            }
                        }
                    }
                }
                pos = if end + 2 < chars.len() {
                    end + 3
                } else {
                    chars.len()
                };
                continue;
            }
            if preview.starts_with("!--") {
                if let Some(top) = stack.last_mut() {
                    top.text_open = false;
                }
                let mut scan = after + 3;
                while scan + 2 < chars.len() {
                    if chars[scan] == '-' && chars[scan + 1] == '-' && chars[scan + 2] == '>' {
                        break;
                    }
                    scan += 1;
                }
                pos = if scan + 2 < chars.len() {
                    scan + 3
                } else {
                    chars.len()
                };
                continue;
            }
            if preview.starts_with('?') {
                if let Some(top) = stack.last_mut() {
                    top.text_open = false;
                }
                let mut scan = after + 1;
                while scan + 1 < chars.len() {
                    if chars[scan] == '?' && chars[scan + 1] == '>' {
                        break;
                    }
                    scan += 1;
                }
                pos = if scan + 1 < chars.len() {
                    scan + 2
                } else {
                    chars.len()
                };
                continue;
            }
            if preview.starts_with("!DOCTYPE") || preview.starts_with("!doctype") {
                if let Some(top) = stack.last_mut() {
                    top.text_open = false;
                }
                let mut scan = after + 1;
                let mut brackets = 0i32;
                while scan < chars.len() {
                    match chars[scan] {
                        '[' => brackets += 1,
                        ']' => brackets -= 1,
                        '>' if brackets <= 0 => break,
                        _ => {}
                    }
                    scan += 1;
                }
                pos = if scan < chars.len() {
                    scan + 1
                } else {
                    chars.len()
                };
                continue;
            }
            if preview.starts_with('/') {
                let mut scan = after + 1;
                while scan < chars.len() && chars[scan] != '>' {
                    scan += 1;
                }
                finish_top(&mut stack, &mut root);
                pos = if scan < chars.len() {
                    scan + 1
                } else {
                    chars.len()
                };
                continue;
            }
            if after >= chars.len() || !is_name_start(chars[after]) {
                // A stray '<' with no element name: libxml2 drops it.
                pos += 1;
                continue;
            }
            // Start tag. An unterminated tag at EOF is recovered as complete
            // once its name is readable.
            let name: String = chars[after..]
                .iter()
                .take_while(|c| is_name_char(**c))
                .collect();
            let mut scan = after + name.chars().count();
            let mut attrs: Vec<(String, String)> = Vec::new();
            let mut self_closing = false;
            let mut invalid = false;
            while scan < chars.len() {
                if chars[scan].is_whitespace() {
                    scan += 1;
                    continue;
                }
                match chars[scan] {
                    '>' => {
                        scan += 1;
                        break;
                    }
                    '/' if chars.get(scan + 1) == Some(&'>') => {
                        self_closing = true;
                        scan += 2;
                        break;
                    }
                    '<' => break, // recovery: a new '<' ends a malformed tag
                    _ => {}
                }
                let attr_name: String = chars[scan..]
                    .iter()
                    .take_while(|c| !c.is_whitespace() && !['=', '>', '/'].contains(c))
                    .collect();
                scan += attr_name.chars().count();
                while scan < chars.len() && chars[scan].is_whitespace() {
                    scan += 1;
                }
                if scan < chars.len() && chars[scan] == '=' {
                    scan += 1;
                    while scan < chars.len() && chars[scan].is_whitespace() {
                        scan += 1;
                    }
                    if scan < chars.len() && (chars[scan] == '"' || chars[scan] == '\'') {
                        let quote = chars[scan];
                        scan += 1;
                        let value: String =
                            chars[scan..].iter().take_while(|c| **c != quote).collect();
                        scan += value.chars().count();
                        if scan < chars.len() {
                            scan += 1; // closing quote
                        }
                        attrs.push((local_of(&attr_name), unescape_text(&value)));
                    } else if scan >= chars.len() {
                        break; // truncated at EOF: recovered, not discarded
                    } else {
                        // An unquoted attribute value: libxml2 discards the tag.
                        invalid = true;
                        while scan < chars.len()
                            && !chars[scan].is_whitespace()
                            && chars[scan] != '>'
                        {
                            scan += 1;
                        }
                    }
                } else {
                    attrs.push((local_of(&attr_name), String::new()));
                }
            }
            if name.is_empty() {
                pos = scan.max(pos + 1);
                continue;
            }
            if invalid {
                // The tag itself is dropped; its content parses on.
                if let Some(top) = stack.last_mut() {
                    top.text_open = false;
                }
                pos = scan;
                continue;
            }
            let element = Element {
                name: local_of(&name),
                attrs,
                text: None,
                children: Vec::new(),
            };
            if self_closing {
                let done = Element {
                    text: None,
                    ..element
                };
                match stack.last_mut() {
                    Some(parent) => {
                        parent.element.children.push(done);
                        parent.text_open = false;
                    }
                    None => {
                        if root.is_none() {
                            root = Some(done);
                        }
                    }
                }
            } else {
                if let Some(parent) = stack.last_mut() {
                    parent.text_open = false;
                }
                stack.push(Open {
                    element,
                    text: String::new(),
                    text_open: true,
                    text_any: false,
                });
            }
            pos = scan;
            continue;
        }
        // Character data.
        if stack.is_empty() {
            if root.is_some() {
                pos += 1; // trailing text between/after roots is ignored
                continue;
            }
            if !ch.is_whitespace() {
                return Parsed::NoDocument;
            }
            pos += 1;
            continue;
        }
        if ch == '&' {
            let (replacement, width) = read_entity(&chars[pos..]);
            if let Some(text) = replacement {
                let top = stack.last_mut().expect("stack non-empty");
                if top.text_open {
                    top.text.push_str(&text);
                }
            }
            pos += width.max(1);
            continue;
        }
        let top = stack.last_mut().expect("stack non-empty");
        if top.text_open && is_valid_xml_char(ch) {
            top.text.push(ch);
            top.text_any = true;
        }
        pos += 1;
    }
    while !stack.is_empty() {
        // An element the document ended inside keeps its structure but loses
        // the unterminated trailing text (the corpus pins cut-107/108).
        stack.last_mut().unwrap().text_open = false;
        finish_top(&mut stack, &mut root);
    }
    match root {
        Some(root) => Parsed::Root(root),
        None => Parsed::Empty,
    }
}

pub(super) fn is_name_start(ch: char) -> bool {
    ch.is_alphabetic() || ch == '_' || ch == ':'
}

pub(super) fn is_name_char(ch: char) -> bool {
    is_name_start(ch) || ch.is_ascii_digit() || ch == '-' || ch == '.'
}

/// The attribute/text local name: everything after the last `:`.
pub(super) fn local_of(raw: &str) -> String {
    raw.rsplit(':').next().unwrap_or(raw).to_owned()
}

/// `&`-reference handling: `(replacement, chars consumed)`. Built-in and
/// numeric references resolve; anything else is dropped, consuming the name
/// and a trailing `;` when present (the recovery the corpus pins).
pub(super) fn read_entity(chars: &[char]) -> (Option<String>, usize) {
    let rest: String = chars[1..].iter().take(12).collect();
    for (name, replacement) in [
        ("amp;", "&"),
        ("lt;", "<"),
        ("gt;", ">"),
        ("quot;", "\""),
        ("apos;", "'"),
    ] {
        if rest.starts_with(name) {
            return (Some(replacement.to_owned()), 1 + name.len());
        }
    }
    if let Some(hex) = rest.strip_prefix("#x").or_else(|| rest.strip_prefix("#X")) {
        let digits: String = hex.chars().take_while(|c| c.is_ascii_hexdigit()).collect();
        if !digits.is_empty() {
            let code = u32::from_str_radix(&digits, 16).unwrap_or(0xFFFD);
            let width = 2 + digits.len() + usize::from(hex[digits.len()..].starts_with(';'));
            return (char::from_u32(code).map(|c| c.to_string()), width);
        }
    }
    if let Some(dec) = rest.strip_prefix('#') {
        let digits: String = dec.chars().take_while(|c| c.is_ascii_digit()).collect();
        if !digits.is_empty() {
            let code = digits.parse::<u32>().unwrap_or(0xFFFD);
            let width = 2 + digits.len() + usize::from(dec[digits.len()..].starts_with(';'));
            return (char::from_u32(code).map(|c| c.to_string()), width);
        }
    }
    // Unknown reference: libxml2's recovery consumes the reference and keeps
    // scanning name characters after it (`a&nope;b` -> `a`), the corpus pins.
    let mut width = 1;
    while chars.get(width).is_some_and(|c| is_name_char(*c)) {
        width += 1;
    }
    if chars.get(width) == Some(&';') && width > 1 {
        width += 1;
        while chars.get(width).is_some_and(|c| is_name_char(*c)) {
            width += 1;
        }
    }
    (None, width)
}

pub(super) fn unescape_text(value: &str) -> String {
    let chars: Vec<char> = value.chars().collect();
    let mut out = String::new();
    let mut pos = 0usize;
    while pos < chars.len() {
        if chars[pos] == '&' {
            let (replacement, width) = read_entity(&chars[pos..]);
            if let Some(text) = replacement {
                out.push_str(&text);
            }
            pos += width.max(1);
        } else {
            out.push(chars[pos]);
            pos += 1;
        }
    }
    out
}
