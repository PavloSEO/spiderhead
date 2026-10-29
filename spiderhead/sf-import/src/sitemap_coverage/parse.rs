//! Sitemap byte parsing: strict defused-XML semantics, sitemap-index recursion
//! with the oracle's guards (depth cap, URL cap, seen set, SSRF allow-list),
//! plus `normalize_url`/`normalized_index` ported from the reference suite.
//!
//! Fetching itself is injected: tests supply document bytes per URL, the real
//! network `_fetch` lands with the recon crate (not ported yet).

use indexmap::IndexMap;
use quick_xml::events::Event;
use quick_xml::Reader;
use serde_json::{Map, Value};
use spiderhead_parse::py_url;
use std::collections::HashSet;
use thiserror::Error;

/// `MAX_DOWNLOAD_BYTES` — cap a single download (real-fetch path).
pub const MAX_DOWNLOAD_BYTES: usize = 50 * 1024 * 1024;
/// `MAX_DECOMPRESSED_BYTES` — cap gunzip output (gzip-bomb guard).
pub const MAX_DECOMPRESSED_BYTES: usize = 100 * 1024 * 1024;
/// `MAX_SITEMAP_DEPTH` — sitemapindex recursion depth.
pub const MAX_SITEMAP_DEPTH: usize = 5;
/// `MAX_SITEMAP_URLS` — total `<loc>` across the chain.
pub const MAX_SITEMAP_URLS: usize = 200_000;
/// `MAX_SITEMAP_URLS_PER_FILE` — protocol limit per document.
pub const MAX_SITEMAP_URLS_PER_FILE: i64 = 50_000;
/// `MAX_SITEMAP_BYTES_PER_FILE` — protocol limit, uncompressed (50 MiB).
pub const MAX_SITEMAP_BYTES_PER_FILE: i64 = 52_428_800;

/// The injected `_fetch` seam: raw document bytes for a URL, or `None`.
/// reference tests monkeypatch `_fetch`; Rust callers pass any closure.
pub type Fetch<'a> = dyn Fn(&str) -> Option<Vec<u8>> + 'a;

/// The offline fetcher: refuses every URL (network is opt-in, never default).
pub fn null_fetch(_url: &str) -> Option<Vec<u8>> {
    None
}

/// Sitemap parsing/IO failure surface.
#[derive(Debug, Error)]
pub enum SitemapParseError {
    /// Decompressed body exceeds `MAX_DECOMPRESSED_BYTES`.
    #[error("decompressed sitemap exceeds size limit")]
    DecompressedTooLarge,
    /// gzip stream could not be read.
    #[error("gzip stream unreadable")]
    Gzip,
}

/// `_host(url)`: `urlparse(url).netloc.lower()`, `""` on error.
pub fn host(url: &str) -> String {
    match py_url::urlsplit(url) {
        Ok(parts) => parts.netloc.to_lowercase(),
        Err(_) => String::new(),
    }
}

/// `_origin(url)`: `scheme://netloc` of an absolute URL, else `None`.
pub fn origin(url: &str) -> Option<String> {
    let parts = py_url::urlsplit(url).ok()?;
    if !parts.scheme.is_empty() && !parts.netloc.is_empty() {
        Some(format!("{}://{}", parts.scheme, parts.netloc))
    } else {
        None
    }
}

/// `_resolve_sitemap_loc`: resolve a sitemap location only when its document
/// URL is explicit and absolute.
pub fn resolve_sitemap_loc(loc: &str, source: &str) -> String {
    if origin(source).is_some() {
        py_url::urljoin(source, loc).unwrap_or_else(|_| loc.to_owned())
    } else {
        loc.to_owned()
    }
}

