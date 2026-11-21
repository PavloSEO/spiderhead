//! URL split, join and quote helpers with the reference URL-parsing behaviour.
//! Provides `urlsplit`, `urlparse`, `urlunsplit`, `urlunparse`, `urljoin`, `parse_qsl`, `unquote`,
//! `quote_plus` and `urlencode`. Output matches the reference byte for byte, quirks included:
//! `urlparse` moves a `;params` tail out of the path, and `urlunsplit` does not put it back;
//! a network scheme with an empty netloc keeps the `///` prefix.
//! The netloc validation done inside `urlsplit` (brackets, NFKC) is shared with core's
//! `url_normalize::check_netloc`.

use thiserror::Error;

/// Schemes whose relative references `urljoin` resolves.
const USES_RELATIVE: &[&str] = &[
    "", "ftp", "http", "gopher", "nntp", "imap", "wais", "file", "https", "shttp", "mms",
    "prospero", "rtsp", "rtsps", "rtspu", "sftp", "svn", "svn+ssh", "ws", "wss",
];

/// Schemes that take a netloc (`//host`), as `urlunsplit` and `urljoin` see them.
const USES_NETLOC: &[&str] = &[
    "",
    "ftp",
    "http",
    "gopher",
    "nntp",
    "telnet",
    "imap",
    "wais",
    "file",
    "mms",
    "https",
    "shttp",
    "snews",
    "prospero",
    "rtsp",
    "rtsps",
    "rtspu",
    "rsync",
    "svn",
    "svn+ssh",
    "sftp",
    "nfs",
    "git",
    "git+ssh",
    "ws",
    "wss",
    "itms-services",
];

/// Schemes whose last path segment may carry `;params`.
const USES_PARAMS: &[&str] = &[
    "", "ftp", "hdl", "prospero", "http", "imap", "https", "shttp", "rtsp", "rtsps", "rtspu",
    "sip", "sips", "mms", "sftp", "tel",
];

/// Failure of the splitter on malformed input.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum UrlError {
    /// The netloc has an unbalanced bracket, so no host can be read from it.
    #[error("Invalid IPv6 URL")]
    InvalidIpv6,
}

/// The five parts `urlsplit` returns.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct SplitUrl {
    /// Lower-cased scheme, or the default scheme when the input has none.
    pub scheme: String,
    /// Authority between `//` and the first of `/?#`.
    pub netloc: String,
    /// Path, with any `;params` still attached.
    pub path: String,
    /// Query after `?`, without the `?`.
    pub query: String,
    /// Fragment after `#`, without the `#`.
    pub fragment: String,
}

/// The six parts `urlparse` returns.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct ParsedUrl {
    /// Lower-cased scheme, or the default scheme when the input has none.
    pub scheme: String,
    /// Authority between `//` and the first of `/?#`.
    pub netloc: String,
    /// Path without its `;params` tail.
    pub path: String,
    /// The `;params` tail of the last path segment.
    pub params: String,
    /// Query after `?`.
    pub query: String,
    /// Fragment after `#`.
    pub fragment: String,
}

/// Characters trimmed at both ends before splitting: WHATWG C0 controls and space.
fn is_c0_or_space(c: char) -> bool {
    (c as u32) <= 0x20
}

/// Remove characters dropped from every URL before splitting (tab, CR, LF).
fn drop_unsafe(text: &str) -> String {
    text.chars()
        .filter(|c| !matches!(c, '\t' | '\r' | '\n'))
        .collect()
}

/// Internal split with an absent component kept as `None`.
///
/// The public functions turn `None` into an empty string; `urljoin` needs the difference, since
/// an empty query (`?`) is kept while an absent one is inherited from the base.
struct RawSplit {
    scheme: Option<String>,
    netloc: Option<String>,
    path: String,
    query: Option<String>,
    fragment: Option<String>,
}

/// Split with an optional default scheme: `None` when absent, otherwise the default.
fn urlsplit_raw(url: &str, default_scheme: Option<&str>) -> Result<RawSplit, UrlError> {
    let mut url = drop_unsafe(url.trim_start_matches(is_c0_or_space));
    let mut scheme = default_scheme.map(|s| drop_unsafe(s.trim_matches(is_c0_or_space)));
    if let Some(i) = url.find(':') {
        let head = &url[..i];
        let scheme_shaped = i > 0
            && head.starts_with(|c: char| c.is_ascii_alphabetic())
            && head
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || matches!(c, '+' | '-' | '.'));
        if scheme_shaped {
            scheme = Some(head.to_ascii_lowercase());
            url = url[i + 1..].to_owned();
        }
    }

    let mut netloc = None;
    if let Some(after) = url.strip_prefix("//") {
        let end = after.find(['/', '?', '#']).unwrap_or(after.len());
        let authority = after[..end].to_owned();
        spiderhead_core::url_normalize::check_netloc(&authority)
            .map_err(|_| UrlError::InvalidIpv6)?;
        url = after[end..].to_owned();
        netloc = Some(authority);
    }

    let fragment = match url.split_once('#') {
        Some((before, after)) => {
            let after = after.to_owned();
            url = before.to_owned();
            Some(after)
        }
        None => None,
    };
    let query = match url.split_once('?') {
        Some((before, after)) => {
            let after = after.to_owned();
            url = before.to_owned();
            Some(after)
        }
        None => None,
    };
    Ok(RawSplit {
        scheme,
        netloc,
        path: url,
        query,
        fragment,
    })
}

