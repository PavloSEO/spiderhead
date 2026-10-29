//! Tag tokenizer for the raw scanners: emits start, start-end and end tags, declarations and character data.
//! Whole document, character references converted outside raw-text elements.
//! Tolerant attribute syntax, raw-text elements and bogus comments are matched with the same regular expressions.

use std::sync::LazyLock;

use fancy_regex::Regex;

/// Elements whose content is opaque text for every scanner.
/// `noscript` is added for the subclassed scanners only.
const OPAQUE: [&str; 9] = [
    "script",
    "style",
    "xmp",
    "iframe",
    "noembed",
    "noframes",
    "title",
    "textarea",
    "plaintext",
];

fn re(pattern: &str) -> Regex {
    // The patterns are literals that the tests below exercise.
    Regex::new(pattern).unwrap_or_else(|_| Regex::new("$^").unwrap_or_else(|_| unreachable!()))
}

static TAGFIND: LazyLock<Regex> =
    LazyLock::new(|| re(r"([a-zA-Z][^\t\n\r\x0c />]*)(?:[\t\n\r\x0c ]|/(?!>))*"));
static ATTRFIND: LazyLock<Regex> = LazyLock::new(|| {
    re(concat!(
        r#"((?<=['"\t\n\r\x0c /])[^\t\n\r\x0c />][^\t\n\r\x0c /=>]*)"#,
        r#"([\t\n\r\x0c ]*=[\t\n\r\x0c ]*('[^']*'|"[^"]*"|(?!['"])[^>\t\n\r\x0c ]*))?"#,
        r"(?:[\t\n\r\x0c ]|/(?!>))*"
    ))
});
static LOCATETAGEND: LazyLock<Regex> = LazyLock::new(|| {
    re(concat!(
        r"[a-zA-Z][^\t\n\r\x0c />]*[\t\n\r\x0c /]*",
        r#"(?:(?<=['"\t\n\r\x0c /])[^\t\n\r\x0c />][^\t\n\r\x0c /=>]*"#,
        r#"(?:[\t\n\r\x0c ]*=[\t\n\r\x0c ]*(?:'[^']*'|"[^"]*"|(?!['"])[^>\t\n\r\x0c ]*))?"#,
        r"[\t\n\r\x0c /]*)*>?"
    ))
});
static COMMENTCLOSE: LazyLock<Regex> = LazyLock::new(|| re(r"--!?>"));
static COMMENTABRUPT: LazyLock<Regex> = LazyLock::new(|| re(r"^-?>"));

/// One tag event of the scan.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Event {
    /// `<tag attrs>`; the flag is true for the self-closing `<tag ... />` form.
    Start {
        tag: String,
        attrs: Vec<(String, Option<String>)>,
        closing: bool,
        /// Byte offset of the `<`.
        pos: usize,
    },
    /// `</tag>`.
    End(String),
    /// `<!doctype ...>`: the declaration text without `<!` and `>`.
    Decl(String),
    /// Character data (character references converted outside raw-text elements).
    Data(String),
}

/// Anchored match: counts only when it starts exactly at `pos`.
fn match_at<'t>(
    regex: &Regex,
    text: &'t str,
    pos: usize,
) -> Option<fancy_regex::Captures<'t, str>> {
    let caps = regex.captures_from_pos(text, pos).ok().flatten()?;
    (caps.get(0)?.start() == pos).then_some(caps)
}

fn is_letter_at(text: &str, pos: usize) -> bool {
    text.as_bytes()
        .get(pos)
        .is_some_and(u8::is_ascii_alphabetic)
}

/// Decode character references in attribute values and data (`entities::unescape`).
pub(crate) fn unescape_attr(value: &str) -> String {
    super::entities::unescape(value)
}

/// Outcome of one construct parse: where to continue, or how the construct ended.
enum Step {
    /// Keep scanning at this byte offset.
    Next(usize),
    /// The construct is unterminated at end of input: the scan ends.
    /// Exceptions: `</` at the exact end is data, and an unterminated `<!doctype` is a declaration;
    /// the parse functions emit those themselves.
    Bogus,
    /// The event callback asked to stop.
    Stop,
}

