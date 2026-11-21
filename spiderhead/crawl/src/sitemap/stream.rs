//! Incremental XML streaming over quick-xml: one pass validates that a decoded
//! document is well-formed, the next delivers each top-level child as it
//! closes, so a large urlset never materializes its tree (issue #46).
//!
//! quick-xml is strict; the sitemap answers pin a tolerant recovering parser
//! (`parse_sitemap`). The split keeps both contracts: a document quick-xml
//! accepts is exactly the document the tolerant parse would recover unchanged,
//! so it streams entry by entry; anything quick-xml refuses falls back to the
//! tolerant document parse, which is bounded by the fetched-body cap.

use super::xml::Element;
use quick_xml::events::{BytesStart, BytesText, Event};
use quick_xml::{Decoder, Reader, XmlVersion};

/// What the validation pass decided about one decoded document.
pub(super) enum StreamScan {
    /// Well-formed XML with exactly one root element: stream it.
    Clean,
    /// Only declarations, comments and whitespace before `EOF`: the tolerant
    /// parse answers `empty document` for this.
    NoRoot,
    /// Anything quick-xml refuses (malformed markup, undefined entities,
    /// non-whitespace outside the root, more than one root): fall back to the
    /// tolerant document parse.
    Malformed,
}

/// Validate one decoded document without materializing anything.
pub(super) fn validate_document(text: &str) -> StreamScan {
    let mut reader = Reader::from_str(text);
    let mut depth = 0usize;
    let mut roots = 0usize;
    let mut root_closed = false;
    loop {
        match reader.read_event() {
            Ok(Event::Start(_)) => {
                if depth == 0 {
                    if roots > 0 {
                        // A second root: the tolerant parse drops it silently,
                        // so hand the document to the fallback instead.
                        return StreamScan::Malformed;
                    }
                    roots += 1;
                }
                depth += 1;
            }
            Ok(Event::Empty(_)) => {
                if depth == 0 {
                    if roots > 0 {
                        return StreamScan::Malformed;
                    }
                    roots += 1;
                    root_closed = true;
                }
            }
            Ok(Event::End(_)) => {
                depth = depth.saturating_sub(1);
                if depth == 0 {
                    root_closed = true;
                }
            }
            Ok(Event::Text(e)) => {
                let Some(text) = decode_text(&e) else {
                    return StreamScan::Malformed;
                };
                if depth == 0 && !text.trim().is_empty() {
                    // Non-whitespace before or after the root: the tolerant
                    // parse answers NoDocument for this.
                    return StreamScan::Malformed;
                }
            }
            Ok(Event::CData(e)) => {
                if depth == 0 || e.decode().is_err() {
                    return StreamScan::Malformed;
                }
            }
            Ok(Event::GeneralRef(e)) => {
                if depth == 0 {
                    return StreamScan::Malformed;
                }
                let Ok(raw) = e.decode() else {
                    return StreamScan::Malformed;
                };
                let resolved = match e.resolve_char_ref() {
                    Ok(Some(ch)) => Some(ch.to_string()),
                    Ok(None) => {
                        quick_xml::escape::resolve_predefined_entity(&raw).map(str::to_owned)
                    }
                    Err(_) => None,
                };
                if resolved.is_none() {
                    // An undefined entity: the tolerant parse recovers it with
                    // its own rules, so this document takes the fallback.
                    return StreamScan::Malformed;
                }
            }
            Ok(Event::Decl(_))
            | Ok(Event::PI(_))
            | Ok(Event::Comment(_))
            | Ok(Event::DocType(_)) => {}
            Ok(Event::Eof) => break,
            Err(_) => return StreamScan::Malformed,
        }
    }
    match roots {
        0 => StreamScan::NoRoot,
        _ if depth == 0 && root_closed => StreamScan::Clean,
        _ => StreamScan::Malformed,
    }
}

/// What one streaming pass reports about a validated document.
pub(super) enum StreamEvent {
    /// The root element's local name, before any child is delivered.
    Root(String),
    /// A completed top-level child of the root, one at a time.
    Child(Element),
}

