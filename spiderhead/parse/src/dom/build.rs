//! libxml2-style HTML tree construction over the html5ever tokenizer.
//!
//! libxml2 2.14 tokenises like HTML5 but builds the tree with its own legacy rules: implied
//! `html`/`head`/`body`, a measured start-tag auto-close table, end-tag priorities, no adoption
//! agency and no foster parenting.
//! This module replays those rules on html5ever's token stream, then applies the text rules
//! (adjacent text merges, ASCII-whitespace-only strings collapse).

use std::cell::RefCell;

use html5ever::tendril::StrTendril;
use html5ever::tokenizer::states::RawKind;
use html5ever::tokenizer::{
    BufferQueue, Token, TokenSink, TokenSinkResult, Tokenizer, TokenizerOpts,
};

use super::tables::{AUTO_CLOSE, EMPTY};

/// One node of the intermediate tree (index 0 is the document root).
#[derive(Debug)]
pub(super) enum Kind {
    Root,
    Element {
        name: String,
        attrs: Vec<(String, String)>,
    },
    Text(String),
    Comment(String),
    /// `text` is the doctype name plus any public and system identifiers.
    Doctype {
        text: String,
    },
}

/// Intermediate tree node.
#[derive(Debug)]
pub(super) struct Node {
    pub(super) kind: Kind,
    pub(super) children: Vec<usize>,
}

/// Tag-name groups that put the tokenizer in a raw-data state (libxml2 2.14 treats them so).
fn raw_state(name: &str) -> Option<Option<RawKind>> {
    match name {
        "script" => Some(Some(RawKind::ScriptData)),
        "style" | "xmp" | "iframe" | "noembed" | "noframes" => Some(Some(RawKind::Rawtext)),
        "title" | "textarea" => Some(Some(RawKind::Rcdata)),
        "plaintext" => Some(None),
        _ => None,
    }
}

fn end_priority(name: &str) -> u32 {
    match name {
        "div" => 150,
        "td" | "th" => 160,
        "tr" => 170,
        // thead, tbody and tfoot share one priority.
        "thead" | "tbody" | "tfoot" => 180,
        "table" => 220,
        // `</head>` closes through a `<body>` nested in it, so head ranks above body.
        "body" => 230,
        "head" => 240,
        "html" => 250,
        _ => 100,
    }
}

fn is_ascii_space(c: char) -> bool {
    matches!(c, ' ' | '\n' | '\t' | '\x0c' | '\r')
}

/// An all-ASCII-whitespace string (empty included) collapses to one newline or space unless a
/// whitespace-preserving tag (`pre`, `textarea`) is open.
fn bs4_string(text: String, preserve: bool) -> String {
    if !preserve && text.chars().all(is_ascii_space) {
        return if text.contains('\n') { "\n" } else { " " }.to_owned();
    }
    text
}

struct Builder {
    nodes: Vec<Node>,
    /// Open elements (node indexes).
    stack: Vec<usize>,
    /// libxml2 `ctxt->html`: 3 once a head was opened, 10 once a body was.
    html_state: u8,
    /// Discarded `html`/`head`/`body` start tags whose end tags must be swallowed.
    depth: u32,
    /// libxml2 push-parser state before the root: 0 start, 1 misc (after a real comment or
    /// doctype), 2 content. Blank text is skipped in 0 and 1.
    state: u8,
    /// The source, for telling real comments (`<!--`) from bogus ones while still in 0/1.
    source: String,
    cursor: usize,
    /// Where the next `<!doctype` search starts (the tokenizer lower-cases the name).
    doctype_cursor: usize,
    pending: String,
    /// Text rules (whitespace-only strings collapse); off for a raw libxml2 tree.
    collapse: bool,
}

impl Builder {
    fn new(source: &str, collapse: bool) -> Self {
        Self {
            nodes: vec![Node {
                kind: Kind::Root,
                children: Vec::new(),
            }],
            stack: Vec::new(),
            html_state: 0,
            depth: 0,
            state: 0,
            source: source.trim_start_matches('\u{feff}').to_owned(),
            cursor: 0,
            doctype_cursor: 0,
            pending: String::new(),
            collapse,
        }
    }

    fn top_name(&self) -> Option<&str> {
        self.stack.last().map(|&i| self.name_of(i))
    }

    fn name_of(&self, index: usize) -> &str {
        match &self.nodes[index].kind {
            Kind::Element { name, .. } => name,
            _ => "",
        }
    }

    fn append(&mut self, kind: Kind) -> usize {
        let parent = self.stack.last().copied().unwrap_or(0);
        self.nodes.push(Node {
            kind,
            children: Vec::new(),
        });
        let id = self.nodes.len() - 1;
        self.nodes[parent].children.push(id);
        id
    }

    fn preserving_whitespace(&self) -> bool {
        self.stack
            .iter()
            .any(|&i| matches!(self.name_of(i), "pre" | "textarea"))
    }

