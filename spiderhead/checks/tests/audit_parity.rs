//! End-to-end parity: Rust findings against the reference audit.v2 baselines of the oracle sites.
//!
//! Only checks the engine has ported are compared (`PORTED`); every other baseline check is
//! reported as not yet covered, never as passing.

use std::collections::BTreeMap;
use std::path::PathBuf;

use serde_json::{json, Value};
use spiderhead_checks::audit::{audit_scan, Issue};

const SITES: [&str; 6] = [
    "basic",
    "canonical-hreflang",
    "errors",
    "redirects",
    "robots-sitemap",
    "schema",
];

/// Checks whose Rust implementation is compared against the baseline.
const PORTED: &[&str] = &[
    "TITLE_MISSING",
    "TITLE_TOO_LONG",
    "TITLE_TOO_SHORT",
    "TITLE_EQUALS_H1",
    "DESC_MISSING",
    "DESC_TOO_LONG",
    "DESC_TOO_SHORT",
    "THIN_CONTENT",
    "LOW_TEXT_RATIO",
    "READABILITY_DIFFICULT",
    "HTTP_URL",
    "VIEWPORT_MISSING",
    "CANONICAL_MISSING",
    "MISSING_HSTS",
    "MISSING_CSP",
    "MISSING_X_CONTENT_TYPE_OPTIONS",
    "MISSING_X_FRAME_OPTIONS",
    "MISSING_REFERRER_POLICY",
    "DUPLICATE_SECURITY_HEADER",
    "OUTLINK_TO_LOCALHOST",
    "NO_RESPONSE",
    "BROKEN_PAGE_4XX",
    "SERVER_ERROR_5XX",
    "NON_INDEXABLE_LINKED",
    "CANONICALISED",
    "CANONICAL_NON_INDEXABLE",
    "NOINDEX",
    "NOFOLLOW_PAGE",
    "H1_MISSING",
    "H1_MULTIPLE",
    "MISSING_CHARSET",
    "INVALID_CHARSET",
    "NO_INTERNAL_OUTLINKS",
    "LOREM_IPSUM_PLACEHOLDER",
    "UNSUPPORTED_PLUGIN",
    "LARGE_HTML",
    "HTML_BLOAT",
    "HTML_OVER_2MB",
    "TITLE_TEMPLATED",
    "TITLE_DUPLICATE",
    "DESC_DUPLICATE",
    "H1_DUPLICATE",
    "H2_DUPLICATE",
    "STRUCTURED_DATA_MISSING",
    "NO_COMPRESSION",
    "BAD_REDIRECT_TYPE",
    "BROKEN_INTERNAL_LINK",
    "LINK_TO_5XX",
    "INTERNAL_LINK_TO_REDIRECT",
    "INTERNAL_LINK_NO_ANCHOR",
    "REDIRECT_CHAIN",
    "REDIRECT_LOOP",
    "SITEMAP_NOT_IN_ROBOTS",
    "SITEMAP_ORPHAN",
    "URL_NOT_IN_SITEMAP",
    "SITEMAP_DESYNC",
    "INTERNALLY_LINKED_ROBOTS_BLOCKED",
    "ROBOTS_BLOCKS_RESOURCES",
    "SITEMAP_URL_3XX",
    "SITEMAP_URL_4XX_5XX",
    "SITEMAP_URL_NON_INDEXABLE",
    "SITEMAP_URL_NON_CANONICAL",
    "SITEMAP_URL_BLOCKED_ROBOTS",
    "SITEMAP_FETCH_INCOMPLETE",
    "MISSING_ABOUT_PAGE",
    "MISSING_CONTACT_PAGE",
    "MISSING_PRIVACY_POLICY",
    "MISSING_TERMS_PAGE",
    "FEW_CITATIONS",
    "NO_AUTHOR_BYLINE",
    "NO_CONTENT_DATES",
    "YMYL_REVIEW_CANDIDATE",
    "META_REFRESH_REDIRECT",
    "CANONICAL_CROSS_ORIGIN",
    "CANONICAL_CHAIN",
    "STRUCTURED_DATA_PARSE_ERROR",
    "HREFLANG_INVALID_CODE",
    "HREFLANG_MULTIPLE_ENTRIES",
    "HREFLANG_MISSING_SELF_REFERENCE",
    "HREFLANG_MISSING_XDEFAULT",
    "HREFLANG_NOT_CANONICAL",
    "HREFLANG_MISSING_RETURN_LINK",
    "A11Y_HTML_LANG_MISSING",
    "A11Y_FORM_LABEL_MISSING",
    "A11Y_IMAGE_ALT_MISSING",
    "A11Y_HEADING_ORDER",
    "A11Y_MAIN_LANDMARK_MISSING",
    "STRUCTURED_DATA_SCHEMA_ERROR",
    "STRUCTURED_DATA_SCHEMA_WARNING",
    "STRUCTURED_DATA_RICH_RESULT_REQUIRED_MISSING",
    "AMP_MISSING_HTML_ATTRIBUTE",
    "DOM_TOO_DEEP",
    "DOM_TOO_MANY_NODES",
    "DUPLICATE_BY_HASH",
    "NEAR_DUPLICATE",
    "SEMANTICALLY_SIMILAR",
    "LOW_RELEVANCE_CONTENT",
    "SEMANTIC_SIMILARITY_CANDIDATE",
    "TITLE_LOW_SEMANTIC_RELEVANCE",
    "H1_LOW_SEMANTIC_RELEVANCE",
    "IMG_MISSING_ALT_ATTRIBUTE",
    "IMG_MISSING_ALT",
    "IMG_MISSING_DIMENSIONS",
    "IMG_ALT_TOO_LONG",
    "IMG_BROKEN",
    "IMG_OVER_KB",
    "HREFLANG_ERROR",
];

