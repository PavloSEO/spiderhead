//! Normalized URL equality key.
//!
//! The key folds scheme and host case and drops trailing slashes from the path. It keeps path,
//! query and fragment case, because a case-sensitive server can serve `/News` and `/news` as
//! different resources. The trailing-slash fold is a deliberate many-to-one tolerance for canonicals
//! written without one.
//!
//! Splitting and rejoining follow the `urlsplit` and `urlunsplit` rules, quirks included: `https://a/x?` loses the `?`.

use thiserror::Error;

/// Error for input that `urlsplit` rejects.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum NormUrlError {
    /// The authority has an unbalanced bracket (`[` without `]`, or `]` without `[`).
    #[error("Invalid IPv6 URL")]
    InvalidIpv6,
    /// The bracketed host is neither an IPv6 literal nor an IPvFuture literal.
    #[error("invalid bracketed host")]
    InvalidBracketedHost,
    /// The non-ASCII authority contains `/ ? # @ :` after NFKC normalization.
    #[error("netloc contains invalid characters under NFKC normalization")]
    NetlocNfkc,
}

/// Schemes whose authority is written as `//host`. The empty string is listed too, so callers
/// must test for a non-empty scheme first.
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

/// Normalize `url` into the equality key used for lookups. `None` and blank input give `""`.
///
/// # Errors
///
/// Returns a [`NormUrlError`] exactly when `urlsplit` would reject the input:
/// unbalanced or malformed brackets, or a non-ASCII authority that NFKC-expands to delimiters.
pub fn norm_url(url: Option<&str>) -> Result<String, NormUrlError> {
    let trimmed = url.unwrap_or("").trim_matches(is_py_space);
    if trimmed.is_empty() {
        return Ok(String::new());
    }
    let parts = split(trimmed)?;
    let path = parts.path.trim_end_matches('/');
    Ok(join(
        &parts.scheme,
        &parts.netloc.to_lowercase(),
        path,
        &parts.query,
        &parts.fragment,
    ))
}

/// Whether `c` is whitespace for stripping purposes.
///
/// `char::is_whitespace` misses the separators U+001C to U+001F, so they are added here.
pub(crate) fn is_py_space(c: char) -> bool {
    c.is_whitespace() || ('\u{1c}'..='\u{1f}').contains(&c)
}

/// The five components of a URL, as `urlsplit` returns them (absent parts are empty).
pub(crate) struct Parts {
    pub(crate) scheme: String,
    pub(crate) netloc: String,
    pub(crate) path: String,
    query: String,
    fragment: String,
}

/// Split a URL key value into its five parts, as `urlsplit` does.
pub(crate) fn split(input: &str) -> Result<Parts, NormUrlError> {
    // Leading C0 controls and spaces are removed, then TAB, CR and LF anywhere.
    let stripped = input.trim_start_matches(|c: char| c <= ' ');
    let cleaned: String = stripped
        .chars()
        .filter(|c| !matches!(c, '\t' | '\r' | '\n'))
        .collect();
    let mut rest = cleaned.as_str();

    let mut scheme = String::new();
    if let Some(i) = rest.find(':') {
        let head = &rest[..i];
        let starts_alpha = head.as_bytes().first().is_some_and(u8::is_ascii_alphabetic);
        if i > 0 && starts_alpha && head.chars().all(is_scheme_char) {
            scheme = head.to_ascii_lowercase();
            rest = &rest[i + 1..];
        }
    }

    let mut netloc = String::new();
    if let Some(after) = rest.strip_prefix("//") {
        let delim = after.find(['/', '?', '#']).unwrap_or(after.len());
        netloc = after[..delim].to_owned();
        rest = &after[delim..];
        check_netloc(&netloc)?;
    }

    let (before_fragment, fragment) = rest.split_once('#').unwrap_or((rest, ""));
    let (path, query) = before_fragment
        .split_once('?')
        .unwrap_or((before_fragment, ""));

    Ok(Parts {
        scheme,
        netloc,
        path: path.to_owned(),
        query: query.to_owned(),
        fragment: fragment.to_owned(),
    })
}

