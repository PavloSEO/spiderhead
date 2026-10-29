//! Domain and URL normalization that never touches DNS.

use super::idna_codec::encode_idna;
use crate::url_normalize::{is_py_space, split, NormUrlError};
use fancy_regex::Regex;
use std::sync::OnceLock;

/// Second-level labels that mark a compound public suffix (`co.uk`-style).
const COMPOUND_SUFFIXES: &[&str] = &["com", "net", "org", "co", "gov", "edu", "ac", "spb", "msk"];

/// Multi-tenant hosting suffixes whose direct child label is a customer, not a subdomain.
const MULTI_TENANT_SUFFIXES: &[&str] = &[
    "github.io",
    "githubusercontent.com",
    "gitlab.io",
    "pages.dev",
    "vercel.app",
    "netlify.app",
    "herokuapp.com",
    "myshopify.com",
    "wordpress.com",
    "blogspot.com",
    "s3.amazonaws.com",
    "web.app",
    "firebaseapp.com",
];

const DOMAIN_PATTERN: &str = r"^(?=.{1,253}\n?\z)(?!-)[a-z0-9-]{1,63}(?<!-)(\.(?!-)[a-z0-9-]{1,63}(?<!-))*\.(?!-)[a-z0-9-]*[a-z][a-z0-9-]*(?<!-)\n?\z";

fn domain_regex() -> Option<&'static Regex> {
    static RE: OnceLock<Option<Regex>> = OnceLock::new();
    RE.get_or_init(|| Regex::new(DOMAIN_PATTERN).ok()).as_ref()
}

/// Approximate the registrable domain of a hostname.
pub fn registrable_domain(host: &str) -> String {
    let host = host.to_lowercase();
    let host = host.trim_matches('.');
    let parts: Vec<&str> = host.split('.').collect();
    if parts.len() <= 2 {
        return host.to_owned();
    }
    // Longest suffix first, so a 3-label entry is tried before a 2-label suffix can mask it.
    for suffix_len in [3_usize, 2] {
        if parts.len() > suffix_len {
            let suffix = parts[parts.len() - suffix_len..].join(".");
            if MULTI_TENANT_SUFFIXES.contains(&suffix.as_str()) {
                return parts[parts.len() - suffix_len - 1..].join(".");
            }
        }
    }
    let second = parts[parts.len() - 2];
    if COMPOUND_SUFFIXES.contains(&second) && parts[parts.len() - 1].chars().count() <= 3 {
        return parts[parts.len() - 3..].join(".");
    }
    parts[parts.len() - 2..].join(".")
}

/// Normalize a URL or hostname to a lowercase, non-www ASCII domain, or `""` if invalid.
///
/// Fails when the URL parser rejects the input, for example an unbalanced bracket;
/// use [`normalize_domain`] to fold that into `""`.
///
/// # Errors
///
/// Returns the [`NormUrlError`] when the URL parser rejects the input.
pub fn try_normalize_domain(value: &str) -> Result<String, NormUrlError> {
    let raw = value.trim_matches(is_py_space);
    if raw.is_empty() {
        return Ok(String::new());
    }
    let parts;
    let raw = if raw.contains("//") {
        parts = split(raw)?;
        if parts.netloc.is_empty() {
            &parts.path
        } else {
            &parts.netloc
        }
    } else {
        raw
    };
    let raw = raw.split('/').next().unwrap_or("");
    let raw = raw
        .rsplit('@')
        .next()
        .unwrap_or("")
        .trim_matches(is_py_space)
        .trim_end_matches('.')
        .to_lowercase();
    if raw.starts_with('[') || raw.matches(':').count() > 1 {
        return Ok(String::new());
    }
    let raw = raw.split(':').next().unwrap_or("");
    let raw = raw.strip_prefix("www.").unwrap_or(raw);
    let Ok(ascii) = encode_idna(raw) else {
        return Ok(String::new());
    };
    Ok(match domain_regex() {
        Some(re) if re.is_match(&ascii).unwrap_or(false) => ascii,
        _ => String::new(),
    })
}

/// [`try_normalize_domain`] with a rejected URL treated as an invalid domain (`""`).
pub fn normalize_domain(value: &str) -> String {
    try_normalize_domain(value).unwrap_or_default()
}