/// Stream one validated document: the root's local name first, then every
/// top-level child as a standalone [`Element`] (the only subtree that is ever
/// materialized). Call only after [`validate_document`] returned `Clean`.
pub(super) fn stream_elements(text: &str, on_event: &mut dyn FnMut(StreamEvent)) {
    let mut reader = Reader::from_str(text);
    let mut stack: Vec<Open> = Vec::new();
    let mut root_reported = false;
    loop {
        match reader.read_event() {
            Ok(Event::Start(e)) => {
                if stack.is_empty() && !root_reported {
                    root_reported = true;
                    on_event(StreamEvent::Root(local_name(e.name().as_ref())));
                } else if let Some(top) = stack.last_mut() {
                    top.text_open = false;
                }
                stack.push(Open {
                    element: Element {
                        name: local_name(e.name().as_ref()),
                        attrs: attributes(&e, reader.decoder()),
                        text: None,
                        children: Vec::new(),
                    },
                    text: String::new(),
                    text_open: true,
                    text_any: false,
                });
            }
            Ok(Event::Empty(e)) => {
                let name = local_name(e.name().as_ref());
                let element = Element {
                    attrs: attributes(&e, reader.decoder()),
                    name,
                    text: None,
                    children: Vec::new(),
                };
                if stack.is_empty() && !root_reported {
                    // A self-closing root: named, with no children.
                    root_reported = true;
                    on_event(StreamEvent::Root(element.name.clone()));
                } else {
                    place(&mut stack, element, on_event);
                }
            }
            Ok(Event::End(_)) => finish_top(&mut stack, on_event),
            Ok(Event::Text(e)) => {
                if let Some(text) = decode_text(&e) {
                    append_text(&mut stack, &text);
                }
            }
            Ok(Event::CData(e)) => {
                if let Ok(text) = e.decode() {
                    append_text(&mut stack, &text);
                }
            }
            Ok(Event::GeneralRef(e)) => {
                let resolved = e.decode().ok().and_then(|raw| match e.resolve_char_ref() {
                    Ok(Some(ch)) => Some(ch.to_string()),
                    Ok(None) => {
                        quick_xml::escape::resolve_predefined_entity(&raw).map(str::to_owned)
                    }
                    Err(_) => None,
                });
                if let Some(text) = resolved {
                    append_text(&mut stack, &text);
                }
            }
            // Comments, PIs and a DOCTYPE end the text run of the open element,
            // like the tolerant parser's recovery.
            Ok(Event::PI(_)) | Ok(Event::Comment(_)) | Ok(Event::DocType(_)) => {
                if let Some(top) = stack.last_mut() {
                    top.text_open = false;
                }
            }
            Ok(Event::Decl(_)) => {}
            Ok(Event::Eof) | Err(_) => break,
        }
    }
    while !stack.is_empty() {
        // A validated document cannot end inside an element (the validator
        // refuses it); unwind defensively all the same.
        stack.last_mut().expect("stack non-empty").text_open = false;
        finish_top(&mut stack, on_event);
    }
}

/// One open element and the text accumulated before its first child.
struct Open {
    element: Element,
    text: String,
    /// `false` once a child element or a comment/PI/DOCTYPE ended the text run.
    text_open: bool,
    /// Whether any character event fed the text at all: an empty element has
    /// `text: None` where a whitespace-only element has `text: Some("")`.
    text_any: bool,
}

fn append_text(stack: &mut [Open], text: &str) {
    if let Some(top) = stack.last_mut() {
        if top.text_open {
            top.text.push_str(text);
            top.text_any = true;
        }
    }
}

/// Place one completed element: a direct child of the root is delivered to
/// the callback (the only subtree that is ever materialized), deeper elements
/// nest into their parent, and the root itself closes silently (it was
/// already reported by name).
fn place(stack: &mut [Open], element: Element, on_event: &mut dyn FnMut(StreamEvent)) {
    match stack.len() {
        0 => {}
        1 => on_event(StreamEvent::Child(element)),
        _ => {
            let parent = stack.last_mut().expect("stack non-empty");
            parent.element.children.push(element);
            parent.text_open = false;
        }
    }
}

fn finish_top(stack: &mut Vec<Open>, on_event: &mut dyn FnMut(StreamEvent)) {
    let Some(done) = stack.pop() else { return };
    let text = if done.text_any { Some(done.text) } else { None };
    let element = Element {
        name: done.element.name,
        attrs: done.element.attrs,
        text,
        children: done.element.children,
    };
    place(stack, element, on_event);
}

/// Resolved text of a Text event; `None` on an undefined entity (the
/// validator has already routed such documents to the fallback).
fn decode_text(e: &BytesText) -> Option<String> {
    let decoded = e.xml_content(XmlVersion::Explicit1_0).ok()?;
    quick_xml::escape::unescape(&decoded)
        .ok()
        .map(|text| text.into_owned())
}

fn attributes(e: &BytesStart, decoder: Decoder) -> Vec<(String, String)> {
    let mut out = Vec::new();
    for attribute in e.attributes().flatten() {
        let value = attribute
            .decoded_and_normalized_value(XmlVersion::Explicit1_0, decoder)
            .map(|value| value.into_owned())
            .unwrap_or_default();
        out.push((local_name(attribute.key.as_ref()), value));
    }
    out
}

/// The tag/attribute local name: everything after the last `}` or `:`.
fn local_name(raw: &[u8]) -> String {
    let name = String::from_utf8_lossy(raw);
    name.rsplit('}')
        .next()
        .unwrap_or(&name)
        .rsplit(':')
        .next()
        .unwrap_or(&name)
        .to_owned()
}
#[cfg(test)]
mod tests {
    use super::*;
    use crate::sitemap::parse::{parse_sitemap, urlset_entry};
    use serde_json::{json, Value};