/// The validation `urlsplit` runs on an authority: balanced brackets, a valid bracketed
/// host, and no delimiters appearing under NFKC normalization (shared with `parse::py_url`).
///
/// # Errors
///
/// Returns the [`NormUrlError`] for the case that `urlsplit` rejects.
pub fn check_netloc(netloc: &str) -> Result<(), NormUrlError> {
    let (open, close) = (netloc.contains('['), netloc.contains(']'));
    if open != close {
        return Err(NormUrlError::InvalidIpv6);
    }
    if open {
        check_bracketed_netloc(netloc)?;
    }
    check_netloc_nfkc(netloc)
}

/// Checks a bracketed authority, run when it has both bracket kinds.
/// The host after the last `@` is `[host]` with nothing before the bracket and only `:port`
/// after it, or (no `[` there) the text before the first `:`. Either way it must be IPv6 or
/// IPvFuture: the plain host is validated too, so `[::1]@h` is rejected.
fn check_bracketed_netloc(netloc: &str) -> Result<(), NormUrlError> {
    let host_and_port = netloc.rsplit_once('@').map_or(netloc, |(_, h)| h);
    let host = if let Some((before, bracketed)) = host_and_port.split_once('[') {
        if !before.is_empty() {
            return Err(NormUrlError::InvalidIpv6);
        }
        let (host, _, port) = partition(bracketed, ']');
        if !port.is_empty() && !port.starts_with(':') {
            return Err(NormUrlError::InvalidIpv6);
        }
        host
    } else {
        partition(host_and_port, ':').0
    };
    if is_valid_bracketed_host(host) {
        Ok(())
    } else {
        Err(NormUrlError::InvalidBracketedHost)
    }
}

/// `str.partition` for a char separator: `(before, found, after)`.
fn partition(s: &str, sep: char) -> (&str, bool, &str) {
    s.split_once(sep)
        .map_or((s, false, ""), |(a, b)| (a, true, b))
}

/// Checks a bracketed host: `v<hex>.<rest>` (IPvFuture) or an IPv6 address with
/// an optional `%scope`. A bracketed IPv4 address is rejected, like any non-IPv6 text.
fn is_valid_bracketed_host(host: &str) -> bool {
    if let Some(tail) = host.strip_prefix('v') {
        return tail.split_once('.').is_some_and(|(hex, rest)| {
            !hex.is_empty() && hex.bytes().all(|b| b.is_ascii_hexdigit()) && !rest.is_empty()
        });
    }
    let (addr, has_scope, scope) = partition(host, '%');
    if has_scope && (scope.is_empty() || scope.contains(['%', '/'])) {
        return false;
    }
    addr.parse::<std::net::Ipv6Addr>().is_ok()
}

/// A non-ASCII authority must not gain `/ ? # @ :` when NFKC normalized (IDNA uses NFKC
/// equivalence). Delimiters already present are ignored first.
fn check_netloc_nfkc(netloc: &str) -> Result<(), NormUrlError> {
    if netloc.is_ascii() {
        return Ok(());
    }
    let n: String = netloc
        .chars()
        .filter(|c| !matches!(c, '@' | ':' | '#' | '?'))
        .collect();
    let folded = icu_normalizer::ComposingNormalizerBorrowed::new_nfkc().normalize(&n);
    if folded == n || !folded.contains(['/', '?', '#', '@', ':']) {
        return Ok(());
    }
    Err(NormUrlError::NetlocNfkc)
}

/// Whether `c` may appear in a URL scheme (ASCII letters, digits, `+`, `-`, `.`).
fn is_scheme_char(c: char) -> bool {
    c.is_ascii_alphanumeric() || matches!(c, '+' | '-' | '.')
}

