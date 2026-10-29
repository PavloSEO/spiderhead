//! Parser for `Cache-Control` directives.
//!
//! Only the directive grammar lives here. Freshness rules (no-store, max-age, Expires, Age) are handled elsewhere in the cache.

use std::collections::BTreeMap;

use crate::url_normalize::is_py_space;

/// Parses a `Cache-Control` header into a map from directive name to argument.
///
/// Names are lower-cased. A directive with an argument maps to `Some(value)`, with surrounding whitespace and double quotes removed. A bare directive maps to `None`.
/// Empty parts are skipped. A repeated name keeps its last value.
/// An empty input yields an empty map.
pub fn parse_cache_control(value: &str) -> BTreeMap<String, Option<String>> {
    let mut out = BTreeMap::new();
    for part in value.split(',') {
        let part = part.trim_matches(is_py_space);
        if part.is_empty() {
            continue;
        }
        match part.split_once('=') {
            Some((name, raw)) => {
                let name = name.trim_matches(is_py_space).to_lowercase();
                let raw = raw.trim_matches(is_py_space).trim_matches('"');
                out.insert(name, Some(raw.to_owned()));
            }
            None => {
                out.insert(part.to_lowercase(), None);
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Expected outputs for each input, checked case by case. The empty string stands for a missing header.
    #[test]
    fn parse_cache_control_matches_reference_oracle_table() {
        type Case<'a> = (&'a str, &'a [(&'a str, Option<&'a str>)]);
        let cases: &[Case] = &[
            ("", &[]),
            ("", &[]),
            (
                "public, max-age=3600, immutable",
                &[
                    ("immutable", None),
                    ("max-age", Some("3600")),
                    ("public", None),
                ],
            ),
            ("no-store", &[("no-store", None)]),
            ("no-cache", &[("no-cache", None)]),
            ("max-age=\"120\"", &[("max-age", Some("120"))]),
            (
                "  MAX-AGE = 5 , , private ",
                &[("max-age", Some("5")), ("private", None)],
            ),
            ("a=b=c", &[("a", Some("b=c"))]),
            ("=x", &[("", Some("x"))]),
            ("Max-Age=\"\"", &[("max-age", Some(""))]),
            ("x=\"\"\"a\"\"\"", &[("x", Some("a"))]),
            (
                "s-maxage=10,must-revalidate",
                &[("must-revalidate", None), ("s-maxage", Some("10"))],
            ),
            ("max-age=10, MAX-AGE=20", &[("max-age", Some("20"))]),
            (
                "private=\"a,b\", no-cache",
                &[("b\"", None), ("no-cache", None), ("private", Some("a"))],
            ),
            ("x=\"a\" ", &[("x", Some("a"))]),
            ("\u{1c}no-store\u{1d}", &[("no-store", None)]),
            ("\"", &[("\"", None)]),
            ("a=\"\"x\"\"", &[("a", Some("x"))]),
            ("no-store,", &[("no-store", None)]),
        ];
        for (input, expected) in cases {
            let expected: BTreeMap<String, Option<String>> = expected
                .iter()
                .map(|(k, v)| ((*k).to_owned(), v.map(str::to_owned)))
                .collect();
            assert_eq!(parse_cache_control(input), expected, "input {input:?}");
        }
    }

    #[test]
    fn parse_cache_control_lowercases_names_and_strips_quotes() {
        let parsed = parse_cache_control("  MAX-AGE = 5 , , private ");
        assert_eq!(parsed.get("max-age"), Some(&Some("5".to_owned())));
        assert_eq!(parsed.get("private"), Some(&None));
    }
}