/// Split with no default scheme.
pub fn urlsplit(url: &str) -> Result<SplitUrl, UrlError> {
    let raw = urlsplit_raw(url, Some(""))?;
    Ok(SplitUrl {
        scheme: raw.scheme.unwrap_or_default(),
        netloc: raw.netloc.unwrap_or_default(),
        path: raw.path,
        query: raw.query.unwrap_or_default(),
        fragment: raw.fragment.unwrap_or_default(),
    })
}

/// Cut the `;params` tail from the last path segment.
fn split_params(path: &str) -> (&str, &str) {
    let found = match path.rfind('/') {
        Some(slash) => path[slash..].find(';').map(|offset| slash + offset),
        None => path.find(';'),
    };
    match found {
        Some(i) => (&path[..i], &path[i + 1..]),
        None => (path, ""),
    }
}

/// Parse with no default scheme: split, then cut `;params` from the path.
pub fn urlparse(url: &str) -> Result<ParsedUrl, UrlError> {
    let raw = urlsplit_raw(url, Some(""))?;
    let scheme = raw.scheme.unwrap_or_default();
    let (path, params) = if USES_PARAMS.contains(&scheme.as_str()) && raw.path.contains(';') {
        let (path, params) = split_params(&raw.path);
        (path.to_owned(), params.to_owned())
    } else {
        (raw.path, String::new())
    };
    Ok(ParsedUrl {
        scheme,
        netloc: raw.netloc.unwrap_or_default(),
        path,
        params,
        query: raw.query.unwrap_or_default(),
        fragment: raw.fragment.unwrap_or_default(),
    })
}

/// Join split components back into a URL; components may be absent (`None`).
fn urlunsplit_raw(
    scheme: Option<&str>,
    netloc: Option<&str>,
    path: &str,
    query: Option<&str>,
    fragment: Option<&str>,
) -> String {
    let mut url = path.to_owned();
    if let Some(netloc) = netloc {
        if !url.is_empty() && !url.starts_with('/') {
            url.insert(0, '/');
        }
        url = format!("//{netloc}{url}");
    } else if url.starts_with("//") {
        url = format!("//{url}");
    }
    if let Some(scheme) = scheme.filter(|s| !s.is_empty()) {
        url = format!("{scheme}:{url}");
    }
    if let Some(query) = query {
        url = format!("{url}?{query}");
    }
    if let Some(fragment) = fragment {
        url = format!("{url}#{fragment}");
    }
    url
}

/// Join five components into a URL; empty components are omitted.
///
/// An empty netloc is restored as `//` only for a network scheme whose path is empty or rooted,
/// as the reference rules decide. Empty query and fragment are omitted.
pub fn urlunsplit(scheme: &str, netloc: &str, path: &str, query: &str, fragment: &str) -> String {
    let netloc_opt = netloc_for(scheme, netloc, path);
    urlunsplit_raw(
        non_empty(scheme),
        netloc_opt,
        path,
        non_empty(query),
        non_empty(fragment),
    )
}

/// The netloc decision for `urlunsplit` and `urlunparse`, from the path before any `;params`.
fn netloc_for<'a>(scheme: &str, netloc: &'a str, path: &str) -> Option<&'a str> {
    if !netloc.is_empty() {
        Some(netloc)
    } else if !scheme.is_empty()
        && USES_NETLOC.contains(&scheme)
        && (path.is_empty() || path.starts_with('/'))
    {
        Some("")
    } else {
        None
    }
}

fn non_empty(text: &str) -> Option<&str> {
    if text.is_empty() {
        None
    } else {
        Some(text)
    }
}

/// Join six components; the netloc decision uses the path before `;params`, then params join it.
pub fn urlunparse(parts: &ParsedUrl) -> String {
    let netloc_opt = netloc_for(&parts.scheme, &parts.netloc, &parts.path);
    let path = if parts.params.is_empty() {
        parts.path.clone()
    } else {
        format!("{};{}", parts.path, parts.params)
    };
    urlunsplit_raw(
        non_empty(&parts.scheme),
        netloc_opt,
        &path,
        non_empty(&parts.query),
        non_empty(&parts.fragment),
    )
}