    fn flush_text(&mut self) {
        if self.pending.is_empty() {
            return;
        }
        let text = std::mem::take(&mut self.pending);
        if !self.collapse && self.stack.is_empty() {
            // libxml2 ignores text outside the root element.
            return;
        }
        let text = if self.collapse {
            bs4_string(text, self.preserving_whitespace())
        } else {
            text
        };
        self.append(Kind::Text(text));
    }

    fn push(&mut self, name: &str, attrs: Vec<(String, String)>) {
        self.flush_text();
        if self.html_state < 3 && name == "head" {
            self.html_state = 3;
        }
        if self.html_state < 10 && name == "body" {
            self.html_state = 10;
        }
        let id = self.append(Kind::Element {
            name: name.to_owned(),
            attrs,
        });
        self.stack.push(id);
    }

    fn pop(&mut self) {
        self.flush_text();
        self.stack.pop();
    }

    /// libxml2 `htmlAutoClose`.
    fn auto_close(&mut self, new_tag: &str) {
        while let Some(top) = self.top_name() {
            if AUTO_CLOSE
                .iter()
                .any(|&(open, new)| open == top && new == new_tag)
            {
                self.pop();
            } else {
                break;
            }
        }
    }

    /// libxml2 `htmlCheckImplied`.
    fn check_implied(&mut self, new_tag: &str) {
        if new_tag == "html" {
            return;
        }
        if self.stack.is_empty() {
            self.push("html", Vec::new());
        }
        if matches!(new_tag, "body" | "head") {
            return;
        }
        if self.stack.len() <= 1
            && matches!(
                new_tag,
                "script" | "style" | "meta" | "link" | "title" | "base"
            )
        {
            if self.html_state >= 3 {
                return;
            }
            self.push("head", Vec::new());
        } else if !matches!(new_tag, "noframes" | "frame" | "frameset") {
            if self.html_state >= 10 {
                return;
            }
            if self
                .stack
                .iter()
                .any(|&i| matches!(self.name_of(i), "body" | "head"))
            {
                return;
            }
            self.push("body", Vec::new());
        }
    }

    fn start_tag(&mut self, name: &str, attrs: Vec<(String, String)>, self_closing: bool) {
        self.state = 2;
        self.auto_close(name);
        self.check_implied(name);
        let discard = match name {
            "html" => !self.stack.is_empty(),
            "head" => self.stack.len() != 1,
            "body" => self.stack.iter().any(|&i| self.name_of(i) == "body"),
            _ => false,
        };
        if discard {
            self.depth += 1;
            // libxml2 applies the `/>` check to the current element even for a discarded tag.
            if self_closing {
                self.pop();
            }
            return;
        }
        self.push(name, attrs);
        if self_closing || EMPTY.contains(&name) {
            self.pop();
        }
    }

    /// libxml2 `htmlParseEndTag` with `htmlAutoCloseOnClose`.
    fn end_tag(&mut self, name: &str) {
        self.state = 2;
        if self.depth > 0 && matches!(name, "html" | "body" | "head") {
            self.depth -= 1;
            return;
        }
        let Some(found) = self.stack.iter().rposition(|&i| self.name_of(i) == name) else {
            return;
        };
        let priority = end_priority(name);
        let blocked = self.stack[found + 1..]
            .iter()
            .any(|&i| end_priority(self.name_of(i)) > priority);
        if blocked {
            return;
        }
        while self.stack.len() > found + 1 {
            self.pop();
        }
        self.pop();
    }

    fn text(&mut self, data: &str) {
        let is_blank = |c: char| matches!(c, ' ' | '\t' | '\n' | '\r');
        let rest = data.trim_start_matches(is_blank);
        if rest.is_empty() {
            if self.state == 2 {
                self.pending.push_str(data);
            }
            return;
        }
        let lead = &data[..data.len() - rest.len()];
        if self.state == 2 && self.stack.is_empty() {
            // Outside any element libxml2 delivers a leading blank run on its own.
            self.pending.push_str(lead);
            self.flush_text();
        } else if self.state == 2 {
            self.pending.push_str(lead);
        }
        self.state = 2;
        if matches!(self.top_name(), None | Some("html" | "head")) {
            self.auto_close("p");
            self.check_implied("p");
        }
        self.pending.push_str(rest);
    }

    /// The doctype name as written (libxml2 keeps its case; html5ever lower-cases it).
    fn written_doctype_name(&mut self, lowered: &str) -> Option<String> {
        let lower_source = self.source.to_ascii_lowercase();
        let at =
            self.doctype_cursor + lower_source.get(self.doctype_cursor..)?.find("<!doctype")?;
        let tail = &self.source[at + 9..];
        let trimmed = tail.trim_start_matches(|c: char| c.is_ascii_whitespace());
        let name: String = trimmed
            .chars()
            .take_while(|c| !c.is_ascii_whitespace() && *c != '>')
            .collect();
        self.doctype_cursor = at + 9;
        (name.to_ascii_lowercase() == lowered).then_some(name)
    }