/// Scan `html`, calling `on_event` for every event; stop early when it returns `false`.
/// `noscript_opaque` selects the subclassed scanners, which treat `<noscript>` as opaque text;
/// otherwise the plain tokenizer is used.
pub(crate) fn scan(html: &str, noscript_opaque: bool, mut on_event: impl FnMut(Event) -> bool) {
    let n = html.len();
    let mut i = 0;
    let mut cdata: Option<String> = None;
    let mut ctx = Ctx {
        noscript_opaque,
        cdata: &mut cdata,
    };
    while i < n {
        let raw = ctx.cdata.clone();
        let j = match &raw {
            None => html[i..].find('<').map_or(n, |p| i + p),
            Some(elem) if elem == "plaintext" => n,
            Some(elem) => find_cdata_end(html, i, elem).unwrap_or(n),
        };
        let escapable = matches!(raw.as_deref(), None | Some("title" | "textarea"));
        let ended = j >= n;
        // With nothing left to match, the raw-text content runs to the end of the input.
        if i < j {
            let text = &html[i..j];
            let data = if escapable {
                unescape_attr(text)
            } else {
                text.to_owned()
            };
            if !on_event(Event::Data(data)) {
                return;
            }
        }
        i = j;
        if ended || i >= n {
            break;
        }
        let rest = &html[i..];
        let next: Step = if is_letter_at(html, i + 1) {
            parse_starttag(html, i, &mut ctx, &mut on_event)
        } else if rest.starts_with("</") {
            if i + 2 == n {
                // `</` at the end of input is character data.
                if !on_event(Event::Data("</".to_owned())) {
                    return;
                }
                Step::Next(n)
            } else {
                parse_endtag(html, i, &mut ctx, &mut on_event)
            }
        } else if rest.starts_with("<!--") {
            parse_comment(html, i).map_or(Step::Bogus, Step::Next)
        } else if rest.starts_with("<?") {
            html[i + 2..]
                .find('>')
                .map_or(Step::Bogus, |p| Step::Next(i + 2 + p + 1))
        } else if rest.starts_with("<!") {
            parse_declaration(html, i, &mut on_event)
        } else {
            if !on_event(Event::Data("<".to_owned())) {
                return;
            }
            Step::Next(i + 1)
        };
        match next {
            Step::Next(k) => i = k,
            Step::Stop | Step::Bogus => return,
        }
    }
}

struct Ctx<'a> {
    noscript_opaque: bool,
    cdata: &'a mut Option<String>,
}

/// `</elem` followed by a whitespace, `/` or `>` (ASCII case-insensitive).
fn find_cdata_end(html: &str, from: usize, elem: &str) -> Option<usize> {
    let bytes = html.as_bytes();
    let needle = format!("</{elem}");
    let mut pos = from;
    while let Some(at) = html[pos..].find("</") {
        let start = pos + at;
        let end = start + needle.len();
        if html.is_char_boundary(end)
            && end <= bytes.len()
            && html[start..end].eq_ignore_ascii_case(&needle)
            && bytes
                .get(end)
                .is_some_and(|b| matches!(b, b'\t' | b'\n' | b'\r' | 0x0c | b' ' | b'/' | b'>'))
        {
            return Some(start);
        }
        pos = start + 2;
    }
    None
}

fn parse_comment(html: &str, i: usize) -> Option<usize> {
    let from = i + 4;
    if let Ok(Some(m)) = COMMENTCLOSE.find_from_pos(html, from) {
        return Some(m.end());
    }
    let abrupt = COMMENTABRUPT.find(&html[from..]).ok().flatten();
    abrupt.map(|m| from + m.end())
}