/// Resolve `url` against `base` with the reference join algorithm.
pub fn urljoin(base: &str, url: &str) -> Result<String, UrlError> {
    if base.is_empty() {
        return Ok(url.to_owned());
    }
    if url.is_empty() {
        return Ok(base.to_owned());
    }
    let base_parts = urlsplit_raw(base, None)?;
    let mut parts = urlsplit_raw(url, None)?;
    if parts.scheme.is_none() {
        parts.scheme.clone_from(&base_parts.scheme);
    }
    let scheme = parts.scheme.clone().unwrap_or_default();
    let base_scheme = base_parts.scheme.clone().unwrap_or_default();
    if scheme != base_scheme || (!scheme.is_empty() && !USES_RELATIVE.contains(&scheme.as_str())) {
        return Ok(url.to_owned());
    }

    if scheme.is_empty() || USES_NETLOC.contains(&scheme.as_str()) {
        if let Some(netloc) = parts.netloc.as_deref().filter(|n| !n.is_empty()) {
            return Ok(urlunsplit_raw(
                parts.scheme.as_deref(),
                Some(netloc),
                &parts.path,
                parts.query.as_deref(),
                parts.fragment.as_deref(),
            ));
        }
        parts.netloc.clone_from(&base_parts.netloc);
    }

    if parts.path.is_empty() {
        let mut query = parts.query.clone();
        let mut fragment = parts.fragment.clone();
        if query.is_none() {
            query.clone_from(&base_parts.query);
            if fragment.is_none() {
                fragment.clone_from(&base_parts.fragment);
            }
        }
        return Ok(urlunsplit_raw(
            parts.scheme.as_deref(),
            parts.netloc.as_deref(),
            &base_parts.path,
            query.as_deref(),
            fragment.as_deref(),
        ));
    }

    let mut base_segments: Vec<&str> = base_parts.path.split('/').collect();
    if base_segments.last() != Some(&"") {
        base_segments.pop();
    }
    let segments: Vec<&str> = if parts.path.starts_with('/') {
        parts.path.split('/').collect()
    } else {
        base_segments.extend(parts.path.split('/'));
        drop_inner_empty(base_segments)
    };

    let mut resolved: Vec<&str> = Vec::with_capacity(segments.len());
    for segment in &segments {
        match *segment {
            ".." => {
                resolved.pop();
            }
            "." => {}
            other => resolved.push(other),
        }
    }
    if matches!(segments.last(), Some(&".") | Some(&"..")) {
        resolved.push("");
    }
    let joined = resolved.join("/");
    let path = if joined.is_empty() {
        "/".to_owned()
    } else {
        joined
    };
    Ok(urlunsplit_raw(
        parts.scheme.as_deref(),
        parts.netloc.as_deref(),
        &path,
        parts.query.as_deref(),
        parts.fragment.as_deref(),
    ))
}

/// Drop empty inner path segments; the first and last segments are kept.
fn drop_inner_empty(segments: Vec<&str>) -> Vec<&str> {
    let count = segments.len();
    if count < 2 {
        return segments;
    }
    let mut out = Vec::with_capacity(count);
    out.push(segments[0]);
    out.extend(
        segments[1..count - 1]
            .iter()
            .copied()
            .filter(|s| !s.is_empty()),
    );
    out.push(segments[count - 1]);
    out
}

fn hex_value(byte: u8) -> Option<u8> {
    match byte {
        b'0'..=b'9' => Some(byte - b'0'),
        b'a'..=b'f' => Some(byte - b'a' + 10),
        b'A'..=b'F' => Some(byte - b'A' + 10),
        _ => None,
    }
}

/// Decode `%XX` escapes in an all-ASCII run, then read the bytes as UTF-8 with replacement.
fn decode_ascii_run(run: &str) -> String {
    let bytes = run.as_bytes();
    let mut buf = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' {
            if let (Some(high), Some(low)) = (
                bytes.get(i + 1).copied().and_then(hex_value),
                bytes.get(i + 2).copied().and_then(hex_value),
            ) {
                buf.push(high * 16 + low);
                i += 3;
                continue;
            }
        }
        buf.push(bytes[i]);
        i += 1;
    }
    String::from_utf8_lossy(&buf).into_owned()
}

/// Percent-decode to UTF-8, replacing invalid sequences.
///
/// Each maximal ASCII run is decoded on its own and non-ASCII text is kept as it is, so
/// escapes that straddle a non-ASCII character are replaced piecewise.
pub fn unquote(text: &str) -> String {
    if !text.contains('%') {
        return text.to_owned();
    }
    let mut out = String::with_capacity(text.len());
    let mut run = String::new();
    for c in text.chars() {
        if c.is_ascii() {
            run.push(c);
        } else {
            out.push_str(&decode_ascii_run(&run));
            run.clear();
            out.push(c);
        }
    }
    out.push_str(&decode_ascii_run(&run));
    out
}