/// `_safe_gunzip`: capped decompression.
pub fn safe_gunzip(data: &[u8]) -> Result<Vec<u8>, SitemapParseError> {
    use std::io::Read;
    let mut decoder = flate2::read::GzDecoder::new(data);
    let mut out = Vec::new();
    let mut chunk = [0u8; 65536];
    loop {
        let n = decoder
            .read(&mut chunk)
            .map_err(|_| SitemapParseError::Gzip)?;
        if n == 0 {
            break;
        }
        out.extend_from_slice(&chunk[..n]);
        if out.len() > MAX_DECOMPRESSED_BYTES {
            return Err(SitemapParseError::DecompressedTooLarge);
        }
    }
    Ok(out)
}

// --------------------------------------------------------------------------
// normalize_url / normalized_index (the reference suite — canonicalisation)
// --------------------------------------------------------------------------

/// `normalize_url` failure: the input is not an absolute URL (`ValueError`).
#[derive(Debug, Error)]
#[error("Invalid URL: {0}")]
pub struct NormalizeUrlError(pub String);

/// reference `SplitResult._hostinfo`: `(hostname, port_str)` — `partition(':')`
/// semantics (first colon, not last) and bracketed-IPv6 handling.
/// `s.partition(':')[2]` — text after the first colon, `""` when there is none.
fn after_first_colon(s: &str) -> &str {
    s.split_once(':').map(|(_, p)| p).unwrap_or("")
}

fn hostinfo(hostport: &str) -> (String, Option<String>) {
    if hostport.contains('[') {
        let bracketed = hostport.split_once('[').map(|(_, b)| b).unwrap_or("");
        let (hostname, rest) = bracketed.split_once(']').unwrap_or((bracketed, ""));
        let port = after_first_colon(rest);
        return (
            hostname.to_owned(),
            if port.is_empty() {
                None
            } else {
                Some(port.to_owned())
            },
        );
    }
    let (hostname, port) = {
        let (h, p) = match hostport.split_once(':') {
            Some((h, p)) => (h, p),
            None => (hostport, ""),
        };
        (h, p)
    };
    (
        hostname.to_owned(),
        if port.is_empty() {
            None
        } else {
            Some(port.to_owned())
        },
    )
}

/// Parse `host[:port]` port text the way the reference `.port` property does —
/// `int(port)`, `ValueError` on junk or out of range.
fn parse_port(text: &str, url_str: &str) -> Result<i64, NormalizeUrlError> {
    let port: i64 = text.parse().map_err(|_| {
        NormalizeUrlError(format!(
            "Port could not be cast to integer value as '{text}'"
        ))
    })?;
    if !(0..=65535).contains(&port) {
        return Err(NormalizeUrlError(format!(
            "Port out of range 0-65535 in {url_str:?}"
        )));
    }
    Ok(port)
}

/// `normalize_url` from the reference suite: lower-case the host, drop default
/// ports, remove the fragment, strip trailing slashes from non-root paths.
/// Err for non-absolute input (the oracle's `ValueError`).
pub fn normalize_url(url_str: &str) -> Result<String, NormalizeUrlError> {
    let parts = py_url::urlsplit(url_str.trim())
        .map_err(|_| NormalizeUrlError(format!("Invalid URL: '{url_str}'")))?;
    if parts.scheme.is_empty() || parts.netloc.is_empty() {
        return Err(NormalizeUrlError(format!("Invalid URL: '{url_str}'")));
    }
    let scheme = parts.scheme.to_lowercase();
    // `netloc.rpartition('@')[2]` — hostinfo is everything after the last '@'.
    let hostport = match parts.netloc.rfind('@') {
        Some(i) => &parts.netloc[i + 1..],
        None => parts.netloc.as_str(),
    };
    let (hostname_raw, port_text) = hostinfo(hostport);
    let host = hostname_raw.to_lowercase();
    let port_raw = match port_text {
        Some(t) => Some(parse_port(&t, url_str)?),
        None => None,
    };
    let port = match port_raw {
        Some(443) if scheme == "https" => None,
        Some(80) if scheme == "http" => None,
        other => other,
    };
    let mut netloc = host;
    if let Some(p) = port {
        netloc = format!("{netloc}:{p}");
    }
    // Rebuild userinfo from the raw netloc prefix, like the oracle does via
    // `parts.username`/`parts.password` (percent-decoded values).
    if let Some(i) = parts.netloc.rfind('@') {
        let userinfo = &parts.netloc[..i];
        let ui = match userinfo.split_once(':') {
            Some((u, p)) => format!("{}:{}", py_url::unquote(u), py_url::unquote(p)),
            None => py_url::unquote(userinfo),
        };
        netloc = format!("{ui}@{netloc}");
    }
    let mut path = parts.path;
    if path.len() > 1 && path.ends_with('/') {
        while path.ends_with('/') {
            path.pop();
        }
    }
    Ok(py_url::urlunsplit(
        &scheme,
        &netloc,
        &path,
        &parts.query,
        "",
    ))
}

