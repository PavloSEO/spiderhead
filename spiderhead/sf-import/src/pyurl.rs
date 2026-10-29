//! reference-compatible URL splitting (`urllib.parse.urlsplit`) for the few places the
//! compare domain inspects URL anatomy: scheme, netloc, path, query, fragment, userinfo
//! and hostname. Field semantics follow Creference, including the case quirks callers rely
//! on (scheme lowercased, hostname lowercased, `hostname` empty only without netloc).

/// One split URL, mirroring the fields a `SplitResult` exposes to these callers.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SplitUrl {
    /// Scheme, lowercased; empty when absent.
    pub scheme: String,
    /// Authority section, verbatim; empty when absent.
    pub netloc: String,
    /// Path section (whatever follows the netloc).
    pub path: String,
    /// Query string without the leading `?`.
    pub query: String,
    /// Fragment without the leading `#`.
    pub fragment: String,
}

impl SplitUrl {
    /// Lowercased host with userinfo and port removed; `None` when the URL has no netloc
    /// (or a bracketed form with an empty host), like `SplitResult.hostname`.
    pub fn hostname(&self) -> Option<String> {
        let (_, hostinfo) = hostinfo_of(&self.netloc);
        let host = match hostinfo.strip_prefix('[') {
            Some(rest) => rest.split(']').next().unwrap_or(rest),
            None => hostinfo.split(':').next().unwrap_or(""),
        };
        if host.is_empty() {
            None
        } else {
            Some(host.to_lowercase())
        }
    }

    /// Username when the netloc carries userinfo; `None` without an `@` separator, and
    /// the empty string for a bare `@` — the distinction `_origin` validates on.
    pub fn username(&self) -> Option<String> {
        let (userinfo, have_info) = userinfo_of(&self.netloc);
        if have_info {
            Some(userinfo.split(':').next().unwrap_or("").to_owned())
        } else {
            None
        }
    }

    /// Password when userinfo carries a colon; `None` without one.
    pub fn password(&self) -> Option<String> {
        let (userinfo, have_info) = userinfo_of(&self.netloc);
        if have_info {
            userinfo.split_once(':').map(|(_, pass)| pass.to_owned())
        } else {
            None
        }
    }
}

fn userinfo_of(netloc: &str) -> (&str, bool) {
    match netloc.rfind('@') {
        Some(at) => (&netloc[..at], true),
        None => ("", false),
    }
}

/// The host-and-port part after the last `@` of the netloc.
fn hostinfo_of(netloc: &str) -> (&str, &str) {
    match netloc.rfind('@') {
        Some(at) => (&netloc[..at], &netloc[at + 1..]),
        None => ("", netloc),
    }
}

fn is_scheme_start(c: char) -> bool {
    c.is_ascii_alphabetic()
}

fn is_scheme_char(c: char) -> bool {
    c.is_ascii_alphanumeric() || matches!(c, '+' | '-' | '.')
}

/// Split a URL the way the reference `urlsplit` does: strip CRLF/TAB, take a leading scheme,
/// then a `//`-introduced netloc up to the first `/?#`, then fragment, then query.
pub fn urlsplit(url: &str) -> SplitUrl {
    let url: String = url
        .chars()
        .filter(|c| !matches!(c, '\t' | '\r' | '\n'))
        .collect();
    let mut scheme = String::new();
    let mut rest = url.as_str();
    if let Some(colon) = url.find(':') {
        if colon > 0 {
            let head = &url[..colon];
            let mut head_chars = head.chars();
            if is_scheme_start(head_chars.next().unwrap_or('\0')) && head_chars.all(is_scheme_char)
            {
                scheme = head.to_lowercase();
                rest = &url[colon + 1..];
            }
        }
    }
    let mut netloc = "";
    if let Some(after) = rest.strip_prefix("//") {
        let bytes = after.as_bytes();
        let mut delim = bytes.len();
        for (index, byte) in bytes.iter().enumerate() {
            if matches!(byte, b'/' | b'?' | b'#') {
                delim = index;
                break;
            }
        }
        netloc = &after[..delim];
        rest = &after[delim..];
    }
    let (rest, fragment) = match rest.split_once('#') {
        Some((head, tail)) => (head, tail),
        None => (rest, ""),
    };
    let (path, query) = match rest.split_once('?') {
        Some((head, tail)) => (head, tail),
        None => (rest, ""),
    };
    SplitUrl {
        scheme,
        netloc: netloc.to_owned(),
        path: path.to_owned(),
        query: query.to_owned(),
        fragment: fragment.to_owned(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // from urllib.parse behaviour checks made while porting the compare domain
    #[test]
    fn splits_the_shapes_the_compare_domain_uses() {
        let split = urlsplit("https://Example.com:8443/a/b?x=1#frag");
        assert_eq!(split.scheme, "https");
        assert_eq!(split.netloc, "Example.com:8443");
        assert_eq!(split.path, "/a/b");
        assert_eq!(split.query, "x=1");
        assert_eq!(split.fragment, "frag");
        assert_eq!(split.hostname().as_deref(), Some("example.com"));
        assert_eq!(split.username(), None);
        assert_eq!(split.password(), None);
    }

    #[test]
    fn userinfo_and_empty_host_follow_python() {
        let split = urlsplit("https://user:pw@host.example/p");
        assert_eq!(split.username().as_deref(), Some("user"));
        assert_eq!(split.password().as_deref(), Some("pw"));
        let bare = urlsplit("https://@host.example");
        assert_eq!(bare.username().as_deref(), Some(""));
        assert_eq!(urlsplit("https://").hostname(), None);
        assert_eq!(urlsplit("/relative/path").hostname(), None);
        assert_eq!(urlsplit("/relative/path").path, "/relative/path");
        assert_eq!(urlsplit("HTTPS://X.example").scheme, "https");
        assert_eq!(urlsplit("https://host.example").path, "");
    }

    #[test]
    fn fragment_before_query_and_colon_inside_scheme_guard() {
        let split = urlsplit("https://h.example/a#f?not=query");
        assert_eq!(split.fragment, "f?not=query");
        assert_eq!(split.query, "");
        assert_eq!(urlsplit("1https://x").scheme, "");
        assert_eq!(urlsplit("1https://x").path, "1https://x");
    }
}