fn oracle(rel: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../oracle")
        .join(rel)
}

/// `http://127.0.0.1:<port>` becomes `{BASE}`, as in the baselines.
fn normalise(text: &str) -> String {
    let mut out = String::new();
    let mut rest = text;
    while let Some(at) = rest.find("http://127.0.0.1:") {
        out.push_str(&rest[..at]);
        let tail = &rest[at + "http://127.0.0.1:".len()..];
        let digits = tail.chars().take_while(char::is_ascii_digit).count();
        out.push_str("{BASE}");
        rest = &tail[digits..];
    }
    out.push_str(rest);
    out
}

fn norm_value(v: &Value) -> Value {
    serde_json::from_str(&normalise(&v.to_string())).unwrap_or(Value::Null)
}

fn rust_issue(i: &Issue) -> Value {
    norm_value(&json!({
        "check": i.check,
        "severity": i.severity,
        "source": i.source,
        "message": i.message,
        "fix_hint": i.fix_hint,
        "target_url": i.target_url,
        "status_code": i.status_code,
        "occurrences_count": i.occurrences_count,
        "fingerprint": i.fingerprint,
        "locations": if i.locations.is_empty() { Value::Null } else { Value::Array(i.locations.clone()) },
        "details": if i.details.is_empty() { Value::Null } else { Value::Object(i.details.clone()) },
    }))
}

fn python_issue(i: &Value) -> Value {
    json!({
        "check": i["check"],
        "severity": i["severity"],
        "source": i["source"],
        "message": i["message"],
        "fix_hint": i["fix_hint"],
        "target_url": i["target_url"],
        "status_code": i["status_code"],
        "occurrences_count": i["occurrences_count"],
        "fingerprint": i["fingerprint"],
        "locations": i.get("locations").cloned().unwrap_or(Value::Null),
        "details": i["details"],
    })
}

/// Key-sorted copy, so object order does not matter (audit.v2 writes sorted keys).
fn canon(v: &Value) -> Value {
    match v {
        Value::Object(m) => Value::Object(
            m.iter()
                .collect::<std::collections::BTreeMap<_, _>>()
                .into_iter()
                .map(|(k, v)| (k.clone(), canon(v)))
                .collect(),
        ),
        Value::Array(a) => Value::Array(a.iter().map(canon).collect()),
        other => other.clone(),
    }
}

fn sorted(mut v: Vec<Value>) -> Vec<String> {
    let mut out: Vec<String> = v.drain(..).map(|x| canon(&x).to_string()).collect();
    out.sort();
    out
}

#[test]
fn ported_checks_match_the_reference_baselines() {
    let mut problems = Vec::new();
    let mut matrix: BTreeMap<String, (usize, usize)> = BTreeMap::new();
    let mut uncovered: BTreeMap<String, usize> = BTreeMap::new();
    for site in SITES {
        let ctx =
            audit_scan(&oracle(&format!("raw-scans/{site}/scan.sqlite"))).expect("scan loads");
        let text = std::fs::read_to_string(oracle(&format!("baselines/{site}/audit.v2.json")))
            .expect("baseline");
        let baseline: Value = serde_json::from_str(&text).expect("baseline json");
        let python: Vec<&Value> = baseline["items"]["/issues"]
            .as_array()
            .expect("issues")
            .iter()
            .collect();
        for check in PORTED {
            let want = sorted(
                python
                    .iter()
                    .filter(|i| i["check"] == *check)
                    .map(|i| python_issue(i))
                    .collect(),
            );
            let have = sorted(
                ctx.issues
                    .iter()
                    .filter(|i| i.check == *check)
                    .map(rust_issue)
                    .collect(),
            );
            let slot = matrix.entry((*check).to_owned()).or_default();
            slot.0 += want.len();
            if want == have {
                slot.1 += want.len();
            } else {
                let only_python: Vec<&String> = want.iter().filter(|w| !have.contains(w)).collect();
                let only_rust: Vec<&String> = have.iter().filter(|h| !want.contains(h)).collect();
                problems.push(format!("[{site}] {check}\n  only python: {only_python:?}\n  only rust:   {only_rust:?}"));
            }
        }
        for i in python
            .iter()
            .filter(|i| !PORTED.contains(&i["check"].as_str().unwrap_or("")))
        {
            *uncovered
                .entry(i["check"].as_str().unwrap_or("").to_owned())
                .or_default() += 1;
        }
    }
    eprintln!("ported checks (python findings / matching): {matrix:?}");
    eprintln!(
        "baseline checks not yet ported: {} ids, {} findings",
        uncovered.len(),
        uncovered.values().sum::<usize>()
    );
    assert!(
        problems.is_empty(),
        "{} mismatches:\n{}",
        problems.len(),
        problems
            .iter()
            .take(6)
            .cloned()
            .collect::<Vec<_>>()
            .join("\n")
    );
}