/// `_normalized_index`: normalised key -> URL as written, first occurrence
/// wins. Insertion-ordered like the reference dict.
pub fn normalized_index<'a, I>(urls: I) -> IndexMap<String, String>
where
    I: IntoIterator<Item = &'a str>,
{
    let mut out = IndexMap::new();
    for url in urls {
        if url.is_empty() {
            continue;
        }
        if let Ok(key) = normalize_url(url) {
            out.entry(key).or_insert_with(|| url.to_owned());
        }
    }
    out
}

// --------------------------------------------------------------------------
// minimal DOM for the strict sitemap parse
// --------------------------------------------------------------------------

/// One element with its direct text (the `.text` convention: content before
/// the first child element) and its children in document order.
struct El {
    name: String,
    text: String,
    children: Vec<El>,
}

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

/// Strict, non-recovering parse (defusedxml's contract): any malformed input
/// is an error, never a partial tree.
fn parse_xml(data: &[u8]) -> Option<El> {
    let data = data.strip_prefix(b"\xef\xbb\xbf").unwrap_or(data);
    let mut reader = Reader::from_reader(data);
    let mut stack: Vec<El> = Vec::new();
    let mut root: Option<El> = None;
    loop {
        match reader.read_event() {
            Ok(Event::Start(e)) => {
                stack.push(El {
                    name: local_name(e.name().as_ref()),
                    text: String::new(),
                    children: Vec::new(),
                });
            }
            Ok(Event::Empty(e)) => {
                let el = El {
                    name: local_name(e.name().as_ref()),
                    text: String::new(),
                    children: Vec::new(),
                };
                match stack.last_mut() {
                    Some(parent) => parent.children.push(el),
                    None => {
                        if root.is_some() {
                            return None;
                        }
                        root = Some(el);
                    }
                }
            }
            Ok(Event::Text(e)) => {
                match stack.last_mut() {
                    Some(top) => {
                        // `.text` is content before the first child element only.
                        if top.children.is_empty() {
                            // ElementTree resolves entities in `.text` and raises
                            // on any that are undefined — `unescape` errors the
                            // same way.
                            let decoded = e.xml_content(quick_xml::XmlVersion::Explicit1_0).ok()?;
                            let text = quick_xml::escape::unescape(&decoded).ok()?.into_owned();
                            top.text.push_str(&text);
                        }
                    }
                    None => {
                        // Outside the root element only whitespace is legal —
                        // anything else is "junk after document element".
                        let decoded = e.xml_content(quick_xml::XmlVersion::Explicit1_0).ok()?;
                        if !decoded.trim().is_empty() {
                            return None;
                        }
                    }
                }
            }
            Ok(Event::CData(e)) => {
                // CDATA outside the root element is a parse error.
                let top = stack.last_mut()?;
                if top.children.is_empty() {
                    let text = e.decode().ok()?.into_owned();
                    top.text.push_str(&text);
                }
            }
            Ok(Event::End(_)) => {
                let el = stack.pop()?;
                match stack.last_mut() {
                    Some(parent) => parent.children.push(el),
                    None => {
                        if root.is_some() {
                            return None;
                        }
                        root = Some(el);
                    }
                }
            }
            Ok(Event::Decl(_)) | Ok(Event::PI(_)) | Ok(Event::Comment(_)) => {}
            Ok(Event::GeneralRef(e)) => {
                // ElementTree resolves the five predefined entities and numeric
                // character references inline; anything else is an undefined
                // entity and raises — the same contract, event by event.
                let raw = e.decode().ok()?;
                let resolved = if let Some(ch) = e.resolve_char_ref().ok().flatten() {
                    Some(ch.to_string())
                } else {
                    quick_xml::escape::resolve_predefined_entity(&raw).map(str::to_owned)
                };
                match (stack.last_mut(), resolved) {
                    (Some(top), Some(text)) => {
                        if top.children.is_empty() {
                            top.text.push_str(&text);
                        }
                    }
                    _ => return None,
                }
            }
            // A DTD can smuggle in entity definitions — reject outright.
            Ok(Event::DocType(_)) => return None,
            Ok(Event::Eof) => break,
            Err(_) => return None,
        }
    }
    if !stack.is_empty() {
        return None;
    }
    root
}