/// Normalize user input to an absolute HTTP(S) URL without touching DNS, or `""`.
pub fn normalize_url(value: &str) -> String {
    let mut raw = value.trim().to_owned();
    if raw.is_empty() {
        return String::new();
    }
    let scheme_prefix = raw.split(':').next().unwrap_or("").to_lowercase();
    // Scheme letters are any Unicode letter. Lowercasing can change byte length ("İ" grows to
    // two chars, the Kelvin sign shrinks to one byte), so the prefix is sliced by character count.
    let is_alpha = !scheme_prefix.is_empty() && scheme_prefix.chars().all(char::is_alphabetic);
    let after: String = raw.chars().skip(scheme_prefix.chars().count()).collect();
    if !raw.contains("//") && is_alpha && after.starts_with(':') {
        return String::new();
    }
    if !raw.contains("//") {
        raw = format!("https://{raw}");
    }
    let Some((scheme, rest)) = raw.split_once("://") else {
        return String::new();
    };
    let scheme = scheme.to_lowercase();
    if scheme != "http" && scheme != "https" {
        return String::new();
    }
    let netloc = rest.split(['/', '?', '#']).next().unwrap_or("");
    if netloc.is_empty() || netloc.contains('@') {
        return String::new();
    }
    raw
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn registrable_domain_table() {
        assert_eq!(registrable_domain("www.example.com"), "example.com");
        assert_eq!(registrable_domain("shop.example.co.uk"), "example.co.uk");
        assert_eq!(registrable_domain("me.github.io"), "me.github.io");
        assert_eq!(registrable_domain("a.me.github.io"), "me.github.io");
        assert_eq!(registrable_domain("example.com"), "example.com");
    }

    // Behaviour of normalize_domain: scheme, path, userinfo, port and www are stripped.
    #[test]
    fn normalize_domain_strips_decoration() {
        assert_eq!(
            normalize_domain("https://WWW.Example.com/path"),
            "example.com"
        );
        assert_eq!(
            normalize_domain("user@host.example.com:8080"),
            "host.example.com"
        );
        assert_eq!(normalize_domain("[::1]"), "");
        assert_eq!(normalize_domain("   "), "");
    }

    // Behaviour of normalize_domain: internationalized labels become punycode.
    #[test]
    fn normalize_domain_punycodes_idn() {
        assert_eq!(normalize_domain("пример.рф"), "xn--e1afmkfd.xn--p1ai");
    }

    // Behaviour of normalize_url: schemeless input gets https, other schemes are refused.
    #[test]
    fn normalize_url_table() {
        assert_eq!(normalize_url("example.com/a"), "https://example.com/a");
        assert_eq!(normalize_url("http://example.com"), "http://example.com");
        assert_eq!(normalize_url("mailto:a@b.c"), "");
        assert_eq!(normalize_url("ftp://example.com"), "");
        assert_eq!(normalize_url("https://user@example.com"), "");
        assert_eq!(normalize_url(""), "");
    }

    // Behaviour of normalize_url: Unicode scheme letters still mean "has a scheme", and a prefix that
    // grows under lowering cannot panic.
    #[test]
    fn normalize_url_unicode_scheme_prefixes() {
        assert_eq!(normalize_url("тест:x"), "");
        assert_eq!(normalize_url("\u{212a}\u{212a}:x"), "");
        assert_eq!(normalize_url("\u{212a}"), "https://\u{212a}");
        assert_eq!(normalize_url("\u{130}"), "https://\u{130}");
        assert_eq!(normalize_url("\u{130}:x"), "https://\u{130}:x");
    }
}

#[cfg(test)]
mod oracle_tests {
    use super::*;

    /// Input and expected-result table. A row that expects an error must return `Err`.
    #[test]
    fn domain_functions_match_oracle_differential_table() {
        let rows: serde_json::Value =
            serde_json::from_str(include_str!("../../tests/fixtures/domain_oracle.json"))
                .expect("fixture parses");
        let rows = rows.as_array().expect("array");
        assert!(rows.len() >= 300);
        let mut bad = Vec::new();
        for row in rows {
            let input = row["input"].as_str().expect("input");
            let want = row.get("ok").and_then(|v| v.as_str());
            let got = match row["fn"].as_str() {
                Some("nd") => try_normalize_domain(input).ok(),
                _ => Some(registrable_domain(input)),
            };
            if got.as_deref() != want {
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