/// Quote for query strings: unreserved ASCII kept, space as `+`, other UTF-8 bytes as `%XX`.
pub fn quote_plus(text: &str) -> String {
    const HEX: &[u8; 16] = b"0123456789ABCDEF";
    let mut out = String::with_capacity(text.len());
    for byte in text.bytes() {
        match byte {
            b' ' => out.push('+'),
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'_' | b'.' | b'-' | b'~' => {
                out.push(char::from(byte));
            }
            _ => {
                out.push('%');
                out.push(char::from(HEX[usize::from(byte >> 4)]));
                out.push(char::from(HEX[usize::from(byte & 0xf)]));
            }
        }
    }
    out
}

/// Parse a query string with `&` separators; blank values are dropped.
pub fn parse_qsl(query: &str) -> Vec<(String, String)> {
    let mut out = Vec::new();
    for name_value in query.split('&') {
        if name_value.is_empty() {
            continue;
        }
        let Some((name, value)) = name_value.split_once('=') else {
            continue;
        };
        if value.is_empty() {
            continue;
        }
        out.push((
            unquote(&name.replace('+', " ")),
            unquote(&value.replace('+', " ")),
        ));
    }
    out
}

/// Encode pairs as a query string, quoting keys and values as `quote_plus` does.
pub fn urlencode(pairs: &[(String, String)]) -> String {
    pairs
        .iter()
        .map(|(key, value)| format!("{}={}", quote_plus(key), quote_plus(value)))
        .collect::<Vec<_>>()
        .join("&")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn splits_scheme_netloc_path_query_fragment() {
        let parts = urlsplit("HTTP://Ex.COM:80/A/B?x=1#frag").unwrap();
        assert_eq!(parts.scheme, "http");
        assert_eq!(parts.netloc, "Ex.COM:80");
        assert_eq!(parts.path, "/A/B");
        assert_eq!(parts.query, "x=1");
        assert_eq!(parts.fragment, "frag");
    }

    #[test]
    fn strips_controls_and_tabs_before_splitting() {
        let parts = urlsplit("\t\nhttps://ex.com/p\r\n").unwrap();
        assert_eq!(parts.path, "/p");
    }

    #[test]
    fn urlparse_moves_params_out_of_the_path() {
        let parts = urlparse("https://ex.com/a;b/c").unwrap();
        assert_eq!(parts.path, "/a;b/c");
        assert_eq!(parts.params, "");
        let parts = urlparse("https://ex.com/a;p").unwrap();
        assert_eq!((parts.path.as_str(), parts.params.as_str()), ("/a", "p"));
    }

    #[test]
    fn unsplit_keeps_triple_slash_for_empty_netloc_on_network_scheme() {
        assert_eq!(urlunsplit("http", "", "/x", "", ""), "http:///x");
        assert_eq!(urlunsplit("mailto", "", "x@y", "", ""), "mailto:x@y");
    }

    #[test]
    fn unbalanced_bracket_is_an_error() {
        assert_eq!(urlsplit("https://[::1"), Err(UrlError::InvalidIpv6));
    }

    #[test]
    fn join_resolves_dot_segments() {
        assert_eq!(
            urljoin("https://example.com/a/b/c", "../../d?q#f").unwrap(),
            "https://example.com/d?q#f"
        );
        assert_eq!(
            urljoin("https://example.com/a/b/c", "//other.org/x").unwrap(),
            "https://other.org/x"
        );
    }

    // A bare `?` keeps its own empty query. The base query is inherited only when the
    // URL has no query at all, and the fragment is inherited only inside that same branch.
    #[test]
    fn join_empty_query_and_fragment_inherit_on_missing_parts_only() {
        assert_eq!(
            urljoin("https://ex.com/p?a=1", "?").unwrap(),
            "https://ex.com/p?"
        );
        assert_eq!(
            urljoin("https://ex.com/p?a=1#b", "?").unwrap(),
            "https://ex.com/p?"
        );
        assert_eq!(
            urljoin("https://ex.com/p?a=1#b", "#x").unwrap(),
            "https://ex.com/p?a=1#x"
        );
        assert_eq!(
            urljoin("https://ex.com/p#b", "").unwrap(),
            "https://ex.com/p#b"
        );
        assert_eq!(
            urljoin("https://ex.com/p?a=1#b", "?q").unwrap(),
            "https://ex.com/p?q"
        );
    }

    #[test]
    fn query_round_trips_plus_and_percent_escapes() {
        assert_eq!(
            parse_qsl("d=%E2%82%AC+x&b=&c"),
            vec![("d".to_owned(), "\u{20ac} x".to_owned())]
        );
        assert_eq!(quote_plus("k y"), "k+y");
        assert_eq!(quote_plus("/"), "%2F");
    }
}