/// `_local_children`: direct children whose local name is `name`.
fn local_children<'a>(el: &'a El, name: &str) -> Vec<&'a El> {
    el.children.iter().filter(|c| c.name == name).collect()
}

/// `_local_text`: trimmed text of the first local-name child, or `None`.
fn local_text(el: &El, name: &str) -> Option<String> {
    // The oracle returns the first local-name child's text — it never scans
    // past an empty one for a later sibling's.
    let child = local_children(el, name).into_iter().next()?;
    let text = child.text.trim();
    if text.is_empty() {
        None
    } else {
        Some(text.to_owned())
    }
}

// --------------------------------------------------------------------------
// _parse_sitemap_bytes
// --------------------------------------------------------------------------

/// The knobs a test pins (the oracle monkeypatches these module constants)
/// and the sinks the parse reports into.
pub struct ParseState {
    /// Document URLs already followed (cycle guard).
    pub seen: HashSet<String>,
    /// SSRF allow-list; empty means "no host filter" (oracle semantics).
    pub allowed_hosts: HashSet<String>,
    /// Child sitemaps that could not be fetched *or parsed*.
    pub failures: Vec<String>,
    /// One record per parsed urlset: `url`, `bytes`, `declared`.
    pub documents: Vec<Map<String, Value>>,
    /// Indexes cut short by the depth cap or the URL cap.
    pub truncated: Vec<String>,
    /// `failures is not None` in the oracle: record failures only when set.
    pub track_failures: bool,
    /// `documents is not None`.
    pub track_documents: bool,
    /// `truncated is not None`.
    pub track_truncated: bool,
    /// `MAX_SITEMAP_URLS` override point for tests.
    pub max_urls: usize,
    /// `MAX_SITEMAP_DEPTH` override point for tests.
    pub max_depth: usize,
}

impl Default for ParseState {
    fn default() -> Self {
        Self {
            seen: HashSet::new(),
            allowed_hosts: HashSet::new(),
            failures: Vec::new(),
            documents: Vec::new(),
            truncated: Vec::new(),
            track_failures: false,
            track_documents: false,
            track_truncated: false,
            max_urls: MAX_SITEMAP_URLS,
            max_depth: MAX_SITEMAP_DEPTH,
        }
    }
}