fn parse_declaration(html: &str, i: usize, on_event: &mut impl FnMut(Event) -> bool) -> Step {
    let rest = &html[i..];
    if rest.starts_with("<![CDATA[") {
        return html[i + 9..]
            .find("]]>")
            .map_or(Step::Bogus, |p| Step::Next(i + 9 + p + 3));
    }
    if rest.len() >= 9 && rest.is_char_boundary(9) && rest[..9].eq_ignore_ascii_case("<!doctype") {
        let (gt, end) = match html[i + 9..].find('>') {
            // Unterminated doctype: the rest of the input is the declaration.
            None => (html.len(), html.len()),
            Some(gt_rel) => (i + 9 + gt_rel, i + 9 + gt_rel + 1),
        };
        if !on_event(Event::Decl(html[i + 2..gt].to_owned())) {
            return Step::Stop;
        }
        return Step::Next(end);
    }
    html[i + 2..]
        .find('>')
        .map_or(Step::Bogus, |p| Step::Next(i + 2 + p + 1))
}

fn parse_starttag(
    html: &str,
    i: usize,
    ctx: &mut Ctx<'_>,
    on_event: &mut impl FnMut(Event) -> bool,
) -> Step {
    let Some(located) = match_at(&LOCATETAGEND, html, i + 1) else {
        return Step::Bogus;
    };
    let Some(endpos) = located.get(0).map(|m| m.end()) else {
        return Step::Bogus;
    };
    if !html[..endpos].ends_with('>') {
        return Step::Bogus;
    }
    let Some(name) = match_at(&TAGFIND, html, i + 1) else {
        return Step::Bogus;
    };
    let Some(tag_g) = name.get(1) else {
        return Step::Bogus;
    };
    let tag = tag_g.as_str().to_lowercase();
    let Some(mut k) = name.get(0).map(|m| m.end()) else {
        return Step::Bogus;
    };
    let mut attrs = Vec::new();
    let window = &html[..endpos];
    while k < endpos {
        let Some(m) = ATTRFIND
            .captures_from_pos(window, k)
            .ok()
            .flatten()
            .filter(|c| c.get(0).is_some_and(|g| g.start() == k))
        else {
            break;
        };
        let attr_name = m
            .get(1)
            .map(|g| g.as_str().to_lowercase())
            .unwrap_or_default();
        let value = if m.get(2).is_none() {
            None
        } else {
            let raw = m.get(3).map_or("", |g| g.as_str());
            let quoted = raw.len() >= 2
                && ((raw.starts_with('\'') && raw.ends_with('\''))
                    || (raw.starts_with('"') && raw.ends_with('"')));
            let inner = if quoted { &raw[1..raw.len() - 1] } else { raw };
            Some(if inner.is_empty() {
                String::new()
            } else {
                unescape_attr(inner)
            })
        };
        attrs.push((attr_name, value));
        k = m.get(0).map_or(endpos, |g| g.end());
    }
    let end = html[k.min(endpos)..endpos].trim();
    if end != ">" && end != "/>" {
        // The malformed tag text is emitted as data, and scanning continues after it.
        if !on_event(Event::Data(html[i..endpos].to_owned())) {
            return Step::Stop;
        }
        return Step::Next(endpos);
    }
    let closing = end.ends_with("/>");
    let opaque =
        !closing && (OPAQUE.contains(&tag.as_str()) || (ctx.noscript_opaque && tag == "noscript"));
    let keep = on_event(Event::Start {
        tag: tag.clone(),
        attrs,
        closing,
        pos: i,
    });
    if !keep {
        return Step::Stop;
    }
    if opaque {
        *ctx.cdata = Some(tag);
    }
    Step::Next(endpos)
}