    /// Advance past a leading comment/doctype in the source; true when it was a real `<!--`.
    fn leading_construct(&mut self) -> bool {
        let tail = &self.source[self.cursor.min(self.source.len())..];
        let trimmed = tail.trim_start_matches(|c: char| c.is_ascii_whitespace());
        let start = self.source.len() - trimmed.len();
        let real = trimmed.starts_with("<!--") || trimmed.starts_with("<?");
        // The comment text can contain non-ASCII bytes, so the end is found in the source
        // rather than derived from the token's byte length.
        let pat = if trimmed.starts_with("<!--") {
            "-->"
        } else {
            ">"
        };
        self.cursor = trimmed
            .find(pat)
            .map_or(self.source.len(), |i| start + i + pat.len());
        real
    }

    fn finish(mut self) -> Vec<Node> {
        while !self.stack.is_empty() {
            self.pop();
        }
        self.flush_text();
        self.nodes
    }
}

struct Sink {
    builder: RefCell<Builder>,
}

impl TokenSink for Sink {
    type Handle = ();

    fn process_token(&self, token: Token, _line: u64) -> TokenSinkResult<()> {
        let mut b = self.builder.borrow_mut();
        match token {
            Token::CharacterTokens(text) => b.text(&text),
            Token::NullCharacterToken => b.text("\u{FFFD}"),
            Token::CommentToken(text) => {
                if b.state < 2 {
                    let real = b.leading_construct();
                    b.state = if real { 1 } else { 2 };
                }
                b.flush_text();
                let text = if b.collapse {
                    bs4_string(text.to_string(), b.preserving_whitespace())
                } else {
                    text.to_string()
                };
                b.append(Kind::Comment(text));
            }
            Token::DoctypeToken(doctype) => {
                if b.state < 2 {
                    b.leading_construct();
                    b.state = 1;
                }
                b.flush_text();
                let lowered = doctype.name.map(|t| t.to_string()).unwrap_or_default();
                let mut text = b.written_doctype_name(&lowered).unwrap_or(lowered);
                match (doctype.public_id, doctype.system_id) {
                    (Some(public), system) => {
                        text.push_str(&format!(" PUBLIC \"{public}\""));
                        if let Some(system) = system {
                            text.push_str(&format!(" \"{system}\""));
                        }
                    }
                    (None, Some(system)) => text.push_str(&format!(" SYSTEM \"{system}\"")),
                    (None, None) => {}
                }
                b.append(Kind::Doctype { text });
            }
            Token::TagToken(tag) => {
                let name: &str = &tag.name;
                if tag.kind == html5ever::tokenizer::StartTag {
                    let attrs = tag
                        .attrs
                        .iter()
                        .map(|a| (a.name.local.to_string(), a.value.to_string()))
                        .collect();
                    b.start_tag(name, attrs, tag.self_closing);
                    if !tag.self_closing {
                        match raw_state(name) {
                            Some(Some(kind)) => return TokenSinkResult::RawData(kind),
                            Some(None) => return TokenSinkResult::Plaintext,
                            None => {}
                        }
                    }
                } else {
                    b.end_tag(name);
                }
            }
            Token::EOFToken | Token::ParseError(_) => {}
        }
        TokenSinkResult::Continue
    }
}

/// Tokenise `html` and build the libxml2-shaped tree.
pub(super) fn build(html: &str, collapse: bool) -> Vec<Node> {
    let tokenizer = Tokenizer::new(
        Sink {
            builder: RefCell::new(Builder::new(html, collapse)),
        },
        TokenizerOpts::default(),
    );
    let queue = BufferQueue::default();
    queue.push_back(StrTendril::from_slice(html));
    let _ = tokenizer.feed(&queue);
    tokenizer.end();
    tokenizer.sink.builder.into_inner().finish()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn has_comment(nodes: &[Node], want: &str) -> bool {
        nodes
            .iter()
            .any(|n| matches!(&n.kind, Kind::Comment(t) if t == want))
    }

    /// A leading comment with non-ASCII text must not slice the source mid-codepoint.
    #[test]
    fn leading_comment_with_non_ascii_text_builds_tree() {
        let nodes = build("<!-- Привет -->", true);
        assert!(has_comment(&nodes, " Привет "));
        let nodes = build("<!-- 文 -->", true);
        assert!(has_comment(&nodes, " 文 "));
        // Content after the comment is still parsed.
        let nodes = build("<!-- Привет --><p>x</p>", true);
        assert!(has_comment(&nodes, " Привет "));
        assert!(nodes
            .iter()
            .any(|n| matches!(&n.kind, Kind::Element { name, .. } if name == "p")));
        // A second leading comment keeps the misc state.
        let nodes = build("<!-- Привет --><!-- 文 -->", true);
        assert!(has_comment(&nodes, " Привет "));
        assert!(has_comment(&nodes, " 文 "));
    }
}