    /// Stream one well-formed document the way `stream_document` does and
    /// return (root kind, index children, urlset entries).
    fn streamed(body: &str, base: &str) -> (String, Vec<Value>, Vec<Value>) {
        let mut kind = String::new();
        let mut index: Vec<Value> = Vec::new();
        let mut entries: Vec<Value> = Vec::new();
        stream_elements(body, &mut |event| match event {
            StreamEvent::Root(name) => {
                kind = match name.as_str() {
                    "sitemapindex" => "index".to_owned(),
                    "urlset" => "urlset".to_owned(),
                    _ => "unknown".to_owned(),
                };
            }
            StreamEvent::Child(child) => match kind.as_str() {
                "index" => {
                    if child.name != "sitemap" {
                        return;
                    }
                    let Some(loc) = child.child_text("loc").filter(|loc| !loc.is_empty()) else {
                        return;
                    };
                    let joined = spiderhead_parse::py_url::urljoin(base, &loc)
                        .unwrap_or_else(|_| loc.clone());
                    index.push(json!({"loc": joined, "lastmod": child.child_text("lastmod")}));
                }
                "urlset" => {
                    if child.name != "url" {
                        return;
                    }
                    if let Some(entry) = urlset_entry(&child, base) {
                        entries.push(entry);
                    }
                }
                _ => {}
            },
        });
        (kind, index, entries)
    }

    /// The streaming walk must classify and extract exactly what the tolerant
    /// document parse does for well-formed documents: the walk streams entries
    /// instead of buffering them (issue #46), and the answers must not drift.
    fn assert_streams_like_parse(body: &str, base: &str) {
        assert!(
            matches!(validate_document(body), StreamScan::Clean),
            "test doc must validate as clean: {body}"
        );
        let parsed = parse_sitemap(body.as_bytes(), base);
        let (kind, index, entries) = streamed(body, base);
        assert_eq!(
            parsed.get("type").and_then(Value::as_str),
            Some(kind.as_str())
        );
        assert_eq!(
            parsed
                .get("sitemaps")
                .and_then(Value::as_array)
                .unwrap_or(&Vec::new()),
            &index,
            "index children diverge"
        );
        assert_eq!(
            parsed
                .get("urls")
                .and_then(Value::as_array)
                .unwrap_or(&Vec::new()),
            &entries,
            "urlset entries diverge"
        );
    }

    #[test]
    fn urlset_streams_exactly_like_the_document_parse() {
        assert_streams_like_parse(
            r#"<?xml version="1.0" encoding="UTF-8"?>
<urlset xmlns="http://www.sitemaps.org/schemas/sitemap/0.9">
  <url><loc>http://site.test/</loc><lastmod>2026-01-01</lastmod></url>
  <url><loc>http://site.test/a.html</loc><lastmod>2026-01-02</lastmod></url>
  <url><loc>
      http://site.test/spaced.html
    </loc><changefreq>daily</changefreq><priority>0.5</priority></url>
  <url><loc>http://site.test/cdata.html</loc></url>
  <empty/>
</urlset>
"#,
            "http://site.test/",
        );
    }

    #[test]
    fn cdata_and_entities_stream_like_the_document_parse() {
        assert_streams_like_parse(
            r#"<?xml version="1.0"?>
<urlset>
  <url><loc><![CDATA[http://site.test/raw.html?a=1&b=2]]></loc></url>
  <url><loc>http://site.test/ent.html?p=a&amp;b</loc><lastmod>2026&#8211;01</lastmod></url>
</urlset>"#,
            "http://site.test/",
        );
    }

    #[test]
    fn sitemapindex_streams_exactly_like_the_document_parse() {
        assert_streams_like_parse(
            r#"<?xml version="1.0" encoding="UTF-8"?>
<sitemapindex xmlns="http://www.sitemaps.org/schemas/sitemap/0.9">
  <sitemap><loc>http://site.test/sitemap-pages.xml</loc><lastmod>2026-01-01</lastmod></sitemap>
  <sitemap><loc>http://site.test/sitemap-more.xml</loc></sitemap>
</sitemapindex>
"#,
            "http://site.test/",
        );
    }

    #[test]
    fn malformed_documents_take_the_fallback_not_the_stream() {
        // Truncated tag, undefined entity, junk before the root, second root.
        for body in [
            r#"<urlset><url><loc>http://site.test/a</loc>"#,
            r#"<urlset><url><loc>http://site.test/&nope;a</loc></url></urlset>"#,
            r#"junk<urlset><url><loc>http://site.test/a</loc></url></urlset>"#,
            r#"<urlset/><sitemapindex/>"#,
        ] {
            assert!(
                !matches!(validate_document(body), StreamScan::Clean),
                "must not stream: {body}"
            );
        }
        assert!(matches!(
            validate_document(concat!(
                "<?xml version=\"1.0\"?>\n",
                "<!-- only comments -->\n"
            )),
            StreamScan::NoRoot
        ));
    }
}