/// `_parse_sitemap_bytes`: `[{loc, lastmod, source, *_entries}]`, recursing
/// through `<sitemapindex>` with the oracle's guards.
pub fn parse_sitemap_bytes(
    data: &[u8],
    state: &mut ParseState,
    source: &str,
    depth: usize,
    fetch: &Fetch<'_>,
) -> Vec<Map<String, Value>> {
    // Reject DTDs/entities outright — sitemaps never use them
    // (XXE / billion-laughs guard).
    if data.windows(b"<!DOCTYPE".len()).any(|w| w == b"<!DOCTYPE")
        || data.windows(b"<!ENTITY".len()).any(|w| w == b"<!ENTITY")
    {
        if state.track_failures && !source.is_empty() {
            state.failures.push(source.to_owned());
        }
        return Vec::new();
    }
    let Some(root) = parse_xml(data) else {
        if state.track_failures && !source.is_empty() {
            state.failures.push(source.to_owned());
        }
        return Vec::new();
    };
    let mut out: Vec<Map<String, Value>> = Vec::new();
    if root.name == "sitemapindex" {
        if depth >= state.max_depth {
            if state.track_truncated && !source.is_empty() {
                state.truncated.push(source.to_owned());
            }
            return out;
        }
        let sitemap_elements = local_children(&root, "sitemap");
        for (idx, sm) in sitemap_elements.iter().enumerate() {
            let Some(loc_text) = local_text(sm, "loc") else {
                continue;
            };
            let loc = resolve_sitemap_loc(loc_text.trim(), source);
            // SSRF guard: only follow children on an allowed host.
            if state.seen.contains(&loc)
                || (!state.allowed_hosts.is_empty() && !state.allowed_hosts.contains(&host(&loc)))
            {
                continue;
            }
            state.seen.insert(loc.clone());
            match fetch(&loc) {
                Some(child) => {
                    out.extend(parse_sitemap_bytes(&child, state, &loc, depth + 1, fetch))
                }
                None => {
                    if state.track_failures {
                        state.failures.push(loc.clone());
                    }
                }
            }
            if out.len() >= state.max_urls {
                // The URL cap was hit mid-index: every sibling after this one
                // is never fetched. Name the dropped children so callers get
                // the same honest "incomplete evidence" signal.
                if state.track_truncated {
                    for remaining in &sitemap_elements[idx + 1..] {
                        if let Some(rloc) = local_text(remaining, "loc") {
                            state
                                .truncated
                                .push(resolve_sitemap_loc(rloc.trim(), source));
                        }
                    }
                }
                break;
            }
        }
    } else {
        // urlset
        let mut declared = 0i64;
        for url_el in local_children(&root, "url") {
            declared += 1;
            let loc = local_text(url_el, "loc");
            let lastmod = local_text(url_el, "lastmod");
            // Extension attachments are counted per entry, never merged into
            // the page population.
            let image_entries = local_children(url_el, "image").len() as i64;
            let news_entries = local_children(url_el, "news").len() as i64;
            let video_entries = local_children(url_el, "video").len() as i64;
            if let Some(loc) = loc {
                let mut entry = Map::new();
                entry.insert(
                    "loc".to_owned(),
                    Value::String(resolve_sitemap_loc(loc.trim(), source)),
                );
                entry.insert(
                    "lastmod".to_owned(),
                    match lastmod {
                        Some(t) if !t.trim().is_empty() => Value::String(t.trim().to_owned()),
                        _ => Value::Null,
                    },
                );
                entry.insert("source".to_owned(), Value::String(source.to_owned()));
                if image_entries > 0 {
                    entry.insert("image_entries".to_owned(), Value::from(image_entries));
                }
                if news_entries > 0 {
                    entry.insert("news_entries".to_owned(), Value::from(news_entries));
                }
                if video_entries > 0 {
                    entry.insert("video_entries".to_owned(), Value::from(video_entries));
                }
                out.push(entry);
                if out.len() >= state.max_urls {
                    // The parse stops here, so the declared count stops being
                    // trustworthy; the document record below says how far it
                    // got rather than implying it read the whole file.
                    break;
                }
            }
        }
        if state.track_documents {
            let mut doc = Map::new();
            doc.insert("url".to_owned(), Value::String(source.to_owned()));
            doc.insert("bytes".to_owned(), Value::from(data.len() as i64));
            doc.insert("declared".to_owned(), Value::from(declared));
            state.documents.push(doc);
        }
    }
    out
}