/// Rejoin the five parts produced by [`split`], as `urlunsplit` does.
fn join(scheme: &str, netloc: &str, path: &str, query: &str, fragment: &str) -> String {
    // An empty authority becomes present (but empty) only for a netloc-bearing scheme whose path
    // is empty or absolute. Otherwise the authority is omitted.
    let authority: Option<&str> = if !netloc.is_empty() {
        Some(netloc)
    } else if !scheme.is_empty()
        && USES_NETLOC.contains(&scheme)
        && (path.is_empty() || path.starts_with('/'))
    {
        Some("")
    } else {
        None
    };

    let mut url = match authority {
        Some(auth) => {
            let absolute = if !path.is_empty() && !path.starts_with('/') {
                format!("/{path}")
            } else {
                path.to_owned()
            };
            format!("//{auth}{absolute}")
        }
        None if path.starts_with("//") => format!("//{path}"),
        None => path.to_owned(),
    };
    if !scheme.is_empty() {
        url = format!("{scheme}:{url}");
    }
    if !query.is_empty() {
        url.push('?');
        url.push_str(query);
    }
    if !fragment.is_empty() {
        url.push('#');
        url.push_str(fragment);
    }
    url
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Key for a URL that is expected to be valid.
    fn key(url: &str) -> String {
        norm_url(Some(url)).expect("valid URL")
    }

    #[test]
    fn norm_url_keeps_path_case_distinct() {
        assert_ne!(
            key("https://example.test/en"),
            key("https://example.test/EN")
        );
        assert_ne!(
            key("https://example.com/News"),
            key("https://example.com/news")
        );
    }

    #[test]
    fn norm_url_still_folds_scheme_and_host_case() {
        assert_eq!(key("HTTPS://Example.COM/x"), key("https://example.com/x"));
    }

    #[test]
    fn norm_url_still_folds_a_trailing_slash() {
        assert_eq!(key("https://example.com/x/"), key("https://example.com/x"));
        assert_eq!(key("https://example.com/"), key("https://example.com"));
    }

    #[test]
    fn norm_url_query_case_is_preserved() {
        assert_ne!(
            key("https://example.com/x?Q=A"),
            key("https://example.com/x?q=a")
        );
    }

    #[test]
    fn norm_url_blank_and_missing_input_give_empty_key() {
        assert_eq!(norm_url(None), Ok(String::new()));
        assert_eq!(norm_url(Some("   ")), Ok(String::new()));
    }

    #[test]
    fn norm_url_rejects_unbalanced_ipv6_brackets() {
        assert_eq!(
            norm_url(Some("https://[ex.com/a")),
            Err(NormUrlError::InvalidIpv6)
        );
        assert_eq!(
            norm_url(Some("https://ex.com]/a")),
            Err(NormUrlError::InvalidIpv6)
        );
    }

    /// Edge-case table: expected keys for unusual inputs, pinning the urlsplit/urlunsplit behaviour.
    #[test]
    fn norm_url_matches_reference_oracle_table() {
        let cases: &[(Option<&str>, &str)] = &[
            (Some(""), ""),
            (None, ""),
            (Some("   "), ""),
            (Some("https://example.com/"), "https://example.com"),
            (Some("HTTPS://Example.COM/x/"), "https://example.com/x"),
            (Some("https://example.com/x?"), "https://example.com/x"),
            (Some("https://example.com/x#"), "https://example.com/x"),
            (
                Some("https://example.com/x?Q=A#Frag/"),
                "https://example.com/x?Q=A#Frag/",
            ),
            (
                Some("https://example.com/News/"),
                "https://example.com/News",
            ),
            (Some("//example.com/x/"), "//example.com/x"),
            (Some("/rel/path/"), "/rel/path"),
            (Some("example.com/x/"), "example.com/x"),
            (Some("mailto:a@b.com"), "mailto:a@b.com"),
            (Some("tel:+7 123"), "tel:+7 123"),
            (Some("http:x"), "http:x"),
            (Some("HTTP:x/y"), "http:x/y"),
            (
                Some("https://user:Pass@Host.COM:8080/P/?a=1"),
                "https://user:pass@host.com:8080/P?a=1",
            ),
            (Some("https://example.com"), "https://example.com"),
            (
                Some("https://example.com/a?b#c?d"),
                "https://example.com/a?b#c?d",
            ),
            (
                Some("https://example.com/a#x?y"),
                "https://example.com/a#x?y",
            ),
            (
                Some("https://example.com/a?b?c"),
                "https://example.com/a?b?c",
            ),
            (Some("\u{9}https://ex.com/a\u{a}/b"), "https://ex.com/a/b"),
            (Some("  https://ex.com/a  "), "https://ex.com/a"),
            (Some("https://ex.com//"), "https://ex.com"),
            (Some("https://ex.com/a/b//"), "https://ex.com/a/b"),
            (Some("?x=1"), "?x=1"),
            (Some("#frag"), "#frag"),
            (Some("localhost:8080/x"), "localhost:8080/x"),
            (Some("a:b:c"), "a:b:c"),
            (Some("https://[::1]:80/X"), "https://[::1]:80/X"),
            (
                Some("https://ex.com/\u{c4}/\u{df}"),
                "https://ex.com/\u{c4}/\u{df}",
            ),
            (
                Some("https://EXAMPLE.com/%7Euser/"),
                "https://example.com/%7Euser",
            ),
            (Some("data:text/plain,Hi"), "data:text/plain,Hi"),
            (Some("javascript:void(0)"), "javascript:void(0)"),
            (Some("http:"), "http://"),
            (Some("https:///x"), "https:///x"),
            (Some("////x"), "////x"),
            (Some("https://ex.com/a]"), "https://ex.com/a]"),
            (Some("https://[::1]/x"), "https://[::1]/x"),
            (Some("x:/y"), "x:/y"),
            (Some("a+b.c-d:/z"), "a+b.c-d:/z"),
            (Some("1http://ex.com"), "1http://ex.com"),
            (Some("\u{0}\u{1}https://ex.com/a/"), "https://ex.com/a"),
            (Some(" \u{1c}https://Ex.com/ \u{1f}"), "https://ex.com"),
            (Some("https://ex.com/a?#"), "https://ex.com/a"),
            (Some("https://ex.com/?#/"), "https://ex.com#/"),
            (Some("file:///etc/hosts/"), "file:///etc/hosts"),
            (Some("ws://Host:1/x/"), "ws://host:1/x"),
            (Some("mailto:"), "mailto:"),
            (Some("http://ex.com?q/"), "http://ex.com?q/"),
            (Some("HTTPS://A.B/%C3%A9/"), "https://a.b/%C3%A9"),
        ];
        for (input, expected) in cases {
            assert_eq!(
                norm_url(*input).as_deref(),
                Ok(*expected),
                "input {input:?}"
            );
        }
    }

    /// Each row must be accepted or rejected as listed, and accepted rows must yield the listed key.
    #[test]
    fn norm_url_matches_oracle_differential_table() {
        let rows: serde_json::Value =
            serde_json::from_str(include_str!("../tests/fixtures/norm_url_oracle.json"))
                .expect("fixture parses");
        let rows = rows.as_array().expect("array");
        assert!(rows.len() >= 200);
        let mut bad = Vec::new();
        for row in rows {
            let input = row["input"].as_str().expect("input");
            let got = norm_url(Some(input));
            let ok = match row.get("ok").and_then(|v| v.as_str()) {
                Some(want) => got.as_deref() == Ok(want),
                None => got.is_err(),
            };
            if !ok {
                bad.push(format!("{input:?}: rust {got:?}, oracle {row}"));
            }
        }
        assert!(
            bad.is_empty(),
            "{} mismatches:\n{}",
            bad.len(),
            bad.join("\n")
        );
    }
}
