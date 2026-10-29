//! Projection of collected evidence onto the analyzer's input contract.

use crate::frontier::{urlsplit, Parts};
use serde_json::Value;

/// Split off the fragment, then the URL parts; the scheme is lowercased.
fn split_with_fragment(url: &str) -> (Parts, String) {
    let (base, fragment) = match url.split_once('#') {
        Some((b, f)) => (b, f.to_string()),
        None => (url, String::new()),
    };
    (urlsplit(base), fragment)
}

/// Is `canonical` the same resource as `url`, per RFC 3986 case rules?
///
/// Scheme and host are case-insensitive and folded; the path, query and fragment are not.
/// Used for the self-canonical question.
pub fn same_page(url: &str, canonical: &str) -> bool {
    let (a, a_fragment) = split_with_fragment(url);
    let (b, b_fragment) = split_with_fragment(canonical);
    (
        a.scheme.to_lowercase(),
        a.netloc.to_lowercase(),
        a.path.trim_end_matches('/').to_string(),
        a.query,
        a_fragment,
    ) == (
        b.scheme.to_lowercase(),
        b.netloc.to_lowercase(),
        b.path.trim_end_matches('/').to_string(),
        b.query,
        b_fragment,
    )
}

/// Lowercased robots directive tokens of the
/// meta and X-Robots values. Simplified compared with the parse crate's version.
fn directive_tokens(sources: &[&str]) -> Vec<String> {
    sources
        .iter()
        .flat_map(|s| s.split([',', ';']))
        .map(|tok| {
            let tok = tok.trim().to_lowercase();
            match tok.rsplit_once(':') {
                Some((_, rest)) => rest.trim().to_string(),
                None => tok,
            }
        })
        .filter(|t| !t.is_empty())
        .collect()
}

/// Derive the indexability pair without inventing a verdict.
///
/// `blocked_by_robots` takes priority over the fetched outcome.
pub fn indexability(record: &Value, blocked_by_robots: bool) -> (String, String) {
    let non = |reason: &str| ("Non-Indexable".to_string(), reason.to_string());
    if blocked_by_robots {
        return non("Blocked by Robots.txt");
    }
    let code = record.get("status_code").and_then(Value::as_i64);
    let error_truthy = match record.get("error") {
        Some(Value::String(s)) => !s.is_empty(),
        Some(Value::Bool(b)) => *b,
        Some(Value::Null) | None => false,
        Some(_) => true,
    };
    if error_truthy && code.is_none() {
        return non("Response unavailable");
    }
    let Some(code) = code else {
        return non("Response unavailable");
    };
    if (300..400).contains(&code) {
        return non("Redirected");
    }
    if code >= 400 {
        return non(if code < 500 {
            "Client Error"
        } else {
            "Server Error"
        });
    }
    let text = |k: &str| record.get(k).and_then(Value::as_str).unwrap_or("");
    let directives = directive_tokens(&[text("meta_robots"), text("x_robots")]);
    if directives.iter().any(|d| d == "noindex") {
        return non("noindex");
    }
    let canonical = text("canonical");
    if !canonical.is_empty() && !same_page(text("url"), canonical) {
        return non("Canonicalised");
    }
    ("Indexable".to_string(), String::new())
}
