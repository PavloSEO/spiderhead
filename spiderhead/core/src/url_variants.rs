//! Addresses that should all resolve to one canonical URL.
//!
//! The input is normalised first. Variants use only the netloc and path: the query and
//! fragment never reach a variant.

use crate::net::normalize_url;

/// One address that should redirect to the canonical URL.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UrlVariant {
    /// The variant as written by its rule: an origin, an index path, a path, or a case form.
    pub variant: String,
    /// The rule that produced it: `origin`, `index`, `path` or `case`.
    pub group: String,
    /// The full address.
    pub url: String,
}

/// Netloc and path of an absolute URL, with the query and fragment dropped.
fn split_netloc_and_path(normalized: &str) -> (String, String) {
    let rest = normalized
        .split_once("://")
        .map_or(normalized, |(_, after)| after);
    let end = rest.find(['/', '?', '#']).unwrap_or(rest.len());
    let netloc = &rest[..end];
    let tail = &rest[end..];
    let path_end = tail.find(['?', '#']).unwrap_or(tail.len());
    (netloc.to_owned(), tail[..path_end].to_owned())
}

/// Build the addresses that should all resolve to one canonical URL.
///
/// Origin variants cover scheme times bare and `www` host. Index files are added for the root.
/// A non-root path adds its slashless and slashed forms and, when it differs, an upper-case form.
pub fn build_variants(url: &str) -> Vec<UrlVariant> {
    let (netloc, path_raw) = split_netloc_and_path(&normalize_url(url));
    let host = netloc.to_lowercase();
    let bare = host.strip_prefix("www.").unwrap_or(&host).to_owned();
    let www = format!("www.{bare}");
    let path = if path_raw.is_empty() {
        "/".to_owned()
    } else {
        path_raw
    };

    let mut out = Vec::new();
    let mut add = |variant: String, scheme: &str, host: &str, vpath: &str, group: &str| {
        out.push(UrlVariant {
            variant,
            group: group.to_owned(),
            url: format!("{scheme}://{host}{vpath}"),
        });
    };

    // 1. Origin variants: scheme x bare/www host.
    add(format!("https://{bare}/"), "https", &bare, "/", "origin");
    add(format!("http://{bare}/"), "http", &bare, "/", "origin");
    add(format!("https://{www}/"), "https", &www, "/", "origin");
    add(format!("http://{www}/"), "http", &www, "/", "origin");

    // 2. Index files are a common source of CMS homepage duplicates.
    for index in ["/index.php", "/index.html"] {
        add(index.to_owned(), "https", &bare, index, "index");
    }

    // 3. Trailing-slash and case variants for a supplied non-root path.
    if path != "/" {
        let slashless = path.trim_end_matches('/');
        if !slashless.is_empty() {
            add(slashless.to_owned(), "https", &bare, slashless, "path");
            let slashed = format!("{slashless}/");
            add(slashed.clone(), "https", &bare, &slashed, "path");
            let upper = slashless.to_uppercase();
            if upper != slashless {
                add(upper.clone(), "https", &bare, &upper, "case");
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn urls(variants: &[UrlVariant]) -> Vec<&str> {
        variants
            .iter()
            .map(|variant| variant.url.as_str())
            .collect()
    }

    #[test]
    fn variants_for_root() {
        let variants = build_variants("https://example.com/");
        let all = urls(&variants);
        assert!(all.contains(&"https://example.com/"));
        assert!(all.contains(&"http://www.example.com/"));
        assert!(all.contains(&"https://example.com/index.php"));
        assert!(all.contains(&"https://example.com/index.html"));
        assert_eq!(variants.iter().filter(|v| v.group == "origin").count(), 4);
        // A root URL has no trailing-slash or case variants.
        assert!(!variants
            .iter()
            .any(|v| v.group == "path" || v.group == "case"));
    }

    #[test]
    fn variants_for_path_add_slash_and_case() {
        let variants = build_variants("https://example.com/services/implant/");
        let groups: std::collections::BTreeSet<&str> =
            variants.iter().map(|v| v.group.as_str()).collect();
        for expected in ["origin", "index", "path", "case"] {
            assert!(groups.contains(expected), "missing group {expected}");
        }
        let paths: std::collections::BTreeSet<&str> = variants
            .iter()
            .filter(|v| v.group == "path")
            .map(|v| v.url.as_str())
            .collect();
        let expected_paths: std::collections::BTreeSet<&str> = [
            "https://example.com/services/implant",
            "https://example.com/services/implant/",
        ]
        .into_iter()
        .collect();
        assert_eq!(paths, expected_paths);
        let cases: Vec<&UrlVariant> = variants.iter().filter(|v| v.group == "case").collect();
        assert_eq!(cases.len(), 1);
        assert_eq!(cases[0].url, "https://example.com/SERVICES/IMPLANT");
    }

    #[test]
    fn variants_strip_www_input() {
        let variants = build_variants("https://www.example.com/");
        assert_eq!(variants[0].url, "https://example.com/");
    }

    // The variant key carries the rule's own spelling, and the query never reaches a variant.
    #[test]
    fn variant_text_and_query_handling_follow_the_reference_rule() {
        let variants = build_variants("https://Example.com/Docs/?ref=1");
        let path = variants
            .iter()
            .find(|v| v.group == "path")
            .expect("path variant");
        assert_eq!(path.variant, "/Docs");
        assert_eq!(path.url, "https://example.com/Docs");
        assert!(variants.iter().all(|v| !v.url.contains('?')));
    }
}