fn parse_endtag(
    html: &str,
    i: usize,
    ctx: &mut Ctx<'_>,
    on_event: &mut impl FnMut(Event) -> bool,
) -> Step {
    if !html[i + 2..].contains('>') {
        return Step::Bogus;
    }
    if !is_letter_at(html, i + 2) {
        if html[i + 2..].starts_with('>') {
            return Step::Next(i + 3);
        }
        return html[i + 2..]
            .find('>')
            .map_or(Step::Bogus, |p| Step::Next(i + 2 + p + 1));
    }
    let Some(located) = match_at(&LOCATETAGEND, html, i + 2) else {
        return Step::Bogus;
    };
    let Some(j) = located.get(0).map(|m| m.end()) else {
        return Step::Bogus;
    };
    if !html[..j].ends_with('>') {
        return Step::Bogus;
    }
    let Some(name) = match_at(&TAGFIND, html, i + 2) else {
        return Step::Bogus;
    };
    let Some(tag_g) = name.get(1) else {
        return Step::Bogus;
    };
    let tag = tag_g.as_str().to_lowercase();
    if !on_event(Event::End(tag)) {
        return Step::Stop;
    }
    *ctx.cdata = None;
    Step::Next(j)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn events(html: &str) -> Vec<Event> {
        let mut out = Vec::new();
        scan(html, true, |e| {
            if !matches!(e, Event::Data(_)) {
                out.push(e);
            }
            true
        });
        out
    }

    #[test]
    fn script_and_comments_are_opaque() {
        let got = events(
            "<head><script>if (a<b) {}</script><!-- <div> --><title><p></title><br/></head>",
        );
        let tags: Vec<String> = got
            .iter()
            .map(|e| match e {
                Event::Start { tag, .. } => tag.clone(),
                Event::End(tag) => format!("/{tag}"),
                _ => String::new(),
            })
            .collect();
        assert_eq!(
            tags,
            ["head", "script", "/script", "title", "/title", "br", "/head"]
        );
    }

    #[test]
    fn attributes_follow_the_tolerant_grammar() {
        let got = events("<base href='/a&amp;b' x y=1 =z>");
        let Some(Event::Start { attrs, .. }) = got.first() else {
            panic!("no start tag");
        };
        assert_eq!(attrs[0], ("href".to_owned(), Some("/a&b".to_owned())));
        assert_eq!(attrs[1], ("x".to_owned(), None));
    }

    /// Character reference decoding: full HTML5 named table with longest-prefix fallback,
    /// cp1252 remaps, U+FFFD for NUL, surrogates and out-of-range code points, and removal
    /// of forbidden code points. `&copy=` has no attribute-context exception.
    #[test]
    fn unescape_follows_html_unescape() {
        let cases: [(&str, &str); 14] = [
            ("&copy=2", "\u{a9}=2"),
            ("&ampx", "&x"),
            ("&#11;", ""),
            ("&#0;", "\u{fffd}"),
            ("&#150;", "\u{2013}"),
            ("&#xD800;", "\u{fffd}"),
            ("&#x110000;", "\u{fffd}"),
            ("&notit;", "\u{ac}it;"),
            ("&copysel;", "\u{a9}sel;"),
            ("&#;", "&#;"),
            ("&gt", ">"),
            ("x&#34", "x\""),
            ("&CounterClockwiseContourIntegral;", "\u{2233}"),
            ("&Tab;&NewLine;", "\t\n"),
        ];
        for (input, want) in cases {
            assert_eq!(unescape_attr(input), want, "unescape {input:?}");
        }
    }

    /// An unterminated construct at end of input ends the scan; `</` alone becomes data and an
    /// unterminated doctype becomes a declaration.
    /// A tag closed by '>' that holds junk between the attributes and the bracket is emitted as data.
    #[test]
    fn unterminated_constructs_follow_cpython_end_handling() {
        fn tags(html: &str) -> Vec<String> {
            let mut out = Vec::new();
            scan(html, true, |e| {
                match e {
                    Event::Start { tag, .. } => out.push(tag),
                    Event::End(tag) => out.push(format!("/{tag}")),
                    _ => {}
                }
                true
            });
            out
        }
        // `<a <title>` is a complete tag: `<title` is a legal attribute name.
        assert_eq!(
            tags("<head>x<a <title>y</title>z</head>"),
            ["head", "a", "/title", "/head"]
        );
        // An unterminated `<a` drops the tail: the scan ends there.
        assert_eq!(tags("<div>t<a href=\"u <p>y</p>"), ["div"]);

        fn events_all(html: &str) -> Vec<Event> {
            let mut out = Vec::new();
            scan(html, true, |e| {
                out.push(e);
                true
            });
            out
        }
        let all = events_all("x</");
        assert_eq!(
            all,
            [Event::Data("x".to_owned()), Event::Data("</".to_owned())]
        );
        let all = events_all("x<!doctype html");
        assert_eq!(
            all,
            [
                Event::Data("x".to_owned()),
                Event::Decl("doctype html".to_owned())
            ]
        );
    }
}
